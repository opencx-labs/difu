//! Codex's split-flap reveal, used across the chat: each new character starts
//! blank, flips through letters from the same text, settles, then glows briefly
//! in the speaker's colour. It plays over already-rendered rows, so markdown
//! styles survive and hidden characters keep their cells: layout never moves.
//! Timings match Codex's realtime transcript cells.
use super::{Entry, Position, Session, reveal, transcript};
use crate::ui::{ACCENT, DIM, PANEL, PURPLE, TEXT};
use crate::voice::Role;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use std::time::{Duration, Instant};

pub(super) const FRAME: Duration = Duration::from_millis(45);
const ANIMATION: Duration = Duration::from_millis(675);
const SETTLE: Duration = Duration::from_millis(180);
const AFTERGLOW: Duration = Duration::from_millis(135);
/// From a character's arrival until it rests in its own style.
pub(super) const LIFETIME: Duration = SETTLE.saturating_add(AFTERGLOW);
const BLANK: Duration = Duration::from_millis(40);
const SAMPLE: usize = 24;
const RUNWAY: usize = 3;

/// Codex's arrival stagger for characters that appear together.
pub(super) fn stagger(index: usize) -> Duration {
    Duration::from_millis(
        u64::try_from(index)
            .unwrap_or(u64::MAX)
            .saturating_mul(8)
            .min(56),
    )
}

/// The visible characters of rendered rows in reading order. Whitespace,
/// including row padding, never animates and is not counted.
pub(super) fn glyphs(lines: &[Line]) -> Vec<char> {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .flat_map(|span| span.content.chars())
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// Recent letters of a text, which unsettled tiles flip through.
pub(super) fn sample(glyphs: impl DoubleEndedIterator<Item = char> + Clone) -> Vec<u8> {
    let ascii = |filter: fn(&char) -> bool| -> Vec<u8> {
        let mut letters: Vec<u8> = glyphs
            .clone()
            .rev()
            .filter(filter)
            .filter_map(|c| u8::try_from(c).ok())
            .take(SAMPLE)
            .collect();
        letters.reverse();
        letters
    };
    let letters = ascii(char::is_ascii_alphabetic);
    if letters.is_empty() {
        ascii(char::is_ascii_graphic)
    } else {
        letters
    }
}

fn tile(sample: &[u8], character: char, index: usize, frame: usize) -> char {
    if sample.is_empty() {
        return character;
    }
    let offset = (character as usize)
        .wrapping_add(index.wrapping_mul(7))
        .wrapping_add(frame.wrapping_mul(11));
    sample
        .get(offset % sample.len())
        .map_or(character, |b| char::from(*b))
}

fn push(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => spans.push(Span::styled(text.to_owned(), style)),
    }
}

fn cells(character: char) -> usize {
    unicode_width::UnicodeWidthChar::width(character).unwrap_or(0)
}

/// How a split-flap pass looks.
#[derive(Clone, Copy)]
pub(super) struct Look {
    /// The colour a settled character glows in.
    pub glow: Color,
    /// Flip a few tiles just ahead of the text, or after it once everything
    /// has arrived, to show that more is coming.
    pub runway: bool,
}

/// The frame of a flip that started at `phase`.
fn frame(now: Instant, phase: Instant) -> usize {
    usize::try_from(now.saturating_duration_since(phase).as_millis() / FRAME.as_millis())
        .unwrap_or(usize::MAX)
}

/// Plays the split-flap over rendered rows. Visible character `i` (see
/// [`glyphs`]) arrives at `arrival(i)`; until then its cells stay blank.
pub(super) fn animate(
    lines: &mut [Line<'static>],
    arrival: impl Fn(usize) -> Option<Instant>,
    now: Instant,
    phase: Instant,
    look: Look,
    width: u16,
) {
    let sample = sample(glyphs(lines).into_iter());
    let frame = frame(now, phase);
    let mut glyph = 0;
    let mut runway = if look.runway { RUNWAY } else { 0 };
    let mut pending = false;
    let mut last_row = None;
    for (row, line) in lines.iter_mut().enumerate() {
        let mut spans = Vec::new();
        for span in std::mem::take(&mut line.spans) {
            for character in span.content.chars() {
                if character.is_whitespace() {
                    push(&mut spans, character.encode_utf8(&mut [0; 4]), span.style);
                    continue;
                }
                let index = glyph;
                glyph += 1;
                let Some(arrived) = arrival(index).filter(|at| *at <= now) else {
                    pending = true;
                    if runway > 0 && cells(character) == 1 && !sample.is_empty() {
                        runway -= 1;
                        let tile = tile(&sample, character, index, frame);
                        push(
                            &mut spans,
                            tile.encode_utf8(&mut [0; 4]),
                            span.style.fg(DIM),
                        );
                    } else {
                        push(&mut spans, &" ".repeat(cells(character)), span.style);
                    }
                    continue;
                };
                last_row = Some(row);
                let elapsed = now.saturating_duration_since(arrived);
                if elapsed < BLANK {
                    push(&mut spans, &" ".repeat(cells(character)), span.style);
                } else if elapsed < SETTLE {
                    if character.is_ascii_alphanumeric() {
                        let tile = tile(&sample, character, index, frame);
                        push(
                            &mut spans,
                            tile.encode_utf8(&mut [0; 4]),
                            span.style.fg(DIM),
                        );
                    } else if character.is_ascii_punctuation() {
                        push(&mut spans, " ", span.style);
                    } else {
                        push(
                            &mut spans,
                            character.encode_utf8(&mut [0; 4]),
                            span.style.fg(DIM),
                        );
                    }
                } else if elapsed < LIFETIME {
                    push(
                        &mut spans,
                        character.encode_utf8(&mut [0; 4]),
                        span.style.fg(look.glow),
                    );
                } else {
                    push(&mut spans, character.encode_utf8(&mut [0; 4]), span.style);
                }
            }
        }
        line.spans = spans;
    }
    // Everything so far has arrived; flip a few tiles after it.
    if runway == RUNWAY
        && look.runway
        && !pending
        && !sample.is_empty()
        && let Some(line) = last_row.and_then(|row| lines.get_mut(row))
        && line.width() + RUNWAY < usize::from(width)
    {
        let tiles: String = (0..RUNWAY)
            .filter_map(|index| {
                sample
                    .get((frame + index) % sample.len())
                    .map(|b| char::from(*b))
            })
            .collect();
        let after = line
            .spans
            .iter()
            .rposition(|span| !span.content.trim_end().is_empty())
            .map_or(0, |index| index + 1);
        line.spans.insert(
            after,
            Span::styled(format!(" {tiles}"), Style::default().fg(DIM)),
        );
    }
}

/// The split-flap in reverse: visible character `i` flips at `departure(i)`
/// and then leaves a blank cell.
pub(super) fn vanish(
    lines: &mut [Line<'static>],
    departure: impl Fn(usize) -> Instant,
    now: Instant,
    phase: Instant,
) {
    let sample = sample(glyphs(lines).into_iter());
    let frame = frame(now, phase);
    let mut glyph = 0;
    for line in lines.iter_mut() {
        let mut spans = Vec::new();
        for span in std::mem::take(&mut line.spans) {
            for character in span.content.chars() {
                if character.is_whitespace() {
                    push(&mut spans, character.encode_utf8(&mut [0; 4]), span.style);
                    continue;
                }
                let index = glyph;
                glyph += 1;
                let departed = departure(index);
                if now < departed {
                    push(&mut spans, character.encode_utf8(&mut [0; 4]), span.style);
                } else if now.saturating_duration_since(departed) < SETTLE
                    && character.is_ascii_alphanumeric()
                {
                    let tile = tile(&sample, character, index, frame);
                    push(
                        &mut spans,
                        tile.encode_utf8(&mut [0; 4]),
                        span.style.fg(DIM),
                    );
                } else {
                    push(&mut spans, &" ".repeat(cells(character)), span.style);
                }
            }
        }
        line.spans = spans;
    }
}

/// A label whose changed characters flip while unchanged ones stay put, like
/// a departure board: a ticking counter flips only its new digits.
#[derive(Default)]
pub(super) struct Board {
    glyphs: Vec<char>,
    arrivals: Vec<Instant>,
    phase: Option<Instant>,
}
impl Board {
    pub(super) fn show(
        &mut self,
        lines: &mut [Line<'static>],
        now: Instant,
        glow: Color,
        width: u16,
    ) {
        let glyphs = glyphs(lines);
        if glyphs != self.glyphs {
            let mut changed = 0;
            self.arrivals = glyphs
                .iter()
                .enumerate()
                .map(|(index, character)| {
                    match (self.glyphs.get(index), self.arrivals.get(index)) {
                        (Some(old), Some(at)) if old == character => *at,
                        _ => {
                            changed += 1;
                            now + stagger(changed - 1)
                        }
                    }
                })
                .collect();
            self.glyphs = glyphs;
        }
        let phase = *self.phase.get_or_insert(now);
        let arrivals = &self.arrivals;
        animate(
            lines,
            |index| arrivals.get(index).copied(),
            now,
            phase,
            Look {
                glow,
                runway: false,
            },
            width,
        );
    }
}

#[cfg(test)]
impl Board {
    /// Moves every arrival back, as if `by` had passed.
    pub(super) fn age(&mut self, by: Duration) {
        for arrival in &mut self.arrivals {
            *arrival = arrival.checked_sub(by).unwrap_or(*arrival);
        }
    }
}

pub(super) struct Flap {
    role: Role,
    text: String,
    /// When each visible character arrived, in order.
    arrivals: Vec<Instant>,
    updated: Instant,
    phase: Instant,
}

impl Flap {
    pub(super) fn new(role: Role, now: Instant) -> Self {
        Self {
            role,
            text: String::new(),
            arrivals: Vec::new(),
            updated: now,
            phase: now,
        }
    }
    /// Replaces the text. Characters already shown keep their timing when the
    /// new text extends the old; new ones arrive in a short stagger.
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
        let total = text.chars().filter(|c| !c.is_whitespace()).count();
        let existing = self.arrivals.len().min(total);
        self.arrivals.truncate(existing);
        for index in 0..total - existing {
            self.arrivals.push(start + stagger(index));
        }
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
        let mut lines: Vec<Line<'static>> = textwrap::wrap(
            &self.text,
            textwrap::Options::new(usize::from(width.max(1))),
        )
        .into_iter()
        .map(|row| Line::from(Span::styled(row.into_owned(), board)))
        .collect();
        let glow = match self.role {
            Role::User => ACCENT,
            Role::Assistant => PURPLE,
        };
        let runway = self.animating(now)
            && self
                .arrivals
                .first()
                .is_some_and(|first| now.saturating_duration_since(*first) >= SETTLE);
        animate(
            &mut lines,
            |index| self.arrivals.get(index).copied(),
            now,
            self.phase,
            Look { glow, runway },
            width,
        );
        for line in &mut lines {
            let padding = usize::from(width).saturating_sub(line.width());
            if padding > 0 {
                line.spans.push(Span::styled(" ".repeat(padding), board));
            }
        }
        lines
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
