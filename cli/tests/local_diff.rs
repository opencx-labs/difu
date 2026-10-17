use anyhow::{Context, Result};
use difu::{
    local_diff::{Checkout, Comparison, status_letter},
    process::{self, Cancel},
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
    .map(|value| value.trim().to_owned())
}

#[test]
fn main_uses_working_changes_and_branches_use_the_true_merge_base() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git(root, &["init", "-b", "main"])?;
    fs::write(root.join("shared.txt"), "original\n")?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-m", "common ancestor"])?;
    let ancestor = git(root, &["rev-parse", "HEAD"])?;
    git(root, &["branch", "feature"])?;
    fs::write(root.join("main-only.txt"), "main advanced\n")?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-m", "main advances"])?;
    let main = git(root, &["rev-parse", "HEAD"])?;
    fs::write(root.join("shared.txt"), "staged\n")?;
    git(root, &["add", "shared.txt"])?;
    fs::write(root.join("shared.txt"), "unstaged\n")?;
    fs::write(root.join("untracked.txt"), "new\n")?;
    let index = fs::read(root.join(".git/index"))?;
    let status = git(root, &["status", "--porcelain=v1"])?;
    let checkout = Checkout::resolve(root, &Cancel::default())?;
    assert_eq!(checkout.branch, "main");
    assert_eq!(checkout.comparison, Comparison::WorkingTree { head: main });
    assert_eq!(fs::read(root.join(".git/index"))?, index);
    assert_eq!(git(root, &["status", "--porcelain=v1"])?, status);
    // Carry uncommitted edits onto a diverged feature branch; they must not
    // alter its committed comparison or move its base to main's latest commit.
    git(root, &["checkout", "feature"])?;
    fs::write(root.join("feature.txt"), "committed branch change\n")?;
    git(root, &["add", "feature.txt"])?;
    git(
        root,
        &["commit", "--only", "-m", "feature change", "feature.txt"],
    )?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let status = git(root, &["status", "--porcelain=v1"])?;
    fs::create_dir(root.join("nested"))?;
    let checkout = Checkout::resolve(&root.join("nested"), &Cancel::default())?;
    assert_eq!(checkout.root, root.canonicalize()?);
    assert_eq!(checkout.branch, "feature");
    assert_eq!(
        checkout.comparison,
        Comparison::Branch {
            head,
            merge_base: ancestor
        }
    );
    assert_eq!(git(root, &["status", "--porcelain=v1"])?, status);
    assert_eq!(fs::read_to_string(root.join("shared.txt"))?, "unstaged\n");
    Ok(())
}

#[test]
fn missing_main_does_not_fetch_or_silently_choose_another_base() -> Result<()> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), &["init", "-b", "feature"])?;
    git(dir.path(), &["commit", "--allow-empty", "-m", "initial"])?;
    let error = Checkout::resolve(dir.path(), &Cancel::default())
        .err()
        .context("expected missing main")?;
    assert!(error.to_string().contains("local main"));
    assert_eq!(status_letter("M"), 'M');
    assert_eq!(status_letter("??"), 'A');
    assert_eq!(status_letter("A"), 'A');
    assert_eq!(status_letter("D"), 'D');
    assert_eq!(status_letter("R100"), 'R');
    Ok(())
}

#[test]
fn local_patch_includes_all_main_working_changes_and_no_feature_working_changes() -> Result<()> {
    use difu::local_diff;
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git(root, &["init", "-b", "main"])?;
    for name in [
        "modified.txt",
        "deleted.txt",
        "old name.txt",
        "committed.txt",
    ] {
        fs::write(root.join(name), format!("original {name}\n"))?;
    }
    git(root, &["add", "."])?;
    git(root, &["commit", "-m", "base"])?;
    fs::write(root.join("modified.txt"), "staged\n")?;
    git(root, &["add", "modified.txt"])?;
    fs::write(root.join("modified.txt"), "unstaged\n")?;
    fs::remove_file(root.join("deleted.txt"))?;
    git(root, &["mv", "old name.txt", "new name.txt"])?;
    fs::write(root.join("new 🦀.txt"), "untracked\n")?;
    fs::write(root.join("empty.txt"), "")?;
    fs::write(root.join(".gitignore"), "ignored.txt\n")?;
    fs::write(root.join("ignored.txt"), "ignored\n")?;
    let index = fs::read(root.join(".git/index"))?;
    let refs = git(root, &["show-ref"])?;
    let cancel = Cancel::default();
    let checkout = Checkout::resolve(root, &cancel)?;
    let snapshot = local_diff::snapshot(&checkout, &cancel)?;
    let statuses: std::collections::BTreeMap<_, _> = snapshot
        .files
        .iter()
        .map(|f| (f.path.as_str(), status_letter(&f.status)))
        .collect();
    assert_eq!(statuses.get("modified.txt"), Some(&'M'));
    assert_eq!(statuses.get("deleted.txt"), Some(&'D'));
    assert_eq!(statuses.get("new name.txt"), Some(&'R'));
    assert_eq!(statuses.get("new 🦀.txt"), Some(&'A'));
    assert_eq!(statuses.get("empty.txt"), Some(&'A'));
    assert!(!statuses.contains_key("ignored.txt"));
    assert!(!statuses.contains_key("committed.txt"));
    let modified = snapshot
        .files
        .iter()
        .find(|f| f.path == "modified.txt")
        .context("modified file")?;
    assert!(
        modified
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.text == "unstaged")
    );
    assert_eq!(fs::read(root.join(".git/index"))?, index);
    assert_eq!(git(root, &["show-ref"])?, refs);
    let mut app = difu::app::App::new(
        difu::storage::Storage {
            config: root.join(".test-config/config.json"),
            cache: root.join(".test-cache"),
        },
        Default::default(),
    );
    app.open_local(root.into(), false);
    let started = std::time::Instant::now();
    while app.local.loading {
        app.tick();
        anyhow::ensure!(started.elapsed().as_secs() < 10, "Local UI timed out");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.view, difu::app::View::Diff);
    let review = app.review().context("local review")?;
    assert!(review.generation.is_none() && review.guide.is_none());
    assert!(!review.polling && !review.revision_polling && !review.interaction.github_loading);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40))?;
    terminal.draw(|f| difu::ui::draw(f, &mut app))?;
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("M modified.txt"));
    assert!(!screen.contains("Viewed on GitHub"));
    app.action(difu::app::Action::SetView(difu::app::View::Guide));
    assert!(app.review().context("review")?.generation.is_none());
    app.workflow_action(difu::workflow::WAction::Controls);
    assert!(app.modal.is_none());
    app.shutdown();
    git(root, &["checkout", "-b", "feature"])?;
    fs::write(root.join("feature.txt"), "branch change\n")?;
    git(root, &["add", "feature.txt"])?;
    git(
        root,
        &["commit", "--only", "-m", "feature change", "feature.txt"],
    )?;
    let checkout = Checkout::resolve(root, &cancel)?;
    let snapshot = local_diff::snapshot(&checkout, &cancel)?;
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(
        snapshot.files.first().context("feature file")?.path,
        "feature.txt"
    );
    Ok(())
}

#[test]
fn discovery_skips_dependencies_symlinks_and_nested_repositories_and_lists_worktrees() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    for name in [
        "project",
        "node_modules/dependency",
        ".hidden/secret",
        "target/generated",
        "project/nested",
    ] {
        let path = root.join(name);
        fs::create_dir_all(&path)?;
        git(&path, &["init", "-b", "main"])?;
    }
    let project = root.join("project");
    git(&project, &["commit", "--allow-empty", "-m", "initial"])?;
    let other = tempfile::tempdir()?;
    git(other.path(), &["init", "-b", "main"])?;
    std::os::unix::fs::symlink(other.path(), root.join("linked"))?;
    std::os::unix::fs::symlink(root, root.join("loop"))?;
    let cancel = Cancel::default();
    let discovered = difu::local_diff::discover(&[root.into(), project.clone()], &cancel)?;
    assert_eq!(discovered, vec![project.canonicalize()?]);
    let worktree = root.join("outside-worktree");
    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().context("worktree path")?,
        ],
    )?;
    let paths = difu::local_diff::worktrees(&project, &cancel)?;
    assert!(paths.contains(&project.canonicalize()?));
    assert!(paths.contains(&worktree.canonicalize()?));
    let config = difu::storage::Config {
        local_diff_roots: vec![root.into()],
        local_diff_repositories: std::collections::BTreeSet::from([project]),
        ..Default::default()
    };
    let storage = difu::storage::Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    storage.save_config(&config)?;
    assert_eq!(
        storage.load_config()?.local_diff_repositories,
        config.local_diff_repositories
    );
    Ok(())
}

#[test]
fn diffs_setup_tracks_selected_repos_and_survives_reopening() -> Result<()> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use difu::{
        app::{Action, App, local},
        model::InboxTab,
        storage::{Config, Storage},
    };
    fn wait(app: &mut App) -> Result<()> {
        let start = std::time::Instant::now();
        while app.local.loading {
            app.tick();
            anyhow::ensure!(start.elapsed().as_secs() < 10, "setup timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::ensure!(
            app.local.error.is_none(),
            "setup failed: {:?}",
            app.local.error
        );
        Ok(())
    }
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("base directory");
    let repo = base.join("selected repo");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["commit", "--allow-empty", "-m", "base"])?;
    let worktree = temp.path().join("worktree outside base");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().context("path")?,
        ],
    )?;
    let storage = Storage {
        config: temp.path().join("config.json"),
        cache: temp.path().join("cache"),
    };
    let mut app = App::new(storage.clone(), Config::default());
    app.action(Action::SetInbox(InboxTab::Diffs));
    assert!(matches!(app.local.setup, Some(local::Setup::Roots(_))));
    app.paste(base.display().to_string());
    app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    wait(&mut app)?;
    assert!(
        matches!(&app.local.setup, Some(local::Setup::Select { repos, chosen, .. }) if repos.len() == 1 && chosen.is_empty())
    );
    app.key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait(&mut app)?;
    assert_eq!(app.local.entries.len(), 2);
    assert!(app.local.setup.is_none());
    app.shutdown();
    let mut reopened = App::new(storage.clone(), storage.load_config()?);
    reopened.action(Action::SetInbox(InboxTab::Diffs));
    wait(&mut reopened)?;
    assert_eq!(reopened.local.entries.len(), 2);
    assert!(reopened.local.setup.is_none());
    reopened.shutdown();
    Ok(())
}
