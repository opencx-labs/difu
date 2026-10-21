//! Pure Settings preview facade over the production transcript and reveal renderers.
use super::*;

pub(crate) struct SettingsPreview {
    session: Session,
    position: Position,
    replay: Instant,
    playing: bool,
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
        }
    }
    pub fn animating(&self) -> bool {
        self.position.animating()
    }
    /// Accept the complete draft so runtime preference adapters can be shared here.
    pub fn render(
        &mut self,
        config: &Config,
        width: u16,
        replay: Instant,
        animate: bool,
    ) -> Vec<Line<'static>> {
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
