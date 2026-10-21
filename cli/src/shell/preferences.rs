//! Committed refresh is independent of the panel's baseline and draft.
use super::*;
use std::time::{Duration, Instant};
impl Shell {
    pub(super) fn screen_motion(&self) -> bool {
        let config = self
            .settings
            .as_ref()
            .map(|panel| panel.preview_config())
            .unwrap_or(&self.reviews.config);
        config.preferences.motion.screen() && config.tweaks.agent_reply_cps > 0
    }
    pub(super) fn apply_committed_settings(&mut self, config: Config) {
        self.settings_fingerprint = serde_json::to_string(&config).ok();
        self.agents.apply_settings(&config);
        self.reviews.config = config;
        if let Some(panel) = &mut self.settings {
            panel.refresh_preview(&self.reviews.config);
        }
        self.reviews.invalidate();
        if !self.screen_motion() {
            self.switched = None;
        }
        crate::repaint::request();
    }
    /// Poll only once per second; ordinary event/result ticks continue independently.
    pub(super) fn refresh_settings(&mut self) {
        if self
            .settings_refreshed
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.settings_refreshed = Some(Instant::now());
        match self.reviews.storage.refresh_settings() {
            Ok(config) => {
                self.settings_refresh_error = None;
                let fingerprint = serde_json::to_string(&config).ok();
                if fingerprint != self.settings_fingerprint {
                    self.apply_committed_settings(config);
                }
            }
            Err(error) => {
                let message = format!("Could not refresh Settings: {error:#}");
                if self.settings_refresh_error.as_ref() != Some(&message) {
                    self.settings_refresh_error = Some(message.clone());
                    self.reviews.notice = crate::app::Notice::error(message.clone());
                    self.agents.notice = Some((message, true));
                    crate::repaint::request();
                }
            }
        }
    }
}
