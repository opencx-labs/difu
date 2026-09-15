use crate::{
    diff::{self, Snapshot},
    model::{PrDetail, PrKey},
    process::{self, Cancel},
};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn git(path: &Path) -> Command {
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
fn fetch(root: &Path, key: &PrKey, reference: &str, cancel: &Cancel) -> Result<()> {
    // Use the agreed gh login rather than requiring a second SSH authentication.
    process::checked(
        git(root).args([
            "-c",
            "credential.helper=",
            "-c",
            "credential.helper=!gh auth git-credential",
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            &format!("https://github.com/{}.git", key.repository()),
            reference,
        ]),
        cancel,
    )?;
    Ok(())
}

pub fn snapshot(root: &Path, pr: &PrDetail, cancel: &Cancel) -> Result<Snapshot> {
    sha(&pr.head)?;
    sha(&pr.base)?;
    if !has_commit(root, &pr.head, cancel)? {
        fetch(
            root,
            &pr.key,
            &format!("refs/pull/{}/head", pr.key.number),
            cancel,
        )?;
    }
    if !has_commit(root, &pr.base, cancel)? {
        fetch(root, &pr.key, &pr.base, cancel)?;
    }
    ensure!(
        has_commit(root, &pr.head, cancel)?,
        "The PR changed while fetching. Refresh its details and open it again."
    );
    let merge_base = read(root, &["merge-base", &pr.base, &pr.head], cancel).context(
        "Cannot find the PR merge base. A shallow clone may need more history; fetch it and retry.",
    )?;
    sha(&merge_base)?;
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
            .args(["--name-status", "-z", &merge_base, &pr.head, "--"]),
        cancel,
    )?;
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
        merge_base,
        files,
    })
}

// Disable checkout filters for both materialization and cleanliness checks.
fn checkout_git(root: &Path, cancel: &Cancel) -> Result<Command> {
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
    root: PathBuf,
    pub path: PathBuf,
    directory: Option<tempfile::TempDir>,
}
impl Worktree {
    pub fn create(root: &Path, revision: &str, cancel: &Cancel) -> Result<Self> {
        sha(revision)?;
        let directory = tempfile::Builder::new().prefix("difu-review-").tempdir()?;
        let path = directory.path().join("source");
        let mut tree = Self {
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
        std::fs::remove_dir(tree.path.parent().context("Missing temporary parent")?)?;
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
