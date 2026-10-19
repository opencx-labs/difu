//! Chat animations from `/tweaks`: replies and sent messages type in character
//! by character, and the composer clears gradually after sending. Masks keep
//! each hidden character's cell, so wrapping and heights never change.
use super::*;
use ratatui::style::Color;
use std::ops::Range;

/// The newest characters of a reply ramp in from the dim text colour.
const FADE: usize = 4;
const CURSOR: char = '▌';

/// Progress of the newest reply. `shown` is infinite once it is fully shown.
#[derive(Clone, Debug)]
pub(super) struct Reply {
    entry: String,
    shown: f64,
    at: Instant,
}
impl Reply {
    fn shown(entry: &str, at: Instant) -> Self {
        Self {
            entry: entry.to_owned(),
            shown: f64::INFINITY,
            at,
        }
    }
}

/// A message typing into the chat, or the composer text clearing after a send.
#[derive(Clone, Debug)]
pub(super) struct Typing {
    text: String,
    started: Instant,
}
impl Typing {
    pub(super) fn start(text: &str, duration_ms: u32, now: Instant) -> Option<Self> {
        (duration_ms > 0 && !text.is_empty()).then(|| Self {
            text: text.to_owned(),
            started: now,
        })
    }
    /// Elapsed share of the animation, or None once it is finished.
    fn progress(&self, duration_ms: u32, now: Instant) -> Option<f64> {
        let fraction = now.saturating_duration_since(self.started).as_secs_f64()
            / (f64::from(duration_ms) / 1000.0);
        (fraction < 1.0).then_some(fraction)
    }
}

impl Position {
    pub(super) fn animating(&self) -> bool {
        self.sent.is_some()
            || self.clearing.is_some()
            || !self.voice_flaps.is_empty()
            || self.reply.as_ref().is_some_and(|r| r.shown.is_finite())
    }
}

/// Characters a row reveals: everything before its trailing whitespace, so
/// padding (such as a sent message's full-width background) shows immediately.
fn content(line: &Line) -> usize {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .trim_end()
        .chars()
        .count()
}

pub(super) fn count(lines: &[Line]) -> usize {
    lines.iter().map(content).sum()
}

fn push(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => spans.push(Span::styled(text.to_owned(), style)),
    }
}

/// `age` 1 is the newest character, closest to the dim colour. Terminal
/// palette colours (ANSI themes) cannot be blended and keep their style.
fn fade(style: Style, age: usize) -> Style {
    let (Color::Rgb(r, g, b), Color::Rgb(dr, dg, db)) = (
        crate::theme::resolve(style.fg.unwrap_or(TEXT)),
        crate::theme::resolve(DIM),
    ) else {
        return style;
    };
    let t = age as f32 / (FADE + 1) as f32;
    let mix = |from: u8, to: u8| (f32::from(from) + (f32::from(to) - f32::from(from)) * t) as u8;
    style.fg(Color::Rgb(mix(dr, r), mix(dg, g), mix(db, b)))
}

/// Show the first `shown` characters of `lines`. Hidden characters become blank
/// cells of the same width in the row's own style. With `edge`, the newest
/// characters fade in and a cursor marks where text is still arriving.
pub(super) fn mask(lines: &mut [Line<'static>], shown: usize, edge: bool, width: u16) {
    let total = count(lines);
    let mut index = 0;
    let mut last_row = None;
    for (row, line) in lines.iter_mut().enumerate() {
        let limit = content(line);
        if limit > 0 {
            last_row = Some(row);
        }
        let mut column = 0;
        let mut spans = Vec::new();
        for span in std::mem::take(&mut line.spans) {
            for character in span.content.chars() {
                let cells = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                if column >= limit {
                    push(&mut spans, character.encode_utf8(&mut [0; 4]), span.style);
                } else if index < shown {
                    let style = if edge && shown - index <= FADE {
                        fade(span.style, shown - index)
                    } else {
                        span.style
                    };
                    push(&mut spans, character.encode_utf8(&mut [0; 4]), style);
                    index += 1;
                } else {
                    if edge && index == shown && cells > 0 {
                        push(&mut spans, &CURSOR.to_string(), Style::default().fg(ACCENT));
                        push(&mut spans, &" ".repeat(cells - 1), Style::default());
                    } else {
                        push(&mut spans, &" ".repeat(cells), Style::default());
                    }
                    index += 1;
                }
                column += 1;
            }
        }
        line.spans = spans;
    }
    // Everything received so far is shown; the cursor waits after it.
    if edge
        && shown >= total
        && let Some(line) = last_row.and_then(|row| lines.get_mut(row))
        && line.width() < usize::from(width)
    {
        let trailing = line
            .spans
            .iter()
            .rposition(|span| !span.content.trim_end().is_empty())
            .map_or(0, |index| index + 1);
        line.spans.insert(
            trailing,
            Span::styled(CURSOR.to_string(), Style::default().fg(ACCENT)),
        );
    }
}

/// Rows of `id` within the rendered window, when all of them are present.
pub(super) fn rows(
    sections: &[transcript::Section],
    start: usize,
    total: usize,
    available: usize,
    id: &str,
) -> Option<Range<usize>> {
    let index = sections.iter().position(|s| s.id == id)?;
    let section = sections.get(index)?;
    let end = sections.get(index + 1).map_or(total, |next| next.row);
    let range = section.row.checked_sub(start)?..end.checked_sub(start)?;
    (range.start < range.end && range.end <= available).then_some(range)
}

/// Advance this frame's animations for the drawn session and mask its newest
/// reply and just-sent message. Replies already present when a session is first
/// drawn are shown as they are.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    position: &mut Position,
    session: &Session,
    sections: &[transcript::Section],
    start: usize,
    total: usize,
    lines: &mut [Line<'static>],
    tweaks: crate::storage::Tweaks,
    width: u16,
    now: Instant,
) {
    let available = lines.len();
    // Spoken replies use the split-flap reveal instead.
    let latest = session
        .entries
        .iter()
        .rev()
        .find(|entry| entry.kind == "agentMessage" && entry.data.get("difuVoice").is_none());
    if !std::mem::replace(&mut position.replies_seen, true) || tweaks.agent_reply_cps == 0 {
        position.reply = latest.map(|entry| Reply::shown(&entry.id, now));
    } else if let Some(entry) = latest {
        let range = rows(sections, start, total, available, &entry.id);
        let received = range
            .clone()
            .and_then(|range| lines.get(range))
            .map_or_else(|| entry.text.chars().count(), count) as f64;
        let reply = match &mut position.reply {
            Some(reply) if reply.entry == entry.id => reply,
            reply => reply.insert(Reply {
                entry: entry.id.clone(),
                shown: 0.0,
                at: now,
            }),
        };
        let elapsed = now.saturating_duration_since(reply.at).as_secs_f64();
        reply.at = now;
        // The cap bounds how far a reply may trail what has been received.
        let catch_up = if tweaks.max_animation_ms == 0 {
            0.0
        } else {
            (received - reply.shown) / (f64::from(tweaks.max_animation_ms) / 1000.0)
        };
        let rate = f64::from(tweaks.agent_reply_cps).max(catch_up);
        reply.shown = (reply.shown + rate * elapsed).min(received);
        let streaming = entry.finished_at.is_none() && session.status.active();
        if reply.shown >= received && !streaming {
            reply.shown = f64::INFINITY;
        } else if let Some(lines) = range.and_then(|range| lines.get_mut(range)) {
            mask(lines, reply.shown as usize, true, width);
        }
    }
    let Some(sent) = &position.sent else {
        return;
    };
    let Some(fraction) = sent.progress(tweaks.sent_message_ms, now) else {
        position.sent = None;
        return;
    };
    // The local copy is replaced by the provider's; animate whichever is newest.
    let newest = session
        .entries
        .iter()
        .rev()
        .find(|e| {
            matches!(
                e.kind.as_str(),
                "userMessage" | "sending" | "awaiting connection"
            ) && e.text.as_str() == sent.text
        })
        .map(|e| e.id.clone());
    let range = position
        .outgoing
        .iter()
        // A failed send shows its retry label at once instead of typing in.
        .filter(|pending| pending.text == sent.text && pending.failed.is_none())
        .map(PendingSend::entry_id)
        .chain(newest)
        .filter_map(|id| rows(sections, start, total, available, &id))
        .max_by_key(|range| range.start);
    if let Some(lines) = range.and_then(|range| lines.get_mut(range)) {
        let shown = (count(lines) as f64 * fraction).ceil() as usize;
        mask(lines, shown, false, width);
    }
}

/// The composer text still visible while it clears after a send.
pub(super) fn clearing(position: &mut Position, duration_ms: u32, now: Instant) -> Option<Editor> {
    // Typing during the clear finishes it, so input is never blocked.
    if !position.draft.chars.is_empty() {
        position.clearing = None;
    }
    let typing = position.clearing.as_ref()?;
    let Some(fraction) = typing.progress(duration_ms, now) else {
        position.clearing = None;
        return None;
    };
    let total = typing.text.chars().count();
    let keep = total - ((total as f64 * fraction) as usize).min(total);
    Some(Editor::from(
        typing.text.chars().take(keep).collect::<String>().as_str(),
    ))
}

#[cfg(test)]
impl Typing {
    pub(super) fn age(&mut self, by: std::time::Duration) {
        self.started -= by;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Tweaks;
    use std::time::Duration;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }
    fn session(status: Status) -> Session {
        let mut session = Session::new(
            "one".into(),
            Job::Coding(super::super::super::Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = status;
        session
    }
    /// One rendered row per entry, like a narrow transcript window.
    fn window(session: &Session) -> (Vec<transcript::Section>, Vec<Line<'static>>) {
        session
            .entries
            .iter()
            .enumerate()
            .map(|(row, entry)| {
                (
                    transcript::Section {
                        id: entry.id.clone(),
                        row,
                        tool: false,
                    },
                    Line::from(Span::styled(
                        entry.text.to_string(),
                        Style::default().fg(TEXT),
                    )),
                )
            })
            .unzip()
    }
    fn frame(
        position: &mut Position,
        session: &Session,
        tweaks: Tweaks,
        now: Instant,
    ) -> Vec<String> {
        let (sections, mut lines) = window(session);
        let total = lines.len();
        reveal_frame(position, session, &sections, total, &mut lines, tweaks, now);
        text(&lines)
    }
    fn reveal_frame(
        position: &mut Position,
        session: &Session,
        sections: &[transcript::Section],
        total: usize,
        lines: &mut [Line<'static>],
        tweaks: Tweaks,
        now: Instant,
    ) {
        apply(
            position, session, sections, 0, total, lines, tweaks, 80, now,
        );
    }

    #[test]
    fn masks_keep_every_cell_and_show_padding_cursor_and_fade() {
        let user = crate::ui::user_message_style();
        let original = vec![
            Line::from(vec![
                Span::styled("› 日本 ok", user),
                Span::styled("      ", user),
            ])
            .style(user),
            Line::from(Span::styled("second row", Style::default().fg(TEXT))),
        ];
        let width = |lines: &[Line]| lines.iter().map(Line::width).collect::<Vec<_>>();
        // A hidden wide character keeps both of its cells.
        let mut lines = original.clone();
        mask(&mut lines, 3, false, 80);
        assert_eq!(width(&lines), width(&original));
        assert_eq!(
            text(&lines),
            [format!("› 日{}", " ".repeat(11)), " ".repeat(10)]
        );
        // Row padding keeps the sent message's background; the line style stays.
        assert_eq!(
            lines.first().and_then(|l| l.spans.last()).map(|s| s.style),
            Some(user)
        );
        assert_eq!(lines.first().map(|l| l.style), Some(user));

        let mut lines = original.clone();
        mask(&mut lines, 9, true, 80);
        assert_eq!(width(&lines), width(&original));
        let row = lines.get(1).cloned().unwrap_or_default();
        assert_eq!(text(&lines).get(1).map(String::as_str), Some("se▌       "));
        let newest = row.spans.first().map(|s| s.style.fg);
        assert_ne!(newest, Some(Some(TEXT)), "newest characters fade in");
        assert!(
            row.spans
                .iter()
                .any(|s| s.content == "▌" && s.style.fg == Some(ACCENT))
        );

        let mut lines = original.clone();
        mask(&mut lines, count(&original), true, 80);
        assert_eq!(text(&lines).get(1).map(String::as_str), Some("second row▌"));
    }

    #[test]
    fn history_shows_at_once_and_new_replies_type_in_then_finish() {
        let tweaks = Tweaks::default();
        let t0 = Instant::now();
        let mut session = session(Status::Running);
        session.note("agentMessage", "Earlier reply");
        let mut position = Position::default();
        assert_eq!(
            frame(&mut position, &session, tweaks, t0),
            ["Earlier reply"]
        );
        assert!(!position.animating());

        let reply = "a".repeat(400);
        session.note("agentMessage", reply.clone());
        let shown = |lines: &[String]| lines.last().map_or(0, |l| l.matches('a').count());
        let first = frame(&mut position, &session, tweaks, t0);
        assert_eq!(shown(&first), 0);
        assert!(first.last().is_some_and(|l| l.starts_with('▌')));
        assert!(position.animating());
        // 400 characters arrived at once: catch-up shows all within the 0.5 s
        // cap, faster than the 200 characters per second base speed.
        let early = frame(
            &mut position,
            &session,
            tweaks,
            t0 + Duration::from_millis(200),
        );
        assert_eq!(shown(&early), 160);
        let caught_up = frame(&mut position, &session, tweaks, t0 + Duration::from_secs(1));
        assert_eq!(shown(&caught_up), 400);
        assert!(position.animating(), "cursor waits while the reply streams");

        if let Some(entry) = session.entries.last_mut() {
            entry.finished_at = Some(1);
        }
        session.status = Status::Idle;
        let done = frame(&mut position, &session, tweaks, t0 + Duration::from_secs(2));
        assert_eq!(done.last(), Some(&reply));
        assert!(!position.animating());

        let off = Tweaks {
            agent_reply_cps: 0,
            ..tweaks
        };
        session.status = Status::Running;
        session.note("agentMessage", "Shown at once");
        let still = frame(&mut position, &session, off, t0 + Duration::from_secs(3));
        assert_eq!(still.last().map(String::as_str), Some("Shown at once"));
    }

    #[test]
    fn sent_messages_type_in_and_the_composer_clears_over_their_durations() {
        let tweaks = Tweaks::default();
        let t0 = Instant::now();
        let mut session = session(Status::Idle);
        session.note("userMessage", "abcdefghij");
        let mut position = Position {
            replies_seen: true,
            sent: Typing::start("abcdefghij", tweaks.sent_message_ms, t0),
            clearing: Typing::start("abcdefghij", tweaks.input_clear_ms, t0),
            ..Position::default()
        };
        let half = t0 + Duration::from_millis(500);
        assert_eq!(frame(&mut position, &session, tweaks, half), ["abcde     "]);
        assert_eq!(
            clearing(&mut position, tweaks.input_clear_ms, half).map(|e| e.text()),
            Some("abcde".into())
        );
        let end = t0 + Duration::from_millis(1000);
        assert_eq!(frame(&mut position, &session, tweaks, end), ["abcdefghij"]);
        assert!(clearing(&mut position, tweaks.input_clear_ms, end).is_none());
        assert!(!position.animating());

        // Typing during the clear ends it so input is never blocked.
        position.clearing = Typing::start("old draft", tweaks.input_clear_ms, t0);
        position.draft = Editor::from("n");
        assert!(clearing(&mut position, tweaks.input_clear_ms, t0).is_none());
        assert!(position.clearing.is_none());
        assert!(
            Typing::start("text", 0, t0).is_none(),
            "0 turns animations off"
        );
    }
}
