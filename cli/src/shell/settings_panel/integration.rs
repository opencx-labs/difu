use super::*;

pub(crate) fn command_category(name: &str) -> Option<Category> {
    match name {
        "/settings" => Some(Category::Appearance),
        "/theme" => Some(Category::Appearance),
        "/tweaks" => Some(Category::Motion),
        "/voice" => Some(Category::Input),
        _ => None,
    }
}
pub(crate) fn is_open_shortcut(config: &Config, key: KeyEvent) -> bool {
    use crate::settings::{BindingKey as K, KeyChord, KeyModifiers as M};
    let chord = config
        .preferences
        .keyboard
        .bindings
        .get("settings.open")
        .cloned()
        .unwrap_or(KeyChord {
            key: K::Character(','),
            modifiers: M {
                control: true,
                ..Default::default()
            },
        });
    let modifiers = M {
        control: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
        super_key: key.modifiers.contains(KeyModifiers::SUPER),
    };
    let code = match key.code {
        KeyCode::Char(ch) => K::Character(ch),
        KeyCode::F(n) => K::Function(n),
        KeyCode::Enter => K::Enter,
        KeyCode::Tab => K::Tab,
        KeyCode::BackTab => K::BackTab,
        KeyCode::Backspace => K::Backspace,
        KeyCode::Delete => K::Delete,
        KeyCode::Insert => K::Insert,
        KeyCode::Up => K::Up,
        KeyCode::Down => K::Down,
        KeyCode::Left => K::Left,
        KeyCode::Right => K::Right,
        KeyCode::Home => K::Home,
        KeyCode::End => K::End,
        KeyCode::PageUp => K::PageUp,
        KeyCode::PageDown => K::PageDown,
        KeyCode::Esc => K::Escape,
        _ => return false,
    };
    chord.key == code && chord.modifiers == modifiers
}
impl crate::shell::Shell {
    /// Later keyboard and notification integrations handle explicit requests here.
    pub(crate) fn settings_action(&mut self, id: SettingId) {
        match id {
            SettingId::Rules | SettingId::McpConnections | SettingId::SlackConnection => {
                self.agents.open_settings_action(&id);
                self.settings_action_open = self.agents.modal.is_some();
                if !self.settings_action_open
                    && let Some(panel) = &mut self.settings
                {
                    panel.error = Some("Select an agent session for this action".into());
                }
            }
            SettingId::ShortcutReset => {
                if let Some(panel) = &mut self.settings {
                    let ids = panel
                        .draft
                        .config()
                        .preferences
                        .keyboard
                        .bindings
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>();
                    for id in ids {
                        panel.apply(SettingId::Shortcut(id), SettingValue::Shortcut(None));
                    }
                    panel.notice = Some("Shortcut overrides reset in draft".into());
                }
            }
            action => {
                self.settings_pending_action = Some(action);
                if let Some(panel) = &mut self.settings {
                    panel.notice = Some("Requested explicit settings action".into());
                }
            }
        }
    }
}
