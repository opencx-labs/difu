use super::*;
use crate::{
    settings::BindingKey,
    shortcuts::{ActionId, ids, key as chord},
};
use anyhow::{Context, Result};

pub(super) fn fixture(action: Option<ActionId>) -> Result<(Ui, tempfile::TempDir, Config)> {
    let dir = tempfile::tempdir()?;
    let mut config = Config::default();
    if let Some(action) = action {
        config
            .preferences
            .keyboard
            .bindings
            .insert(action.0.into(), chord(BindingKey::Function(12), 0));
    }
    let mut ui = Ui::new(
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        },
        &config,
    );
    let mut session = Session::new(
        "one".into(),
        Job::Coding(Launch {
            repository: dir.path().into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "task".into(),
            model: None,
            effort: None,
        }),
    );
    session.status = Status::Idle;
    ui.summaries.push(session.summary());
    ui.sessions.insert("one".into(), session);
    ui.selected = Some("one".into());
    ui.drilled = true;
    ui.focus = Focus::Composer;
    ui.positions.entry("one".into()).or_default().draft = Editor::from("unsent draft");
    Ok((ui, dir, config))
}
pub(super) const BINDING_PHASES: [(&str, bool); 4] = [
    ("default", false),
    ("custom", true),
    ("reset", false),
    ("discard", true),
];
/// Each panel assertion exercises behavior after a real Settings reset/save or
/// discard as well as fresh default/custom configuration.
pub(super) fn binding_fixture(
    action: ActionId,
    phase: &str,
) -> Result<(Ui, tempfile::TempDir, Config)> {
    let (mut ui, dir, mut config) = fixture((phase != "default").then_some(action))?;
    if phase == "reset" || phase == "discard" {
        ui.storage.update_config(|saved| *saved = config.clone())?;
        let mut draft = ui.storage.settings_draft()?;
        draft.reset(&crate::settings::SettingId::Shortcut(action.0.into()))?;
        assert!(
            ui.shortcut_config
                .preferences
                .keyboard
                .bindings
                .contains_key(action.0),
            "reset remains draft-only"
        );
        if phase == "reset" {
            config = ui.storage.save_settings_draft(&mut draft)?;
            assert!(!config.preferences.keyboard.bindings.contains_key(action.0));
        } else {
            draft.discard(ui.storage.load_config()?);
            config = draft.config().clone();
            assert!(config.preferences.keyboard.bindings.contains_key(action.0));
        }
        ui.apply_settings(&config);
    }
    Ok((ui, dir, config))
}

/// Observe the existing IPC boundary without a daemon or provider process.
pub(super) fn request_at_boundary(ui: &mut Ui, key: KeyEvent) -> Result<Request> {
    request_at_boundary_with(ui, |ui| ui.key(key))
}
pub(super) fn request_at_boundary_with(
    ui: &mut Ui,
    dispatch: impl FnOnce(&mut Ui),
) -> Result<Request> {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };
    let socket = super::super::server::socket(&ui.storage)?;
    let listener = UnixListener::bind(socket)?;
    listener.set_nonblocking(true)?;
    let reader = thread::spawn(move || -> Result<Request> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                    let mut line = String::new();
                    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                    let request = serde_json::from_str(&line)?;
                    stream.write_all(b"\"Ok\"\n")?;
                    return Ok(request);
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) => return Err(error.into()),
            }
        }
    });
    dispatch(ui);
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("IPC fixture reader"))?
}

fn press(ui: &mut Ui, code: KeyCode, modifiers: KeyModifiers) {
    ui.key(KeyEvent::new(code, modifiers));
}
fn text(ui: &Ui) -> Result<String> {
    Ok(ui.positions.get("one").context("position")?.draft.text())
}

#[test]
fn question_open_rebind_replaces_default_and_preserves_draft_selection() -> Result<()> {
    for custom in [false, true] {
        let (mut ui, _dir, _) = fixture(custom.then_some(ids::QUESTION_OPEN))?;
        press(&mut ui, KeyCode::Left, KeyModifiers::SHIFT);
        if custom {
            press(&mut ui, KeyCode::Up, KeyModifiers::ALT);
            assert!(ui.modal.is_none(), "removed question key");
            ui.positions.get_mut("one").context("draft")?.draft = Editor::from("unsent draft");
            press(&mut ui, KeyCode::Left, KeyModifiers::SHIFT);
        }
        press(
            &mut ui,
            if custom { KeyCode::F(12) } else { KeyCode::Up },
            if custom {
                KeyModifiers::NONE
            } else {
                KeyModifiers::ALT
            },
        );
        assert!(
            matches!(ui.modal, Some(Modal::Pending { .. })),
            "question composer opened"
        );
        press(&mut ui, KeyCode::Esc, KeyModifiers::NONE);
        assert!(ui.modal.is_none());
        assert_eq!(text(&ui)?, "unsent draft");
        assert_eq!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .selected_text()
                .as_deref(),
            Some("t")
        );
    }
    Ok(())
}

#[test]
fn question_shift_left_requires_consent_at_actual_composer_dispatch() -> Result<()> {
    for consent in [false, true] {
        let (mut ui, _dir, mut config) = fixture(None)?;
        config
            .preferences
            .keyboard
            .bindings
            .insert(ids::QUESTION_OPEN.0.into(), chord(BindingKey::Left, 4));
        config.preferences.keyboard.question_overrides_selection = consent;
        ui.apply_settings(&config);
        press(&mut ui, KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(ui.modal.is_some(), consent);
        let draft = &ui.positions.get("one").context("draft")?.draft;
        assert_eq!(draft.text(), "unsent draft");
        assert_eq!(draft.selected_text().is_some(), !consent);
    }
    Ok(())
}

#[test]
fn send_presets_and_remaps_dispatch_primary_queue_and_newline() -> Result<()> {
    use crate::settings::{RunningInput, SendKey};
    for preset in [SendKey::Enter, SendKey::CtrlEnter] {
        for running in [false, true] {
            for policy in [RunningInput::Steer, RunningInput::Queue] {
                for action in [
                    ids::COMPOSER_SEND,
                    ids::COMPOSER_QUEUE,
                    ids::COMPOSER_NEWLINE,
                ] {
                    for custom in [false, true] {
                        let (mut ui, _dir, mut config) = fixture(custom.then_some(action))?;
                        config.preferences.input.send_key = preset;
                        config.preferences.input.running = policy;
                        ui.apply_settings(&config);
                        if running {
                            ui.sessions.get_mut("one").context("session")?.turn_id =
                                Some("turn".into());
                        }
                        // An in-flight delivery keeps this test at the real queue boundary,
                        // without starting a worker or contacting an agent service.
                        ui.send_queues
                            .insert("one".into(), VecDeque::from([(99, Control::Interrupt)]));
                        let modifiers = match (preset, action) {
                            (SendKey::Enter, ids::COMPOSER_SEND)
                            | (SendKey::CtrlEnter, ids::COMPOSER_NEWLINE) => KeyModifiers::NONE,
                            (SendKey::Enter, ids::COMPOSER_NEWLINE) => KeyModifiers::SHIFT,
                            (SendKey::CtrlEnter, ids::COMPOSER_QUEUE) => KeyModifiers::ALT,
                            _ => KeyModifiers::CONTROL,
                        };
                        if custom {
                            press(&mut ui, KeyCode::Enter, modifiers);
                            assert_eq!(
                                ui.send_queues.get("one").context("queue")?.len(),
                                1,
                                "removed send chord"
                            );
                            // Removed Enter can still perform ordinary Editor newline input.
                            ui.positions.get_mut("one").context("draft")?.draft =
                                Editor::from("unsent draft");
                        }
                        press(
                            &mut ui,
                            if custom {
                                KeyCode::F(12)
                            } else {
                                KeyCode::Enter
                            },
                            if custom {
                                KeyModifiers::NONE
                            } else {
                                modifiers
                            },
                        );
                        if action == ids::COMPOSER_NEWLINE {
                            assert_eq!(text(&ui)?, "unsent draft\n");
                            assert_eq!(ui.send_queues.get("one").context("queue")?.len(), 1);
                        } else {
                            assert_eq!(text(&ui)?, "");
                            let control = &ui
                                .send_queues
                                .get("one")
                                .context("queue")?
                                .back()
                                .context("sent")?
                                .1;
                            let Control::Message { text, queue, .. } = control else {
                                anyhow::bail!("message not enqueued")
                            };
                            assert_eq!(text, "unsent draft");
                            assert_eq!(
                                *queue,
                                action == ids::COMPOSER_QUEUE
                                    || (running && policy == RunningInput::Queue)
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn main_view_actions_dispatch_defaults_and_replacements() -> Result<()> {
    for (action, code, modifiers) in [
        (ids::AGENTS_HELP, KeyCode::Char('?'), KeyModifiers::NONE),
        (ids::AGENTS_COMMANDS, KeyCode::Char('/'), KeyModifiers::NONE),
        (ids::AGENTS_SIDEBAR, KeyCode::Char('['), KeyModifiers::ALT),
        (ids::AGENTS_CHANGES, KeyCode::Char('d'), KeyModifiers::ALT),
        (ids::SESSION_REFRESH, KeyCode::Char('r'), KeyModifiers::NONE),
        (
            ids::CONVERSATION_COMPOSE,
            KeyCode::Char('i'),
            KeyModifiers::NONE,
        ),
        (ids::AGENTS_BACK, KeyCode::Esc, KeyModifiers::NONE),
        (
            ids::CONVERSATION_INTERRUPT,
            KeyCode::Esc,
            KeyModifiers::NONE,
        ),
        (ids::NAVIGATION_ACTIVATE, KeyCode::Enter, KeyModifiers::NONE),
        (
            ids::NAVIGATION_NEXT_FOCUS_COMPOSER,
            KeyCode::Tab,
            KeyModifiers::NONE,
        ),
        (ids::NAVIGATION_NEXT_FOCUS, KeyCode::Tab, KeyModifiers::NONE),
        (
            ids::NAVIGATION_PREVIOUS_FOCUS,
            KeyCode::BackTab,
            KeyModifiers::NONE,
        ),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            ui.focus = match action {
                ids::CONVERSATION_COMPOSE => Focus::Changes,
                ids::AGENTS_BACK => Focus::ChangeTree,
                ids::CONVERSATION_INTERRUPT | ids::NAVIGATION_NEXT_FOCUS_COMPOSER => {
                    Focus::Composer
                }
                _ => Focus::List,
            };
            ui.refreshed = Some(Instant::now());
            ui.changes_at = Some(Instant::now());
            if custom {
                let focus = ui.focus;
                press(&mut ui, code, modifiers);
                assert!(ui.modal.is_none(), "removed {action:?} opened modal");
                assert_eq!(ui.focus, focus, "removed {action:?} changed focus");
                assert!(ui.list_visible && !ui.changes_visible && ui.drilled);
                assert!(ui.refreshed.is_some());
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    modifiers
                },
            );
            match action {
                ids::AGENTS_HELP => assert!(matches!(ui.modal, Some(Modal::Help(_)))),
                ids::AGENTS_COMMANDS => assert!(matches!(ui.modal, Some(Modal::Menu { .. }))),
                ids::AGENTS_SIDEBAR => assert!(!ui.list_visible),
                ids::AGENTS_CHANGES => assert!(ui.changes_visible),
                ids::SESSION_REFRESH => assert!(ui.refreshed.is_none() && ui.changes_at.is_none()),
                ids::CONVERSATION_COMPOSE
                | ids::NAVIGATION_ACTIVATE
                | ids::NAVIGATION_NEXT_FOCUS
                | ids::NAVIGATION_PREVIOUS_FOCUS => assert_eq!(ui.focus, Focus::Composer),
                ids::AGENTS_BACK | ids::CONVERSATION_INTERRUPT => {
                    assert!(!ui.drilled && ui.focus == Focus::List)
                }
                ids::NAVIGATION_NEXT_FOCUS_COMPOSER => assert_eq!(ui.focus, Focus::List),
                _ => anyhow::bail!("unhandled test action"),
            }
        }
    }
    Ok(())
}

#[test]
fn change_navigation_dispatches_semantic_motion_and_removes_old_keys() -> Result<()> {
    for (action, code, modifiers, changes, horizontal) in [
        (
            ids::CHANGES_SELECT_UP,
            KeyCode::Up,
            KeyModifiers::SHIFT,
            19,
            20,
        ),
        (
            ids::CHANGES_SELECT_DOWN,
            KeyCode::Down,
            KeyModifiers::SHIFT,
            21,
            20,
        ),
        (ids::NAVIGATION_UP, KeyCode::Up, KeyModifiers::NONE, 19, 20),
        (
            ids::NAVIGATION_DOWN,
            KeyCode::Down,
            KeyModifiers::NONE,
            21,
            20,
        ),
        (
            ids::NAVIGATION_FAST_UP,
            KeyCode::Up,
            KeyModifiers::SUPER,
            10,
            20,
        ),
        (
            ids::NAVIGATION_FAST_DOWN,
            KeyCode::Down,
            KeyModifiers::SUPER,
            30,
            20,
        ),
        (
            ids::NAVIGATION_PAGE_UP,
            KeyCode::PageUp,
            KeyModifiers::NONE,
            13,
            20,
        ),
        (
            ids::NAVIGATION_PAGE_DOWN,
            KeyCode::PageDown,
            KeyModifiers::NONE,
            27,
            20,
        ),
        (
            ids::NAVIGATION_FIRST,
            KeyCode::Home,
            KeyModifiers::NONE,
            0,
            20,
        ),
        (
            ids::NAVIGATION_LAST,
            KeyCode::End,
            KeyModifiers::NONE,
            99,
            20,
        ),
        (
            ids::NAVIGATION_LEFT,
            KeyCode::Left,
            KeyModifiers::NONE,
            20,
            16,
        ),
        (
            ids::NAVIGATION_RIGHT,
            KeyCode::Right,
            KeyModifiers::NONE,
            20,
            24,
        ),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            ui.focus = Focus::Changes;
            ui.changes_visible = true;
            ui.viewport = 7;
            ui.change_lines = 100;
            let p = ui.positions.get_mut("one").context("position")?;
            p.changes = 20;
            p.horizontal = 20;
            if custom {
                press(&mut ui, code, modifiers);
                let p = ui.positions.get("one").context("position")?;
                assert_eq!((p.changes, p.horizontal), (20, 20), "removed {action:?}");
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    modifiers
                },
            );
            let p = ui.positions.get("one").context("position")?;
            assert_eq!(
                (p.changes, p.horizontal),
                (changes, horizontal),
                "{action:?}"
            );
            assert_eq!(
                p.selection,
                matches!(action, ids::CHANGES_SELECT_UP | ids::CHANGES_SELECT_DOWN).then_some(20)
            );
        }
    }
    Ok(())
}

#[test]
fn inline_question_controls_dispatch_default_and_remapped_actions() -> Result<()> {
    for (action, code, modifiers) in [
        (ids::QUESTION_PREVIOUS, KeyCode::Up, KeyModifiers::ALT),
        (ids::QUESTION_NEXT, KeyCode::Down, KeyModifiers::ALT),
        (ids::QUESTION_NEXT_FIELD, KeyCode::Tab, KeyModifiers::NONE),
        (
            ids::QUESTION_PREVIOUS_FIELD,
            KeyCode::BackTab,
            KeyModifiers::NONE,
        ),
        (ids::QUESTION_NOTE, KeyCode::Char('n'), KeyModifiers::NONE),
        (ids::QUESTION_TOGGLE, KeyCode::Char(' '), KeyModifiers::NONE),
        (ids::QUESTION_SUBMIT, KeyCode::Enter, KeyModifiers::NONE),
        (
            ids::QUESTION_SKIP,
            KeyCode::Char(']'),
            KeyModifiers::CONTROL,
        ),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            ui.sessions.get_mut("one").context("session")?.pending.push(Pending {
                id: serde_json::json!("request"), method: "item/tool/requestUserInput".into(), responded: false,
                params: serde_json::json!({"questions":[{"id":"a","question":"First?","multiSelect":true,"options":[{"label":"Small"},{"label":"Large"}]},{"id":"b","question":"Second?","options":[{"label":"Next"}]}]}),
            });
            ui.open_question(0);
            if custom {
                press(&mut ui, code, modifiers);
                assert!(
                    matches!(ui.modal, Some(Modal::Approval { field: 0, .. })),
                    "removed {action:?}"
                );
                assert!(
                    !ui.question_note_focused && ui.question_send.is_none(),
                    "removed {action:?}"
                );
                // Former printable action keys become ordinary answer editing.
                ui.open_question(0);
                if let Some(Modal::Approval { selected, .. }) = &mut ui.modal {
                    *selected = 0;
                }
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    modifiers
                },
            );
            match action {
                ids::QUESTION_PREVIOUS
                | ids::QUESTION_NEXT_FIELD
                | ids::QUESTION_PREVIOUS_FIELD => {
                    assert!(matches!(ui.modal, Some(Modal::Approval { field: 1, .. })))
                }
                ids::QUESTION_NEXT => assert!(ui.modal.is_none() && ui.focus == Focus::Composer),
                ids::QUESTION_NOTE => assert!(ui.question_note_focused),
                ids::QUESTION_TOGGLE => assert!(
                    ui.positions
                        .get("one")
                        .context("position")?
                        .question_checks
                        .values()
                        .any(|checks| !checks.is_empty())
                ),
                ids::QUESTION_SUBMIT | ids::QUESTION_SKIP => assert!(
                    ui.question_send
                        .as_ref()
                        .is_some_and(|(_, _, q, _)| q == "a")
                ),
                _ => anyhow::bail!("test action"),
            }
            assert_eq!(text(&ui)?, "unsent draft");
        }
    }
    Ok(())
}

#[test]
fn copy_binding_replaces_super_alias_while_ctrl_c_keeps_selection_precedence() -> Result<()> {
    let (mut ui, _dir, _) = fixture(Some(ids::SELECTION_COPY))?;
    press(&mut ui, KeyCode::Left, KeyModifiers::SHIFT);
    press(&mut ui, KeyCode::Char('c'), KeyModifiers::SUPER);
    assert!(ui.clipboard.is_none(), "old copy alias removed");
    press(&mut ui, KeyCode::F(12), KeyModifiers::NONE);
    assert_eq!(ui.clipboard.as_deref(), Some("t"));
    ui.clipboard = None;
    press(&mut ui, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(ui.clipboard.as_deref(), Some("t"));
    assert_eq!(text(&ui)?, "unsent draft");
    Ok(())
}

#[test]
fn agent_help_and_voice_controls_render_effective_keys() -> Result<()> {
    for (action, query) in [
        (ids::QUESTION_OPEN, "question"),
        (ids::COMPOSER_SEND, "Send message"),
        (ids::COMPOSER_QUEUE, "Queue message"),
        (ids::COMPOSER_NEWLINE, "newline"),
        (ids::VOICE_HOLD, "Talk when"),
        (ids::AGENTS_CHANGES, "Open or close Changes"),
    ] {
        let (mut ui, _dir, _) = fixture(Some(action))?;
        let state = crate::help::State {
            query: Editor::from(query),
            ..Default::default()
        };
        ui.modal = Some(Modal::Help(state));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("F12"), "{action:?}: {text}");
    }
    Ok(())
}

#[test]
fn creation_permission_and_interrupt_keys_reach_the_real_service_boundary() -> Result<()> {
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
    // A separate process is required: ensure_running rejects a same-process PID.
    const SERVER: &str = r#"
import json,socket,sys
server=socket.socket(socket.AF_UNIX)
server.bind(sys.argv[1])
server.listen()
server.settimeout(3)
print('ready',flush=True)
while True:
    client,_=server.accept()
    with client:
        client.settimeout(3)
        line=client.makefile('rb').readline()
        if not line: continue
        request=json.loads(line)
        if request=='ServiceVersion':
            reply={'ServiceVersion':{'version':sys.argv[2]}}
        else:
            reply='Ok'
        client.sendall((json.dumps(reply)+'\n').encode())
        if request!='ServiceVersion':
            print(json.dumps(request),flush=True)
            break
"#;
    for (action, code, modifiers) in [
        (ids::SESSION_NEW, KeyCode::Char('n'), KeyModifiers::NONE),
        (
            ids::COMPOSER_PERMISSION_MODE,
            KeyCode::BackTab,
            KeyModifiers::NONE,
        ),
        (
            ids::CONVERSATION_INTERRUPT,
            KeyCode::Esc,
            KeyModifiers::NONE,
        ),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            if action == ids::SESSION_NEW {
                ui.focus = Focus::List;
            }
            if action == ids::CONVERSATION_INTERRUPT {
                ui.sessions.get_mut("one").context("session")?.status = Status::Running;
            }
            let socket = super::super::server::socket(&ui.storage)?;
            let child = Command::new("python3")
                .args(["-u", "-c", SERVER])
                .arg(socket)
                .arg(env!("CARGO_PKG_VERSION"))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let mut service = Service(child);
            let stdout = service.0.stdout.take().context("fixture stdout")?;
            let (tx, rx) = mpsc::channel();
            let reader = thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            assert_eq!(rx.recv_timeout(Duration::from_secs(3))??, "ready");
            if custom {
                press(&mut ui, code, modifiers);
                assert!(
                    rx.recv_timeout(Duration::from_millis(30)).is_err(),
                    "old {action:?} contacted service"
                );
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    modifiers
                },
            );
            let line = rx
                .recv_timeout(Duration::from_secs(3))
                .with_context(|| format!("{action:?} custom={custom}"))??;
            let request: Request = serde_json::from_str(&line)?;
            reader
                .join()
                .map_err(|_| anyhow::anyhow!("fixture reader"))?;
            match action {
                ids::SESSION_NEW => assert!(matches!(request, Request::NewAgent { .. })),
                ids::COMPOSER_PERMISSION_MODE => assert!(
                    matches!(request, Request::Permissions { id, action: crate::agents::permissions::Action::CycleMode } if id == "one")
                ),
                ids::CONVERSATION_INTERRUPT => assert!(
                    matches!(request, Request::Control { id, control:Control::Interrupt } if id == "one")
                ),
                _ => anyhow::bail!("action"),
            }
            assert_eq!(text(&ui)?, "unsent draft");
        }
    }
    Ok(())
}

#[test]
fn composer_completions_history_suggestion_and_paste_expansion_follow_bindings() -> Result<()> {
    for (action, code, modifiers) in [
        (ids::COMPOSER_PATHS, KeyCode::Char('@'), KeyModifiers::NONE),
        (ids::COMPOSER_SKILLS, KeyCode::Char('$'), KeyModifiers::NONE),
        (ids::COMPOSER_SUGGESTION, KeyCode::Right, KeyModifiers::NONE),
        (
            ids::COMPOSER_HISTORY_PREVIOUS,
            KeyCode::Up,
            KeyModifiers::NONE,
        ),
        (
            ids::COMPOSER_HISTORY_NEXT,
            KeyCode::Down,
            KeyModifiers::NONE,
        ),
        (
            ids::COMPOSER_ATTACH,
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
        ),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            ui.positions.get_mut("one").context("position")?.draft = Editor::default();
            let session = ui.sessions.get_mut("one").context("session")?;
            session.note("userMessage", "previous prompt");
            session.completed_turn = Some("finished".into());
            session.suggestion = Some(crate::agents::suggestions::Suggestion {
                turn: "finished".into(),
                prompt: session.entries.last().context("entry")?.id.clone(),
                text: "Suggested followup".into(),
            });
            if action == ids::COMPOSER_HISTORY_NEXT {
                press(&mut ui, KeyCode::Up, KeyModifiers::NONE);
            }
            if action == ids::COMPOSER_ATTACH {
                ui.positions
                    .get_mut("one")
                    .context("position")?
                    .draft
                    .paste_with_threshold("pasted content", Some(1));
            }
            if custom {
                let before = text(&ui)?;
                press(&mut ui, code, modifiers);
                assert!(ui.modal.is_none(), "old completion key");
                if !matches!(action, ids::COMPOSER_PATHS | ids::COMPOSER_SKILLS) {
                    assert_eq!(text(&ui)?, before, "old {action:?}");
                }
                if matches!(action, ids::COMPOSER_PATHS | ids::COMPOSER_SKILLS) {
                    ui.positions.get_mut("one").context("position")?.draft = Editor::default();
                }
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    modifiers
                },
            );
            match action {
                ids::COMPOSER_PATHS => assert!(matches!(
                    ui.modal,
                    Some(Modal::Commands {
                        files_only: true,
                        ..
                    })
                )),
                ids::COMPOSER_SKILLS => assert!(matches!(
                    ui.modal,
                    Some(Modal::Commands {
                        skills_only: true,
                        ..
                    })
                )),
                ids::COMPOSER_SUGGESTION => assert_eq!(text(&ui)?, "Suggested followup"),
                ids::COMPOSER_HISTORY_PREVIOUS => assert_eq!(text(&ui)?, "previous prompt"),
                ids::COMPOSER_HISTORY_NEXT => assert_eq!(text(&ui)?, ""),
                ids::COMPOSER_ATTACH => assert_eq!(
                    ui.positions
                        .get("one")
                        .context("position")?
                        .draft
                        .chars
                        .iter()
                        .collect::<String>(),
                    "pasted content"
                ),
                _ => anyhow::bail!("action"),
            }
        }
    }
    Ok(())
}

#[test]
fn composer_and_menu_show_the_active_main_bindings() -> Result<()> {
    for action in [
        ids::COMPOSER_PERMISSION_MODE,
        ids::QUESTION_OPEN,
        ids::AGENTS_MODELS,
        ids::AGENTS_SIDEBAR,
        ids::AGENTS_CHANGES,
    ] {
        let (mut ui, _dir, _) = fixture(Some(action))?;
        if action != ids::COMPOSER_PERMISSION_MODE {
            ui.modal = Some(Modal::Menu {
                query: Editor::default(),
                selected: 0,
            });
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 45))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("F12"), "{action:?}: {text}");
    }
    Ok(())
}

#[test]
fn session_reordering_and_transcript_navigation_replace_their_default_chords() -> Result<()> {
    for (action, code, modifiers) in [
        (ids::SESSION_REORDER_UP, KeyCode::Up, KeyModifiers::CONTROL),
        (
            ids::SESSION_REORDER_DOWN,
            KeyCode::Down,
            KeyModifiers::CONTROL,
        ),
        (ids::CONVERSATION_PREVIOUS, KeyCode::Up, KeyModifiers::SUPER),
        (ids::CONVERSATION_NEXT, KeyCode::Down, KeyModifiers::SUPER),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            let reorder = matches!(action, ids::SESSION_REORDER_UP | ids::SESSION_REORDER_DOWN);
            let mut second = ui.summaries.first().context("summary")?.clone();
            second.id = "two".into();
            ui.summaries.push(second);
            ui.session_order = vec!["one".into(), "two".into()];
            ui.focus = if reorder {
                Focus::List
            } else {
                Focus::Conversation
            };
            if action == ids::SESSION_REORDER_UP {
                ui.selected = Some("two".into());
            }
            ui.conversation_sections = vec![
                transcript::Section {
                    id: "first".into(),
                    row: 0,
                    tool: false,
                },
                transcript::Section {
                    id: "second".into(),
                    row: 10,
                    tool: false,
                },
            ];
            ui.positions
                .get_mut("one")
                .context("position")?
                .focused_entry = Some(
                if action == ids::CONVERSATION_PREVIOUS {
                    "second"
                } else {
                    "first"
                }
                .into(),
            );
            let before = ui
                .positions
                .get("one")
                .context("position")?
                .focused_entry
                .clone();
            if custom {
                press(&mut ui, code, modifiers);
                assert_eq!(ui.session_order, vec!["one", "two"]);
                assert_eq!(
                    ui.positions.get("one").context("position")?.focused_entry,
                    before
                );
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    modifiers
                },
            );
            if reorder {
                assert_eq!(ui.session_order, vec!["two", "one"]);
                assert_eq!(ui.storage.load_config()?.session_order, vec!["two", "one"]);
            } else {
                assert_eq!(
                    ui.positions
                        .get("one")
                        .context("position")?
                        .focused_entry
                        .as_deref(),
                    Some(if action == ids::CONVERSATION_PREVIOUS {
                        "first"
                    } else {
                        "second"
                    })
                );
            }
        }
    }
    Ok(())
}

#[test]
fn model_picker_toggle_uses_effective_main_binding_without_provider_discovery() -> Result<()> {
    for custom in [false, true] {
        let (mut ui, _dir, _) = fixture(custom.then_some(ids::AGENTS_MODELS))?;
        ui.model_completion.options = vec![crate::model::ModelInfo {
            id: "sample-model".into(),
            name: "Sample model".into(),
            efforts: vec!["medium".into()],
        }];
        if custom {
            press(&mut ui, KeyCode::Char('m'), KeyModifiers::ALT);
            assert!(ui.modal.is_none());
        }
        let event = if custom {
            KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE)
        } else {
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)
        };
        ui.key(event);
        assert!(matches!(ui.modal, Some(Modal::ModelPicker(_))));
        ui.key(event);
        assert!(ui.modal.is_none());
    }
    Ok(())
}

#[test]
fn change_pr_switching_replaces_old_chords_and_displays_effective_hints() -> Result<()> {
    for (action, code, want) in [
        (ids::CHANGES_PREVIOUS_PR, KeyCode::Char('['), 1),
        (ids::CHANGES_NEXT_PR, KeyCode::Char(']'), 3),
    ] {
        for custom in [false, true] {
            let (mut ui, dir, _) = fixture(custom.then_some(action))?;
            std::fs::create_dir_all(&ui.storage.cache)?;
            let links=(1..=3).map(|n|serde_json::json!({"workspace":dir.path(),"pr":{"owner":"sample","repo":"sample","number":n}})).collect::<Vec<_>>();
            std::fs::write(
                ui.storage.cache.join("agent-prs.json"),
                serde_json::to_vec(&serde_json::json!({"one":links}))?,
            )?;
            ui.prs = prs::State::load(&ui.storage);
            ui.focus = Focus::Changes;
            ui.changes_visible = true;
            ui.positions.get_mut("one").context("position")?.change_pr =
                Some(crate::model::PrKey {
                    owner: "sample".into(),
                    repo: "sample".into(),
                    number: 2,
                });
            if custom {
                press(&mut ui, code, KeyModifiers::NONE);
                assert_eq!(
                    ui.positions
                        .get("one")
                        .context("position")?
                        .change_pr
                        .as_ref()
                        .map(|p| p.number),
                    Some(2)
                );
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                KeyModifiers::NONE,
            );
            assert_eq!(
                ui.positions
                    .get("one")
                    .context("position")?
                    .change_pr
                    .as_ref()
                    .map(|p| p.number),
                Some(want)
            );
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 35))?;
            terminal.draw(|frame| ui.draw(frame))?;
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            if custom {
                assert!(text.contains("F12"), "{text}");
            }
        }
    }
    Ok(())
}

#[test]
fn reset_question_binding_takes_effect_only_after_committed_save() -> Result<()> {
    use crate::settings::SettingId;
    let (mut ui, _dir, config) = fixture(Some(ids::QUESTION_OPEN))?;
    ui.storage.try_patch_config(|stored| {
        *stored = config.clone();
        Ok(())
    })?;
    let mut draft = ui.storage.settings_draft()?;
    draft.reset(&SettingId::Shortcut(ids::QUESTION_OPEN.0.into()))?;
    press(&mut ui, KeyCode::F(12), KeyModifiers::NONE);
    assert!(matches!(ui.modal, Some(Modal::Pending { .. })));
    press(&mut ui, KeyCode::Esc, KeyModifiers::NONE);
    let committed = ui.storage.save_settings_draft(&mut draft)?;
    ui.apply_settings(&committed);
    press(&mut ui, KeyCode::F(12), KeyModifiers::NONE);
    assert!(ui.modal.is_none());
    press(&mut ui, KeyCode::Up, KeyModifiers::ALT);
    assert!(matches!(ui.modal, Some(Modal::Pending { .. })));
    press(&mut ui, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(text(&ui)?, "unsent draft");
    Ok(())
}

#[test]
fn inline_question_navigation_uses_custom_keys_and_preserves_answers() -> Result<()> {
    for (action, code, expected_selected, expected_scroll) in [
        (ids::NAVIGATION_UP, KeyCode::Up, 0, 5),
        (ids::NAVIGATION_DOWN, KeyCode::Down, 2, 5),
        (ids::NAVIGATION_PAGE_UP, KeyCode::PageUp, 1, 2),
        (ids::NAVIGATION_PAGE_DOWN, KeyCode::PageDown, 1, 8),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            ui.sessions.get_mut("one").context("session")?.pending.push(Pending {
                id: serde_json::json!("request"), method: "item/tool/requestUserInput".into(), responded: false,
                params: serde_json::json!({"questions":[{"id":"a","question":"Scope?","options":[{"label":"Small"},{"label":"Large"}]}]}),
            });
            ui.open_question(0);
            if let Some(Modal::Approval {
                selected, scroll, ..
            }) = &mut ui.modal
            {
                *selected = 1;
                *scroll = 5;
            }
            if custom {
                press(&mut ui, code, KeyModifiers::NONE);
                assert!(matches!(ui.modal, Some(Modal::Approval { scroll: 5, .. })));
                if let Some(Modal::Approval { selected, .. }) = &mut ui.modal {
                    *selected = 1;
                }
            }
            press(
                &mut ui,
                if custom { KeyCode::F(12) } else { code },
                KeyModifiers::NONE,
            );
            let Some(Modal::Approval {
                selected, scroll, ..
            }) = &ui.modal
            else {
                anyhow::bail!("question missing");
            };
            assert_eq!((*selected, *scroll), (expected_selected, expected_scroll));
            assert_eq!(text(&ui)?, "unsent draft");
        }
    }
    Ok(())
}

#[test]
fn inline_question_preserves_non_shift_enter_aliases_and_rebind_removes_them() -> Result<()> {
    for modifiers in [
        KeyModifiers::NONE,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SUPER,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyModifiers::CONTROL | KeyModifiers::SUPER,
        KeyModifiers::ALT | KeyModifiers::SUPER,
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
    ]
    .into_iter()
    .flat_map(|base| {
        [
            KeyModifiers::NONE,
            KeyModifiers::HYPER,
            KeyModifiers::META,
            KeyModifiers::HYPER | KeyModifiers::META,
        ]
        .map(|extra| base | extra)
    }) {
        for note in [false, true] {
            for custom in [false, true] {
                let (mut ui, _dir, _) = fixture(custom.then_some(ids::QUESTION_SUBMIT))?;
                ui.sessions.get_mut("one").context("session")?.pending.push(Pending {
                    id: serde_json::json!("request"), method: "item/tool/requestUserInput".into(), responded: false,
                    params: serde_json::json!({"questions":[{"id":"a","question":"Scope?","options":[{"label":"Small"},{"label":"Large"}]}]}),
                });
                ui.open_question(0);
                if note {
                    ui.open_question_note();
                }
                ui.key(KeyEvent::new_with_kind(
                    KeyCode::Enter,
                    modifiers,
                    crossterm::event::KeyEventKind::Release,
                ));
                assert!(ui.question_send.is_none(), "Release must never submit");
                press(&mut ui, KeyCode::Enter, modifiers | KeyModifiers::SHIFT);
                assert!(ui.question_send.is_none(), "Shift must keep editing");
                if let Some(Modal::Approval { selected, .. }) = &mut ui.modal {
                    *selected = 0;
                }
                press(&mut ui, KeyCode::Enter, modifiers);
                if custom {
                    assert!(
                        ui.question_send.is_none(),
                        "removed alias {modifiers:?}, note={note}"
                    );
                    if let Some(Modal::Approval { selected, .. }) = &mut ui.modal {
                        *selected = 0;
                    }
                    press(&mut ui, KeyCode::F(12), KeyModifiers::NONE);
                }
                assert!(
                    ui.question_send
                        .as_ref()
                        .is_some_and(|(_, _, id, _)| id == "a"),
                    "submission {modifiers:?}, note={note}, custom={custom}"
                );
                assert_eq!(text(&ui)?, "unsent draft");
            }
        }
    }
    Ok(())
}

#[test]
fn early_main_actions_remain_reachable_from_real_modal_contexts() -> Result<()> {
    for (action, code) in [
        (ids::AGENTS_SIDEBAR, KeyCode::Char('[')),
        (ids::AGENTS_CHANGES, KeyCode::Char('d')),
        (ids::AGENTS_MODELS, KeyCode::Char('m')),
        (ids::QUESTION_OPEN, KeyCode::Up),
    ] {
        for modal in ["rename", "help", "queue", "delete"] {
            for custom in [false, true] {
                let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
                ui.model_completion.options = vec![crate::model::ModelInfo {
                    id: "sample-model".into(),
                    name: "Sample model".into(),
                    efforts: vec!["medium".into()],
                }];
                ui.modal = Some(match modal {
                    "rename" => Modal::Rename(Editor::from("kept name")),
                    "help" => Modal::Help(Default::default()),
                    "queue" => Modal::Queue { selected: 0 },
                    _ => Modal::Delete,
                });
                let context = ui.shortcut_context();
                if custom {
                    press(&mut ui, code, KeyModifiers::ALT);
                    assert_eq!(
                        ui.shortcut_context(),
                        context,
                        "removed {action:?} in {modal}"
                    );
                    assert!(ui.list_visible && !ui.changes_visible);
                }
                press(
                    &mut ui,
                    if custom { KeyCode::F(12) } else { code },
                    if custom {
                        KeyModifiers::NONE
                    } else {
                        KeyModifiers::ALT
                    },
                );
                match action {
                    ids::AGENTS_SIDEBAR => assert!(!ui.list_visible, "sidebar in {modal}"),
                    ids::AGENTS_CHANGES => assert!(ui.changes_visible, "changes in {modal}"),
                    ids::AGENTS_MODELS => assert!(
                        matches!(ui.modal, Some(Modal::ModelPicker(_))),
                        "models in {modal}"
                    ),
                    ids::QUESTION_OPEN => assert!(
                        matches!(ui.modal, Some(Modal::Pending { .. })),
                        "questions in {modal}"
                    ),
                    _ => anyhow::bail!("action"),
                }
                assert_eq!(text(&ui)?, "unsent draft");
            }
        }
    }
    Ok(())
}

#[test]
fn early_modal_actions_cannot_steal_rename_typing_or_question_selection() -> Result<()> {
    for action in [
        ids::AGENTS_SIDEBAR,
        ids::AGENTS_CHANGES,
        ids::AGENTS_MODELS,
        ids::QUESTION_OPEN,
    ] {
        let (mut ui, _dir, mut config) = fixture(None)?;
        config
            .preferences
            .keyboard
            .bindings
            .insert(action.0.into(), chord(BindingKey::Character('z'), 0));
        assert!(crate::shortcuts::Bindings::new(&config).validate().is_err());
        ui.apply_settings(&config);
        ui.modal = Some(Modal::Rename(Editor::from("name")));
        press(&mut ui, KeyCode::Char('z'), KeyModifiers::NONE);
        let Some(Modal::Rename(editor)) = &ui.modal else {
            anyhow::bail!("typing opened action");
        };
        assert_eq!(editor.text(), "namez");
        assert!(ui.list_visible && !ui.changes_visible);
    }
    let (mut ui, _dir, mut config) = fixture(None)?;
    config
        .preferences
        .keyboard
        .bindings
        .insert(ids::QUESTION_OPEN.0.into(), chord(BindingKey::Left, 4));
    config.preferences.keyboard.question_overrides_selection = true;
    crate::shortcuts::Bindings::new(&config).validate()?;
    ui.apply_settings(&config);
    ui.modal = Some(Modal::Rename(Editor::from("name")));
    press(&mut ui, KeyCode::Left, KeyModifiers::SHIFT);
    let Some(Modal::Rename(editor)) = &ui.modal else {
        anyhow::bail!("selection opened question");
    };
    assert_eq!(editor.selected_text().as_deref(), Some("e"));
    Ok(())
}

#[test]
fn plan_and_inline_questions_keep_priority_over_early_main_actions() -> Result<()> {
    for (action, code) in [
        (ids::AGENTS_SIDEBAR, KeyCode::Char('[')),
        (ids::AGENTS_CHANGES, KeyCode::Char('d')),
        (ids::AGENTS_MODELS, KeyCode::Char('m')),
        (ids::QUESTION_OPEN, KeyCode::Up),
    ] {
        for custom in [false, true] {
            let (mut ui, _dir, _) = fixture(custom.then_some(action))?;
            ui.sessions.get_mut("one").context("session")?.plan_prompt =
                Some(crate::agents::plan::Proposal {
                    id: "sample-plan".into(),
                    thread_id: "thread".into(),
                    turn_id: "turn".into(),
                    text: "Sample plan".into(),
                });
            ui.open_plan_prompt();
            assert!(matches!(ui.modal, Some(Modal::Plan(_))));
            let event = KeyEvent::new(
                if custom { KeyCode::F(12) } else { code },
                if custom {
                    KeyModifiers::NONE
                } else {
                    KeyModifiers::ALT
                },
            );
            ui.key(event);
            assert!(matches!(ui.modal, Some(Modal::Plan(_))));
            assert!(ui.list_visible && !ui.changes_visible);
            ui.sessions.get_mut("one").context("session")?.pending.push(Pending {
                id: serde_json::json!("question"), method: "item/tool/requestUserInput".into(), responded: false,
                params: serde_json::json!({"questions":[{"id":"a","question":"Scope?","options":[{"label":"Small"}]}]}),
            });
            ui.open_question(0);
            ui.key(event);
            assert!(ui.inline_question());
            assert!(ui.list_visible && !ui.changes_visible);
        }
    }
    Ok(())
}
