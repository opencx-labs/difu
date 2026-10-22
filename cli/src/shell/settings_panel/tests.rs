use super::*;
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
fn storage(dir: &tempfile::TempDir) -> Storage {
    Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().into(),
    }
}
#[test]
fn intent_search_and_modified_filter_follow_draft_edits() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), None)?;
    panel.query = "stop flashing".into();
    assert_eq!(
        panel.rows().first().map(|s| &s.id),
        Some(&SettingId::Motion)
    );
    panel.query = "change question key".into();
    assert_eq!(
        panel.rows().first().map(|s| &s.id),
        Some(&SettingId::Shortcut("question.open".into()))
    );
    panel.query = "nothing matches this phrase".into();
    assert!(panel.rows().is_empty());
    panel.query.clear();
    panel.modified_only = true;
    assert!(panel.rows().is_empty());
    panel
        .draft
        .set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    assert_eq!(
        panel
            .rows()
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
        vec![SettingId::WrapDiff]
    );
    Ok(())
}
#[test]
fn save_conflict_retains_draft_and_discard_reads_latest() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, None)?;
    panel
        .draft
        .set(SettingId::ReplySpeed, SettingValue::Number(15))?;
    storage.update_config(|c| c.tweaks.agent_reply_cps = 40)?;
    panel.save(&storage);
    assert!(panel.error.is_some());
    assert_eq!(
        panel.draft.value(&SettingId::ReplySpeed),
        SettingValue::Number(15)
    );
    panel.discard(&storage);
    assert_eq!(
        panel.draft.value(&SettingId::ReplySpeed),
        SettingValue::Number(40)
    );
    assert!(panel.draft.edited().is_empty());
    panel
        .draft
        .set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    assert!(panel.save(&storage).is_some());
    assert!(storage.load_config()?.wrap_diff);
    Ok(())
}
#[test]
fn section_reset_export_and_escape_never_lose_unsaved_work() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Code))?;
    panel
        .draft
        .set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    panel
        .draft
        .set(SettingId::VoiceEnabled, SettingValue::Bool(true))?;
    panel.reset_section();
    assert_eq!(
        panel.draft.value(&SettingId::WrapDiff),
        SettingValue::Bool(false)
    );
    assert_eq!(panel.draft.edited().len(), 1);
    assert!(
        !panel
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &storage)
            .close
    );
    assert!(panel.confirm_discard);
    panel.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &storage);
    assert!(!panel.confirm_discard);
    panel.export(&storage)?;
    let path = panel.export_path.as_ref().context("export path")?;
    let exported: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    assert!(exported.get("input.voice").is_some());
    assert!(exported.get("connections.providers").is_none());
    assert!(!storage.config.exists());
    Ok(())
}
#[test]
fn narrow_panel_keeps_search_selection_details_and_actions_reachable() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Code))?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(38, 18))?;
    terminal.draw(|frame| panel.draw(frame))?;
    let content = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(content.contains("Settings"));
    assert!(content.contains("^S Save"));
    panel.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &storage);
    panel.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &storage);
    assert!(!panel.draft.edited().is_empty());
    terminal.draw(|frame| panel.draw(frame))?;
    assert_eq!(panel.draft.edited().len(), 1);
    Ok(())
}
#[test]
fn numeric_validation_keeps_draft_and_editor_until_a_valid_value_is_entered() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Motion))?;
    panel.query = "Reply speed".into();
    panel.search_focused = false;
    panel.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &storage);
    panel.edit = Some("not a number".into());
    panel.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &storage);
    assert!(panel.error.is_some());
    assert!(panel.draft.edited().is_empty());
    assert!(panel.edit.is_some());
    panel.edit = Some("0".into());
    panel.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &storage);
    assert!(panel.error.is_none());
    assert_eq!(panel.draft.config().tweaks.agent_reply_cps, 0);
    assert!(panel.edit.is_none());
    Ok(())
}
#[test]
fn keyboard_can_read_full_details_in_a_narrow_terminal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Input))?;
    panel.key(
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL),
        &storage,
    );
    assert!(panel.details_expanded);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(38, 18))?;
    terminal.draw(|frame| panel.draw(frame))?;
    panel.key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL),
        &storage,
    );
    assert!(panel.details_scroll > 0);
    panel.key(
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        &storage,
    );
    assert_eq!(panel.category, Category::Keyboard);
    assert_eq!(panel.details_scroll, 0);
    Ok(())
}
#[test]
fn browsing_every_section_and_real_previews_never_creates_config_or_exports() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, None)?;
    for size in [(1, 1), (38, 18), (90, 30), (140, 40)] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(size.0, size.1))?;
        for category in Category::ALL {
            panel.category = category;
            terminal.draw(|frame| panel.draw(frame))?;
        }
    }
    assert!(!storage.config.exists());
    assert!(!storage.cache.join("preference-exports").exists());
    assert!(panel.draft.edited().is_empty());
    Ok(())
}
#[test]
fn real_diff_renderer_switches_between_split_and_unified_preview() {
    let mut config = crate::storage::Config::default();
    let split = draw::diff_preview(&config, 80)
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    config.unified = true;
    let unified = draw::diff_preview(&config, 80)
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(split.contains('│'));
    assert!(!unified.contains('│'));
    assert!(unified.contains("Hello, Difu!"));
    assert!(unified.contains("Hi"));
}
#[test]
fn supplied_shortcuts_rank_exact_aliases_before_other_label_matches() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), None)?;
    let mut spec = SettingSpec::shortcut("synthetic.action", "Example action");
    spec.aliases = &["voice"];
    panel.set_shortcuts(vec![spec]);
    panel.query = "VOICE".into();
    assert_eq!(
        panel.rows().first().map(|s| s.id.clone()),
        Some(SettingId::Shortcut("synthetic.action".into()))
    );
    Ok(())
}
#[test]
fn real_transcript_preview_uses_expansion_and_motion_draft_values() {
    use crate::agents::ui::settings_preview::SettingsPreview;
    let replay = Instant::now();
    let mut sample = SettingsPreview::new(replay);
    let mut config = crate::storage::Config::default();
    let compact = sample
        .render(&config, 65, replay, false)
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    config.preferences.conversation.tool_display = ToolDisplay::Expanded;
    let expanded = sample
        .render(&config, 65, replay, false)
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(!compact.contains("Extra tool output"));
    assert!(expanded.contains("Extra tool output"));
    config.preferences.motion.level = Motion::Off;
    let off = sample
        .render(&config, 65, replay, true)
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert_eq!(expanded, off);
}
#[test]
fn preview_test_request_is_explicit_and_never_edits_or_saves() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Notifications))?;
    let outcome = panel.key(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        &storage,
    );
    assert_eq!(outcome.preview_test, Some(Preview::Notification));
    assert!(outcome.action.is_none());
    assert!(panel.draft.edited().is_empty());
    assert!(!storage.config.exists());
    Ok(())
}
#[test]
fn provider_choice_hook_cycles_both_ways_including_automatic() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::AgentDefaults))?;
    panel.query = "Default model".into();
    panel.search_focused = false;
    panel.set_dynamic_choices(
        SettingId::DefaultModel,
        vec!["first".into(), "second".into()],
    );
    panel.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &storage);
    assert_eq!(
        panel.draft.value(&SettingId::DefaultModel),
        SettingValue::Text(Some("second".into()))
    );
    panel.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &storage);
    assert_eq!(
        panel.draft.value(&SettingId::DefaultModel),
        SettingValue::Text(None)
    );
    panel.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &storage);
    assert_eq!(
        panel.draft.value(&SettingId::DefaultModel),
        SettingValue::Text(Some("first".into()))
    );
    assert!(!storage.config.exists());
    Ok(())
}
#[test]
fn keyboard_section_reset_preserves_unknown_future_overrides() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|c| {
        c.preferences
            .keyboard
            .bindings
            .insert("future.action".into(), KeyChord::character('x'));
    })?;
    let mut panel = Panel::new(&storage, Some(Category::Keyboard))?;
    panel.reset_section();
    assert_eq!(
        panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .get("future.action"),
        Some(&KeyChord::character('x'))
    );
    assert!(panel.draft.edited().is_empty());
    Ok(())
}
#[test]
fn disabled_reply_animation_does_not_request_periodic_preview_repaints() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), Some(Category::Motion))?;
    panel
        .draft
        .set(SettingId::ReplySpeed, SettingValue::Number(0))?;
    assert!(!panel.animating());
    Ok(())
}

fn narrow_screen(panel: &mut Panel) -> Result<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(38, 18))?;
    terminal.draw(|frame| panel.draw(frame))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect())
}
#[test]
fn narrow_long_label_controls_show_current_value_in_list_and_expanded_details() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    for (id, query) in [
        (
            SettingId::QuestionOverridesSelection,
            "Question key overrides text selection",
        ),
        (
            SettingId::QuestionNotification,
            "Question and approval notifications",
        ),
    ] {
        let mut panel = Panel::new(&storage, None)?;
        panel.query = query.into();
        for expanded in [false, true] {
            panel.details_expanded = expanded;
            panel.draft.set(id.clone(), SettingValue::Bool(false))?;
            assert!(narrow_screen(&mut panel)?.contains("Current: Off"));
            panel.draft.set(id.clone(), SettingValue::Bool(true))?;
            assert!(narrow_screen(&mut panel)?.contains("Current: On"));
        }
    }
    Ok(())
}
#[test]
fn narrow_details_show_in_progress_numeric_value() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), Some(Category::Motion))?;
    panel.query = "Reply speed".into();
    panel.details_expanded = true;
    panel.edit = Some("123".into());
    assert!(narrow_screen(&mut panel)?.contains("Editing: 123"));
    Ok(())
}
#[test]
fn narrow_footer_keeps_details_scroll_and_navigation_instructions_visible() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), None)?;
    let screen = narrow_screen(&mut panel)?;
    for hint in ["Details", "Scroll", "PgUp/Dn", "Tab Focus", "Esc"] {
        assert!(screen.contains(hint), "missing visible instruction: {hint}");
    }
    Ok(())
}

#[test]
fn motion_controls_route_to_dedicated_synthetic_previews() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 40))?;
    for (id, label, scene) in [
        (
            SettingId::SidebarEffect,
            "Sidebar effects",
            "Session sidebar",
        ),
        (
            SettingId::ScreenTransitionEffect,
            "Screen transitions",
            "Switch to Changes",
        ),
        (
            SettingId::InputClearDuration,
            "Input clearing duration",
            "Composer after send",
        ),
    ] {
        let mut panel = Panel::new(&storage, Some(Category::Motion))?;
        panel.query = label.into();
        assert_eq!(panel.rows().first().map(|s| &s.id), Some(&id));
        // These effects must run even when the generic transcript has no effect.
        panel
            .draft
            .set(SettingId::ReplyToolEffect, SettingValue::Bool(false))?;
        panel
            .draft
            .set(SettingId::SentDuration, SettingValue::Number(0))?;
        terminal.draw(|frame| panel.draw(frame))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains(scene), "{label} should show {scene}");
        assert!(panel.animating(), "{label} needs repainting");
        panel.replay = panel
            .replay
            .checked_sub(std::time::Duration::from_secs(2))
            .context("earlier replay")?;
        terminal.draw(|frame| panel.draw(frame))?;
        assert!(!panel.animating(), "{label} must finish");
        let outcome = panel.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &storage,
        );
        assert!(outcome.action.is_none() && outcome.saved.is_none() && !outcome.close);
        terminal.draw(|frame| panel.draw(frame))?;
        assert!(panel.animating(), "{label} Ctrl+P must replay the effect");
        panel.draft.set(
            id.clone(),
            if id == SettingId::InputClearDuration {
                SettingValue::Number(0)
            } else {
                SettingValue::Bool(false)
            },
        )?;
        terminal.draw(|frame| panel.draw(frame))?;
        assert!(!panel.animating());
        assert!(!storage.config.exists(), "preview never saves config");
    }
    Ok(())
}

#[test]
fn diff_preview_uses_context_gutters_wrap_and_selected_whitespace() {
    let mut config = Config {
        unified: true,
        ..Default::default()
    };
    config.preferences.code.context_lines = 0;
    config.preferences.code.line_numbers = false;
    config.preferences.code.whitespace = crate::settings::Whitespace::SelectedLines;
    let text = draw::diff_preview(&config, 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("····println!"), "{text}");
    assert!(
        text.contains("    println!(\"Hi\")"),
        "unselected old line: {text}"
    );
    assert!(!text.contains("fn greeting"), "{text}");
    config.unified = false;
    let split = draw::diff_preview(&config, 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        split.contains("    println!(\"Hi\")"),
        "unselected split side: {split}"
    );
    assert!(
        split.contains("····println!"),
        "selected split side: {split}"
    );
    config.unified = true;
    let mut counts = Vec::new();
    for context in [0, 3, 5, 10] {
        config.preferences.code.context_lines = context;
        counts.push(draw::diff_preview(&config, 100).len());
    }
    assert_eq!(counts, vec![2, 8, 12, 22]);
    config.wrap_diff = false;
    let short = draw::diff_preview(&config, 18).len();
    config.wrap_diff = true;
    assert!(draw::diff_preview(&config, 18).len() > short);
}

#[test]
fn settings_navigation_uses_committed_custom_keys_until_save() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|config| {
        config.preferences.keyboard.bindings.insert(
            "settings.down".into(),
            crate::shortcuts::key(BindingKey::Function(8), 0),
        );
    })?;
    let mut panel = Panel::new(&storage, Some(Category::Motion))?;
    panel.search_focused = false;
    panel.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &storage);
    assert_eq!(panel.selected, 0, "old key must not navigate");
    panel.key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE), &storage);
    assert_eq!(panel.selected, 1);
    panel.draft.set(
        SettingId::Shortcut("settings.down".into()),
        SettingValue::Shortcut(Some(crate::shortcuts::key(BindingKey::Function(9), 0))),
    )?;
    panel.key(KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE), &storage);
    assert_eq!(
        panel.selected, 1,
        "draft bindings cannot control the editor"
    );
    panel.key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE), &storage);
    assert_eq!(panel.selected, 2);
    panel.key(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        &storage,
    );
    panel.key(KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE), &storage);
    assert_eq!(panel.selected, 3);
    Ok(())
}

#[test]
fn every_settings_action_has_default_and_remapped_observable_effects() -> Result<()> {
    use crate::shortcuts::{ids, key};
    let cases = [
        (
            ids::SETTINGS_SAVE,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_DISCARD,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_RESET_SECTION,
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_EXPORT,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_MODIFIED,
            KeyCode::Char('m'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_REPLAY,
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_TEST,
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_SEARCH,
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_DETAILS,
            KeyCode::Char('i'),
            KeyModifiers::CONTROL,
        ),
        (
            ids::SETTINGS_DETAILS_DOWN,
            KeyCode::Down,
            KeyModifiers::CONTROL,
        ),
        (ids::SETTINGS_DETAILS_UP, KeyCode::Up, KeyModifiers::CONTROL),
        (ids::SETTINGS_FOCUS, KeyCode::Tab, KeyModifiers::NONE),
        (
            ids::SETTINGS_PREVIOUS_SECTION,
            KeyCode::PageUp,
            KeyModifiers::NONE,
        ),
        (
            ids::SETTINGS_NEXT_SECTION,
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ),
        (ids::SETTINGS_UP, KeyCode::Up, KeyModifiers::NONE),
        (ids::SETTINGS_DOWN, KeyCode::Down, KeyModifiers::NONE),
        (ids::SETTINGS_ACTIVATE, KeyCode::Enter, KeyModifiers::NONE),
        (
            ids::SETTINGS_PREVIOUS_VALUE,
            KeyCode::Left,
            KeyModifiers::NONE,
        ),
        (
            ids::SETTINGS_SEARCH_SLASH,
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        ),
        (
            ids::SETTINGS_EDIT_ACCEPT,
            KeyCode::Enter,
            KeyModifiers::NONE,
        ),
        (
            ids::SETTINGS_DISCARD_CLOSE,
            KeyCode::Enter,
            KeyModifiers::NONE,
        ),
        (
            ids::SETTINGS_RESET_BINDING,
            KeyCode::Backspace,
            KeyModifiers::NONE,
        ),
    ];
    for (action, code, modifiers) in cases {
        for custom in [false, true] {
            let dir = tempfile::tempdir()?;
            let storage = storage(&dir);
            storage.update_config(|c| {
                if custom {
                    c.preferences
                        .keyboard
                        .bindings
                        .insert(action.0.into(), key(BindingKey::Function(6), 0));
                }
            })?;
            let mut panel = Panel::new(&storage, Some(Category::Motion))?;
            panel.search_focused = false;
            panel.replay = Instant::now() - std::time::Duration::from_secs(1);
            let replay = panel.replay;
            panel.details_scroll = 2;
            match action {
                ids::SETTINGS_SAVE | ids::SETTINGS_DISCARD | ids::SETTINGS_DISCARD_CLOSE => panel
                    .draft
                    .set(SettingId::WrapDiff, SettingValue::Bool(true))?,
                ids::SETTINGS_RESET_SECTION => panel
                    .draft
                    .set(SettingId::ReplySpeed, SettingValue::Number(7))?,
                ids::SETTINGS_UP => panel.selected = 1,
                ids::SETTINGS_TEST | ids::SETTINGS_RESET_BINDING => {
                    panel.query = "change question key".into();
                    if action == ids::SETTINGS_RESET_BINDING {
                        panel.draft.set(
                            SettingId::Shortcut("question.open".into()),
                            SettingValue::Shortcut(Some(key(BindingKey::Function(12), 0))),
                        )?;
                    }
                }
                ids::SETTINGS_EDIT_ACCEPT => {
                    panel.query = "Agent reply speed".into();
                    panel.selected = 0;
                    panel.edit = Some("42".into());
                }
                _ => {}
            }
            if action == ids::SETTINGS_EDIT_ACCEPT {
                panel.query.clear();
                panel.selected = panel
                    .rows()
                    .iter()
                    .position(|s| s.id == SettingId::ReplySpeed)
                    .context("reply speed row")?;
            }
            if action == ids::SETTINGS_DISCARD_CLOSE {
                panel.confirm_discard = true;
            }
            let outcome = panel.key(
                if custom {
                    KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)
                } else {
                    KeyEvent::new(code, modifiers)
                },
                &storage,
            );
            match action {
                ids::SETTINGS_SAVE => {
                    assert!(outcome.saved.is_some() && storage.load_config()?.wrap_diff)
                }
                ids::SETTINGS_DISCARD => {
                    assert!(!panel.draft.config().wrap_diff && panel.draft.edited().is_empty())
                }
                ids::SETTINGS_RESET_SECTION => {
                    assert_eq!(panel.draft.config().tweaks.agent_reply_cps, 200)
                }
                ids::SETTINGS_EXPORT => {
                    assert!(panel.export_path.as_ref().is_some_and(|p| p.is_file()))
                }
                ids::SETTINGS_MODIFIED => assert!(panel.modified_only),
                ids::SETTINGS_REPLAY => assert!(panel.replay > replay),
                ids::SETTINGS_TEST => assert!(panel.testing),
                ids::SETTINGS_SEARCH | ids::SETTINGS_SEARCH_SLASH | ids::SETTINGS_FOCUS => {
                    assert!(panel.search_focused)
                }
                ids::SETTINGS_DETAILS => assert!(panel.details_expanded),
                ids::SETTINGS_DETAILS_DOWN => assert_eq!(panel.details_scroll, 3),
                ids::SETTINGS_DETAILS_UP => assert_eq!(panel.details_scroll, 1),
                ids::SETTINGS_PREVIOUS_SECTION => assert_eq!(panel.category, Category::Appearance),
                ids::SETTINGS_NEXT_SECTION => assert_eq!(panel.category, Category::Conversation),
                ids::SETTINGS_UP => assert_eq!(panel.selected, 0),
                ids::SETTINGS_DOWN => assert_eq!(panel.selected, 1),
                ids::SETTINGS_ACTIVATE => assert_eq!(
                    panel.draft.config().preferences.motion.level,
                    Motion::Reduced
                ),
                ids::SETTINGS_PREVIOUS_VALUE => {
                    assert_eq!(panel.draft.config().preferences.motion.level, Motion::Off)
                }
                ids::SETTINGS_EDIT_ACCEPT => {
                    assert_eq!(panel.draft.config().tweaks.agent_reply_cps, 42)
                }
                ids::SETTINGS_DISCARD_CLOSE => {
                    assert!(outcome.close && !panel.draft.config().wrap_diff)
                }
                ids::SETTINGS_RESET_BINDING => assert!(
                    !panel
                        .draft
                        .config()
                        .preferences
                        .keyboard
                        .bindings
                        .contains_key("question.open")
                ),
                _ => anyhow::bail!("unexpected Settings action"),
            }
        }
    }
    Ok(())
}

#[test]
fn input_and_keyboard_reset_keep_their_documented_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|c| {
        c.preferences.input.send_key = SendKey::CtrlEnter;
        c.preferences.keyboard.bindings.insert(
            "question.open".into(),
            crate::shortcuts::key(BindingKey::Function(8), 0),
        );
        c.preferences
            .keyboard
            .bindings
            .insert("future.action".into(), KeyChord::character('x'));
    })?;
    let mut panel = Panel::new(&storage, Some(Category::Input))?;
    panel.reset_section();
    assert_eq!(
        crate::shortcuts::Bindings::new(panel.draft.config()).hint(crate::shortcuts::ids::SEND),
        "Enter"
    );
    assert_eq!(panel.draft.config().preferences.keyboard.bindings.len(), 2);
    assert!(
        panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .contains_key("question.open")
    );
    panel.draft.set(
        SettingId::SendKey,
        SettingValue::Choice("ctrl_enter".into()),
    )?;
    panel.category = Category::Keyboard;
    panel.reset_section();
    assert_eq!(
        crate::shortcuts::Bindings::new(panel.draft.config()).hint(crate::shortcuts::ids::SEND),
        "Enter"
    );
    assert_eq!(panel.draft.config().preferences.keyboard.bindings.len(), 1);
    assert!(
        panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .contains_key("future.action")
    );
    assert_eq!(
        storage.load_config()?.preferences.input.send_key,
        SendKey::CtrlEnter,
        "reset is draft-only"
    );
    panel.save(&storage).context("save reset")?;
    assert_eq!(
        storage.load_config()?.preferences.input.send_key,
        SendKey::Enter
    );
    Ok(())
}

#[test]
fn shortcut_controls_and_effective_send_labels_use_the_real_draft() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Keyboard))?;
    assert!(panel.shortcut_action(&SettingId::ShortcutCapture));
    assert_eq!(panel.capture, Some(crate::shortcuts::ids::QUESTION_OPEN));
    panel.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &storage);
    panel.draft.set(
        SettingId::Shortcut("question.open".into()),
        SettingValue::Shortcut(Some(crate::shortcuts::key(BindingKey::Function(8), 0))),
    )?;
    panel.draft.set(
        SettingId::Shortcut("settings.open".into()),
        SettingValue::Shortcut(Some(crate::shortcuts::key(BindingKey::Function(8), 0))),
    )?;
    assert!(panel.shortcut_action(&SettingId::ShortcutConflicts));
    assert!(
        panel
            .error
            .as_ref()
            .is_some_and(|s| s.contains("conflicts"))
    );
    assert!(panel.shortcut_action(&SettingId::ShortcutReset));
    assert!(
        panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .is_empty()
    );
    panel.shortcut_action(&SettingId::ShortcutConflicts);
    assert!(panel.error.is_none());
    panel.draft.set(
        SettingId::SendKey,
        SettingValue::Choice("ctrl_enter".into()),
    )?;
    panel.draft.set(
        SettingId::Shortcut("composer.send".into()),
        SettingValue::Shortcut(Some(crate::shortcuts::key(BindingKey::Function(9), 0))),
    )?;
    panel.query = "Send message with".into();
    panel.selected = 0;
    panel.details_expanded = true;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 60))?;
    terminal.draw(|f| panel.draw(f))?;
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(screen.contains("Custom"));
    assert!(screen.contains("Send: F9 · Newline: Enter · Explicit queue: Alt+Enter"));
    assert!(
        panel
            .draft
            .edited()
            .contains(&SettingId::Shortcut("composer.send".into()))
    );
    Ok(())
}

#[test]
fn remapped_panel_hints_show_committed_keys_and_capture_ignores_paste_repeat_release() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|c| {
        c.preferences.keyboard.bindings.insert(
            "settings.save".into(),
            crate::shortcuts::key(BindingKey::Function(6), 0),
        );
    })?;
    let mut panel = Panel::new(&storage, Some(Category::Keyboard))?;
    assert!(narrow_screen(&mut panel)?.contains("F6 Save"));
    panel.draft.set(
        SettingId::Shortcut("settings.save".into()),
        SettingValue::Shortcut(Some(crate::shortcuts::key(BindingKey::Function(7), 0))),
    )?;
    assert!(narrow_screen(&mut panel)?.contains("F6 Save"));
    panel.shortcut_action(&SettingId::Shortcut("question.open".into()));
    let query = panel.query.clone();
    panel.paste("Never a key binding");
    for kind in [
        crossterm::event::KeyEventKind::Repeat,
        crossterm::event::KeyEventKind::Release,
    ] {
        panel.key(
            KeyEvent::new_with_kind(KeyCode::F(8), KeyModifiers::NONE, kind),
            &storage,
        );
    }
    assert_eq!(panel.query, query);
    assert!(
        !panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .contains_key("question.open")
    );
    panel.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &storage);
    panel.key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE), &storage);
    assert!(narrow_screen(&mut panel)?.contains("F7 Save"));
    Ok(())
}

#[test]
fn editing_reservations_keep_search_typing_and_capture_isolation() -> Result<()> {
    use crate::shortcuts::{ids, key};
    for (code, chord, before, after) in [
        (
            KeyCode::Char('z'),
            key(BindingKey::Character('z'), 0),
            "",
            "z",
        ),
        (KeyCode::Backspace, key(BindingKey::Backspace, 0), "az", "a"),
    ] {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        storage.update_config(|config| {
            config
                .preferences
                .keyboard
                .bindings
                .insert(ids::SETTINGS_SAVE.0.into(), chord);
        })?;
        let bytes = std::fs::read(&storage.config)?;
        let mut panel = Panel::new(&storage, None)?;
        panel.search_focused = true;
        panel.query = before.into();
        panel
            .draft
            .set(SettingId::WrapDiff, SettingValue::Bool(true))?;
        let outcome = panel.key(KeyEvent::new(code, KeyModifiers::NONE), &storage);
        assert_eq!(panel.query, after);
        assert!(outcome.saved.is_none());
        assert!(!panel.draft.edited().is_empty());
        assert_eq!(std::fs::read(&storage.config)?, bytes);
    }
    for (action, event) in [
        (
            ids::SETTINGS_SAVE,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        ),
        (
            ids::SETTINGS_OPEN,
            KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        let mut panel = Panel::new(&storage, None)?;
        panel.shortcut_action(&SettingId::Shortcut(action.0.into()));
        panel.key(event, &storage);
        assert!(
            panel
                .error
                .as_ref()
                .is_some_and(|s| s.contains("reserved for text editing")),
            "{:?}",
            panel.error
        );
        assert!(panel.draft.edited().is_empty());
        assert_eq!(panel.capture, Some(action));
        assert!(!storage.config.exists());
    }
    Ok(())
}

#[test]
fn editing_reservations_preserve_settings_field_alt_character_input() -> Result<()> {
    use crate::shortcuts::{ids, key};
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|config| {
        config.preferences.keyboard.bindings.insert(
            ids::SETTINGS_EDIT_ACCEPT.0.into(),
            key(BindingKey::Character('z'), 2),
        );
    })?;
    let mut panel = Panel::new(&storage, None)?;
    panel.edit = Some("draft".into());
    panel.key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT),
        &storage,
    );
    assert_eq!(panel.edit.as_deref(), Some("draftz"));
    assert!(panel.draft.edited().is_empty());
    Ok(())
}

#[test]
fn tester_reports_only_contexts_matching_the_pressed_alias() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), Some(Category::Keyboard))?;
    panel.start_shortcut_test();
    for (key, label, present, absent) in [
        ('k', "Move up", "AgentPermissions, AgentMcp", "AgentList"),
        (' ', "Previous page", "ReviewHome", "AgentList"),
        ('c', "Copy selection", "AgentChanges", "AgentComposer"),
    ] {
        panel.shortcut_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        let notice = panel.notice.as_deref().context("notice")?;
        // Space's action is Next page, not Previous page.
        let label = if key == ' ' { "Next page" } else { label };
        let matched = notice
            .split("; ")
            .find(|entry| entry.contains(label))
            .context("action result")?;
        assert!(matched.contains(present), "{matched}");
        assert!(!matched.contains(absent), "{matched}");
    }
    Ok(())
}

#[test]
fn tester_opens_real_synthetic_question_with_draft_binding_without_sending() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Keyboard))?;
    panel.query = "change question key".into();
    panel.draft.set(
        SettingId::Shortcut("question.open".into()),
        SettingValue::Shortcut(Some(crate::shortcuts::key(BindingKey::Function(8), 0))),
    )?;
    panel.start_shortcut_test();
    panel.shortcut_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 55))?;
    terminal.draw(|frame| panel.draw(frame))?;
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        text.contains("Which sample scope should be used?"),
        "{text}"
    );
    for event in [
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    ] {
        panel.shortcut_key(event);
    }
    terminal.draw(|frame| panel.draw(frame))?;
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        text.contains("Which sample scope should be used?"),
        "{text}"
    );
    assert!(!storage.config.exists());
    assert!(!panel.draft.edited().is_empty());
    Ok(())
}

#[test]
fn capture_rejects_embedded_coactive_actions_but_accepts_disjoint_filter() -> Result<()> {
    use crate::shortcuts::{ids, key};
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|config| {
        config.preferences.keyboard.bindings.insert(
            ids::HELPER_PLACEMENT.0.into(),
            key(BindingKey::Function(12), 0),
        );
    })?;
    let bytes = std::fs::read(&storage.config)?;
    let mut panel = Panel::new(&storage, None)?;
    panel.shortcut_action(&SettingId::Shortcut(ids::REVIEW_HELP.0.into()));
    panel.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE), &storage);
    assert!(panel.error.is_some());
    assert_eq!(panel.capture, Some(ids::REVIEW_HELP));
    assert!(panel.draft.edited().is_empty());
    assert_eq!(std::fs::read(&storage.config)?, bytes);
    panel.shortcut_action(&SettingId::Shortcut(ids::REVIEW_FILTER_ACCEPT.0.into()));
    panel.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE), &storage);
    assert!(panel.error.is_none());
    assert!(panel.capture.is_none());
    assert_eq!(
        panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .get(ids::REVIEW_FILTER_ACCEPT.0)
            .context("captured filter binding")?
            .clone(),
        key(BindingKey::Function(12), 0)
    );
    Ok(())
}
#[test]
fn notification_preview_reflects_selected_event_disable_and_quiet_without_actions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), Some(Category::Notifications))?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 50))?;
    panel.selected = panel
        .rows()
        .iter()
        .position(|row| row.id == SettingId::QuestionNotification)
        .context("question row")?;
    panel
        .draft
        .set(SettingId::QuestionNotification, SettingValue::Bool(false))?;
    terminal.draw(|frame| panel.draw(frame))?;
    let content = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(content.contains("Example · Question or approval needed"));
    assert!(content.contains("Disabled in draft"));
    panel
        .draft
        .set(SettingId::QuestionNotification, SettingValue::Bool(true))?;
    panel
        .draft
        .set(SettingId::NotificationQuiet, SettingValue::Bool(true))?;
    terminal.draw(|frame| panel.draw(frame))?;
    let content = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(content.contains("Quiet is on"));
    assert!(!storage(&dir).config.exists());
    Ok(())
}

#[test]
fn notification_samples_render_production_toast_borders_severity_and_wrapping() -> Result<()> {
    use crate::ui::{BG, GREEN, RED};
    let dir = tempfile::tempdir()?;
    let mut panel = Panel::new(&storage(&dir), Some(Category::Notifications))?;
    for width in [80, 140] {
        for (id, color, text) in [
            (
                SettingId::CompletionNotification,
                GREEN,
                "Example · Turn completed",
            ),
            (
                SettingId::ErrorNotification,
                RED,
                "Example · Session failed",
            ),
            (
                SettingId::QuestionNotification,
                GREEN,
                "Example · Question or approval needed",
            ),
        ] {
            panel.selected = panel
                .rows()
                .iter()
                .position(|row| row.id == id)
                .context("row")?;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 40))?;
            terminal.draw(|frame| panel.draw(frame))?;
            let buffer = terminal.backend().buffer();
            let start = buffer
                .content
                .iter()
                .enumerate()
                .find(|(_, c)| c.symbol() == "┌" && c.fg == color)
                .map(|(n, _)| n)
                .context("production toast corner with severity color")?;
            let x = start as u16 % width;
            let y = start as u16 / width;
            let right = (x + 1..width)
                .find(|right| {
                    buffer
                        .cell((*right, y))
                        .is_some_and(|c| c.symbol() == "┐" && c.fg == color)
                })
                .context("top right")?;
            let bottom = (y + 1..40)
                .find(|bottom| {
                    buffer
                        .cell((x, *bottom))
                        .is_some_and(|c| c.symbol() == "└" && c.fg == color)
                })
                .context("bottom left")?;
            let mut lines = Vec::new();
            for row in y + 1..bottom {
                let mut line = String::new();
                for column in x + 1..right {
                    let cell = buffer.cell((column, row)).context("sample cell")?;
                    assert_eq!(cell.fg, color);
                    assert_eq!(cell.bg, BG);
                    line.push_str(cell.symbol());
                }
                lines.push(line.trim_end().to_owned());
            }
            assert_eq!(lines.join(" "), text);
            assert_eq!(
                bottom - y,
                if width == 80 && id == SettingId::QuestionNotification {
                    3
                } else {
                    2
                }
            );
            assert_eq!(
                buffer
                    .cell((right, bottom))
                    .context("bottom right")?
                    .symbol(),
                "┘"
            );
        }
    }
    assert!(!storage(&dir).config.exists());
    Ok(())
}

#[test]
fn discovered_defaults_reject_invalid_selection_without_losing_offline_values() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::AgentDefaults))?;
    panel.draft.set(
        SettingId::DefaultModel,
        SettingValue::Text(Some("fixture-model".into())),
    )?;
    panel.draft.set(
        SettingId::DefaultEffort,
        SettingValue::Text(Some("unsupported".into())),
    )?;
    panel.set_dynamic_choices(SettingId::DefaultModel, vec!["fixture-model".into()]);
    panel.set_dynamic_choices(SettingId::DefaultEffort, vec!["high".into()]);
    assert!(panel.save(&storage).is_none());
    assert_eq!(
        panel.draft.config().agent_defaults.effort.as_deref(),
        Some("unsupported")
    );
    panel.draft.set(
        SettingId::DefaultEffort,
        SettingValue::Text(Some("high".into())),
    )?;
    assert!(panel.save(&storage).is_some());
    panel.reset_section();
    assert!(panel.draft.config().agent_defaults.model.is_none());
    panel.discard(&storage);
    assert_eq!(
        panel.draft.config().agent_defaults.effort.as_deref(),
        Some("high")
    );
    Ok(())
}

#[test]
fn rules_manager_uses_settings_draft_repository_and_returns_to_same_draft() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|c| c.agent_defaults.repository = Some(dir.path().join("saved")))?;
    let mut shell = crate::shell::Shell::new(storage.clone(), storage.load_config()?, None);
    shell.settings = Some(Panel::new(&storage, Some(Category::AgentDefaults))?);
    let path = dir.path().join("draft-target");
    shell.settings.as_mut().context("panel")?.draft.set(
        SettingId::DefaultRepository,
        SettingValue::Text(Some(path.to_string_lossy().into_owned())),
    )?;
    shell.settings_action(SettingId::Rules);
    let Some(crate::agents::ui::Modal::RepositoryRules(form)) = &shell.agents.modal else {
        anyhow::bail!("rules")
    };
    assert_eq!(form.repository.text(), path.to_string_lossy());
    shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(shell.settings.is_some());
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .draft
            .edited()
            .contains(&SettingId::DefaultRepository)
    );
    assert_ne!(storage.load_config()?.agent_defaults.repository, Some(path));
    Ok(())
}

#[test]
fn explicit_sign_in_handoff_keeps_unsaved_draft_and_separate_status_actions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut shell = crate::shell::Shell::new(storage.clone(), Config::default(), None);
    shell.settings = Some(Panel::new(&storage, Some(Category::Connections))?);
    shell
        .settings
        .as_mut()
        .context("panel")?
        .draft
        .set(SettingId::ReplySpeed, SettingValue::Number(17))?;
    assert!(shell.take_sign_in().is_none());
    shell.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for (id, provider) in [
        (
            SettingId::CodexSignIn,
            crate::agents::provider::Provider::Codex,
        ),
        (
            SettingId::ClaudeSignIn,
            crate::agents::provider::Provider::Claude,
        ),
    ] {
        let panel = shell.settings.as_mut().context("panel")?;
        panel.selected = panel
            .rows()
            .iter()
            .position(|row| row.id == id)
            .context("sign-in action")?;
        shell.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(shell.take_sign_in(), Some(provider));
        assert!(shell.take_sign_in().is_none());
        shell.finish_sign_in(false, true);
        assert_eq!(
            shell
                .settings
                .as_ref()
                .context("panel")?
                .draft
                .value(&SettingId::ReplySpeed),
            SettingValue::Number(17)
        );
        assert_eq!(
            storage.load_config()?.tweaks.agent_reply_cps,
            Config::default().tweaks.agent_reply_cps
        );
    }
    shell.settings_action(SettingId::ProviderConnections);
    assert!(shell.take_sign_in().is_none());
    assert!(shell.settings.is_some());
    Ok(())
}

#[test]
fn saving_agent_defaults_changes_only_future_sessions_and_preserves_running_session() -> Result<()>
{
    use crate::agents::{Job, Launch, Session, Status, provider::Provider};
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut shell = crate::shell::Shell::new(storage.clone(), Config::default(), None);
    let mut session = Session::new(
        "running".into(),
        Job::Coding(Launch {
            repository: dir.path().into(),
            isolated: true,
            base: "HEAD".into(),
            prompt: "existing turn".into(),
            model: Some("existing-codex".into()),
            effort: Some("low".into()),
        }),
    );
    session.status = Status::Running;
    session.permission_state.mode = Some("default".into());
    session.completed_turn = Some("previous-completed".into());
    session.completed_turn_successful = true;
    let before = serde_json::to_value(&session)?;
    shell.agents.sessions.insert(session.id.clone(), session);
    shell.agents.selected = Some("running".into());
    shell.settings = Some(Panel::new(&storage, Some(Category::AgentDefaults))?);
    let panel = shell.settings.as_mut().context("panel")?;
    for (id, value) in [
        (SettingId::Provider, SettingValue::Choice("claude".into())),
        (
            SettingId::DefaultModel,
            SettingValue::Text(Some("claude/sonnet".into())),
        ),
        (
            SettingId::DefaultEffort,
            SettingValue::Text(Some("high".into())),
        ),
        (
            SettingId::DefaultRepository,
            SettingValue::Text(Some(dir.path().to_string_lossy().into_owned())),
        ),
        (
            SettingId::StartingMode,
            SettingValue::Text(Some("dontAsk".into())),
        ),
    ] {
        panel.draft.set(id, value)?;
    }
    shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .draft
            .edited()
            .is_empty()
    );
    assert_eq!(
        serde_json::to_value(shell.agents.sessions.get("running").context("session")?)?,
        before
    );
    assert_eq!(shell.agents.defaults.resolved_provider(), Provider::Claude);
    assert_eq!(
        shell.agents.defaults.starting_mode.as_deref(),
        Some("dontAsk")
    );
    assert_eq!(
        storage.load_config()?.agent_defaults.model.as_deref(),
        Some("claude/sonnet")
    );
    let saved = std::fs::read(&storage.config)?;
    shell.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    let reset = &shell
        .settings
        .as_ref()
        .context("panel")?
        .draft
        .config()
        .agent_defaults;
    assert_eq!(reset.resolved_provider(), Provider::Codex);
    assert!(
        reset.model.is_none()
            && reset.effort.is_none()
            && reset.repository.is_none()
            && reset.starting_mode.is_none()
    );
    assert_eq!(std::fs::read(&storage.config)?, saved);
    shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .draft
            .config()
            .agent_defaults
            .starting_mode
            .as_deref(),
        Some("dontAsk")
    );
    shell.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let reset = storage.load_config()?.agent_defaults;
    assert_eq!(reset.resolved_provider(), Provider::Codex);
    assert!(
        reset.model.is_none()
            && reset.effort.is_none()
            && reset.repository.is_none()
            && reset.starting_mode.is_none()
    );
    assert_eq!(
        serde_json::to_value(shell.agents.sessions.get("running").context("session")?)?,
        before
    );
    Ok(())
}

#[test]
fn repository_edit_cannot_save_a_known_unavailable_retained_starting_mode() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    storage.update_config(|config| {
        config.agent_defaults.provider = Some(crate::agents::provider::Provider::Claude);
        config.agent_defaults.starting_mode = Some("auto".into());
    })?;
    let original = std::fs::read(&storage.config)?;
    let mut panel = Panel::new(&storage, Some(Category::AgentDefaults))?;
    panel.draft.set(
        SettingId::DefaultRepository,
        SettingValue::Text(Some(
            dir.path().join("restricted").to_string_lossy().into_owned(),
        )),
    )?;
    panel.set_dynamic_choices(
        SettingId::StartingMode,
        vec!["default".into(), "dontAsk".into()],
    );
    assert!(panel.save(&storage).is_none());
    assert!(
        panel
            .error
            .as_ref()
            .is_some_and(|message| message.contains("unavailable"))
    );
    assert_eq!(std::fs::read(&storage.config)?, original);
    panel
        .draft
        .set(SettingId::StartingMode, SettingValue::Text(None))?;
    assert!(panel.save(&storage).is_some());
    Ok(())
}
