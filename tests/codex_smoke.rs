//! Optional live integration check. Uses synthetic code, never a user's checkout.
use anyhow::{Context, Result};
use difu::{
    codex,
    model::{ModelChoice, PrDetail, PrKey},
    process::{self, Cancel},
    repo,
    storage::Storage,
};
use std::{fs, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> Result<String> {
    process::checked(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Difu Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args),
        &Cancel::default(),
    )
    .map(|s| s.trim().to_owned())
}

#[test]
#[ignore = "uses installed Codex, saved login, and one Luna Low generation"]
fn generates_a_real_guide_for_synthetic_code() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    git(root, &["init"])?;
    fs::write(
        root.join("main.rs"),
        format!(
            "fn main() {{ println!(\"Hello\"); }}\n{}fn allowed() -> bool {{ false }}\n",
            "// unchanged context\n".repeat(12)
        ),
    )?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-m", "base"])?;
    let base = git(root, &["rev-parse", "HEAD"])?;
    fs::write(
        root.join("main.rs"),
        format!(
            "fn main() {{ println!(\"Hello, world!\"); }}\n{}fn allowed() -> bool {{ true }}\n",
            "// unchanged context\n".repeat(12)
        ),
    )?;
    git(root, &["commit", "-am", "greet the world"])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let pr = PrDetail {
        requested_reviewers: Vec::new(),
        requested_teams: Vec::new(),
        key: PrKey {
            owner: "difu-test".into(),
            repo: "synthetic".into(),
            number: 1,
        },
        title: "Expand the greeting".into(),
        body: "Say hello to the world and enable access.".into(),
        author: "test".into(),
        head,
        base,
        head_branch: "feature".into(),
        base_branch: "main".into(),
        state: "open".into(),
        additions: 2,
        deletions: 2,
        changed_files: 1,
    };
    let cancel = Cancel::default();
    let snapshot = repo::snapshot(root, &pr, &cancel)?;
    assert_eq!(snapshot.units().count(), 2);
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.into(),
    };
    let guide = codex::generate(
        root,
        &pr,
        &snapshot,
        &ModelChoice {
            effort: "low".into(),
            ..ModelChoice::default()
        },
        &storage,
        &cancel,
        |activity| eprintln!("{activity}"),
    )?;
    guide.validate(&snapshot)?;
    assert!(
        !guide
            .chapters
            .first()
            .context("No guide chapter")?
            .explanation
            .is_empty()
    );
    assert_eq!(
        git(root, &["worktree", "list", "--porcelain"])?
            .matches("worktree ")
            .count(),
        1
    );
    Ok(())
}
