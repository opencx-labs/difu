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
#[cfg(test)]
pub(crate) fn is_open_shortcut(config: &Config, key: KeyEvent) -> bool {
    crate::shortcuts::Bindings::new(config).matches(
        crate::shortcuts::ids::SETTINGS_OPEN,
        crate::shortcuts::contexts::AGENT_COMPOSER,
        key,
    )
}
impl crate::shell::Shell {
    /// Later keyboard and notification integrations handle explicit requests here.
    pub(crate) fn settings_action(&mut self, id: SettingId) {
        if self
            .settings
            .as_mut()
            .is_some_and(|panel| panel.shortcut_action(&id))
        {
            return;
        }
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
            action => {
                self.settings_pending_action = Some(action);
                if let Some(panel) = &mut self.settings {
                    panel.notice = Some("Requested explicit settings action".into());
                }
            }
        }
    }
}
