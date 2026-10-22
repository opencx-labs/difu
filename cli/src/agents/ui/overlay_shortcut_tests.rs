use super::shortcut_tests::{BINDING_PHASES, binding_fixture};
use super::*;
use crate::shortcuts::ids::*;
use anyhow::{Context, Result};

fn event(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn shell(ui: &mut Ui) {
    ui.panels.view = Some(panels::View::Shell {
        id: "shell".into(),
        command: "output".into(),
    });
    ui.panels.rect = Rect::new(0, 0, 40, 12);
    ui.panels.focused = true;
    ui.panels.scroll = 20;
}
#[test]
fn helper_controls_use_committed_bindings_and_menu_intent() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for (action, legacy) in [(HELPER_PLACEMENT, 'p'), (HELPER_VISIBILITY, ']')] {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            shell(&mut ui);
            let original_right = ui.panels.right;
            let old = KeyEvent::new(KeyCode::Char(legacy), KeyModifiers::ALT);
            if custom {
                ui.key(old);
                assert_eq!(ui.panels.right, original_right);
                assert!(!ui.panels.hidden);
            }
            ui.key(if custom { event(KeyCode::F(12)) } else { old });
            if action == HELPER_PLACEMENT {
                assert_ne!(ui.panels.right, original_right);
                assert_eq!(ui.storage.load_config()?.agent_panel_right, ui.panels.right);
                let right = ui.panels.right;
                ui.menu_action(16);
                assert_ne!(ui.panels.right, right, "menu is direct intent after remap");
            } else {
                assert!(ui.panels.hidden);
                assert!(!ui.panels.focused);
            }
        }
    }
    Ok(())
}
#[test]
fn remaining_scroll_consumers_replace_literal_navigation_keys() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for surface in ["helper", "status", "transcript", "usage", "help"] {
            for (action, code, want) in [
                (NAVIGATION_UP, KeyCode::Up, 19),
                (NAVIGATION_DOWN, KeyCode::Down, 21),
                (NAVIGATION_FAST_UP, KeyCode::Up, 10),
                (NAVIGATION_FAST_DOWN, KeyCode::Down, 30),
                (NAVIGATION_PAGE_UP, KeyCode::PageUp, 10),
                (NAVIGATION_PAGE_DOWN, KeyCode::PageDown, 30),
                (NAVIGATION_FIRST, KeyCode::Home, 0),
                (NAVIGATION_LAST, KeyCode::End, 100),
            ] {
                // Help has only arrow/fast-arrow scrolling; helper has no fast arrows.
                if surface == "help"
                    && ![
                        NAVIGATION_UP,
                        NAVIGATION_DOWN,
                        NAVIGATION_FAST_UP,
                        NAVIGATION_FAST_DOWN,
                    ]
                    .contains(&action)
                {
                    continue;
                }
                if surface == "helper"
                    && [NAVIGATION_FAST_UP, NAVIGATION_FAST_DOWN].contains(&action)
                {
                    continue;
                }
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                shell(&mut ui);
                if surface != "helper" {
                    ui.modal = Some(match surface {
                        "status" => Modal::Status { scroll: 20 },
                        "transcript" => Modal::Transcript {
                            entry: "item".into(),
                            scroll: 20,
                            rows: vec![Line::from("row"); 110],
                            links: vec![],
                            layout: None,
                            height: 10,
                        },
                        "usage" => Modal::Usage {
                            id: "one".into(),
                            provider: provider::Provider::Codex,
                            data: None,
                            error: None,
                            scroll: 20,
                            maximum: 100,
                        },
                        _ => Modal::Help(crate::help::State {
                            query: Editor::default(),
                            scroll: 20,
                            ..Default::default()
                        }),
                    });
                }
                let scroll = |ui: &Ui| match &ui.modal {
                    Some(Modal::Status { scroll })
                    | Some(Modal::Transcript { scroll, .. })
                    | Some(Modal::Usage { scroll, .. }) => *scroll,
                    Some(Modal::Help(state)) => state.scroll,
                    _ => ui.panels.scroll,
                };
                let old = KeyEvent::new(
                    code,
                    if [NAVIGATION_FAST_UP, NAVIGATION_FAST_DOWN].contains(&action) {
                        KeyModifiers::SUPER
                    } else {
                        KeyModifiers::NONE
                    },
                );
                if custom {
                    ui.key(old);
                    if surface == "help" {
                        if let Some(Modal::Help(state)) = &mut ui.modal {
                            assert_eq!(state.query.text(), "");
                            state.scroll = 20;
                        }
                    } else {
                        assert_eq!(scroll(&ui), 20, "removed {surface} {action:?}");
                    }
                }
                ui.key(if custom { event(KeyCode::F(12)) } else { old });
                let want = if surface == "helper" && action == NAVIGATION_PAGE_UP {
                    8
                } else if surface == "helper" && action == NAVIGATION_PAGE_DOWN {
                    32
                } else if ["helper", "status"].contains(&surface) && action == NAVIGATION_LAST {
                    usize::MAX
                } else {
                    want
                };
                assert_eq!(scroll(&ui), want, "{phase} {surface} {action:?}");
            }
        }
    }
    Ok(())
}

#[test]
fn remaining_form_focus_and_native_editing_use_exact_contexts() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for slack in [false, true] {
            for (action, old, want) in [
                (NAVIGATION_NEXT_FOCUS, KeyCode::Tab, 1),
                (
                    NAVIGATION_PREVIOUS_FOCUS,
                    KeyCode::BackTab,
                    if slack { 5 } else { 4 },
                ),
            ] {
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                if slack {
                    ui.open_slack();
                } else {
                    ui.open_tweaks();
                }
                let field = |ui: &Ui| match &ui.modal {
                    Some(Modal::Slack(form)) => form.field,
                    Some(Modal::Tweaks { field, .. }) => *field,
                    _ => usize::MAX,
                };
                if custom {
                    ui.key(event(old));
                    assert_eq!(field(&ui), 0);
                }
                ui.key(if custom {
                    event(KeyCode::F(12))
                } else {
                    event(old)
                });
                assert_eq!(field(&ui), want, "{phase} slack={slack} {action:?}");
                if slack {
                    if let Some(Modal::Slack(form)) = &mut ui.modal {
                        form.field = 4;
                    }
                } else if let Some(Modal::Tweaks { field, .. }) = &mut ui.modal {
                    *field = 4;
                }
                assert_eq!(
                    ui.shortcut_context().0,
                    if slack {
                        "AgentSlackButton"
                    } else {
                        "AgentTweaksButton"
                    }
                );
                ui.key(event(KeyCode::Esc));
                assert!(ui.modal.is_none());
            }
        }
    }
    Ok(())
}
#[test]
fn remaining_list_navigation_and_activation_replace_the_old_aliases() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for surface in [
            "menu",
            "commands",
            "picker",
            "resources",
            "pending",
            "queue",
            "theme",
        ] {
            for (action, old, initial, want) in [
                (NAVIGATION_UP, KeyCode::Up, 1, 0),
                (NAVIGATION_DOWN, KeyCode::Down, 0, 1),
            ] {
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                ui.sessions.get_mut("one").context("session")?.queue =
                    vec![Prompt::Text("a".into()), Prompt::Text("b".into())];
                ui.sessions.get_mut("one").context("session")?.pending =
                    vec![pending(false), pending(false)];
                ui.panels.shells.insert(
                    "one".into(),
                    vec![
                        serde_json::json!({"id":"a","command":"one"}),
                        serde_json::json!({"id":"b","command":"two"}),
                    ],
                );
                ui.modal = Some(match surface {
                    "menu" => Modal::Menu {
                        query: Editor::default(),
                        selected: initial,
                    },
                    "commands" => Modal::Commands {
                        query: Editor::default(),
                        selected: initial,
                        skills_only: false,
                        files_only: false,
                    },
                    "picker" => Modal::InstallBrowser {
                        path: "/tmp/unopened".into(),
                        title: "preview".into(),
                        selected: initial,
                    },
                    "resources" => Modal::Resources {
                        artifacts: false,
                        selected: initial,
                    },
                    "pending" => Modal::Pending { selected: initial },
                    "queue" => Modal::Queue { selected: initial },
                    _ => Modal::Theme { selected: initial },
                });
                let selected = |ui: &Ui| match &ui.modal {
                    Some(
                        Modal::Menu { selected, .. }
                        | Modal::Commands { selected, .. }
                        | Modal::InstallBrowser { selected, .. }
                        | Modal::Resources { selected, .. }
                        | Modal::Pending { selected }
                        | Modal::Queue { selected }
                        | Modal::Theme { selected },
                    ) => *selected,
                    _ => usize::MAX,
                };
                if custom {
                    ui.key(event(old));
                    if let Some(Modal::Menu { selected, .. } | Modal::Commands { selected, .. }) =
                        &mut ui.modal
                    {
                        assert_eq!(*selected, 0);
                        *selected = initial;
                    } else {
                        assert_eq!(selected(&ui), initial, "removed {surface}");
                    }
                }
                ui.key(if custom {
                    event(KeyCode::F(12))
                } else {
                    event(old)
                });
                assert_eq!(selected(&ui), want, "{phase} {surface} {action:?}");
                ui.key(event(KeyCode::Esc));
                assert!(ui.modal.is_none());
            }
        }
    }
    Ok(())
}
fn pending(text: bool) -> Pending {
    Pending {
        responded: false,
        id: serde_json::json!("request-one"),
        method: if text {
            "item/tool/requestUserInput"
        } else {
            "item/commandExecution/requestApproval"
        }
        .into(),
        params: if text {
            serde_json::json!({"questions":[{"id":"q1","question":"Choose","options":[{"label":"first"},{"label":"second"}]},{"id":"q2","question":"Other"}]})
        } else {
            serde_json::json!({"command":"never run"})
        },
    }
}
#[test]
fn queue_actions_replace_literal_enter_and_control_enter_at_real_boundary() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for (action, legacy, focus, delete) in [
            (QUEUE_SAVE, KeyModifiers::CONTROL, 0, false),
            (QUEUE_ACTIVATE, KeyModifiers::NONE, 1, false),
            (QUEUE_ACTIVATE, KeyModifiers::NONE, 2, true),
        ] {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            ui.modal = Some(Modal::QueuedEdit {
                index: 2,
                expected: Prompt::Text("before".into()),
                editor: Editor::from("after"),
                focus,
            });
            let old = KeyEvent::new(KeyCode::Enter, legacy);
            let request = super::shortcut_tests::request_at_connected_boundary_with(
                &mut ui,
                |ui, _requests| {
                    if custom {
                        ui.key(old);
                        assert!(!ui.busy, "removed {action:?}");
                        if let Some(Modal::QueuedEdit { editor, .. }) = &mut ui.modal {
                            *editor = Editor::from("after");
                        }
                    }
                    ui.key(if custom { event(KeyCode::F(12)) } else { old });
                },
            )?;
            match request {
                Request::Control {
                    id,
                    control:
                        Control::ReplaceQueued {
                            index,
                            expected,
                            replacement,
                        },
                } => {
                    assert_eq!(id, "one");
                    assert_eq!(index, 2);
                    assert_eq!(expected.text(), "before");
                    assert_eq!(
                        replacement.as_ref().map(|p| p.text()),
                        if delete { None } else { Some("after") }
                    );
                }
                _ => anyhow::bail!("wrong queue request"),
            }
        }
    }
    Ok(())
}
#[test]
fn approval_navigation_fields_and_submit_follow_committed_bindings() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for (action, code, text) in [
            (NAVIGATION_DOWN, KeyCode::Down, false),
            (NAVIGATION_UP, KeyCode::Up, false),
            (NAVIGATION_PAGE_DOWN, KeyCode::PageDown, false),
            (NAVIGATION_PAGE_UP, KeyCode::PageUp, false),
        ] {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            ui.modal = Some(Modal::Approval {
                pending: pending(text),
                selected: if action == NAVIGATION_UP { 1 } else { 0 },
                answers: vec![Editor::default(), Editor::default()],
                field: 0,
                scroll: 20,
            });
            if custom {
                ui.key(event(code));
                if let Some(Modal::Approval {
                    selected,
                    field,
                    scroll,
                    ..
                }) = &ui.modal
                {
                    assert_eq!(*field, 0);
                    if action == NAVIGATION_UP {
                        assert_eq!(*selected, 1);
                    }
                    if action == NAVIGATION_PAGE_UP || action == NAVIGATION_PAGE_DOWN {
                        assert_eq!(*scroll, 20);
                    }
                }
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(code)
            });
            let Some(Modal::Approval {
                selected,
                field,
                answers,
                scroll,
                ..
            }) = &ui.modal
            else {
                anyhow::bail!("approval closed")
            };
            match action {
                NAVIGATION_UP => assert_eq!(*selected, 0),
                NAVIGATION_DOWN => assert_eq!(*selected, 1),
                NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS => assert_eq!(*field, 1),
                NAVIGATION_ACTIVATE => {
                    assert_eq!(answers.first().map(Editor::text).as_deref(), Some("first"));
                    assert_eq!(*field, 1);
                }
                NAVIGATION_PAGE_UP => assert_eq!(*scroll, 10),
                NAVIGATION_PAGE_DOWN => assert_eq!(*scroll, 30),
                _ => {}
            }
        }
    }
    Ok(())
}

#[test]
fn refresh_commands_and_usage_send_exact_requests_with_reset_and_discard() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for files in [false, true] {
            let (mut ui, _dir, _) = binding_fixture(COMMANDS_REFRESH, phase)?;
            ui.modal = Some(Modal::Commands {
                query: Editor::from("keep"),
                selected: 0,
                skills_only: !files,
                files_only: files,
            });
            let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
                    assert!(ui.skills_loading.is_none());
                    assert!(ui.paths_loading.is_empty());
                }
                ui.key(if custom {
                    event(KeyCode::F(12))
                } else {
                    KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
                });
            })?;
            match request {
                Request::Skills { id, force } if !files => {
                    assert_eq!(id, "one");
                    assert!(force);
                }
                Request::WorkspacePaths { id } if files => assert_eq!(id, "one"),
                _ => anyhow::bail!("wrong refresh request"),
            }
            assert!(matches!(&ui.modal,Some(Modal::Commands{query,..}) if query.text()=="keep"));
        }
        let (mut ui, _dir, _) = binding_fixture(USAGE_REFRESH, phase)?;
        ui.modal = Some(Modal::Usage {
            id: "one".into(),
            provider: provider::Provider::Codex,
            data: Some(serde_json::json!({"old":true})),
            error: None,
            scroll: 30,
            maximum: 100,
        });
        let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
            if custom {
                ui.key(event(KeyCode::Char('r')));
                assert!(matches!(
                    &ui.modal,
                    Some(Modal::Usage {
                        data: Some(_),
                        scroll: 30,
                        ..
                    })
                ));
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(KeyCode::Char('r'))
            });
        })?;
        assert!(matches!(request,Request::Usage{id} if id=="one"));
        assert!(matches!(
            ui.modal,
            Some(Modal::Usage {
                data: None,
                scroll: 0,
                ..
            })
        ));
    }
    Ok(())
}
#[test]
fn approval_submit_preserves_explicit_control_aliases_and_override_removes_them() -> Result<()> {
    for modifiers in [
        KeyModifiers::NONE,
        KeyModifiers::CONTROL,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::CONTROL | KeyModifiers::SUPER,
        KeyModifiers::CONTROL | KeyModifiers::HYPER,
        KeyModifiers::CONTROL | KeyModifiers::META,
    ] {
        for (phase, custom) in BINDING_PHASES {
            let (mut ui, _dir, _) = binding_fixture(APPROVAL_SUBMIT, phase)?;
            ui.modal = Some(Modal::Approval {
                pending: pending(false),
                selected: 1,
                answers: vec![],
                field: 0,
                scroll: 0,
            });
            let request = super::shortcut_tests::request_at_connected_boundary_with(
                &mut ui,
                |ui, _requests| {
                    if custom {
                        ui.key(KeyEvent::new(KeyCode::Enter, modifiers));
                        assert!(!ui.busy, "removed {modifiers:?}");
                    }
                    ui.key(if custom {
                        event(KeyCode::F(12))
                    } else {
                        KeyEvent::new(KeyCode::Enter, modifiers)
                    });
                },
            )?;
            assert!(
                matches!(request,Request::Control{id,control:Control::Respond{request,response}} if id=="one" && request==serde_json::json!("request-one") && response==serde_json::json!({"decision":"acceptForSession"}))
            );
        }
    }
    Ok(())
}
#[test]
fn plan_feedback_rebinds_submit_and_preserves_native_shift_enter() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        let (mut ui, _dir, _) = binding_fixture(PLAN_FEEDBACK, phase)?;
        seed_claude_plan(&mut ui)?;
        ui.choose_plan(2);
        ui.paste("change the labels");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        let Some(Modal::Plan(prompt)) = &ui.modal else {
            anyhow::bail!("plan")
        };
        assert_eq!(
            prompt.editor().map(Editor::text).as_deref(),
            Some("change the labels\n")
        );
        let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
            if custom {
                ui.key(event(KeyCode::Enter));
                if let Some(Modal::Plan(prompt)) = &mut ui.modal {
                    assert!(prompt.editor().is_some());
                    prompt.paste("more");
                }
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(KeyCode::Enter)
            });
        })?;
        assert!(
            matches!(request,Request::Control{id,control:Control::DecideClaudePlan{request,choice:crate::agents::plan::ClaudeChoice::Feedback{text}}} if id=="one" && request==serde_json::json!("plan-request") && text==if custom{"change the labels\n\nmore"}else{"change the labels\n"})
        );
    }
    Ok(())
}
fn seed_claude_plan(ui: &mut Ui) -> Result<()> {
    let session = ui.sessions.get_mut("one").context("session")?;
    session.provider = provider::Provider::Claude;
    session.pending.push(Pending{responded:false,id:serde_json::json!("plan-request"),method:"item/tool/requestApproval".into(),params:serde_json::json!({"difuClaudePlan":{"text":"Plan the safe change", "auto_available":true}})});
    ui.open_plan_prompt();
    Ok(())
}

fn native_text(ui: &mut Ui, width: u16) -> Result<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 44))?;
    terminal.draw(|frame| ui.draw(frame))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect())
}
#[test]
fn compact_question_and_main_help_show_effective_submit_hint_at_native_widths() -> Result<()> {
    for width in [40, 80, 160] {
        for custom in [false, true] {
            let (mut ui, _dir, _) =
                super::shortcut_tests::fixture(custom.then_some(QUESTION_SUBMIT))?;
            ui.sessions
                .get_mut("one")
                .context("session")?
                .pending
                .push(pending(true));
            ui.open_question(0);
            let screen = native_text(&mut ui, width)?;
            assert!(
                screen.contains(if custom { "F12 submit" } else { "Enter submit" }),
                "{width}: {screen}"
            );
            assert!(!screen.contains("Alt+Enter"), "compact aliases must fit");
            ui.modal = Some(Modal::Help(crate::help::State {
                query: Editor::from("Submit the current question answer"),
                ..Default::default()
            }));
            let screen = native_text(&mut ui, width)?;
            assert!(
                screen.contains(if custom { "F12" } else { "Enter" }),
                "help {width}: {screen}"
            );
            assert!(!screen.contains("Ctrl+Enter answers"));
        }
    }
    Ok(())
}
#[test]
fn remaining_native_action_hints_follow_effective_bindings() -> Result<()> {
    for width in [80, 160] {
        for (action, surface) in [
            (HELPER_PLACEMENT, "helper"),
            (HELPER_PLACEMENT, "menu"),
            (QUEUE_SAVE, "queue"),
            (PLAN_FEEDBACK, "plan"),
            (COMMANDS_REFRESH, "commands"),
            (USAGE_REFRESH, "usage"),
            (NAVIGATION_ACTIVATE, "rename"),
            (NAVIGATION_ACTIVATE, "delete"),
            (NAVIGATION_ACTIVATE, "clear"),
            (NAVIGATION_DOWN, "theme"),
            (NAVIGATION_NEXT_FOCUS, "tweaks"),
            (NAVIGATION_NEXT_FOCUS, "slack"),
        ] {
            let (mut ui, _dir, _) = super::shortcut_tests::fixture(Some(action))?;
            match surface {
                "helper" => shell(&mut ui),
                "menu" => {
                    ui.modal = Some(Modal::Menu {
                        query: Editor::from(if action == HELPER_PLACEMENT {
                            "Move shell"
                        } else {
                            "Toggle agents list"
                        }),
                        selected: 0,
                    })
                }
                "queue" => {
                    ui.modal = Some(Modal::QueuedEdit {
                        index: 0,
                        expected: Prompt::Text("before".into()),
                        editor: Editor::from("after"),
                        focus: 0,
                    })
                }
                "plan" => {
                    seed_claude_plan(&mut ui)?;
                    ui.choose_plan(2);
                }
                "commands" => {
                    ui.modal = Some(Modal::Commands {
                        query: Editor::default(),
                        selected: 0,
                        skills_only: false,
                        files_only: false,
                    })
                }
                "usage" => {
                    ui.modal = Some(Modal::Usage {
                        id: "one".into(),
                        provider: provider::Provider::Codex,
                        data: None,
                        error: None,
                        scroll: 0,
                        maximum: 0,
                    })
                }
                "rename" => ui.modal = Some(Modal::Rename(Editor::from("renamed"))),
                "delete" => ui.modal = Some(Modal::Delete),
                "clear" => ui.modal = Some(Modal::Clear),
                "theme" => ui.open_theme(),
                "slack" => ui.open_slack(),
                _ => ui.open_tweaks(),
            }
            let screen = native_text(&mut ui, width)?;
            assert!(
                screen.contains("F12"),
                "{surface} {width} {action:?}: {screen}"
            );
        }
    }
    Ok(())
}
#[test]
fn request_user_input_has_only_real_inline_question_contexts_and_old_override_is_opaque()
-> Result<()> {
    use crate::shortcuts::{Bindings, contexts::*};
    let (mut ui, _dir, mut config) = super::shortcut_tests::fixture(None)?;
    config.preferences.keyboard.bindings.insert(
        "approval.submit_text".into(),
        crate::shortcuts::key(crate::settings::BindingKey::Function(12), 0),
    );
    config.preferences.keyboard.bindings.insert(
        QUESTION_SUBMIT.0.into(),
        crate::shortcuts::key(crate::settings::BindingKey::Function(11), 0),
    );
    ui.apply_settings(&config);
    ui.sessions
        .get_mut("one")
        .context("session")?
        .pending
        .push(pending(true));
    ui.open_question(0);
    assert_eq!(ui.shortcut_context(), AGENT_QUESTION);
    ui.key(event(KeyCode::F(12)));
    assert!(ui.question_send.is_none());
    assert!(!ui.busy);
    ui.key(event(KeyCode::Up));
    ui.key(event(KeyCode::Char('n')));
    assert_eq!(ui.shortcut_context(), AGENT_QUESTION_NOTE);
    ui.key(event(KeyCode::Char('x')));
    assert_eq!(ui.question_note().map(Editor::text).as_deref(), Some("x"));
    ui.key(event(KeyCode::F(12)));
    assert!(ui.question_send.is_none());
    ui.key(event(KeyCode::Esc));
    assert_eq!(ui.shortcut_context(), AGENT_QUESTION);
    assert!(crate::shortcuts::spec(crate::shortcuts::ActionId("approval.submit_text")).is_none());
    assert!(
        !crate::shortcuts::contexts::ALL
            .iter()
            .any(|c| c.0 == "AgentApprovalText" || c.0 == "AgentTerminal")
    );
    assert_eq!(
        Bindings::new(&config).resolve(AGENT_QUESTION, event(KeyCode::F(11))),
        Some(QUESTION_SUBMIT)
    );
    ui.storage.update_config(|saved| *saved = config.clone())?;
    let mut draft = ui.storage.settings_draft()?;
    draft.set(
        crate::settings::SettingId::VoiceEnabled,
        crate::settings::SettingValue::Bool(true),
    )?;
    let saved = ui.storage.save_settings_draft(&mut draft)?;
    assert!(
        saved
            .preferences
            .keyboard
            .bindings
            .contains_key("approval.submit_text")
    );
    Ok(())
}

#[test]
fn focused_queue_buttons_do_not_copy_the_unfocused_editor_selection() -> Result<()> {
    let (mut ui, _dir, _) = super::shortcut_tests::fixture(Some(SELECTION_COPY))?;
    let mut editor = Editor::from("private draft");
    editor.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
    ui.modal = Some(Modal::QueuedEdit {
        index: 0,
        expected: Prompt::Text("old".into()),
        editor,
        focus: 1,
    });
    ui.key(event(KeyCode::F(12)));
    assert!(
        ui.clipboard.is_none(),
        "unfocused editor is not the selection owner"
    );
    Ok(())
}
#[test]
fn tweaks_selection_copy_uses_actual_editor_and_removes_old_copy_alias() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        let (mut ui, _dir, _) = binding_fixture(SELECTION_COPY, phase)?;
        ui.open_tweaks();
        if let Some(Modal::Tweaks { fields, .. }) = &mut ui.modal
            && let Some(editor) = fields.first_mut()
        {
            *editor = Editor::from("123");
        }
        ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        if custom {
            ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
            assert!(ui.clipboard.is_none());
        }
        ui.key(if custom {
            event(KeyCode::F(12))
        } else {
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER)
        });
        assert_eq!(ui.clipboard.as_deref(), Some("123"));
    }
    Ok(())
}
#[test]
fn remaining_fields_keep_early_main_actions_in_each_exact_context() -> Result<()> {
    for slack in [false, true] {
        for field in [0, 2, 4] {
            for (phase, custom) in BINDING_PHASES {
                for (action, code) in [
                    (AGENTS_SIDEBAR, KeyCode::Char('[')),
                    (AGENTS_CHANGES, KeyCode::Char('d')),
                    (AGENTS_MODELS, KeyCode::Char('m')),
                    (QUESTION_OPEN, KeyCode::Up),
                ] {
                    let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                    ui.model_completion.options = vec![crate::model::ModelInfo {
                        id: "cached".into(),
                        name: "Cached".into(),
                        efforts: vec!["medium".into()],
                    }];
                    if slack {
                        ui.open_slack();
                        if let Some(Modal::Slack(form)) = &mut ui.modal {
                            form.field = field;
                        }
                    } else {
                        ui.open_tweaks();
                        if let Some(Modal::Tweaks { field: active, .. }) = &mut ui.modal {
                            *active = field;
                        }
                    }
                    let old = KeyEvent::new(code, KeyModifiers::ALT);
                    if custom {
                        ui.key(old);
                        assert!(ui.list_visible);
                        assert!(!ui.changes_visible);
                        assert!(if slack {
                            matches!(ui.modal, Some(Modal::Slack(_)))
                        } else {
                            matches!(ui.modal, Some(Modal::Tweaks { .. }))
                        });
                    }
                    ui.key(if custom { event(KeyCode::F(12)) } else { old });
                    match action {
                        AGENTS_SIDEBAR => assert!(!ui.list_visible),
                        AGENTS_CHANGES => assert!(ui.changes_visible),
                        QUESTION_OPEN => assert!(matches!(ui.modal, Some(Modal::Pending { .. }))),
                        AGENTS_MODELS => assert!(matches!(ui.modal, Some(Modal::ModelPicker(_)))),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}
#[test]
fn slack_copy_is_available_only_for_unmasked_active_fields() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        let (mut ui, _dir, _) = binding_fixture(SELECTION_COPY, phase)?;
        ui.open_slack();
        for field in [0, 2, 3, 4] {
            if let Some(Modal::Slack(form)) = &mut ui.modal {
                form.field = field;
            }
            ui.paste("safe-field");
            ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER)
            });
            assert_eq!(
                ui.clipboard.take().as_deref(),
                if field == 2 || field == 3 {
                    Some("safe-field")
                } else {
                    None
                }
            );
        }
    }
    Ok(())
}

#[test]
fn remaining_activation_consumers_apply_real_effects_and_remove_enter() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for surface in [
            "menu",
            "commands",
            "picker",
            "resources",
            "pending",
            "queue",
            "theme",
            "tweaks",
            "slack",
        ] {
            let (mut ui, _dir, _) = binding_fixture(NAVIGATION_ACTIVATE, phase)?;
            prepare_activation(&mut ui, surface)?;
            if custom {
                ui.key(event(KeyCode::Enter));
                assert!(ui.modal.is_some(), "removed {surface}");
                if surface == "menu" {
                    ui.modal = Some(Modal::Menu {
                        query: Editor::from("Open Reviews"),
                        selected: 0,
                    });
                }
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(KeyCode::Enter)
            });
            match surface {
                "commands" => assert!(matches!(ui.modal, Some(Modal::Help(_)))),
                "menu" => assert!(ui.review_requested),
                "picker" | "theme" | "tweaks" | "slack" => assert!(ui.modal.is_none(), "{surface}"),
                "resources" => assert!(
                    matches!(&ui.panels.view,Some(panels::View::Shell{command,..}) if command=="safe output")
                ),
                "pending" => assert!(matches!(ui.modal, Some(Modal::Approval { .. }))),
                "queue" => assert!(matches!(ui.modal, Some(Modal::QueuedEdit { index: 0, .. }))),
                _ => {}
            }
            if surface == "tweaks" {
                assert_eq!(ui.storage.load_config()?.tweaks.agent_reply_cps, 123);
            }
            if surface == "slack" {
                assert!(
                    ui.notice
                        .as_ref()
                        .is_some_and(|(text, error)| text == "Slack disabled" && !*error)
                );
            }
        }
        for surface in ["rename", "delete", "clear"] {
            let (mut ui, _dir, _) = binding_fixture(NAVIGATION_ACTIVATE, phase)?;
            ui.modal = Some(match surface {
                "rename" => Modal::Rename(Editor::from("new title")),
                "delete" => Modal::Delete,
                _ => Modal::Clear,
            });
            let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
                if custom {
                    ui.key(event(KeyCode::Enter));
                    assert!(!ui.busy);
                    assert!(ui.deleting.is_empty());
                }
                ui.key(if custom {
                    event(KeyCode::F(12))
                } else {
                    event(KeyCode::Enter)
                });
            })?;
            match request {
                Request::Rename { id, title } if surface == "rename" => {
                    assert_eq!(id, "one");
                    assert_eq!(title, if custom { "new title\n" } else { "new title" });
                }
                Request::Delete { id } if surface == "delete" => {
                    assert_eq!(id, "one");
                    assert!(ui.deleting.contains("one"));
                }
                Request::Clear { id } if surface == "clear" => assert_eq!(id, "one"),
                _ => anyhow::bail!("wrong activation request"),
            }
        }
    }
    Ok(())
}
fn prepare_activation(ui: &mut Ui, surface: &str) -> Result<()> {
    ui.modal = Some(match surface {
        "menu" => Modal::Menu {
            query: Editor::from("Toggle agents list"),
            selected: 0,
        },
        "commands" => Modal::Commands {
            query: Editor::from("help"),
            selected: 0,
            skills_only: false,
            files_only: false,
        },
        "picker" => Modal::InstallBrowser {
            path: "/temporary/unopened".into(),
            title: "safe preview".into(),
            selected: if cfg!(target_os = "macos") { 2 } else { 1 },
        },
        "resources" => {
            ui.panels.shells.insert(
                "one".into(),
                vec![serde_json::json!({"itemId":"shell-one","command":"safe output"})],
            );
            Modal::Resources {
                artifacts: false,
                selected: 0,
            }
        }
        "pending" => {
            ui.sessions
                .get_mut("one")
                .context("session")?
                .pending
                .push(pending(false));
            Modal::Pending { selected: 0 }
        }
        "queue" => {
            ui.sessions
                .get_mut("one")
                .context("session")?
                .queue
                .push(Prompt::Text("queued text".into()));
            Modal::Queue { selected: 0 }
        }
        "theme" => Modal::Theme { selected: 0 },
        "tweaks" => Modal::Tweaks {
            fields: Box::new([
                Editor::from("123"),
                Editor::from("456"),
                Editor::from("789"),
                Editor::from("100"),
            ]),
            field: 4,
        },
        "slack" => {
            ui.open_slack();
            let Some(Modal::Slack(form)) = &mut ui.modal else {
                anyhow::bail!("slack")
            };
            form.field = 5;
            return Ok(());
        }
        _ => anyhow::bail!("unknown fixture"),
    });
    if surface == "menu" {
        ui.modal = Some(Modal::Menu {
            query: Editor::from("Open Reviews"),
            selected: 0,
        });
    }
    Ok(())
}

#[test]
fn visible_queue_save_and_helper_menu_mouse_targets_survive_remapping() -> Result<()> {
    use super::shortcut_tests::{fixture, native_blank_below, native_target};
    for width in [40, 80, 160] {
        for blank in [true, false] {
            let (mut ui, _dir, _) = fixture(Some(QUEUE_SAVE))?;
            ui.modal = Some(Modal::QueuedEdit {
                index: 0,
                expected: Prompt::Text("before".into()),
                editor: Editor::from("after"),
                focus: 0,
            });
            let target = if blank {
                native_blank_below(&mut ui, width, "Save changes · F12")?
            } else {
                native_target(&mut ui, width, "Save changes · F12")?
            };
            if blank {
                ui.mouse(target);
                assert!(!ui.busy);
            } else {
                let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
                    ui.mouse(target)
                })?;
                assert!(
                    matches!(request,Request::Control{control:Control::ReplaceQueued{replacement:Some(prompt),..},..} if prompt.text()=="after")
                );
            }
            let (mut ui, _dir, _) = fixture(Some(HELPER_PLACEMENT))?;
            shell(&mut ui);
            ui.modal = Some(Modal::Menu {
                query: Editor::from("pane"),
                selected: 0,
            });
            let right = ui.panels.right;
            let label = if width == 40 {
                "Move shell"
            } else {
                "Move shell / artifact pane · F12"
            };
            let target = if blank {
                native_blank_below(&mut ui, width, label)
                    .with_context(|| format!("blank below {label} at {width}"))?
            } else {
                native_target(&mut ui, width, label)?
            };
            ui.mouse(target);
            assert_eq!(ui.panels.right, if blank { right } else { !right });
        }
    }
    Ok(())
}
#[test]
fn visible_safe_overlay_buttons_route_directly_after_navigation_remaps() -> Result<()> {
    use super::shortcut_tests::{fixture, native_blank_below, native_target};
    for width in [40, 80, 160] {
        for (surface, label) in [
            ("commands", "/help"),
            ("picker", "Cancel"),
            ("resources", "safe output"),
            ("tweaks", "Save tweaks"),
            ("slack", "Disable Slack"),
        ] {
            for blank in [true, false] {
                let (mut ui, _dir, _) = fixture(Some(NAVIGATION_ACTIVATE))?;
                prepare_activation(&mut ui, surface)?;
                // Footer below these targets can itself be active; only assert an actual blank cell.
                let target = if blank && surface == "commands" {
                    native_blank_margin(&mut ui, width)?
                } else if blank {
                    native_blank_below(&mut ui, width, label)
                        .with_context(|| format!("blank below {label} at {width}"))?
                } else {
                    native_target(&mut ui, width, label)?
                };
                ui.mouse(target);
                if blank {
                    assert!(ui.modal.is_some(), "blank {surface}");
                    assert!(ui.panels.view.is_none());
                } else {
                    match surface {
                        "commands" => assert!(matches!(ui.modal, Some(Modal::Help(_)))),
                        "resources" => {
                            assert!(matches!(ui.panels.view, Some(panels::View::Shell { .. })))
                        }
                        _ => assert!(ui.modal.is_none(), "{surface}"),
                    }
                }
            }
        }
    }
    Ok(())
}

fn native_blank_margin(ui: &mut Ui, width: u16) -> Result<MouseEvent> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 44))?;
    terminal.draw(|frame| ui.draw(frame))?;
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((0, 0))
            .map(|cell| cell.symbol()),
        Some(" ")
    );
    Ok(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn visible_approval_mouse_choice_is_direct_intent_after_submit_remap() -> Result<()> {
    use super::shortcut_tests::{fixture, native_target};
    for width in [40, 80, 160] {
        for blank in [true, false] {
            let (mut ui, _dir, _) = fixture(Some(APPROVAL_SUBMIT))?;
            ui.modal = Some(Modal::Approval {
                pending: pending(false),
                selected: 0,
                answers: vec![],
                field: 0,
                scroll: 0,
            });
            let target = if blank {
                native_blank_margin(&mut ui, width)?
            } else {
                native_target(&mut ui, width, "Allow once")?
            };
            if blank {
                ui.mouse(target);
                assert!(!ui.busy);
            } else {
                let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
                    ui.mouse(target)
                })?;
                assert!(
                    matches!(request,Request::Control{control:Control::Respond{response,..},..} if response==serde_json::json!({"decision":"accept"}))
                );
            }
        }
    }
    Ok(())
}
#[test]
fn remaining_native_validation_errors_do_not_submit_or_discard_input() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        let (mut ui, _dir, _) = binding_fixture(QUEUE_SAVE, phase)?;
        ui.modal = Some(Modal::QueuedEdit {
            index: 0,
            expected: Prompt::Text("before".into()),
            editor: Editor::from("   "),
            focus: 0,
        });
        ui.key(if custom {
            event(KeyCode::F(12))
        } else {
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
        });
        assert!(!ui.busy);
        assert!(matches!(&ui.modal,Some(Modal::QueuedEdit{editor,..}) if editor.text()=="   "));
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| *error && text.contains("choose Remove"))
        );
        let (mut ui, _dir, _) = binding_fixture(NAVIGATION_ACTIVATE, phase)?;
        ui.modal = Some(Modal::Tweaks {
            fields: Box::new([
                Editor::from("bad"),
                Editor::from("1"),
                Editor::from("1"),
                Editor::from("1"),
            ]),
            field: 4,
        });
        ui.key(if custom {
            event(KeyCode::F(12))
        } else {
            event(KeyCode::Enter)
        });
        assert!(ui.modal.is_some());
        assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
        assert_ne!(ui.storage.load_config()?.tweaks.agent_reply_cps, 123);
        let (mut ui, _dir, _) = binding_fixture(HELPER_PLACEMENT, phase)?;
        shell(&mut ui);
        std::fs::write(&ui.storage.config, b"{malformed")?;
        ui.key(if custom {
            event(KeyCode::F(12))
        } else {
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT)
        });
        assert_eq!(std::fs::read(&ui.storage.config)?, b"{malformed");
        assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
    }
    Ok(())
}

#[test]
fn copy_metadata_and_hidden_draft_guards_match_real_agents_selection_owners() -> Result<()> {
    use crate::shortcuts::{contexts::*, spec};
    let mut actual = spec(SELECTION_COPY)
        .context("copy action")?
        .contexts
        .iter()
        .filter(|c| c.0.starts_with("Agent"))
        .map(|c| c.0)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut want = vec![
        "AgentList",
        "AgentComposer",
        "AgentComposerPanes",
        "AgentConversation",
        "AgentConversationInput",
        "AgentChanges",
        "AgentChangeTree",
        "AgentResources",
        "AgentHelper",
        "AgentHelperMain",
        "AgentQuestion",
        "AgentQuestionNote",
        "AgentQueueEditor",
        "AgentPlanFeedback",
        "AgentCommands",
        "AgentMenu",
        "AgentHelp",
        "AgentModel",
        "AgentDefaults",
        "AgentPermissionSearch",
        "AgentPermissionForm",
        "AgentPermissionMultiline",
        "AgentRules",
        "AgentRulesRepository",
        "AgentSlack",
        "AgentTweaks",
        "AgentTranscript",
        "AgentRepository",
        "AgentChangeRepository",
        "AgentRename",
    ];
    want.sort_unstable();
    assert_eq!(
        actual, want,
        "only actual selected-text owners are advertised"
    );
    for (phase, custom) in BINDING_PHASES {
        for surface in [
            "status",
            "pending",
            "queue",
            "approval",
            "theme",
            "resources",
            "delete",
            "clear",
            "tweaks_button",
            "slack_button",
            "slack_secret",
        ] {
            let (mut ui, _dir, _) = binding_fixture(SELECTION_COPY, phase)?;
            ui.positions
                .get_mut("one")
                .context("position")?
                .draft
                .key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
            match surface {
                "status" => ui.modal = Some(Modal::Status { scroll: 0 }),
                "pending" => ui.modal = Some(Modal::Pending { selected: 0 }),
                "queue" => ui.modal = Some(Modal::Queue { selected: 0 }),
                "approval" => {
                    ui.modal = Some(Modal::Approval {
                        pending: pending(false),
                        selected: 0,
                        answers: vec![],
                        field: 0,
                        scroll: 0,
                    })
                }
                "theme" => ui.open_theme(),
                "resources" => {
                    ui.modal = Some(Modal::Resources {
                        artifacts: false,
                        selected: 0,
                    })
                }
                "delete" => ui.modal = Some(Modal::Delete),
                "clear" => ui.modal = Some(Modal::Clear),
                "tweaks_button" => {
                    ui.open_tweaks();
                    if let Some(Modal::Tweaks { field, .. }) = &mut ui.modal {
                        *field = 4;
                    }
                }
                _ => {
                    ui.open_slack();
                    if let Some(Modal::Slack(form)) = &mut ui.modal {
                        form.field = if surface == "slack_secret" { 0 } else { 4 };
                    }
                }
            }
            if surface == "slack_secret" {
                assert_eq!(ui.shortcut_context().0, "AgentSlackSecret");
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER)
            });
            assert!(ui.clipboard.is_none(), "{phase} {surface}");
            assert!(!ui.copy_chat_selection());
        }
    }
    assert!(ALL.iter().any(|c| c.0 == "AgentSlackSecret"));
    Ok(())
}

pub(crate) fn prepare_slack_copy(ui: &mut Ui, field: usize) {
    ui.open_slack();
    if let Some(Modal::Slack(form)) = &mut ui.modal {
        form.field = field;
    }
    ui.paste("safe member");
    ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
}

#[test]
fn explicit_remaining_control_and_nonshift_enter_predicates_keep_every_legacy_combination()
-> Result<()> {
    for action in [APPROVAL_SUBMIT, QUEUE_SAVE, COMMANDS_REFRESH, PLAN_FEEDBACK] {
        for mask in 0..32 {
            let flags = if action == PLAN_FEEDBACK {
                [
                    KeyModifiers::CONTROL,
                    KeyModifiers::ALT,
                    KeyModifiers::SUPER,
                    KeyModifiers::HYPER,
                    KeyModifiers::META,
                ]
            } else {
                [
                    KeyModifiers::SHIFT,
                    KeyModifiers::ALT,
                    KeyModifiers::SUPER,
                    KeyModifiers::HYPER,
                    KeyModifiers::META,
                ]
            };
            let modifiers = flags.into_iter().enumerate().fold(
                if action == PLAN_FEEDBACK {
                    KeyModifiers::NONE
                } else {
                    KeyModifiers::CONTROL
                },
                |value, (index, flag)| {
                    if mask & (1 << index) != 0 {
                        value | flag
                    } else {
                        value
                    }
                },
            );
            for custom in [false, true] {
                let (mut ui, _dir, _) = super::shortcut_tests::fixture(custom.then_some(action))?;
                if action == APPROVAL_SUBMIT {
                    ui.modal = Some(Modal::Approval {
                        pending: pending(false),
                        selected: 2,
                        answers: vec![],
                        field: 0,
                        scroll: 0,
                    });
                } else if action == QUEUE_SAVE {
                    ui.modal = Some(Modal::QueuedEdit {
                        index: 0,
                        expected: Prompt::Text("before".into()),
                        editor: Editor::from("after"),
                        focus: 0,
                    });
                } else if action == COMMANDS_REFRESH {
                    ui.modal = Some(Modal::Commands {
                        query: Editor::from("keep"),
                        selected: 0,
                        skills_only: true,
                        files_only: false,
                    });
                } else {
                    seed_claude_plan(&mut ui)?;
                    ui.choose_plan(2);
                    ui.paste("feedback");
                }
                let old = KeyEvent::new(
                    if action == COMMANDS_REFRESH {
                        KeyCode::Char('r')
                    } else {
                        KeyCode::Enter
                    },
                    modifiers,
                );
                let request = super::shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
                    if custom {
                        ui.key(old);
                        assert!(!ui.busy);
                        assert!(ui.skills_loading.is_none());
                        if let Some(Modal::QueuedEdit { editor, .. }) = &mut ui.modal {
                            assert_eq!(editor.text(), "after\n");
                            *editor = Editor::from("after");
                        }
                    }
                    ui.key(if custom { event(KeyCode::F(12)) } else { old });
                })?;
                match request {
                    Request::Control {
                        control: Control::Respond { response, .. },
                        ..
                    } if action == APPROVAL_SUBMIT => {
                        assert_eq!(response, serde_json::json!({"decision":"decline"}))
                    }
                    Request::Control {
                        control:
                            Control::ReplaceQueued {
                                replacement: Some(prompt),
                                ..
                            },
                        ..
                    } if action == QUEUE_SAVE => assert_eq!(prompt.text(), "after"),
                    Request::Skills { force: true, .. } if action == COMMANDS_REFRESH => {}
                    Request::Control {
                        control:
                            Control::DecideClaudePlan {
                                choice: crate::agents::plan::ClaudeChoice::Feedback { text },
                                ..
                            },
                        ..
                    } if action == PLAN_FEEDBACK => {
                        assert_eq!(text, if custom { "feedback\n" } else { "feedback" })
                    }
                    _ => anyhow::bail!("wrong modified action {action:?}"),
                }
            }
        }
    }
    Ok(())
}
#[test]
fn resource_and_helper_focus_aliases_replace_keys_without_losing_the_draft() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for (action, old) in [
            (NAVIGATION_LEFT, KeyCode::Left),
            (NAVIGATION_RIGHT, KeyCode::Right),
            (NAVIGATION_UP, KeyCode::Up),
            (NAVIGATION_NEXT_FOCUS, KeyCode::Tab),
            (NAVIGATION_PREVIOUS_FOCUS, KeyCode::BackTab),
        ] {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            ui.panels
                .shells
                .insert("one".into(), vec![serde_json::json!({"command":"shell"})]);
            ui.sessions
                .get_mut("one")
                .context("session")?
                .artifacts
                .push(crate::agents::artifacts::Artifact {
                    path: "unused".into(),
                    title: "Preview".into(),
                    workspace: None,
                });
            ui.focus = Focus::Resources(false);
            if custom {
                ui.key(event(old));
                assert_eq!(ui.focus, Focus::Resources(false));
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(old)
            });
            if [NAVIGATION_LEFT, NAVIGATION_RIGHT].contains(&action) {
                assert_eq!(ui.focus, Focus::Resources(true));
            } else {
                assert_ne!(ui.focus, Focus::Resources(false));
            }
            assert_eq!(
                ui.positions.get("one").context("draft")?.draft.text(),
                "unsent draft"
            );
        }
        for (action, old) in [
            (NAVIGATION_NEXT_FOCUS, KeyCode::Tab),
            (NAVIGATION_PREVIOUS_FOCUS, KeyCode::BackTab),
            (AGENTS_BACK, KeyCode::Esc),
        ] {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            shell(&mut ui);
            ui.panels.right = true;
            if custom {
                ui.key(event(old));
                assert!(ui.panels.focused);
                assert!(ui.panels.view.is_some());
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(old)
            });
            assert!(!ui.panels.focused);
            if action == AGENTS_BACK {
                assert!(ui.panels.view.is_none());
            }
        }
    }
    Ok(())
}
#[test]
fn pending_advertises_only_reachable_navigation_without_inventing_scroll() -> Result<()> {
    use crate::shortcuts::{contexts::AGENT_PENDING, spec};
    for (action, old) in [
        (NAVIGATION_PAGE_UP, KeyCode::PageUp),
        (NAVIGATION_PAGE_DOWN, KeyCode::PageDown),
        (NAVIGATION_FIRST, KeyCode::Home),
        (NAVIGATION_LAST, KeyCode::End),
    ] {
        assert!(
            !spec(action)
                .context("navigation")?
                .contexts
                .contains(&AGENT_PENDING)
        );
        let (mut ui, _dir, _) = super::shortcut_tests::fixture(Some(action))?;
        ui.modal = Some(Modal::Pending { selected: 1 });
        ui.positions
            .get_mut("one")
            .context("position")?
            .conversation = 20;
        for code in [old, KeyCode::F(12)] {
            ui.key(event(code));
            assert!(matches!(ui.modal, Some(Modal::Pending { selected: 1 })));
            assert_eq!(
                ui.positions.get("one").context("position")?.conversation,
                20
            );
        }
    }
    Ok(())
}
#[test]
fn theme_edges_and_resource_picker_tab_aliases_replace_the_old_keys() -> Result<()> {
    for (phase, custom) in BINDING_PHASES {
        for (action, old, initial, want) in [
            (NAVIGATION_FIRST, KeyCode::Home, 1, 0),
            (
                NAVIGATION_LAST,
                KeyCode::End,
                0,
                crate::theme::themes().len() - 1,
            ),
            (NAVIGATION_UP, KeyCode::BackTab, 1, 0),
            (NAVIGATION_DOWN, KeyCode::Tab, 0, 1),
        ] {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            ui.modal = Some(Modal::Theme { selected: initial });
            if custom {
                ui.key(event(old));
                assert!(matches!(ui.modal,Some(Modal::Theme{selected}) if selected==initial));
            }
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                event(old)
            });
            assert!(matches!(ui.modal,Some(Modal::Theme{selected}) if selected==want));
            ui.key(event(KeyCode::Esc));
        }
        let (mut ui, _dir, _) = binding_fixture(NAVIGATION_DOWN, phase)?;
        ui.modal = Some(Modal::InstallBrowser {
            path: "/unused".into(),
            title: "preview".into(),
            selected: 0,
        });
        if custom {
            ui.key(event(KeyCode::Tab));
            assert!(matches!(
                ui.modal,
                Some(Modal::InstallBrowser { selected: 0, .. })
            ));
        }
        ui.key(if custom {
            event(KeyCode::F(12))
        } else {
            event(KeyCode::Tab)
        });
        assert!(matches!(
            ui.modal,
            Some(Modal::InstallBrowser { selected: 1, .. })
        ));
    }
    Ok(())
}
#[test]
fn focused_queue_button_and_approval_submit_hints_show_custom_keys() -> Result<()> {
    for width in [40, 80, 160] {
        for focus in [1, 2] {
            let (mut ui, _dir, _) = super::shortcut_tests::fixture(Some(QUEUE_ACTIVATE))?;
            ui.modal = Some(Modal::QueuedEdit {
                index: 0,
                expected: Prompt::Text("before".into()),
                editor: Editor::from("after"),
                focus,
            });
            assert!(native_text(&mut ui, width)?.contains("F12"));
        }
        let (mut ui, _dir, _) = super::shortcut_tests::fixture(Some(APPROVAL_SUBMIT))?;
        ui.modal = Some(Modal::Approval {
            pending: pending(false),
            selected: 0,
            answers: vec![],
            field: 0,
            scroll: 0,
        });
        assert!(native_text(&mut ui, width)?.contains("F12"));
    }
    Ok(())
}

#[test]
fn focused_helper_does_not_invent_models_or_question_shortcuts() -> Result<()> {
    use crate::shortcuts::{contexts::AGENT_HELPER, spec};
    for (action, old) in [
        (AGENTS_MODELS, KeyCode::Char('m')),
        (QUESTION_OPEN, KeyCode::Up),
    ] {
        assert!(
            !spec(action)
                .context("main action")?
                .contexts
                .contains(&AGENT_HELPER)
        );
        for (phase, custom) in BINDING_PHASES {
            let (mut ui, _dir, _) = binding_fixture(action, phase)?;
            shell(&mut ui);
            ui.model_completion.options = vec![crate::model::ModelInfo {
                id: "cached".into(),
                name: "Cached".into(),
                efforts: vec![],
            }];
            let position = ui.positions.get_mut("one").context("position")?;
            position.draft.anchor = Some(0);
            ui.key(if custom {
                event(KeyCode::F(12))
            } else {
                KeyEvent::new(old, KeyModifiers::ALT)
            });
            assert!(ui.modal.is_none());
            assert!(ui.panels.focused);
            assert!(matches!(ui.panels.view, Some(panels::View::Shell { .. })));
            let position = ui.positions.get("one").context("position")?;
            assert_eq!(position.draft.text(), "unsent draft");
            assert_eq!(
                position.draft.selected_text().as_deref(),
                Some("unsent draft")
            );
        }
    }
    Ok(())
}

#[test]
fn helper_focus_routing_preserves_right_bottom_and_main_placements() -> Result<()> {
    use crate::settings::HelperPlacement;
    for placement in [
        HelperPlacement::Right,
        HelperPlacement::Bottom,
        HelperPlacement::Main,
    ] {
        for (action, code, modifiers) in [
            (NAVIGATION_NEXT_FOCUS, KeyCode::Tab, KeyModifiers::NONE),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
            ),
            (NAVIGATION_PREVIOUS_FOCUS, KeyCode::Tab, KeyModifiers::SHIFT),
        ] {
            for (phase, custom) in BINDING_PHASES {
                let (mut ui, _dir, mut config) = binding_fixture(action, phase)?;
                config.preferences.layout.helper_placement = Some(placement);
                ui.apply_settings(&config);
                shell(&mut ui);
                assert_eq!(
                    ui.shortcut_context().0,
                    if placement == HelperPlacement::Main {
                        "AgentHelperMain"
                    } else {
                        "AgentHelper"
                    }
                );
                let old = KeyEvent::new(code, modifiers);
                if custom {
                    ui.key(old);
                    assert!(ui.panels.focused);
                }
                ui.key(if custom { event(KeyCode::F(12)) } else { old });
                assert_eq!(
                    ui.panels.focused,
                    placement == HelperPlacement::Main && action == NAVIGATION_PREVIOUS_FOCUS
                );
                assert_eq!(
                    ui.positions.get("one").context("draft")?.draft.text(),
                    "unsent draft"
                );
            }
        }
    }
    Ok(())
}
