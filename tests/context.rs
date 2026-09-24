use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use difu::{
    app::{Action, App, Review, View},
    codex::{self, Chapter, Guide},
    context::Direction,
    model::{ModelChoice, PrDetail, PrKey, PrSummary},
    process::{self, Cancel},
    repo,
    storage::Storage,
};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

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

fn fixture(root: &Path) -> Result<(App, PrDetail)> {
    git(root, &["init"])?;
    let original = (1..=80).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(root.join("file.rs"), &original)?;
    git(root, &["add", "file.rs"])?;
    git(root, &["commit", "-m", "base"])?;
    let base = git(root, &["rev-parse", "HEAD"])?;
    fs::write(
        root.join("file.rs"),
        original
            .replace("line 20\n", "changed 20\n")
            .replace("line 32\n", "changed 32\n"),
    )?;
    git(root, &["commit", "-am", "head"])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let key = PrKey {
        owner: "example".into(),
        repo: "repo".into(),
        number: 1,
    };
    let pr = PrDetail {
        draft: false,
        requested_reviewers: Vec::new(),
        requested_teams: Vec::new(),
        key: key.clone(),
        title: "Context".into(),
        body: String::new(),
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
    let snapshot = repo::snapshot(root, &pr, &Cancel::default())?;
    assert_eq!(
        snapshot.files.first().context("Missing file")?.hunks.len(),
        2
    );
    fs::write(
        root.join("file.rs"),
        "uncommitted changes must stay private\n",
    )?;
    let mut app = App::new(
        Storage {
            config: root.join("config.json"),
            cache: root.into(),
        },
        Default::default(),
    );
    app.inbox.push(PrSummary {
        key: key.clone(),
        title: pr.title.clone(),
        author: pr.author.clone(),
        updated: String::new(),
        created: String::new(),
        stats: None,
        stats_error: false,
        draft: false,
    });
    let guide = Guide {
        chapters: vec![
            Chapter {
                category: difu::codex::ChapterCategory::Regular,
                title: "First concern".into(),
                explanation: "First change".into(),
                hunks: vec!["f0-h0".into()],
            },
            Chapter {
                category: difu::codex::ChapterCategory::Regular,
                title: "Second concern".into(),
                explanation: "Second change".into(),
                hunks: vec!["f0-h1".into()],
            },
            Chapter {
                category: difu::codex::ChapterCategory::Regular,
                title: "Another use".into(),
                explanation: "Reused change".into(),
                hunks: vec!["f0-h1".into()],
            },
        ],
    };
    guide.validate(&snapshot)?;
    app.reviews.insert(
        key.id(),
        Review {
            root: Some(root.into()),
            snapshot: Some(Arc::new(snapshot)),
            guide: Some(Arc::new(guide)),
            ..Default::default()
        },
    );
    app.action(Action::SetView(View::Guide));
    Ok((app, pr))
}

fn render(app: &mut App, width: u16) -> Result<()> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 40))?;
    terminal.draw(|frame| difu::ui::draw(frame, app))?;
    Ok(())
}

fn wait_bounds(app: &mut App, width: u16) -> Result<()> {
    render(app, width)?;
    let started = Instant::now();
    loop {
        app.tick();
        let review = app.review().context("Missing review")?;
        if review
            .context
            .get("file.rs")
            .is_some_and(|state| state.data.is_some())
            || review
                .bounds
                .get("file.rs")
                .is_some_and(|state| state.data.is_some())
        {
            return Ok(());
        }
        if let Some(error) = review
            .bounds
            .get("file.rs")
            .and_then(|state| state.error.as_ref())
        {
            anyhow::bail!("Boundary read failed: {error}");
        }
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Boundary read timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn click(app: &mut App, width: u16, matches: impl Fn(&Action) -> bool) -> Result<()> {
    wait_bounds(app, width)?;
    render(app, width)?;
    let row = app
        .document
        .as_ref()
        .context("Missing document")?
        .rows
        .iter()
        .position(|row| row.right.action.as_ref().is_some_and(&matches))
        .context("Missing control in document")?;
    app.scroll = row.saturating_sub(5);
    render(app, width)?;
    let rect = app
        .hits
        .iter()
        .rev()
        .find(|(_, action)| matches(action))
        .map(|(rect, _)| *rect)
        .context("Missing clickable control")?;
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    Ok(())
}

fn wait_context(app: &mut App) -> Result<()> {
    let started = Instant::now();
    loop {
        app.tick();
        let state = app
            .review()
            .and_then(|r| r.context.get("file.rs"))
            .context("Missing context state")?;
        ensure!(state.error.is_none(), "Context error: {:?}", state.error);
        if state.data.is_some() {
            return Ok(());
        }
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Context load timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn mouse_expansion_is_per_hunk_and_neighbor_links_navigate_in_both_layouts() -> Result<()> {
    for width in [80, 180] {
        let dir = tempfile::tempdir()?;
        let (mut app, pr) = fixture(dir.path())?;
        let snapshot = app
            .review()
            .and_then(|r| r.snapshot.clone())
            .context("Missing snapshot")?;
        let before = serde_json::to_vec(&snapshot)?;
        let cache_key = codex::cache_key(&pr, &snapshot, &ModelChoice::default())?;
        click(
            &mut app,
            width,
            |a| matches!(a, Action::ExpandHunk(id, Direction::Below) if id == "f0-h0"),
        )?;
        wait_context(&mut app)?;
        let review = app.review().context("Missing review")?;
        assert_eq!(
            review
                .expanded
                .get("f0-h0")
                .context("Missing expansion")?
                .below,
            10
        );
        assert!(!review.expanded.contains_key("f0-h1"));
        render(&mut app, width)?;
        let doc = app.document.as_ref().context("Missing document")?;
        let second = doc.sections.get(1).context("Missing second chapter")?.start;
        let rows = doc
            .rows
            .iter()
            .take(second)
            .flat_map(|row| row.right.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rows.contains("Neighboring hunk"));
        assert!(rows.contains("Explained in Chapter 2: Second concern"));
        assert!(rows.contains("Explained in Chapter 3: Another use"));
        assert!(rows.contains("changed 32"));
        assert!(!rows.contains("uncommitted"));
        click(&mut app, width, |a| matches!(a, Action::GoToChapter(1)))?;
        render(&mut app, width)?;
        assert_eq!(
            app.scroll,
            app.document
                .as_ref()
                .and_then(|d| d.sections.get(1))
                .context("Missing destination")?
                .start
        );
        click(
            &mut app,
            width,
            |a| matches!(a, Action::ExpandHunk(id, Direction::Below) if id == "f0-h0"),
        )?;
        assert_eq!(
            app.review()
                .and_then(|r| r.expanded.get("f0-h0"))
                .context("Missing expansion")?
                .below,
            20
        );
        click(
            &mut app,
            width,
            |a| matches!(a, Action::ExpandHunk(id, Direction::Above) if id == "f0-h0"),
        )?;
        assert_eq!(
            app.review()
                .and_then(|r| r.expanded.get("f0-h0"))
                .context("Missing expansion")?
                .above,
            10
        );
        app.action(Action::SetView(View::Diff));
        click(&mut app, width, |a| matches!(a, Action::GoToChapter(2)))?;
        render(&mut app, width)?;
        assert_eq!(app.view, View::Guide);
        assert_eq!(
            app.scroll,
            app.document
                .as_ref()
                .and_then(|d| d.sections.get(2))
                .context("Missing third chapter")?
                .start
        );
        assert_eq!(serde_json::to_vec(&snapshot)?, before);
        assert_eq!(
            codex::cache_key(&pr, &snapshot, &ModelChoice::default())?,
            cache_key
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("file.rs"))?,
            "uncommitted changes must stay private\n"
        );
        app.shutdown();
    }
    Ok(())
}

#[test]
fn context_read_failure_is_visible_and_can_be_retried() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut app, _) = fixture(dir.path())?;
    wait_bounds(&mut app, 180)?;
    fs::rename(dir.path().join(".git"), dir.path().join("git-backup"))?;
    click(
        &mut app,
        180,
        |a| matches!(a, Action::ExpandHunk(id, Direction::Below) if id == "f0-h0"),
    )?;
    let started = Instant::now();
    loop {
        app.tick();
        if app
            .review()
            .and_then(|r| r.context.get("file.rs"))
            .is_some_and(|s| s.error.is_some())
        {
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Missing context error"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.review().context("Missing review")?.expanded.is_empty());
    render(&mut app, 180)?;
    let text = app
        .document
        .as_ref()
        .context("Missing document")?
        .rows
        .iter()
        .flat_map(|r| r.right.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(text.contains("Could not load context"));
    fs::rename(dir.path().join("git-backup"), dir.path().join(".git"))?;
    click(
        &mut app,
        180,
        |a| matches!(a, Action::ExpandHunk(id, Direction::Below) if id == "f0-h0"),
    )?;
    wait_context(&mut app)?;
    assert_eq!(
        app.review()
            .and_then(|r| r.expanded.get("f0-h0"))
            .context("Missing expansion")?
            .below,
        10
    );
    app.shutdown();
    Ok(())
}

#[test]
fn copying_across_hunks_reads_pinned_context_and_preserves_the_review() -> Result<()> {
    use crossterm::{
        clipboard::CopyToClipboard,
        event::{KeyCode, KeyEvent},
        execute,
    };
    use difu::{
        app::Focus,
        review::{Anchor, Side},
        workflow::Target,
    };
    let dir = tempfile::tempdir()?;
    let (mut app, _) = fixture(dir.path())?;
    render(&mut app, 160)?;
    let cursor = app
        .document
        .as_ref()
        .context("Missing document")?
        .rows
        .iter()
        .position(|row| matches!(row.right.target, Some(Target::Code { new: Some(32), .. })))
        .context("Missing selected line")?;
    app.workflow.cursor = Some(cursor);
    app.workflow.side = Side::Right;
    app.workflow.selection = Some(Anchor {
        path: "file.rs".into(),
        side: Side::Right,
        start: 20,
        end: 20,
    });
    app.focus = Focus::Content;
    let scroll = app.scroll;
    app.key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
    wait_context(&mut app)?;
    app.tick();
    let expected = (20..=32)
        .map(|n| {
            if n == 20 || n == 32 {
                format!("changed {n}")
            } else {
                format!("line {n}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut encoded = Vec::new();
    execute!(&mut encoded, CopyToClipboard::to_clipboard_from(&expected))?;
    let mut output = Vec::new();
    app.flush_clipboard(&mut output);
    assert_eq!(output, encoded);
    assert_eq!(app.workflow.cursor, Some(cursor));
    assert_eq!(app.workflow.selection.as_ref().map(|a| a.start), Some(20));
    assert_eq!(app.scroll, scroll);
    assert_eq!(
        fs::read_to_string(dir.path().join("file.rs"))?,
        "uncommitted changes must stay private\n"
    );
    // Repeating the copy uses the loaded immutable context synchronously.
    app.key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    let mut again = Vec::new();
    app.flush_clipboard(&mut again);
    assert_eq!(again, encoded);
    app.shutdown();
    Ok(())
}

#[test]
fn boundaries_hide_controls_at_file_edges_and_reuse_the_pinned_cache() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut app, pr) = fixture(dir.path())?;
    render(&mut app, 180)?;
    assert!(
        !app.hits
            .iter()
            .any(|(_, action)| matches!(action, Action::ExpandHunk(..))),
        "Unknown boundaries must not offer speculative expansion"
    );
    let selected = app
        .document
        .as_ref()
        .context("Missing document")?
        .rows
        .iter()
        .position(|row| {
            matches!(
                row.right.target,
                Some(difu::workflow::Target::Code { new: Some(20), .. })
            )
        })
        .context("Missing cursor line")?;
    app.workflow.cursor = Some(selected);
    wait_bounds(&mut app, 180)?;
    render(&mut app, 180)?;
    let cursor = app.workflow.cursor.context("Lost cursor")?;
    assert!(
        matches!(
            app.document
                .as_ref()
                .and_then(|doc| doc.rows.get(cursor))
                .and_then(|row| row.right.target.as_ref()),
            Some(difu::workflow::Target::Code { new: Some(20), .. })
        ),
        "Boundary hydration must preserve the focused source line"
    );
    let snapshot = app
        .review()
        .and_then(|review| review.snapshot.clone())
        .context("Missing snapshot")?;
    let file = snapshot.files.first().context("Missing file")?;
    let bounds = difu::bounds::load(dir.path(), &snapshot, file, dir.path(), &Cancel::default())?;
    assert_eq!(bounds, difu::bounds::Bounds { old: 80, new: 80 });
    // The working copy has only one private line, so these must be pinned counts.
    assert!(
        file.hunks
            .iter()
            .all(|hunk| bounds.can_expand(hunk, Direction::Above)
                && bounds.can_expand(hunk, Direction::Below))
    );
    let mut full = file.clone();
    full.hunks = vec![difu::diff::Hunk {
        id: "whole".into(),
        header: "@@ -1,80 +1,80 @@".into(),
        lines: repo::file_context(dir.path(), &snapshot, file, &Cancel::default())?,
    }];
    let whole = full.hunks.first().context("Missing full hunk")?;
    assert!(!bounds.can_expand(whole, Direction::Above));
    assert!(!bounds.can_expand(whole, Direction::Below));
    fs::rename(dir.path().join(".git"), dir.path().join("git-backup"))?;
    assert_eq!(
        difu::bounds::load(dir.path(), &snapshot, file, dir.path(), &Cancel::default())?,
        bounds
    );
    let mut newer = snapshot.as_ref().clone();
    newer.head = "a".repeat(40);
    assert!(
        difu::bounds::load(dir.path(), &newer, file, dir.path(), &Cancel::default()).is_err(),
        "Changed revisions must not reuse old boundaries"
    );
    fs::rename(dir.path().join("git-backup"), dir.path().join(".git"))?;
    // Render the whole file through both views: neither expansion control belongs at its edges.
    let review = app
        .reviews
        .get_mut(&pr.key.id())
        .context("Missing review")?;
    review.guide = None;
    review.snapshot = Some(Arc::new(difu::diff::Snapshot {
        files: vec![full],
        ..snapshot.as_ref().clone()
    }));
    for view in [View::Guide, View::Diff] {
        app.action(Action::SetView(view));
        render(&mut app, 180)?;
        assert!(
            !app.document
                .as_ref()
                .context("Missing document")?
                .rows
                .iter()
                .any(|row| matches!(row.right.action, Some(Action::ExpandHunk(..))))
        );
    }
    app.shutdown();
    Ok(())
}

#[test]
fn brace_keys_adjust_only_focused_hunk_one_line_and_keep_source_anchor() -> Result<()> {
    use crossterm::event::{KeyCode, KeyEvent};
    use difu::workflow::Target;
    for width in [80, 180] {
        for view in [View::Guide, View::Diff] {
            let dir = tempfile::tempdir()?;
            let (mut app, _) = fixture(dir.path())?;
            app.action(Action::SetView(view));
            render(&mut app, width)?;
            let cursor = app
                .document
                .as_ref()
                .context("Missing document")?
                .rows
                .iter()
                .position(|r| matches!(r.right.target, Some(Target::Code { new: Some(20), .. })))
                .context("Missing selected line")?;
            app.workflow.cursor = Some(cursor);
            for _ in 0..3 {
                app.key_event(KeyEvent::new(KeyCode::Char('}'), KeyModifiers::SHIFT));
            }
            wait_context(&mut app)?;
            render(&mut app, width)?;
            let review = app.review().context("Missing review")?;
            let expansion = review.expanded.get("f0-h0").context("Missing expansion")?;
            assert_eq!((expansion.above, expansion.below), (3, 3));
            assert!(!review.expanded.contains_key("f0-h1"));
            let row = app
                .document
                .as_ref()
                .and_then(|d| d.rows.get(app.workflow.cursor?))
                .context("Missing anchor")?;
            assert!(matches!(
                row.right.target,
                Some(Target::Code { new: Some(20), .. })
            ));
            for _ in 0..5 {
                app.key_event(KeyEvent::new(KeyCode::Char('{'), KeyModifiers::SHIFT));
                render(&mut app, width)?;
            }
            let expansion = app
                .review()
                .and_then(|r| r.expanded.get("f0-h0"))
                .context("Missing expansion")?;
            assert_eq!((expansion.above, expansion.below), (0, 0));
            assert_eq!(
                fs::read_to_string(dir.path().join("file.rs"))?,
                "uncommitted changes must stay private\n"
            );
        }
    }
    Ok(())
}
