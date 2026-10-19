//! Codex's split-flap reveal for live voice transcripts: each new letter flips
//! through letters already spoken before settling, then glows briefly in the
//! speaker's colour. Timings match Codex's realtime transcript cells.
use super::{Entry, Position, Session, reveal, transcript};
use crate::ui::{DIM, PANEL, PURPLE, TEXT};
use crate::voice::Role;
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::time::{Duration, Instant};

pub(super) const FRAME: Duration = Duration::from_millis(45);
const ANIMATION: Duration = Duration::from_millis(675);
const SETTLE: Duration = Duration::from_millis(180);
const AFTERGLOW: Duration = Duration::from_millis(135);
const BLANK: Duration = Duration::from_millis(40);
const SAMPLE: usize = 24;

pub(super) struct Flap {
    role: Role,
    text: String,
    /// When each flippable glyph (ASCII letter or digit) arrived, in order.
    arrivals: Vec<Instant>,
    /// Recent letters that unsettled tiles flip through.
    sample: Vec<u8>,
    updated: Instant,
    phase: Instant,
}

impl Flap {
    pub(super) fn new(role: Role, now: Instant) -> Self {
        Self {
            role,
            text: String::new(),
            arrivals: Vec::new(),
            sample: Vec::new(),
            updated: now,
            phase: now,
        }
    }
    /// Replaces the text. Glyphs already shown keep their timing when the new
    /// text extends the old; new ones arrive in a short stagger.
    pub(super) fn set(&mut self, text: &str, now: Instant) {
        if text == self.text {
            return;
        }
        let extends = !self.text.is_empty() && text.starts_with(&self.text);
        if !extends {
            self.arrivals.clear();
            self.phase = now;
        }
        let start = if extends {
            now.checked_sub(FRAME).unwrap_or(now)
        } else {
            now
        };
        let total = text.bytes().filter(u8::is_ascii_alphanumeric).count();
        let existing = self.arrivals.len().min(total);
        self.arrivals.truncate(existing);
        for index in 0..total - existing {
            let offset = u64::try_from(index)
                .unwrap_or(u64::MAX)
                .saturating_mul(8)
                .min(56);
            self.arrivals.push(start + Duration::from_millis(offset));
        }
        let mut sample: Vec<u8> = text
            .bytes()
            .rev()
            .filter(u8::is_ascii_alphabetic)
            .take(SAMPLE)
            .collect();
        if sample.is_empty() {
            sample.extend(
                text.bytes()
                    .rev()
                    .filter(|b| !b.is_ascii_whitespace())
                    .take(SAMPLE),
            );
        }
        sample.reverse();
        self.sample = sample;
        self.text = text.to_owned();
        self.updated = now;
    }
    pub(super) fn animating(&self, now: Instant) -> bool {
        !self.arrivals.is_empty() && now.saturating_duration_since(self.updated) < ANIMATION
    }
    /// Wraps the text to `width` and renders the current animation frame on
    /// `board`, the style of the surface the text sits on.
    pub(super) fn lines(&self, width: u16, now: Instant, board: Style) -> Vec<Line<'static>> {
        if self.text.is_empty() {
            return Vec::new();
        }
        let width = usize::from(width.max(1));
        let glow = match self.role {
            Role::User => crate::ui::ACCENT,
            Role::Assistant => crate::ui::PURPLE,
        };
        let frame = usize::try_from(
            now.saturating_duration_since(self.phase).as_millis() / FRAME.as_millis(),
        )
        .unwrap_or(usize::MAX);
        let wrapped = textwrap::wrap(&self.text, textwrap::Options::new(width));
        let last = wrapped.len().saturating_sub(1);
        let mut glyph = 0;
        let mut lines = Vec::with_capacity(wrapped.len());
        for (row, text) in wrapped.iter().enumerate() {
            let mut spans = Vec::new();
            let mut unsettled = false;
            for character in text.chars() {
                if character.is_ascii_alphanumeric() {
                    let arrival = self.arrivals.get(glyph).copied().unwrap_or(self.updated);
                    let elapsed = now.saturating_duration_since(arrival);
                    let (shown, style) = if elapsed < BLANK {
                        (' ', board.fg(DIM))
                    } else if elapsed < SETTLE {
                        (self.tile(character, glyph, frame), board.fg(DIM))
                    } else if elapsed < SETTLE + AFTERGLOW {
                        (character, board.fg(glow))
                    } else {
                        (character, board)
                    };
                    unsettled |= elapsed < SETTLE;
                    glyph += 1;
                    spans.push(Span::styled(shown.to_string(), style));
                } else {
                    if character.is_whitespace() {
                        unsettled = false;
                    }
                    let shown = if unsettled && character.is_ascii_punctuation() {
                        ' '
                    } else {
                        character
                    };
                    spans.push(Span::styled(shown.to_string(), board));
                }
            }
            let used = textwrap::core::display_width(text);
            let room = width.saturating_sub(used);
            if row == last
                && room > 1
                && self.animating(now)
                && !self.sample.is_empty()
                && self
                    .arrivals
                    .first()
                    .is_some_and(|first| now.saturating_duration_since(*first) >= SETTLE)
            {
                let runway: String = (0..room.saturating_sub(1).min(3))
                    .filter_map(|index| {
                        self.sample
                            .get((frame + index) % self.sample.len())
                            .map(|b| char::from(*b))
                    })
                    .collect();
                spans.push(Span::styled(format!(" {runway}"), board.fg(DIM)));
            }
            let used: usize = spans.iter().map(|span| span.width()).sum();
            if used < width {
                spans.push(Span::styled(" ".repeat(width - used), board));
            }
            lines.push(Line::from(spans));
        }
        lines
    }
    fn tile(&self, character: char, index: usize, frame: usize) -> char {
        if self.sample.is_empty() {
            return character;
        }
        let offset = (character as usize)
            .wrapping_add(index.wrapping_mul(7))
            .wrapping_add(frame.wrapping_mul(11));
        self.sample
            .get(offset % self.sample.len())
            .map_or(character, |b| char::from(*b))
    }
}

/// The surface the dictation preview sits on.
pub(super) fn preview_board() -> Style {
    Style::default().bg(PANEL).fg(TEXT)
}

/// The voice role of a chat entry, for transcripts difu recorded from voice.
pub(super) fn voice_role(entry: &Entry) -> Option<Role> {
    match entry
        .data
        .get("difuVoice")
        .and_then(serde_json::Value::as_str)
    {
        Some("user") => Some(Role::User),
        Some("assistant") => Some(Role::Assistant),
        _ => None,
    }
}

/// Chat rows for a voice transcript: the user's words in their message style
/// with `›`, the voice's words behind a purple `•`. `text` renders the words.
pub(super) fn rows(
    role: Role,
    width: u16,
    text: impl FnOnce(u16, Style) -> Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let (marker, board) = match role {
        Role::User => ("›", crate::ui::user_message_style()),
        Role::Assistant => ("•", Style::default()),
    };
    let marker_style = match role {
        Role::User => board,
        Role::Assistant => Style::default().fg(PURPLE),
    };
    text(width.saturating_sub(2), board)
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            let prefix = if index == 0 {
                format!("{marker} ")
            } else {
                "  ".into()
            };
            line.spans.insert(0, Span::styled(prefix, marker_style));
            let padding = usize::from(width).saturating_sub(line.width());
            if padding > 0 {
                line.spans.push(Span::styled(" ".repeat(padding), board));
            }
            line.style(board)
        })
        .collect()
}

/// Plays Codex's split-flap reveal over voice transcripts still being spoken.
/// Entries already finished when first seen are left as they are.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    position: &mut Position,
    session: &Session,
    sections: &[transcript::Section],
    start: usize,
    total: usize,
    lines: &mut [Line<'static>],
    width: u16,
    now: Instant,
) {
    let available = lines.len();
    for entry in &session.entries {
        let Some(role) = voice_role(entry) else {
            continue;
        };
        let live = entry.finished_at.is_none();
        let flap = match position.voice_flaps.get_mut(&entry.id) {
            Some(flap) => flap,
            None if live => position
                .voice_flaps
                .entry(entry.id.clone())
                .or_insert_with(|| Flap::new(role, now)),
            None => continue,
        };
        flap.set(&crate::model::clean(&entry.text), now);
        let Some(range) = reveal::rows(sections, start, total, available, &entry.id) else {
            continue;
        };
        let animated = rows(role, width, |width, board| flap.lines(width, now, board));
        if let Some(target) = lines.get_mut(range)
            && target.len() == animated.len()
        {
            target.clone_from_slice(&animated);
        }
    }
    position.voice_flaps.retain(|id, flap| {
        flap.animating(now)
            || session
                .entries
                .iter()
                .any(|entry| &entry.id == id && entry.finished_at.is_none())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn new_letters_flip_then_settle_while_extended_text_keeps_its_timing() {
        let start = Instant::now();
        let mut flap = Flap::new(Role::User, start);
        flap.set("hey there", start);
        let blank = plain(&flap.lines(20, start, preview_board()));
        assert!(!blank.contains("hey"), "letters start blank: {blank:?}");
        let settled = start + SETTLE + Duration::from_millis(60);
        assert!(plain(&flap.lines(20, settled, preview_board())).starts_with("hey there"));
        // Extending keeps the settled prefix and flips only the new word.
        flap.set("hey there friend", settled);
        let next = plain(&flap.lines(30, settled + Duration::from_millis(50), preview_board()));
        assert!(next.starts_with("hey there "), "{next:?}");
        assert!(!next.contains("friend"), "{next:?}");
        let later = settled + ANIMATION;
        assert!(plain(&flap.lines(30, later, preview_board())).starts_with("hey there friend"));
        assert!(!flap.animating(later));
    }

    #[test]
    fn chat_flips_live_voice_messages_and_leaves_spoken_history_alone() {
        use crate::agents::{Job, Launch};
        let mut session = Session::new(
            "voice".into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Task".into(),
                model: None,
                effort: None,
            }),
        );
        for (id, kind, role, text, finished_at) in [
            (
                "old",
                "agentMessage",
                "assistant",
                "Earlier spoken reply",
                Some(1),
            ),
            ("live", "userMessage", "user", "hello there", None),
        ] {
            session.entries.push(Entry {
                id: id.into(),
                kind: kind.into(),
                text: text.to_owned().into(),
                data: serde_json::json!({"difuVoice": role}).into(),
                started_at: None,
                finished_at,
            });
        }
        let mut position = Position::default();
        let frame = |position: &mut Position, at: Instant| {
            let (mut lines, sections) = transcript::render(&session, position, 40, false);
            let total = lines.len();
            apply(position, &session, &sections, 0, total, &mut lines, 40, at);
            plain(&lines)
        };
        let start = Instant::now();
        let first = frame(&mut position, start);
        assert!(first.contains("• Earlier spoken reply"), "{first}");
        assert!(
            first.contains("› ") && !first.contains("hello there"),
            "{first}"
        );
        assert!(!position.voice_flaps.contains_key("old"));
        let settled = frame(&mut position, start + Duration::from_secs(1));
        assert!(settled.contains("› hello there"), "{settled}");
    }

    #[test]
    fn replaced_text_restarts_and_lines_fill_the_board_width() {
        let start = Instant::now();
        let mut flap = Flap::new(Role::Assistant, start);
        flap.set("first words here", start);
        flap.set("other", start + Duration::from_millis(500));
        let lines = flap.lines(8, start + Duration::from_secs(2), preview_board());
        assert_eq!(plain(&lines), "other   ");
        assert!(lines.iter().all(|line| line.width() == 8));
    }
}
