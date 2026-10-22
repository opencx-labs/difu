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
fn keyboard_section_reset_clears_overrides_even_before_full_registry_arrives() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = storage(&dir);
    let mut panel = Panel::new(&storage, Some(Category::Keyboard))?;
    panel.draft.set(
        SettingId::Shortcut("future.action".into()),
        SettingValue::Shortcut(Some(KeyChord::character('x'))),
    )?;
    panel.reset_section();
    assert!(
        panel
            .draft
            .config()
            .preferences
            .keyboard
            .bindings
            .is_empty()
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
