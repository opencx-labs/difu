use super::{Job, Session};
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn read(root: &Path, args: &[&str], cancel: &Cancel) -> Result<String> {
    Ok(process::checked(
        repo::git(root).env("GIT_OPTIONAL_LOCKS", "0").args(args),
        cancel,
    )?
    .trim_end_matches('\n')
    .to_owned())
}

pub fn repository(path: &Path, cancel: &Cancel) -> Result<PathBuf> {
    let path = if let Some(relative) = path.to_str().and_then(|s| s.strip_prefix("~/")) {
        dirs::home_dir()
            .context("Cannot locate home directory")?
            .join(relative)
    } else {
        path.to_owned()
    };
    let path = path
        .canonicalize()
        .context("Cannot find the selected local directory")?;
    PathBuf::from(read(&path, &["rev-parse", "--show-toplevel"], cancel)?)
        .canonicalize()
        .context("Cannot resolve repository path")
}

/// Pin a starting commit for an empty session without creating a worktree or running Codex.
pub fn inspect(session: &mut Session, cancel: &Cancel) -> Result<()> {
    let root = repository(session.job.root(), cancel)?;
    if session.baseline.is_none() {
        session.baseline = Some(read(
            &root,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            cancel,
        )?);
    }
    if let Job::Coding(launch) = &mut session.job {
        launch.repository = root.clone();
    }
    session.workspace = Some(root);
    Ok(())
}

pub fn prepare(
    session: &mut Session,
    home: &Path,
    cancel: &Cancel,
    persist: impl Fn(&Session) -> Result<()>,
) -> Result<()> {
    let Job::Coding(launch) = &session.job else {
        return Ok(());
    };
    if session.workspace_ready
        && let Some(path) = &session.workspace
    {
        ensure!(
            path.is_dir(),
            "Session workspace no longer exists: {}",
            path.display()
        );
        return Ok(());
    }
    let launch = launch.clone();
    let path = if let Some(relative) = launch
        .repository
        .to_str()
        .and_then(|s| s.strip_prefix("~/"))
    {
        dirs::home_dir()
            .context("Cannot locate home directory")?
            .join(relative)
    } else {
        launch.repository.clone()
    }
    .canonicalize()
    .context("Cannot find the selected local directory")?;
    let root =
        PathBuf::from(read(&path, &["rev-parse", "--show-toplevel"], cancel)?).canonicalize()?;
    let base = if launch.isolated {
        launch.base.trim()
    } else {
        "HEAD"
    };
    ensure!(!base.is_empty(), "Choose a local base branch or revision");
    let sha = match &session.baseline {
        Some(sha) => sha.clone(),
        None => read(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{base}^{{commit}}"),
            ],
            cancel,
        )?,
    };
    session.baseline = Some(sha.clone());
    if let Job::Coding(launch) = &mut session.job {
        launch.repository = root.clone();
    }
    if launch.isolated {
        let trees = home.join("worktrees");
        fs::create_dir_all(&trees)?;
        let destination = trees.join(&session.id);
        let branch = format!("difu/agent-{}", session.id);
        session.workspace = Some(destination.clone());
        session.branch = Some(branch.clone());
        // Record ownership and the pinned base before Git creates any worktree.
        persist(session)?;
        if destination.exists() {
            ensure!(
                read(&destination, &["rev-parse", "HEAD"], cancel)? == sha
                    && read(&destination, &["branch", "--show-current"], cancel)? == branch,
                "Interrupted workspace preparation left a different revision; inspect {} before continuing",
                destination.display()
            );
        } else {
            let reference = format!("refs/heads/{branch}");
            let existing = process::run(
                repo::git(&root).args(["rev-parse", "--verify", &reference]),
                None,
                cancel,
            )?;
            if existing.code == 0 {
                ensure!(
                    String::from_utf8(existing.stdout)?.trim() == sha,
                    "Prepared branch has changed; inspect it before continuing"
                );
                process::checked(
                    repo::checkout_git(&root, cancel)?
                        .args(["worktree", "add"])
                        .arg(&destination)
                        .arg(&branch),
                    cancel,
                )?;
            } else {
                process::checked(
                    repo::checkout_git(&root, cancel)?
                        .args(["worktree", "add", "-b", &branch])
                        .arg(&destination)
                        .arg(&sha),
                    cancel,
                )?;
            }
        }
    } else {
        session.workspace = Some(path);
        session.branch = Some(read(&root, &["rev-parse", "--abbrev-ref", "HEAD"], cancel)?);
    }
    session.workspace_ready = true;
    persist(session)
}

/// Read worktree changes without touching its index, checking out code or fetching.
pub fn changes(session: &Session, cancel: &Cancel) -> Result<String> {
    ensure!(
        !session.workspace_removed,
        "This worktree was removed; its named branch and commits remain in the repository"
    );
    if session.waiting_for_workspace() {
        return Ok(String::new());
    }
    let path = session
        .workspace
        .as_deref()
        .context("No coding workspace for this job")?;
    let base = session
        .baseline
        .as_deref()
        .context("Session has no starting revision")?;
    let root = PathBuf::from(read(path, &["rev-parse", "--show-toplevel"], cancel)?);
    let path = root.as_path();
    let mut command = repo::git(path);
    command.env("GIT_OPTIONAL_LOCKS", "0").args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
        base,
        "--",
    ]);
    let output = process::run_limited(&mut command, cancel, 32 * 1024 * 1024)?;
    ensure!(
        output.code == 0,
        "Cannot read workspace diff: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut patch = String::from_utf8(output.stdout).context("Diff output is not UTF-8")?;
    let untracked = read(
        path,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancel,
    )?;
    for name in untracked.split('\0').filter(|s| !s.is_empty()) {
        cancel.check()?;
        let output = process::run_limited(
            repo::git(path).env("GIT_OPTIONAL_LOCKS", "0").args([
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--",
                "/dev/null",
                name,
            ]),
            cancel,
            32 * 1024 * 1024,
        )?;
        ensure!(
            output.code <= 1,
            "Cannot read untracked file {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        patch.push_str(&String::from_utf8(output.stdout).context("Untracked diff is not UTF-8")?);
        ensure!(
            patch.len() <= 32 * 1024 * 1024,
            "Changes exceed the 32 MiB display limit"
        );
    }
    Ok(patch)
}

/// Lightweight counts use the same baseline and untracked-file scope as Changes.
pub fn statistics(session: &Session, cancel: &Cancel) -> Result<super::DiffStatistics> {
    ensure!(!session.workspace_removed, "Session worktree was removed");
    if session.waiting_for_workspace() {
        return Ok(super::DiffStatistics::default());
    }
    let path = session
        .workspace
        .as_deref()
        .context("No coding workspace")?;
    let base = session
        .baseline
        .as_deref()
        .context("No starting revision")?;
    let mut stats = super::DiffStatistics::default();
    let output = read(
        path,
        &[
            "diff",
            "--numstat",
            "-z",
            "--find-renames",
            "--no-ext-diff",
            "--no-textconv",
            base,
            "--",
        ],
        cancel,
    )?;
    add_statistics(&mut stats, &output)?;
    let untracked = read(
        path,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancel,
    )?;
    for name in untracked.split('\0').filter(|name| !name.is_empty()) {
        cancel.check()?;
        let output = process::run_limited(
            repo::git(path).env("GIT_OPTIONAL_LOCKS", "0").args([
                "diff",
                "--numstat",
                "-z",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                "/dev/null",
                name,
            ]),
            cancel,
            1024 * 1024,
        )?;
        ensure!(
            output.code <= 1,
            "Cannot read statistics for {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        add_statistics(&mut stats, &String::from_utf8(output.stdout)?)?;
    }
    Ok(stats)
}

fn add_statistics(stats: &mut super::DiffStatistics, output: &str) -> Result<()> {
    let mut records = output.split('\0').filter(|row| !row.is_empty());
    while let Some(row) = records.next() {
        let mut columns = row.splitn(3, '\t');
        let added = columns.next().context("Missing addition count")?;
        let removed = columns.next().context("Missing deletion count")?;
        let path = columns.next().context("Missing file in Git statistics")?;
        if path.is_empty() {
            ensure!(
                records.next().is_some() && records.next().is_some(),
                "Missing renamed paths in Git statistics"
            );
        }
        if added == "-" && removed == "-" {
            continue;
        }
        stats.added = stats.added.saturating_add(added.parse::<u64>()?);
        stats.removed = stats.removed.saturating_add(removed.parse::<u64>()?);
    }
    Ok(())
}

pub fn paths(session: &Session, cancel: &Cancel) -> Result<Vec<String>> {
    ensure!(!session.workspace_removed, "Session worktree was removed");
    let root = session
        .workspace
        .as_deref()
        .context("Workspace is still being prepared")?;
    let output = read(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        cancel,
    )?;
    let mut paths = std::collections::BTreeSet::new();
    for name in output.split('\0').filter(|name| !name.is_empty()) {
        if root.join(name).symlink_metadata().is_err() {
            continue;
        }
        paths.insert(name.to_owned());
        for (index, _) in name.match_indices('/') {
            if let Some(directory) = name.get(..=index) {
                paths.insert(directory.to_owned());
            }
        }
    }
    Ok(paths.into_iter().collect())
}

pub fn validate_cleanup(session: &Session, home: &Path, cancel: &Cancel) -> Result<()> {
    ensure!(
        !session.status.active(),
        "An active session's workspace is protected"
    );
    let Job::Coding(launch) = &session.job else {
        anyhow::bail!("Review worktrees use Reviews worktree management");
    };
    ensure!(
        launch.isolated,
        "An existing directory is never deleted by difu"
    );
    let path = session
        .workspace
        .as_ref()
        .context("No workspace to delete")?;
    ensure!(
        path == &home.join("worktrees").join(&session.id),
        "Workspace ownership does not match this session"
    );
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Symlink workspace is protected"
    );
    let status = read(
        path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored",
        ],
        cancel,
    )?;
    ensure!(
        status.is_empty(),
        "Modified, untracked, or ignored files are protected"
    );
    // Git's own remove performs the final lock/registration check without force.
    Ok(())
}

pub fn cleanup(session: &Session, home: &Path, cancel: &Cancel) -> Result<()> {
    validate_cleanup(session, home, cancel)?;
    let Job::Coding(launch) = &session.job else {
        anyhow::bail!("Not a coding workspace");
    };
    let path = session
        .workspace
        .as_ref()
        .context("No workspace to delete")?;
    // Git refuses locked worktrees; never pass --force. Retain the named branch and commits.
    process::checked(
        repo::checkout_git(&launch.repository, cancel)?
            .args(["worktree", "remove"])
            .arg(path),
        cancel,
    )?;
    Ok(())
}
