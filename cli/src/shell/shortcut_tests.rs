use super::*;
use crate::{
    settings::BindingKey,
    shortcuts::{ids, key},
};
use anyhow::Result;

#[test]
fn remapped_shell_globals_dispatch_and_replace_the_old_action_keys() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    for (action, code, modifiers) in [
        (
            ids::SETTINGS_OPEN,
            KeyCode::Char(','),
            KeyModifiers::CONTROL,
        ),
        (ids::PALETTE_OPEN, KeyCode::Char('k'), KeyModifiers::SUPER),
        (ids::SHELL_AGENTS, KeyCode::Char('1'), KeyModifiers::ALT),
        (ids::SHELL_REVIEWS, KeyCode::Char('2'), KeyModifiers::ALT),
        (ids::APP_QUIT, KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        for custom in [false, true] {
            let mut config = Config::default();
            if custom {
                config
                    .preferences
                    .keyboard
                    .bindings
                    .insert(action.0.into(), key(BindingKey::Function(11), 0));
            }
            let mut shell = Shell::new(storage.clone(), config, None);
            shell.reviews_started = true;
            if action == ids::SHELL_AGENTS || action == ids::APP_QUIT {
                shell.agents_active = false;
            }
            if custom {
                let before = shell.agents_active;
                shell.key(KeyEvent::new(code, modifiers));
                assert_eq!(shell.agents_active, before, "removed tab chord");
                assert!(
                    shell.settings.is_none() && shell.palette.is_none() && !shell.reviews.quit,
                    "removed global action chord"
                );
            }
            shell.key(if custom {
                KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE)
            } else {
                KeyEvent::new(code, modifiers)
            });
            match action {
                ids::SETTINGS_OPEN => assert!(shell.settings.is_some()),
                ids::PALETTE_OPEN => assert!(shell.palette.is_some()),
                ids::SHELL_AGENTS => assert!(shell.agents_active),
                ids::SHELL_REVIEWS => assert!(!shell.agents_active),
                ids::APP_QUIT => assert!(shell.reviews.quit),
                _ => anyhow::bail!("unexpected action"),
            }
        }
    }
    Ok(())
}

#[test]
fn capture_and_draft_tester_never_dispatch_session_or_global_actions() -> Result<()> {
    use crate::settings::{SettingId, SettingValue};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut shell = Shell::new(storage.clone(), Config::default(), None);
    shell.open_settings(None);
    shell.settings_action(SettingId::Shortcut("question.open".into()));
    shell.key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
    let panel = shell
        .settings
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("settings missing"))?;
    assert_eq!(
        panel
            .draft
            .value(&SettingId::Shortcut("question.open".into())),
        SettingValue::Shortcut(Some(key(BindingKey::Function(7), 0)))
    );
    assert!(shell.agents.modal.is_none());
    assert!(!storage.config.exists(), "capture must stay draft-only");
    shell.settings_action(SettingId::ShortcutTest);
    shell.key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
    assert!(
        shell
            .settings
            .as_ref()
            .and_then(|p| p.notice.as_ref())
            .is_some_and(|s| s.contains("Open question composer"))
    );
    shell.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!shell.reviews.quit);
    assert!(shell.settings.is_some() && shell.agents.modal.is_none());
    shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        shell.settings.is_some(),
        "Escape leaves tester before panel"
    );
    Ok(())
}

#[test]
fn capture_explains_conflicts_and_selection_override_then_can_be_cancelled() -> Result<()> {
    use crate::settings::{SettingId, SettingValue};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut shell = Shell::new(storage, Config::default(), None);
    shell.open_settings(None);
    shell.settings_action(SettingId::Shortcut("question.open".into()));
    shell.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert!(
        shell
            .settings
            .as_ref()
            .and_then(|p| p.error.as_ref())
            .is_some_and(|s| s.contains("text-selection override"))
    );
    assert!(
        shell
            .settings
            .as_ref()
            .is_some_and(|p| p.draft.edited().is_empty())
    );
    shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let panel = shell
        .settings
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("settings missing"))?;
    panel.draft.set(
        SettingId::QuestionOverridesSelection,
        SettingValue::Bool(true),
    )?;
    shell.settings_action(SettingId::Shortcut("question.open".into()));
    shell.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(
        shell
            .settings
            .as_ref()
            .map(|p| p.draft.value(&SettingId::Shortcut("question.open".into()))),
        Some(SettingValue::Shortcut(Some(key(BindingKey::Left, 4))))
    );
    shell.settings_action(SettingId::Shortcut("question.open".into()));
    shell.key(KeyEvent::new(KeyCode::Char(','), KeyModifiers::CONTROL));
    assert!(
        shell
            .settings
            .as_ref()
            .and_then(|p| p.error.as_ref())
            .is_some_and(|s| s.contains("conflicts"))
    );
    shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        shell
            .settings
            .as_ref()
            .map(|p| p.draft.value(&SettingId::Shortcut("question.open".into()))),
        Some(SettingValue::Shortcut(Some(key(BindingKey::Left, 4))))
    );
    Ok(())
}

#[test]
fn quit_action_keeps_selection_copy_precedence_and_does_not_submit_the_draft() -> Result<()> {
    for custom in [false, true] {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut config = Config::default();
        if custom {
            config
                .preferences
                .keyboard
                .bindings
                .insert(ids::APP_QUIT.0.into(), key(BindingKey::Function(11), 0));
        }
        let mut shell = Shell::new(storage, config, None);
        shell.agents.selected = Some("copy-only".into());
        shell.agents.drilled = true;
        shell.agents.focus = crate::agents::ui::Focus::Composer;
        let position = shell
            .agents
            .positions
            .entry("copy-only".into())
            .or_default();
        position.draft = crate::editor::Editor::from("keep this draft");
        position.draft.anchor = Some(0);
        let event = if custom {
            KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE)
        } else {
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        };
        shell.key(event);
        assert!(!shell.reviews.quit);
        assert_eq!(shell.agents.clipboard.as_deref(), Some("keep this draft"));
        if let Some(position) = shell.agents.positions.get_mut("copy-only") {
            assert_eq!(position.draft.text(), "keep this draft");
            position.draft.anchor = None;
        }
        shell.key(event);
        assert!(shell.reviews.quit);
    }
    Ok(())
}

#[test]
fn editing_reservations_preserve_actual_composer_input_from_invalid_stored_globals() -> Result<()> {
    use crate::{editor::Editor, shortcuts::Bindings};
    for (code, modifiers) in [
        (KeyCode::Char('z'), KeyModifiers::NONE),
        (KeyCode::Char('x'), KeyModifiers::NONE),
        (KeyCode::Right, KeyModifiers::SHIFT),
        (KeyCode::Left, KeyModifiers::SHIFT),
        (KeyCode::Char('a'), KeyModifiers::CONTROL),
        (KeyCode::Char('e'), KeyModifiers::CONTROL),
        (KeyCode::Char('u'), KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::ALT),
        (KeyCode::Char('f'), KeyModifiers::ALT),
        (KeyCode::Char('a'), KeyModifiers::SUPER),
        (KeyCode::Char('z'), KeyModifiers::SUPER),
        (KeyCode::Home, KeyModifiers::NONE),
        (KeyCode::End, KeyModifiers::SUPER | KeyModifiers::SHIFT),
        (KeyCode::Backspace, KeyModifiers::ALT),
        (KeyCode::Delete, KeyModifiers::NONE),
        (KeyCode::Enter, KeyModifiers::SHIFT),
    ] {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let event = KeyEvent::new(code, modifiers);
        let chord = crate::shortcuts::capture(event).ok_or_else(|| anyhow::anyhow!("capture"))?;
        let mut config = Config::default();
        config
            .preferences
            .keyboard
            .bindings
            .insert(ids::SETTINGS_OPEN.0.into(), chord);
        let mut shell = Shell::new(storage, config.clone(), None);
        shell.agents.selected = Some("editing-only".into());
        shell.agents.drilled = true;
        shell.agents.focus = crate::agents::ui::Focus::Composer;
        let mut expected = Editor::from("alpha beta");
        expected.insert("!");
        expected.cursor = 3;
        shell
            .agents
            .positions
            .entry("editing-only".into())
            .or_default()
            .draft = expected.clone();
        expected.key(event);
        shell.key(event);
        assert!(shell.settings.is_none(), "global stole {event:?}");
        let actual = &shell
            .agents
            .positions
            .get("editing-only")
            .ok_or_else(|| anyhow::anyhow!("position"))?
            .draft;
        assert_eq!(actual.text(), expected.text(), "text for {event:?}");
        assert_eq!(actual.cursor, expected.cursor, "cursor for {event:?}");
        assert_eq!(
            actual.selected_text(),
            expected.selected_text(),
            "selection for {event:?}"
        );
        assert!(
            Bindings::new(&config).validate().is_err(),
            "accepted {event:?}"
        );
    }
    Ok(())
}

#[test]
fn agent_globals_use_actual_list_and_modal_editor_contexts() -> Result<()> {
    use crate::agents::ui::{Focus, Modal};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    config.preferences.keyboard.bindings.insert(
        ids::SETTINGS_OPEN.0.into(),
        key(BindingKey::Character('z'), 0),
    );
    let mut shell = Shell::new(storage, config, None);
    shell.agents_active = true;
    shell.agents.focus = Focus::List;
    shell.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(
        shell.settings.is_some(),
        "list may use a printable custom global"
    );
    shell.settings = None;
    shell.agents.modal = Some(Modal::Rename(crate::editor::Editor::from("name")));
    shell.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(
        shell.settings.is_none(),
        "invalid persisted key must not steal rename input"
    );
    assert!(matches!(&shell.agents.modal, Some(Modal::Rename(e)) if e.text() == "namez"));
    Ok(())
}

#[test]
fn agent_globals_distinguish_queue_editor_from_its_buttons() -> Result<()> {
    use crate::agents::{Prompt, ui::Modal};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    config.preferences.keyboard.bindings.insert(
        ids::SETTINGS_OPEN.0.into(),
        key(BindingKey::Character('z'), 0),
    );
    let mut shell = Shell::new(storage, config, None);
    shell.agents_active = true;
    for focus in [0, 1, 2] {
        shell.settings = None;
        shell.agents.modal = Some(Modal::QueuedEdit {
            index: 0,
            expected: Prompt::Text("queued".into()),
            editor: crate::editor::Editor::from("queued"),
            focus,
        });
        shell.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(
            shell.settings.is_some(),
            focus != 0,
            "actual queue field {focus}"
        );
        if focus == 0 {
            assert!(
                matches!(&shell.agents.modal, Some(Modal::QueuedEdit{editor,..}) if editor.text() == "queuedz")
            );
        }
    }
    Ok(())
}

#[test]
fn settings_entry_and_cross_client_disable_drop_active_voice_without_losing_draft() -> Result<()> {
    use crate::agents::ui::Focus;
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let config = Config {
        voice_enabled: true,
        ..Default::default()
    };
    storage.update_config(|saved| *saved = config.clone())?;
    let mut shell = Shell::new(storage.clone(), config.clone(), None);
    shell.agents.selected = Some("one".into());
    shell.agents.drilled = true;
    shell.agents.focus = Focus::Composer;
    shell
        .agents
        .positions
        .entry("one".into())
        .or_default()
        .draft = crate::editor::Editor::from("keep draft");
    let tx = crate::agents::ui::active_dictation(&mut shell.agents)?;
    shell.open_settings(None);
    assert!(tx.send(crate::voice::Event::Listening).is_err());
    assert_eq!(
        shell
            .agents
            .positions
            .get("one")
            .ok_or_else(|| anyhow::anyhow!("position"))?
            .draft
            .text(),
        "keep draft"
    );
    shell.settings = None;
    let tx = crate::agents::ui::active_dictation(&mut shell.agents)?;
    storage.update_config(|saved| saved.preferences.appearance.message_separators = true)?;
    shell.settings_refreshed = None;
    shell.refresh_settings();
    tx.send(crate::voice::Event::Listening)?;
    storage.update_config(|saved| saved.voice_enabled = false)?;
    shell.settings_refreshed = None;
    shell.refresh_settings();
    assert!(tx.send(crate::voice::Event::Listening).is_err());
    assert_eq!(
        shell
            .agents
            .positions
            .get("one")
            .ok_or_else(|| anyhow::anyhow!("position"))?
            .draft
            .text(),
        "keep draft"
    );
    Ok(())
}

#[test]
fn invalid_persisted_global_cannot_steal_typing_from_a_coding_transcript() -> Result<()> {
    use crate::agents::{Job, Launch, Session, ui::Focus};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    };
    let mut config = Config::default();
    config.preferences.keyboard.bindings.insert(
        ids::SETTINGS_OPEN.0.into(),
        key(BindingKey::Character('z'), 0),
    );
    let mut shell = Shell::new(storage, config, None);
    shell.agents_active = true;
    shell.agents.selected = Some("one".into());
    shell.agents.sessions.insert(
        "one".into(),
        Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: dir.path().into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        ),
    );
    shell.agents.drilled = true;
    shell.agents.focus = Focus::Conversation;
    shell.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(shell.settings.is_none());
    assert_eq!(shell.agents.focus, Focus::Composer);
    assert_eq!(
        shell
            .agents
            .positions
            .get("one")
            .ok_or_else(|| anyhow::anyhow!("draft"))?
            .draft
            .text(),
        "z"
    );
    Ok(())
}

#[test]
fn embedded_browser_receives_raw_ctrl_c_instead_of_copy_or_quit() -> Result<()> {
    for custom in [false, true] {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let mut config = Config::default();
        if custom {
            config
                .preferences
                .keyboard
                .bindings
                .insert(ids::APP_QUIT.0.into(), key(BindingKey::Function(11), 0));
        }
        let mut shell = Shell::new(storage, config, None);
        shell.agents.selected = Some("sample".into());
        shell.agents.drilled = true;
        let draft = &mut shell
            .agents
            .positions
            .entry("sample".into())
            .or_default()
            .draft;
        *draft = crate::editor::Editor::from("selected draft");
        draft.anchor = Some(0);
        crate::agents::ui::install_browser(&mut shell.agents)?;
        assert_eq!(
            shell.agents.shortcut_context(),
            crate::shortcuts::contexts::AGENT_BROWSER
        );
        shell.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!shell.reviews.quit);
        assert!(shell.agents.clipboard.is_none());
        assert_eq!(
            crate::agents::ui::browser_keys(&shell.agents)?,
            vec![serde_json::json!({
                "type":"key", "key":"c", "kind":"press",
                "mods":{"shift":false,"alt":false,"ctrl":true,"super":false}
            })]
        );
    }
    Ok(())
}

#[test]
fn native_tweaks_and_unmasked_slack_ctrl_c_copy_before_shell_quit() -> Result<()> {
    use crate::{agents::ui::Modal, editor::Editor};
    for slack in [false, true] {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let mut shell = Shell::new(storage, Config::default(), None);
        if slack {
            crate::agents::ui::prepare_slack_copy(&mut shell.agents, 2);
        } else {
            let mut value = Editor::from("123");
            value.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
            shell.agents.modal = Some(Modal::Tweaks {
                fields: Box::new([
                    value,
                    Editor::default(),
                    Editor::default(),
                    Editor::default(),
                ]),
                field: 0,
            });
        }
        shell.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!shell.reviews.quit);
        assert_eq!(
            shell.agents.clipboard.as_deref(),
            Some(if slack { "safe member" } else { "123" })
        );
        shell.agents.clipboard = None;
        if slack {
            crate::agents::ui::prepare_slack_copy(&mut shell.agents, 0);
        } else if let Some(Modal::Tweaks { field, .. }) = &mut shell.agents.modal {
            *field = 4;
        }
        shell.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(
            shell.reviews.quit,
            "no current copy selection retains original quit fallback"
        );
        assert!(shell.agents.clipboard.is_none());
    }
    Ok(())
}

#[test]
fn new_slack_and_tweaks_contexts_protect_editing_from_invalid_stored_globals() -> Result<()> {
    use crate::{agents::ui::Modal, editor::Editor};
    for slack in [false, true] {
        for field in [0, 2, 4] {
            let dir = tempfile::tempdir()?;
            let storage = Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            };
            let mut config = Config::default();
            config.preferences.keyboard.bindings.insert(
                ids::PALETTE_OPEN.0.into(),
                key(BindingKey::Character('q'), 0),
            );
            let mut shell = Shell::new(storage, config, None);
            if slack {
                crate::agents::ui::prepare_slack_copy(&mut shell.agents, field);
            } else {
                let mut value = Editor::from("123");
                value.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
                shell.agents.modal = Some(Modal::Tweaks {
                    fields: Box::new([value.clone(), value.clone(), value, Editor::default()]),
                    field,
                });
            }
            shell.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
            assert_eq!(
                shell.palette.is_some(),
                field == 4,
                "slack={slack} field={field}"
            );
            if field < 4 && !(slack && field == 0) {
                shell
                    .agents
                    .key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
                assert!(shell.agents.copy_chat_selection());
                assert_eq!(shell.agents.clipboard.as_deref(), Some("q"));
            }
        }
    }
    Ok(())
}

#[test]
fn embedded_browser_focus_and_raw_protocol_keep_each_placement_and_removed_alias() -> Result<()> {
    use crate::settings::HelperPlacement;
    for placement in [
        HelperPlacement::Right,
        HelperPlacement::Bottom,
        HelperPlacement::Main,
    ] {
        for (action, code, modifiers) in [
            (ids::NAVIGATION_NEXT_FOCUS, KeyCode::Tab, KeyModifiers::NONE),
            (
                ids::NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
            ),
            (
                ids::NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::Tab,
                KeyModifiers::SHIFT,
            ),
        ] {
            for custom in [false, true] {
                let dir = tempfile::tempdir()?;
                let storage = Storage {
                    config: dir.path().join("config.json"),
                    cache: dir.path().join("cache"),
                };
                let mut config = Config::default();
                config.preferences.layout.helper_placement = Some(placement);
                if custom {
                    config
                        .preferences
                        .keyboard
                        .bindings
                        .insert(action.0.into(), key(BindingKey::Character('g'), 2));
                }
                let mut shell = Shell::new(storage, config, None);
                shell.agents.drilled = true;
                shell.agents.selected = Some("sample".into());
                shell.agents.sessions.insert(
                    "sample".into(),
                    crate::agents::Session::new(
                        "sample".into(),
                        crate::agents::Job::Coding(crate::agents::Launch {
                            repository: dir.path().into(),
                            isolated: false,
                            base: "HEAD".into(),
                            prompt: "safe draft".into(),
                            model: None,
                            effort: None,
                        }),
                    ),
                );
                crate::agents::ui::install_browser(&mut shell.agents)?;
                assert_eq!(
                    shell.agents.shortcut_context().0,
                    if placement == HelperPlacement::Main {
                        "AgentBrowserMain"
                    } else {
                        "AgentBrowser"
                    }
                );
                let old = KeyEvent::new(code, modifiers);
                shell.key(old);
                let raw = crate::agents::ui::browser_keys(&shell.agents)?;
                let passes = placement == HelperPlacement::Main || custom;
                if passes && code == KeyCode::Tab {
                    assert_eq!(
                        raw,
                        vec![
                            serde_json::json!({"type":"key","key":"tab","kind":"press","mods":{"shift":modifiers.contains(KeyModifiers::SHIFT),"alt":false,"ctrl":false,"super":false}})
                        ]
                    );
                } else {
                    assert!(
                        raw.is_empty(),
                        "placement={placement:?} action={action:?} code={code:?} custom={custom}: {raw:?}"
                    );
                }
                assert_eq!(
                    shell.agents.browser_input(),
                    passes,
                    "placement={placement:?} action={action:?} code={code:?} custom={custom}"
                );
                if custom {
                    shell.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT));
                    assert_eq!(
                        shell.agents.browser_input(),
                        placement == HelperPlacement::Main
                    );
                    if placement == HelperPlacement::Main {
                        assert_eq!(
                            crate::agents::ui::browser_keys(&shell.agents)?.last(),
                            Some(
                                &serde_json::json!({"type":"key","key":"g","kind":"press","mods":{"shift":false,"alt":true,"ctrl":false,"super":false}})
                            )
                        );
                    }
                }
            }
        }
        for custom_quit in [false, true] {
            let dir = tempfile::tempdir()?;
            let storage = Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            };
            let mut config = Config::default();
            config.preferences.layout.helper_placement = Some(placement);
            if custom_quit {
                config
                    .preferences
                    .keyboard
                    .bindings
                    .insert(ids::APP_QUIT.0.into(), key(BindingKey::Function(11), 0));
            }
            let mut shell = Shell::new(storage, config, None);
            crate::agents::ui::install_browser(&mut shell.agents)?;
            shell.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
            assert!(!shell.reviews.quit);
            assert_eq!(
                crate::agents::ui::browser_keys(&shell.agents)?.last(),
                Some(
                    &serde_json::json!({"type":"key","key":"c","kind":"press","mods":{"shift":false,"alt":false,"ctrl":true,"super":false}})
                )
            );
        }
    }
    Ok(())
}

#[test]
fn native_filter_control_u_predicate_cannot_be_stolen_by_stored_globals() -> Result<()> {
    use crate::{agents::ui::Modal, editor::Editor};
    for surface in ["commands", "menu", "help", "repository"] {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let mut config = Config::default();
        config.preferences.keyboard.bindings.insert(
            ids::PALETTE_OPEN.0.into(),
            key(BindingKey::Character('u'), 3),
        );
        let mut shell = Shell::new(storage, config, None);
        shell.agents.modal = Some(match surface {
            "commands" => Modal::Commands {
                query: Editor::from("filter"),
                selected: 2,
                skills_only: false,
                files_only: false,
            },
            "menu" => Modal::Menu {
                query: Editor::from("filter"),
                selected: 2,
            },
            "help" => Modal::Help(crate::help::State {
                query: Editor::from("filter"),
                ..Default::default()
            }),
            _ => Modal::Repository(Editor::from("filter")),
        });
        shell.key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert!(
            shell.palette.is_none(),
            "{surface} explicit ControlU convention"
        );
        let value = match &shell.agents.modal {
            Some(Modal::Commands { query, .. } | Modal::Menu { query, .. }) => query.text(),
            Some(Modal::Help(state)) => state.query.text(),
            Some(Modal::Repository(editor)) => editor.text(),
            _ => anyhow::bail!("lost native editor"),
        };
        assert_eq!(value, "");
    }
    Ok(())
}

#[test]
fn filter_control_u_capture_and_locked_save_reject_only_owned_editing_contexts() -> Result<()> {
    use crate::settings::{SettingId, SettingValue};
    let dir = tempfile::tempdir()?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().join("cache"),
    };
    storage.update_config(|saved| *saved = Config::default())?;
    let bytes = std::fs::read(&storage.config)?;
    let mut shell = Shell::new(storage.clone(), Config::default(), None);
    shell.open_settings(None);
    shell.settings_action(SettingId::Shortcut(ids::PALETTE_OPEN.0.into()));
    shell.key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    let panel = shell
        .settings
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("settings"))?;
    assert!(
        panel
            .error
            .as_ref()
            .is_some_and(|error| error.contains("reserved for text editing"))
    );
    assert!(panel.draft.edited().is_empty());
    panel.draft.set(
        SettingId::Shortcut(ids::PALETTE_OPEN.0.into()),
        SettingValue::Shortcut(Some(key(BindingKey::Character('u'), 3))),
    )?;
    assert!(storage.save_settings_draft(&mut panel.draft).is_err());
    assert_eq!(std::fs::read(&storage.config)?, bytes);
    assert!(!panel.draft.edited().is_empty());
    let mut permitted = Config::default();
    permitted.preferences.keyboard.bindings.insert(
        ids::REMOTE_COPY.0.into(),
        key(BindingKey::Character('u'), 3),
    );
    crate::shortcuts::Bindings::new(&permitted).validate()?;
    assert!(crate::shortcuts::Bindings::new(&permitted).matches(
        ids::REMOTE_COPY,
        crate::shortcuts::contexts::AGENT_REMOTE,
        KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )
    ));
    Ok(())
}
