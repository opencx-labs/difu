use super::{Job, Session};
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, ensure};
use nix::fcntl::{Flock, FlockArg};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
}

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

/// Serialize fetch-and-pin across sessions sharing the same Git repository.
/// This never checks out or merges into the user's local branch.
fn fresh_main(root: &Path, cancel: &Cancel) -> Result<String> {
    let common = read(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel,
    )?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(Path::new(&common).join("difu-main-fetch.lock"))?;
    let _lease = loop {
        cancel.check()?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lease) => break lease,
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
            Err((_, error)) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    process::checked(
        repo::git(root)
            .env("GIT_ALLOW_PROTOCOL", "https:ssh:file")
            .args([
                "fetch",
                "--atomic",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
                "--refmap=",
                "--",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main",
            ]),
        cancel,
    )
    .context("Cannot fetch origin/main for the new worktree; queued messages are preserved")?;
    read(
        root,
        &["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"],
        cancel,
    )
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
    let sha = if launch.isolated && session.branch.is_none() {
        // Even a legacy inspection may have pinned stale HEAD. Every new
        // isolated session must fetch before pinning its actual starting point.
        fresh_main(&root, cancel)?
    } else if launch.isolated {
        // An interrupted worktree add already recorded ownership and its base.
        // Recover that same branch; never reset a partially prepared workspace.
        session
            .baseline
            .clone()
            .context("Prepared worktree has no pinned base")?
    } else {
        match &session.baseline {
            Some(sha) => sha.clone(),
            None => read(&root, &["rev-parse", "--verify", "HEAD^{commit}"], cancel)?,
        }
    };
    session.baseline = Some(sha.clone());
    if let Job::Coding(launch) = &mut session.job {
        launch.repository = root.clone();
        if launch.isolated {
            launch.base = "origin/main".into();
        }
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

/// Both diff consumers resolve the current workspace against the same cached PR history.
fn comparison(
    session: &Session,
    storage: &crate::storage::Storage,
    cancel: &Cancel,
) -> Result<String> {
    let path = session
        .workspace
        .as_deref()
        .context("No coding workspace")?;
    let branch = read(path, &["branch", "--show-current"], cancel)?;
    let mut links =
        super::pr_cache::load_links(&storage.cache.join(super::pr_cache::SESSION_LINKS))
            .remove(&session.id)
            .unwrap_or_default();
    let local = super::pr_cache::load_links(&storage.cache.join("agent-prs.json"))
        .remove(&session.id)
        .unwrap_or_default();
    super::pr_cache::merge_links(&mut links, local);
    links.retain(|link| super::pr_cache::current(link, path, Some(&branch)));
    if links.iter().any(|link| link.pr.state == "OPEN") {
        return read(path, &["rev-parse", "--verify", "HEAD^{commit}"], cancel);
    }
    links.sort_by(|a, b| b.pr.updated.cmp(&a.pr.updated));
    for link in links
        .iter()
        .filter(|link| link.pr.state == "MERGED" && !link.pr.head.is_empty())
    {
        let head = &link.pr.head;
        ensure!(
            head.chars().all(|c| c.is_ascii_hexdigit()),
            "Invalid merged PR revision"
        );
        // A reused branch name on a fresh history must not resurrect its old diff.
        let ancestor = process::run(
            repo::git(path).args(["merge-base", "--is-ancestor", head, "HEAD"]),
            None,
            cancel,
        )?;
        if ancestor.code == 0 {
            return Ok(head.clone());
        }
        ensure!(
            ancestor.code == 1,
            "Cannot locate the merged PR's final head locally; fetch its branch before viewing changes"
        );
    }
    let base = if let Some(base) = &session.comparison_base {
        base.clone()
    } else {
        read(path, &["symbolic-ref", "refs/remotes/origin/HEAD"], cancel)
            .context("Cannot resolve the repository default branch locally; set origin/HEAD or register an explicit comparison base")?
    };
    ensure!(
        !base.starts_with('-') && !base.is_empty(),
        "Invalid comparison base"
    );
    read(path, &["merge-base", "HEAD", &base], cancel)
        .with_context(|| format!("Cannot find the workspace merge base against {base}"))
}

/// Read worktree changes without touching its index, checking out code or fetching.
pub fn changes(
    session: &Session,
    storage: &crate::storage::Storage,
    cancel: &Cancel,
) -> Result<String> {
    ensure!(
        !session.workspace_removed,
        "This worktree was removed; its named branch and commits remain in the repository"
    );
    if !session.workspace_ready {
        return Ok(String::new());
    }
    let path = session
        .workspace
        .as_deref()
        .context("No coding workspace for this job")?;
    let base = comparison(session, storage, cancel)?;
    let root = PathBuf::from(read(path, &["rev-parse", "--show-toplevel"], cancel)?);
    let path = root.as_path();
    let mut command = repo::git(path);
    command.env("GIT_OPTIONAL_LOCKS", "0").args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
        &base,
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
pub fn statistics(
    session: &Session,
    storage: &crate::storage::Storage,
    cancel: &Cancel,
) -> Result<super::DiffStatistics> {
    ensure!(!session.workspace_removed, "Session worktree was removed");
    if !session.workspace_ready {
        return Ok(super::DiffStatistics::default());
    }
    let path = session
        .workspace
        .as_deref()
        .context("No coding workspace")?;
    let base = comparison(session, storage, cancel)?;
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
            &base,
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
    if !session.workspace_ready && !session.waiting_for_workspace() {
        return Ok(Vec::new());
    }
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
    validate_removal(session, home, cancel, false)
}

fn validate_removal(
    session: &Session,
    home: &Path,
    cancel: &Cancel,
    discard_changes: bool,
) -> Result<()> {
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
    if discard_changes {
        return Ok(());
    }
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
    remove(session, home, cancel, false)
}

pub(super) fn delete(session: &Session, home: &Path, cancel: &Cancel) -> Result<()> {
    remove(session, home, cancel, true)
}

fn remove(session: &Session, home: &Path, cancel: &Cancel, discard_changes: bool) -> Result<()> {
    validate_removal(session, home, cancel, discard_changes)?;
    let Job::Coding(launch) = &session.job else {
        anyhow::bail!("Not a coding workspace");
    };
    let path = session
        .workspace
        .as_ref()
        .context("No workspace to delete")?;
    let mut command = repo::checkout_git(&launch.repository, cancel)?;
    command.args(["worktree", "remove"]);
    if discard_changes {
        command.arg("--force");
    }
    // One --force discards changes, but Git still refuses locked worktrees.
    // Retain the named branch and commits.
    process::checked(command.arg(path), cancel)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::{
            Launch, Status,
            pr_cache::{SessionLink, SessionLinks},
        },
        github::SessionPr,
        model::PrKey,
        storage::Storage,
    };

    fn git(root: &Path, args: &[&str]) -> Result<String> {
        process::checked(
            repo::git(root)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args),
            &Cancel::default(),
        )
        .map(|s| s.trim().to_owned())
    }
    #[test]
    fn new_worktrees_fetch_main_without_changing_dirty_checkout_or_resumed_work() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let origin = root.join("origin");
        let local = root.join("local");
        for path in [&origin, &local] {
            fs::create_dir(path)?;
            git(path, &["init", "-b", "main"])?;
            fs::write(path.join("tracked.txt"), "base\n")?;
            git(path, &["add", "."])?;
            git(path, &["commit", "-m", "base"])?;
        }
        git(
            &local,
            &[
                "remote",
                "add",
                "origin",
                origin.to_str().context("origin path")?,
            ],
        )?;
        git(&local, &["checkout", "-b", "unrelated-local-work"])?;
        let original_head = git(&local, &["rev-parse", "HEAD"])?;
        fs::write(local.join("tracked.txt"), "precious local edit\n")?;
        let make = |id: &str| {
            Session::new(
                id.into(),
                Job::Coding(Launch {
                    repository: local.clone(),
                    isolated: true,
                    base: "HEAD".into(),
                    prompt: String::new(),
                    model: None,
                    effort: None,
                }),
            )
        };
        let cancel = Cancel::default();
        let mut first = make("first");
        inspect(&mut first, &cancel)?;
        fs::write(origin.join("tracked.txt"), "fresh remote main\n")?;
        git(&origin, &["commit", "-am", "advance main"])?;
        let fresh = git(&origin, &["rev-parse", "HEAD"])?;
        prepare(&mut first, &root, &cancel, |_| Ok(()))?;
        assert_eq!(first.baseline.as_deref(), Some(fresh.as_str()));
        let tree = first.workspace.as_ref().context("workspace")?.clone();
        assert_eq!(
            fs::read_to_string(tree.join("tracked.txt"))?,
            "fresh remote main\n"
        );
        assert_eq!(git(&local, &["rev-parse", "HEAD"])?, original_head);
        assert_eq!(
            fs::read_to_string(local.join("tracked.txt"))?,
            "precious local edit\n"
        );
        fs::write(tree.join("tracked.txt"), "session edits\n")?;
        fs::write(origin.join("tracked.txt"), "next remote commit\n")?;
        git(&origin, &["commit", "-am", "advance again"])?;
        let next = git(&origin, &["rev-parse", "HEAD"])?;
        let mut second = make("second");
        prepare(&mut second, &root, &cancel, |_| Ok(()))?;
        assert_eq!(second.baseline.as_deref(), Some(next.as_str()));
        git(
            &local,
            &[
                "remote",
                "set-url",
                "origin",
                root.join("missing").to_str().context("missing path")?,
            ],
        )?;
        prepare(&mut first, &root, &cancel, |_| Ok(()))?;
        assert_eq!(first.baseline.as_deref(), Some(fresh.as_str()));
        assert_eq!(
            fs::read_to_string(tree.join("tracked.txt"))?,
            "session edits\n"
        );
        let mut failed = make("failed");
        assert!(prepare(&mut failed, &root, &cancel, |_| Ok(())).is_err());
        assert!(!root.join("worktrees/failed").exists());
        assert!(failed.baseline.is_none());
        Ok(())
    }

    #[test]
    fn current_worktree_diff_tracks_pr_lifecycle_and_registered_workspaces() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repository = root.join("repo");
        fs::create_dir(&repository)?;
        git(&repository, &["init", "-b", "main"])?;
        fs::write(repository.join("tracked.txt"), "base\n")?;
        git(&repository, &["add", "."])?;
        git(&repository, &["commit", "-m", "base"])?;
        git(
            &repository,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        )?;
        git(
            &repository,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )?;
        let workspace = root.join("first");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "first",
                workspace.to_str().context("path")?,
            ],
        )?;
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: repository.clone(),
                isolated: true,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.workspace = Some(workspace.clone());
        session.branch = Some("first".into());
        session.workspace_ready = true;
        let storage = Storage {
            config: root.join("config.json"),
            cache: root.join("cache"),
        };
        fs::create_dir(&storage.cache)?;
        let cancel = Cancel::default();
        fs::write(workspace.join("tracked.txt"), "base\ncommitted\n")?;
        git(&workspace, &["add", "."])?;
        git(&workspace, &["commit", "-m", "PR commit"])?;
        let head = git(&workspace, &["rev-parse", "HEAD"])?;
        fs::write(workspace.join("tracked.txt"), "base\ncommitted\nlocal\n")?;
        fs::write(workspace.join("staged.txt"), "staged\n")?;
        git(&workspace, &["add", "staged.txt"])?;
        fs::write(workspace.join("untracked.txt"), "untracked\n")?;
        assert_eq!(statistics(&session, &storage, &cancel)?.added, 4);
        assert!(changes(&session, &storage, &cancel)?.contains("+committed"));
        let mut link = SessionLink {
            workspace: workspace.clone(),
            pr: SessionPr {
                summary: None,
                key: PrKey {
                    owner: "example".into(),
                    repo: "project".into(),
                    number: 1,
                },
                state: "OPEN".into(),
                draft: false,
                conflicts: false,
                head_branch: "first".into(),
                head: head.clone(),
                updated: "2026-09-26T00:00:00Z".into(),
            },
        };
        let cache = storage.cache.join(super::super::pr_cache::SESSION_LINKS);
        let save = |link: &SessionLink| -> Result<()> {
            let links: SessionLinks = [(session.id.clone(), vec![link.clone()])]
                .into_iter()
                .collect();
            crate::storage::atomic_json(&cache, &links)
        };
        save(&link)?;
        assert_eq!(statistics(&session, &storage, &cancel)?.added, 3);
        let patch = changes(&session, &storage, &cancel)?;
        assert!(!patch.contains("+committed"));
        assert!(
            patch.contains("+local") && patch.contains("+staged") && patch.contains("+untracked")
        );
        // Squash merge: default-branch history contains the work under a different SHA.
        fs::write(repository.join("tracked.txt"), "base\ncommitted\n")?;
        git(&repository, &["add", "."])?;
        git(&repository, &["commit", "-m", "Squash PR"])?;
        git(
            &repository,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        )?;
        link.pr.state = "MERGED".into();
        save(&link)?;
        assert_eq!(comparison(&session, &storage, &cancel)?, head);
        assert_eq!(statistics(&session, &storage, &cancel)?.added, 3);
        let second = root.join("second");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "second",
                second.to_str().context("path")?,
                "origin/main",
            ],
        )?;
        let prepared = super::super::registration::prepare(
            &session,
            &serde_json::json!({"path":second}),
            &cancel,
        )?;
        super::super::registration::apply(&mut session, &prepared);
        assert_eq!(session.workspaces.len(), 2);
        assert_eq!(session.workspace_revision, 1);
        assert_eq!(statistics(&session, &storage, &cancel)?.added, 0);
        assert!(changes(&session, &storage, &cancel)?.is_empty());
        assert!(
            super::super::registration::prepare(
                &session,
                &serde_json::json!({"path":repository}),
                &cancel
            )
            .is_err()
        );
        let explicit = super::super::registration::prepare(
            &session,
            &serde_json::json!({"path":second,"base":"first"}),
            &cancel,
        )?;
        super::super::registration::apply(&mut session, &explicit);
        assert_eq!(session.comparison_base.as_deref(), Some("first"));
        assert_eq!(
            comparison(&session, &storage, &cancel)?,
            git(&second, &["merge-base", "HEAD", "first"])?
        );
        Ok(())
    }
}
