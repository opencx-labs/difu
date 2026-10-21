//! Codex's split-flap reveal, used across the chat: each new character starts
//! blank, flips through letters from the same text, settles, then glows briefly
//! in the speaker's colour. It plays over already-rendered rows, so markdown
//! styles survive and hidden characters keep their cells: wrapping never moves.
//! In the chat, rows still waiting for their first character take no space, so
//! entries grow row by row. Timings match Codex's realtime transcript cells.
use super::{Entry, Position, Session, reveal, transcript};
use crate::ui::{ACCENT, DIM, PANEL, PURPLE, TEXT};
use crate::voice::Role;
use ratatui::{
    buffer::{Buffer, Cell, CellDiffOption},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::collections::HashSet;
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

/// How a character looks a while after it arrived.
enum Face {
    Blank,
    /// Flipping through letters from the text.
    Tile,
    /// Flipping, for characters with no letters to flip through.
    Dim,
    Glow,
    Rest,
}
fn face(character: char, elapsed: Duration) -> Face {
    if elapsed < BLANK {
        Face::Blank
    } else if elapsed < SETTLE {
        if character.is_ascii_alphanumeric() {
            Face::Tile
        } else if character.is_ascii_punctuation() {
            Face::Blank
        } else {
            Face::Dim
        }
    } else if elapsed < LIFETIME {
        Face::Glow
    } else {
        Face::Rest
    }
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
                        // Marked so [`grow`] can tell waiting rows from blank ones.
                        push(
                            &mut spans,
                            &" ".repeat(cells(character)),
                            span.style.add_modifier(Modifier::HIDDEN),
                        );
                    }
                    continue;
                };
                last_row = Some(row);
                let mut buffer = [0; 4];
                match face(character, now.saturating_duration_since(arrived)) {
                    Face::Blank => push(&mut spans, &" ".repeat(cells(character)), span.style),
                    Face::Tile => {
                        let tile = tile(&sample, character, index, frame);
                        push(
                            &mut spans,
                            tile.encode_utf8(&mut buffer),
                            span.style.fg(DIM),
                        );
                    }
                    Face::Dim => push(
                        &mut spans,
                        character.encode_utf8(&mut buffer),
                        span.style.fg(DIM),
                    ),
                    Face::Glow => push(
                        &mut spans,
                        character.encode_utf8(&mut buffer),
                        span.style.fg(look.glow),
                    ),
                    Face::Rest => push(&mut spans, character.encode_utf8(&mut buffer), span.style),
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

/// The character a screen cell shows, unless it is blank or part of an image:
/// image protocols mark their cells, write escapes or use Kitty placeholders.
fn cell_glyph(cell: &Cell) -> Option<char> {
    if cell.diff_option != CellDiffOption::None
        || cell
            .symbol()
            .chars()
            .any(|c| c == '\x1b' || c == '\u{10EEEE}')
    {
        return None;
    }
    cell.symbol().chars().next().filter(|c| !c.is_whitespace())
}

/// How long a whole screen takes to flip in, before the longest animation.
pub(super) const SCREEN_MS: u32 = 500;

/// Flips a whole drawn screen in, in reading order over `duration` from
/// `started`, as when switching tabs. Backgrounds and images show at once.
/// Returns whether the flip is still playing.
pub(super) fn screen(
    buffer: &mut Buffer,
    started: Instant,
    duration: Duration,
    now: Instant,
) -> bool {
    let glyphs: Vec<char> = buffer.content.iter().filter_map(cell_glyph).collect();
    let sample = sample(glyphs.iter().copied());
    let total = glyphs.len().max(1);
    let frame = frame(now, started);
    let mut index = 0;
    for cell in &mut buffer.content {
        let Some(character) = cell_glyph(cell) else {
            continue;
        };
        let arrived = started + duration.mul_f64(index as f64 / total as f64);
        index += 1;
        if now < arrived {
            cell.set_symbol(" ");
            continue;
        }
        match face(character, now.saturating_duration_since(arrived)) {
            Face::Blank => {
                cell.set_symbol(" ");
            }
            Face::Tile => {
                cell.set_char(tile(&sample, character, index, frame)).fg = DIM;
            }
            Face::Dim => cell.fg = DIM,
            Face::Glow => cell.fg = ACCENT,
            Face::Rest => {}
        }
    }
    now < started + duration + LIFETIME
}

/// How long a selection highlight takes to cross a sidebar card, and how
/// long each cell's flap takes to fall.
const HIGHLIGHT: Duration = Duration::from_millis(240);
const FLAP: Duration = Duration::from_millis(90);

/// A sidebar card's highlight flipping on or off, cell by cell in reading
/// order: each cell's flap falls with its top half in the new colour.
#[derive(Clone, Copy)]
pub(super) struct Highlight {
    on: bool,
    started: Instant,
    duration: Duration,
}
impl Highlight {
    pub(super) fn start(on: bool, now: Instant, tweaks: crate::storage::Tweaks) -> Option<Self> {
        let ms = u64::try_from(HIGHLIGHT.as_millis()).unwrap_or(u64::MAX);
        let duration = Duration::from_millis(u64::from(
            tweaks.capped(u32::try_from(ms).unwrap_or(u32::MAX)),
        ));
        (tweaks.agent_reply_cps > 0 && !duration.is_zero()).then_some(Self {
            on,
            started: now,
            duration,
        })
    }
    pub(super) fn done(&self, now: Instant) -> bool {
        now >= self.started + self.duration + FLAP
    }
    /// Plays over a card already drawn in its new colour, `highlight` being
    /// the selected background and `plain` the other.
    pub(super) fn apply(
        &self,
        buffer: &mut Buffer,
        area: ratatui::layout::Rect,
        highlight: Color,
        plain: Color,
        now: Instant,
    ) {
        let (from, to) = if self.on {
            (plain, highlight)
        } else {
            (highlight, plain)
        };
        let total = usize::from(area.width) * usize::from(area.height);
        for (index, position) in area.positions().enumerate() {
            let Some(cell) = buffer.cell_mut(position) else {
                continue;
            };
            let at = self.started + self.duration.mul_f64(index as f64 / total.max(1) as f64);
            if now < at {
                cell.bg = from;
            } else if now < at + FLAP {
                cell.set_symbol("▀");
                cell.fg = to;
                cell.bg = from;
            }
        }
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

/// [`glyphs`] row by row.
fn row_glyphs(lines: &[Line]) -> Vec<Vec<char>> {
    lines
        .iter()
        .map(|line| glyphs(std::slice::from_ref(line)))
        .collect()
}

/// How an item of new text lines up with the text it replaces.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pair {
    /// Unchanged: the same item as this old one.
    Same(usize),
    /// Changed in place of this old one.
    Replaced(usize),
    /// New.
    Inserted,
}

/// The most items [`align`] compares pairwise; larger changes pair by position.
const ALIGN_LIMIT: usize = 1 << 18;

/// Lines `new` up against `old` by their longest common subsequence, so text
/// that only moved keeps its place. Each changed item replaces an old one in
/// the same gap between unchanged items, in order, or is new.
fn align<T: PartialEq>(old: &[T], new: &[T]) -> Vec<Pair> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let (old_rest, new_rest) = (
        old.get(prefix..).unwrap_or_default(),
        new.get(prefix..).unwrap_or_default(),
    );
    let suffix = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_middle, new_middle) = (
        old_rest.get(..old_rest.len() - suffix).unwrap_or_default(),
        new_rest.get(..new_rest.len() - suffix).unwrap_or_default(),
    );
    let (rows, columns) = (old_middle.len(), new_middle.len());
    // The old item each new one equals, within the middle.
    let mut same: Vec<Option<usize>> = vec![None; columns];
    if rows.saturating_mul(columns) <= ALIGN_LIMIT {
        // `lengths[i * (columns + 1) + j]`: the common length of the tails
        // from old item `i` and new item `j`.
        let stride = columns + 1;
        let mut lengths = vec![0u32; (rows + 1) * stride];
        let length =
            |lengths: &[u32], i: usize, j: usize| lengths.get(i * stride + j).copied().unwrap_or(0);
        for (i, a) in old_middle.iter().enumerate().rev() {
            for (j, b) in new_middle.iter().enumerate().rev() {
                let value = if a == b {
                    length(&lengths, i + 1, j + 1) + 1
                } else {
                    length(&lengths, i + 1, j).max(length(&lengths, i, j + 1))
                };
                if let Some(cell) = lengths.get_mut(i * stride + j) {
                    *cell = value;
                }
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < rows && j < columns {
            if old_middle.get(i) == new_middle.get(j) {
                if let Some(cell) = same.get_mut(j) {
                    *cell = Some(i);
                }
                i += 1;
                j += 1;
            } else if length(&lengths, i + 1, j) >= length(&lengths, i, j + 1) {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    // The old index bounding each new item's gap: the next unchanged item's.
    let mut bounds = vec![rows; columns];
    let mut bound = rows;
    for (cell, matched) in bounds.iter_mut().zip(&same).rev() {
        if let Some(i) = matched {
            bound = *i;
        }
        *cell = bound;
    }
    let mut next = 0;
    let middle = same
        .iter()
        .zip(&bounds)
        .map(|(matched, bound)| match matched {
            Some(i) => {
                next = i + 1;
                Pair::Same(prefix + i)
            }
            None if next < *bound => {
                next += 1;
                Pair::Replaced(prefix + next - 1)
            }
            None => Pair::Inserted,
        });
    (0..prefix)
        .map(Pair::Same)
        .chain(middle.collect::<Vec<_>>())
        .chain((0..suffix).map(|index| Pair::Same(old.len() - suffix + index)))
        .collect()
}

/// Text whose changed characters flip while unchanged ones stay put, like a
/// departure board: a ticking counter flips only its new digits.
#[derive(Clone, Default)]
pub(super) struct Board {
    /// The visible characters of each row.
    rows: Vec<Vec<char>>,
    /// When each visible character arrived, in reading order.
    arrivals: Vec<Instant>,
    phase: Option<Instant>,
    /// When the newest flip has settled.
    until: Option<Instant>,
}
/// A digit that changed this recently updates in place rather than flipping
/// again, so fast counters such as tool timings stay readable.
const DIGIT_REFLIP: Duration = Duration::from_secs(1);
impl Board {
    /// Flips changes in Codex's short stagger: labels and notes.
    pub(super) fn show(
        &mut self,
        lines: &mut [Line<'static>],
        now: Instant,
        glow: Color,
        width: u16,
    ) {
        self.show_over(lines, now, glow, width, |changed| {
            stagger(changed.saturating_sub(1))
        });
    }
    /// Flips changes in reading order, spreading their arrivals over `spread`
    /// of how many characters changed.
    pub(super) fn show_over(
        &mut self,
        lines: &mut [Line<'static>],
        now: Instant,
        glow: Color,
        width: u16,
        spread: impl Fn(usize) -> Duration,
    ) {
        self.show_after(lines, now, now, glow, width, spread);
    }
    /// Like [`Board::show_over`], with changes waiting to flip until `after`.
    pub(super) fn show_after(
        &mut self,
        lines: &mut [Line<'static>],
        now: Instant,
        after: Instant,
        glow: Color,
        width: u16,
        spread: impl Fn(usize) -> Duration,
    ) {
        self.update(row_glyphs(lines), now, after.max(now), spread);
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
    /// Takes these rows as they are, without flipping them.
    pub(super) fn settle(&mut self, lines: &[Line<'static>], now: Instant) {
        let rested = now.checked_sub(LIFETIME).unwrap_or(now);
        let rows = row_glyphs(lines);
        if rows != self.rows {
            self.arrivals = vec![rested; rows.iter().map(Vec::len).sum()];
            self.rows = rows;
        }
    }
    pub(super) fn animating(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }
    /// When the last character to flip here arrives.
    pub(super) fn arrived(&self) -> Option<Instant> {
        self.until.and_then(|until| until.checked_sub(LIFETIME))
    }
    /// Whether nothing has been shown on this board yet.
    pub(super) fn is_empty(&self) -> bool {
        self.rows.iter().all(Vec::is_empty) && self.phase.is_none()
    }
    /// Lines the new rows up with the old ones, so characters that only moved,
    /// such as a tool's output under its finished heading, keep their arrival.
    /// Changed characters arrive from `start`.
    fn update(
        &mut self,
        rows: Vec<Vec<char>>,
        now: Instant,
        start: Instant,
        spread: impl Fn(usize) -> Duration,
    ) {
        if rows == self.rows {
            return;
        }
        let starts: Vec<usize> = self
            .rows
            .iter()
            .scan(0, |start, row| {
                let here = *start;
                *start += row.len();
                Some(here)
            })
            .collect();
        let arrival = |row: usize, column: usize| {
            starts
                .get(row)
                .and_then(|start| self.arrivals.get(start + column))
                .copied()
        };
        // Each new character's arrival when it stays put; `None` flips it.
        let mut kept: Vec<Option<Instant>> = Vec::new();
        for (characters, pair) in rows.iter().zip(align(&self.rows, &rows)) {
            match pair {
                Pair::Same(row) => {
                    kept.extend((0..characters.len()).map(|column| arrival(row, column)));
                }
                Pair::Replaced(row) => {
                    let old = self.rows.get(row).map_or(&[][..], Vec::as_slice);
                    for (character, pair) in characters.iter().zip(align(old, characters)) {
                        kept.push(match pair {
                            Pair::Same(column) => arrival(row, column),
                            Pair::Replaced(column) => old
                                .get(column)
                                .filter(|old| old.is_ascii_digit() && character.is_ascii_digit())
                                .and(arrival(row, column))
                                .filter(|at| now.saturating_duration_since(*at) < DIGIT_REFLIP),
                            Pair::Inserted => None,
                        });
                    }
                }
                Pair::Inserted => kept.extend(std::iter::repeat_n(None, characters.len())),
            }
        }
        let changed = kept.iter().filter(|kept| kept.is_none()).count();
        let total = spread(changed);
        let mut order = 0;
        let mut last = None;
        self.arrivals = kept
            .into_iter()
            .map(|kept| {
                kept.unwrap_or_else(|| {
                    let at = start + total.mul_f64(order as f64 / changed.max(1) as f64);
                    order += 1;
                    last = Some(at);
                    at
                })
            })
            .collect();
        if let Some(last) = last {
            self.until = Some(last + LIFETIME);
        }
        self.rows = rows;
    }
}

#[cfg(test)]
impl Board {
    /// Moves every arrival back, as if `by` had passed.
    pub(super) fn age(&mut self, by: Duration) {
        for arrival in self.arrivals.iter_mut().chain(self.until.as_mut()) {
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

/// How long `changed` characters of agent text take to arrive: the reply
/// speed, within the longest animation.
pub(super) fn paced(tweaks: crate::storage::Tweaks, changed: usize) -> Duration {
    let paced = Duration::from_secs_f64(changed as f64 / f64::from(tweaks.agent_reply_cps.max(1)));
    match tweaks.max_animation_ms {
        0 => paced,
        cap => paced.min(Duration::from_millis(u64::from(cap))),
    }
}

/// Flips every chat entry that appears or changes after the session was first
/// drawn: tool calls and their details, reasoning, plans, notes and messages.
/// Entries flip in reading order, each waiting for the ones above it, so a
/// batch of new entries plays one after another. Entries already there show as
/// they are. The newest reply, a message being sent and voice transcripts play
/// their own flips; returns when the newest reply's turn comes.
#[allow(clippy::too_many_arguments)]
pub(super) fn chat(
    position: &mut Position,
    session: &Session,
    sections: &[transcript::Section],
    start: usize,
    total: usize,
    lines: &mut [Line<'static>],
    tweaks: crate::storage::Tweaks,
    width: u16,
    now: Instant,
) -> Instant {
    let mut owned = reveal::owned(position, session);
    let mut users = HashSet::new();
    for entry in &session.entries {
        if voice_role(entry).is_some() {
            owned.insert(entry.id.clone());
        } else if matches!(
            entry.kind.as_str(),
            "userMessage"
                | "sending"
                | "awaiting connection"
                | "unsent"
                | "unsent or unacknowledged"
        ) {
            users.insert(entry.id.as_str());
        }
    }
    let opening = position.known.is_none();
    let known = position
        .known
        .get_or_insert_with(|| session.entries.iter().map(|e| e.id.clone()).collect());
    let cps = tweaks.agent_reply_cps;
    let spread = |changed| paced(tweaks, changed);
    let available = lines.len();
    let mut drawn = HashSet::new();
    let reply = reveal::latest_reply(session);
    // When everything above the current entry has arrived.
    let mut cursor = now;
    let mut reply_turn = now;
    for section in sections {
        let rows = reveal::rows(sections, start, total, available, &section.id)
            .and_then(|range| lines.get_mut(range));
        if let Some(reply) = reply.filter(|reply| reply.id == section.id)
            && cps > 0
            && !opening
        {
            reply_turn = cursor;
            let received = rows.as_deref().map_or_else(
                || reply.text.chars().filter(|c| !c.is_whitespace()).count(),
                |rows| glyphs(rows).len(),
            );
            let finish = match &position.reply {
                Some(shown) if shown.entry() == reply.id => shown.finish(received, tweaks, now),
                _ => Some(cursor + paced(tweaks, received)),
            };
            cursor = cursor.max(finish.unwrap_or(cursor));
        }
        let Some(rows) = rows else {
            continue;
        };
        drawn.insert(section.id.clone());
        let board = position.boards.entry(section.id.clone()).or_default();
        let fresh = board.is_empty();
        if owned.contains(&section.id)
            || cps == 0
            || opening
            || (fresh && known.contains(&section.id))
        {
            board.settle(rows, now);
        } else {
            let glow = if users.contains(section.id.as_str())
                || section.id.starts_with("difu-outgoing-")
            {
                ACCENT
            } else {
                PURPLE
            };
            board.show_after(rows, now, cursor, glow, width, spread);
            cursor = cursor.max(board.arrived().unwrap_or(cursor));
        }
        known.insert(section.id.clone());
    }
    // Entries out of view rest; coming back into view does not flip them.
    position.boards.retain(|id, _| drawn.contains(id));
    reply_turn
}

/// Whether a row shows text, only waits for text still to arrive, or is blank.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Shown,
    Waiting,
    Blank,
}
fn row(line: &Line) -> Row {
    if line
        .spans
        .iter()
        .any(|span| span.content.chars().any(|c| !c.is_whitespace()))
    {
        Row::Shown
    } else if line
        .spans
        .iter()
        .any(|span| !span.content.is_empty() && span.style.add_modifier.contains(Modifier::HIDDEN))
    {
        Row::Waiting
    } else {
        Row::Blank
    }
}

/// Drops chat rows whose characters have not begun to arrive, so each entry
/// grows row by row as it flips in and one waiting its turn takes no space.
/// Blank rows go with the text below them; an entry's trailing blank rows go
/// only while all of it waits. Shifts the sections and links below, and
/// returns how many dropped rows were above row `above`.
pub(super) fn grow(
    sections: &mut [transcript::Section],
    start: usize,
    total: &mut usize,
    lines: &mut Vec<Line<'static>>,
    links: &mut Vec<transcript::Link>,
    above: usize,
) -> usize {
    let mut dropped = vec![false; lines.len()];
    for (index, section) in sections.iter().enumerate() {
        let end = sections.get(index + 1).map_or(*total, |next| next.row);
        let Some(range) = section.row.checked_sub(start).zip(end.checked_sub(start)) else {
            continue;
        };
        let (Some(rows), Some(flags)) = (
            lines.get(range.0..range.1),
            dropped.get_mut(range.0..range.1),
        ) else {
            continue;
        };
        let kinds: Vec<Row> = rows.iter().map(row).collect();
        if !kinds.contains(&Row::Waiting) {
            continue;
        }
        let mut below = !kinds.contains(&Row::Shown);
        for (flag, kind) in flags.iter_mut().zip(&kinds).rev() {
            if *kind != Row::Blank {
                below = *kind == Row::Waiting;
            }
            *flag = below;
        }
    }
    let count = dropped.iter().filter(|dropped| **dropped).count();
    if count == 0 {
        return 0;
    }
    // Dropped rows before absolute row `row`.
    let sums: Vec<usize> = std::iter::once(0)
        .chain(dropped.iter().scan(0, |sum, dropped| {
            *sum += usize::from(*dropped);
            Some(*sum)
        }))
        .collect();
    let before = |row: usize| {
        sums.get(row.saturating_sub(start).min(dropped.len()))
            .copied()
            .unwrap_or(count)
    };
    for section in sections.iter_mut() {
        section.row -= before(section.row);
    }
    links.retain(|link| {
        !link
            .row
            .checked_sub(start)
            .and_then(|row| dropped.get(row))
            .is_some_and(|dropped| *dropped)
    });
    for link in links.iter_mut() {
        link.row -= before(link.row);
    }
    let mut flags = dropped.iter();
    lines.retain(|_| !flags.next().is_some_and(|dropped| *dropped));
    *total -= count;
    before(above)
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
        // Animate only the voice text; optional entry separators stay outside its board.
        if let Some(target) = lines
            .get_mut(range)
            .and_then(|rows| rows.get_mut(..animated.len()))
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
        for separators in [false, true] {
            let mut position = Position::default();
            let frame = |position: &mut Position, at: Instant| {
                let (mut lines, sections, _) = transcript::render_entries_with_appearance(
                    &session,
                    &session.entries,
                    position,
                    40,
                    false,
                    &crate::settings::Appearance {
                        message_separators: separators,
                        ..Default::default()
                    },
                );
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
            if separators {
                assert_eq!(first.matches('─').count(), 80);
            }
            assert!(!position.voice_flaps.contains_key("old"));
            let settled = frame(&mut position, start + Duration::from_secs(1));
            assert!(settled.contains("› hello there"), "{settled}");
        }
    }

    #[test]
    fn chat_entries_flip_in_when_new_and_flip_only_their_changes() {
        use crate::agents::{Job, Launch};
        let mut session = Session::new(
            "chat".into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Task".into(),
                model: None,
                effort: None,
            }),
        );
        session.note("system", "Earlier note");
        let frame = |position: &mut Position,
                     session: &Session,
                     tweaks: crate::storage::Tweaks,
                     at: Instant| {
            let (mut lines, sections) = transcript::render(session, position, 60, false);
            let total = lines.len();
            chat(
                position, session, &sections, 0, total, &mut lines, tweaks, 60, at,
            );
            plain(&lines)
        };
        let tweaks = crate::storage::Tweaks::default();
        let mut position = Position::default();
        let t0 = Instant::now();
        assert!(frame(&mut position, &session, tweaks, t0).contains("Earlier note"));
        session.note("error", "Build failed");
        let first = frame(&mut position, &session, tweaks, t0);
        assert!(first.contains("Earlier note"), "history stays put");
        assert!(!first.contains("Build failed"), "new text starts blank");
        assert!(position.animating());
        let t1 = t0 + Duration::from_secs(1);
        assert!(frame(&mut position, &session, tweaks, t1).contains("Build failed"));
        if let Some(entry) = session.entries.last_mut() {
            entry.text = "Build failed twice".to_owned().into();
        }
        let changed = frame(&mut position, &session, tweaks, t1);
        assert!(changed.contains("Build failed") && !changed.contains("twice"));
        let t2 = t1 + Duration::from_secs(1);
        assert!(frame(&mut position, &session, tweaks, t2).contains("Build failed twice"));
        // A reply speed of 0 turns the flip off.
        let off = crate::storage::Tweaks {
            agent_reply_cps: 0,
            ..tweaks
        };
        session.note("error", "Shown at once");
        assert!(frame(&mut position, &session, off, t2).contains("Shown at once"));
    }

    #[test]
    fn fast_counters_update_in_place_between_flips() {
        let row = |text: &str| vec![Line::from(text.to_owned())];
        let t0 = Instant::now();
        let mut board = Board::default();
        board.settle(&row("Ran ls · done · 1.2s"), t0);
        let mut ticked = row("Ran ls · done · 1.3s");
        board.show(&mut ticked, t0 + Duration::from_secs(2), ACCENT, 80);
        assert!(!plain(&ticked).contains("1.3s"), "a slow tick flips");
        // Within a second of that flip, the next tick changes in place.
        let mut fast = row("Ran ls · done · 1.4s");
        board.show(&mut fast, t0 + Duration::from_millis(2400), ACCENT, 80);
        assert!(plain(&fast).contains("1.4s"), "{}", plain(&fast));
        let mut later = row("Ran ls · done · 1.5s");
        board.show(&mut later, t0 + Duration::from_millis(3100), ACCENT, 80);
        assert!(
            !plain(&later).contains("1.5s"),
            "a second later it flips again"
        );
    }

    #[test]
    fn text_that_only_moved_keeps_its_place() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        assert_eq!(
            align(&chars("abc"), &chars("xacd")),
            vec![Pair::Inserted, Pair::Same(0), Pair::Same(2), Pair::Inserted]
        );
        assert_eq!(
            align(&chars("1.2s"), &chars("1.3s")),
            vec![
                Pair::Same(0),
                Pair::Same(1),
                Pair::Replaced(2),
                Pair::Same(3)
            ]
        );
        let rows = |rows: &[&str]| rows.iter().map(|row| chars(row)).collect::<Vec<_>>();
        assert_eq!(
            align(
                &rows(&["Running ls", "out"]),
                &rows(&["Ran ls", "out", "more"])
            ),
            vec![Pair::Replaced(0), Pair::Same(1), Pair::Inserted]
        );
    }

    #[test]
    fn a_finished_tool_flips_only_its_changed_heading() {
        let rows = |heading: &str| {
            vec![
                Line::from(heading.to_owned()),
                Line::from("  └ hello world"),
                Line::from("    second line"),
            ]
        };
        let t0 = Instant::now();
        let mut board = Board::default();
        board.settle(&rows("• Running ls · running · 1.2s"), t0);
        let mut done = rows("• Ran ls · done · 1.5s");
        board.show(&mut done, t0 + Duration::from_secs(2), ACCENT, 80);
        let text = plain(&done);
        assert!(
            text.contains("└ hello world") && text.contains("second line"),
            "the output stays put: {text}"
        );
        assert!(
            text.contains(" ls · ") && !text.contains("done"),
            "only the heading's changes flip: {text}"
        );
    }

    #[test]
    fn new_entries_flip_in_one_after_another_and_grow_as_they_arrive() {
        use crate::agents::{Job, Launch};
        let mut session = Session::new(
            "chat".into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Task".into(),
                model: None,
                effort: None,
            }),
        );
        session.note("system", "Earlier note");
        let tweaks = crate::storage::Tweaks {
            agent_reply_cps: 100,
            max_animation_ms: 0,
            ..Default::default()
        };
        let frame = |position: &mut Position, session: &Session, at: Instant| {
            let (mut lines, mut sections) = transcript::render(session, position, 60, false);
            let mut total = lines.len();
            chat(
                position, session, &sections, 0, total, &mut lines, tweaks, 60, at,
            );
            grow(&mut sections, 0, &mut total, &mut lines, &mut Vec::new(), 0);
            (plain(&lines), lines.len())
        };
        let mut position = Position::default();
        let t0 = Instant::now();
        let (_, before) = frame(&mut position, &session, t0);
        session.note("error", "alpha beta gamma");
        session.note("error", "delta");
        let (_, waiting) = frame(&mut position, &session, t0);
        assert_eq!(waiting, before, "entries take no space before their text");
        // The first note arrives over 140 ms, and the second waits for it.
        let (middle, _) = frame(&mut position, &session, t0 + Duration::from_millis(250));
        assert!(!middle.contains("delta"), "{middle}");
        let (done, _) = frame(&mut position, &session, t0 + Duration::from_secs(2));
        assert!(
            done.contains("alpha beta gamma") && done.contains("delta"),
            "{done}"
        );
    }

    #[test]
    fn rows_waiting_for_text_take_no_space() {
        let waiting = || {
            Line::from(Span::styled(
                "    ",
                Style::new().add_modifier(Modifier::HIDDEN),
            ))
        };
        let section = |id: &str, row| transcript::Section {
            id: id.into(),
            row,
            tool: false,
        };
        let mut sections = vec![
            section("old", 0),
            section("growing", 2),
            section("queued", 5),
            section("after", 7),
        ];
        let mut lines = vec![
            Line::from("old"),
            Line::default(),
            Line::from("partial"),
            waiting(),
            Line::default(),
            waiting(),
            Line::default(),
            Line::from("after"),
        ];
        let mut total = lines.len();
        let mut links = vec![transcript::Link {
            row: 7,
            column: 0,
            width: 5,
            url: "https://example.com".into(),
        }];
        let above = grow(&mut sections, 0, &mut total, &mut lines, &mut links, 8);
        assert_eq!(plain(&lines), "old\n\npartial\n\nafter");
        assert_eq!(
            sections.iter().map(|s| s.row).collect::<Vec<_>>(),
            vec![0, 2, 4, 4]
        );
        assert_eq!((total, above), (5, 3));
        assert_eq!(links.first().map(|link| link.row), Some(4));
    }

    #[test]
    fn a_switched_screen_flips_in_while_images_show_at_once() {
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 2));
        buffer.set_string(0, 0, "Agents tab", Style::default());
        if let Some(cell) = buffer.cell_mut((0, 1)) {
            cell.set_symbol("\u{10EEEE}");
        }
        let text = |buffer: &Buffer| buffer.content.iter().map(Cell::symbol).collect::<String>();
        let (t0, duration) = (Instant::now(), Duration::from_millis(500));
        let mut first = buffer.clone();
        assert!(screen(&mut first, t0, duration, t0));
        assert!(!text(&first).contains("Agents"), "{}", text(&first));
        assert!(text(&first).contains('\u{10EEEE}'), "images stay put");
        let mut settled = buffer.clone();
        assert!(!screen(
            &mut settled,
            t0,
            duration,
            t0 + Duration::from_secs(1)
        ));
        assert_eq!(text(&settled), text(&buffer));
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
