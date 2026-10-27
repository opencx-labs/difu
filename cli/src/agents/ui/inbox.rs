//! One questions/requests view, retaining the existing per-session answer drafts.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    session: String,
    request: Value,
    field: Option<usize>,
}

#[derive(Clone)]
struct Item {
    key: Key,
    pending: Pending,
}

#[derive(Default)]
struct Section {
    id: String,
    title: String,
    items: Vec<Item>,
    answered: usize,
    rows: Vec<Line<'static>>,
    leaving: Option<(Instant, Duration)>,
}

#[derive(Default)]
pub(super) struct State {
    pub visible: bool,
    pub reads: HashSet<String>,
    pub attempts: HashMap<String, Instant>,
    sections: Vec<Section>,
    active: Option<Key>,
    sending: Vec<Key>,
    scroll: usize,
    maximum: usize,
    reveal: bool,
    area: Rect,
}

impl State {
    pub fn animating(&self) -> bool {
        self.visible
            && self
                .sections
                .iter()
                .any(|section| section.leaving.is_some())
    }
}

fn items(session: &Session) -> Vec<Item> {
    session
        .pending
        .iter()
        .flat_map(|pending| {
            let fields = if pending.method == "item/tool/requestUserInput" {
                pending
                    .unanswered_questions()
                    .into_iter()
                    .map(|(field, _)| Some(field))
                    .collect()
            } else if !pending.responded {
                vec![None]
            } else {
                Vec::new()
            };
            fields.into_iter().map(|field| Item {
                key: Key {
                    session: session.id.clone(),
                    request: pending.id.clone(),
                    field,
                },
                pending: pending.clone(),
            })
        })
        .collect()
}

impl Ui {
    pub(super) fn inbox_count(&self) -> (usize, usize) {
        self.summaries
            .iter()
            .filter(|s| !s.archived && !self.deleting.contains(&s.id))
            .fold((0, 0), |(count, sessions), s| {
                (count + s.pending, sessions + usize::from(s.pending > 0))
            })
    }

    pub(super) fn open_questions_inbox(&mut self) {
        if self.inbox.visible {
            return;
        }
        self.remember_answers();
        self.modal = None;
        self.inbox.visible = true;
        self.inbox.reveal = true;
        self.inbox.active = None;
        self.inbox.scroll = 0;
        self.orchestrator_list = false;
        self.archived = false;
        self.list_scroll = 0;
        self.drilled = true;
        self.focus = Focus::Conversation;
        self.panels.focused = false;
        self.text_selection.clear();
        self.sync_inbox();
    }

    pub(super) fn close_questions_inbox(&mut self) {
        self.remember_answers();
        self.inbox.visible = false;
        self.modal = None;
        self.question_note_focused = false;
        self.focus = Focus::Composer;
    }

    pub(super) fn tick_inbox(&mut self, visible: bool) {
        if !visible || !self.inbox.visible {
            return;
        }
        self.sync_inbox();
        let summaries = self.summaries.clone();
        for summary in &summaries {
            if summary.archived || self.deleting.contains(&summary.id) {
                continue;
            }
            if self.inbox.reads.len() >= 2 {
                break;
            }
            if self.inbox.reads.contains(&summary.id)
                || self
                    .inbox
                    .attempts
                    .get(&summary.id)
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
                || (summary.pending == 0
                    && !self
                        .inbox
                        .sections
                        .iter()
                        .any(|s| s.id == summary.id && !s.items.is_empty()))
                || self
                    .sessions
                    .get(&summary.id)
                    .is_some_and(|s| s.version >= summary.version)
            {
                continue;
            }
            self.read_inbox_session(&summary.id);
        }
    }

    fn read_inbox_session(&mut self, id: &str) {
        if !self.inbox.reads.insert(id.into()) {
            return;
        }
        self.inbox.attempts.insert(id.into(), Instant::now());
        self.task(
            Task::InboxRead(id.into()),
            Request::Read {
                id: id.into(),
                version: None,
            },
            false,
        );
    }

    pub(super) fn sync_inbox(&mut self) {
        if !self.inbox.visible {
            return;
        }
        let now = Instant::now();
        let tweaks = self.preferences.motion.tweaks(self.tweaks);
        let duration = Duration::from_millis(u64::from(tweaks.capped(tweaks.input_clear_ms)));
        self.inbox.sections.retain(|section| {
            self.summaries
                .iter()
                .any(|s| s.id == section.id && !s.archived && !self.deleting.contains(&s.id))
        });
        for summary in self
            .summaries
            .iter()
            .filter(|s| !s.archived && !self.deleting.contains(&s.id))
        {
            let Some(session) = self.sessions.get(&summary.id) else {
                continue;
            };
            let pending = items(session);
            let index = match self.inbox.sections.iter().position(|s| s.id == summary.id) {
                Some(index) => index,
                None if pending.is_empty() => continue,
                None => {
                    self.inbox.sections.push(Section {
                        id: summary.id.clone(),
                        ..Section::default()
                    });
                    self.inbox.sections.len() - 1
                }
            };
            let Some(section) = self.inbox.sections.get_mut(index) else {
                continue;
            };
            section.title.clone_from(&summary.title);
            section.answered += section
                .items
                .iter()
                .filter(|old| !pending.iter().any(|new| new.key == old.key))
                .count();
            if !section.items.is_empty()
                && pending.is_empty()
                && !duration.is_zero()
                && !section.rows.is_empty()
            {
                section.leaving = Some((now, duration));
            }
            if !pending.is_empty() {
                section.leaving = None;
            }
            section.items = pending;
            if section
                .leaving
                .is_some_and(|(started, duration)| now >= started + duration + flap::LIFETIME)
                || !self.preferences.motion.screen()
            {
                section.leaving = None;
            }
        }
        let keys: Vec<_> = self
            .inbox
            .sections
            .iter()
            .flat_map(|s| s.items.iter().map(|item| item.key.clone()))
            .collect();
        self.inbox.sending.retain(|key| keys.contains(key));
        let current = self.inbox.active.clone();
        if current.as_ref().is_some_and(|key| keys.contains(key)) {
            if self.modal.is_none()
                && let Some(key) = current
            {
                self.focus_inbox_item(key);
            }
            return;
        }
        if let Some(key) = &current
            && self
                .inbox
                .sections
                .iter()
                .any(|s| s.id == key.session && s.leaving.is_some())
        {
            self.remember_answers();
            self.modal = None;
            return;
        }
        let next = current
            .as_ref()
            .and_then(|key| {
                keys.iter()
                    .find(|candidate| candidate.session == key.session)
            })
            .or_else(|| {
                let current = current.as_ref()?;
                let index = self
                    .inbox
                    .sections
                    .iter()
                    .position(|s| s.id == current.session)?;
                self.inbox
                    .sections
                    .iter()
                    .skip(index + 1)
                    .find_map(|s| s.items.first().map(|item| &item.key))
            })
            .or_else(|| keys.first())
            .cloned();
        if let Some(key) = next {
            self.focus_inbox_item(key);
        } else {
            self.remember_answers();
            self.modal = None;
            self.inbox.active = None;
        }
    }

    fn focus_inbox_item(&mut self, key: Key) {
        let Some(index) = self
            .sessions
            .get(&key.session)
            .and_then(|s| s.pending.iter().position(|p| p.id == key.request))
        else {
            return;
        };
        self.remember_answers();
        self.select(key.session.clone());
        self.inbox.visible = true;
        self.pending(index);
        if let Some(field) = key.field
            && let Some(Modal::Approval {
                field: selected_field,
                selected,
                ..
            }) = &mut self.modal
        {
            *selected_field = field;
            *selected = self
                .positions
                .get(&key.session)
                .and_then(|p| p.question_choices.get(&(key.request.to_string(), field)))
                .copied()
                .unwrap_or(0);
        }
        self.inbox.active = Some(key);
        self.inbox.reveal = true;
        self.question_note_focused = false;
        self.focus = Focus::Composer;
    }

    pub(super) fn inbox_item_action(&mut self, key: Key, action: Option<Box<Action>>) {
        if !self
            .inbox
            .sections
            .iter()
            .any(|s| s.items.iter().any(|item| item.key == key))
        {
            return;
        }
        if self.inbox.active.as_ref() != Some(&key) || self.modal.is_none() {
            self.focus_inbox_item(key);
        }
        if let Some(action) = action {
            self.action(*action);
        }
    }

    pub(super) fn move_inbox_session(&mut self, down: bool) {
        let sections: Vec<_> = self
            .inbox
            .sections
            .iter()
            .filter(|s| !s.items.is_empty())
            .collect();
        let current = self
            .inbox
            .active
            .as_ref()
            .and_then(|key| sections.iter().position(|s| s.id == key.session))
            .unwrap_or(0);
        let next = if down {
            current
                .saturating_add(1)
                .min(sections.len().saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
        if let Some(key) = sections
            .get(next)
            .and_then(|s| s.items.first())
            .map(|item| item.key.clone())
        {
            self.focus_inbox_item(key);
        }
    }

    fn move_inbox_item(&mut self, down: bool) {
        let keys: Vec<_> = self
            .inbox
            .sections
            .iter()
            .flat_map(|s| s.items.iter().map(|item| item.key.clone()))
            .collect();
        let current = self
            .inbox
            .active
            .as_ref()
            .and_then(|key| keys.iter().position(|candidate| candidate == key))
            .unwrap_or(0);
        let next = if down {
            current.saturating_add(1).min(keys.len().saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
        if let Some(key) = keys.get(next) {
            self.focus_inbox_item(key.clone());
        }
    }

    pub(super) fn inbox_key(&mut self, key: KeyEvent) -> bool {
        use crate::shortcuts::ids::*;
        if self.shortcut(key) == Some(INBOX_OPEN) {
            self.open_questions_inbox();
            return true;
        }
        if !self.inbox.visible || !matches!(self.modal, None | Some(Modal::Approval { .. })) {
            return false;
        }
        match self.shortcut(key) {
            Some(SELECTION_COPY) => {
                self.copy_chat_selection();
            }
            Some(AGENTS_SIDEBAR) => self.toggle_list(),
            Some(AGENTS_CHANGES) => {
                self.close_questions_inbox();
                self.toggle_changes();
            }
            Some(AGENTS_MODELS) => {
                self.close_questions_inbox();
                self.open_model_picker();
            }
            Some(INBOX_PREVIOUS_SESSION) => self.move_inbox_session(false),
            Some(INBOX_NEXT_SESSION) => self.move_inbox_session(true),
            Some(QUESTION_NEXT_FIELD | QUESTION_NEXT | NAVIGATION_NEXT_FOCUS) => {
                self.move_inbox_item(true)
            }
            Some(QUESTION_PREVIOUS_FIELD | QUESTION_PREVIOUS | NAVIGATION_PREVIOUS_FOCUS) => {
                self.move_inbox_item(false)
            }
            Some(NAVIGATION_PAGE_UP) => {
                self.inbox.scroll = self.inbox.scroll.saturating_sub(5);
                self.inbox.reveal = false;
                self.question_reveal = false;
            }
            Some(NAVIGATION_PAGE_DOWN) => {
                self.inbox.scroll = self.inbox.scroll.saturating_add(5).min(self.inbox.maximum);
                self.inbox.reveal = false;
                self.question_reveal = false;
            }
            _ if key.code == KeyCode::Esc && !self.question_note_focused => {
                self.close_questions_inbox()
            }
            _ if self.inline_question() => {
                self.question_key(key);
            }
            _ if self.modal.is_some() => {
                self.modal_key(key);
                self.remember_answers();
            }
            _ => {}
        }
        true
    }

    pub(super) fn inbox_mouse(&mut self, event: MouseEvent) -> bool {
        if !self.inbox.visible || !matches!(self.modal, None | Some(Modal::Approval { .. })) {
            return false;
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, action)) = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains((event.column, event.row).into()))
                    .cloned()
                {
                    self.action(action);
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let rows = if event.kind == MouseEventKind::ScrollDown {
                    3
                } else {
                    -3
                };
                if self.inbox.area.contains((event.column, event.row).into()) {
                    self.inbox.scroll = self
                        .inbox
                        .scroll
                        .saturating_add_signed(rows)
                        .min(self.inbox.maximum);
                    self.inbox.reveal = false;
                    self.question_reveal = false;
                } else {
                    self.list_scroll = self.list_scroll.saturating_add_signed(rows);
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn respond_inbox(&mut self, request: Value, response: Value) {
        let Some(key) = self
            .inbox
            .active
            .clone()
            .filter(|key| key.request == request)
        else {
            return;
        };
        if self.inbox.sending.contains(&key) {
            return;
        }
        self.inbox.sending.push(key.clone());
        self.task(
            Task::InboxRespond(key.clone()),
            Request::Control {
                id: key.session,
                control: Control::Respond { request, response },
            },
            false,
        );
    }

    pub(super) fn inbox_response(&mut self, key: Key, result: Result<Reply, String>) {
        match result {
            Ok(Reply::Ok) => {
                self.refreshed = None;
                self.read_inbox_session(&key.session);
            }
            Err(error) => {
                self.inbox.sending.retain(|item| item != &key);
                self.notice = Some((format!("Could not answer request: {error}"), true));
            }
            _ => {
                self.inbox.sending.retain(|item| item != &key);
                self.notice = Some((
                    "Unexpected request response; refresh and try again".into(),
                    true,
                ));
            }
        }
    }

    pub(super) fn draw_inbox_card(&mut self, frame: &mut Frame, area: Rect) {
        let (count, sessions) = self.inbox_count();
        let color = if count == 0 { GREEN } else { crate::ui::YELLOW };
        let title = if area.width < 27 {
            "Questions"
        } else {
            "Questions & requests"
        };
        let hint = crate::shortcuts::Bindings::new(&self.shortcut_config)
            .hint(crate::shortcuts::ids::INBOX_OPEN);
        let rows = vec![
            Line::from(Span::styled(
                format!("   {title}"),
                Style::default().fg(color),
            )),
            Line::from(Span::styled(
                format!("   {count} pending · {sessions} sessions"),
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(format!("   {hint}"), Style::default().fg(DIM))),
        ];
        frame.render_widget(
            Paragraph::new(rows).style(if self.inbox.visible {
                crate::ui::user_message_style()
            } else {
                Style::default().bg(BG)
            }),
            area,
        );
        self.status_icons.draw(
            frame,
            Rect::new(area.x, area.y, status_icons::WIDTH, 1),
            if count == 0 {
                Status::Completed
            } else {
                Status::Waiting
            },
            false,
            self.preferences.motion.level == crate::settings::Motion::Full
                && self.tweaks.agent_reply_cps > 0,
        );
        self.hits.push((area, Action::InboxOpen));
    }

    pub(super) fn draw_inbox(&mut self, frame: &mut Frame, rect: Rect) {
        self.inbox.area = rect;
        let (count, sessions) = self.inbox_count();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" Questions & requests · {count} pending "))
            .border_style(Style::default().fg(crate::ui::YELLOW));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.height < 3 || inner.width < 4 {
            return;
        }
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        frame.render_widget(
            Paragraph::new(format!(
                " {sessions} sessions · {}/{} jump session",
                bindings.hint(crate::shortcuts::ids::INBOX_PREVIOUS_SESSION),
                bindings.hint(crate::shortcuts::ids::INBOX_NEXT_SESSION)
            ))
            .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let area = Rect::new(
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(3),
        );
        let mut lines = Vec::new();
        let mut hits: Vec<(usize, usize, Action)> = Vec::new();
        let mut cursor = None;
        let mut active_row = None;
        for index in 0..self.inbox.sections.len() {
            let Some(section) = self.inbox.sections.get(index) else {
                continue;
            };
            let id = section.id.clone();
            let active = self
                .inbox
                .active
                .as_ref()
                .is_some_and(|key| key.session == id);
            let complete = section.items.is_empty();
            let title = format!(
                "{} {} · {}",
                if complete {
                    "✓"
                } else if active {
                    "›"
                } else {
                    " "
                },
                section.title,
                if complete {
                    format!("{} resolved", section.answered)
                } else {
                    format!("{} pending", section.items.len())
                }
            );
            let header = lines.len();
            lines.extend(wrapped(&title, area.width).into_iter().map(|line| {
                Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(if complete {
                            GREEN
                        } else if active {
                            crate::ui::YELLOW
                        } else {
                            TEXT
                        })
                        .bg(PANEL),
                ))
            }));
            if let Some(item) = section.items.first() {
                hits.push((
                    header,
                    lines.len() - header,
                    Action::InboxItem(item.key.clone(), None),
                ));
            }
            let mut body = Vec::new();
            if let Some((started, duration)) = section.leaving {
                body.clone_from(&section.rows);
                let total = flap::glyphs(&body).len();
                flap::vanish(
                    &mut body,
                    |i| {
                        started
                            + duration
                                .mul_f64(total.saturating_sub(i + 1) as f64 / total.max(1) as f64)
                    },
                    Instant::now(),
                    started,
                );
                while body.last().is_some_and(|line| {
                    line.spans.iter().all(|span| span.content.trim().is_empty())
                }) {
                    body.pop();
                }
            } else {
                for item in section.items.clone() {
                    body.push(Line::default());
                    let offset = lines.len() + body.len();
                    let focused = self.inbox.active.as_ref() == Some(&item.key);
                    if focused {
                        active_row = Some(offset);
                    }
                    body.push(Line::from(Span::styled(
                        if item.key.field.is_some() {
                            "Question"
                        } else {
                            "Permission request"
                        },
                        Style::default().fg(DIM),
                    )));
                    let start = body.len();
                    if let Some(field) = item.key.field {
                        if let Some((question, question_cursor, question_hits)) =
                            self.inbox_question_lines(&id, &item.pending, field, area.width)
                        {
                            if let Some((x, y)) = question_cursor {
                                cursor = Some((x, lines.len() + start + y));
                            }
                            for (row, height, action) in question_hits {
                                hits.push((
                                    lines.len() + start + row,
                                    height,
                                    Action::InboxItem(item.key.clone(), Some(Box::new(action))),
                                ));
                            }
                            body.extend(question);
                        }
                        body.push(Line::default());
                        let sending =
                            self.question_send
                                .as_ref()
                                .is_some_and(|(session, request, _, _)| {
                                    session == &id && request == &item.key.request
                                });
                        let row = lines.len() + body.len();
                        body.push(Line::from(Span::styled(
                            if sending {
                                "  Sending answer…".into()
                            } else {
                                format!(
                                    "  [ Send answer · {} ]",
                                    bindings.hint(crate::shortcuts::ids::QUESTION_SUBMIT)
                                )
                            },
                            Style::default().fg(GREEN),
                        )));
                        if !sending {
                            hits.push((
                                row,
                                1,
                                Action::InboxItem(
                                    item.key.clone(),
                                    Some(Box::new(Action::SubmitQuestion(false))),
                                ),
                            ));
                        }
                    } else {
                        let description = item
                            .pending
                            .params
                            .pointer("/difuClaudePlan/text")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                serde_json::to_string_pretty(&item.pending.params)
                                    .unwrap_or_else(|_| "Cannot display request".into())
                            });
                        body.extend(
                            wrapped(&description, area.width)
                                .into_iter()
                                .map(Line::from),
                        );
                        body.push(Line::default());
                        let selected = match &self.modal {
                            Some(Modal::Approval { selected, .. }) if focused => Some(*selected),
                            _ => None,
                        };
                        for (choice, label) in approval_choices(&item.pending).iter().enumerate() {
                            let row = lines.len() + body.len();
                            let sending = self.inbox.sending.contains(&item.key);
                            body.push(Line::from(Span::styled(
                                format!(
                                    "{} [ {label} ]{}",
                                    if selected == Some(choice) { "›" } else { " " },
                                    if sending { " · sending" } else { "" }
                                ),
                                Style::default().fg(if selected == Some(choice) {
                                    ACCENT
                                } else {
                                    TEXT
                                }),
                            )));
                            if !sending {
                                hits.push((
                                    row,
                                    1,
                                    Action::InboxItem(
                                        item.key.clone(),
                                        Some(Box::new(Action::Approve(choice))),
                                    ),
                                ));
                            }
                        }
                    }
                    hits.push((offset, 1, Action::InboxItem(item.key.clone(), None)));
                }
                if let Some(section) = self.inbox.sections.get_mut(index) {
                    section.rows.clone_from(&body);
                }
            }
            lines.extend(body);
            lines.push(Line::default());
        }
        if self.inbox.sections.is_empty()
            || self
                .inbox
                .sections
                .iter()
                .all(|s| s.items.is_empty() && s.leaving.is_none())
        {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                if count > 0 {
                    "Loading questions & requests…"
                } else {
                    "✓ All caught up."
                },
                Style::default().fg(GREEN),
            )));
            if count == 0 {
                lines.push(Line::from(Span::styled(
                    "New questions and requests will appear here.",
                    Style::default().fg(DIM),
                )));
            }
        }
        let height = usize::from(area.height);
        self.inbox.maximum = lines.len().saturating_sub(height);
        if self.inbox.reveal || self.question_reveal {
            if let Some(row) = active_row {
                self.inbox.scroll = row.saturating_sub(2);
            }
            if let Some((_, row)) = cursor {
                self.inbox.scroll = self
                    .inbox
                    .scroll
                    .max(row.saturating_sub(height.saturating_sub(1)));
            }
            self.inbox.reveal = false;
            self.question_reveal = false;
        }
        self.inbox.scroll = self.inbox.scroll.min(self.inbox.maximum);
        let scroll = self.inbox.scroll;
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(scroll)
                    .take(height)
                    .collect::<Vec<_>>(),
            ),
            area,
        );
        for (row, count, action) in hits {
            let top = row.max(scroll);
            let bottom = (row + count).min(scroll + height);
            if bottom > top {
                self.hits.push((
                    Rect::new(
                        area.x,
                        area.y + (top - scroll) as u16,
                        area.width,
                        (bottom - top) as u16,
                    ),
                    action,
                ));
            }
        }
        if let Some((x, row)) = cursor
            && row >= scroll
            && row < scroll + height
            && x < usize::from(area.width)
        {
            frame.set_cursor_position((area.x + x as u16, area.y + (row - scroll) as u16));
        }
        frame.render_widget(
            Paragraph::new(" Tab next item · Shift+Tab previous · Esc back")
                .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn fixture(root: &std::path::Path) -> Ui {
        let mut ui = Ui::new(
            Storage {
                config: root.join("config.json"),
                cache: root.join("cache"),
            },
            &Config::default(),
        );
        for id in ["one", "two", "three"] {
            let mut session = Session::new(
                id.into(),
                Job::Coding(Launch {
                    repository: root.into(),
                    isolated: true,
                    base: "HEAD".into(),
                    prompt: id.into(),
                    model: None,
                    effort: None,
                }),
            );
            session.title = format!("Session {id}");
            session.status = Status::Running;
            session.pending.push(Pending {
                // Providers may reuse request IDs across different sessions.
                id: json!(7), method: "item/tool/requestUserInput".into(), responded: true,
                params: json!({"difuAsync":true,"questions":[{"id":"a","question":format!("Question for {id}")}]}),
            });
            ui.summaries.push(session.summary());
            ui.sessions.insert(id.into(), session);
        }
        ui
    }

    #[test]
    fn navigation_keeps_drafts_isolated_and_excludes_archived_sessions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.summaries.last_mut().context("summary")?.archived = true;
        ui.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert!(ui.inbox.visible);
        assert_eq!(ui.inbox_count(), (2, 2));
        assert_eq!(ui.inbox.sections.len(), 2);
        ui.paste("First draft");
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(ui.selected.as_deref(), Some("two"));
        ui.paste("Second draft");
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("First draft")
        );
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("Second draft")
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.inbox.visible);
        assert!(
            ui.positions
                .get("two")
                .context("position")?
                .draft
                .text()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn sections_collapse_only_after_all_items_resolve_and_select_the_next_session() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.sessions
            .get_mut("two")
            .context("session")?
            .pending
            .push(Pending {
                id: json!(8),
                method: "item/commandExecution/requestApproval".into(),
                params: json!({"command":"git status"}),
                responded: false,
            });
        ui.open_questions_inbox();
        ui.move_inbox_session(true);
        let mut terminal = Terminal::new(TestBackend::new(110, 50))?;
        terminal.draw(|frame| ui.draw_inbox(frame, frame.area()))?;
        let session = ui.sessions.get_mut("two").context("session")?;
        session.pending.first_mut().context("question")?.params["difuAnswers"] =
            json!({"a":"done"});
        ui.sync_inbox();
        let section = ui.inbox.sections.get(1).context("section")?;
        assert_eq!(section.items.len(), 1);
        assert!(section.leaving.is_none());
        terminal.draw(|frame| ui.draw_inbox(frame, frame.area()))?;
        ui.sessions
            .get_mut("two")
            .context("session")?
            .pending
            .clear();
        ui.sync_inbox();
        assert!(
            ui.inbox
                .sections
                .get(1)
                .context("section")?
                .leaving
                .is_some()
        );
        let section = ui.inbox.sections.get_mut(1).context("section")?;
        if let Some((started, duration)) = &mut section.leaving {
            *started -= *duration + flap::LIFETIME + Duration::from_secs(1);
        }
        ui.sync_inbox();
        assert_eq!(ui.selected.as_deref(), Some("three"));
        assert!(
            ui.inbox
                .sections
                .get(1)
                .context("section")?
                .items
                .is_empty()
        );
        terminal.draw(|frame| ui.draw_inbox(frame, frame.area()))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(screen.contains("Session two · 2 resolved"));
        assert!(!screen.contains("git status"));
        Ok(())
    }

    #[test]
    fn empty_inbox_does_not_edit_chat_and_reduced_motion_finishes_immediately() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.preferences.motion.level = crate::settings::Motion::Off;
        ui.open_questions_inbox();
        let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
        terminal.draw(|frame| ui.draw_inbox(frame, frame.area()))?;
        for session in ui.sessions.values_mut() {
            session.pending.clear();
        }
        for summary in &mut ui.summaries {
            summary.pending = 0;
        }
        ui.sync_inbox();
        ui.paste("Do not send this to a chat");
        assert!(!ui.inbox.animating());
        assert!(ui.inbox.active.is_none());
        assert!(ui.focused_editor().is_none());
        assert!(ui.positions.values().all(|p| p.draft.text().is_empty()));
        terminal.draw(|frame| ui.draw_inbox(frame, frame.area()))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(screen.contains("All caught up"));
        Ok(())
    }

    #[test]
    fn late_permission_failure_preserves_the_new_sessions_draft() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.open_questions_inbox();
        let origin = ui.inbox.active.clone().context("active")?;
        ui.inbox.sending.push(origin.clone());
        ui.move_inbox_session(true);
        ui.paste("Keep this answer");
        ui.inbox_response(origin, Err("Disconnected".into()));
        assert_eq!(ui.selected.as_deref(), Some("two"));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("Keep this answer")
        );
        assert!(ui.inbox.sending.is_empty());
        assert!(ui.inbox.sections.iter().all(|s| s.leaving.is_none()));
        Ok(())
    }
}
