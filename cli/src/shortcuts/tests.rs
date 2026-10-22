use super::*;
use anyhow::Result;

#[test]
fn catalog_defaults_have_unique_ids_known_contexts_and_no_ambiguous_dispatch() -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    for spec in all() {
        assert!(ids.insert(spec.id), "Duplicate {}", spec.id.0);
        assert!(
            !spec.contexts.is_empty(),
            "Missing contexts for {}",
            spec.id.0
        );
        for context in &spec.contexts {
            assert!(contexts::ALL.contains(context), "Unknown {}", context.0);
        }
        for binding in &spec.defaults {
            assert!(
                binding.contexts.iter().all(|c| spec.contexts.contains(c)),
                "Alias outside action context: {}",
                spec.id.0
            );
        }
    }
    Bindings::new(&Config::default()).validate()?;
    Ok(())
}
#[test]
fn normalized_capture_matches_terminal_equivalents_and_replaces_default_aliases() -> Result<()> {
    for (left, right) in [
        (
            KeyEvent::new(KeyCode::BackTab, M::NONE),
            KeyEvent::new(KeyCode::Tab, M::SHIFT),
        ),
        (
            KeyEvent::new(KeyCode::Char('A'), M::NONE),
            KeyEvent::new(KeyCode::Char('a'), M::SHIFT),
        ),
        (
            KeyEvent::new(KeyCode::Char('{'), M::NONE),
            KeyEvent::new(KeyCode::Char('['), M::SHIFT),
        ),
    ] {
        assert_eq!(capture(left), capture(right));
    }
    let mut config = Config::default();
    let context = contexts::PALETTE;
    assert_eq!(
        Bindings::new(&config).resolve(context, KeyEvent::new(KeyCode::BackTab, M::NONE)),
        Some(ids::PALETTE_UP)
    );
    config
        .preferences
        .keyboard
        .bindings
        .insert("palette.up".into(), key(BindingKey::Function(4), 0));
    let bindings = Bindings::new(&config);
    assert_eq!(
        bindings.resolve(context, KeyEvent::new(KeyCode::F(4), M::NONE)),
        Some(ids::PALETTE_UP)
    );
    assert_eq!(
        bindings.resolve(context, KeyEvent::new(KeyCode::Up, M::NONE)),
        None
    );
    assert_eq!(
        bindings.resolve(context, KeyEvent::new(KeyCode::BackTab, M::SHIFT)),
        None
    );
    assert_eq!(
        bindings.resolve(
            context,
            KeyEvent::new_with_kind(KeyCode::F(4), M::NONE, KeyEventKind::Release)
        ),
        None
    );
    assert_eq!(bindings.hint(ids::PALETTE_UP), "F4");
    Ok(())
}
#[test]
fn contextual_conflicts_include_normalized_aliases_but_allow_disjoint_views() -> Result<()> {
    let mut config = Config::default();
    config
        .preferences
        .keyboard
        .bindings
        .insert("palette.up".into(), key(BindingKey::Function(4), 0));
    config
        .preferences
        .keyboard
        .bindings
        .insert("settings.up".into(), key(BindingKey::Function(4), 0));
    Bindings::new(&config).validate()?;
    config
        .preferences
        .keyboard
        .bindings
        .insert("palette.down".into(), key(BindingKey::Function(4), 0));
    assert!(Bindings::new(&config).validate().is_err());
    config
        .preferences
        .keyboard
        .bindings
        .insert("palette.down".into(), key(BindingKey::BackTab, 0));
    config
        .preferences
        .keyboard
        .bindings
        .insert("palette.up".into(), key(BindingKey::Tab, 4));
    assert!(Bindings::new(&config).validate().is_err());
    Ok(())
}
#[test]
fn question_selection_override_requires_explicit_consent_and_unknown_keys_are_opaque() -> Result<()>
{
    let mut config = Config::default();
    config
        .preferences
        .keyboard
        .bindings
        .insert("question.open".into(), key(BindingKey::Left, 4));
    assert!(Bindings::new(&config).validate().is_err());
    config.preferences.keyboard.question_overrides_selection = true;
    Bindings::new(&config).validate()?;
    config
        .preferences
        .keyboard
        .bindings
        .insert("future.action".into(), key(BindingKey::Escape, 0));
    Bindings::new(&config).validate()?;
    assert!(find("future.action").is_none());
    Ok(())
}

#[test]
fn selection_override_authorizes_only_the_question_action() -> Result<()> {
    let mut config = Config::default();
    config.preferences.keyboard.question_overrides_selection = true;
    config
        .preferences
        .keyboard
        .bindings
        .insert("settings.open".into(), key(BindingKey::Left, 4));
    assert!(
        Bindings::new(&config).validate().is_err(),
        "a question-only opt-in cannot authorize another action to intercept selection"
    );
    Ok(())
}

#[test]
fn editing_reservations_keep_owned_defaults_presets_and_only_exact_question_exception() -> Result<()>
{
    // Explicitly storing an existing default must not disable that alias in its original context.
    for action in all() {
        for default in &action.defaults {
            let mut config = Config::default();
            config
                .preferences
                .keyboard
                .bindings
                .insert(action.id.0.into(), default.chord.clone());
            for context in &default.contexts {
                assert_eq!(
                    Bindings::new(&config).effective(action.id, *context),
                    vec![normalize(default.chord.clone())],
                    "{} in {}",
                    action.id.0,
                    context.0
                );
            }
        }
    }
    for preset in [SendKey::Enter, SendKey::CtrlEnter] {
        let mut config = Config::default();
        config.preferences.input.send_key = preset;
        for (id, chord) in send_companions(preset) {
            config
                .preferences
                .keyboard
                .bindings
                .insert(id.0.into(), chord.clone());
            assert_eq!(
                Bindings::new(&config).effective(id, contexts::AGENT_COMPOSER),
                vec![chord]
            );
        }
        Bindings::new(&config).validate()?;
    }
    let mut config = Config::default();
    config.preferences.keyboard.question_overrides_selection = true;
    for chord in [
        key(BindingKey::Right, 4),
        key(BindingKey::Left, 5),
        key(BindingKey::Character('z'), 0),
    ] {
        config
            .preferences
            .keyboard
            .bindings
            .insert(ids::QUESTION_OPEN.0.into(), chord);
        assert!(Bindings::new(&config).validate().is_err());
        assert!(
            Bindings::new(&config)
                .effective(ids::QUESTION_OPEN, contexts::AGENT_COMPOSER)
                .is_empty()
        );
    }
    config
        .preferences
        .keyboard
        .bindings
        .insert(ids::QUESTION_OPEN.0.into(), key(BindingKey::Left, 4));
    Bindings::new(&config).validate()?;
    assert!(Bindings::new(&config).matches(
        ids::QUESTION_OPEN,
        contexts::AGENT_COMPOSER,
        KeyEvent::new(KeyCode::Left, M::SHIFT)
    ));
    Ok(())
}

#[test]
fn editing_reservations_are_contextual_and_hints_exclude_suppressed_keys() -> Result<()> {
    let mut config = Config::default();
    config.preferences.keyboard.bindings.insert(
        ids::PALETTE_PIN.0.into(),
        key(BindingKey::Character('z'), 0),
    );
    let bindings = Bindings::new(&config);
    assert!(bindings.validate().is_err());
    assert!(bindings.hint(ids::PALETTE_PIN).is_empty());
    assert!(!bindings.matches(
        ids::PALETTE_PIN,
        contexts::PALETTE,
        KeyEvent::new(KeyCode::Char('z'), M::NONE)
    ));
    assert!(
        !bindings.matches(
            ids::PALETTE_PIN,
            contexts::PALETTE,
            KeyEvent::new(KeyCode::Char('p'), M::SUPER)
        ),
        "invalid override must not restore removed defaults"
    );
    config.preferences.keyboard.bindings.clear();
    config.preferences.keyboard.bindings.insert(
        ids::SESSION_NEW.0.into(),
        key(BindingKey::Character('z'), 0),
    );
    Bindings::new(&config).validate()?;
    assert_eq!(
        Bindings::new(&config).resolve(
            contexts::AGENT_LIST,
            KeyEvent::new(KeyCode::Char('z'), M::NONE)
        ),
        Some(ids::SESSION_NEW)
    );
    for (character, modifiers) in [('c', 1), ('v', 1), ('c', 8), ('v', 8)] {
        config.preferences.keyboard.bindings.insert(
            ids::SETTINGS_OPEN.0.into(),
            key(BindingKey::Character(character), modifiers),
        );
        assert!(
            Bindings::new(&config)
                .effective(ids::SETTINGS_OPEN, contexts::AGENT_COMPOSER)
                .is_empty(),
            "copy/paste must retain precedence"
        );
    }
    Ok(())
}

#[test]
fn legacy_question_aliases_stay_contextual_and_opaque_modifiers_are_not_capturable() -> Result<()> {
    for modifiers in [
        M::HYPER,
        M::META,
        M::HYPER | M::META | M::CONTROL | M::ALT | M::SUPER,
    ] {
        let event = KeyEvent::new(KeyCode::Enter, modifiers);
        assert!(capture(event).is_none());
        let mut config = Config::default();
        for context in [contexts::AGENT_QUESTION, contexts::AGENT_QUESTION_NOTE] {
            assert_eq!(
                Bindings::new(&config).resolve(context, event),
                Some(ids::QUESTION_SUBMIT)
            );
        }
        for context in [
            contexts::AGENT_COMPOSER,
            contexts::AGENT_RENAME,
            contexts::AGENT_PLAN,
        ] {
            assert!(!Bindings::new(&config).matches(ids::QUESTION_SUBMIT, context, event));
        }
        config.preferences.keyboard.bindings.insert(
            ids::QUESTION_SUBMIT.0.into(),
            key(BindingKey::Function(12), 0),
        );
        assert!(!Bindings::new(&config).matches(
            ids::QUESTION_SUBMIT,
            contexts::AGENT_QUESTION,
            event
        ));
    }
    let mut config = Config::default();
    config
        .preferences
        .keyboard
        .bindings
        .insert(ids::QUESTION_OPEN.0.into(), key(BindingKey::Left, 4));
    config.preferences.keyboard.question_overrides_selection = true;
    Bindings::new(&config).validate()?;
    let selection = KeyEvent::new(KeyCode::Left, M::SHIFT);
    assert!(Bindings::new(&config).matches(
        ids::QUESTION_OPEN,
        contexts::AGENT_COMPOSER,
        selection
    ));
    for context in [
        contexts::AGENT_RENAME,
        contexts::AGENT_HELP,
        contexts::AGENT_QUEUE_EDITOR,
        contexts::AGENT_PERMISSION_FORM,
    ] {
        assert!(!Bindings::new(&config).matches(ids::QUESTION_OPEN, context, selection));
    }
    Ok(())
}

#[test]
fn permission_control_aliases_are_authoritative_contextual_and_replaceable() -> Result<()> {
    for (action, binding_key, code) in [
        (ids::PERMISSIONS_SUBMIT, BindingKey::Enter, KeyCode::Enter),
        (
            ids::PERMISSIONS_REFRESH_FORM,
            BindingKey::Character('r'),
            KeyCode::Char('r'),
        ),
    ] {
        let mut config = Config::default();
        let bindings = Bindings::new(&config);
        let effective = bindings.effective(action, contexts::AGENT_PERMISSION_FORM);
        assert_eq!(effective.len(), 8);
        for modifiers in [1, 3, 5, 7, 9, 11, 13, 15] {
            assert!(effective.contains(&key(binding_key.clone(), modifiers)));
        }
        let combined = KeyEvent::new(
            code,
            M::CONTROL | M::SHIFT | M::ALT | M::SUPER | M::HYPER | M::META,
        );
        assert!(bindings.matches(action, contexts::AGENT_PERMISSION_MULTILINE, combined));
        assert!(!bindings.matches(action, contexts::AGENT_MCP, combined));
        assert!(!bindings.matches(action, contexts::AGENT_COMPOSER, combined));
        assert!(!bindings.matches(
            action,
            contexts::AGENT_PERMISSION_FORM,
            KeyEvent::new(code, M::HYPER | M::META)
        ));
        assert!(!bindings.matches(
            action,
            contexts::AGENT_PERMISSION_FORM,
            KeyEvent::new_with_kind(code, combined.modifiers, KeyEventKind::Release)
        ));
        config
            .preferences
            .keyboard
            .bindings
            .insert(action.0.into(), key(BindingKey::Function(12), 0));
        let rebound = Bindings::new(&config);
        assert!(!rebound.matches(action, contexts::AGENT_PERMISSION_FORM, combined));
        assert_eq!(rebound.hint(action), "F12");
    }
    let mut config = Config::default();
    config.preferences.keyboard.bindings.insert(
        ids::NAVIGATION_NEXT_FOCUS.0.into(),
        key(BindingKey::Enter, 15),
    );
    assert!(
        Bindings::new(&config)
            .conflicts()
            .iter()
            .any(|error| error.contains("Save permission form")
                && error.contains("Ctrl+Alt+Shift+Super+Enter"))
    );
    Ok(())
}

#[test]
fn model_default_rules_contexts_aliases_conflicts_and_old_reset_id_are_truthful() -> Result<()> {
    use contexts::*;
    for (action, contexts, expected_count) in [
        (
            ids::DEFAULTS_SAVE,
            vec![AGENT_DEFAULTS, AGENT_DEFAULTS_BUTTON],
            8,
        ),
        (
            ids::DEFAULTS_REPOSITORY_RULES,
            vec![AGENT_DEFAULTS, AGENT_DEFAULTS_BUTTON],
            8,
        ),
        (
            ids::RULES_SAVE,
            vec![AGENT_RULES, AGENT_RULES_REPOSITORY, AGENT_RULES_BUTTON],
            12,
        ),
    ] {
        let config = Config::default();
        let bindings = Bindings::new(&config);
        for context in contexts {
            assert_eq!(bindings.effective(action, context).len(), expected_count);
            for bits in 0..16 {
                let mut mods = M::NONE;
                for (bit, m) in [(1, M::CONTROL), (2, M::ALT), (4, M::SHIFT), (8, M::SUPER)] {
                    if bits & bit != 0 {
                        mods |= m;
                    }
                }
                let code = if action == ids::DEFAULTS_REPOSITORY_RULES {
                    KeyCode::Char('r')
                } else {
                    KeyCode::Enter
                };
                let event = KeyEvent::new(code, mods);
                let expected = if action == ids::RULES_SAVE {
                    bits & 9 != 0
                } else {
                    bits & 1 != 0
                };
                assert_eq!(
                    bindings.matches(action, context, event),
                    expected,
                    "{} {context:?} {bits}",
                    action.0
                );
                assert!(!bindings.matches(action, AGENT_MODEL_PICKER, event));
                assert!(!bindings.matches(
                    action,
                    context,
                    KeyEvent {
                        kind: KeyEventKind::Release,
                        ..event
                    }
                ));
            }
        }
    }
    let mut config = Config::default();
    config
        .preferences
        .keyboard
        .bindings
        .insert(ids::NAVIGATION_ACTIVATE.0.into(), key(BindingKey::Enter, 5));
    assert!(
        Bindings::new(&config)
            .conflicts()
            .iter()
            .any(|s| s.contains("Save agent defaults")),
        "combined Control alias must participate in conflicts"
    );
    assert!(find("defaults.reset").is_none());
    let dir = tempfile::tempdir()?;
    let storage = crate::storage::Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().join("cache"),
    };
    let mut config = Config::default();
    config
        .preferences
        .keyboard
        .bindings
        .insert("defaults.reset".into(), key(BindingKey::Function(11), 0));
    storage.save_config(&config)?;
    let mut draft = storage.settings_draft()?;
    draft.set(
        crate::settings::SettingId::VoiceEnabled,
        crate::settings::SettingValue::Bool(!config.voice_enabled),
    )?;
    let saved = storage.save_settings_draft(&mut draft)?;
    assert_eq!(
        saved.preferences.keyboard.bindings.get("defaults.reset"),
        Some(&key(BindingKey::Function(11), 0))
    );
    assert_eq!(
        Bindings::new(&saved).resolve(AGENT_DEFAULTS, KeyEvent::new(KeyCode::F(11), M::NONE)),
        None
    );
    Ok(())
}
