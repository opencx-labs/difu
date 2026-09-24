use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use difu::{
    app::{Action, App, Focus, Modal, Review, View},
    codex::{Chapter, Guide},
    model::{PrDetail, PrKey, PrSummary},
    navigation::{self, Request},
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
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args),
        &Cancel::default(),
    )
    .map(|s| s.trim().into())
}
fn wait(app: &mut App) -> Result<()> {
    let start = Instant::now();
    loop {
        app.tick();
        if matches!(&app.modal, Some(Modal::Definition(v)) if v.output.is_some()) {
            return Ok(());
        }
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "Navigation did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn modal_uses_pinned_revisions_and_restores_review_in_all_diff_layouts() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git(root, &["init"])?;
    let original = "import {target as run} from './library';\n\nexport function useIt() {\n\tconst label = '界'; run();\n}\n";
    fs::write(root.join("a-old.ts"), original)?;
    fs::write(
        root.join("library.ts"),
        "import {leaf} from './leaf';\nexport function target() { return 'old' + leaf(); }\n",
    )?;
    fs::write(
        root.join("leaf.ts"),
        "import {finish} from './terminal';\nconst 界 = 1; export const leaf = () => finish();\n",
    )?;
    fs::write(
        root.join("terminal.ts"),
        "export function finish() { return 'base'; }\n",
    )?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-m", "base"])?;
    let base = git(root, &["rev-parse", "HEAD"])?;
    fs::rename(root.join("a-old.ts"), root.join("a-new.ts"))?;
    fs::write(root.join("a-new.ts"), original.replace("界", "世"))?;
    let body = (0..50)
        .map(|i| format!("  // detail {i}\n"))
        .collect::<String>();
    fs::write(
        root.join("library.ts"),
        format!(
            "import {{leaf}} from './leaf';\nexport function target() {{\n{body}  return 'new' + leaf();\n}}\n"
        ),
    )?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-m", "head"])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let key = PrKey {
        owner: "example".into(),
        repo: "fixture".into(),
        number: 1,
    };
    let pr = PrDetail {
        requested_reviewers: Vec::new(),
        requested_teams: Vec::new(),
        key: key.clone(),
        title: "Navigation".into(),
        body: String::new(),
        author: "test".into(),
        base: base.clone(),
        head: head.clone(),
        base_branch: "main".into(),
        head_branch: "feature".into(),
        state: "open".into(),
        additions: 0,
        deletions: 0,
        changed_files: 2,
    };
    let snapshot = repo::snapshot(root, &pr, &Cancel::default())?;
    let guide = Guide {
        chapters: vec![Chapter {
            category: difu::codex::ChapterCategory::Regular,
            title: "Navigate".into(),
            explanation: "Keep this chapter and scroll position".into(),
            hunks: snapshot.units().map(|(_, h)| h.id.clone()).collect(),
        }],
    };
    fs::write(root.join("library.ts"), "uncommitted private content")?;
    let worktrees = git(root, &["worktree", "list", "--porcelain"])?;
    fs::create_dir(root.join("cache"))?;
    let mut app = App::new(
        Storage {
            config: root.join("settings.json"),
            cache: root.join("cache"),
        },
        Default::default(),
    );
    app.inbox.push(PrSummary {
        key: key.clone(),
        title: pr.title.clone(),
        author: "test".into(),
        created: String::new(),
        updated: String::new(),
        stats: None,
        stats_error: false,
        draft: false,
    });
    // No PR details: this fixture cannot trigger any GitHub requests while ticking.
    app.reviews.insert(
        key.id(),
        Review {
            root: Some(root.into()),
            snapshot: Some(Arc::new(snapshot)),
            guide: Some(Arc::new(guide)),
            ..Default::default()
        },
    );
    for (view, width, unified, horizontal) in [
        (View::Guide, 180, false, 0),
        (View::Guide, 80, false, 5),
        (View::Diff, 150, false, 0),
        (View::Diff, 100, true, 5),
    ] {
        app.action(Action::SetView(view));
        app.config.unified = unified;
        app.horizontal = horizontal;
        app.focus = Focus::Content;
        app.invalidate();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 35))?;
        for old in [true, false] {
            terminal.draw(|f| difu::ui::draw(f, &mut app))?;
            // Finish independent metadata hydration before measuring modal restoration.
            // Boundary tests separately verify that hydration preserves the source anchor.
            let started = Instant::now();
            loop {
                app.tick();
                let review = app.review().context("Missing review")?;
                if !review.bounds.is_empty() && review.bounds.values().all(|state| !state.loading) {
                    ensure!(
                        review.bounds.values().all(|state| state.error.is_none()),
                        "Could not read fixture boundaries"
                    );
                    break;
                }
                ensure!(
                    started.elapsed() < Duration::from_secs(10),
                    "Boundary hydration timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            terminal.draw(|f| difu::ui::draw(f, &mut app))?;
            let link = app.hits.iter().find(|(_, action)| matches!(action, Action::Definition { path, line: 4, column, old: side } if path == "a-new.ts" && *side == old && *column == "\tconst label = '界'; ".len())).map(|(rect, _)| *rect).context("Missing symbol hit region")?;
            app.mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: link.x,
                row: link.y,
                modifiers: KeyModifiers::NONE,
            });
            terminal.draw(|f| difu::ui::draw(f, &mut app))?;
            assert_eq!(app.hover.rect, Some(link));
            assert!(!app.hover.active());
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .cell((link.x, link.y))
                    .context("Missing hovered cell")?
                    .modifier
                    .contains(ratatui::style::Modifier::UNDERLINED)
            );
            app.mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: link.x,
                row: link.y,
                modifiers: KeyModifiers::NONE,
            });
            assert!(
                app.modal.is_none(),
                "Plain clicks should focus a line without opening a definition"
            );
            if old {
                app.key_event(KeyEvent::new(
                    KeyCode::Modifier(crossterm::event::ModifierKeyCode::LeftSuper),
                    KeyModifiers::SUPER,
                ));
            }
            let saved = (
                app.scroll,
                app.horizontal,
                app.focus,
                app.file,
                app.workflow.cursor,
                app.workflow.nav,
                app.nav_scroll,
            );
            app.mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: link.x,
                row: link.y,
                modifiers: if old {
                    KeyModifiers::NONE
                } else {
                    KeyModifiers::CONTROL
                },
            });
            wait(&mut app)?;
            let Some(Modal::Definition(viewer)) = &app.modal else {
                anyhow::bail!("Definition modal missing");
            };
            let definition = viewer
                .output
                .as_ref()
                .context("Missing result")?
                .as_ref()
                .map_err(|e| anyhow::anyhow!(e.clone()))?;
            assert_eq!(
                definition.revision,
                if old { base.clone() } else { head.clone() }
            );
            assert_eq!(definition.path, "library.ts");
            assert!(
                definition
                    .source
                    .contains(if old { "'old'" } else { "'new'" })
            );
            terminal.draw(|f| difu::ui::draw(f, &mut app))?;
            app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
            if !old {
                assert!(matches!(&app.modal, Some(Modal::Definition(v)) if v.scroll == 10));
            }
            // Follow two imported functions within the same modal, including
            // an arrow-function snippet starting mid-line after a Unicode prefix.
            for (symbol, expected_path) in [("leaf", "leaf.ts"), ("finish", "terminal.ts")] {
                let Some(Modal::Definition(viewer)) = &mut app.modal else {
                    anyhow::bail!("Definition modal missing before navigation");
                };
                let previous_id = viewer.id;
                let previous_cancel = viewer.cancel.clone();
                viewer.scroll = usize::MAX;
                viewer.horizontal = 2;
                terminal.draw(|f| difu::ui::draw(f, &mut app))?;
                let nested = app
                    .hits
                    .iter()
                    .find_map(|(rect, action)| {
                        let Action::Definition {
                            path, line, column, ..
                        } = action
                        else {
                            return None;
                        };
                        let source = git(
                            root,
                            &[
                                "show",
                                &format!("{}:{path}", if old { &base } else { &head }),
                            ],
                        )
                        .ok()?;
                        source
                            .lines()
                            .nth(*line as usize - 1)?
                            .get(*column..)?
                            .starts_with(symbol)
                            .then_some(*rect)
                    })
                    .context("Missing nested symbol hit region")?;
                app.mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: nested.x,
                    row: nested.y,
                    modifiers: KeyModifiers::NONE,
                });
                terminal.draw(|f| difu::ui::draw(f, &mut app))?;
                assert_eq!(app.hover.rect, Some(nested));
                app.mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: nested.x,
                    row: nested.y,
                    modifiers: KeyModifiers::CONTROL,
                });
                assert!(previous_cancel.cancelled());
                wait(&mut app)?;
                let Some(Modal::Definition(viewer)) = &app.modal else {
                    anyhow::bail!("Definition modal missing after navigation");
                };
                assert_ne!(viewer.id, previous_id);
                assert_eq!(viewer.scroll, 0);
                assert_eq!(viewer.horizontal, 0);
                assert_eq!(
                    viewer.request.revision,
                    if old { base.clone() } else { head.clone() }
                );
                let next = viewer
                    .output
                    .as_ref()
                    .context("Missing nested result")?
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!(e.clone()))?;
                assert_eq!(next.path, expected_path);
            }
            app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(app.modal.is_none());
            app.mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
            terminal.draw(|f| difu::ui::draw(f, &mut app))?;
            assert!(app.hover.rect.is_none());
            assert!(
                !terminal
                    .backend()
                    .buffer()
                    .cell((link.x, link.y))
                    .context("Missing old hovered cell")?
                    .modifier
                    .contains(ratatui::style::Modifier::UNDERLINED)
            );
            assert_eq!(
                saved,
                (
                    app.scroll,
                    app.horizontal,
                    app.focus,
                    app.file,
                    app.workflow.cursor,
                    app.workflow.nav,
                    app.nav_scroll
                )
            );
        }
    }
    assert_eq!(
        fs::read_to_string(root.join("library.ts"))?,
        "uncommitted private content"
    );
    assert_eq!(git(root, &["worktree", "list", "--porcelain"])?, worktrees);
    // Symlinks and untracked files must not become alternate resolution targets.
    std::os::unix::fs::symlink("library.ts", root.join("link.ts"))?;
    git(root, &["add", "link.ts"])?;
    git(root, &["commit", "-m", "symlink"])?;
    let request = Request {
        root: root.into(),
        revision: git(root, &["rev-parse", "HEAD"])?,
        path: "link.ts".into(),
        line: 1,
        column: 0,
    };
    assert!(navigation::resolve(&request, &Cancel::default()).is_err());
    Ok(())
}
