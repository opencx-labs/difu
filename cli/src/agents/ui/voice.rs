//! Hold Space to talk. Codex sessions get a spoken conversation with their own
//! agent; other sessions get dictation into the composer. Both reuse Codex's
//! voice strip and split-flap transcript reveal.
use super::flap::{self, Flap};
use super::*;
use crate::ui::PURPLE;
use crate::voice::{Event, Role, Session as Voice};
use crossterm::event::KeyEventKind;
use ratatui::style::Modifier;
use std::collections::VecDeque;

/// Holding Space this long turns a press into talking instead of typing.
const HOLD: Duration = Duration::from_millis(300);
/// Without key-release reporting, a hold ends when key repeats stop.
const REPEAT_GAP: Duration = Duration::from_millis(180);
const METER_FRAMES: usize = 12;
const METER_NOISE_FLOOR: u16 = 512;
const METER_FULL_SCALE: u16 = 8192;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Rows of live dictation shown above the composer.
const PREVIEW_ROWS: usize = 4;
/// Speech keeps the speaker "active" this long, bridging gaps between words.
const SPEAKER_HOLD: Duration = Duration::from_millis(800);
/// After release, a conversation ends once nothing has happened for this long:
/// your last words are transcribed, the reply finished, and the agent idle.
const QUIET: Duration = Duration::from_millis(2500);
/// A conversation waits at most this long after release for work it handed off.
const WORK_LIMIT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Talk with a Codex session's agent; replies are spoken.
    Converse,
    /// Speech becomes text in the composer.
    Dictate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Listening,
    /// Space was released: the microphone is muted while dictation finishes
    /// or the voice replies.
    Released,
}

pub(super) struct Hold {
    session: String,
    mode: Mode,
    before: Editor,
    started: Instant,
    last: Instant,
    voice: Option<Voice>,
    /// When talking began, in Unix ms, to recognise turns the voice started.
    began: i64,
    phase: Phase,
    phase_started: Instant,
    /// Microphone and speaker intensity per 100 ms sample, newest last.
    levels: VecDeque<(u8, u8)>,
    speaking: Option<Instant>,
    released: Option<Instant>,
    busy: Instant,
    working: bool,
    connected: bool,
    segments: Vec<String>,
    partial: String,
    flap: Flap,
}
impl Hold {
    fn new(session: String, before: Editor) -> Self {
        let now = Instant::now();
        Self {
            session,
            mode: Mode::Dictate,
            before,
            started: now,
            last: now,
            voice: None,
            began: chrono::Utc::now().timestamp_millis(),
            phase: Phase::Connecting,
            phase_started: now,
            levels: VecDeque::new(),
            speaking: None,
            released: None,
            busy: now,
            working: false,
            connected: false,
            segments: Vec::new(),
            partial: String::new(),
            flap: Flap::new(Role::User, now),
        }
    }
    fn transcript(&self) -> String {
        self.segments
            .iter()
            .map(String::as_str)
            .chain((!self.partial.trim().is_empty()).then_some(self.partial.as_str()))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn set_phase(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase;
            self.phase_started = Instant::now();
        }
        let now = Instant::now();
        self.released = (phase == Phase::Released).then_some(now);
        self.busy = now;
    }
    fn speaking(&self) -> bool {
        self.speaking.is_some_and(|at| at.elapsed() < SPEAKER_HOLD)
    }
}
#[derive(Default)]
pub(super) struct State {
    pub enabled: bool,
    hold: Option<Hold>,
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
    pub(super) fn voice_active(&self) -> bool {
        self.voice.hold.is_some()
    }
    pub fn voice_enabled(&self) -> bool {
        self.voice.enabled
    }
    pub fn cancel_voice(&mut self) {
        if let Some(hold) = self.voice.hold.take()
            && hold.voice.is_some()
            && hold.mode == Mode::Dictate
            && let Some(position) = self.positions.get_mut(&hold.session)
        {
            position.draft = hold.before;
        }
    }
    /// Ready Codex sessions talk back, including ones just switched from
    /// Claude that have no Codex thread yet; others take dictation.
    fn voice_mode(&self, id: &str) -> Mode {
        let codex = self.sessions.get(id).is_some_and(|session| {
            matches!(session.job, Job::Coding(_))
                && session.provider == provider::Provider::Codex
                && !session.archived
                && !session.preparing()
        });
        if codex { Mode::Converse } else { Mode::Dictate }
    }
    fn start_voice(&self, id: &str, mode: Mode) -> Voice {
        if mode == Mode::Dictate {
            return crate::voice::dictate();
        }
        let (storage, start) = (self.storage.clone(), id.to_owned());
        let (stop_storage, stop) = (self.storage.clone(), id.to_owned());
        crate::voice::converse(
            move |offer| {
                let request = Request::Voice {
                    id: start,
                    action: VoiceAction::Start {
                        offer: offer.to_owned(),
                    },
                };
                match client::request(&storage, request)? {
                    Reply::Voice(Some(answer)) => Ok(answer),
                    _ => anyhow::bail!("The agent service returned no voice connection"),
                }
            },
            move || {
                let request = Request::Voice {
                    id: stop,
                    action: VoiceAction::Stop,
                };
                let _ = client::request(&stop_storage, request);
            },
        )
    }
    pub(super) fn voice_key(&mut self, key: KeyEvent) -> bool {
        let space = key.code == KeyCode::Char(' ') && key.modifiers.is_empty();
        if key.kind == KeyEventKind::Release {
            if space && let Some(hold) = &mut self.voice.hold {
                let Some(voice) = &hold.voice else {
                    self.voice.hold = None;
                    return true;
                };
                if hold.phase != Phase::Released {
                    voice.release();
                    hold.set_phase(Phase::Released);
                }
            }
            return true;
        }
        if let Some(hold) = &mut self.voice.hold
            && let Some(voice) = &hold.voice
        {
            if key.code == KeyCode::Esc {
                let mode = hold.mode;
                self.cancel_voice();
                self.notice = Some((
                    match mode {
                        Mode::Converse => "Voice ended".into(),
                        Mode::Dictate => "Voice cancelled; draft unchanged".into(),
                    },
                    false,
                ));
                return true;
            }
            if space {
                hold.last = Instant::now();
                // Holding Space while a conversation is still connected talks again at once.
                if hold.mode == Mode::Converse && hold.phase == Phase::Released {
                    voice.resume();
                    hold.set_phase(Phase::Listening);
                }
                return true;
            }
            // While the voice replies or the agent works, the composer stays usable.
            return !(hold.mode == Mode::Converse && hold.phase == Phase::Released);
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
        if self
            .voice
            .hold
            .as_ref()
            .is_some_and(|hold| hold.started.elapsed() >= HOLD)
        {
            let mode = self.voice_mode(&id);
            let voice = self.start_voice(&id, mode);
            if let Some(hold) = &mut self.voice.hold {
                // The tap's space was provisional; talking replaces it.
                self.positions.entry(id).or_default().draft = hold.before.clone();
                hold.last = Instant::now();
                hold.began = chrono::Utc::now().timestamp_millis();
                hold.mode = mode;
                hold.voice = Some(voice);
                hold.set_phase(Phase::Connecting);
            }
        } else if let Some(hold) = &mut self.voice.hold {
            hold.last = Instant::now();
        } else {
            let position = self.positions.entry(id.clone()).or_default();
            let before = position.draft.clone();
            position.draft.insert(" ");
            self.voice.hold = Some(Hold::new(id, before));
        }
        true
    }
    pub(super) fn tick_voice(&mut self, visible: bool) {
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
        let Some(voice) = &hold.voice else {
            if hold.last.elapsed() > Duration::from_millis(800) {
                self.voice.hold = None;
            }
            return;
        };
        // Legacy terminal protocols have no key-up events. Cessation of repeat ends the hold.
        let released = hold.phase != Phase::Released && hold.last.elapsed() > REPEAT_GAP;
        if released {
            voice.release();
        }
        let mut events = Vec::new();
        let mut ended = false;
        loop {
            match voice.events.try_recv() {
                Ok(event) => events.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    ended = true;
                    break;
                }
            }
        }
        if released {
            hold.set_phase(Phase::Released);
        }
        let mut outcome = ended.then_some(Ok(()));
        for event in events {
            crate::repaint::request();
            match event {
                Event::Listening => {
                    if hold.phase == Phase::Connecting {
                        hold.set_phase(Phase::Listening);
                    }
                }
                Event::Levels {
                    microphone,
                    speaker,
                } => {
                    if hold.levels.len() >= METER_FRAMES {
                        hold.levels.pop_front();
                    }
                    let speaker = intensity(speaker);
                    hold.levels.push_back((intensity(microphone), speaker));
                    if speaker > 0 {
                        hold.speaking = Some(Instant::now());
                    }
                }
                Event::Delta(Role::User, delta) => hold.partial.push_str(&delta),
                Event::Segment(Role::User, text) => {
                    hold.segments.push(text);
                    hold.partial.clear();
                }
                Event::Delta(..) | Event::Segment(..) => {}
                Event::Error(error) => outcome = Some(Err(error)),
            }
        }
        match hold.mode {
            Mode::Dictate => self.tick_dictation(outcome),
            Mode::Converse => self.tick_conversation(outcome),
        }
    }
    fn tick_dictation(&mut self, outcome: Option<Result<(), String>>) {
        let Some(hold) = &mut self.voice.hold else {
            return;
        };
        let transcript = crate::model::clean(&hold.transcript());
        hold.flap.set(&transcript, Instant::now());
        let Some(outcome) = outcome else {
            return;
        };
        let Some(hold) = self.voice.hold.take() else {
            return;
        };
        let position = self.positions.entry(hold.session).or_default();
        position.draft = hold.before;
        self.notice = Some(match outcome {
            Err(error) => (error, true),
            Ok(()) if transcript.is_empty() => ("No speech heard; draft unchanged".into(), false),
            Ok(()) => {
                let draft = &mut position.draft;
                let joined = draft
                    .cursor
                    .checked_sub(1)
                    .and_then(|index| draft.chars.get(index))
                    .is_some_and(|c| !c.is_whitespace());
                draft.insert(&if joined {
                    format!(" {transcript}")
                } else {
                    transcript
                });
                ("Dictation inserted · Enter to send".into(), false)
            }
        });
    }
    /// A conversation ends once, after release, nothing has happened for a
    /// moment: your words are transcribed, the reply has been spoken, and any
    /// work the voice handed to the agent is done.
    fn tick_conversation(&mut self, outcome: Option<Result<(), String>>) {
        let Some(hold) = &mut self.voice.hold else {
            return;
        };
        if let Some(outcome) = outcome {
            self.voice.hold = None;
            if let Err(error) = outcome {
                self.notice = Some((error, true));
            }
            return;
        }
        let Some(session) = self.sessions.get(&hold.session) else {
            return;
        };
        if session.voice {
            hold.connected = true;
        } else if hold.connected {
            self.voice.hold = None;
            self.notice = Some(("The voice connection closed".into(), false));
            return;
        }
        let transcribing = session
            .entries
            .iter()
            .any(|entry| flap::voice_role(entry).is_some() && entry.finished_at.is_none());
        hold.working = session.status.active()
            && session
                .turn_started_at
                .is_some_and(|started| started >= hold.began);
        if transcribing || hold.working || hold.speaking() {
            hold.busy = Instant::now();
        }
        if let Some(released) = hold.released
            && (hold.busy.elapsed() >= QUIET || released.elapsed() >= WORK_LIMIT)
        {
            self.voice.hold = None;
        }
    }
    /// Rows the voice strip (and live dictation) need above the composer.
    fn voice_preview(&self, hold: &Hold, width: u16, now: Instant) -> Vec<Line<'static>> {
        if self.preferences.motion.reply() {
            hold.flap.lines(width, now, flap::preview_board())
        } else {
            wrapped(&crate::model::clean(&hold.transcript()), width)
                .into_iter()
                .map(|text| Line::from(Span::styled(text, flap::preview_board())))
                .collect()
        }
    }
    pub(super) fn voice_rows(&self, width: u16) -> u16 {
        let Some(hold) = self.voice.hold.as_ref().filter(|h| h.voice.is_some()) else {
            return 0;
        };
        if hold.mode == Mode::Converse {
            return 2;
        }
        let preview = self
            .voice_preview(hold, width.saturating_sub(2), Instant::now())
            .len()
            .min(PREVIEW_ROWS);
        2 + u16::try_from(preview).unwrap_or(0)
    }
    /// Codex's voice strip: state and controls, then the microphone and voice
    /// meters, then (when dictating) the live transcript.
    pub(super) fn draw_voice(&self, frame: &mut Frame, rect: Rect) {
        let Some(hold) = self.voice.hold.as_ref().filter(|h| h.voice.is_some()) else {
            return;
        };
        let area = Rect::new(
            rect.x.saturating_add(1),
            rect.y,
            rect.width.saturating_sub(2),
            rect.height,
        );
        if area.is_empty() {
            return;
        }
        let now = Instant::now();
        let spinner = || {
            if self.preferences.motion.level != crate::settings::Motion::Full {
                return "●";
            }
            let frame = now
                .saturating_duration_since(hold.phase_started)
                .as_millis()
                / 100;
            SPINNER
                .get(usize::try_from(frame).unwrap_or_default() % SPINNER.len())
                .copied()
                .unwrap_or("◌")
        };
        let live = Style::default().fg(RED).add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(DIM);
        let (marker, activity) = match (hold.phase, hold.mode) {
            (Phase::Connecting, _) => (Span::styled(spinner(), live), "connecting"),
            (Phase::Listening, _) => (Span::styled("●", live), "listening"),
            (Phase::Released, Mode::Dictate) => (
                Span::styled(spinner(), Style::default().fg(ACCENT)),
                "transcribing",
            ),
            (Phase::Released, Mode::Converse) if hold.speaking() => {
                (Span::styled("◌", dim), "speaking")
            }
            (Phase::Released, Mode::Converse) if hold.working => (
                Span::styled(spinner(), Style::default().fg(ACCENT)),
                "agent working",
            ),
            (Phase::Released, Mode::Converse) => (
                Span::styled(spinner(), Style::default().fg(ACCENT)),
                "thinking",
            ),
        };
        let key = Style::default().fg(TEXT).add_modifier(Modifier::BOLD);
        let mut status = vec![
            Span::styled("voice ", dim),
            marker,
            Span::styled(format!(" {activity}"), Style::default().fg(TEXT)),
        ];
        let hint = |verb: &'static str, rest: &'static str, end: &'static str| {
            vec![
                Span::styled(verb, dim),
                Span::styled("space", key),
                Span::styled(rest, dim),
                Span::styled("esc", key),
                Span::styled(end, dim),
            ]
        };
        let controls = match (hold.phase, hold.mode) {
            (Phase::Released, Mode::Dictate) => {
                vec![Span::styled("esc", key), Span::styled(" cancel", dim)]
            }
            (Phase::Released, Mode::Converse) => hint("hold ", " to reply   ", " end"),
            (_, Mode::Converse) => hint("release ", " to stop talking   ", " end"),
            (_, Mode::Dictate) => hint("release ", " to insert   ", " cancel"),
        };
        let short = vec![
            Span::styled("esc", key),
            Span::styled(
                if hold.mode == Mode::Converse {
                    " end"
                } else {
                    " cancel"
                },
                dim,
            ),
        ];
        let used = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
        let available = usize::from(area.width);
        if let Some(controls) = [controls, short]
            .into_iter()
            .find(|controls| available > used(&status) + used(controls))
        {
            status.push(Span::raw(
                " ".repeat(available - used(&status) - used(&controls)),
            ));
            status.extend(controls);
        }
        frame.render_widget(
            Paragraph::new(Line::from(status)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if area.height < 2 {
            return;
        }
        let width = (available.saturating_sub(14) / 2).min(6);
        let microphone_live = hold.phase == Phase::Listening;
        let mut meters = vec![Span::styled("  mic ", dim)];
        meters.extend(meter(
            hold.levels.iter().map(|(microphone, _)| *microphone),
            width,
            ACCENT,
            !microphone_live,
        ));
        if hold.mode == Mode::Converse {
            meters.push(Span::styled("  codex ", dim));
            meters.extend(meter(
                hold.levels.iter().map(|(_, speaker)| *speaker),
                width,
                PURPLE,
                false,
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(meters)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        if hold.mode == Mode::Converse {
            return;
        }
        let lines = self.voice_preview(hold, area.width, now);
        let shown = lines.len().min(usize::from(area.height.saturating_sub(2)));
        let tail: Vec<_> = lines.into_iter().rev().take(shown).rev().collect();
        frame.render_widget(
            Paragraph::new(tail),
            Rect::new(
                area.x,
                area.y + 2,
                area.width,
                u16::try_from(shown).unwrap_or(0),
            ),
        );
    }
    pub(super) fn toggle_voice(&mut self) {
        let enabled = !self.voice.enabled;
        if enabled && let Err(error) = crate::voice::available() {
            self.notice = Some((format!("{error:#}"), true));
            return;
        }
        match self.storage.update_config(|c| c.voice_enabled = enabled) {
            Ok(_) => {
                self.voice.enabled = enabled;
                self.notice = Some(if enabled {
                    (
                        "Voice on · hold Space in the composer to talk · uses your Codex login"
                            .into(),
                        false,
                    )
                } else {
                    ("Voice off".into(), false)
                });
            }
            Err(error) => {
                self.notice = Some((format!("Could not save voice setting: {error:#}"), true));
            }
        }
    }
}

/// Codex's sample-history meter: the newest `width` samples as rising bars.
fn meter(
    samples: impl ExactSizeIterator<Item = u8>,
    width: usize,
    color: ratatui::style::Color,
    muted: bool,
) -> Vec<Span<'static>> {
    let skip = samples.len().saturating_sub(width);
    let pad = width.saturating_sub(samples.len());
    std::iter::repeat_n(0, pad)
        .chain(samples.skip(skip))
        .map(|level| {
            let height = if muted {
                0
            } else {
                usize::from(level).saturating_mul(7).div_ceil(255)
            };
            let cell = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"]
                .get(height)
                .copied()
                .unwrap_or("█");
            Span::styled(
                cell,
                Style::default().fg(if height == 0 { DIM } else { color }),
            )
        })
        .collect()
}

/// Codex's calibration: peaks below the noise floor read as silence.
fn intensity(peak: u16) -> u8 {
    let level = usize::from(peak.saturating_sub(METER_NOISE_FLOOR))
        .saturating_mul(usize::from(u8::MAX))
        .div_ceil(usize::from(METER_FULL_SCALE - METER_NOISE_FLOOR))
        .min(usize::from(u8::MAX));
    u8::try_from(level).unwrap_or(u8::MAX)
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
    fn coding() -> Session {
        Session::new(
            "voice-test".into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Task".into(),
                model: None,
                effort: None,
            }),
        )
    }
    fn talking(ui: &mut Ui, mode: Mode) -> Result<mpsc::Sender<Event>> {
        let before = ui
            .positions
            .get("voice-test")
            .context("draft")?
            .draft
            .clone();
        let (voice, tx) = crate::voice::fixture();
        let mut hold = Hold::new("voice-test".into(), before);
        hold.mode = mode;
        hold.voice = Some(voice);
        ui.voice.hold = Some(hold);
        Ok(tx)
    }
    fn draft(ui: &Ui) -> Result<String> {
        Ok(ui
            .positions
            .get("voice-test")
            .context("draft")?
            .draft
            .text())
    }
    fn release(ui: &mut Ui) {
        ui.voice_key(KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
    }
    fn screen(ui: &mut Ui, width: u16) -> Result<String> {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(width, 24))?;
        terminal.draw(|frame| ui.draw(frame))?;
        Ok(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect())
    }
    #[test]
    fn space_tap_types_and_finished_dictation_inserts_without_sending() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        ui.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        release(&mut ui);
        assert_eq!(draft(&ui)?, "keep this draft ");
        let tx = talking(&mut ui, Mode::Dictate)?;
        tx.send(Event::Listening)?;
        tx.send(Event::Delta(Role::User, "hello".into()))?;
        // The voice's own replies never reach a dictated draft.
        tx.send(Event::Delta(Role::Assistant, "Hi there!".into()))?;
        ui.tick_voice(true);
        assert_eq!(draft(&ui)?, "keep this draft ");
        tx.send(Event::Segment(Role::User, "hello world".into()))?;
        tx.send(Event::Delta(Role::User, "again".into()))?;
        drop(tx);
        ui.tick_voice(true);
        assert_eq!(draft(&ui)?, "keep this draft hello world again");
        assert!(ui.voice.hold.is_none());
        assert!(!ui.busy);
        Ok(())
    }
    #[test]
    fn cancelling_errors_or_losing_focus_preserve_the_draft() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        let tx = talking(&mut ui, Mode::Dictate)?;
        tx.send(Event::Delta(Role::User, "discarded".into()))?;
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let p = ui.positions.get("voice-test").context("draft")?;
        assert_eq!(p.draft.text(), "keep this draft");
        assert_eq!(p.draft.selected_text().as_deref(), Some("t"));
        let tx = talking(&mut ui, Mode::Dictate)?;
        ui.tick_voice(false);
        assert!(ui.voice.hold.is_none());
        assert!(
            tx.send(Event::Segment(Role::User, "too late".into()))
                .is_err()
        );
        let tx = talking(&mut ui, Mode::Dictate)?;
        tx.send(Event::Delta(Role::User, "half".into()))?;
        tx.send(Event::Error("Codex voice failed: closed".into()))?;
        ui.tick_voice(true);
        assert_eq!(draft(&ui)?, "keep this draft");
        assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
        Ok(())
    }
    #[test]
    fn codex_sessions_converse_and_others_dictate() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        assert_eq!(ui.voice_mode("voice-test"), Mode::Dictate);
        let mut session = coding();
        ui.sessions.insert(session.id.clone(), session.clone());
        assert_eq!(
            ui.voice_mode("voice-test"),
            Mode::Dictate,
            "still preparing"
        );
        // Just switched from Claude: idle, with no Codex thread until needed.
        session.status = Status::Idle;
        ui.sessions.insert(session.id.clone(), session.clone());
        assert_eq!(ui.voice_mode("voice-test"), Mode::Converse);
        session.provider = provider::Provider::Claude;
        ui.sessions.insert(session.id.clone(), session);
        assert_eq!(ui.voice_mode("voice-test"), Mode::Dictate);
        Ok(())
    }
    #[test]
    fn reduced_and_off_voice_show_received_words_without_flapping() -> Result<()> {
        for motion in [
            crate::settings::Motion::Reduced,
            crate::settings::Motion::Off,
        ] {
            let (mut ui, _tmp) = fixture()?;
            ui.preferences.motion.level = motion;
            ui.list_visible = false;
            let mut session = coding();
            session.status = Status::Idle;
            ui.sessions.insert(session.id.clone(), session);
            let tx = talking(&mut ui, Mode::Dictate)?;
            tx.send(Event::Delta(Role::User, "complete spoken words".into()))?;
            ui.tick_voice(true);
            let first = screen(&mut ui, 100)?;
            assert!(first.contains("complete spoken words"), "{first}");
            ui.voice.hold.as_mut().context("hold")?.phase_started -= Duration::from_millis(350);
            assert_eq!(
                screen(&mut ui, 100)?,
                first,
                "decorative spinner must remain static"
            );
            assert_eq!(draft(&ui)?, "keep this draft");
        }
        Ok(())
    }

    #[test]
    fn dictation_strip_shows_codex_states_meter_and_transcript() -> Result<()> {
        for width in [48, 100] {
            let (mut ui, _tmp) = fixture()?;
            ui.list_visible = false;
            let session = coding();
            ui.sessions.insert(session.id.clone(), session);
            let tx = talking(&mut ui, Mode::Dictate)?;
            assert!(screen(&mut ui, width)?.contains("connecting"));
            tx.send(Event::Listening)?;
            tx.send(Event::Levels {
                microphone: 8192,
                speaker: 0,
            })?;
            tx.send(Event::Delta(Role::User, "spoken words".into()))?;
            ui.voice.hold.as_mut().context("hold")?.last = Instant::now();
            ui.tick_voice(true);
            std::thread::sleep(Duration::from_millis(700));
            let listening = screen(&mut ui, width)?;
            assert!(listening.contains("● listening"), "{listening}");
            assert!(listening.contains("mic ▁▁▁▁▁█"), "{listening}");
            assert!(
                !listening.contains("codex ▁"),
                "dictation has no voice meter"
            );
            assert!(listening.contains("spoken words"));
            release(&mut ui);
            assert!(screen(&mut ui, width)?.contains("transcribing"));
            drop(tx);
            ui.tick_voice(true);
            assert!(!screen(&mut ui, width)?.contains("transcribing"));
            assert_eq!(draft(&ui)?, "keep this draft spoken words");
        }
        Ok(())
    }
    #[test]
    fn conversation_waits_for_the_reply_then_ends_and_never_edits_the_draft() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        ui.list_visible = false;
        let mut session = coding();
        session.thread_id = Some("thread".into());
        session.voice = true;
        ui.sessions.insert(session.id.clone(), session.clone());
        let tx = talking(&mut ui, Mode::Converse)?;
        tx.send(Event::Listening)?;
        tx.send(Event::Levels {
            microphone: 4000,
            speaker: 8192,
        })?;
        ui.voice.hold.as_mut().context("hold")?.last = Instant::now();
        ui.tick_voice(true);
        let listening = screen(&mut ui, 100)?;
        assert!(listening.contains("● listening"), "{listening}");
        assert!(listening.contains("codex ▁▁▁▁▁█"), "{listening}");
        assert!(listening.contains("release space to stop talking"));
        release(&mut ui);
        // The voice is still answering: the session stays open.
        ui.tick_voice(true);
        let replying = screen(&mut ui, 100)?;
        assert!(replying.contains("speaking"), "{replying}");
        assert!(replying.contains("hold space to reply"));
        // Typing works while the voice replies.
        ui.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(draft(&ui)?, "keep this draftx");
        let hold = ui.voice.hold.as_mut().context("hold")?;
        hold.speaking = None;
        hold.busy = Instant::now() - QUIET;
        ui.tick_voice(true);
        assert!(ui.voice.hold.is_none(), "quiet after the reply ends it");
        assert_eq!(draft(&ui)?, "keep this draftx");
        drop(tx);
        Ok(())
    }
    #[test]
    fn conversation_stays_while_the_agent_works_on_what_the_voice_asked() -> Result<()> {
        let (mut ui, _tmp) = fixture()?;
        let mut session = coding();
        session.thread_id = Some("thread".into());
        session.voice = true;
        ui.sessions.insert(session.id.clone(), session.clone());
        let _tx = talking(&mut ui, Mode::Converse)?;
        release(&mut ui);
        session.status = Status::Running;
        session.turn_started_at = Some(chrono::Utc::now().timestamp_millis() + 1);
        ui.sessions.insert(session.id.clone(), session.clone());
        ui.voice.hold.as_mut().context("hold")?.busy = Instant::now() - QUIET;
        ui.tick_voice(true);
        assert!(ui.voice.hold.as_ref().is_some_and(|hold| hold.working));
        assert!(screen(&mut ui, 100)?.contains("agent working"));
        // A new hold talks again on the same connection.
        ui.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            ui.voice
                .hold
                .as_ref()
                .is_some_and(|h| h.phase == Phase::Listening)
        );
        // Codex closing the voice ends it here too.
        session.voice = false;
        ui.sessions.insert(session.id.clone(), session);
        ui.tick_voice(true);
        assert!(ui.voice.hold.is_none());
        Ok(())
    }
    #[test]
    fn meter_follows_codex_calibration() {
        assert_eq!(intensity(0), 0);
        assert_eq!(intensity(METER_NOISE_FLOOR), 0);
        assert_eq!(intensity(METER_FULL_SCALE), 255);
        assert_eq!(intensity(u16::MAX), 255);
    }
}
