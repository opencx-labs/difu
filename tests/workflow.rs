use anyhow::{Context, Result, ensure};
use difu::{
    app::{Action, App, View},
    model::{InboxTab, PrKey, PrState},
    process::{self, Cancel},
    storage::{Config, Storage},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn git(path: &Path, args: &[&str]) -> Result<String> {
    process::checked(
        Command::new("git")
            .arg("-C")
            .arg(path)
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
fn wait(app: &mut App, ready: impl Fn(&App) -> bool) -> Result<()> {
    let start = Instant::now();
    while !ready(app) {
        ensure!(
            start.elapsed() < Duration::from_secs(15),
            "Workflow timed out: {} {:?}",
            app.notice,
            app.review().and_then(|r| r.guide_error.as_ref())
        );
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn render(app: &mut App, width: u16) -> Result<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 36))?;
    terminal.draw(|f| difu::ui::draw(f, app))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>())
}

#[test]
fn scripted_workflow() -> Result<()> {
    if let Ok(fixture) = std::env::var("DIFU_TEST_FIXTURE") {
        return exercise(Path::new(&fixture));
    }
    // The subprocess owns PATH changes; parallel tests retain their environment.
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let clone = root.join("clone");
    fs::create_dir(&clone)?;
    git(&clone, &["init"])?;
    git(
        &clone,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/project.git",
        ],
    )?;
    fs::write(clone.join("main.rs"), "fn main() {\n    old();\n}\n")?;
    git(&clone, &["add", "."])?;
    git(&clone, &["commit", "-m", "base"])?;
    let base = git(&clone, &["rev-parse", "HEAD"])?;
    fs::write(clone.join("main.rs"), "fn main() {\n    new();\n}\n")?;
    git(&clone, &["commit", "-am", "head"])?;
    let head = git(&clone, &["rev-parse", "HEAD"])?;
    fs::write(clone.join("main.rs"), "precious uncommitted work\n")?;
    fs::write(
        root.join("revisions.json"),
        serde_json::to_vec(&serde_json::json!({"head":head,"base":base}))?,
    )?;
    for (name, script) in [
        ("gh", include_str!("fixtures/gh.py")),
        ("codex", include_str!("fixtures/codex.py")),
    ] {
        let path = root.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let path = std::env::join_paths(std::iter::once(root.to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH unavailable")?),
    ))?;
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", "scripted_workflow", "--nocapture"])
        .env("DIFU_TEST_FIXTURE", root)
        .env("PATH", path)
        .output()?;
    ensure!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(clone.join("main.rs"))?,
        "precious uncommitted work\n"
    );
    assert_eq!(
        git(&clone, &["worktree", "list", "--porcelain"])?
            .matches("worktree ")
            .count(),
        1
    );
    Ok(())
}

fn exercise(root: &Path) -> Result<()> {
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.to_owned(),
    };
    let mut config = Config::default();
    config
        .repositories
        .insert("example/project".into(), root.join("clone"));
    let mut app = App::new(storage.clone(), config.clone());
    app.start(None);
    wait(&mut app, |a| {
        a.review()
            .is_some_and(|r| !r.timeline.is_empty() && !r.checks.is_empty())
    })?;
    assert!(
        app.review()
            .is_some_and(|r| r.generation.is_none() && r.snapshot.is_none())
    );
    let overview = render(&mut app, 160)?;
    assert!(overview.contains("Describe the behavior"));
    assert!(overview.contains("ACTIVITY"));
    assert!(overview.contains("CHECKS"));
    assert!(overview.contains("1 Review requests"));
    assert!(overview.contains("2 Authored"));
    assert!(!overview.contains("2 Guide"));
    assert!(
        app.review()
            .is_some_and(|r| r.checks.first().is_some_and(|c| c.state == "pending"))
    );
    app.action(Action::OpenPr);
    wait(&mut app, |a| a.review().is_some_and(|r| r.guide.is_some()))?;
    // Revision checks are lightweight, spaced by 30 seconds, and pin open code.
    let revision_file = root.join("revisions.json");
    let original_revisions = fs::read(&revision_file)?;
    let original_head = app
        .review()
        .and_then(|r| r.snapshot.as_ref())
        .context("Missing snapshot")?
        .head
        .clone();
    let review = app
        .reviews
        .get_mut("example/project#1")
        .context("Missing review")?;
    review.revision_poll_at = Instant::now().checked_sub(Duration::from_secs(29));
    app.tick();
    assert!(!root.join("revision-polls").exists());
    let review = app
        .reviews
        .get_mut("example/project#1")
        .context("Missing review")?;
    review.revision_poll_at = Instant::now().checked_sub(Duration::from_secs(31));
    app.tick();
    wait(&mut app, |a| {
        a.review().is_some_and(|r| !r.revision_polling)
    })?;
    assert_eq!(fs::read_to_string(root.join("revision-polls"))?, "poll\n");
    assert!(app.review().is_some_and(|r| r.newer.is_none()));
    app.tick();
    assert_eq!(fs::read_to_string(root.join("revision-polls"))?, "poll\n");
    let mut changed: serde_json::Value = serde_json::from_slice(&original_revisions)?;
    *changed.get_mut("head").context("Missing fixture head")? =
        serde_json::json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    fs::write(&revision_file, serde_json::to_vec(&changed)?)?;
    app.reviews
        .get_mut("example/project#1")
        .context("Missing review")?
        .revision_poll_at = Instant::now().checked_sub(Duration::from_secs(31));
    app.tick();
    wait(&mut app, |a| {
        a.review().is_some_and(|r| !r.revision_polling)
    })?;
    assert!(app.review().is_some_and(|r| r.newer.is_some()
        && r.guide.is_some()
        && r.snapshot.as_ref().is_some_and(|s| s.head == original_head)));
    app.action(Action::Refresh);
    wait(&mut app, |a| a.review().is_some_and(|r| !r.preparing))?;
    assert!(app.review().is_some_and(|r| r.preparation_failed
        && r.guide.is_some()
        && r.snapshot.as_ref().is_some_and(|s| s.head == original_head)));
    assert!(render(&mut app, 180)?.contains("not available locally"));
    app.action(Action::Regenerate);
    wait(&mut app, |a| a.review().is_some_and(|r| !r.preparing))?;
    assert_eq!(fs::read_to_string(root.join("turns"))?, "turn\n");
    fs::write(&revision_file, original_revisions)?;
    let wide = render(&mut app, 180)?;
    assert!(!app.home);
    assert_eq!(app.view, View::Guide);
    assert!(wide.contains("1 Overview"));
    assert!(wide.contains("2 Guide"));
    assert!(wide.contains("Use the new behavior"));
    assert!(app.document.as_ref().is_some_and(|d| d.guide_columns));
    render(&mut app, 80)?;
    assert!(app.document.as_ref().is_some_and(|d| !d.guide_columns));
    assert!(!app.config.unified);
    render(&mut app, 180)?;
    assert!(app.document.as_ref().is_some_and(|d| d.guide_columns));
    app.action(Action::SetView(View::Diff));
    assert!(render(&mut app, 140)?.contains("main.rs"));
    app.action(Action::Models);
    wait(&mut app, |a| !a.models_loading)?;
    assert_eq!(app.model_options("luna").len(), 1);
    assert_eq!(app.model_options("sol").len(), 2);
    app.modal = None;
    app.action(Action::SetView(View::Overview));
    assert!(!app.home);
    assert!(!render(&mut app, 160)?.contains("REVIEW REQUESTS"));
    app.key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(app.home);
    assert!(!app.quit);
    wait(&mut app, |a| !a.inbox_loading)?;

    render(&mut app, 160)?;
    let authored_tab = app
        .hits
        .iter()
        .find_map(|(rect, action)| {
            matches!(action, Action::SetInbox(InboxTab::Authored)).then_some(*rect)
        })
        .context("Missing clickable Authored tab")?;
    app.mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: authored_tab.x,
        row: authored_tab.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    wait(&mut app, |a| !a.inbox_loading)?;
    assert_eq!(app.inbox.first().context("No authored PR")?.key.number, 2);
    assert_eq!(app.state(), PrState::Open);
    for state in [PrState::Merged, PrState::Closed, PrState::All] {
        app.action(Action::SetState(state));
        wait(&mut app, |a| !a.inbox_loading)?;
        assert!(app.inbox_error.is_none());
    }
    app.action(Action::SetInbox(InboxTab::Repositories));
    wait(&mut app, |a| !a.inbox_loading && !a.repositories_loading)?;
    assert!(app.inbox.is_empty());
    assert_eq!(app.repository_options.len(), 2);
    assert!(render(&mut app, 160)?.contains("Choose your repository whitelist"));
    app.paste("second".into());
    let filtered = render(&mut app, 160)?;
    assert!(filtered.contains("example/second"));
    assert!(!filtered.contains("example/project"));
    for _ in 0..6 {
        app.key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    app.action(Action::ToggleRepository("example/project".into()));
    app.action(Action::ToggleRepository("example/second".into()));
    app.action(Action::SaveRepositories);
    wait(&mut app, |a| !a.inbox_loading)?;
    assert_eq!(app.inbox.len(), 2);
    app.action(Action::Repositories(false));
    app.action(Action::ToggleRepository("example/project".into()));
    app.action(Action::SaveRepositories);
    wait(&mut app, |a| !a.inbox_loading)?;
    assert_eq!(app.inbox.len(), 1);
    assert_eq!(
        app.inbox
            .first()
            .context("No repository PR")?
            .key
            .repository(),
        "example/second"
    );
    assert_eq!(
        storage
            .load_config()?
            .review_repositories
            .get("example/project"),
        Some(&false)
    );
    assert_eq!(
        storage
            .load_config()?
            .review_repositories
            .get("example/second"),
        Some(&true)
    );
    app.action(Action::Repositories(false));
    app.action(Action::AllRepositories);
    // Cancelling must preserve the saved active filters.
    app.key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        app.config.review_repositories.get("example/project"),
        Some(&false)
    );
    app.action(Action::Repositories(false));
    app.action(Action::ToggleRepository("example/second".into()));
    app.action(Action::SaveRepositories);
    wait(&mut app, |a| !a.inbox_loading)?;
    assert!(app.inbox.is_empty());
    let searches = fs::read_to_string(root.join("searches.jsonl"))?;
    assert!(searches.contains("--author=@me"));
    assert!(searches.contains("--merged=false"));
    assert!(searches.contains("--merged\""));
    app.shutdown();
    // Old releases' cache entries migrate without another Codex turn.
    let review = app
        .reviews
        .get("example/project#1")
        .context("Missing cached review")?;
    let pr = review.detail.as_ref().context("Missing PR")?;
    let snapshot = review.snapshot.as_ref().context("Missing snapshot")?;
    let guide = review.guide.as_ref().context("Missing guide")?;
    let cache_key = difu::codex::cache_key(pr, snapshot, &config.model)?;
    storage.save_guide(
        &difu::codex::legacy_cache_key(pr, snapshot, &config.model)?,
        guide,
    )?;
    fs::remove_file(storage.cache.join(format!("{cache_key}.json")))?;
    let mut reopened = App::new(storage.clone(), config.clone());
    reopened.start(Some(PrKey::from_url(
        "https://github.com/example/project/pull/1",
    )?));
    wait(&mut reopened, |a| {
        a.review().is_some_and(|r| r.guide.is_some())
    })?;
    assert_eq!(fs::read_to_string(root.join("turns"))?, "turn\n");
    reopened.shutdown();
    assert!(storage.load_guide(&cache_key)?.is_some());
    // Rewriting only the commit message preserves code and reuses the guide.
    git(
        &root.join("clone"),
        &["commit", "--amend", "-m", "same code, new message"],
    )?;
    let rewritten = git(&root.join("clone"), &["rev-parse", "HEAD"])?;
    let mut revisions: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("revisions.json"))?)?;
    *revisions.get_mut("head").context("Missing head")? = serde_json::json!(rewritten);
    fs::write(root.join("revisions.json"), serde_json::to_vec(&revisions)?)?;
    let mut rewritten_app = App::new(storage, config);
    rewritten_app.start(Some(PrKey::from_url(
        "https://github.com/example/project/pull/1",
    )?));
    wait(&mut rewritten_app, |a| {
        a.review().is_some_and(|r| r.guide.is_some())
    })?;
    assert_eq!(fs::read_to_string(root.join("turns"))?, "turn\n");
    rewritten_app.shutdown();
    Ok(())
}
