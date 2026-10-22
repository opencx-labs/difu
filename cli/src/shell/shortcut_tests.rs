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
