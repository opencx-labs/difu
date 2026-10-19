//! Chat animations from `/tweaks`, played as Codex's split-flap: replies flip
//! in at their speed, sent and queued messages flip in over their time, and the
//! composer's text flips away after sending. None takes longer than the cap.
//! The flip keeps every character's cell, so wrapping and heights never change.
use super::flap::{self, Look};
use super::*;
use crate::ui::PURPLE;
use std::ops::Range;

/// Progress of the newest reply. `shown` is infinite once it has settled.
#[derive(Clone, Debug)]
pub(super) struct Reply {
    entry: String,
    shown: f64,
    at: Instant,
    started: Instant,
    /// When each shown character arrived.
    arrivals: Vec<Instant>,
}
impl Reply {
    fn shown(entry: &str, at: Instant) -> Self {
        Self {
            entry: entry.to_owned(),
            shown: f64::INFINITY,
            at,
            started: at,
            arrivals: Vec::new(),
        }
    }
}

/// A message flipping into the chat or the queue, or the composer's text
/// flipping away after a send, over a fixed time.
#[derive(Clone, Debug)]
pub(super) struct Typing {
    text: String,
    started: Instant,
    duration: Duration,
}
impl Typing {
    pub(super) fn start(text: &str, duration_ms: u32, now: Instant) -> Option<Self> {
        (duration_ms > 0 && !text.is_empty()).then(|| Self {
            text: text.to_owned(),
            started: now,
            duration: Duration::from_millis(u64::from(duration_ms)),
        })
    }
    /// When character `index` of `total` takes its turn.
    fn at(&self, index: usize, total: usize) -> Instant {
        self.started + self.duration.mul_f64(index as f64 / total.max(1) as f64)
    }
    fn done(&self, now: Instant) -> bool {
        now >= self.started + self.duration + flap::LIFETIME
    }
    /// Flips this message's rows in, first character first.
    fn flip(&self, lines: &mut [Line<'static>], now: Instant, width: u16) {
        let total = flap::glyphs(lines).len();
        flap::animate(
            lines,
            |index| Some(self.at(index, total)),
            now,
            self.started,
            Look {
                glow: ACCENT,
                runway: false,
            },
            width,
        );
    }
    /// Flips this text's rows away, last character first.
    pub(super) fn vanish(&self, lines: &mut [Line<'static>], now: Instant) {
        let total = flap::glyphs(lines).len();
        flap::vanish(
            lines,
            |index| self.at(total.saturating_sub(index + 1), total),
            now,
            self.started,
        );
    }
    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

impl Position {
    pub(super) fn animating(&self) -> bool {
        let now = Instant::now();
        self.sent.is_some()
            || self.clearing.is_some()
            || !self.voice_flaps.is_empty()
            || self.reply.as_ref().is_some_and(|r| r.shown.is_finite())
            || self.activity.animating(now)
            || self.waiting.animating(now)
            || self.boards.values().any(|board| board.animating(now))
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

/// Advance this frame's animations for the drawn session: flip in its newest
/// reply and just-sent message. Replies already present when a session is
/// first drawn are shown as they are.
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
    // Spoken replies flip in from their own transcript timing.
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
            .map_or_else(
                || entry.text.chars().filter(|c| !c.is_whitespace()).count(),
                |lines| flap::glyphs(lines).len(),
            ) as f64;
        let reply = match &mut position.reply {
            Some(reply) if reply.entry == entry.id => reply,
            reply => reply.insert(Reply {
                entry: entry.id.clone(),
                shown: 0.0,
                at: now,
                started: now,
                arrivals: Vec::new(),
            }),
        };
        if reply.shown.is_finite() {
            let (from, elapsed) = (reply.at, now.saturating_duration_since(reply.at));
            let behind = received - reply.shown;
            // The cap bounds how far a reply may trail what has been received.
            let catch_up = if tweaks.max_animation_ms == 0 {
                0.0
            } else {
                behind / (f64::from(tweaks.max_animation_ms) / 1000.0)
            };
            let rate = f64::from(tweaks.agent_reply_cps).max(catch_up);
            let before = reply.shown;
            reply.shown = (reply.shown + rate * elapsed.as_secs_f64()).min(received);
            reply.at = now;
            // Each newly shown character arrived when the reveal passed it.
            while (reply.arrivals.len() as f64) < reply.shown.floor() {
                let passed = (reply.arrivals.len() as f64 + 1.0 - before) / rate.max(1.0);
                reply
                    .arrivals
                    .push(from + elapsed.min(Duration::from_secs_f64(passed.max(0.0))));
            }
            let streaming = entry.finished_at.is_none() && session.status.active();
            let settled = reply
                .arrivals
                .last()
                .is_none_or(|arrived| now >= *arrived + flap::LIFETIME);
            if reply.shown >= received && !streaming && settled {
                reply.shown = f64::INFINITY;
                reply.arrivals = Vec::new();
            } else if let Some(lines) = range.and_then(|range| lines.get_mut(range)) {
                let arrivals = &reply.arrivals;
                flap::animate(
                    lines,
                    |index| arrivals.get(index).copied(),
                    now,
                    reply.started,
                    Look {
                        glow: PURPLE,
                        runway: streaming || reply.shown < received,
                    },
                    width,
                );
            }
        }
    }
    let Some(sent) = &position.sent else {
        return;
    };
    if sent.done(now) {
        position.sent = None;
        return;
    }
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
        // A failed send shows its retry label at once instead of flipping in.
        .filter(|pending| pending.text == sent.text && pending.failed.is_none())
        .map(PendingSend::entry_id)
        .chain(newest)
        .filter_map(|id| rows(sections, start, total, available, &id))
        .max_by_key(|range| range.start);
    if let Some(lines) = range.and_then(|range| lines.get_mut(range)) {
        sent.flip(lines, now, width);
    }
}

/// Chat entries whose flip this module plays: the newest reply, which flips in
/// at the reply speed, and the copies of a message being sent.
pub(super) fn owned(position: &Position, session: &Session) -> HashSet<String> {
    let mut owned: HashSet<String> = session
        .entries
        .iter()
        .rev()
        .find(|entry| entry.kind == "agentMessage" && entry.data.get("difuVoice").is_none())
        .map(|entry| entry.id.clone())
        .into_iter()
        .collect();
    if let Some(sent) = &position.sent {
        owned.extend(
            position
                .outgoing
                .iter()
                .filter(|pending| pending.text == sent.text)
                .map(PendingSend::entry_id),
        );
        owned.extend(
            session
                .entries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.kind.as_str(),
                        "userMessage" | "sending" | "awaiting connection"
                    ) && entry.text.as_str() == sent.text
                })
                .map(|entry| entry.id.clone()),
        );
    }
    owned
}

/// The composer text still flipping away after a send.
pub(super) fn clearing(position: &mut Position, now: Instant) -> Option<Typing> {
    // Typing during the clear finishes it, so input is never blocked.
    if !position.draft.chars.is_empty()
        || position
            .clearing
            .as_ref()
            .is_some_and(|typing| typing.done(now))
    {
        position.clearing = None;
    }
    position.clearing.clone()
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
    fn history_shows_at_once_and_new_replies_flip_in_then_settle() {
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

        let reply = "word ".repeat(80).trim_end().to_owned();
        session.note("agentMessage", reply.clone());
        let words = |lines: &[String]| lines.last().map_or(0, |l| l.matches("word").count());
        let first = frame(&mut position, &session, tweaks, t0);
        assert_eq!(words(&first), 0, "nothing has arrived yet");
        assert_eq!(
            first.last().map(|l| l.chars().count()),
            Some(reply.chars().count()),
            "hidden characters keep their cells"
        );
        assert!(
            first.last().is_some_and(|l| !l.trim().is_empty()),
            "a runway flips ahead of the text"
        );
        assert!(position.animating());
        // 320 letters arrived at once: the 500 ms cap brings them all in
        // faster than the 200 characters per second base speed.
        let caught_up = frame(
            &mut position,
            &session,
            tweaks,
            t0 + Duration::from_millis(600),
        );
        let settled = frame(&mut position, &session, tweaks, t0 + Duration::from_secs(1));
        assert!(words(&caught_up) < 80 && words(&settled) == 80);
        assert!(
            position.animating(),
            "the runway waits while the reply streams"
        );

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
    fn letters_flip_through_the_text_then_glow_and_settle() {
        let t0 = Instant::now();
        let original = vec![Line::from(Span::styled(
            "› hello",
            Style::default().fg(TEXT),
        ))];
        let look = Look {
            glow: ACCENT,
            runway: false,
        };
        let at = |offset: u64, lines: &[Line<'static>]| {
            let mut lines = lines.to_vec();
            flap::animate(
                &mut lines,
                |_| Some(t0),
                t0 + Duration::from_millis(offset),
                t0,
                look,
                80,
            );
            lines
        };
        assert_eq!(text(&at(10, &original)), ["       "], "blank at first");
        let flipping = at(100, &original);
        let letters: String = text(&flipping).concat();
        assert!(
            letters.chars().skip(2).all(|c| "helo".contains(c)),
            "{letters}"
        );
        assert!(
            flipping
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.fg == Some(DIM))
        );
        let glowing = at(250, &original);
        assert_eq!(text(&glowing), ["› hello"]);
        assert!(
            glowing
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.fg == Some(ACCENT))
        );
        assert_eq!(
            at(400, &original),
            original,
            "settled text keeps its own style"
        );
    }

    #[test]
    fn sent_messages_flip_in_and_the_composer_flips_away_within_the_cap() {
        let tweaks = Tweaks::default();
        let t0 = Instant::now();
        let mut session = session(Status::Idle);
        session.note("userMessage", "abcdefghij");
        // 1000 ms each, capped to 500 ms.
        let mut position = Position {
            replies_seen: true,
            sent: Typing::start("abcdefghij", tweaks.capped(tweaks.sent_message_ms), t0),
            clearing: Typing::start("abcdefghij", tweaks.capped(tweaks.input_clear_ms), t0),
            ..Position::default()
        };
        let half = t0 + Duration::from_millis(250);
        let shown = frame(&mut position, &session, tweaks, half);
        assert!(
            shown.first().is_some_and(|row| row.ends_with("     ")),
            "the end has not arrived: {shown:?}"
        );
        assert!(
            shown.first().is_some_and(|row| row.starts_with("ab")),
            "the start has settled: {shown:?}"
        );
        let vanishing = |position: &mut Position, at| {
            let clearing = clearing(position, at)?;
            let mut lines = vec![Line::from("abcdefghij")];
            clearing.vanish(&mut lines, at);
            Some(text(&lines).concat())
        };
        let gone = vanishing(&mut position, half).unwrap_or_default();
        assert!(gone.starts_with("abc") && gone.ends_with("  "), "{gone:?}");
        let end = t0 + Duration::from_millis(500) + flap::LIFETIME;
        assert_eq!(frame(&mut position, &session, tweaks, end), ["abcdefghij"]);
        assert!(vanishing(&mut position, end).is_none());
        assert!(!position.animating());

        // Typing during the clear ends it so input is never blocked.
        position.clearing = Typing::start("old draft", tweaks.input_clear_ms, t0);
        position.draft = Editor::from("n");
        assert!(clearing(&mut position, t0).is_none());
        assert!(position.clearing.is_none());
        assert!(
            Typing::start("text", 0, t0).is_none(),
            "0 turns animations off"
        );
        let uncapped = Tweaks {
            max_animation_ms: 0,
            ..tweaks
        };
        assert_eq!(uncapped.capped(1000), 1000);
    }

    #[test]
    fn a_label_flips_only_the_characters_that_change() {
        let t0 = Instant::now();
        let mut board = flap::Board::default();
        let row = |text: &str| {
            vec![Line::from(Span::styled(
                text.to_owned(),
                Style::default().fg(DIM),
            ))]
        };
        let mut first = row("Working (9s)");
        board.show(&mut first, t0, ACCENT, 80);
        assert_eq!(text(&first).concat().trim(), "", "a new label starts blank");
        let mut settled = row("Working (9s)");
        board.show(&mut settled, t0 + Duration::from_secs(1), ACCENT, 80);
        assert_eq!(text(&settled), ["Working (9s)"]);
        let later = t0 + Duration::from_secs(2);
        let mut ticked = row("Working (10s)");
        board.show(&mut ticked, later, ACCENT, 80);
        assert_eq!(
            text(&ticked).concat().get(..9),
            Some("Working ("),
            "unchanged characters stay put"
        );
        assert!(!text(&ticked).concat().contains("10s"));
    }
}
