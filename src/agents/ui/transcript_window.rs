//! Lazy transcript layout. Retain row counts for navigation, but format/cache only
//! the messages near the viewport. Source history remains in the session store.
use super::*;

#[derive(Default)]
pub(super) struct Window {
    session: String,
    width: u16,
    last_position: usize,
    revision: Option<(u64, i64, Option<String>)>,
    heights: HashMap<String, usize>,
    rows: HashMap<String, Vec<Line<'static>>>,
    #[cfg(test)]
    pub formatted: usize,
}
pub(super) struct View {
    pub lines: Vec<Line<'static>>,
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
            self.revision = None;
        }
        let revision = (
            session.version,
            if session.status.active() {
                chrono::Utc::now().timestamp()
            } else {
                0
            },
            if focused {
                position.focused_entry.clone()
            } else {
                None
            },
        );
        if self.revision.as_ref() != Some(&revision) {
            self.rows.clear();
            self.revision = Some(revision);
        }
        let outgoing = position
            .outgoing
            .iter()
            .enumerate()
            .filter(|(_, pending)| pending.in_chat(session))
            .map(|(index, pending)| Entry {
                id: format!("difu-outgoing-{index}"),
                kind: "sending".into(),
                text: pending.text.clone(),
                ..Entry::default()
            })
            .collect::<Vec<_>>();
        // Outgoing slots can be reused without a new session revision.
        for entry in &outgoing {
            self.rows.remove(&entry.id);
        }
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
                start: 0,
                total: 0,
                sections,
            };
        }
        let follow = position.follow && !position.keep_transcript_position;
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
                if !self.rows.contains_key(&section.id)
                    && let Some(entry) = entries.get(section.id.as_str())
                {
                    let (rows, _) = transcript::render_entries(
                        session,
                        std::slice::from_ref(*entry),
                        position,
                        width,
                        focused,
                    );
                    self.heights.insert(section.id.clone(), rows.len());
                    self.rows.insert(section.id.clone(), rows);
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
        self.rows.retain(|id, _| keep.contains(id.as_str()));
        let lines = sections
            .get(first..end)
            .unwrap_or_default()
            .iter()
            .flat_map(|s| self.rows.get(&s.id).into_iter().flatten().cloned())
            .collect();
        View {
            lines,
            start: sections.get(first).map_or(0, |s| s.row),
            total,
            sections,
        }
    }
}
