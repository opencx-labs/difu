//! Lazy transcript layout. Retain row counts for navigation, but format/cache only
//! the messages near the viewport. Source history remains in the session store.
use super::*;

#[derive(Default)]
pub(super) struct Window {
    pub appearance: crate::settings::Appearance,
    pub conversation: crate::settings::Conversation,
    session: String,
    width: u16,
    last_position: usize,
    sources: HashMap<String, Source>,
    heights: HashMap<String, usize>,
    rows: HashMap<String, Vec<Line<'static>>>,
    links: HashMap<String, Vec<transcript::Link>>,
    #[cfg(test)]
    pub formatted: usize,
}
#[derive(PartialEq)]
struct Source {
    appearance: crate::settings::Appearance,
    conversation: crate::settings::Conversation,
    entry: Entry,
    expanded: bool,
    focused: bool,
    // Only unfinished timed entries depend on session state/the clock.
    timer: Option<(Status, i64)>,
}
impl Source {
    fn matches(
        &self,
        entry: &Entry,
        position: &Position,
        focused: bool,
        timer: Option<(Status, i64)>,
        appearance: &crate::settings::Appearance,
        conversation: &crate::settings::Conversation,
    ) -> bool {
        self.appearance == *appearance
            && self.conversation == *conversation
            && self.entry == *entry
            && self.expanded == position.expanded.contains(&entry.id)
            && self.focused == (focused && position.focused_entry.as_ref() == Some(&entry.id))
            && self.timer == timer
    }
}
pub(super) struct View {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<transcript::Link>,
    pub start: usize,
    pub total: usize,
    pub sections: Vec<transcript::Section>,
}
impl Window {
    fn sections(&self, entries: &[&Entry]) -> (Vec<transcript::Section>, usize) {
        let mut row = 0;
        let sections = entries
            .iter()
            .map(|entry| {
                let section = transcript::Section {
                    id: entry.id.clone(),
                    row,
                    tool: transcript::tool(entry),
                };
                row += self.heights.get(&entry.id).copied().unwrap_or(1);
                section
            })
            .collect();
        (sections, row)
    }
    pub fn render(
        &mut self,
        session: &Session,
        position: &mut Position,
        width: u16,
        height: u16,
        focused: bool,
    ) -> View {
        if self.session != session.id || self.width != width {
            self.session.clone_from(&session.id);
            self.width = width;
            self.heights.clear();
            self.rows.clear();
            self.links.clear();
            self.sources.clear();
        }
        let outgoing = position
            .outgoing
            .iter()
            .filter(|pending| pending.in_chat(session))
            .map(|pending| Entry {
                id: pending.entry_id(),
                kind: if pending.failed.is_some() {
                    "unsent"
                } else {
                    "sending"
                }
                .into(),
                text: pending.text.clone().into(),
                timestamp: None,
                data: serde_json::json!({"difuRetry":pending.failed.is_some()}).into(),
                ..Entry::default()
            })
            .collect::<Vec<_>>();
        let visible = session
            .entries
            .iter()
            .chain(&outgoing)
            .filter(|entry| transcript::visible(session, entry))
            .collect::<Vec<_>>();
        let entries: HashMap<_, _> = visible.iter().map(|&e| (e.id.as_str(), e)).collect();
        let (mut sections, _) = self.sections(&visible);
        let mut total;
        if sections.is_empty() {
            return View {
                lines: Vec::new(),
                links: Vec::new(),
                start: 0,
                total: 0,
                sections,
            };
        }
        let follow = match self.conversation.auto_scroll {
            crate::settings::AutoScroll::WhenAtBottom => position.follow,
            crate::settings::AutoScroll::Always => true,
            crate::settings::AutoScroll::Manual => false,
        } && !position.keep_transcript_position;
        // Capture the source message before measuring new rows. Discovering older
        // content must never move the text already visible at the top of the pane.
        let anchor = position
            .scroll_anchor
            .as_ref()
            .map(|(id, offset)| (id.clone(), *offset as isize, true))
            .or_else(|| {
                let section = sections
                    .iter()
                    .rev()
                    .find(|s| s.row <= position.conversation)?;
                // A page/wheel step can enter an unmeasured message. Count backwards
                // from the next measured boundary so its estimated one-row height
                // cannot turn a small scroll into a jump over its actual contents.
                if position.conversation > self.last_position
                    && !self.heights.contains_key(&section.id)
                    && let Some(previous) = sections
                        .iter()
                        .rev()
                        .find(|s| s.row < position.conversation && self.heights.contains_key(&s.id))
                {
                    return Some((
                        previous.id.clone(),
                        position.conversation.saturating_sub(previous.row) as isize,
                        false,
                    ));
                }
                if position.conversation > 0
                    && !self.heights.contains_key(&section.id)
                    && let Some(next) = sections
                        .iter()
                        .find(|s| s.row > position.conversation && self.heights.contains_key(&s.id))
                {
                    return Some((
                        next.id.clone(),
                        position.conversation as isize - next.row as isize,
                        false,
                    ));
                }
                Some((
                    section.id.clone(),
                    position.conversation.saturating_sub(section.row) as isize,
                    true,
                ))
            });
        let center = if follow {
            sections.len() - 1
        } else {
            anchor
                .as_ref()
                .and_then(|(id, _, _)| sections.iter().position(|s| &s.id == id))
                .unwrap_or(0)
        };
        let mut first = if follow {
            sections.len().saturating_sub(10)
        } else {
            center.saturating_sub(2)
        };
        let mut end = (first + 10).min(sections.len());
        loop {
            for section in sections.get(first..end).unwrap_or_default() {
                let Some(entry) = entries.get(section.id.as_str()).copied() else {
                    continue;
                };
                let timer =
                    (entry.started_at.is_some() && entry.finished_at.is_none()).then(|| {
                        (
                            session.status,
                            if session.status.active() {
                                chrono::Utc::now().timestamp_millis() / 100
                            } else {
                                session.updated
                            },
                        )
                    });
                if !self.sources.get(&section.id).is_some_and(|source| {
                    source.matches(
                        entry,
                        position,
                        focused,
                        timer,
                        &self.appearance,
                        &self.conversation,
                    )
                }) {
                    let (rows, _, links) = transcript::render_entries_with_preferences(
                        session,
                        std::slice::from_ref(entry),
                        position,
                        width,
                        focused,
                        &self.appearance,
                        &self.conversation,
                    );
                    self.sources.insert(
                        section.id.clone(),
                        Source {
                            appearance: self.appearance.clone(),
                            conversation: self.conversation.clone(),
                            entry: entry.clone(),
                            expanded: position.expanded.contains(&entry.id),
                            focused: focused && position.focused_entry.as_ref() == Some(&entry.id),
                            timer,
                        },
                    );
                    self.heights.insert(section.id.clone(), rows.len());
                    self.rows.insert(section.id.clone(), rows);
                    self.links.insert(section.id.clone(), links);
                    #[cfg(test)]
                    {
                        self.formatted += 1;
                    }
                }
            }
            (sections, total) = self.sections(&visible);
            position.conversation = if follow {
                total.saturating_sub(usize::from(height))
            } else {
                anchor
                    .as_ref()
                    .and_then(|(id, offset, clamp)| {
                        sections.iter().find(|s| &s.id == id).map(|s| {
                            if !clamp || *offset < 0 {
                                s.row.saturating_add_signed(*offset)
                            } else {
                                s.row
                                    + (*offset as usize).min(
                                        self.heights
                                            .get(id)
                                            .copied()
                                            .unwrap_or(1)
                                            .saturating_sub(1),
                                    )
                            }
                        })
                    })
                    .unwrap_or(position.conversation)
            };
            position.conversation = position.conversation.min(total.saturating_sub(
                if position.keep_transcript_position {
                    1
                } else {
                    usize::from(height)
                },
            ));
            if first > 0
                && sections
                    .get(first)
                    .is_some_and(|s| s.row > position.conversation)
            {
                first = first.saturating_sub(10);
                continue;
            }
            let bottom = sections.get(end).map_or(total, |s| s.row);
            if end < sections.len()
                && bottom < position.conversation.saturating_add(usize::from(height))
            {
                end = (end + 10).min(sections.len());
                continue;
            }
            break;
        }
        let maximum = total.saturating_sub(if position.keep_transcript_position {
            1
        } else {
            usize::from(height)
        });
        position.conversation = position.conversation.min(maximum);
        position.scroll_anchor = sections
            .iter()
            .rev()
            .find(|s| s.row <= position.conversation)
            .map(|s| (s.id.clone(), position.conversation - s.row));
        self.last_position = position.conversation;
        let keep: HashSet<_> = sections
            .get(first..end)
            .unwrap_or_default()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        self.sources.retain(|id, _| keep.contains(id.as_str()));
        self.rows.retain(|id, _| keep.contains(id.as_str()));
        self.links.retain(|id, _| keep.contains(id.as_str()));
        let links = sections
            .get(first..end)
            .unwrap_or_default()
            .iter()
            .flat_map(|section| {
                self.links
                    .get(&section.id)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .map(move |mut link| {
                        link.row += section.row;
                        link
                    })
            })
            .collect();
        let lines = sections
            .get(first..end)
            .unwrap_or_default()
            .iter()
            .flat_map(|s| self.rows.get(&s.id).into_iter().flatten().cloned())
            .collect();
        View {
            lines,
            links,
            start: sections.get(first).map_or(0, |s| s.row),
            total,
            sections,
        }
    }
}
