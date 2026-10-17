use super::{Job, Session};
use crate::{
    process::{self, Cancel},
    repo,
    storage::Storage,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Git lists the original checkout first, including when called from a linked worktree.
pub(super) fn repository(path: &Path) -> Result<PathBuf> {
    let root = super::workspace::repository(path, &Cancel::default())?;
    let trees = process::checked(
        repo::git(&root).args(["worktree", "list", "--porcelain", "-z"]),
        &Cancel::default(),
    )?;
    let original = trees
        .split('\0')
        .find_map(|entry| entry.strip_prefix("worktree "))
        .context("Cannot find the original repository")?;
    PathBuf::from(original)
        .canonicalize()
        .context("Cannot resolve the original repository")
}

pub(super) fn load(storage: &Storage, path: &Path) -> Result<(PathBuf, String)> {
    let root = repository(path)?;
    let mut config = storage.load_config()?;
    let rules = config.repository_rules.remove(&root).unwrap_or_default();
    Ok((root, rules))
}

pub(super) fn capture(storage: &Storage, session: &mut Session) -> Result<()> {
    if let Job::Coding(launch) = &session.job {
        session.repository_rules = load(storage, &launch.repository)?.1;
    }
    Ok(())
}

pub(super) fn instructions(session: &Session, base: String) -> String {
    if session.repository_rules.trim().is_empty() {
        base
    } else {
        format!("{base}\n\nRepository rules:\n{}", session.repository_rules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Launch;

    fn git(root: &Path, args: &[&str]) -> Result<()> {
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
        )?;
        Ok(())
    }

    #[test]
    fn worktrees_share_rules_and_sessions_keep_their_initial_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("repository");
        std::fs::create_dir(&root)?;
        git(&root, &["init", "-b", "main"])?;
        git(&root, &["commit", "--allow-empty", "-m", "initial"])?;
        let tree = dir.path().join("linked tree");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--detach",
                tree.to_str().context("path")?,
            ],
        )?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let root = root.canonicalize()?;
        let mut config = storage.load_config()?;
        config.repository_rules.insert(
            root.clone(),
            "Use the repository's service layer.\nPreserve billing behavior.".into(),
        );
        storage.save_config(&config)?;
        assert_eq!(repository(&tree)?, root);
        let mut session = Session::new(
            "test".into(),
            Job::Coding(Launch {
                repository: tree,
                isolated: true,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        capture(&storage, &mut session)?;
        config
            .repository_rules
            .insert(root.clone(), "Updated rules".into());
        storage.save_config(&config)?;
        let restored: Session = serde_json::from_value(serde_json::to_value(&session)?)?;
        let prompt = instructions(&restored, "Base instructions".into());
        assert!(prompt.contains("Preserve billing behavior."));
        assert!(!prompt.contains("Updated rules"));
        assert_eq!(load(&storage, &root)?.1, "Updated rules");
        let mut legacy = serde_json::to_value(&session)?;
        legacy
            .as_object_mut()
            .context("session")?
            .remove("repository_rules");
        let legacy: Session = serde_json::from_value(legacy)?;
        assert_eq!(
            instructions(&legacy, "Base instructions".into()),
            "Base instructions"
        );
        Ok(())
    }
}
