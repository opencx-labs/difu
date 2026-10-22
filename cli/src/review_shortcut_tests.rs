use super::*;
use crate::{
    settings::{BindingKey, SettingId},
    shortcuts::{ActionId, ids::*, key},
};
use anyhow::Context;

const PHASES: [(&str, bool); 4] = [
    ("default", false),
    ("custom", true),
    ("reset", false),
    ("discard", true),
];
fn fixture(action: ActionId, phase: &str) -> Result<(App, tempfile::TempDir)> {
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    if phase != "default" {
        config
            .preferences
            .keyboard
            .bindings
            .insert(action.0.into(), key(BindingKey::Function(12), 0));
    }
    storage.update_config(|saved| *saved = config.clone())?;
    if phase == "reset" || phase == "discard" {
        let mut draft = storage.settings_draft()?;
        draft.reset(&SettingId::Shortcut(action.0.into()))?;
        assert!(
            storage
                .load_config()?
                .preferences
                .keyboard
                .bindings
                .contains_key(action.0)
        );
        if phase == "reset" {
            config = storage.save_settings_draft(&mut draft)?;
        } else {
            draft.discard(storage.load_config()?);
            config = draft.config().clone();
        }
    }
    let mut app = App::new(storage, config);
    app.models_loading = true;
    Ok((app, dir))
}
fn event(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn active(custom: bool, old: KeyEvent) -> KeyEvent {
    if custom { event(KeyCode::F(12)) } else { old }
}

#[test]
fn review_main_actions_replace_defaults_with_real_saved_reset_and_discard_effects() -> Result<()> {
    for (phase, custom) in PHASES {
        for (id, old) in [
            (REVIEW_HELP, event(KeyCode::Char('?'))),
            (REVIEW_MODELS, event(KeyCode::Char('m'))),
            (REVIEW_FILTER, event(KeyCode::Char('f'))),
            (REVIEW_WRAP, event(KeyCode::Char('w'))),
            (
                REVIEW_LAYOUT,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            ),
            (REVIEW_CONTROLS, event(KeyCode::Char('/'))),
            (REVIEW_BACK, event(KeyCode::Esc)),
            (NAVIGATION_NEXT_FOCUS, event(KeyCode::Tab)),
            (NAVIGATION_PREVIOUS_FOCUS, event(KeyCode::BackTab)),
        ] {
            let (mut app, _dir) = fixture(id, phase)?;
            let wrap = app.config.wrap_diff;
            let unified = app.config.unified;
            app.focus = Focus::Navigation;
            if custom {
                app.key_event(old);
                assert!(
                    app.modal.is_none() && app.filters.focused.is_none() && !app.quit,
                    "removed {id:?}"
                );
                assert_eq!(
                    (app.config.wrap_diff, app.config.unified, app.focus),
                    (wrap, unified, Focus::Navigation),
                    "removed {id:?}"
                );
            }
            app.key_event(active(custom, old));
            match id {
                REVIEW_HELP => assert!(matches!(app.modal, Some(Modal::Help(_)))),
                REVIEW_MODELS => assert!(matches!(app.modal, Some(Modal::Models { .. }))),
                REVIEW_FILTER => assert!(app.filters.focused.is_some()),
                REVIEW_WRAP => assert_eq!(app.storage.load_config()?.wrap_diff, !wrap),
                REVIEW_LAYOUT => assert_eq!(app.storage.load_config()?.unified, !unified),
                REVIEW_CONTROLS => assert!(matches!(app.modal, Some(Modal::Workflow(_)))),
                REVIEW_BACK => assert!(app.quit),
                _ => assert_eq!(app.focus, Focus::Content),
            }
        }
    }
    Ok(())
}

#[test]
fn review_help_navigation_replaces_keys_while_home_end_stay_editor_keys() -> Result<()> {
    for (phase, custom) in PHASES {
        for (id, code, modifiers, want) in [
            (NAVIGATION_UP, KeyCode::Up, KeyModifiers::NONE, 19),
            (NAVIGATION_DOWN, KeyCode::Down, KeyModifiers::NONE, 21),
            (NAVIGATION_PAGE_UP, KeyCode::PageUp, KeyModifiers::NONE, 10),
            (
                NAVIGATION_PAGE_DOWN,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                30,
            ),
            (NAVIGATION_FAST_UP, KeyCode::Up, KeyModifiers::SUPER, 10),
            (NAVIGATION_FAST_DOWN, KeyCode::Down, KeyModifiers::SUPER, 30),
        ] {
            let (mut app, _dir) = fixture(id, phase)?;
            app.modal = Some(Modal::Help(crate::help::State {
                scroll: 20,
                rows: 110,
                viewport: 10,
                ..Default::default()
            }));
            let old = KeyEvent::new(code, modifiers);
            if custom {
                app.key_event(old);
                assert!(
                    matches!(&app.modal, Some(Modal::Help(s)) if s.scroll == 20),
                    "removed {id:?}"
                );
            }
            app.key_event(active(custom, old));
            assert!(
                matches!(&app.modal, Some(Modal::Help(s)) if s.scroll == want),
                "{phase} {id:?}"
            );
            app.key_event(event(KeyCode::Esc));
            assert!(app.modal.is_none());
        }
    }
    Ok(())
}

#[test]
fn local_repository_picker_actions_remap_without_touching_repositories() -> Result<()> {
    for (phase, custom) in PHASES {
        for (id, code) in [
            (NAVIGATION_UP, KeyCode::Up),
            (NAVIGATION_DOWN, KeyCode::Down),
            (LOCAL_TOGGLE, KeyCode::Char(' ')),
            (LOCAL_CONFIGURE, KeyCode::Char('e')),
            (NAVIGATION_ACTIVATE, KeyCode::Enter),
        ] {
            let (mut app, dir) = fixture(id, phase)?;
            app.inbox_tab = InboxTab::Diffs;
            app.local.setup = Some(local::Setup::Select {
                roots: vec![],
                repos: vec![
                    dir.path().join("a"),
                    dir.path().join("b"),
                    dir.path().join("c"),
                ],
                chosen: if id == NAVIGATION_ACTIVATE {
                    [dir.path().join("b")].into()
                } else {
                    Default::default()
                },
                selected: 1,
            });
            if custom {
                app.key_event(event(code));
                assert!(
                    matches!(&app.local.setup, Some(local::Setup::Select { selected: 1, chosen, .. }) if chosen.len() == usize::from(id == NAVIGATION_ACTIVATE)),
                    "removed {id:?}"
                );
            }
            app.key_event(active(custom, event(code)));
            match id {
                NAVIGATION_UP => assert!(matches!(
                    app.local.setup,
                    Some(local::Setup::Select { selected: 0, .. })
                )),
                NAVIGATION_DOWN => assert!(matches!(
                    app.local.setup,
                    Some(local::Setup::Select { selected: 2, .. })
                )),
                LOCAL_TOGGLE => assert!(
                    matches!(&app.local.setup, Some(local::Setup::Select { chosen, .. }) if chosen.contains(&dir.path().join("b")))
                ),
                LOCAL_CONFIGURE => assert!(matches!(app.local.setup, Some(local::Setup::Roots(_)))),
                _ => assert!(app.local.setup.is_none()),
            }
        }
    }
    Ok(())
}

#[test]
fn workflow_form_actions_remap_and_keep_editor_events_raw() -> Result<()> {
    use crate::workflow::{Compose, Kind, Wizard};
    for (phase, custom) in PHASES {
        for (id, code, modifiers, focus, want_focus, want_choice) in [
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                KeyModifiers::NONE,
                0,
                1,
                1,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                0,
                1,
                1,
            ),
            (
                WORKFLOW_CHOICE_PREVIOUS,
                KeyCode::Left,
                KeyModifiers::NONE,
                1,
                1,
                0,
            ),
            (
                WORKFLOW_CHOICE_PREVIOUS,
                KeyCode::Up,
                KeyModifiers::NONE,
                1,
                1,
                0,
            ),
            (
                WORKFLOW_CHOICE_NEXT,
                KeyCode::Right,
                KeyModifiers::NONE,
                1,
                1,
                2,
            ),
            (
                WORKFLOW_CHOICE_NEXT,
                KeyCode::Down,
                KeyModifiers::NONE,
                1,
                1,
                2,
            ),
            (
                WORKFLOW_CHOICE_ACCEPT,
                KeyCode::Enter,
                KeyModifiers::NONE,
                1,
                2,
                1,
            ),
        ] {
            let (mut app, _dir) = fixture(id, phase)?;
            app.modal = Some(Modal::Workflow(Box::new(Wizard::Compose(Compose {
                key: PrKey {
                    owner: "test".into(),
                    repo: "repo".into(),
                    number: 1,
                },
                head: "head".into(),
                kind: Kind::Review,
                editor: "draft".into(),
                choice: 1,
                focus,
                mention: 0,
            }))));
            let old = KeyEvent::new(code, modifiers);
            if custom {
                app.key_event(old);
                assert!(
                    matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Compose(d) if d.focus == focus && d.choice == 1)),
                    "removed {id:?}"
                );
            }
            app.key_event(active(custom, old));
            let Some(Modal::Workflow(wizard)) = &app.modal else {
                anyhow::bail!("wizard missing")
            };
            let Wizard::Compose(draft) = wizard.as_ref() else {
                anyhow::bail!("compose missing")
            };
            assert_eq!(
                (draft.focus, draft.choice, draft.editor.text()),
                (want_focus, want_choice, "draft".into()),
                "{phase} {id:?}"
            );
            app.key_event(event(KeyCode::Esc));
            assert!(app.modal.is_none());
        }
    }
    Ok(())
}

#[test]
fn unavailable_review_copy_and_help_edges_do_not_advertise_actions() -> Result<()> {
    use crate::shortcuts::{Bindings, contexts::*};
    let (mut app, _dir) = fixture(SELECTION_COPY, "custom")?;
    for context in [
        REVIEW_HOME,
        REVIEW_REPOSITORY,
        REVIEW_NAVIGATION,
        REVIEW_OVERVIEW,
        REVIEW_LOCAL_LIST,
        REVIEW_LOCAL_ROOTS,
        REVIEW_LOCAL_SELECT,
        REVIEW_IMAGE,
        REVIEW_DEFINITION,
        WORKFLOW_HOME,
        WORKFLOW_REVIEWERS,
        WORKFLOW_CHOICE,
        WORKFLOW_NEXT,
        WORKFLOW_CONFIRM,
        WORKFLOW_RESULT,
        WORKFLOW_TREES,
        WORKFLOW_DELETE,
        WORKFLOW_RESOLVE,
        WORKFLOW_BUSY,
    ] {
        assert!(
            Bindings::new(&app.config)
                .effective(SELECTION_COPY, context)
                .is_empty(),
            "unsupported copy {context:?}"
        );
    }
    app.modal = Some(Modal::Help(crate::help::State {
        query: "query".into(),
        scroll: 20,
        ..Default::default()
    }));
    for id in [NAVIGATION_FIRST, NAVIGATION_LAST] {
        assert!(
            Bindings::new(&app.config)
                .effective(id, REVIEW_HELP)
                .is_empty()
        );
    }
    app.key_event(event(KeyCode::Home));
    app.key_event(event(KeyCode::Char('a')));
    assert!(matches!(&app.modal, Some(Modal::Help(s)) if s.query.text() == "aquery"));
    app.key_event(event(KeyCode::End));
    app.key_event(event(KeyCode::Char('b')));
    assert!(matches!(&app.modal, Some(Modal::Help(s)) if s.query.text() == "aqueryb"));
    Ok(())
}

#[test]
fn native_review_filter_clear_with_extra_modifiers_survives_invalid_stored_globals() -> Result<()> {
    let (mut app, _dir) = fixture(REVIEW_HELP, "default")?;
    app.config
        .preferences
        .keyboard
        .bindings
        .insert(APP_QUIT.0.into(), key(BindingKey::Character('u'), 3));
    app.filters.focused = Some(crate::filter::Kind::PullRequests);
    app.filters
        .editor_mut(crate::filter::Kind::PullRequests)
        .insert("filter");
    app.key_event(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert!(!app.quit);
    assert_eq!(
        app.filters.editor(crate::filter::Kind::PullRequests).text(),
        ""
    );
    Ok(())
}

#[test]
fn review_workers_inherit_scoped_external_boundary() -> Result<()> {
    let (mut app, _dir) = fixture(REVIEW_HELP, "default")?;
    let scope = crate::process::fixture::WorkerScope::new(app.storage.clone(), vec![]);
    let (sender, receiver) = std::sync::mpsc::channel();
    app.spawn(move |_, _| {
        let _ = sender.send(crate::process::fixture::worker().is_some());
    });
    assert!(
        receiver.recv_timeout(Duration::from_secs(2))?,
        "worker lost fail-closed fixture"
    );
    app.shutdown();
    scope.verify()?;
    Ok(())
}

#[test]
fn explicit_review_control_predicates_keep_aliases_until_overridden() -> Result<()> {
    use crate::workflow::{Compose, Kind, Wizard};
    for flags in 0..64 {
        let modifiers = KeyModifiers::from_bits_truncate(flags);
        if !modifiers.contains(KeyModifiers::CONTROL) {
            continue;
        }
        for custom in [false, true] {
            for id in [REVIEW_LAYOUT, LOCAL_SCAN, WORKFLOW_NEXT] {
                let (mut app, _dir) = fixture(id, if custom { "custom" } else { "default" })?;
                let old = KeyEvent::new(
                    if id == REVIEW_LAYOUT {
                        KeyCode::Char('b')
                    } else {
                        KeyCode::Enter
                    },
                    modifiers,
                );
                let original = app.config.unified;
                if id == LOCAL_SCAN {
                    app.inbox_tab = InboxTab::Diffs;
                    app.local.setup = Some(local::Setup::Roots(Default::default()));
                } else if id == WORKFLOW_NEXT {
                    app.modal = Some(Modal::Workflow(Box::new(Wizard::Compose(Compose {
                        key: PrKey {
                            owner: "test".into(),
                            repo: "repo".into(),
                            number: 1,
                        },
                        head: "head".into(),
                        kind: Kind::Review,
                        editor: "draft".into(),
                        choice: 1,
                        focus: 0,
                        mention: 0,
                    }))));
                }
                app.key_event(old);
                if custom {
                    assert_eq!(app.config.unified, original);
                    assert!(app.local.error.is_none());
                    assert!(
                        !matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Confirm { .. }))
                    );
                    app.key_event(event(KeyCode::F(12)));
                }
                match id {
                    REVIEW_LAYOUT => assert_eq!(app.config.unified, !original, "{flags} {custom}"),
                    LOCAL_SCAN => assert_eq!(
                        app.local.error.as_deref(),
                        Some("Enter at least one base directory."),
                        "{flags} {custom}"
                    ),
                    _ => assert!(
                        matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Confirm { .. })),
                        "{flags} {custom}"
                    ),
                }
            }
        }
    }
    Ok(())
}

fn source_fixture(id: ActionId, phase: &str) -> Result<(App, tempfile::TempDir)> {
    let (base, dir) = fixture(id, phase)?;
    let mut app = crate::ui::tests::guide_app(dir.path());
    app.config = base.config.clone();
    app.config.unified = true;
    app.focus = Focus::Content;
    app.view = View::Diff;
    app.document = Some(crate::ui::build(&app, 100));
    app.viewport = 10;
    app.workflow.cursor = Some(40);
    app.scroll = 35;
    Ok((app, dir))
}

#[test]
fn review_source_navigation_rebinds_cursor_selection_side_and_pan() -> Result<()> {
    for (phase, custom) in PHASES {
        for (id, code, modifiers, want) in [
            (NAVIGATION_UP, KeyCode::Up, KeyModifiers::NONE, 39),
            (NAVIGATION_DOWN, KeyCode::Down, KeyModifiers::NONE, 41),
            (NAVIGATION_FAST_UP, KeyCode::Up, KeyModifiers::SUPER, 30),
            (NAVIGATION_FAST_DOWN, KeyCode::Down, KeyModifiers::SUPER, 50),
            (NAVIGATION_PAGE_UP, KeyCode::PageUp, KeyModifiers::NONE, 30),
            (
                NAVIGATION_PAGE_DOWN,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                50,
            ),
            (
                NAVIGATION_PAGE_DOWN,
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                50,
            ),
            (NAVIGATION_FIRST, KeyCode::Home, KeyModifiers::NONE, 0),
            (REVIEW_SELECT_UP, KeyCode::Up, KeyModifiers::SHIFT, 39),
            (REVIEW_SELECT_DOWN, KeyCode::Down, KeyModifiers::SHIFT, 41),
        ] {
            let (mut app, _dir) = source_fixture(id, phase)?;
            let old = KeyEvent::new(code, modifiers);
            if custom {
                app.key_event(old);
                assert_eq!(app.workflow.cursor, Some(40), "removed {id:?}");
            }
            app.key_event(active(custom, old));
            assert_eq!(app.workflow.cursor, Some(want), "{phase} {id:?}");
            assert_eq!(
                app.workflow.selection.is_some(),
                [REVIEW_SELECT_UP, REVIEW_SELECT_DOWN].contains(&id)
            );
        }
        for (id, code, modifiers, side, horizontal) in [
            (
                REVIEW_SIDE_LEFT,
                KeyCode::Left,
                KeyModifiers::ALT,
                crate::review::Side::Left,
                8,
            ),
            (
                REVIEW_SIDE_RIGHT,
                KeyCode::Right,
                KeyModifiers::ALT,
                crate::review::Side::Right,
                8,
            ),
            (
                NAVIGATION_LEFT,
                KeyCode::Left,
                KeyModifiers::NONE,
                crate::review::Side::Right,
                4,
            ),
            (
                NAVIGATION_RIGHT,
                KeyCode::Right,
                KeyModifiers::NONE,
                crate::review::Side::Right,
                12,
            ),
        ] {
            let (mut app, _dir) = source_fixture(id, phase)?;
            app.horizontal = 8;
            app.workflow.side = if id == REVIEW_SIDE_RIGHT {
                crate::review::Side::Left
            } else {
                crate::review::Side::Right
            };
            let before = app.workflow.side;
            let old = KeyEvent::new(code, modifiers);
            if custom {
                app.key_event(old);
                assert_eq!((app.workflow.side, app.horizontal), (before, 8));
            }
            app.key_event(active(custom, old));
            assert_eq!(
                (app.workflow.side, app.horizontal),
                (side, horizontal),
                "{phase} {id:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn review_rendered_help_hints_and_mouse_wheel_follow_custom_bindings() -> Result<()> {
    let (mut app, _dir) = fixture(REVIEW_HELP, "custom")?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30))?;
    terminal.draw(|frame| crate::ui::draw(frame, &mut app))?;
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(screen.contains("F12 Help"), "effective action hint missing");
    assert!(!screen.contains("? Help"));
    let target = app
        .hits
        .iter()
        .find(|(_, a)| matches!(a, Action::Help))
        .map(|(r, _)| *r)
        .ok_or_else(|| anyhow::anyhow!("visible help target"))?;
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: target.x,
        row: target.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(app.modal, Some(Modal::Help(_))));
    app.config
        .preferences
        .keyboard
        .bindings
        .insert(NAVIGATION_DOWN.0.into(), key(BindingKey::Function(10), 0));
    terminal.draw(|frame| crate::ui::draw(frame, &mut app))?;
    if let Some(Modal::Help(state)) = &mut app.modal {
        state.scroll = 5;
    }
    app.mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 10,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        matches!(&app.modal, Some(Modal::Help(state)) if state.scroll == 6),
        "wheel must dispatch scroll intent independently of physical Down"
    );
    Ok(())
}

fn finish_workers(app: &mut App) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.jobs.iter().any(|(_, job)| !job.is_finished()) {
        anyhow::ensure!(Instant::now() < deadline, "review worker timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
    for (_, job) in app.jobs.drain(..) {
        job.join()
            .map_err(|_| anyhow::anyhow!("review worker panicked"))?;
    }
    Ok(())
}
fn detail_value() -> serde_json::Value {
    serde_json::json!({"title":"Fixture PR", "body":"Body", "user":{"login":"author"},
        "head":{"sha":"head", "ref":"feature"}, "base":{"sha":"base","ref":"main"},
        "state":"open", "draft":false, "merged":false, "additions":80, "deletions":0,
        "changed_files":3, "requested_reviewers":[], "requested_teams":[]})
}

#[test]
fn review_open_link_rebinds_exact_external_url_without_launching_a_browser() -> Result<()> {
    use crate::process::fixture::{Expected, WorkerScope};
    for (phase, custom) in PHASES {
        let (mut app, _dir) = source_fixture(REVIEW_OPEN_LINK, phase)?;
        let pr = PrKey {
            owner: "example".into(),
            repo: "repo".into(),
            number: 1,
        };
        app.reviews
            .get_mut(&pr.id())
            .ok_or_else(|| anyhow::anyhow!("review missing"))?
            .detail = Some(Arc::new(crate::github::parse_detail(&pr, &detail_value())?));
        let scope = WorkerScope::new(
            app.storage.clone(),
            vec![Expected {
                program: if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                }
                .into(),
                args: vec!["https://github.com/example/repo/pull/1".into()],
                input: None,
                stdout: vec![],
                code: 0,
                execute_local_git: false,
            }],
        );
        let old = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        if custom {
            app.key_event(old);
            assert!(app.jobs.is_empty());
        }
        app.key_event(active(custom, old));
        finish_workers(&mut app)?;
        scope.verify()?;
        assert!(
            app.receiver.try_recv().is_err(),
            "successful launch should not produce an error notice"
        );
    }
    Ok(())
}

#[test]
fn inherited_worker_fixture_stays_fail_closed_after_parent_scope_drops() -> Result<()> {
    let (mut app, _dir) = fixture(REVIEW_HELP, "default")?;
    let scope = crate::process::fixture::WorkerScope::new(app.storage.clone(), vec![]);
    let (go, wait) = std::sync::mpsc::channel();
    let (result, received) = std::sync::mpsc::channel();
    app.spawn(move |_, cancel| {
        if wait.recv_timeout(Duration::from_secs(2)).is_ok() {
            let rejected = crate::process::run(
                std::process::Command::new("unconfigured-fixture-command").arg("must-not-run"),
                None,
                &cancel,
            )
            .is_err();
            let _ = result.send(rejected);
        }
    });
    drop(scope);
    assert!(crate::process::fixture::worker().is_none());
    go.send(())?;
    assert!(received.recv_timeout(Duration::from_secs(2))?);
    finish_workers(&mut app)?;
    Ok(())
}

#[test]
fn explicit_review_selection_and_side_modifiers_are_removed_as_one_binding() -> Result<()> {
    for flags in 0..64 {
        let modifiers = KeyModifiers::from_bits_truncate(flags);
        for (id, code, applicable) in [
            (
                REVIEW_SELECT_UP,
                KeyCode::Up,
                modifiers.contains(KeyModifiers::SHIFT),
            ),
            (
                REVIEW_SELECT_DOWN,
                KeyCode::Down,
                modifiers.contains(KeyModifiers::SHIFT),
            ),
            (
                REVIEW_SIDE_LEFT,
                KeyCode::Left,
                modifiers.contains(KeyModifiers::ALT),
            ),
            (
                REVIEW_SIDE_RIGHT,
                KeyCode::Right,
                modifiers.contains(KeyModifiers::ALT),
            ),
        ] {
            if !applicable {
                continue;
            }
            for custom in [false, true] {
                let (mut app, _dir) =
                    source_fixture(id, if custom { "custom" } else { "default" })?;
                app.workflow.side = if id == REVIEW_SIDE_RIGHT {
                    crate::review::Side::Left
                } else {
                    crate::review::Side::Right
                };
                let initial_side = app.workflow.side;
                let old = KeyEvent::new(code, modifiers);
                if custom {
                    app.key_event(old);
                    assert_eq!(
                        (app.workflow.cursor, app.workflow.side),
                        (Some(40), initial_side),
                        "removed {id:?} {flags}"
                    );
                }
                app.key_event(active(custom, old));
                match id {
                    REVIEW_SELECT_UP => assert_eq!(app.workflow.cursor, Some(39), "{flags}"),
                    REVIEW_SELECT_DOWN => assert_eq!(app.workflow.cursor, Some(41), "{flags}"),
                    REVIEW_SIDE_LEFT => {
                        assert_eq!(app.workflow.side, crate::review::Side::Left, "{flags}")
                    }
                    _ => assert_eq!(app.workflow.side, crate::review::Side::Right, "{flags}"),
                }
            }
        }
    }
    Ok(())
}

#[test]
fn isolated_worktree_delete_reaches_ownership_guard_without_using_global_temp() -> Result<()> {
    let (app, dir) = fixture(WORKFLOW_DELETE_CONFIRM, "default")?;
    let owned = dir.path().join("difu-review-owned");
    std::fs::create_dir(&owned)?;
    let scope = crate::process::fixture::WorkerScope::new(app.storage.clone(), vec![]);
    let error = crate::worktrees::delete(&owned, &Cancel::default())
        .err()
        .ok_or_else(|| anyhow::anyhow!("missing ownership accepted"))?;
    assert!(
        !error.to_string().contains("Not a difu temporary directory"),
        "fixture-owned root must reach the real ownership guard"
    );
    assert!(owned.is_dir());
    scope.verify()?;
    Ok(())
}

fn pr_key() -> PrKey {
    PrKey {
        owner: "example".into(),
        repo: "repo".into(),
        number: 1,
    }
}
fn compose(text: &str) -> crate::workflow::Compose {
    crate::workflow::Compose {
        key: pr_key(),
        head: "head".into(),
        kind: crate::workflow::Kind::PrComment,
        editor: text.into(),
        choice: 0,
        focus: 0,
        mention: 0,
    }
}
fn expected_gh(
    args: &[&str],
    input: Option<serde_json::Value>,
    output: serde_json::Value,
) -> crate::process::fixture::Expected {
    crate::process::fixture::Expected {
        program: "gh".into(),
        args: args.iter().map(|s| (*s).into()).collect(),
        input: input.map(|v| v.to_string().into_bytes()),
        stdout: output.to_string().into_bytes(),
        code: 0,
        execute_local_git: false,
    }
}

#[test]
fn workflow_submit_rebinds_exact_write_payload_without_remote_mutation() -> Result<()> {
    use crate::{
        process::fixture::WorkerScope,
        review::Operation,
        workflow::{Event, Wizard},
    };
    for (phase, custom) in PHASES {
        let (mut app, _dir) = fixture(WORKFLOW_SUBMIT, phase)?;
        let draft = compose("Reviewed locally\nExact comment body");
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Confirm {
            key: pr_key(),
            head: "head".into(),
            operation: draft.operation(),
            draft: Some(draft),
        })));
        let scope = WorkerScope::new(
            app.storage.clone(),
            vec![
                expected_gh(
                    &["api", "repos/example/repo/pulls/1", "--include"],
                    None,
                    detail_value(),
                ),
                expected_gh(
                    &[
                        "api",
                        "--method",
                        "POST",
                        "repos/example/repo/issues/1/comments",
                        "--input",
                        "-",
                        "--include",
                    ],
                    Some(serde_json::json!({"body":"Reviewed locally\nExact comment body"})),
                    serde_json::json!({"id":123}),
                ),
            ],
        );
        if custom {
            app.key_event(event(KeyCode::Enter));
            assert!(!app.workflow.busy && app.jobs.is_empty());
        }
        app.key_event(active(custom, event(KeyCode::Enter)));
        assert!(app.workflow.busy);
        finish_workers(&mut app)?;
        scope.verify()?;
        let Message::Workflow(Event::Written(key, Operation::PrComment { body }, result)) =
            app.receiver.try_recv()?
        else {
            anyhow::bail!("missing completed write");
        };
        assert_eq!(key, pr_key());
        assert_eq!(body, "Reviewed locally\nExact comment body");
        assert!(result.is_ok(), "{result:?}");
    }
    Ok(())
}

#[test]
fn workflow_reviewer_next_result_and_cancel_have_concrete_effects() -> Result<()> {
    use crate::workflow::{ReviewerPicker, Wizard};
    for (phase, custom) in PHASES {
        let (mut app, _dir) = fixture(WORKFLOW_REVIEWER_TOGGLE, phase)?;
        let reviewer = crate::review::Reviewer {
            team: false,
            name: "alice".into(),
        };
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Reviewers(
            ReviewerPicker {
                key: pr_key(),
                head: "head".into(),
                query: "".into(),
                options: vec![reviewer.clone()],
                chosen: Default::default(),
                selected: 0,
                loading: false,
                error: None,
            },
        ))));
        if custom {
            app.key_event(event(KeyCode::Char(' ')));
            assert!(
                matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Reviewers(p) if p.chosen.is_empty()))
            );
        }
        app.key_event(active(custom, event(KeyCode::Char(' '))));
        assert!(
            matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Reviewers(p) if p.chosen.contains(&reviewer)))
        );
        app.key_event(event(KeyCode::Enter));
        assert!(
            matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Confirm { operation: crate::review::Operation::RequestReviewers { users, teams }, .. } if users == &["alice"] && teams.is_empty()))
        );
        let (mut app, _dir) = fixture(WORKFLOW_NEXT, phase)?;
        let draft = compose("preserved draft");
        let draft_id = draft.id();
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Compose(draft))));
        let old = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        if custom {
            app.key_event(old);
            assert!(
                matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Compose(d) if d.editor.text() == "preserved draft\n"))
            );
            app.modal = Some(Modal::Workflow(Box::new(Wizard::Compose(compose(
                "preserved draft",
            )))));
        }
        app.key_event(active(custom, old));
        assert!(
            matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Confirm { draft: Some(d), .. } if d.editor.text() == "preserved draft"))
        );
        assert_eq!(
            app.workflow
                .drafts
                .get(&draft_id)
                .context("saved draft")?
                .editor
                .text(),
            "preserved draft"
        );
        app.key_event(event(KeyCode::Esc));
        assert!(
            matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Compose(d) if d.editor.text() == "preserved draft"))
        );
        let (mut app, _dir) = fixture(WORKFLOW_RESULT_BACK, phase)?;
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Result {
            notice: Notice::info("done"),
        })));
        if custom {
            app.key_event(event(KeyCode::Enter));
            assert!(app.modal.is_some());
        }
        app.key_event(active(custom, event(KeyCode::Enter)));
        assert!(app.modal.is_none());
        let (mut app, _dir) = fixture(WORKFLOW_CANCEL, phase)?;
        let cancel = Cancel::default();
        app.workflow.busy = true;
        app.workflow.conflict_cancel = Some(cancel.clone());
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Resolving {
            activity: "working".into(),
        })));
        if custom {
            app.key_event(event(KeyCode::Char('x')));
            assert!(cancel.check().is_ok());
        }
        app.key_event(active(custom, event(KeyCode::Char('x'))));
        assert!(cancel.check().is_err());
        assert!(
            matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Resolving { activity } if activity == "working"))
        );
    }
    Ok(())
}

#[test]
fn workflow_mention_navigation_acceptance_and_shift_tab_keep_native_intent() -> Result<()> {
    use crate::workflow::Wizard;
    for (phase, custom) in PHASES {
        for (id, old, selected, text) in [
            (WORKFLOW_MENTION_PREVIOUS, event(KeyCode::Up), 0, "@a"),
            (WORKFLOW_MENTION_NEXT, event(KeyCode::Down), 2, "@a"),
            (WORKFLOW_MENTION_ACCEPT, event(KeyCode::Tab), 0, "@amber "),
            (NAVIGATION_PREVIOUS_FOCUS, event(KeyCode::BackTab), 1, "@a"),
        ] {
            let (mut app, _dir) = fixture(id, phase)?;
            app.workflow.mentions.insert(
                pr_key().id(),
                crate::review::Mentions {
                    fetched: 0,
                    users: vec!["alice".into(), "amber".into(), "amy".into()],
                },
            );
            let mut draft = compose("@a");
            draft.mention = 1;
            app.modal = Some(Modal::Workflow(Box::new(Wizard::Compose(draft))));
            if custom {
                app.key_event(old);
                assert!(
                    matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Compose(d) if d.focus == 0 && d.editor.text() == "@a"))
                );
            }
            if let Some(Modal::Workflow(w)) = &mut app.modal
                && let Wizard::Compose(d) = w.as_mut()
            {
                d.mention = 1;
            }
            app.key_event(active(custom, old));
            assert!(
                matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Compose(d) if d.mention == selected && d.editor.text() == text && d.focus == usize::from(id == NAVIGATION_PREVIOUS_FOCUS))),
                "{phase} {id:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn workflow_tree_actions_rebind_and_refuse_unowned_or_outside_paths() -> Result<()> {
    use crate::{
        process::fixture::WorkerScope,
        workflow::{Event, Wizard},
        worktrees::Entry,
    };
    for (phase, custom) in PHASES {
        for (id, old) in [
            (WORKFLOW_TREE_DELETE, KeyCode::Delete),
            (WORKFLOW_TREE_DELETE, KeyCode::Enter),
            (WORKFLOW_TREE_PRUNE, KeyCode::Char('a')),
        ] {
            let (mut app, dir) = fixture(id, phase)?;
            let entries = [None, Some("active".into()), None]
                .into_iter()
                .enumerate()
                .map(|(i, reason)| Entry {
                    directory: dir.path().join(format!("difu-review-{i}")),
                    path: dir.path().join(format!("difu-review-{i}/source")),
                    reason,
                })
                .collect::<Vec<_>>();
            let wanted = if id == WORKFLOW_TREE_PRUNE {
                vec![
                    entries.first().context("first entry")?.directory.clone(),
                    entries.get(2).context("third entry")?.directory.clone(),
                ]
            } else {
                vec![entries.first().context("first entry")?.directory.clone()]
            };
            app.modal = Some(Modal::Workflow(Box::new(Wizard::Trees {
                entries,
                selected: 0,
                loading: false,
            })));
            if custom {
                app.key_event(event(old));
                assert!(
                    matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Trees { .. }))
                );
            }
            app.key_event(active(custom, event(old)));
            assert!(
                matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Delete { directories } if directories == &wanted))
            );
        }
        let (mut app, dir) = fixture(WORKFLOW_TREE_REFRESH, phase)?;
        let owned = dir.path().join("difu-review-owned");
        std::fs::create_dir(&owned)?;
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Trees {
            entries: vec![],
            selected: 0,
            loading: false,
        })));
        let scope = WorkerScope::new(app.storage.clone(), vec![]);
        if custom {
            app.key_event(event(KeyCode::Char('r')));
            assert!(app.jobs.is_empty());
        }
        app.key_event(active(custom, event(KeyCode::Char('r'))));
        finish_workers(&mut app)?;
        let Message::Workflow(Event::Trees(result)) = app.receiver.try_recv()? else {
            anyhow::bail!("missing inventory");
        };
        let entries = result.map_err(anyhow::Error::msg)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.first().context("first entry")?.directory, owned);
        assert!(entries.first().context("first entry")?.reason.is_some());
        scope.verify()?;
        drop(scope);
        for outside in [false, true] {
            let (mut app, dir) = fixture(WORKFLOW_DELETE_CONFIRM, phase)?;
            let sibling = tempfile::tempdir()?;
            let path = if outside { sibling.path() } else { dir.path() }.join("difu-review-owned");
            std::fs::create_dir(&path)?;
            app.modal = Some(Modal::Workflow(Box::new(Wizard::Delete {
                directories: vec![path.clone()],
            })));
            let scope = WorkerScope::new(app.storage.clone(), vec![]);
            if custom {
                app.key_event(event(KeyCode::Enter));
                assert!(app.jobs.is_empty());
            }
            app.key_event(active(custom, event(KeyCode::Enter)));
            finish_workers(&mut app)?;
            let Message::Workflow(Event::Deleted(result)) = app.receiver.try_recv()? else {
                anyhow::bail!("missing deletion result");
            };
            assert!(result.is_err());
            assert!(path.is_dir());
            if outside {
                assert!(
                    result
                        .err()
                        .context("refusal")?
                        .contains("Not a difu temporary directory")
                );
            }
            scope.verify()?;
        }
    }
    Ok(())
}

#[test]
fn workflow_mentions_refresh_rebinds_exact_read_and_updates_cache() -> Result<()> {
    use crate::{
        process::fixture::WorkerScope,
        workflow::{Event, Wizard},
    };
    let query = "query($owner:String!,$repo:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){participants(first:100,after:$cursor){nodes{login} pageInfo{hasNextPage endCursor}}}}}";
    for (phase, custom) in PHASES {
        let (mut app, _dir) = fixture(WORKFLOW_MENTIONS_REFRESH, phase)?;
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Compose(compose("@a")))));
        let scope = WorkerScope::new(
            app.storage.clone(),
            vec![
                expected_gh(
                    &["api", "--method", "GET", "users/example", "--include"],
                    None,
                    serde_json::json!({"type":"User"}),
                ),
                expected_gh(
                    &[
                        "api",
                        "--method",
                        "POST",
                        "graphql",
                        "--input",
                        "-",
                        "--include",
                    ],
                    Some(
                        serde_json::json!({"query":query,"variables":{"owner":"example","repo":"repo","number":1,"cursor":null}}),
                    ),
                    serde_json::json!({"data":{"repository":{"pullRequest":{"participants":{"nodes":[{"login":"alice"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}),
                ),
            ],
        );
        let old = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        if custom {
            app.key_event(old);
            assert!(app.jobs.is_empty());
        }
        app.key_event(active(custom, old));
        finish_workers(&mut app)?;
        scope.verify()?;
        let Message::Workflow(Event::Mentions(key, result)) = app.receiver.try_recv()? else {
            anyhow::bail!("missing mentions result");
        };
        assert_eq!(key, pr_key());
        assert_eq!(result.map_err(anyhow::Error::msg)?.users, ["alice"]);
        assert_eq!(
            crate::review::cached_mentions(&app.storage, &key)?.users,
            ["alice"]
        );
    }
    Ok(())
}

#[test]
fn explicit_review_chapter_fast_and_mention_modifier_predicates_keep_precedence() -> Result<()> {
    for modifiers in [
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyModifiers::SUPER | KeyModifiers::ALT,
        KeyModifiers::ALT | KeyModifiers::META,
    ] {
        for (id, code, target) in [
            (REVIEW_CHAPTER_NEXT, KeyCode::Down, 1),
            (REVIEW_CHAPTER_PREVIOUS, KeyCode::Up, 0),
        ] {
            for custom in [false, true] {
                let (mut app, _dir) =
                    source_fixture(id, if custom { "custom" } else { "default" })?;
                app.view = View::Guide;
                app.document = Some(crate::ui::build(&app, 100));
                let sections = &app.document.as_ref().context("document")?.sections;
                let initial = sections.get(1 - target).context("initial chapter")?.start;
                let wanted = sections.get(target).context("target chapter")?.start;
                app.scroll = initial;
                let old = KeyEvent::new(code, modifiers);
                if custom {
                    app.key_event(old);
                    assert_eq!(app.scroll, initial, "removed chapter {modifiers:?}");
                }
                app.key_event(active(custom, old));
                assert_eq!(app.scroll, wanted, "chapter {modifiers:?}");
            }
        }
    }
    for modifiers in [
        KeyModifiers::CONTROL | KeyModifiers::SUPER,
        KeyModifiers::SUPER | KeyModifiers::META,
    ] {
        for custom in [false, true] {
            let (mut app, _dir) = source_fixture(
                NAVIGATION_FAST_DOWN,
                if custom { "custom" } else { "default" },
            )?;
            let old = KeyEvent::new(KeyCode::Down, modifiers);
            if custom {
                app.key_event(old);
                assert_eq!(app.workflow.cursor, Some(40));
            }
            app.key_event(active(custom, old));
            assert_eq!(app.workflow.cursor, Some(50), "fast {modifiers:?}");
        }
    }
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SUPER | KeyModifiers::META,
    ] {
        for custom in [false, true] {
            let (mut app, _dir) = fixture(
                WORKFLOW_MENTION_ACCEPT,
                if custom { "custom" } else { "default" },
            )?;
            app.workflow.mentions.insert(
                pr_key().id(),
                crate::review::Mentions {
                    fetched: 0,
                    users: vec!["alice".into()],
                },
            );
            app.modal = Some(Modal::Workflow(Box::new(crate::workflow::Wizard::Compose(
                compose("@a"),
            ))));
            let old = KeyEvent::new(KeyCode::Tab, modifiers);
            if custom {
                app.key_event(old);
                assert!(
                    matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), crate::workflow::Wizard::Compose(d) if d.editor.text() == "@a"))
                );
            }
            app.key_event(active(custom, old));
            assert!(
                matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), crate::workflow::Wizard::Compose(d) if d.editor.text() == "@alice ")),
                "mention {modifiers:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn review_views_locate_cancel_pin_and_filter_accept_apply_saved_bindings() -> Result<()> {
    for (phase, custom) in PHASES {
        for (id, code, wanted) in [
            (REVIEW_TAB_1, '1', View::Overview),
            (REVIEW_TAB_2, '2', View::Diff),
            (REVIEW_TAB_3, '3', View::Guide),
            (REVIEW_PREVIOUS_VIEW, '[', View::Overview),
            (REVIEW_NEXT_VIEW, ']', View::Guide),
        ] {
            let (mut app, _dir) = source_fixture(id, phase)?;
            app.view = if id == REVIEW_TAB_2 {
                View::Guide
            } else {
                View::Diff
            };
            let before = app.view;
            if custom {
                app.key_event(event(KeyCode::Char(code)));
                assert_eq!(app.view, before);
            }
            app.key_event(active(custom, event(KeyCode::Char(code))));
            assert_eq!(app.view, wanted);
            assert!(!app.home);
            assert_eq!(app.opened.as_deref(), Some(pr_key().id().as_str()));
        }
        let (mut app, dir) = source_fixture(REVIEW_LOCATE, phase)?;
        app.reviews.get_mut(&pr_key().id()).context("review")?.root = Some(dir.path().into());
        if custom {
            app.key_event(event(KeyCode::Char('l')));
            assert!(app.modal.is_none());
        }
        app.key_event(active(custom, event(KeyCode::Char('l'))));
        assert!(
            matches!(&app.modal, Some(Modal::Clone { value, key }) if value.text() == dir.path().display().to_string() && key == &pr_key().id())
        );
        let (mut app, _dir) = source_fixture(REVIEW_CANCEL, phase)?;
        let generation = Cancel::default();
        let preparation = Cancel::default();
        let review = app.reviews.get_mut(&pr_key().id()).context("review")?;
        review.generation_requested = true;
        review.preparation = Some(preparation.clone());
        review.generation = Some(Generation {
            id: 1,
            cancel: generation.clone(),
            started: Instant::now(),
            activity: "working".into(),
        });
        if custom {
            app.key_event(event(KeyCode::Char('x')));
            assert!(generation.check().is_ok() && preparation.check().is_ok());
        }
        app.key_event(active(custom, event(KeyCode::Char('x'))));
        assert!(generation.check().is_err() && preparation.check().is_err());
        assert!(!app.review().context("review")?.generation_requested);
        let (mut app, _dir) = fixture(REVIEW_PIN_REPOSITORY, phase)?;
        app.inbox_tab = InboxTab::Repositories;
        app.repository_options = vec!["example/repo".into()];
        app.repo_selected = Some("example/repo".into());
        if custom {
            app.key_event(event(KeyCode::Char('*')));
            assert!(app.config.pinned_repositories.is_empty());
        }
        app.key_event(active(custom, event(KeyCode::Char('*'))));
        assert!(
            app.storage
                .load_config()?
                .pinned_repositories
                .contains("example/repo")
        );
        let (mut app, _dir) = fixture(REVIEW_FILTER_ACCEPT, phase)?;
        app.filters.focused = Some(crate::filter::Kind::PullRequests);
        app.filters
            .editor_mut(crate::filter::Kind::PullRequests)
            .insert("fixture");
        if custom {
            app.key_event(event(KeyCode::Enter));
            assert!(app.filters.focused.is_some());
        }
        app.key_event(active(custom, event(KeyCode::Enter)));
        assert!(app.filters.focused.is_none());
        assert_eq!(app.focus, Focus::Navigation);
        assert!(
            app.filters
                .editor(crate::filter::Kind::PullRequests)
                .text()
                .starts_with("fixture")
        );
    }
    Ok(())
}

#[test]
fn review_refresh_and_state_keys_reach_exact_repository_search() -> Result<()> {
    use crate::process::fixture::WorkerScope;
    for (phase, custom) in PHASES {
        for (id, code, state, flag) in [
            (REVIEW_REFRESH, 'r', PrState::Open, Some("--state=open")),
            (REVIEW_NEXT_STATE, 's', PrState::Merged, Some("--merged")),
            (REVIEW_NEXT_STATE, ']', PrState::Merged, Some("--merged")),
            (REVIEW_PREVIOUS_STATE, '[', PrState::All, None),
        ] {
            let (mut app, _dir) = fixture(id, phase)?;
            app.inbox_tab = InboxTab::Repositories;
            app.repository = Some("example/repo".into());
            app.repository_state = PrState::Open;
            let mut args = vec![
                "search",
                "prs",
                "--repo=example/repo",
                "--sort=updated",
                "--order=desc",
                "--limit=1000",
                "--json=number,title,url,author,updatedAt,createdAt,isDraft,state",
            ];
            if let Some(flag) = flag {
                args.push(flag);
            }
            let scope = WorkerScope::new(
                app.storage.clone(),
                vec![expected_gh(&args, None, serde_json::json!([]))],
            );
            if custom {
                app.key_event(event(KeyCode::Char(code)));
                assert!(app.jobs.is_empty());
                assert_eq!(app.state(), PrState::Open);
            }
            app.key_event(active(custom, event(KeyCode::Char(code))));
            finish_workers(&mut app)?;
            scope.verify()?;
            assert_eq!(app.state(), state);
            assert!(
                app.receiver
                    .try_iter()
                    .any(|m| matches!(m, Message::InboxFinished(_)))
            );
        }
    }
    Ok(())
}

#[test]
fn local_scan_and_refresh_have_isolated_inventory_effects() -> Result<()> {
    use crate::process::fixture::WorkerScope;
    for (phase, custom) in PHASES {
        let (mut app, dir) = fixture(LOCAL_SCAN, phase)?;
        let root = dir.path().join("scan");
        std::fs::create_dir(&root)?;
        app.inbox_tab = InboxTab::Diffs;
        app.local.setup = Some(local::Setup::Roots(root.display().to_string().into()));
        let scope = WorkerScope::new(app.storage.clone(), vec![]);
        let old = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        if custom {
            app.key_event(old);
            assert!(app.jobs.is_empty());
        }
        app.key_event(active(custom, old));
        finish_workers(&mut app)?;
        let Message::Local(result) = app.receiver.try_recv()? else {
            anyhow::bail!("missing scan result");
        };
        app.local_receive(result);
        assert!(
            matches!(&app.local.setup, Some(local::Setup::Select { roots, repos, .. }) if roots == &[root] && repos.is_empty())
        );
        scope.verify()?;
        drop(scope);
        let (mut app, dir) = fixture(LOCAL_REFRESH, phase)?;
        app.inbox_tab = InboxTab::Diffs;
        app.config.local_diff_roots.push(dir.path().into());
        let scope = WorkerScope::new(app.storage.clone(), vec![]);
        app.local.entries = vec![dir.path().join("stale")];
        if custom {
            app.key_event(event(KeyCode::Char('r')));
            assert_eq!(app.local.entries.len(), 1);
            assert!(app.jobs.is_empty());
        }
        app.key_event(active(custom, event(KeyCode::Char('r'))));
        finish_workers(&mut app)?;
        let Message::Local(result) = app.receiver.try_recv()? else {
            anyhow::bail!("missing local inventory");
        };
        app.local_receive(result);
        assert!(app.local.entries.is_empty());
        assert!(!app.local.loading);
        scope.verify()?;
    }
    Ok(())
}

fn job_at_fixture_service(
    app: &mut App,
    dispatch: impl FnOnce(&mut App),
) -> Result<crate::agents::Job> {
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
    };
    struct Service(Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    const SERVER: &str = r#"
import json,socket,sys
server=socket.socket(socket.AF_UNIX)
server.bind(sys.argv[1]);server.listen();server.settimeout(3)
print('ready',flush=True)
while True:
    client,_=server.accept()
    with client:
        client.settimeout(3)
        line=client.makefile('rb').readline()
        if not line: continue
        request=json.loads(line)
        if request=='ServiceVersion': reply={'ServiceVersion':{'version':sys.argv[2]}}
        else: reply={'Error':'isolated fixture: provider execution refused'}
        client.sendall((json.dumps(reply)+'\n').encode())
        if request!='ServiceVersion':
            print(json.dumps(request),flush=True)
            break
"#;
    let socket = crate::agents::server::socket(&app.storage)?;
    let mut service = Service(
        Command::new("python3")
            .args(["-u", "-c", SERVER])
            .arg(socket)
            .arg(env!("CARGO_PKG_VERSION"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = service
        .0
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("fixture stdout"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let result = (|| -> Result<crate::agents::Job> {
        anyhow::ensure!(
            rx.recv_timeout(Duration::from_secs(3))?? == "ready",
            "fixture readiness"
        );
        dispatch(app);
        let request: crate::agents::Request =
            serde_json::from_str(&rx.recv_timeout(Duration::from_secs(3))??)?;
        finish_workers(app)?;
        match request {
            crate::agents::Request::Launch { job } => Ok(*job),
            _ => anyhow::bail!("wrong service request"),
        }
    })();
    let _ = service.0.kill();
    let _ = service.0.wait();
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("fixture reader panicked"))?;
    result
}

#[test]
fn review_generation_and_resolution_rebind_exact_jobs_without_provider_execution() -> Result<()> {
    use crate::{
        agents::Job,
        workflow::{Event, Wizard},
    };
    for (phase, custom) in PHASES {
        let (mut app, dir) = source_fixture(REVIEW_REGENERATE, phase)?;
        let detail = crate::github::parse_detail(&pr_key(), &detail_value())?;
        let review = app.reviews.get_mut(&pr_key().id()).context("review")?;
        review.detail = Some(Arc::new(detail));
        review.root = Some(dir.path().into());
        let expected_snapshot =
            serde_json::to_value(review.snapshot.as_ref().context("snapshot")?)?;
        let model = app.config.model.clone();
        let scope = crate::process::fixture::WorkerScope::new(app.storage.clone(), vec![]);
        let job = job_at_fixture_service(&mut app, |app| {
            if custom {
                app.key_event(event(KeyCode::Char('g')));
                assert!(app.jobs.is_empty() && app.review().is_some_and(|r| r.guide.is_some()));
            }
            app.key_event(active(custom, event(KeyCode::Char('g'))));
        })?;
        let Job::Guide {
            root,
            pr,
            snapshot,
            model: used,
        } = job
        else {
            anyhow::bail!("wrong guide job");
        };
        assert_eq!(root, dir.path());
        assert_eq!(pr.key, pr_key());
        assert_eq!(pr.head, "head");
        assert_eq!(used, model);
        assert_eq!(serde_json::to_value(snapshot)?, expected_snapshot);
        assert!(
            matches!(app.receiver.try_recv()?, Message::Guide(_, _, _, Err(e)) if e.contains("isolated fixture"))
        );
        scope.verify()?;
        drop(scope);
        let (mut app, dir) = fixture(WORKFLOW_RESOLVE, phase)?;
        app.config
            .repositories
            .insert(pr_key().repository(), dir.path().into());
        let model = app.config.model.clone();
        app.modal = Some(Modal::Workflow(Box::new(Wizard::Resolve {
            key: pr_key(),
            head: "head".into(),
            model: model.clone(),
        })));
        let scope = crate::process::fixture::WorkerScope::new(app.storage.clone(), vec![]);
        let job = job_at_fixture_service(&mut app, |app| {
            if custom {
                app.key_event(event(KeyCode::Enter));
                assert!(!app.workflow.busy && app.jobs.is_empty());
            }
            app.key_event(active(custom, event(KeyCode::Enter)));
        })?;
        let Job::Conflict {
            root,
            key,
            head,
            model: used,
        } = job
        else {
            anyhow::bail!("wrong conflict job");
        };
        assert_eq!(root, dir.path());
        assert_eq!(key, pr_key());
        assert_eq!(head, "head");
        assert_eq!(used, model);
        assert!(
            matches!(app.receiver.try_recv()?, Message::Workflow(Event::Resolved(_, Err(e))) if e.contains("isolated fixture"))
        );
        scope.verify()?;
    }
    Ok(())
}

#[test]
fn review_image_browser_rebinds_exact_url_and_preserves_modal_escape() -> Result<()> {
    use crate::process::fixture::{Expected, WorkerScope};
    for (phase, custom) in PHASES {
        let (mut app, _dir) = fixture(REVIEW_IMAGE_BROWSER, phase)?;
        let pr = crate::github::parse_detail(&pr_key(), &detail_value())?;
        app.modal = Some(Modal::Image(crate::images::Request::new(
            "https://example.invalid/image.png".into(),
            "fixture image".into(),
            &pr,
        )));
        let scope = WorkerScope::new(
            app.storage.clone(),
            vec![Expected {
                program: if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                }
                .into(),
                args: vec!["https://example.invalid/image.png".into()],
                input: None,
                stdout: vec![],
                code: 0,
                execute_local_git: false,
            }],
        );
        if custom {
            app.key_event(event(KeyCode::Char('o')));
            assert!(app.jobs.is_empty());
        }
        app.key_event(active(custom, event(KeyCode::Char('o'))));
        finish_workers(&mut app)?;
        scope.verify()?;
        assert!(matches!(app.modal, Some(Modal::Image(_))));
        app.key_event(event(KeyCode::Esc));
        assert!(app.modal.is_none());
    }
    Ok(())
}

#[test]
fn review_hunk_context_aliases_rebind_both_real_expansion_edges() -> Result<()> {
    use crate::context::{Expansion, FileContext, FileState};
    for (phase, custom) in PHASES {
        for (id, old, want) in [
            (REVIEW_HUNK_LESS, event(KeyCode::Char('{')), 1),
            (REVIEW_HUNK_MORE, event(KeyCode::Char('}')), 3),
            (
                REVIEW_HUNK_LESS,
                KeyEvent::new(KeyCode::Char('['), KeyModifiers::SHIFT),
                1,
            ),
            (
                REVIEW_HUNK_MORE,
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::SHIFT),
                3,
            ),
        ] {
            let (mut app, dir) = source_fixture(id, phase)?;
            let review = app.reviews.get_mut(&pr_key().id()).context("review")?;
            let mut snapshot = (**review.snapshot.as_ref().context("snapshot")?).clone();
            let file = snapshot.files.first_mut().context("file")?;
            let original = file.hunks.first_mut().context("hunk")?;
            let lines = original.lines.clone();
            original.header = "@@ -1,0 +21,10 @@".into();
            original.lines = lines.get(20..30).context("hunk source")?.into();
            let hunk = original.id.clone();
            let data = FileContext::new(file, lines)?;
            review.context.insert(
                file.path.clone(),
                FileState {
                    data: Some(Arc::new(data)),
                    ..Default::default()
                },
            );
            review
                .expanded
                .insert(hunk.clone(), Expansion { above: 2, below: 2 });
            review.snapshot = Some(Arc::new(snapshot));
            review.root = Some(dir.path().into());
            app.document = Some(crate::ui::build(&app, 100));
            let cursor = app
                .document
                .as_ref()
                .context("document")?
                .rows
                .iter()
                .position(|r| r.right.hunk.as_deref() == Some(&hunk))
                .context("source row")?;
            app.workflow.cursor = Some(cursor);
            if custom {
                app.key_event(old);
                let e = &app
                    .review()
                    .context("review")?
                    .expanded
                    .get(&hunk)
                    .context("expanded hunk")?;
                assert_eq!((e.above, e.below), (2, 2));
            }
            app.key_event(active(custom, old));
            let e = &app
                .review()
                .context("review")?
                .expanded
                .get(&hunk)
                .context("expanded hunk")?;
            assert_eq!((e.above, e.below), (want, want));
        }
    }
    Ok(())
}

#[test]
fn review_definition_and_model_navigation_use_actions_with_raw_editing() -> Result<()> {
    for (phase, custom) in PHASES {
        for (id, code, mods, scroll, horizontal) in [
            (NAVIGATION_UP, KeyCode::Up, KeyModifiers::NONE, 19, 8),
            (NAVIGATION_DOWN, KeyCode::Down, KeyModifiers::NONE, 21, 8),
            (NAVIGATION_FAST_UP, KeyCode::Up, KeyModifiers::SUPER, 10, 8),
            (
                NAVIGATION_FAST_DOWN,
                KeyCode::Down,
                KeyModifiers::SUPER,
                30,
                8,
            ),
            (
                NAVIGATION_PAGE_UP,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                13,
                8,
            ),
            (
                NAVIGATION_PAGE_DOWN,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                27,
                8,
            ),
            (
                NAVIGATION_PAGE_DOWN,
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                27,
                8,
            ),
            (NAVIGATION_FIRST, KeyCode::Home, KeyModifiers::NONE, 0, 8),
            (NAVIGATION_LAST, KeyCode::End, KeyModifiers::NONE, 93, 8),
            (NAVIGATION_LEFT, KeyCode::Left, KeyModifiers::NONE, 20, 4),
            (NAVIGATION_RIGHT, KeyCode::Right, KeyModifiers::NONE, 20, 12),
        ] {
            let (mut app, dir) = fixture(id, phase)?;
            let cancel = Cancel::default();
            app.modal = Some(Modal::Definition(crate::navigation::Viewer {
                id: 1,
                request: crate::navigation::Request {
                    root: dir.path().into(),
                    revision: "head".into(),
                    path: "test.rs".into(),
                    line: 1,
                    column: 0,
                },
                output: Some(Ok(Arc::new(crate::navigation::Definition {
                    path: "test.rs".into(),
                    revision: "head".into(),
                    line: 1,
                    column: 0,
                    source: "line\n".repeat(100),
                }))),
                scroll: 20,
                horizontal: 8,
                viewport: 7,
                cancel: cancel.clone(),
            }));
            let old = KeyEvent::new(code, mods);
            if custom {
                app.key_event(old);
                assert!(
                    matches!(&app.modal, Some(Modal::Definition(v)) if (v.scroll,v.horizontal) == (20,8))
                );
            }
            app.key_event(active(custom, old));
            assert!(
                matches!(&app.modal, Some(Modal::Definition(v)) if (v.scroll,v.horizontal) == (scroll,horizontal)),
                "{phase} {id:?}"
            );
            app.key_event(event(KeyCode::Esc));
            assert!(cancel.check().is_err());
            assert!(app.modal.is_none());
        }
        for (id, code, selected) in [
            (NAVIGATION_UP, KeyCode::Up, 0),
            (NAVIGATION_DOWN, KeyCode::Down, 2),
            (NAVIGATION_ACTIVATE, KeyCode::Enter, 1),
        ] {
            let (mut app, _dir) = fixture(id, phase)?;
            app.models = vec![ModelInfo {
                id: "fixture-model".into(),
                name: "Fixture".into(),
                efforts: vec!["low".into(), "medium".into(), "high".into()],
            }];
            app.modal = Some(Modal::Models {
                selected: 1,
                effort: 0,
                query: "".into(),
            });
            if custom {
                app.key_event(event(code));
                assert!(app.modal.is_some());
                if let Some(Modal::Models {
                    selected, query, ..
                }) = &mut app.modal
                {
                    *selected = 1;
                    query.clear();
                }
            }
            app.key_event(active(custom, event(code)));
            if id == NAVIGATION_ACTIVATE {
                assert!(app.modal.is_none());
                assert_eq!(
                    app.storage.load_config()?.model,
                    ModelChoice {
                        model: "fixture-model".into(),
                        effort: "medium".into()
                    }
                );
            } else {
                assert!(
                    matches!(&app.modal, Some(Modal::Models { selected: i, .. }) if *i == selected)
                );
            }
        }
    }
    Ok(())
}

#[test]
fn review_source_copy_rebinds_literal_aliases_and_preserves_selection() -> Result<()> {
    for (phase, custom) in PHASES {
        for old in [
            event(KeyCode::Char('c')),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER),
        ] {
            let (mut app, _dir) = source_fixture(SELECTION_COPY, phase)?;
            let row = app
                .document
                .as_ref()
                .context("document")?
                .rows
                .iter()
                .position(|r| {
                    matches!(
                        &r.right.target,
                        Some(crate::workflow::Target::Code { new: Some(40), .. })
                    )
                })
                .context("source row")?;
            let target = app
                .document
                .as_ref()
                .context("document")?
                .rows
                .get(row)
                .and_then(|r| r.right.target.clone())
                .context("source target")?;
            app.workflow.cursor = Some(row);
            app.workflow.selection = target.line(crate::review::Side::Right).map(|mut a| {
                a.start = 39;
                a
            });
            let before = app.workflow.selection.clone();
            if custom {
                app.key_event(old);
                assert!(app.clipboard.is_none());
            }
            app.key_event(active(custom, old));
            assert_eq!(app.clipboard.as_deref(), Some("line_39\nline_40"));
            assert_eq!(app.workflow.selection, before);
            assert_eq!(app.workflow.cursor, Some(row));
            app.clipboard = Some("sentinel".into());
            app.home = true;
            app.key_event(active(custom, old));
            assert_eq!(app.clipboard.as_deref(), Some("sentinel"));
            assert_eq!(app.workflow.selection, before);
        }
    }
    Ok(())
}

#[test]
fn local_list_activation_loads_an_owned_real_git_snapshot() -> Result<()> {
    for (phase, custom) in PHASES {
        let (mut app, dir) = fixture(NAVIGATION_ACTIVATE, phase)?;
        let root = dir.path().join("repository");
        std::fs::create_dir(&root)?;
        let git = |args: &[&str]| {
            crate::process::checked(
                crate::repo::git(&root)
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
            )
        };
        git(&["init", "-b", "main"])?;
        std::fs::write(root.join("sample.txt"), "original\n")?;
        git(&["add", "sample.txt"])?;
        git(&["commit", "-m", "fixture"])?;
        std::fs::write(root.join("sample.txt"), "changed fixture\n")?;
        app.inbox_tab = InboxTab::Diffs;
        app.local.entries = vec![root.clone()];
        if custom {
            app.key_event(event(KeyCode::Enter));
            assert!(app.home && app.jobs.is_empty());
        }
        app.key_event(active(custom, event(KeyCode::Enter)));
        finish_workers(&mut app)?;
        let Message::Local(output) = app.receiver.try_recv()? else {
            anyhow::bail!("missing local open result");
        };
        app.local_receive(output);
        assert!(!app.home, "{}", app.notice);
        assert_eq!(app.view, View::Diff);
        let snapshot = app
            .review()
            .context("review")?
            .snapshot
            .as_ref()
            .context("snapshot")?;
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files.first().context("file")?.path, "sample.txt");
        assert!(
            snapshot
                .files
                .first()
                .context("file")?
                .hunks
                .first()
                .context("hunk")?
                .lines
                .iter()
                .any(|line| line.text == "changed fixture")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("sample.txt"))?,
            "changed fixture\n"
        );
    }
    Ok(())
}

#[test]
fn review_modifier_priority_and_worktree_lock_cancellation_are_preserved() -> Result<()> {
    use crate::workflow::Wizard;
    let (mut app, _dir) = source_fixture(REVIEW_HELP, "default")?;
    app.key_event(KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::SUPER,
    ));
    assert_eq!(app.workflow.cursor, Some(41));
    assert!(app.workflow.selection.is_some());
    app.modal = Some(Modal::Help(crate::help::State {
        query: "text".into(),
        scroll: 20,
        rows: 100,
        viewport: 10,
    }));
    app.key_event(KeyEvent::new(
        KeyCode::Up,
        KeyModifiers::SHIFT | KeyModifiers::SUPER,
    ));
    assert!(
        matches!(&app.modal, Some(Modal::Help(s)) if s.scroll == 20 && s.query.selected_text().as_deref() == Some("text"))
    );
    let (mut app, dir) = fixture(WORKFLOW_DELETE_CONFIRM, "custom")?;
    let owned = dir.path().join("difu-review-locked");
    let source = owned.join("source");
    std::fs::create_dir_all(&source)?;
    let scope = crate::process::fixture::WorkerScope::new(app.storage.clone(), vec![]);
    let lease = crate::worktrees::register(&owned, dir.path(), &source)?;
    app.modal = Some(Modal::Workflow(Box::new(Wizard::Delete {
        directories: vec![owned.clone()],
    })));
    app.key_event(event(KeyCode::F(12)));
    finish_workers(&mut app)?;
    assert!(
        matches!(app.receiver.try_recv()?, Message::Workflow(crate::workflow::Event::Deleted(Err(e))) if e.contains("Active worktree"))
    );
    assert!(source.is_dir());
    drop(lease);
    let cancel = Cancel::default();
    cancel.cancel();
    assert!(crate::worktrees::list(&cancel).is_err());
    scope.verify()?;
    Ok(())
}

#[test]
fn workflow_delete_and_prune_remove_only_registered_owned_worktrees() -> Result<()> {
    use crate::{
        process::fixture::{Expected, WorkerScope},
        workflow::{Event, Wizard},
        worktrees::Entry,
    };
    for (phase, custom) in PHASES {
        for prune in [false, true] {
            let action = if prune {
                WORKFLOW_TREE_PRUNE
            } else {
                WORKFLOW_TREE_DELETE
            };
            let (mut app, dir) = fixture(action, phase)?;
            let root = dir.path().join("repository");
            std::fs::create_dir(&root)?;
            let git = |args: &[&str]| {
                crate::process::checked(
                    crate::repo::git(&root)
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
                )
            };
            git(&["init", "-b", "main"])?;
            std::fs::write(root.join("file"), "owned\n")?;
            git(&["add", "file"])?;
            git(&["commit", "-m", "fixture"])?;
            let mut entries = vec![];
            let mut expected = vec![];
            for i in 0..2 {
                let directory = dir.path().join(format!("difu-review-owned-{i}"));
                std::fs::create_dir(&directory)?;
                let source = directory.join("source");
                crate::process::checked(
                    crate::repo::git(&root)
                        .args(["worktree", "add", "--detach"])
                        .arg(&source)
                        .arg("HEAD"),
                    &Cancel::default(),
                )?;
                drop(crate::worktrees::register(&directory, &root, &source)?);
                entries.push(Entry {
                    directory,
                    path: source.clone(),
                    reason: None,
                });
                if prune || i == 0 {
                    let canonical_root = root.canonicalize()?;
                    let commands = [
                        (
                            canonical_root.as_path(),
                            vec!["worktree", "list", "--porcelain", "-z"],
                        ),
                        (
                            source.as_path(),
                            vec![
                                "config",
                                "--name-only",
                                "--get-regexp",
                                "^filter\\..*\\.(clean|smudge|process|required)$",
                            ],
                        ),
                        (
                            source.as_path(),
                            vec![
                                "status",
                                "--porcelain",
                                "--untracked-files=all",
                                "--ignored",
                            ],
                        ),
                        (
                            canonical_root.as_path(),
                            vec![
                                "config",
                                "--name-only",
                                "--get-regexp",
                                "^filter\\..*\\.(clean|smudge|process|required)$",
                            ],
                        ),
                        (
                            canonical_root.as_path(),
                            vec![
                                "worktree",
                                "remove",
                                source.to_str().context("UTF-8 fixture path")?,
                            ],
                        ),
                    ];
                    for (path, args) in commands {
                        let mut command = crate::repo::git(path);
                        command.args(args);
                        expected.push(Expected {
                            program: "git".into(),
                            args: command
                                .get_args()
                                .map(|a| a.to_string_lossy().into_owned())
                                .collect(),
                            input: None,
                            stdout: vec![],
                            code: 0,
                            execute_local_git: true,
                        });
                    }
                }
            }
            let paths = entries
                .iter()
                .map(|e| e.directory.clone())
                .collect::<Vec<_>>();
            app.modal = Some(Modal::Workflow(Box::new(Wizard::Trees {
                entries,
                selected: 0,
                loading: false,
            })));
            let scope = WorkerScope::new(app.storage.clone(), expected);
            let old = event(if prune {
                KeyCode::Char('a')
            } else {
                KeyCode::Delete
            });
            if custom {
                app.key_event(old);
                assert!(
                    matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Trees { .. }))
                );
            }
            app.key_event(active(custom, old));
            // The second committed binding exercises the actual confirmation path.
            if custom {
                app.config.preferences.keyboard.bindings.insert(
                    WORKFLOW_DELETE_CONFIRM.0.into(),
                    key(BindingKey::Function(10), 0),
                );
                app.key_event(event(KeyCode::Enter));
                assert!(!app.workflow.busy);
            }
            app.key_event(event(if custom {
                KeyCode::F(10)
            } else {
                KeyCode::Enter
            }));
            finish_workers(&mut app)?;
            let Message::Workflow(Event::Deleted(result)) = app.receiver.try_recv()? else {
                anyhow::bail!("missing deletion result");
            };
            assert!(result.is_ok(), "{result:?}");
            scope.verify()?;
            assert!(!paths.first().context("first path")?.exists());
            assert_eq!(paths.get(1).context("second path")?.exists(), !prune);
            assert_eq!(std::fs::read_to_string(root.join("file"))?, "owned\n");
        }
    }
    Ok(())
}

#[test]
fn review_editor_copy_super_predicate_preserves_selection_before_control_quit() -> Result<()> {
    for (phase, custom) in PHASES {
        for modifiers in [
            KeyModifiers::SUPER | KeyModifiers::ALT,
            KeyModifiers::SUPER | KeyModifiers::CONTROL,
            KeyModifiers::SUPER | KeyModifiers::SHIFT | KeyModifiers::META,
        ] {
            let (mut app, _dir) = fixture(SELECTION_COPY, phase)?;
            let mut query: crate::editor::Editor = "selected editor".into();
            query.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
            app.modal = Some(Modal::Help(crate::help::State {
                query,
                ..Default::default()
            }));
            let old = KeyEvent::new(KeyCode::Char('c'), modifiers);
            if custom {
                assert!(!crate::shortcuts::Bindings::new(&app.config).matches(
                    SELECTION_COPY,
                    app.shortcut_context(),
                    old
                ));
                app.key_event(old);
                assert_eq!(
                    app.clipboard.as_deref(),
                    modifiers
                        .contains(KeyModifiers::CONTROL)
                        .then_some("selected editor")
                );
                assert!(!app.quit);
            }
            app.key_event(active(custom, old));
            assert_eq!(app.clipboard.as_deref(), Some("selected editor"));
            assert!(!app.quit);
            assert!(
                matches!(&app.modal, Some(Modal::Help(s)) if s.query.selected_text().as_deref() == Some("selected editor"))
            );
        }
    }
    let (mut app, _dir) = source_fixture(SELECTION_COPY, "default")?;
    app.key_event(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    ));
    assert!(app.clipboard.is_none());
    Ok(())
}

#[test]
fn review_help_non_shift_arrow_predicate_preserves_scroll_and_editor_selection() -> Result<()> {
    for (phase, custom) in PHASES {
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::META,
        ] {
            let (mut app, _dir) = fixture(NAVIGATION_UP, phase)?;
            app.modal = Some(Modal::Help(crate::help::State {
                query: "query".into(),
                scroll: 20,
                rows: 100,
                viewport: 10,
            }));
            let old = KeyEvent::new(KeyCode::Up, modifiers);
            if custom {
                app.key_event(old);
                assert!(matches!(&app.modal, Some(Modal::Help(s)) if s.scroll == 20));
            }
            app.key_event(active(custom, old));
            assert!(
                matches!(&app.modal, Some(Modal::Help(s)) if s.scroll == 19 && s.query.text() == "query")
            );
        }
    }
    Ok(())
}

#[test]
fn model_and_controls_non_shift_arrows_keep_aliases_and_native_selection() -> Result<()> {
    for (phase, custom) in PHASES {
        for controls in [false, true] {
            for (id, code, initial, wanted) in [
                (NAVIGATION_UP, KeyCode::Up, 2, 1),
                (NAVIGATION_DOWN, KeyCode::Down, 1, 2),
            ] {
                for modifiers in [
                    KeyModifiers::NONE,
                    KeyModifiers::CONTROL,
                    KeyModifiers::ALT,
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                    KeyModifiers::SUPER,
                    KeyModifiers::SUPER | KeyModifiers::CONTROL,
                    KeyModifiers::SUPER | KeyModifiers::ALT,
                    KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::ALT,
                    KeyModifiers::HYPER,
                    KeyModifiers::META | KeyModifiers::SUPER | KeyModifiers::CONTROL,
                ] {
                    let (mut app, _dir) = fixture(id, phase)?;
                    app.models = vec![ModelInfo {
                        id: "fixture-model".into(),
                        name: "Fixture".into(),
                        efforts: vec!["low".into(), "medium".into(), "high".into()],
                    }];
                    let modal = || {
                        if controls {
                            Modal::Workflow(Box::new(crate::workflow::Wizard::Controls {
                                key: pr_key(),
                                head: "head".into(),
                                draft: Some(false),
                                selected: initial,
                                query: "".into(),
                            }))
                        } else {
                            Modal::Models {
                                selected: initial,
                                effort: 0,
                                query: "".into(),
                            }
                        }
                    };
                    app.modal = Some(modal());
                    let old = KeyEvent::new(code, modifiers);
                    let selection = |app: &App| -> Result<usize> {
                        match &app.modal {
                            Some(Modal::Models { selected, .. }) => Ok(*selected),
                            Some(Modal::Workflow(w)) => match w.as_ref() {
                                crate::workflow::Wizard::Controls { selected, .. } => Ok(*selected),
                                _ => anyhow::bail!("controls missing"),
                            },
                            _ => anyhow::bail!("picker missing"),
                        }
                    };
                    for mut release in [old, active(custom, old)] {
                        release.kind = crossterm::event::KeyEventKind::Release;
                        app.key_event(release);
                        assert_eq!(selection(&app)?, initial, "release must be inert");
                    }
                    if custom {
                        app.key_event(old);
                        assert_ne!(
                            selection(&app)?,
                            wanted,
                            "removed {id:?} {modifiers:?} controls={controls}"
                        );
                        app.modal = Some(modal());
                    }
                    app.key_event(active(custom, old));
                    assert_eq!(
                        selection(&app)?,
                        wanted,
                        "{phase} {id:?} {modifiers:?} controls={controls}"
                    );
                }
            }
            let (mut app, _dir) = fixture(NAVIGATION_UP, phase)?;
            let query: crate::editor::Editor = "one\ntwo".into();
            app.modal = Some(if controls {
                Modal::Workflow(Box::new(crate::workflow::Wizard::Controls {
                    key: pr_key(),
                    head: "head".into(),
                    draft: Some(false),
                    selected: 2,
                    query,
                }))
            } else {
                Modal::Models {
                    selected: 2,
                    effort: 0,
                    query,
                }
            });
            app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
            let query = match &app.modal {
                Some(Modal::Models { query, .. }) => query,
                Some(Modal::Workflow(w)) => match w.as_ref() {
                    crate::workflow::Wizard::Controls { query, .. } => query,
                    _ => anyhow::bail!("controls missing"),
                },
                _ => anyhow::bail!("picker missing"),
            };
            assert!(
                query.selected_text().is_some(),
                "Shift+Up retains editor selection"
            );
            assert_eq!(query.text(), "one\ntwo");
        }
    }
    Ok(())
}

#[test]
fn repository_detail_pin_hint_and_mouse_follow_saved_binding_in_both_states() -> Result<()> {
    for (phase, custom) in PHASES {
        for pinned in [false, true] {
            let (mut app, _dir) = fixture(REVIEW_PIN_REPOSITORY, phase)?;
            app.inbox_tab = InboxTab::Repositories;
            app.repository_options = vec!["example/repo".into()];
            app.repo_selected = Some("example/repo".into());
            if pinned {
                app.config.pinned_repositories.insert("example/repo".into());
            }
            app.storage
                .update_config(|saved| *saved = app.config.clone())?;
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30))?;
            terminal.draw(|frame| crate::ui::draw(frame, &mut app))?;
            let screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            let verb = if pinned { "Unpin" } else { "Pin" };
            let hint = if custom { "F12" } else { "Shift+8" };
            assert!(
                screen.contains(&format!("{hint} · {verb} repository")),
                "{phase} pinned={pinned}: effective detail hint missing"
            );
            if custom {
                assert!(!screen.contains(&format!("* · {verb} repository")));
            }
            let target = app
                .hits
                .iter()
                .find_map(|(rect, action)| {
                    matches!(action, Action::PinRepository(name) if name == "example/repo")
                        .then_some(*rect)
                })
                .ok_or_else(|| anyhow::anyhow!("rendered pin target"))?;
            app.mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: target.x,
                row: target.y,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(
                app.storage
                    .load_config()?
                    .pinned_repositories
                    .contains("example/repo"),
                !pinned
            );
        }
    }
    Ok(())
}
