use super::*;

pub(crate) fn command_category(name: &str) -> Option<Category> {
    match name {
        "/settings" => Some(Category::Appearance),
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
    /// Explicit actions apply separately while the Settings draft remains open.
    pub(crate) fn settings_action(&mut self, id: SettingId) {
        if self
            .settings
            .as_mut()
            .is_some_and(|panel| panel.shortcut_action(&id))
        {
            return;
        }
        match id {
            SettingId::ProviderConnections => {
                if let Some(panel) = &mut self.settings {
                    panel.refresh_connections();
                    panel.retry_capabilities();
                    panel.connection_tick();
                }
            }
            SettingId::CodexSignIn | SettingId::ClaudeSignIn => {
                self.settings_pending_action = Some(id);
            }
            SettingId::Rules
            | SettingId::McpConnections
            | SettingId::SlackConnection
            | SettingId::RemoteConnection => {
                if id == SettingId::Rules {
                    let path = self
                        .settings
                        .as_ref()
                        .and_then(|panel| panel.draft.config().agent_defaults.repository.clone())
                        .or_else(|| std::env::current_dir().ok());
                    self.agents.open_settings_rules(path);
                } else {
                    self.agents.open_settings_action(&id);
                }
                self.settings_action_open = self.agents.modal.is_some();
                if !self.settings_action_open
                    && let Some(panel) = &mut self.settings
                {
                    panel.error = Some(
                        self.agents
                            .notice
                            .as_ref()
                            .map(|(message, _)| message.clone())
                            .unwrap_or_else(|| "Select an agent session for this action".into()),
                    );
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

impl crate::shell::Shell {
    /// Only an explicit sign-in action hands terminal ownership to main.
    pub fn take_sign_in(&mut self) -> Option<crate::agents::provider::Provider> {
        let provider = match self.settings_pending_action.as_ref()? {
            SettingId::CodexSignIn => crate::agents::provider::Provider::Codex,
            SettingId::ClaudeSignIn => crate::agents::provider::Provider::Claude,
            _ => return None,
        };
        self.settings_pending_action = None;
        Some(provider)
    }
    pub fn finish_sign_in(&mut self, success: bool, cancelled: bool) {
        if let Some(panel) = &mut self.settings {
            panel.notice = Some(
                if cancelled {
                    "Sign-in cancelled; draft retained"
                } else if success {
                    "Native sign-in finished; refreshing status"
                } else {
                    "Sign-in did not complete; check CLI installation and try again"
                }
                .into(),
            );
            panel.refresh_connections();
            panel.retry_capabilities();
            panel.connection_tick();
        }
    }
}
