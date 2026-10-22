use super::*;
use crate::storage::{Config, Storage};
use anyhow::{Context, Result};
use std::fs;

fn store(directory: &tempfile::TempDir) -> Storage {
    Storage {
        config: directory.path().join("config.json"),
        cache: directory.path().into(),
    }
}

#[test]
fn legacy_values_remain_canonical_and_provider_is_inferred() -> Result<()> {
    let config: Config = serde_json::from_str(
        r#"{"theme":"nord","unified":true,"agent_panel_right":false,"voice_enabled":true,"agent_defaults":{"model":"claude/sonnet"},"tweaks":{"agent_reply_cps":0,"max_animation_ms":0}}"#,
    )?;
    assert_eq!(
        get(&config, &SettingId::HelperPlacement),
        SettingValue::Choice("main".into())
    );
    assert_eq!(
        get(&config, &SettingId::Provider),
        SettingValue::Choice("claude".into())
    );
    assert_eq!(
        get(&config, &SettingId::ReplySpeed),
        SettingValue::Number(0)
    );
    assert_eq!(config.tweaks.capped(2000), 2000);
    assert_eq!(
        get(&config, &SettingId::ComposerHeight),
        SettingValue::Number(10)
    );
    assert_eq!(
        get(&config, &SettingId::PasteThreshold),
        SettingValue::Choice("500".into())
    );
    assert_eq!(
        get(&config, &SettingId::ToolDisplay),
        SettingValue::Choice("compact".into())
    );
    assert_eq!(
        get(&config, &SettingId::ToolPreviewLines),
        SettingValue::Number(4)
    );
    let decoded: Config = serde_json::from_str(&serde_json::to_string(&config)?)?;
    assert_eq!(
        decoded.agent_defaults.resolved_provider(),
        crate::agents::provider::Provider::Claude
    );
    assert!(decoded.voice_enabled && decoded.unified);
    Ok(())
}

#[test]
fn validation_rejects_invalid_ranges_without_mutating_draft() -> Result<()> {
    let mut draft = SettingsDraft::new(Config::default());
    for (id, value) in [
        (SettingId::ComposerHeight, SettingValue::Number(2)),
        (SettingId::ComposerHeight, SettingValue::Number(21)),
        (SettingId::SidebarWidth, SettingValue::Number(19)),
        (SettingId::SidebarWidth, SettingValue::Number(41)),
        (SettingId::DiffContext, SettingValue::Number(4)),
        (
            SettingId::PasteThreshold,
            SettingValue::Choice("600".into()),
        ),
        (
            SettingId::Theme,
            SettingValue::Choice("unknown-theme".into()),
        ),
        (SettingId::Rules, SettingValue::Bool(true)),
    ] {
        assert!(draft.set(id, value).is_err());
    }
    assert!(draft.edited().is_empty());
    for id in [
        SettingId::ReplySpeed,
        SettingId::SentDuration,
        SettingId::InputClearDuration,
        SettingId::MaxAnimationDelay,
    ] {
        draft.set(id, SettingValue::Number(0))?;
    }
    assert_eq!(draft.config().tweaks.capped(9000), 9000);
    assert_eq!(draft.config().tweaks.agent_reply_cps, 0);
    draft.set(
        SettingId::HelperPlacement,
        SettingValue::Choice("bottom".into()),
    )?;
    let roundtrip: Config = serde_json::from_str(&serde_json::to_string(draft.config())?)?;
    assert_eq!(
        get(&roundtrip, &SettingId::HelperPlacement),
        SettingValue::Choice("bottom".into())
    );
    draft.reset(&SettingId::ReplySpeed)?;
    assert_eq!(draft.config().tweaks.agent_reply_cps, 200);
    Ok(())
}

#[test]
fn drafts_merge_fresh_unrelated_updates_and_remaps_without_replacing_the_draft() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    storage.update_config(|_| {})?;
    let mut first = storage.settings_draft()?;
    let mut other = storage.settings_draft()?;
    first.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    first.set(
        SettingId::Shortcut("settings.open".into()),
        SettingValue::Shortcut(Some(KeyChord::character(','))),
    )?;
    other.set(
        SettingId::Shortcut("session.new".into()),
        SettingValue::Shortcut(Some(KeyChord::character('n'))),
    )?;
    storage.save_settings_draft(&mut other)?;
    storage.update_config(|config| {
        config.voice_enabled = true;
        config.pinned_sessions.insert("keep-pin".into());
    })?;
    let refreshed = storage.refresh_settings()?;
    assert!(refreshed.voice_enabled);
    assert!(!first.config().voice_enabled);
    assert!(first.config().wrap_diff);
    let saved = storage.save_settings_draft(&mut first)?;
    assert!(saved.wrap_diff && saved.voice_enabled);
    assert!(saved.pinned_sessions.contains("keep-pin"));
    assert_eq!(saved.preferences.keyboard.bindings.len(), 2);
    assert!(first.edited().is_empty());
    Ok(())
}

#[test]
fn same_field_conflicts_keep_draft_and_saved_bytes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    storage.update_config(|_| {})?;
    let mut draft = storage.settings_draft()?;
    draft.set(SettingId::ReplySpeed, SettingValue::Number(120))?;
    storage.update_config(|config| config.tweaks.agent_reply_cps = 90)?;
    let before = fs::read(&storage.config)?;
    let error = storage
        .save_settings_draft(&mut draft)
        .err()
        .context("Expected same-field conflict")?;
    assert_eq!(error.conflicts, vec![SettingId::ReplySpeed]);
    assert_eq!(fs::read(&storage.config)?, before);
    assert_eq!(draft.config().tweaks.agent_reply_cps, 120);
    assert_eq!(draft.edited().len(), 1);
    draft.discard(storage.refresh_settings()?);
    assert_eq!(draft.config().tweaks.agent_reply_cps, 90);
    assert!(draft.edited().is_empty());
    Ok(())
}

#[test]
fn failed_updates_and_malformed_documents_preserve_original_bytes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    let original = r#"{"review_repositories":{"owner/repo":true}}"#;
    fs::write(&storage.config, original)?;
    assert!(
        storage
            .try_update_config(|config| {
                config.wrap_diff = true;
                anyhow::bail!("rejected")
            })
            .is_err()
    );
    assert_eq!(fs::read_to_string(&storage.config)?, original);
    let mut draft = storage.settings_draft()?;
    draft.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    fs::write(&storage.config, "{malformed")?;
    assert!(storage.save_settings_draft(&mut draft).is_err());
    assert_eq!(fs::read_to_string(&storage.config)?, "{malformed");
    assert_eq!(draft.edited().len(), 1);
    Ok(())
}

#[test]
fn exported_preferences_exclude_rules_connections_and_session_content() -> Result<()> {
    let mut config = Config::default();
    config
        .repository_rules
        .insert("/secret".into(), "PRIVATE RULE".into());
    config.pinned_sessions.insert("PRIVATE SESSION".into());
    config
        .repositories
        .insert("secret/repo".into(), "/secret".into());
    let mut draft = SettingsDraft::new(config);
    draft.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    let export: serde_json::Value = serde_json::from_str(&export_preferences(draft.config())?)?;
    assert_eq!(
        export.get("diff.wrap"),
        Some(&serde_json::json!({"Bool":true}))
    );
    let text = export.to_string();
    assert!(
        !text.contains("PRIVATE")
            && !text.contains("connections")
            && !text.contains("repository_rules")
    );
    Ok(())
}

#[test]
fn registry_search_prioritizes_aliases_and_covers_all_sections() -> Result<()> {
    let registry = registry();
    let sections: std::collections::BTreeSet<_> =
        registry.iter().map(|entry| entry.category).collect();
    assert_eq!(sections.len(), 10);
    assert_eq!(
        search("stop flashing").first().map(|spec| &spec.id),
        Some(&SettingId::Motion)
    );
    assert_eq!(
        search("change question key").first().map(|spec| &spec.id),
        Some(&SettingId::Shortcut("question.open".into()))
    );
    assert!(
        registry
            .iter()
            .filter(|entry| entry.kind == SettingKind::Action)
            .all(|entry| entry.effect == Effect::SeparateAction)
    );
    assert!(
        registry
            .iter()
            .filter(|entry| entry.effect == Effect::NextLaunch)
            .all(|entry| entry.id == SettingId::StartupTab)
    );
    Ok(())
}

#[test]
fn explicit_provider_selection_is_saved_even_when_it_matches_inferred_default() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    let mut draft = storage.settings_draft()?;
    draft.set(SettingId::Provider, SettingValue::Choice("codex".into()))?;
    assert!(draft.edited().contains(&SettingId::Provider));
    storage.save_settings_draft(&mut draft)?;
    assert_eq!(
        storage.load_config()?.agent_defaults.provider,
        Some(crate::agents::provider::Provider::Codex)
    );
    Ok(())
}

#[test]
fn notification_bundles_reset_only_the_draft_and_preserve_unrelated_changes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    storage.update_config(|_| {})?;
    let before = fs::read(&storage.config)?;
    let mut draft = storage.settings_draft()?;
    draft.set(
        SettingId::NotificationPreset,
        SettingValue::Choice("quiet".into()),
    )?;
    assert!(draft.config().preferences.notifications.quiet);
    assert_eq!(
        draft.edited(),
        &std::collections::BTreeSet::from([SettingId::NotificationQuiet])
    );
    assert_eq!(fs::read(&storage.config)?, before);
    storage.update_config(|config| config.voice_enabled = true)?;
    let config = storage.save_settings_draft(&mut draft)?;
    assert!(config.voice_enabled && config.preferences.notifications.quiet);
    draft.reset_all()?;
    assert!(!draft.config().voice_enabled && !draft.config().preferences.notifications.quiet);
    assert!(storage.load_config()?.voice_enabled);
    Ok(())
}

#[test]
fn real_write_failure_preserves_saved_config_and_dirty_draft() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    struct RestorePermissions(std::path::PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    storage.update_config(|_| {})?;
    let before = fs::read(&storage.config)?;
    let mut draft = storage.settings_draft()?;
    draft.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    let _restore = RestorePermissions(directory.path().into());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o500))?;
    let error = storage
        .save_settings_draft(&mut draft)
        .err()
        .context("Expected write permission failure")?;
    assert!(error.conflicts.is_empty());
    assert_eq!(fs::read(&storage.config)?, before);
    assert!(draft.config().wrap_diff && draft.edited().contains(&SettingId::WrapDiff));
    Ok(())
}

#[test]
fn concurrent_draft_writers_merge_under_file_lock() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    storage.update_config(|_| {})?;
    let mut left = storage.settings_draft()?;
    let mut right = storage.settings_draft()?;
    left.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    right.set(SettingId::VoiceEnabled, SettingValue::Bool(true))?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let child_storage = storage.clone();
    let child_barrier = barrier.clone();
    let child = std::thread::spawn(move || {
        child_barrier.wait();
        child_storage.save_settings_draft(&mut left)
    });
    barrier.wait();
    storage.save_settings_draft(&mut right)?;
    child
        .join()
        .map_err(|_| anyhow::anyhow!("Settings writer thread failed"))??;
    let committed = storage.load_config()?;
    assert!(committed.wrap_diff && committed.voice_enabled);
    Ok(())
}

#[test]
fn keyboard_reset_is_a_draft_action_and_never_claims_separate_application() -> Result<()> {
    let reset = spec(&SettingId::ShortcutReset).context("Missing reset metadata")?;
    assert_eq!(reset.effect, Effect::ImmediateOnSave);
    Ok(())
}

#[test]
fn settings_save_patches_only_edited_leaves_and_preserves_unknown_fields() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    fs::write(
        &storage.config,
        r#"{
        "future_top": {"keep": "top-value"},
        "preferences": {
            "future_group": {"keep": true},
            "input": {"composer_max_height": 10, "future_input": "input-value"},
            "keyboard": {"bindings": {
                "session.new": {"key": {"Character": "n"}, "modifiers": {"control": true}, "future_chord": 42}
            }}
        }
    }"#,
    )?;
    let mut draft = storage.settings_draft()?;
    draft.set(SettingId::ComposerHeight, SettingValue::Number(12))?;
    let mut chord = KeyChord::character('n');
    chord.modifiers.alt = true;
    draft.set(
        SettingId::Shortcut("session.new".into()),
        SettingValue::Shortcut(Some(chord)),
    )?;
    storage.save_settings_draft(&mut draft)?;
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&storage.config)?)?;
    assert_eq!(
        saved.pointer("/future_top/keep"),
        Some(&serde_json::json!("top-value"))
    );
    assert_eq!(
        saved.pointer("/preferences/future_group/keep"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        saved.pointer("/preferences/input/future_input"),
        Some(&serde_json::json!("input-value"))
    );
    assert_eq!(
        saved.pointer("/preferences/input/composer_max_height"),
        Some(&serde_json::json!(12))
    );
    assert_eq!(
        saved.pointer("/preferences/keyboard/bindings/session.new/future_chord"),
        Some(&serde_json::json!(42))
    );
    assert_eq!(
        saved.pointer("/preferences/keyboard/bindings/session.new/modifiers/alt"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        saved.pointer("/preferences/keyboard/bindings/session.new/modifiers/control"),
        Some(&serde_json::json!(false))
    );
    assert!(saved.get("voice_enabled").is_none());
    draft.reset(&SettingId::Shortcut("session.new".into()))?;
    storage.save_settings_draft(&mut draft)?;
    let reset: serde_json::Value = serde_json::from_slice(&fs::read(&storage.config)?)?;
    assert!(
        reset
            .pointer("/preferences/keyboard/bindings/session.new")
            .is_none()
    );
    assert_eq!(
        reset.pointer("/preferences/future_group/keep"),
        Some(&serde_json::json!(true))
    );
    Ok(())
}

#[test]
fn no_op_save_keeps_legacy_document_bytes_and_rejects_malformed_without_writing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    let original = "{\"review_repositories\":{\"owner/repo\":true},\"unknown\":3}\n";
    fs::write(&storage.config, original)?;
    let mut draft = storage.settings_draft()?;
    storage.save_settings_draft(&mut draft)?;
    assert_eq!(fs::read_to_string(&storage.config)?, original);
    fs::write(&storage.config, "{broken")?;
    assert!(storage.save_settings_draft(&mut draft).is_err());
    assert_eq!(fs::read_to_string(&storage.config)?, "{broken");
    let mut empty = SettingsDraft::new(Config::default());
    // A missing file in an existing directory is also a no-op, not a request to create config.
    let empty_directory = tempfile::tempdir()?;
    let missing = store(&empty_directory);
    missing.save_settings_draft(&mut empty)?;
    assert!(!missing.config.exists());
    Ok(())
}

#[test]
fn question_metadata_lookup_matches_intent_search() -> Result<()> {
    let found = search("change question key")
        .into_iter()
        .next()
        .context("question search result")?;
    let looked_up = spec(&found.id).context("question metadata")?;
    assert_eq!(looked_up.label, "Open question composer");
    assert!(looked_up.aliases.contains(&"change question key"));
    Ok(())
}

#[test]
fn concurrent_distinct_action_remaps_reject_the_final_context_collision() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = store(&directory);
    storage.update_config(|_| {})?;
    let mut first = storage.settings_draft()?;
    let mut second = storage.settings_draft()?;
    let chord = KeyChord {
        key: BindingKey::Function(12),
        modifiers: Default::default(),
    };
    first.set(
        SettingId::Shortcut("settings.open".into()),
        SettingValue::Shortcut(Some(chord.clone())),
    )?;
    second.set(
        SettingId::Shortcut("question.open".into()),
        SettingValue::Shortcut(Some(chord)),
    )?;
    storage.save_settings_draft(&mut first)?;
    let before = fs::read(&storage.config)?;
    let result = storage.save_settings_draft(&mut second);
    assert!(
        result.is_err(),
        "different edited fields still conflict when their effective contexts overlap"
    );
    assert_eq!(fs::read(&storage.config)?, before);
    assert_eq!(second.edited().len(), 1);
    Ok(())
}

#[test]
fn send_preset_atomically_replaces_custom_companions_and_reports_custom_afterwards() -> Result<()> {
    let mut draft = SettingsDraft::new(Config::default());
    for (action, key) in [
        ("composer.send", 'a'),
        ("composer.newline", 'b'),
        ("composer.queue", 'c'),
    ] {
        draft.set(
            SettingId::Shortcut(action.into()),
            SettingValue::Shortcut(Some(KeyChord::character(key))),
        )?;
    }
    draft.set(
        SettingId::SendKey,
        SettingValue::Choice("ctrl_enter".into()),
    )?;
    for (action, control, alt) in [
        ("composer.send", true, false),
        ("composer.newline", false, false),
        ("composer.queue", false, true),
    ] {
        assert_eq!(
            draft.config().preferences.keyboard.bindings.get(action),
            Some(&KeyChord {
                key: BindingKey::Enter,
                modifiers: KeyModifiers {
                    control,
                    alt,
                    ..Default::default()
                },
            })
        );
        assert!(draft.edited().contains(&SettingId::Shortcut(action.into())));
    }
    draft.set(
        SettingId::Shortcut("composer.send".into()),
        SettingValue::Shortcut(Some(KeyChord {
            key: BindingKey::Function(9),
            modifiers: Default::default(),
        })),
    )?;
    assert_eq!(
        draft.value(&SettingId::SendKey),
        SettingValue::Choice("custom".into())
    );
    Ok(())
}

#[test]
fn keyboard_reset_restores_enter_preset_and_preserves_unknown_future_actions() -> Result<()> {
    let mut config = Config::default();
    config.preferences.input.send_key = SendKey::CtrlEnter;
    config
        .preferences
        .keyboard
        .bindings
        .insert("future.action".into(), KeyChord::character('z'));
    let mut draft = SettingsDraft::new(config);
    draft.set(
        SettingId::Shortcut("question.open".into()),
        SettingValue::Shortcut(Some(KeyChord {
            key: BindingKey::Function(8),
            modifiers: Default::default(),
        })),
    )?;
    draft.reset_all()?;
    assert_eq!(draft.config().preferences.input.send_key, SendKey::Enter);
    assert_eq!(
        draft
            .config()
            .preferences
            .keyboard
            .bindings
            .get("future.action"),
        Some(&KeyChord::character('z'))
    );
    assert_eq!(
        crate::shortcuts::Bindings::new(draft.config()).hint(crate::shortcuts::ids::SEND),
        "Enter"
    );
    Ok(())
}

#[test]
fn preset_selection_checks_all_companions_even_when_the_selected_preset_was_unchanged() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let storage = store(&dir);
    let mut original = storage.settings_draft()?;
    original.set(
        SettingId::SendKey,
        SettingValue::Choice("ctrl_enter".into()),
    )?;
    storage.save_settings_draft(&mut original)?;
    let mut preset = storage.settings_draft()?;
    let mut other = storage.settings_draft()?;
    preset.set(
        SettingId::SendKey,
        SettingValue::Choice("ctrl_enter".into()),
    )?;
    other.set(
        SettingId::Shortcut("composer.send".into()),
        SettingValue::Shortcut(Some(KeyChord {
            key: BindingKey::Function(9),
            modifiers: Default::default(),
        })),
    )?;
    storage.save_settings_draft(&mut other)?;
    let bytes = fs::read(&storage.config)?;
    assert!(
        storage.save_settings_draft(&mut preset).is_err(),
        "explicitly reselecting a bundle must guard its unchanged companion fields"
    );
    assert_eq!(fs::read(&storage.config)?, bytes);
    Ok(())
}

#[test]
fn custom_send_display_does_not_create_preset_edits_or_overwrite_concurrent_fields() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = store(&dir);
    let mut first = storage.settings_draft()?;
    let mut other = storage.settings_draft()?;
    first.set(
        SettingId::Shortcut("composer.send".into()),
        SettingValue::Shortcut(Some(KeyChord {
            key: BindingKey::Function(9),
            modifiers: Default::default(),
        })),
    )?;
    assert_eq!(
        first.value(&SettingId::SendKey),
        SettingValue::Choice("custom".into())
    );
    assert!(!first.edited().contains(&SettingId::SendKey));
    other.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    storage.save_settings_draft(&mut other)?;
    let saved = storage.save_settings_draft(&mut first)?;
    assert!(saved.wrap_diff);
    assert_eq!(saved.preferences.input.send_key, SendKey::Enter);
    assert_eq!(saved.preferences.keyboard.bindings.len(), 1);
    Ok(())
}

#[test]
fn unknown_action_overrides_are_preserved_but_not_editable_controls() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = store(&dir);
    fs::write(
        &storage.config,
        r#"{"preferences":{"keyboard":{"bindings":{"future.action":{"key":"Escape","modifiers":{}}}}}}"#,
    )?;
    let mut draft = storage.settings_draft()?;
    assert!(spec(&SettingId::Shortcut("future.action".into())).is_none());
    assert!(
        draft
            .set(
                SettingId::Shortcut("future.action".into()),
                SettingValue::Shortcut(Some(KeyChord::character('z')))
            )
            .is_err()
    );
    draft.set(SettingId::WrapDiff, SettingValue::Bool(true))?;
    storage.save_settings_draft(&mut draft)?;
    let saved = storage.load_config()?;
    assert!(saved.wrap_diff);
    assert!(
        saved
            .preferences
            .keyboard
            .bindings
            .contains_key("future.action")
    );
    Ok(())
}
