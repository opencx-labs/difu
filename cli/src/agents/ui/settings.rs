//! Shell Settings adapters; the preview facade owns no live UI or services.
use super::*;

impl Ui {
    /// Apply committed preference fields without replacing session, composer or viewport state.
    pub(crate) fn apply_settings(&mut self, config: &Config) {
        self.defaults = config.agent_defaults.clone();
        self.list_visible = config.agent_list_visible;
        self.changes_visible = config.agent_changes_visible;
        self.panels.right = config.agent_panel_right;
        self.tweaks = config.tweaks;
        self.theme = crate::theme::index(config.theme.as_deref());
        self.voice.enabled = config.voice_enabled;
        crate::theme::select(self.theme);
    }
}
impl Ui {
    pub(crate) fn open_settings_action(&mut self, id: &crate::settings::SettingId) {
        use crate::settings::SettingId;
        match id {
            SettingId::Rules => self.open_rules(None),
            SettingId::McpConnections => self.open_mcp(),
            SettingId::SlackConnection => self.open_slack(),
            _ => {}
        }
    }
}

impl Ui {
    pub(crate) fn draw_settings_action(&mut self, frame: &mut Frame) {
        self.draw_modal(frame);
    }
}
