//! Shell Settings adapters; the preview facade owns no live UI or services.
use super::*;

impl Ui {
    /// Apply committed preference fields without replacing session, composer or viewport state.
    pub(crate) fn apply_settings(&mut self, config: &Config) {
        use crate::shortcuts::{Bindings, contexts, ids};
        if !config.voice_enabled
            || Bindings::new(&self.shortcut_config)
                .effective(ids::VOICE_HOLD, contexts::AGENT_COMPOSER)
                != Bindings::new(config).effective(ids::VOICE_HOLD, contexts::AGENT_COMPOSER)
        {
            self.cancel_voice();
        }
        self.shortcut_config = config.clone();
        self.preferences = config.preferences.clone();
        self.pinned_sessions = config.pinned_sessions.clone();
        self.session_order = config.session_order.clone();
        self.change_wrap = config.wrap_diff;
        self.change_unified = config.unified;
        self.tweaks = config.tweaks;
        self.settle_disabled_motion();
        self.defaults = config.agent_defaults.clone();
        self.list_visible = config.agent_list_visible;
        self.changes_visible = config.agent_changes_visible;
        self.panels.right = config.preferences.layout.helper(config.agent_panel_right)
            != crate::settings::HelperPlacement::Main;
        self.theme = crate::theme::index(config.theme.as_deref());
        self.voice.enabled = config.voice_enabled;
        crate::theme::select(self.theme);
        crate::theme::set_contrast(config.preferences.appearance.contrast);
        if let Some(panels::View::PullRequest { app }) = &mut self.panels.view {
            app.config = config.clone();
            app.invalidate();
        }
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

#[cfg(test)]
#[path = "settings_tests.rs"]
mod runtime_tests;

impl Ui {
    fn settle_disabled_motion(&mut self) {
        let motion = &self.preferences.motion;
        if !motion.sidebar() || self.tweaks.agent_reply_cps == 0 {
            self.sidebar_boards.clear();
            self.sidebar_highlights.clear();
        }
        if motion.level != crate::settings::Motion::Full || self.tweaks.agent_reply_cps == 0 {
            self.question_flip.settle_opening();
        }
        if !motion.sent() || self.tweaks.input_clear_ms == 0 || self.tweaks.agent_reply_cps == 0 {
            self.question_flip.settle_leaving();
        }
        for position in self.positions.values_mut() {
            if !motion.reply() || self.tweaks.agent_reply_cps == 0 {
                position.reply = None;
                position.boards.clear();
                position.voice_flaps.clear();
            }
            if motion.level != crate::settings::Motion::Full {
                position.activity = Default::default();
                position.waiting = Default::default();
            }
            if !motion.sent() || self.tweaks.sent_message_ms == 0 {
                position.sent = None;
                position.waiting = Default::default();
            }
            if !motion.sent() || self.tweaks.input_clear_ms == 0 {
                position.clearing = None;
            }
        }
    }
}

impl Ui {
    /// Local visual preview only: never change voice, provider defaults or service preferences.
    pub(crate) fn preview_visual_settings(&mut self, config: &Config) {
        let changed =
            self.preferences.motion != config.preferences.motion || self.tweaks != config.tweaks;
        self.preferences.code = config.preferences.code.clone();
        self.change_wrap = config.wrap_diff;
        self.change_unified = config.unified;
        self.preferences.appearance = config.preferences.appearance.clone();
        // Follow policy changes behavior as content arrives; only the synthetic
        // sample may preview it. Keep the live reading anchor until Save.
        let committed_scroll = self.preferences.conversation.auto_scroll;
        self.preferences.conversation = config.preferences.conversation.clone();
        self.preferences.conversation.auto_scroll = committed_scroll;
        self.preferences.layout = config.preferences.layout.clone();
        self.preferences.input.composer_max_height = config.preferences.input.composer_max_height;
        self.preferences.input.paste_threshold = config.preferences.input.paste_threshold;
        self.list_visible = config.agent_list_visible;
        self.changes_visible = config.agent_changes_visible;
        self.panels.right = config.preferences.layout.helper(config.agent_panel_right)
            != crate::settings::HelperPlacement::Main;
        self.preferences.motion = config.preferences.motion.clone();
        self.tweaks = config.tweaks;
        if changed {
            self.settle_disabled_motion();
        }
        crate::theme::select(crate::theme::index(config.theme.as_deref()));
        crate::theme::set_contrast(config.preferences.appearance.contrast);
    }
}
