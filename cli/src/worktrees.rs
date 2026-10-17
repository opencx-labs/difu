//! Inventory and deletion of positively identified difu worktrees.
use crate::{
    process::{self, Cancel},
    repo, storage,
};
use anyhow::{Context, Result, ensure};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Marker {
    kind: String,
    root: PathBuf,
    path: PathBuf,
}
pub struct Lease {
    _lock: Flock<File>,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub directory: PathBuf,
    pub path: PathBuf,
    pub reason: Option<String>,
}
fn lock(directory: &Path) -> Result<Lease> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(directory.join("difu.lock"))?;
    let lock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, e)| anyhow::anyhow!("Active worktree: {e}"))?;
    Ok(Lease { _lock: lock })
}
pub fn register(directory: &Path, root: &Path, path: &Path) -> Result<Lease> {
    let lease = lock(directory)?;
    let marker = Marker {
        kind: "difu-worktree-v1".into(),
        root: root.canonicalize()?,
        path: path.into(),
    };
    storage::atomic_json(&directory.join("difu-owner.json"), &marker)?;
    Ok(lease)
}
fn marker(directory: &Path) -> Result<Marker> {
    ensure!(
        !fs::symlink_metadata(directory)?.file_type().is_symlink(),
        "Symlink directory is protected"
    );
    let owner = directory.join("difu-owner.json");
    ensure!(
        !fs::symlink_metadata(&owner)?.file_type().is_symlink(),
        "Symlink marker is protected"
    );
    let marker: Marker = serde_json::from_slice(&fs::read(owner)?)?;
    ensure!(
        marker.kind == "difu-worktree-v1" && marker.path == directory.join("source"),
        "Unverified worktree ownership"
    );
    Ok(marker)
}
fn inspect(marker: &Marker, cancel: &Cancel) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(&marker.path)?.file_type().is_symlink(),
        "Symlink worktree is protected"
    );
    let list = process::checked(
        repo::git(&marker.root).args(["worktree", "list", "--porcelain", "-z"]),
        cancel,
    )?;
    let wanted = format!("worktree {}", marker.path.canonicalize()?.to_string_lossy());
    let mut found = false;
    let mut locked = false;
    for value in list.split('\0') {
        if value.starts_with("worktree ") {
            if found {
                break;
            }
            found = value == wanted;
        }
        if found && value.starts_with("locked") {
            locked = true;
        }
    }
    ensure!(
        found,
        "Worktree registration does not match its ownership marker"
    );
    ensure!(!locked, "Git-locked worktree is protected");
    let status = process::checked(
        repo::checkout_git(&marker.path, cancel)?.args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored",
        ]),
        cancel,
    )?;
    ensure!(
        status.trim().is_empty(),
        "Modified or untracked files are protected"
    );
    Ok(())
}
pub fn list(cancel: &Cancel) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(std::env::temp_dir())? {
        cancel.check()?;
        let item = item?;
        if !item
            .file_name()
            .to_string_lossy()
            .starts_with("difu-review-")
            || !item.file_type()?.is_dir()
        {
            continue;
        }
        let directory = item.path();
        let status = (|| -> Result<()> {
            let marker =
                marker(&directory).context("Legacy or unverified worktree is protected")?;
            let _lease = lock(&directory)?;
            inspect(&marker, cancel)
        })();
        entries.push(Entry {
            path: directory.join("source"),
            directory,
            reason: status.err().map(|e| format!("{e:#}")),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}
pub fn delete(directory: &Path, cancel: &Cancel) -> Result<()> {
    ensure!(
        directory.parent() == Some(std::env::temp_dir().as_path()),
        "Not a difu temporary directory"
    );
    ensure!(
        directory
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("difu-review-")),
        "Invalid worktree directory"
    );
    let marker = marker(directory)?;
    let _lease = lock(directory)?;
    inspect(&marker, cancel)?;
    process::checked(
        repo::checkout_git(&marker.root, cancel)?
            .args(["worktree", "remove"])
            .arg(&marker.path),
        cancel,
    )?;
    // Remove only our metadata, never recursively delete an unknown directory.
    fs::remove_file(directory.join("difu-owner.json"))?;
    fs::remove_file(directory.join("difu.lock"))?;
    fs::remove_dir(directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_protects_active_dirty_and_git_locked_worktrees() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cancel = Cancel::default();
        let git = |args: &[&str]| {
            process::checked(
                repo::git(root.path())
                    .args([
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .args(args),
                &cancel,
            )
        };
        git(&["init"])?;
        fs::write(root.path().join("file"), "committed\n")?;
        git(&["add", "."])?;
        git(&["commit", "-m", "initial"])?;
        let directory = tempfile::Builder::new().prefix("difu-review-").tempdir()?;
        let path = directory.path().join("source");
        let lease = register(directory.path(), root.path(), &path)?;
        process::checked(
            repo::git(root.path())
                .args(["worktree", "add", "--detach"])
                .arg(&path)
                .arg("HEAD"),
            &cancel,
        )?;
        assert!(delete(directory.path(), &cancel).is_err());
        drop(lease);
        fs::write(path.join("file"), "precious edits")?;
        assert!(delete(directory.path(), &cancel).is_err());
        assert_eq!(fs::read_to_string(path.join("file"))?, "precious edits");
        fs::write(path.join("file"), "committed\n")?;
        process::checked(
            repo::git(root.path()).args(["worktree", "lock"]).arg(&path),
            &cancel,
        )?;
        assert!(delete(directory.path(), &cancel).is_err());
        process::checked(
            repo::git(root.path())
                .args(["worktree", "unlock"])
                .arg(&path),
            &cancel,
        )?;
        let marker = marker(directory.path())?;
        inspect(&marker, &cancel)?;
        delete(directory.path(), &cancel)?;
        assert!(!path.exists());
        assert!(root.path().join("file").exists());
        Ok(())
    }
}
