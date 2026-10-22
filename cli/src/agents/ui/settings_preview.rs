//! Pure Settings preview facade over the production transcript and reveal renderers.
use super::*;

pub(crate) struct SettingsPreview {
    session: Session,
    position: Position,
    replay: Instant,
    playing: bool,
    control_animating: bool,
    frame_time: Instant,
    control: Option<crate::settings::SettingId>,
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
            Entry { id: "sample-reasoning".into(), kind:"reasoning".into(), text:"**Inspect paths**\nUnique reasoning detail".into(), ..Default::default() },
            Entry { id: "sample-reply".into(), kind: "agentMessage".into(), text: "The greeting now says **Hello, Difu!**\n\nThe focused check passed. You can review the change before saving.".to_owned().into(), finished_at: Some(1), ..Default::default() },
        ];
        Self {
            session,
            position: Position::default(),
            replay,
            playing: false,
            control_animating: false,
            frame_time: replay,
            control: None,
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
        use crate::settings::{Category, SettingId};
        self.frame_time = now;
        if self.control.as_ref() != Some(id) {
            self.position = Position::default();
            self.playing = false;
            self.control = Some(id.clone());
        }
        if *id == SettingId::AutoScroll {
            return self.scroll_scene(config, width);
        }
        if crate::settings::spec(id)
            .is_some_and(|spec| matches!(spec.category, Category::Layout | Category::Input))
        {
            return self.native_scene(config, width, id);
        }
        use ratatui::{buffer::Buffer, widgets::Widget};
        if !matches!(
            id,
            SettingId::SidebarEffect
                | SettingId::ScreenTransitionEffect
                | SettingId::InputClearDuration
        ) {
            let animate = crate::settings::spec(id)
                .is_some_and(|spec| spec.preview == crate::settings::Preview::Animation);
            let mut session = self.session.clone();
            match id {
                SettingId::ReplyToolEffect => session.entries.retain(|entry| {
                    matches!(entry.kind.as_str(), "commandExecution" | "agentMessage")
                }),
                SettingId::ReplySpeed | SettingId::MaxAnimationDelay => {
                    session.entries.retain(|entry| entry.kind == "agentMessage")
                }
                SettingId::SentMessageEffect | SettingId::SentDuration => {
                    session.entries.retain(|entry| entry.kind == "userMessage")
                }
                _ => {}
            }
            let mut scene = config.clone();
            if matches!(id, SettingId::SentMessageEffect | SettingId::SentDuration) {
                // The sent-message example must not fall back to the generic
                // chat-entry effect when its own effect is disabled.
                scene.preferences.motion.reply_tool = false;
            }
            return self.render_session_at(&session, &scene, width, replay, animate, now);
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
    fn scroll_scene(&self, config: &Config, width: u16) -> Vec<Line<'static>> {
        let mut session = self.session.clone();
        session.entries.clear();
        for n in 0..12 {
            session.note("agentMessage", format!("Reading message {n}"));
        }
        let mut window = transcript_window::Window::default();
        window.appearance = config.preferences.appearance.clone();
        window.conversation = config.preferences.conversation.clone();
        let mut position = Position::default();
        // The reader is above the bottom when a new response arrives.
        window.render(&session, &mut position, width.max(1), 5, false);
        session.note("agentMessage", "Newest streaming reply");
        let view = window.render(&session, &mut position, width.max(1), 5, false);
        view.lines
            .into_iter()
            .skip(position.conversation.saturating_sub(view.start))
            .take(5)
            .collect()
    }
    fn native_scene(
        &self,
        config: &Config,
        width: u16,
        id: &crate::settings::SettingId,
    ) -> Vec<Line<'static>> {
        use crate::settings::{Category, SettingId};
        use ratatui::{Terminal, backend::TestBackend};
        // This Ui owns synthetic sessions and an unconnected internal channel.
        // Draw is the only entry point: no tick, key, service, browser or microphone action.
        let mut visual = config.clone();
        visual.preferences.motion.level = crate::settings::Motion::Off;
        visual.voice_enabled = false;
        let storage = Storage {
            config: "/synthetic/difu-settings/config.json".into(),
            cache: "/synthetic/difu-settings/cache".into(),
        };
        let mut ui = Ui::new(storage, &visual);
        let mut session = self.session.clone();
        session.title = "Example session".into();
        session.updated = 100;
        session.status = Status::Idle;
        ui.selected = Some(session.id.clone());
        ui.summaries.push(session.summary());
        ui.session_order = vec![
            session.id.clone(),
            "synthetic-active".into(),
            "synthetic-recent".into(),
        ];
        for (id, title, status, updated) in [
            ("synthetic-active", "Active session", Status::Running, 200),
            ("synthetic-recent", "Recent session", Status::Idle, 300),
        ] {
            let mut summary = session.summary();
            summary.id = id.into();
            summary.title = title.into();
            summary.status = status;
            summary.updated = updated;
            ui.summaries.push(summary);
        }
        ui.sessions.insert(session.id.clone(), session);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let position = ui.positions.entry("settings-synthetic".into()).or_default();
        position.media_loaded = true;
        if *id == SettingId::PasteThreshold {
            position.draft.paste_with_threshold(
                &"pasted example ".repeat(45),
                config.preferences.input.paste_threshold,
            );
        } else if crate::settings::spec(id).is_some_and(|spec| spec.category == Category::Input) {
            position.draft = Editor::from(
                (1..=22)
                    .map(|n| format!("composer row {n}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        } else {
            position.draft = Editor::from("A synthetic composer draft");
            ui.panels.view = Some(panels::View::Shell {
                id: "sample-tool".into(),
                command: "cargo test greeting".into(),
            });
        }
        let mut terminal = match Terminal::new(TestBackend::new(width.max(40), 30)) {
            Ok(terminal) => terminal,
            Err(error) => match error {},
        };
        if terminal.draw(|frame| ui.draw(frame)).is_err() {
            return Vec::new();
        }
        let mut rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width.max(40)))
            .map(|cells| {
                Line::from(
                    cells
                        .iter()
                        .map(|cell| Span::styled(cell.symbol().to_owned(), cell.style()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        if *id == SettingId::StartupTab {
            rows.insert(
                0,
                Line::from(format!(
                    "Launch opens {}",
                    if crate::shell::Shell::reviews_on_launch(config, false) {
                        "Reviews"
                    } else {
                        "Agents"
                    }
                )),
            );
        }
        if *id == SettingId::RunningInput {
            rows.insert(
                0,
                Line::from(format!(
                    "Primary send while running: {}",
                    config.preferences.input.running.key()
                )),
            );
        }
        rows
    }
    pub fn animating(&self) -> bool {
        self.control_animating || self.position.animating_at(self.frame_time)
    }
    /// Accept the complete draft so runtime preference adapters can be shared here.
    pub fn render(
        &mut self,
        config: &Config,
        width: u16,
        replay: Instant,
        animate: bool,
    ) -> Vec<Line<'static>> {
        self.render_session_at(
            &self.session.clone(),
            config,
            width,
            replay,
            animate,
            Instant::now(),
        )
    }
    fn render_session_at(
        &mut self,
        session: &Session,
        config: &Config,
        width: u16,
        replay: Instant,
        animate: bool,
        now: Instant,
    ) -> Vec<Line<'static>> {
        self.frame_time = now;
        self.control_animating = false;
        let restart = self.replay != replay || !animate || !self.playing;
        if restart {
            self.position = Position::default();
            self.replay = replay;
            if animate {
                self.position.replies_seen = true;
                self.position.known = Some(HashSet::new());
                if let Some(entry) = session
                    .entries
                    .iter()
                    .find(|entry| entry.kind == "userMessage")
                {
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
        // Seed at the replay instant so every control uses the same clock, even
        // when its first visible frame arrives after the replay request.
        if restart && animate && now > replay {
            self.transcript_frame(session, config, width, animate, replay);
        }
        self.transcript_frame(session, config, width, animate, now)
    }
    fn transcript_frame(
        &mut self,
        session: &Session,
        config: &Config,
        width: u16,
        animate: bool,
        now: Instant,
    ) -> Vec<Line<'static>> {
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
        let (mut lines, sections, _) = transcript::render_entries_with_preferences(
            session,
            &session.entries,
            &self.position,
            width.max(1),
            false,
            &config.preferences.appearance,
            &config.preferences.conversation,
        );
        if animate {
            let tweaks = config.preferences.motion.tweaks(config.tweaks);
            let total = lines.len();
            let turn = flap::chat(
                &mut self.position,
                session,
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
                session,
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

/// Explain effective motion dependencies without changing the draft or its timings.
pub(crate) fn motion_disabled(
    config: &Config,
    id: &crate::settings::SettingId,
) -> Option<&'static str> {
    use crate::settings::{Motion, SettingId};
    let motion = &config.preferences.motion;
    if motion.level != Motion::Full {
        return Some("Motion is disabled by Animations. Choose Full to preview this effect.");
    }
    match id {
        SettingId::ReplyToolEffect | SettingId::ReplySpeed | SettingId::MaxAnimationDelay => {
            if !motion.reply_tool {
                Some("Reply and tool effects are Off. Turn them On to preview this effect.")
            } else if config.tweaks.agent_reply_cps == 0 {
                Some("Reply speed is 0. Set it above 0 to preview this effect.")
            } else {
                None
            }
        }
        SettingId::SentMessageEffect | SettingId::SentDuration | SettingId::InputClearDuration => {
            if !motion.sent_message {
                Some("Sent-message effect is Off. Turn it On to preview this effect.")
            } else if *id == SettingId::InputClearDuration && config.tweaks.input_clear_ms == 0 {
                Some("Input clearing duration is 0 ms. Set it above 0 to preview this effect.")
            } else if *id != SettingId::InputClearDuration && config.tweaks.sent_message_ms == 0 {
                Some("Sent-message duration is 0 ms. Set it above 0 to preview this effect.")
            } else {
                None
            }
        }
        SettingId::SidebarEffect | SettingId::ScreenTransitionEffect => {
            if *id == SettingId::SidebarEffect && !motion.sidebar {
                Some("Sidebar effects are Off. Turn them On to preview this effect.")
            } else if *id == SettingId::ScreenTransitionEffect && !motion.screen_transition {
                Some("Screen transitions are Off. Turn them On to preview this effect.")
            } else if config.tweaks.agent_reply_cps == 0 {
                Some("Reply speed is 0. Set it above 0 to preview this effect.")
            } else {
                None
            }
        }
        _ => None,
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
    #[test]
    fn motion_preview_toggles_do_not_replay_an_unrelated_effect() {
        let start = Instant::now();
        for id in [
            SettingId::ReplyToolEffect,
            SettingId::ReplySpeed,
            SettingId::SentMessageEffect,
            SettingId::SentDuration,
        ] {
            let mut config = Config::default();
            match id {
                SettingId::ReplyToolEffect => config.preferences.motion.reply_tool = false,
                SettingId::ReplySpeed => config.tweaks.agent_reply_cps = 0,
                SettingId::SentMessageEffect => config.preferences.motion.sent_message = false,
                _ => config.tweaks.sent_message_ms = 0,
            }
            let mut preview = SettingsPreview::new(start);
            let first = preview.render_control_at(&config, 60, start, &id, start);
            assert!(
                !preview.animating(),
                "{id:?} must not animate another control's effect"
            );
            let last =
                preview.render_control_at(&config, 60, start, &id, start + Duration::from_secs(5));
            assert_eq!(first, last);
        }
    }

    #[test]
    fn motion_preview_reply_speed_and_cap_change_the_visible_progress() {
        let start = Instant::now();
        let mut slow = Config::default();
        slow.tweaks.agent_reply_cps = 10;
        slow.tweaks.max_animation_ms = 0;
        let mut fast = slow.clone();
        fast.tweaks.agent_reply_cps = 1000;
        let mut capped = slow.clone();
        capped.tweaks.max_animation_ms = 100;
        for (id, config) in [
            (SettingId::ReplySpeed, fast),
            (SettingId::MaxAnimationDelay, capped),
        ] {
            let mut slow_preview = SettingsPreview::new(start);
            let mut fast_preview = SettingsPreview::new(start);
            slow_preview.render_control_at(&slow, 60, start, &id, start);
            fast_preview.render_control_at(&config, 60, start, &id, start);
            let slow_rows = slow_preview.render_control_at(
                &slow,
                60,
                start,
                &id,
                start + Duration::from_secs(1),
            );
            let fast_rows = fast_preview.render_control_at(
                &config,
                60,
                start,
                &id,
                start + Duration::from_secs(1),
            );
            assert!(!text(&slow_rows).contains("saving."));
            assert!(
                text(&fast_rows).contains("saving."),
                "{id:?}: faster timing must visibly finish earlier: {}",
                text(&fast_rows)
            );
        }
    }
    #[test]
    fn motion_preview_every_control_progresses_settles_and_replays() {
        let start = Instant::now();
        let config = Config::default();
        for id in [
            SettingId::Motion,
            SettingId::ReplyToolEffect,
            SettingId::SentMessageEffect,
            SettingId::SidebarEffect,
            SettingId::ScreenTransitionEffect,
            SettingId::ReplySpeed,
            SettingId::SentDuration,
            SettingId::InputClearDuration,
            SettingId::MaxAnimationDelay,
        ] {
            let mut preview = SettingsPreview::new(start);
            let first = preview.render_control_at(&config, 60, start, &id, start);
            assert!(
                preview.animating(),
                "{id:?}: first frame must schedule repainting"
            );
            let later = preview.render_control_at(
                &config,
                60,
                start,
                &id,
                start + Duration::from_millis(200),
            );
            assert_ne!(first, later, "{id:?}: pixels must change during playback");
            let settled =
                preview.render_control_at(&config, 60, start, &id, start + Duration::from_secs(5));
            assert!(!preview.animating(), "{id:?}: playback must finish");
            assert_ne!(first, settled);
            let replay = start + Duration::from_secs(6);
            assert_eq!(
                preview.render_control_at(&config, 60, replay, &id, replay),
                first
            );
            assert!(preview.animating(), "{id:?}: replay must restart");
        }
    }

    #[test]
    fn motion_preview_sent_duration_obeys_the_cap_and_zero() {
        let start = Instant::now();
        for (duration, cap, active) in [
            (0, 0, false),
            (100, 0, false),
            (1000, 0, true),
            (1000, 100, false),
        ] {
            let mut config = Config::default();
            config.tweaks.sent_message_ms = duration;
            config.tweaks.max_animation_ms = cap;
            let mut preview = SettingsPreview::new(start);
            preview.render_control_at(&config, 60, start, &SettingId::SentDuration, start);
            preview.render_control_at(
                &config,
                60,
                start,
                &SettingId::SentDuration,
                start + Duration::from_millis(500),
            );
            assert_eq!(
                preview.animating(),
                active,
                "duration {duration}, cap {cap}"
            );
        }
    }
}
