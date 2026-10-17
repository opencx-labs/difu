//! A disposable merge attempt. Codex edits; difu validates and owns the push.
use crate::{
    codex, github,
    model::{ModelChoice, PrDetail, PrKey},
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
    process::Command,
    sync::Arc,
};

#[derive(Clone)]
struct Target {
    pr: PrDetail,
    repository: String,
}
fn target(key: &PrKey, cancel: &Cancel) -> Result<Target> {
    let value = github::json(
        &[
            "api",
            &format!("repos/{}/pulls/{}", key.repository(), key.number),
        ],
        cancel,
    )?;
    let pr = github::parse_detail(key, &value)?;
    ensure!(pr.state == "open", "Only open PRs can be resolved");
    let repository = value
        .pointer("/head/repo/full_name")
        .and_then(Value::as_str)
        .context("The PR source repository is unavailable")?
        .to_owned();
    crate::model::validate_repository(&repository)?;
    Ok(Target { pr, repository })
}
fn read(root: &Path, args: &[&str], cancel: &Cancel) -> Result<String> {
    process::checked(repo::git(root).args(args), cancel)
        .map(|s| s.trim_end_matches('\n').to_owned())
}
fn matching(before: &Target, current: &Target) -> Result<()> {
    ensure!(
        before.pr.head == current.pr.head
            && before.pr.base == current.pr.base
            && before.pr.head_branch == current.pr.head_branch
            && before.pr.base_branch == current.pr.base_branch
            && before.repository == current.repository,
        "PR or base branch changed during conflict resolution. The attempt was not pushed; refresh before trying again"
    );
    Ok(())
}

// Never execute repository-defined merge drivers, hooks, filters or rerere.
fn merge_git(root: &Path, cancel: &Cancel) -> Result<Command> {
    let mut command = repo::checkout_git(root, cancel)?;
    command.args([
        "-c",
        "rerere.enabled=false",
        "-c",
        "merge.autoStash=false",
        "-c",
        "merge.renormalize=false",
    ]);
    let drivers = process::run(
        repo::git(root).args([
            "config",
            "--name-only",
            "--get-regexp",
            "^merge\\..*\\.driver$",
        ]),
        None,
        cancel,
    )?;
    ensure!(
        drivers.code == 0 || drivers.code == 1,
        "Cannot inspect configured merge drivers"
    );
    for name in String::from_utf8(drivers.stdout)?.lines() {
        command.arg("-c").arg(format!("{name}=false"));
    }
    Ok(command)
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    mode: String,
    oid: String,
    stage: u8,
    path: String,
}
fn index(root: &Path, cancel: &Cancel) -> Result<Vec<Entry>> {
    let output = process::checked(repo::git(root).args(["ls-files", "--stage", "-z"]), cancel)?;
    output
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|row| {
            let (meta, path) = row.split_once('\t').context("Invalid Git index entry")?;
            let parts = meta.split_whitespace().collect::<Vec<_>>();
            let [mode, oid, stage] = parts.as_slice() else {
                bail!("Invalid Git index metadata");
            };
            Ok(Entry {
                mode: (*mode).into(),
                oid: (*oid).into(),
                stage: stage.parse()?,
                path: path.into(),
            })
        })
        .collect()
}
fn safe_path(root: &Path, path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Invalid conflict path: {path}"
    );
    let mut current = root.to_owned();
    for part in Path::new(path).components() {
        current.push(part.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "Symlink conflict paths are not supported: {path}"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn validate_conflicts(root: &Path, entries: &[Entry], cancel: &Cancel) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for entry in entries.iter().filter(|e| e.stage != 0) {
        ensure!(
            matches!(entry.mode.as_str(), "100644" | "100755"),
            "Symlink and submodule conflicts require manual resolution: {}",
            entry.path
        );
        safe_path(root, &entry.path)?;
        let bytes = process::run(
            repo::git(root).args(["cat-file", "blob", &entry.oid]),
            None,
            cancel,
        )?;
        ensure!(
            bytes.code == 0,
            "Cannot read conflict content: {}",
            entry.path
        );
        ensure!(
            !bytes.stdout.contains(&0) && std::str::from_utf8(&bytes.stdout).is_ok(),
            "Binary or non-UTF-8 conflicts require manual resolution: {}",
            entry.path
        );
        paths.insert(entry.path.clone());
    }
    ensure!(
        !paths.is_empty(),
        "No conflicted text files remain; refresh the PR's merge status"
    );
    Ok(paths)
}
fn outside(entries: &[Entry], allowed: &BTreeSet<String>) -> BTreeMap<String, Entry> {
    entries
        .iter()
        .filter(|e| !allowed.contains(&e.path))
        .map(|e| (e.path.clone(), e.clone()))
        .collect()
}
fn audit(
    root: &Path,
    before: &[Entry],
    allowed: &BTreeSet<String>,
    head: &str,
    base: &str,
    git_file: &[u8],
    cancel: &Cancel,
) -> Result<()> {
    ensure!(
        fs::read(root.join(".git"))? == git_file,
        "Codex changed the worktree's Git metadata"
    );
    ensure!(
        read(root, &["rev-parse", "HEAD"], cancel)? == head,
        "Codex changed HEAD; resolution rejected"
    );
    ensure!(
        read(root, &["rev-parse", "MERGE_HEAD"], cancel)? == base,
        "Codex changed the merge state; resolution rejected"
    );
    let after = index(root, cancel)?;
    ensure!(
        after == before,
        "Codex modified the Git index; resolution rejected"
    );
    for args in [
        vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "-z",
            "--",
        ],
        vec!["ls-files", "--others", "-z"],
    ] {
        let names = process::checked(repo::git(root).args(args), cancel)?;
        for path in names.split('\0').filter(|p| !p.is_empty()) {
            ensure!(
                allowed.contains(path),
                "Codex edited outside the conflicted files: {path}"
            );
        }
    }
    for path in allowed {
        safe_path(root, path)?;
        if root.join(path).exists() {
            let content = fs::read(root.join(path))?;
            ensure!(
                !content.contains(&0) && std::str::from_utf8(&content).is_ok(),
                "Resolution is not a text file: {path}"
            );
        }
    }
    Ok(())
}

pub fn resolve(
    root: &Path,
    key: &PrKey,
    expected_head: &str,
    model: &ModelChoice,
    cancel: &Cancel,
    progress: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<String> {
    let root = repo::validate(root, key, cancel)?;
    progress("Reading the PR and base branch revisions".into());
    let initial = target(key, cancel)?;
    ensure!(
        initial.pr.head == expected_head,
        "PR head changed; refresh before resolving conflicts"
    );
    for branch in [&initial.pr.head_branch, &initial.pr.base_branch] {
        read(
            &root,
            &["check-ref-format", &format!("refs/heads/{branch}")],
            cancel,
        )?;
    }
    // Fail before spending a model turn if Git cannot create a commit identity.
    read(&root, &["var", "GIT_AUTHOR_IDENT"], cancel)?;
    read(&root, &["var", "GIT_COMMITTER_IDENT"], cancel)?;
    let relay = progress.clone();
    let relay: repo::Progress = Arc::new(move |p: repo::SnapshotProgress| relay(p.activity));
    repo::sync_revisions(&root, &initial.pr, cancel, &relay)?;
    progress("Creating an isolated PR worktree".into());
    let mut tree = repo::Worktree::create_disposable(&root, &initial.pr.head, cancel)?;
    let result = (|| -> Result<String> {
        let path = &tree.path;
        progress("Merging the base branch and identifying conflicts".into());
        let merge = process::run(
            merge_git(path, cancel)?.args([
                "merge",
                "--no-commit",
                "--no-ff",
                "--no-edit",
                "--no-verify",
                "--strategy=ort",
                &initial.pr.base,
            ]),
            None,
            cancel,
        )?;
        ensure!(
            merge.code == 1,
            "Expected merge conflicts; Git returned {}: {} {}",
            merge.code,
            String::from_utf8_lossy(&merge.stdout).trim(),
            String::from_utf8_lossy(&merge.stderr).trim()
        );
        let before = index(path, cancel)?;
        let allowed = validate_conflicts(path, &before, cancel)?;
        let git_file = fs::read(path.join(".git"))?;
        progress(format!(
            "Resolving {} conflicted files with {model}",
            allowed.len()
        ));
        codex::resolve_conflicts(
            &root,
            path,
            &allowed.iter().cloned().collect::<Vec<_>>(),
            model,
            cancel,
            progress.clone(),
        )?;
        progress("Validating edit scope and merge state".into());
        audit(
            path,
            &before,
            &allowed,
            &initial.pr.head,
            &initial.pr.base,
            &git_file,
            cancel,
        )?;
        let paths = allowed
            .iter()
            .map(|p| format!(":(literal){p}"))
            .collect::<Vec<_>>();
        process::checked(
            repo::checkout_git(path, cancel)?
                .args(["add", "-A", "--"])
                .args(paths),
            cancel,
        )?;
        let after = index(path, cancel)?;
        ensure!(
            after.iter().all(|e| e.stage == 0),
            "Unresolved conflict entries remain"
        );
        ensure!(
            outside(&before, &allowed) == outside(&after, &allowed),
            "Resolution changed an automatically merged file"
        );
        for entry in after.iter().filter(|e| allowed.contains(&e.path)) {
            let original = before
                .iter()
                .find(|e| e.path == entry.path && e.stage == 2)
                .or_else(|| before.iter().find(|e| e.path == entry.path && e.stage == 3))
                .context("Unknown resolution path")?;
            ensure!(
                entry.mode == original.mode,
                "Resolution changed file permissions: {}",
                entry.path
            );
        }
        read(path, &["diff", "--cached", "--check"], cancel)
            .context("Git found unresolved markers or whitespace errors; no commit was created")?;
        // No project tests, typechecks, builds, hooks or dependency installation.
        matching(&initial, &target(key, cancel)?)?;
        progress("Creating the conflict-resolution merge commit".into());
        process::checked(
            repo::checkout_git(path, cancel)?.args([
                "commit",
                "--no-verify",
                "-m",
                &format!(
                    "Merge {} into {} and resolve conflicts",
                    initial.pr.base_branch, initial.pr.head_branch
                ),
            ]),
            cancel,
        )?;
        let commit = read(path, &["rev-parse", "HEAD"], cancel)?;
        let parents = read(path, &["show", "-s", "--format=%P", "HEAD"], cancel)?;
        ensure!(
            parents == format!("{} {}", initial.pr.head, initial.pr.base),
            "Unexpected merge commit parents; push stopped"
        );
        ensure!(
            read(
                path,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                    "--ignored"
                ],
                cancel
            )?
            .is_empty(),
            "Worktree changed after validation; push stopped"
        );
        progress("Rechecking both branches before pushing".into());
        matching(&initial, &target(key, cancel)?)?;
        let url = format!("https://github.com/{}.git", initial.repository);
        let reference = format!("refs/heads/{}", initial.pr.head_branch);
        progress("Pushing the resolution to the PR branch".into());
        let output = process::run(
            repo::git(path).env("GIT_ALLOW_PROTOCOL", "https").args([
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!gh auth git-credential",
                "push",
                "--porcelain",
                "--no-verify",
                "--recurse-submodules=no",
                "--",
                &url,
                &format!("{commit}:{reference}"),
            ]),
            None,
            cancel,
        )
        .context("Push did not complete; check GitHub for its outcome. No automatic retry")?;
        ensure!(
            output.code == 0,
            "Push failed; no retry was attempted. Check GitHub if the network result is uncertain: {} {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(format!(
            "Conflict resolution pushed to {} · {}. Project checks are delegated to CI.",
            initial.repository, initial.pr.head_branch
        ))
    })();
    progress("Discarding the isolated worktree".into());
    let cleanup = tree.discard();
    match (result, cleanup) {
        (Ok(message), Ok(())) => Ok(message),
        (Err(error), Ok(())) => {
            Err(error.context("Conflict-resolution attempt discarded; no automatic retry"))
        }
        (result, Err(error)) => Err(error.context(format!(
            "{}; could not discard worktree at {}",
            match result {
                Ok(message) => message,
                Err(e) => format!("{e:#}"),
            },
            tree.path.display()
        ))),
    }
}
