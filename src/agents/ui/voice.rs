use super::*;
use crate::voice::{Event, Recording};
use crossterm::event::KeyEventKind;

pub(super) struct Hold {
    session: String,
    before: Editor,
    started: Instant,
    last: Instant,
    recording: Option<Recording>,
    finishing: bool,
    listening: bool,
    partial: String,
    level: f32,
}
#[derive(Default)]
pub(super) struct State {
    pub enabled: bool,
    pub key: Option<String>,
    hold: Option<Hold>,
    saving: Option<mpsc::Receiver<Result<String, String>>>,
}
impl State {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }
}
impl Ui {
    pub fn voice_enabled(&self) -> bool {
        self.voice.enabled
    }
    pub(super) fn save_voice_key(&mut self) {
        if self.voice.saving.is_some() {
            return;
        }
        let Some(Modal::Voice { key, .. }) = &self.modal else {
            return;
        };
        let key = key.text().trim().to_owned();
        if key.is_empty() {
            self.notice = Some((
                "Enter an API key, or enable voice using OPENAI_API_KEY / an existing Keychain key"
                    .into(),
                true,
            ));
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.voice.saving = Some(rx);
        thread::spawn(move || {
            let result = crate::voice::save_key(&key)
                .map(|()| key)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }

    pub(super) fn open_voice(&mut self) {
        self.cancel_voice();
        self.modal = Some(Modal::Voice {
            key: Editor::default(),
            field: 0,
        });
    }
    pub fn cancel_voice(&mut self) {
        if let Some(hold) = self.voice.hold.take()
            && hold.recording.is_some()
            && let Some(position) = self.positions.get_mut(&hold.session)
        {
            position.draft = hold.before;
        }
    }
    pub(super) fn voice_key(&mut self, key: KeyEvent) -> bool {
        let space = key.code == KeyCode::Char(' ') && key.modifiers.is_empty();
        if key.kind == KeyEventKind::Release {
            if space && let Some(hold) = &mut self.voice.hold {
                if let Some(recording) = &hold.recording {
                    recording.finish();
                    hold.finishing = true;
                } else {
                    self.voice.hold = None;
                }
            }
            return true;
        }
        if let Some(hold) = &mut self.voice.hold
            && hold.recording.is_some()
        {
            if key.code == KeyCode::Esc {
                self.cancel_voice();
            } else if space {
                hold.last = Instant::now();
            }
            return true;
        }
        if !space {
            self.voice.hold = None;
            return false;
        }
        if !self.voice.enabled
            || self.modal.is_some()
            || self.focus != Focus::Composer
            || !self.drilled
        {
            return false;
        }
        let Some(id) = self.selected.clone() else {
            return false;
        };
        if let Some(hold) = &mut self.voice.hold {
            hold.last = Instant::now();
            if hold.started.elapsed() >= Duration::from_millis(300) {
                let key = self
                    .voice
                    .key
                    .clone()
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                    .filter(|s| !s.trim().is_empty());
                if !cfg!(target_os = "macos") {
                    self.voice.hold = None;
                    self.notice = Some((
                        "Voice dictation is available on macOS in this release".into(),
                        true,
                    ));
                } else {
                    self.positions.entry(id).or_default().draft = hold.before.clone();
                    hold.recording = Some(crate::voice::start(key));
                }
            }
        } else {
            let position = self.positions.entry(id.clone()).or_default();
            let before = position.draft.clone();
            position.draft.insert(" ");
            self.voice.hold = Some(Hold {
                session: id,
                before,
                started: Instant::now(),
                last: Instant::now(),
                recording: None,
                finishing: false,
                listening: false,
                partial: String::new(),
                level: 0.0,
            });
        }
        true
    }
    pub(super) fn tick_voice(&mut self, visible: bool) {
        if let Some(result) = self.voice.saving.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.voice.saving = None;
            match result {
                Ok(key) => {
                    self.voice.key = Some(key);
                    if !self.voice.enabled {
                        self.toggle_voice();
                    }
                    if matches!(self.modal, Some(Modal::Voice { .. })) {
                        self.modal = None;
                    }
                    self.notice = Some((
                        "API key saved in macOS Keychain · hold Space in the composer to dictate"
                            .into(),
                        false,
                    ));
                }
                Err(error) => self.notice = Some((error, true)),
            }
        }

        if !visible
            || self.focus != Focus::Composer
            || self.modal.is_some()
            || self
                .voice
                .hold
                .as_ref()
                .is_some_and(|h| self.selected.as_ref() != Some(&h.session))
        {
            self.cancel_voice();
            return;
        }
        let Some(hold) = &mut self.voice.hold else {
            return;
        };
        let Some(recording) = &hold.recording else {
            if hold.last.elapsed() > Duration::from_millis(800) {
                self.voice.hold = None;
            }
            return;
        };
        // Legacy terminal protocols have no key-up events. Cessation of repeat ends the hold.
        if !hold.finishing && hold.last.elapsed() > Duration::from_millis(180) {
            recording.finish();
            hold.finishing = true;
        }
        let mut done = None;
        while let Ok(event) = recording.events.try_recv() {
            match event {
                Event::Listening => hold.listening = true,
                Event::Level(level) => hold.level = level,
                Event::Partial(text) => hold.partial.push_str(&crate::model::clean(&text)),
                Event::Done(text) => {
                    done = Some(Ok(text));
                    break;
                }
                Event::Error(error) => {
                    done = Some(Err(error));
                    break;
                }
            }
        }
        if let Some(result) = done
            && let Some(hold) = self.voice.hold.take()
        {
            let position = self.positions.entry(hold.session).or_default();
            position.draft = hold.before;
            match result {
                Ok(text) if !text.trim().is_empty() => {
                    position.draft.insert(&text);
                    self.notice = Some(("Dictation inserted · Enter to send".into(), false));
                }
                Ok(_) => {
                    self.notice = Some(("No speech transcribed; draft unchanged".into(), false))
                }
                Err(error) => self.notice = Some((error, true)),
            }
        }
    }
    pub(super) fn voice_status(&self) -> Option<String> {
        self.voice
            .hold
            .as_ref()
            .filter(|h| h.recording.is_some())
            .map(|h| {
                if h.finishing {
                    "Transcribing… · Esc cancels".into()
                } else if !h.listening {
                    "Warming up microphone… · keep holding Space · Esc cancels".into()
                } else {
                    let bars = (h.level * 80.0).clamp(0.0, 12.0) as usize;
                    format!(
                        "● Listening {}{} · release Space to insert · Esc cancels",
                        "▰".repeat(bars),
                        "▱".repeat(12usize.saturating_sub(bars))
                    )
                }
            })
    }
    pub(super) fn draw_voice_preview(&self, frame: &mut Frame, rect: Rect) {
        if let Some(hold) = &self.voice.hold {
            frame.render_widget(
                Paragraph::new(hold.partial.clone())
                    .style(Style::default().fg(DIM))
                    .wrap(Wrap { trim: false }),
                rect,
            );
        }
    }
    pub(super) fn voice_modal_key(&mut self, key: KeyEvent) {
        let Some(Modal::Voice { key: value, field }) = &mut self.modal else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Tab => *field = (*field + 1) % 3,
            KeyCode::BackTab => *field = (*field + 2) % 3,
            KeyCode::Enter if *field == 0 || *field == 1 => self.save_voice_key(),
            KeyCode::Enter | KeyCode::Char(' ') if *field == 2 => self.toggle_voice(),
            _ if *field == 0 => value.key(key),
            _ => {}
        }
    }
    pub(super) fn toggle_voice(&mut self) {
        if !cfg!(target_os = "macos") {
            self.notice = Some((
                "Voice dictation is available on macOS in this release".into(),
                true,
            ));
            return;
        }
        self.voice.enabled = !self.voice.enabled;
        let result = self.storage.load_config().and_then(|mut c| {
            c.voice_enabled = self.voice.enabled;
            self.storage.save_config(&c)
        });
        if let Err(error) = result {
            self.notice = Some((format!("Could not save voice setting: {error:#}"), true));
        }
    }
    pub(super) fn draw_voice_settings(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::Voice { key, field }) = &self.modal else {
            return;
        };
        let field = *field;
        let mut masked = key.clone();
        masked.chars.fill('•');
        let explanation = if cfg!(target_os = "macos") {
            format!(
                "OpenAI live transcription · automatic language\nVoice: {} · credential: {}\nAudio is streamed only while dictating and never saved to disk.\nTap Space to type; hold to dictate; release inserts without sending.",
                if self.voice.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                if self.voice.key.is_some() {
                    "entered key"
                } else if std::env::var_os("OPENAI_API_KEY").is_some() {
                    "OPENAI_API_KEY"
                } else {
                    "Keychain (read when dictating)"
                }
            )
        } else {
            "Voice dictation is available on macOS in this release.".into()
        };
        frame.render_widget(
            Paragraph::new(explanation).wrap(Wrap { trim: false }),
            Rect::new(area.x, area.y, area.width, 7.min(area.height)),
        );
        let input = Rect::new(area.x, area.y.saturating_add(8), area.width, 3);
        editor(frame, input, "OpenAI API key (masked)", &masked, field == 0);
        self.hits.push((input, Action::VoiceField));
        self.button(
            frame,
            Rect::new(area.x, area.y.saturating_add(12), area.width, 1),
            if self.voice.saving.is_some() {
                "Saving to macOS Keychain…"
            } else {
                "Save API key in macOS Keychain and enable voice"
            },
            Action::VoiceSave,
            field == 1,
        );
        self.button(
            frame,
            Rect::new(area.x, area.y.saturating_add(14), area.width, 1),
            if self.voice.enabled {
                "Disable voice"
            } else {
                "Enable voice"
            },
            Action::VoiceToggle,
            field == 2,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    fn fixture() -> Result<(Ui, tempfile::TempDir)> {
        let tmp = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: tmp.path().join("config.json"),
                cache: tmp.path().join("cache"),
            },
            &Config::default(),
        );
        ui.voice.enabled = true;
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.selected = Some("voice-test".into());
        ui.positions.entry("voice-test".into()).or_default().draft =
            Editor::from("keep this draft");
        Ok((ui, tmp))
    }
    fn recording(ui: &mut Ui) -> Result<mpsc::Sender<Event>> {
        let before = ui
            .positions
            .get("voice-test")
            .context("draft")?
            .draft
            .clone();
        let (recording, tx) = crate::voice::fixture();
        ui.voice.hold = Some(Hold {
            session: "voice-test".into(),
            before,
            started: Instant::now(),
            last: Instant::now(),
            recording: Some(recording),
            finishing: false,
            listening: true,
            partial: String::new(),
            level: 0.0,
        });
        Ok(tx)
    }
    #[test]
    fn space_tap_and_completed_dictation_never_submit() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        ui.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        ui.key(KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(
            ui.positions
                .get("voice-test")
                .context("draft")?
                .draft
                .text(),
            "keep this draft "
        );
        let tx = recording(&mut ui)?;
        tx.send(Event::Partial("hello".into()))?;
        ui.tick_voice(true);
        assert_eq!(
            ui.positions
                .get("voice-test")
                .context("draft")?
                .draft
                .text(),
            "keep this draft "
        );
        tx.send(Event::Done("hello world".into()))?;
        ui.tick_voice(true);
        assert_eq!(
            ui.positions
                .get("voice-test")
                .context("draft")?
                .draft
                .text(),
            "keep this draft hello world"
        );
        assert!(!ui.busy);
        Ok(())
    }
    #[test]
    fn cancelling_or_losing_focus_preserves_cursor_selection_and_draft() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        let tx = recording(&mut ui)?;
        tx.send(Event::Partial("discarded".into()))?;
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let p = ui.positions.get("voice-test").context("draft")?;
        assert_eq!(p.draft.text(), "keep this draft");
        assert_eq!(p.draft.selected_text().as_deref(), Some("t"));
        let tx = recording(&mut ui)?;
        ui.tick_voice(false);
        assert!(ui.voice.hold.is_none());
        assert!(tx.send(Event::Done("too late".into())).is_err());
        assert_eq!(
            ui.positions
                .get("voice-test")
                .context("draft")?
                .draft
                .text(),
            "keep this draft"
        );
        Ok(())
    }
    #[test]
    fn credential_input_masks_text_and_never_enters_clipboard_or_config() -> Result<()> {
        use ratatui::{Terminal, backend::TestBackend};
        let (mut ui, _tmp) = fixture()?;
        ui.open_voice();
        ui.paste("test-secret-not-a-real-key");
        ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert!(ui.clipboard.is_none());
        let mut terminal = Terminal::new(TestBackend::new(100, 35))?;
        terminal.draw(|f| ui.draw(f))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(!text.contains("test-secret"));
        assert!(text.contains("••••"));
        assert!(!ui.storage.config.exists());
        Ok(())
    }
}
