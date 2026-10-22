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
