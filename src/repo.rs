use crate::{
    diff::{self, Snapshot},
    model::{PrDetail, PrKey},
    process::{self, Cancel},
};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

pub(crate) fn git(path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(path)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "submodule.recurse=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.suppressBlankEmpty=false",
            "-c",
            "diff.submodule=short",
        ])
        // No local review operation may trigger an implicit partial-clone fetch.
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_LFS_SKIP_SMUDGE", "1");
    cmd
}
fn read(path: &Path, args: &[&str], cancel: &Cancel) -> Result<String> {
    process::checked(git(path).args(args), cancel).map(|s| s.trim_end_matches('\n').to_owned())
}

pub fn validate(path: &Path, key: &PrKey, cancel: &Cancel) -> Result<PathBuf> {
    let path = if let Some(suffix) = path.to_str().and_then(|p| p.strip_prefix("~/")) {
        dirs::home_dir()
            .context("Cannot locate home directory")?
            .join(suffix)
    } else {
        path.to_owned()
    };
    let root =
        PathBuf::from(read(&path, &["rev-parse", "--show-toplevel"], cancel)?).canonicalize()?;
    let remotes = read(&root, &["remote", "-v"], cancel)?;
    let expected = key.repository().to_lowercase();
    ensure!(
        remotes
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1))
            .any(|remote| remote_repository(remote).as_deref() == Some(&expected)),
        "This clone has no GitHub remote matching {}",
        key.repository()
    );
    Ok(root)
}

fn remote_repository(remote: &str) -> Option<String> {
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path.to_owned()
    } else {
        let url = url::Url::parse(remote).ok()?;
        if url.host_str()? != "github.com" {
            return None;
        }
        url.path().trim_start_matches('/').to_owned()
    };
    Some(
        path.trim_end_matches('/')
            .trim_end_matches(".git")
            .to_lowercase(),
    )
}

fn sha(value: &str) -> Result<()> {
    ensure!(
        value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Git revision from GitHub"
    );
    Ok(())
}
fn has_commit(root: &Path, revision: &str, cancel: &Cancel) -> Result<bool> {
    Ok(process::run(
        git(root).args(["cat-file", "-e", &format!("{revision}^{{commit}}")]),
        None,
        cancel,
    )?
    .code
        == 0)
}
#[derive(Clone, Debug)]
pub struct SnapshotProgress {
    pub step: u8,
    pub activity: String,
}
pub type Progress = Arc<dyn Fn(SnapshotProgress) + Send + Sync>;
fn report(progress: &Progress, step: u8, activity: impl Into<String>) {
    progress(SnapshotProgress {
        step,
        activity: activity.into(),
    });
}

fn revision_ref(pr: &PrDetail, kind: &str) -> String {
    format!(
        "refs/difu/{}/{}/pr/{}/{kind}",
        pr.key.owner, pr.key.repo, pr.key.number
    )
}

pub(crate) fn sync_revisions(
    root: &Path,
    pr: &PrDetail,
    cancel: &Cancel,
    progress: &Progress,
) -> Result<()> {
    pr.key.validate()?;
    sha(&pr.head)?;
    sha(&pr.base)?;
    let mut missing = Vec::new();
    for (kind, revision) in [("head", &pr.head), ("base", &pr.base)] {
        let reference = revision_ref(pr, kind);
        if has_commit(root, revision, cancel)? {
            // Advertise existing PR history before negotiating the next fetch,
            // including objects downloaded by earlier difu releases.
            read(root, &["update-ref", &reference, revision], cancel)?;
        } else {
            missing.push(format!("+{revision}:{reference}"));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    report(progress, 2, "Syncing missing PR revisions");
    let mut command = git(root);
    command
        .env("GIT_ALLOW_PROTOCOL", "https")
        .args([
            "-c",
            "credential.helper=",
            "-c",
            "credential.helper=!gh auth git-credential",
            "fetch",
            "--atomic",
            "--progress",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--refmap=",
            "--",
            &format!("https://github.com/{}.git", pr.key.repository()),
        ])
        .args(missing);
    let progress = progress.clone();
    let output = process::streaming_with_stderr(
        &mut command,
        None,
        cancel,
        |_| {},
        move |line| {
            let line = line.trim().trim_start_matches("remote: ");
            if [
                "Enumerating objects:",
                "Counting objects:",
                "Compressing objects:",
                "Receiving objects:",
                "Resolving deltas:",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
            {
                report(&progress, 2, line);
            }
        },
    )
    .context("Automatic PR sync failed")?;
    let errors = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.code == 0,
        "Automatic PR sync failed: {}",
        errors
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Git exited without an error message")
    );
    for revision in [&pr.head, &pr.base] {
        ensure!(
            has_commit(root, revision, cancel)?,
            "The requested PR revision is unavailable after syncing. Refresh PR details and retry."
        );
    }
    Ok(())
}

pub fn snapshot(root: &Path, pr: &PrDetail, cancel: &Cancel) -> Result<Snapshot> {
    snapshot_with_progress(root, pr, cancel, Arc::new(|_| {}))
}

/// Read context from the same immutable revisions as the visible diff. Never
/// consult the working tree, run diff helpers, or fetch missing objects here.
pub fn file_context(
    root: &Path,
    snapshot: &Snapshot,
    file: &diff::DiffFile,
    cancel: &Cancel,
) -> Result<Vec<diff::DiffLine>> {
    sha(&snapshot.merge_base)?;
    sha(&snapshot.head)?;
    let paths = [
        format!(":(literal){}", file.old_path),
        format!(":(literal){}", file.path),
    ];
    let common = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--ignore-submodules=none",
    ];
    let names = process::checked(
        git(root)
            .args(common)
            .args([
                "--name-status",
                "-z",
                &snapshot.merge_base,
                &snapshot.head,
                "--",
            ])
            .args(&paths),
        cancel,
    )?;
    let patch = process::checked(
        git(root)
            .args(common)
            .args([
                "--no-color",
                "--unified=2147483647",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                &snapshot.merge_base,
                &snapshot.head,
                "--",
            ])
            .args(&paths),
        cancel,
    )?;
    let files = diff::parse(&names, &patch)?;
    let expanded = files
        .into_iter()
        .find(|candidate| candidate.path == file.path && candidate.old_path == file.old_path)
        .context("The pinned file is unavailable for context expansion")?;
    Ok(expanded
        .hunks
        .into_iter()
        .filter(|hunk| hunk.header.starts_with("@@ "))
        .flat_map(|hunk| hunk.lines)
        .collect())
}

pub fn snapshot_with_progress(
    root: &Path,
    pr: &PrDetail,
    cancel: &Cancel,
    progress: Progress,
) -> Result<Snapshot> {
    pr.key.validate()?;
    sha(&pr.head)?;
    sha(&pr.base)?;
    report(&progress, 2, "Checking local PR revisions");
    sync_revisions(root, pr, cancel, &progress)?;
    report(&progress, 3, "Finding the merge base");
    let merge_base = read(root, &["merge-base", &pr.base, &pr.head], cancel).context(
        "Cannot find the PR merge base. A shallow clone may need more history; update your clone manually and retry.",
    )?;
    sha(&merge_base)?;
    let common = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--ignore-submodules=none",
    ];
    report(&progress, 4, "Reading changed files");
    let names = process::checked(
        git(root)
            .args(common)
            .args(["--name-status", "-z", &merge_base, &pr.head, "--"]),
        cancel,
    )?;
    report(&progress, 5, "Building and validating the diff");
    let patch = process::checked(
        git(root).args(common).args([
            "--no-color",
            "--unified=3",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &merge_base,
            &pr.head,
            "--",
        ]),
        cancel,
    )?;
    let files = diff::parse(&names, &patch)?;
    ensure!(
        files.len() as u64 == pr.changed_files,
        "The local diff has {} files but GitHub reports {}. Refresh and retry; difu will not show an incomplete snapshot.",
        files.len(),
        pr.changed_files
    );
    Ok(Snapshot {
        base: pr.base.clone(),
        head: pr.head.clone(),
        head_tree: read(
            root,
            &["rev-parse", &format!("{}^{{tree}}", pr.head)],
            cancel,
        )?,
        base_tree: read(
            root,
            &["rev-parse", &format!("{merge_base}^{{tree}}")],
            cancel,
        )?,
        merge_base,
        files,
    })
}

// Disable checkout filters for both materialization and cleanliness checks.
pub(crate) fn checkout_git(root: &Path, cancel: &Cancel) -> Result<Command> {
    let mut command = git(root);
    // Avoid executing checkout filters (including LFS downloads) during a
    // read-only review. Read their names only; never expose their commands.
    let filters = process::run(
        git(root).args([
            "config",
            "--name-only",
            "--get-regexp",
            "^filter\\..*\\.(clean|smudge|process|required)$",
        ]),
        None,
        cancel,
    )?;
    for name in String::from_utf8(filters.stdout)?.lines() {
        command.arg("-c").arg(format!(
            "{name}={}",
            if name.ends_with(".required") {
                "false"
            } else {
                ""
            }
        ));
    }
    Ok(command)
}

pub struct Worktree {
    disposable: bool,
    root: PathBuf,
    pub path: PathBuf,
    directory: Option<tempfile::TempDir>,
    _lease: crate::worktrees::Lease,
}
impl Worktree {
    /// Only the explicitly disposable conflict-resolution attempt uses this.
    /// Guide worktrees keep their conservative cleanup policy.
    pub(crate) fn discard(&mut self) -> Result<()> {
        if self.directory.is_none() {
            return Ok(());
        }
        if self.path.exists() {
            process::checked(
                checkout_git(&self.root, &Cancel::default())?
                    .args(["worktree", "remove", "--force"])
                    .arg(&self.path),
                &Cancel::default(),
            )?;
        }
        if let Some(directory) = self.directory.take() {
            directory.close()?;
        }
        Ok(())
    }
    pub fn create(root: &Path, revision: &str, cancel: &Cancel) -> Result<Self> {
        Self::create_inner(root, revision, cancel, false)
    }
    pub(crate) fn create_disposable(root: &Path, revision: &str, cancel: &Cancel) -> Result<Self> {
        Self::create_inner(root, revision, cancel, true)
    }
    fn create_inner(
        root: &Path,
        revision: &str,
        cancel: &Cancel,
        disposable: bool,
    ) -> Result<Self> {
        sha(revision)?;
        let directory = tempfile::Builder::new().prefix("difu-review-").tempdir()?;
        let path = directory.path().join("source");
        let lease = crate::worktrees::register(directory.path(), root, &path)?;
        let mut tree = Self {
            disposable,
            _lease: lease,
            root: root.to_owned(),
            path,
            directory: Some(directory),
        };
        let mut command = checkout_git(root, cancel)?;
        let result = process::checked(
            command
                .args([
                    "-c",
                    "core.autocrlf=false",
                    "-c",
                    "core.sparseCheckout=false",
                    "-c",
                    "core.sparseCheckoutCone=false",
                    "worktree",
                    "add",
                    "--detach",
                ])
                .arg(&tree.path)
                .arg(revision),
            cancel,
        );
        if let Err(error) = result {
            if let Err(cleanup) = tree.cleanup() {
                return Err(
                    error.context(format!("Could not prepare the PR worktree; {cleanup:#}"))
                );
            }
            return Err(error.context("Could not prepare the PR worktree"));
        }
        Ok(tree)
    }
    pub fn cleanup(&mut self) -> Result<()> {
        if self.disposable {
            return self.discard();
        }
        let Some(directory) = self.directory.take() else {
            return Ok(());
        };
        if self.path.exists() {
            let result = (|| -> Result<String> {
                process::checked(
                    checkout_git(&self.root, &Cancel::default())?
                        .args(["worktree", "remove"])
                        .arg(&self.path),
                    &Cancel::default(),
                )
            })();
            if let Err(error) = result {
                let kept = directory.keep();
                anyhow::bail!(
                    "Worktree cleanup failed; preserved {} for inspection: {error}",
                    kept.display()
                );
            }
        }
        directory.close()?;
        Ok(())
    }
}
impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ok(root: &Path, args: &[&str]) -> Result<()> {
        read(root, args, &Cancel::default())?;
        Ok(())
    }
    #[test]
    fn file_context_uses_pinned_revisions_and_literal_renamed_paths() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        ok(root, &["init"])?;
        ok(root, &["config", "user.name", "Test"])?;
        ok(root, &["config", "user.email", "test@example.invalid"])?;
        ok(root, &["config", "commit.gpgsign", "false"])?;
        let old_path = "old [file].rs";
        let new_path = "new [file].rs";
        let original = (1..=80).map(|n| format!("line {n}\n")).collect::<String>();
        std::fs::write(root.join(old_path), &original)?;
        ok(root, &["add", "."])?;
        ok(root, &["commit", "-m", "base"])?;
        let base = read(root, &["rev-parse", "HEAD"], &Cancel::default())?;
        std::fs::rename(root.join(old_path), root.join(new_path))?;
        std::fs::write(
            root.join(new_path),
            original
                .replace("line 20\n", "changed 20\n")
                .replace("line 60\n", "changed 60\n"),
        )?;
        ok(root, &["add", "."])?;
        ok(root, &["commit", "-m", "head"])?;
        let head = read(root, &["rev-parse", "HEAD"], &Cancel::default())?;
        let names = process::checked(
            git(root).args([
                "diff",
                "--find-renames",
                "--name-status",
                "-z",
                &base,
                &head,
            ]),
            &Cancel::default(),
        )?;
        let patch = process::checked(
            git(root).args(["diff", "--find-renames", "--unified=3", &base, &head]),
            &Cancel::default(),
        )?;
        let snapshot = Snapshot {
            base: base.clone(),
            head,
            merge_base: base,
            head_tree: String::new(),
            base_tree: String::new(),
            files: diff::parse(&names, &patch)?,
        };
        let file = snapshot.files.first().context("Missing renamed file")?;
        assert_eq!(file.old_path, old_path);
        assert_eq!(file.path, new_path);
        assert_eq!(
            file.hunks
                .iter()
                .filter(|h| h.header.starts_with("@@ "))
                .count(),
            2
        );
        std::fs::write(root.join(new_path), "precious uncommitted work\n")?;
        ok(root, &["config", "diff.external", "false"])?;
        let context = file_context(root, &snapshot, file, &Cancel::default())?;
        assert_eq!(
            context.first().map(|line| line.text.as_str()),
            Some("line 1")
        );
        assert_eq!(
            context.last().map(|line| line.text.as_str()),
            Some("line 80")
        );
        assert!(
            context
                .iter()
                .any(|line| line.kind == diff::LineKind::Add && line.text == "changed 20")
        );
        assert!(!context.iter().any(|line| line.text.contains("precious")));
        assert_eq!(
            std::fs::read_to_string(root.join(new_path))?,
            "precious uncommitted work\n"
        );
        Ok(())
    }

    #[test]
    fn worktree_cleanup_preserves_original_dirty_checkout() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        ok(root, &["init"])?;
        ok(root, &["config", "user.name", "Test"])?;
        ok(root, &["config", "user.email", "test@example.invalid"])?;
        ok(root, &["config", "commit.gpgsign", "false"])?;
        std::fs::write(root.join("file"), "committed")?;
        ok(root, &["add", "file"])?;
        ok(root, &["commit", "-m", "initial"])?;
        let head = read(root, &["rev-parse", "HEAD"], &Cancel::default())?;
        std::fs::write(root.join("file"), "my unfinished work")?;
        let mut tree = Worktree::create(root, &head, &Cancel::default())?;
        assert_eq!(
            std::fs::read_to_string(tree.path.join("file"))?,
            "committed"
        );
        tree.cleanup()?;
        assert!(!tree.path.exists());
        assert_eq!(
            std::fs::read_to_string(root.join("file"))?,
            "my unfinished work"
        );
        assert_eq!(
            read(
                root,
                &["worktree", "list", "--porcelain"],
                &Cancel::default()
            )?
            .matches("worktree ")
            .count(),
            1
        );
        Ok(())
    }
    #[test]
    fn checkout_does_not_run_hooks_or_filters_and_preserves_dirty_review_files() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        ok(root, &["init"])?;
        ok(root, &["config", "user.name", "Test"])?;
        ok(root, &["config", "user.email", "test@example.invalid"])?;
        ok(root, &["config", "commit.gpgsign", "false"])?;
        std::fs::write(root.join("file"), "committed")?;
        std::fs::write(root.join(".gitattributes"), "file filter=difu_test\n")?;
        ok(root, &["add", "."])?;
        ok(root, &["commit", "-m", "initial"])?;
        let hook = root.join(".git/hooks/post-checkout");
        std::fs::write(&hook, "#!/bin/sh\ntouch hook-ran\n")?;
        std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o700))?;
        ok(
            root,
            &["config", "filter.difu_test.smudge", "touch filter-ran; cat"],
        )?;
        ok(
            root,
            &["config", "filter.difu_test.clean", "touch filter-ran; cat"],
        )?;
        ok(root, &["config", "filter.difu_test.required", "true"])?;
        let head = read(root, &["rev-parse", "HEAD"], &Cancel::default())?;
        let mut tree = Worktree::create(root, &head, &Cancel::default())?;
        assert!(!tree.path.join("hook-ran").exists());
        assert!(!tree.path.join("filter-ran").exists());
        std::fs::write(tree.path.join("file"), "unexpected user changes")?;
        assert!(tree.cleanup().is_err());
        assert_eq!(
            std::fs::read_to_string(tree.path.join("file"))?,
            "unexpected user changes"
        );
        assert!(!tree.path.join("filter-ran").exists());
        std::fs::write(tree.path.join("file"), "committed")?;
        process::checked(
            checkout_git(root, &Cancel::default())?
                .args(["worktree", "remove"])
                .arg(&tree.path),
            &Cancel::default(),
        )?;
        let parent = tree.path.parent().context("Missing temporary parent")?;
        std::fs::remove_file(parent.join("difu-owner.json"))?;
        std::fs::remove_file(parent.join("difu.lock"))?;
        std::fs::remove_dir(parent)?;
        Ok(())
    }
    #[test]
    fn remote_matching_rejects_other_hosts() {
        assert_eq!(
            remote_repository("git@github.com:Owner/Repo.git"),
            Some("owner/repo".into())
        );
        assert_eq!(
            remote_repository("https://github.com/Owner/Repo.git"),
            Some("owner/repo".into())
        );
        assert_eq!(
            remote_repository("https://example.com/Owner/Repo.git"),
            None
        );
    }
}
