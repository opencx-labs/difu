//! Pure Settings preview facade over the production transcript and reveal renderers.
use super::*;

pub(crate) struct SettingsPreview {
    session: Session,
    position: Position,
    replay: Instant,
    playing: bool,
    control_animating: bool,
}
impl SettingsPreview {
    pub fn new(replay: Instant) -> Self {
        let mut session = Session::new(
            "settings-synthetic".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: std::path::PathBuf::from("/synthetic/settings-preview"),
                isolated: false,
                base: "main".into(),
                prompt: "Settings preview".into(),
                model: None,
                effort: None,
            }),
        );
        session.entries = vec![
            Entry { id: "sample-user".into(), kind: "userMessage".into(), text: "Please make the greeting clearer.".to_owned().into(), ..Default::default() },
            Entry { id: "sample-tool".into(), kind: "commandExecution".into(), text: "Hello, Difu!\nAll checks passed.\nNo files outside the example changed.\nReady to review.\nExtra tool output.".to_owned().into(), data: serde_json::json!({"command":"cargo test greeting","exitCode":0}).into(), finished_at: Some(1), ..Default::default() },
            Entry { id: "sample-reply".into(), kind: "agentMessage".into(), text: "The greeting now says **Hello, Difu!**\n\nThe focused check passed. You can review the change before saving.".to_owned().into(), finished_at: Some(1), ..Default::default() },
        ];
        Self {
            session,
            position: Position::default(),
            replay,
            playing: false,
            control_animating: false,
        }
    }
    /// Replay a control-specific scene with the same effect renderer as the live UI.
    pub fn render_control(
        &mut self,
        config: &Config,
        width: u16,
        replay: Instant,
        id: &crate::settings::SettingId,
    ) -> Vec<Line<'static>> {
        self.render_control_at(config, width, replay, id, Instant::now())
    }
    fn render_control_at(
        &mut self,
        config: &Config,
        width: u16,
        replay: Instant,
        id: &crate::settings::SettingId,
        now: Instant,
    ) -> Vec<Line<'static>> {
        use crate::settings::SettingId;
        use ratatui::{buffer::Buffer, widgets::Widget};
        if !matches!(
            id,
            SettingId::SidebarEffect
                | SettingId::ScreenTransitionEffect
                | SettingId::InputClearDuration
        ) {
            return self.render(config, width, replay, true);
        }
        // These scenes own only synthetic rows and effect state. They never touch
        // a live Ui, composer, service request or microphone.
        self.position = Position::default();
        self.playing = false;
        self.control_animating = false;
        let motion = &config.preferences.motion;
        let width = width.max(1);
        match id {
            SettingId::SidebarEffect => {
                let mut rows = vec![
                    Line::from("Example session"),
                    Line::from("Working on greeting.rs"),
                ];
                let mut board = flap::Board::default();
                if motion.sidebar() && config.tweaks.agent_reply_cps > 0 {
                    // Seed at replay time, then render the current frame through
                    // the same Board::show path used for live sidebar cards.
                    board.show(&mut rows.clone(), replay, ACCENT, width);
                    board.show(&mut rows, now, ACCENT, width);
                    self.control_animating = board.animating(now);
                } else {
                    board.settle(&rows, now);
                }
                rows.insert(0, Line::from("Session sidebar"));
                rows
            }
            SettingId::ScreenTransitionEffect => {
                let area = Rect::new(0, 0, width, 3);
                let mut buffer = Buffer::empty(area);
                Paragraph::new(vec![
                    Line::from("Changes"),
                    Line::from("greeting.rs"),
                    Line::from("+ Hello, Difu!"),
                ])
                .render(area, &mut buffer);
                if motion.screen() && config.tweaks.agent_reply_cps > 0 {
                    let duration =
                        Duration::from_millis(u64::from(config.tweaks.capped(flap::SCREEN_MS)));
                    self.control_animating = flap::screen(&mut buffer, replay, duration, now);
                }
                let mut rows = vec![Line::from("Switch to Changes")];
                rows.extend(buffer.content.chunks(usize::from(width)).map(|cells| {
                    Line::from(
                        cells
                            .iter()
                            .map(|cell| Span::styled(cell.symbol().to_owned(), cell.style()))
                            .collect::<Vec<_>>(),
                    )
                }));
                rows
            }
            SettingId::InputClearDuration => {
                let tweaks = motion.tweaks(config.tweaks);
                let mut position = Position {
                    clearing: reveal::Typing::start(
                        "Please make the greeting clearer.",
                        tweaks.capped(tweaks.input_clear_ms),
                        replay,
                    ),
                    ..Default::default()
                };
                let mut rows = vec![Line::from("Composer after send")];
                if let Some(clearing) = reveal::clearing(&mut position, now) {
                    let mut input = vec![Line::from(clearing.text().to_owned())];
                    clearing.vanish(&mut input, now);
                    rows.extend(input);
                    self.control_animating = true;
                }
                rows
            }
            _ => self.render(config, width, replay, true),
        }
    }
    pub fn animating(&self) -> bool {
        self.control_animating || self.position.animating()
    }
    /// Accept the complete draft so runtime preference adapters can be shared here.
    pub fn render(
        &mut self,
        config: &Config,
        width: u16,
        replay: Instant,
        animate: bool,
    ) -> Vec<Line<'static>> {
        self.control_animating = false;
        if self.replay != replay || !animate || !self.playing {
            self.position = Position::default();
            self.replay = replay;
            if animate {
                self.position.replies_seen = true;
                self.position.known = Some(HashSet::new());
                if let Some(entry) = self.session.entries.first() {
                    let tweaks = config.preferences.motion.tweaks(config.tweaks);
                    self.position.sent = reveal::Typing::start(
                        &entry.text,
                        tweaks.capped(tweaks.sent_message_ms),
                        replay,
                    );
                }
            }
        }
        self.playing = animate;
        if config
            .preferences
            .motion
            .tweaks(config.tweaks)
            .sent_message_ms
            == 0
        {
            self.position.sent = None;
        }
        self.position.expanded.clear();
        if config.preferences.conversation.tool_display == crate::settings::ToolDisplay::Expanded {
            self.position.expanded.insert("sample-tool".into());
        }
        let (mut lines, sections, _) = transcript::render_entries_with_appearance(
            &self.session,
            &self.session.entries,
            &self.position,
            width.max(1),
            false,
            &config.preferences.appearance,
        );
        if animate {
            let tweaks = config.preferences.motion.tweaks(config.tweaks);
            let now = Instant::now();
            let total = lines.len();
            let turn = flap::chat(
                &mut self.position,
                &self.session,
                &sections,
                0,
                total,
                &mut lines,
                tweaks,
                width.max(1),
                now,
            );
            reveal::apply(
                &mut self.position,
                &self.session,
                &sections,
                0,
                total,
                &mut lines,
                tweaks,
                width.max(1),
                now,
                turn,
            );
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Motion, SettingId};
    fn text(rows: &[Line]) -> String {
        rows.iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn dedicated_control_previews_replay_and_settle_their_own_effects() {
        let start = Instant::now();
        for (id, label, complete) in [
            (
                SettingId::SidebarEffect,
                "Session sidebar",
                "Example session",
            ),
            (
                SettingId::ScreenTransitionEffect,
                "Switch to Changes",
                "greeting.rs",
            ),
            (SettingId::InputClearDuration, "Composer after send", ""),
        ] {
            let config = Config::default();
            let mut preview = SettingsPreview::new(start);
            let playing = preview.render_control_at(&config, 60, start, &id, start);
            assert!(
                text(&playing).contains(label),
                "{id:?} should have its own sample"
            );
            assert!(preview.animating(), "{id:?} should schedule replay");
            let settled =
                preview.render_control_at(&config, 60, start, &id, start + Duration::from_secs(2));
            assert!(
                !preview.animating(),
                "{id:?} must stop repainting at completion"
            );
            assert_ne!(playing, settled, "{id:?} must visibly progress");
            if id == SettingId::InputClearDuration {
                assert!(text(&playing).contains("Please make"));
                assert!(!text(&settled).contains("Please make"));
            } else {
                assert!(text(&settled).contains(complete));
            }
            let replay = start + Duration::from_secs(3);
            let again = preview.render_control_at(&config, 60, replay, &id, replay);
            assert_eq!(
                again, playing,
                "{id:?} replay must restart the same synthetic scene"
            );
        }
    }
    #[test]
    fn dedicated_previews_honor_disabled_modes_zero_and_independent_toggles() {
        let start = Instant::now();
        for id in [
            SettingId::SidebarEffect,
            SettingId::ScreenTransitionEffect,
            SettingId::InputClearDuration,
        ] {
            for disabled in 0..4 {
                let mut config = Config::default();
                match disabled {
                    0 => config.preferences.motion.level = Motion::Off,
                    1 => config.preferences.motion.level = Motion::Reduced,
                    2 => match id {
                        SettingId::SidebarEffect => config.preferences.motion.sidebar = false,
                        SettingId::ScreenTransitionEffect => {
                            config.preferences.motion.screen_transition = false
                        }
                        _ => config.preferences.motion.sent_message = false,
                    },
                    _ => match id {
                        SettingId::InputClearDuration => config.tweaks.input_clear_ms = 0,
                        _ => config.tweaks.agent_reply_cps = 0,
                    },
                }
                let mut preview = SettingsPreview::new(start);
                let first = preview.render_control_at(&config, 60, start, &id, start);
                assert!(!preview.animating(), "{id:?} disabled by {disabled}");
                let later = preview.render_control_at(
                    &config,
                    60,
                    start,
                    &id,
                    start + Duration::from_secs(2),
                );
                assert_eq!(first, later);
                match id {
                    SettingId::SidebarEffect => assert!(text(&first).contains("Example session")),
                    SettingId::ScreenTransitionEffect => {
                        assert!(text(&first).contains("greeting.rs"))
                    }
                    _ => assert!(!text(&first).contains("Please make")),
                }
            }
            let mut config = Config::default();
            config.preferences.motion.reply_tool = false;
            config.tweaks.sent_message_ms = 0;
            let mut preview = SettingsPreview::new(start);
            preview.render_control_at(&config, 60, start, &id, start);
            assert!(
                preview.animating(),
                "{id:?} is independent of reply/sent duration"
            );
        }
    }
    #[test]
    fn clearing_and_screen_previews_obey_duration_caps_without_mutating_the_transcript() {
        let start = Instant::now();
        let mut config = Config::default();
        let mut preview = SettingsPreview::new(start);
        let original = preview.render(&config, 60, start, false);
        for (duration, cap, active) in [(100, 0, false), (1000, 0, true), (1000, 100, false)] {
            config.tweaks.input_clear_ms = duration;
            config.tweaks.max_animation_ms = cap;
            let rows = preview.render_control_at(
                &config,
                60,
                start,
                &SettingId::InputClearDuration,
                start + Duration::from_millis(500),
            );
            assert_eq!(preview.animating(), active);
            assert_eq!(text(&rows).contains("Please make"), active);
        }
        for (cap, active) in [(0, true), (100, false)] {
            config.tweaks.max_animation_ms = cap;
            let rows = preview.render_control_at(
                &config,
                60,
                start,
                &SettingId::ScreenTransitionEffect,
                start + Duration::from_millis(500),
            );
            assert_eq!(preview.animating(), active, "screen cap {cap}");
            if !active {
                assert!(text(&rows).contains("greeting.rs"));
            }
        }
        assert_eq!(preview.render(&config, 60, start, false), original);
    }
}
