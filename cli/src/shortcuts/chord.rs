use super::*;

/// Canonicalize terminal-equivalent shifted printable and BackTab representations.
/// Ctrl+Enter stays distinct: only enhanced terminals can report it unambiguously.
pub fn normalize(mut chord: KeyChord) -> KeyChord {
    if chord.key == BindingKey::BackTab {
        chord.key = BindingKey::Tab;
        chord.modifiers.shift = true;
    }
    if let BindingKey::Character(c) = chord.key {
        let shifted = "~!@#$%^&*()_+{}|:\"<>?";
        let bases = "`1234567890-=[]\\;',./";
        if c.is_ascii_uppercase() {
            chord.key = BindingKey::Character(c.to_ascii_lowercase());
            chord.modifiers.shift = true;
        } else if let Some(index) = shifted.chars().position(|v| v == c)
            && let Some(base) = bases.chars().nth(index)
        {
            chord.key = BindingKey::Character(base);
            chord.modifiers.shift = true;
        }
    }
    chord
}
pub fn capture(event: KeyEvent) -> Option<KeyChord> {
    if event.modifiers.intersects(M::HYPER | M::META) {
        return None;
    }
    let key = match event.code {
        KeyCode::Char(c) => BindingKey::Character(c),
        KeyCode::Enter => BindingKey::Enter,
        KeyCode::Esc => BindingKey::Escape,
        KeyCode::Tab => BindingKey::Tab,
        KeyCode::BackTab => BindingKey::BackTab,
        KeyCode::Backspace => BindingKey::Backspace,
        KeyCode::Delete => BindingKey::Delete,
        KeyCode::Insert => BindingKey::Insert,
        KeyCode::Up => BindingKey::Up,
        KeyCode::Down => BindingKey::Down,
        KeyCode::Left => BindingKey::Left,
        KeyCode::Right => BindingKey::Right,
        KeyCode::Home => BindingKey::Home,
        KeyCode::End => BindingKey::End,
        KeyCode::PageUp => BindingKey::PageUp,
        KeyCode::PageDown => BindingKey::PageDown,
        KeyCode::F(n) => BindingKey::Function(n),
        _ => return None,
    };
    Some(normalize(KeyChord {
        key,
        modifiers: KeyModifiers {
            control: event.modifiers.contains(M::CONTROL),
            alt: event.modifiers.contains(M::ALT),
            shift: event.modifiers.contains(M::SHIFT),
            super_key: event.modifiers.contains(M::SUPER),
        },
    }))
}
pub fn label(chord: &KeyChord) -> String {
    let chord = normalize(chord.clone());
    let mut parts = Vec::new();
    for (on, name) in [
        (chord.modifiers.control, "Ctrl"),
        (chord.modifiers.alt, "Alt"),
        (chord.modifiers.shift, "Shift"),
        (chord.modifiers.super_key, "Super"),
    ] {
        if on {
            parts.push(name.to_owned());
        }
    }
    parts.push(match chord.key {
        BindingKey::Character(' ') => "Space".into(),
        BindingKey::Character(c) => {
            if chord.modifiers.control || chord.modifiers.alt || chord.modifiers.super_key {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        BindingKey::Function(n) => format!("F{n}"),
        BindingKey::Escape => "Esc".into(),
        other => format!("{other:?}"),
    });
    parts.join("+")
}
