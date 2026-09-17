use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    git(root, &["clone", "--bare", "clone", "remote"])?;
    let real_git = std::env::split_paths(&std::env::var_os("PATH").context("Missing PATH")?)
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .context("Missing real Git")?;
    for (name, script) in [
        ("git", include_str!("fixtures/git.py")),
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
        .env("DIFU_TEST_REAL_GIT", real_git)
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
    exercise_checks(root)?;
    exercise_mention_shortcut(root)?;
    exercise_writes(root)?;
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
    assert!(overview.contains("2026-09-10"));
    assert!(overview.contains("1 file"));

    assert!(overview.contains("1 Review requests"));
    assert!(overview.contains("2 Authored"));
    assert!(!overview.contains("2 Guide"));
    // Card layout reflows the activity into a taller, bounded column. Checks
    // and their click targets must remain reachable after scrolling and resize.
    for width in [80, 240] {
        render(&mut app, width)?;
        if width == 240 {
            assert_eq!(app.content_rect.width, 110);
        }
        let doc = app.document.as_ref().context("Missing overview")?;
        assert!(doc.rows.iter().all(|row| {
            row.right
                .spans
                .iter()
                .map(ratatui::text::Span::width)
                .sum::<usize>()
                <= usize::from(doc.width)
        }));
        app.scroll = doc.max_scroll(app.viewport);
        assert!(render(&mut app, width)?.contains("CHECKS"));
        let (rect, _) = app
            .hits
            .iter()
            .find(|(_, action)| matches!(action,Action::Link(url) if url.ends_with("/checks")))
            .context("Missing check link")?;
        assert!(rect.x >= app.content_rect.x && rect.right() <= app.content_rect.right());
        assert!(rect.y >= app.content_rect.y && rect.bottom() <= app.content_rect.bottom());
        app.scroll = 0;
    }
    assert!(
        app.review()
            .is_some_and(|r| r.checks.first().is_some_and(|c| c.state == "pending"))
    );
    app.action(Action::OpenPr);
    assert_eq!(app.focus, difu::app::Focus::Content);
    wait(&mut app, |a| a.review().is_some_and(|r| r.guide.is_some()))?;
    let guide = app
        .review()
        .and_then(|r| r.guide.as_ref())
        .context("Missing guide")?;
    assert_eq!(guide.chapters.len(), 2);
    assert!(guide.chapters.iter().all(|c| c.hunks == ["f0-h0"]));
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
    app.key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    wait(&mut app, |a| a.review().is_some_and(|r| !r.preparing))?;
    assert!(app.review().is_some_and(|r| r.preparation_failed
        && r.guide.is_some()
        && r.snapshot.as_ref().is_some_and(|s| s.head == original_head)));
    assert!(render(&mut app, 180)?.contains("Automatic PR sync failed"));
    app.key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
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
    app.key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
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
        app.key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(app.state(), state);
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
    app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
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
    app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
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
    app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    app.action(Action::ToggleRepository("example/second".into()));
    app.action(Action::SaveRepositories);
    wait(&mut app, |a| !a.inbox_loading)?;
    assert!(app.inbox.is_empty());
    let searches = fs::read_to_string(root.join("searches.jsonl"))?;
    assert!(searches.contains("--author=@me"));
    assert!(searches.contains("--merged=false"));
    assert!(searches.contains("--merged\""));
    app.shutdown();
    fs::write(root.join("updated-title"), "")?;
    let mut cached = App::new(storage.clone(), config.clone());
    cached.start(None);
    assert!(
        !cached.inbox.is_empty(),
        "Cached list must be available before processing replies"
    );
    assert!(cached.inbox_loading);
    assert_ne!(
        cached.inbox.first().map(|p| p.title.as_str()),
        Some("Fresh title from GitHub")
    );
    assert!(render(&mut cached, 160)?.contains("Showing cached PRs"));
    wait(&mut cached, |a| !a.inbox_loading)?;
    assert_eq!(
        cached.inbox.first().map(|p| p.title.as_str()),
        Some("Fresh title from GitHub")
    );
    cached.shutdown();
    fs::remove_file(root.join("updated-title"))?;
    fs::write(root.join("fail-search"), "")?;
    let mut offline = App::new(storage.clone(), config.clone());
    offline.start(None);
    let cached_ids: Vec<_> = offline.inbox.iter().map(|p| p.key.id()).collect();
    wait(&mut offline, |a| !a.inbox_loading)?;
    assert!(offline.inbox_error.is_some());
    assert_eq!(
        offline.inbox.iter().map(|p| p.key.id()).collect::<Vec<_>>(),
        cached_ids
    );
    assert!(render(&mut offline, 160)?.contains("Refresh failed"));
    offline.shutdown();
    fs::remove_file(root.join("fail-search"))?;
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
    let restored = storage
        .load_guide(&cache_key)?
        .context("Missing restored guide")?;
    assert_eq!(restored.chapters.len(), 2);
    assert!(restored.chapters.iter().all(|c| c.hunks == ["f0-h0"]));
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
    let mut rewritten_app = App::new(storage.clone(), config.clone());
    rewritten_app.start(Some(PrKey::from_url(
        "https://github.com/example/project/pull/1",
    )?));
    wait(&mut rewritten_app, |a| {
        a.review().is_some_and(|r| r.guide.is_some())
    })?;
    assert_eq!(fs::read_to_string(root.join("turns"))?, "turn\n");
    rewritten_app.shutdown();
    // Missing remote commits sync automatically, preserve the working branch,
    // and remain named so subsequent fetches advertise the downloaded history.
    let original_checkout = git(&root.join("clone"), &["rev-parse", "HEAD"])?;
    let remote = root.join("remote");
    let old_base = revisions
        .get("base")
        .and_then(serde_json::Value::as_str)
        .context("Missing base")?;
    let base_tree = git(&remote, &["rev-parse", &format!("{old_base}^{{tree}}")])?;
    let remote_base = git(
        &remote,
        &[
            "commit-tree",
            &base_tree,
            "-p",
            old_base,
            "-m",
            "remote base metadata",
        ],
    )?;
    let tree = git(&remote, &["rev-parse", "HEAD^{tree}"])?;
    let remote_commit = git(
        &remote,
        &[
            "commit-tree",
            &tree,
            "-p",
            &remote_base,
            "-m",
            "remote message-only commit",
        ],
    )?;
    git(
        &remote,
        &["update-ref", "refs/heads/feature", &remote_commit],
    )?;
    *revisions.get_mut("head").context("Missing head")? = serde_json::json!(remote_commit);
    *revisions.get_mut("base").context("Missing base")? = serde_json::json!(remote_base);
    fs::write(root.join("revisions.json"), serde_json::to_vec(&revisions)?)?;
    let fetches_before = fs::read_to_string(root.join("fetches.jsonl"))?
        .lines()
        .count();
    let mut synced = App::new(storage.clone(), config.clone());
    synced.start(Some(PrKey::from_url(
        "https://github.com/example/project/pull/1",
    )?));
    wait(&mut synced, |a| {
        a.review().is_some_and(|r| r.guide.is_some())
    })?;
    assert_eq!(
        fs::read_to_string(root.join("fetches.jsonl"))?
            .lines()
            .count(),
        fetches_before + 1
    );
    assert_eq!(
        git(
            &root.join("clone"),
            &["rev-parse", "refs/difu/example/project/pr/1/head"]
        )?,
        remote_commit
    );
    assert_eq!(
        git(&root.join("clone"), &["rev-parse", "HEAD"])?,
        original_checkout
    );
    assert_eq!(fs::read_to_string(root.join("turns"))?, "turn\n");
    synced.shutdown();
    let mut already_local = App::new(storage, config);
    already_local.start(Some(PrKey::from_url(
        "https://github.com/example/project/pull/1",
    )?));
    wait(&mut already_local, |a| {
        a.review().is_some_and(|r| r.guide.is_some())
    })?;
    assert_eq!(
        fs::read_to_string(root.join("fetches.jsonl"))?
            .lines()
            .count(),
        fetches_before + 1
    );
    already_local.shutdown();
    Ok(())
}

fn exercise_writes(root: &Path) -> Result<()> {
    use difu::review::{self, Anchor, Operation, Side};
    let cancel = Cancel::default();
    let key = PrKey {
        owner: "example".into(),
        repo: "project".into(),
        number: 1,
    };
    let revisions: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("revisions.json"))?)?;
    let head = revisions
        .get("head")
        .and_then(serde_json::Value::as_str)
        .context("Missing head")?;
    let anchor = Anchor {
        path: "main.rs".into(),
        side: Side::Right,
        start: 1,
        end: 3,
    };
    let pending = Operation::Comment {
        anchor: anchor.clone(),
        body: "Please explain @alice 🦀".into(),
        pending: true,
    };
    assert!(review::execute(&key, "outdated", &pending, &cancel).is_err());
    assert!(!root.join("writes.jsonl").exists());
    review::execute(&key, head, &pending, &cancel)?;
    assert!(review::state(&key, &cancel)?.pending.is_some());
    review::execute(&key, head, &pending, &cancel)?;
    let writes = fs::read_to_string(root.join("writes.jsonl"))?;
    assert_eq!(writes.matches("AddPullRequestReviewInput").count(), 1);
    assert_eq!(writes.matches("AddPullRequestReviewThreadInput").count(), 1);
    for line in writes.lines() {
        let data: serde_json::Value = serde_json::from_str(line)?;
        let input = data.pointer("/variables/input").context("Missing input")?;
        let thread = input.pointer("/threads/0").unwrap_or(input);
        assert_eq!(
            thread.get("startLine").and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            thread.get("line").and_then(serde_json::Value::as_u64),
            Some(3)
        );
        assert_eq!(
            thread.get("side").and_then(serde_json::Value::as_str),
            Some("RIGHT")
        );
    }
    review::execute(
        &key,
        head,
        &Operation::Review {
            event: "REQUEST_CHANGES".into(),
            body: "Needs changes".into(),
        },
        &cancel,
    )?;
    assert!(review::state(&key, &cancel)?.pending.is_none());
    let old = Anchor {
        side: Side::Left,
        ..anchor
    };
    review::execute(
        &key,
        head,
        &Operation::Comment {
            anchor: old,
            body: "Old side".into(),
            pending: false,
        },
        &cancel,
    )?;
    review::execute(
        &key,
        head,
        &Operation::Viewed {
            path: "main.rs".into(),
            viewed: true,
        },
        &cancel,
    )?;
    assert!(review::state(&key, &cancel)?.viewed.contains("main.rs"));
    review::execute(
        &key,
        head,
        &Operation::Viewed {
            path: "main.rs".into(),
            viewed: false,
        },
        &cancel,
    )?;
    assert!(review::state(&key, &cancel)?.viewed.is_empty());
    for squash in [false, true] {
        for admin in [false, true] {
            review::execute(&key, head, &Operation::Merge { squash, admin }, &cancel)?;
        }
    }
    review::execute(
        &key,
        head,
        &Operation::Close {
            body: "Closing explanation".into(),
        },
        &cancel,
    )?;
    let writes = fs::read_to_string(root.join("writes.jsonl"))?;
    assert!(writes.contains("start_line"));
    assert!(writes.contains("LEFT"));
    assert!(writes.contains("SubmitPullRequestReviewInput"));
    assert_eq!(writes.matches("match-head-commit").count(), 4);
    assert!(writes.contains("Closing explanation"));
    fs::write(root.join("fail-write"), "")?;
    assert!(review::execute(&key, head, &pending, &cancel).is_err());
    assert_eq!(writes, fs::read_to_string(root.join("writes.jsonl"))?);
    fs::remove_file(root.join("fail-write"))?;
    let mentions = review::mentions(&key, &cancel)?;
    assert_eq!(mentions.users, vec!["alice", "author", "reviewer"]);
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.into(),
    };
    review::save_mentions(&storage, &key, &mentions)?;
    assert_eq!(
        review::cached_mentions(&storage, &key)?.users,
        mentions.users
    );
    Ok(())
}

fn exercise_checks(root: &Path) -> Result<()> {
    let key = PrKey {
        owner: "example".into(),
        repo: "project".into(),
        number: 1,
    };
    let cancel = Cancel::default();
    fs::write(root.join("status-case"), "conflict")?;
    let report = difu::github::checks(&key, &cancel)?;
    assert_eq!(report.mergeable, "CONFLICTING");
    assert_eq!(report.checks.len(), 2);
    assert!(report.checks.iter().all(|c| c.state == "expected"));
    fs::write(root.join("status-case"), "unknown")?;
    let report = difu::github::checks(&key, &cancel)?;
    assert_eq!(report.mergeable, "UNKNOWN");
    assert!(report.checks.is_empty());
    fs::write(root.join("status-case"), "failure")?;
    let report = difu::github::checks(&key, &cancel)?;
    let check = report.checks.first().context("Missing failed check")?;
    assert_eq!(check.state, "fail");
    let failures = difu::ci::load(&key, check, &cancel);
    assert!(
        failures
            .tests
            .first()
            .is_some_and(|t| t.name.contains("sends reply"))
    );
    fs::write(root.join("status-case"), "rules-denied")?;
    let report = difu::github::checks(&key, &cancel)?;
    assert!(report.rules_error.is_some());
    assert_eq!(
        report.checks.first().map(|c| c.state.as_str()),
        Some("fail")
    );
    fs::remove_file(root.join("status-case"))?;
    Ok(())
}

fn exercise_mention_shortcut(root: &Path) -> Result<()> {
    use difu::{
        editor::Editor,
        review::Mentions,
        workflow::{Compose, Kind, Wizard},
    };
    let storage = Storage {
        config: root.join("mention-shortcut-config.json"),
        cache: root.into(),
    };
    let mut app = App::new(storage, Config::default());
    let key = PrKey {
        owner: "example".into(),
        repo: "project".into(),
        number: 1,
    };
    app.workflow.mentions.insert(
        key.id(),
        Mentions {
            users: vec!["cached-user".into()],
            fetched: chrono::Utc::now().timestamp(),
        },
    );
    app.wizard(Wizard::Compose(Compose {
        key: key.clone(),
        head: "head".into(),
        kind: Kind::Review,
        editor: Editor::default(),
        choice: 0,
        focus: 0,
        mention: 0,
    }));
    app.key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert!(app.workflow.mentions_loading.is_empty());
    app.key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(app.workflow.mentions_loading.contains(&key.id()));
    wait(&mut app, |a| a.workflow.mentions_loading.is_empty())?;
    assert!(
        app.workflow
            .mentions
            .get(&key.id())
            .is_some_and(|m| m.users.iter().any(|u| u == "alice"))
    );
    assert!(
        matches!(&app.modal,Some(difu::app::Modal::Workflow(w)) if matches!(w.as_ref(),Wizard::Compose(draft) if draft.editor.text()=="r"))
    );
    Ok(())
}
