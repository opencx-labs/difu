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

fn section_header(title: &str, width: u16, color: ratatui::style::Color) -> Line<'static> {
    let title = crate::ui::crop(title, 0, usize::from(width.saturating_sub(5)));
    let padding = usize::from(width.saturating_sub(5))
        .saturating_sub(unicode_width::UnicodeWidthStr::width(title.as_str()));
    Line::from(Span::styled(
        format!("┌─ {title} {}┐", "─".repeat(padding)),
        Style::default().fg(color),
    ))
}

fn section_row(line: Line<'static>, width: u16, color: ratatui::style::Color) -> Line<'static> {
    let mut remaining = usize::from(width.saturating_sub(4));
    let mut spans = vec![Span::styled("│ ", Style::default().fg(color))];
    for span in line.spans {
        let text = crate::ui::crop(&span.content, 0, remaining);
        remaining = remaining.saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
        spans.push(Span::styled(
            text,
            Style::default()
                .fg(TEXT)
                .patch(line.style)
                .patch(span.style),
        ));
    }
    spans.push(Span::raw(" ".repeat(remaining)));
    spans.push(Span::styled(" │", Style::default().fg(color)));
    Line::from(spans)
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
            self.focus = Focus::Composer;
            return;
        }
        self.remember_answers();
        self.modal = None;
        self.inbox.visible = true;
        self.inbox.reveal = true;
        self.inbox.active = None;
        self.inbox.scroll = 0;
        self.orchestrator_list = false;
        self.folder_view = None;
        self.folder_cursor = None;
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
        if self.deleting.contains(&key.session) {
            return;
        }
        let Some(index) = self
            .sessions
            .get(&key.session)
            .and_then(|s| s.pending.iter().position(|p| p.id == key.request))
        else {
            return;
        };
        let sidebar = self.inbox.visible && self.focus == Focus::List;
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
        self.focus = if sidebar {
            Focus::List
        } else {
            Focus::Composer
        };
    }

    pub(super) fn inbox_item_action(&mut self, key: Key, action: Option<Box<Action>>) {
        if self.deleting.contains(&key.session)
            || !self.sessions.get(&key.session).is_some_and(|session| {
                !session.archived && items(session).iter().any(|item| item.key == key)
            })
        {
            return;
        }
        self.focus = Focus::Composer;
        if self.inbox.active.as_ref() != Some(&key) || self.modal.is_none() {
            self.focus_inbox_item(key.clone());
        }
        // A card can disappear between drawing and the click. Never dispatch its
        // action against whichever other session happens to remain selected.
        if self.inbox.active.as_ref() != Some(&key)
            || self.selected.as_ref() != Some(&key.session)
            || !matches!(&self.modal, Some(Modal::Approval { pending, field, .. })
                if pending.id == key.request && key.field.is_none_or(|expected| expected == *field))
        {
            return;
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
        if self.focus == Focus::List {
            match self.shortcut(key) {
                Some(NAVIGATION_DOWN | NAVIGATION_FAST_DOWN | NAVIGATION_PAGE_DOWN) => {
                    self.move_list(1);
                }
                Some(NAVIGATION_LAST) => self.move_list(i32::MAX),
                Some(NAVIGATION_ACTIVATE | NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                    self.focus = Focus::Composer;
                    self.inbox.reveal = true;
                }
                Some(AGENTS_SIDEBAR) => self.toggle_list(),
                _ if key.code == KeyCode::Esc => self.close_questions_inbox(),
                _ => {}
            }
            return true;
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
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                self.remember_answers();
                self.list_visible = true;
                self.panels.focused = false;
                self.focus = Focus::List;
            }
            Some(QUESTION_NEXT_FIELD | QUESTION_NEXT) => self.move_inbox_item(true),
            Some(QUESTION_PREVIOUS_FIELD | QUESTION_PREVIOUS) => self.move_inbox_item(false),
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
            .border_style(Style::default().fg(if self.focus == Focus::List {
                DIM
            } else {
                ACCENT
            }));
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
        let content = Rect::new(
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(3),
        );
        let (area, minimap) = if content.width >= 36 {
            let width = (content.width / 3).clamp(12, 32);
            (
                Rect::new(
                    content.x,
                    content.y,
                    content.width - width - 1,
                    content.height,
                ),
                Rect::new(content.right() - width, content.y, width, content.height),
            )
        } else {
            let height =
                (self.inbox.sections.len().saturating_add(2) as u16).min(content.height / 3);
            (
                Rect::new(
                    content.x,
                    content.y + height,
                    content.width,
                    content.height - height,
                ),
                Rect::new(content.x, content.y, content.width, height),
            )
        };
        let question_width = area.width.saturating_sub(4).max(1);
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
                .is_some_and(|key| key.session == id)
                && self.focus != Focus::List;
            let complete = section.items.is_empty();
            let title = format!(
                "{} · {}",
                section.title,
                if complete {
                    format!("{} resolved", section.answered)
                } else {
                    format!("{} pending", section.items.len())
                }
            );
            let header = lines.len();
            let border = if active { ACCENT } else { DIM };
            lines.push(section_header(&title, area.width, border));
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
                    let item_hit_start = hits.len();
                    let selected = self.inbox.active.as_ref() == Some(&item.key);
                    let focused = selected && self.focus != Focus::List;
                    if selected {
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
                            self.inbox_question_lines(&id, &item.pending, field, question_width)
                        {
                            if let Some((x, y)) = question_cursor {
                                cursor = Some((x + 2, lines.len() + start + y));
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
                                    bindings.primary_hint_in(
                                        crate::shortcuts::ids::QUESTION_SUBMIT,
                                        crate::shortcuts::contexts::AGENT_INBOX_QUESTION
                                    )
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
                            wrapped(&description, question_width)
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
                    hits.insert(
                        item_hit_start,
                        (
                            offset,
                            lines.len() + body.len() - offset,
                            Action::InboxItem(item.key.clone(), None),
                        ),
                    );
                }
                if let Some(section) = self.inbox.sections.get_mut(index) {
                    section.rows.clone_from(&body);
                }
            }
            lines.extend(
                body.into_iter()
                    .map(|line| section_row(line, area.width, border)),
            );
            lines.push(Line::from(Span::styled(
                format!(
                    "└{}┘",
                    "─".repeat(usize::from(area.width.saturating_sub(2)))
                ),
                Style::default().fg(border),
            )));
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
            Paragraph::new(format!(
                " {} sessions list · {}/{} question · Esc back",
                bindings.primary_hint_in(
                    crate::shortcuts::ids::NAVIGATION_NEXT_FOCUS,
                    crate::shortcuts::contexts::AGENT_INBOX
                ),
                bindings.primary_hint_in(
                    crate::shortcuts::ids::QUESTION_PREVIOUS,
                    crate::shortcuts::contexts::AGENT_INBOX
                ),
                bindings.primary_hint_in(
                    crate::shortcuts::ids::QUESTION_NEXT,
                    crate::shortcuts::contexts::AGENT_INBOX
                )
            ))
            .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
        self.draw_inbox_minimap(frame, minimap);
    }

    fn draw_inbox_minimap(&mut self, frame: &mut Frame, area: Rect) {
        let area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height
                .min(self.inbox.sections.len().saturating_add(2) as u16),
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Sessions · questions ")
            .border_style(Style::default().fg(DIM));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let active = self.inbox.active.as_ref().map(|key| key.session.as_str());
        let selected = self
            .inbox
            .sections
            .iter()
            .position(|section| Some(section.id.as_str()) == active)
            .unwrap_or(0);
        let offset = selected.saturating_sub(usize::from(inner.height).saturating_sub(1));
        for (row, section) in self
            .inbox
            .sections
            .iter()
            .skip(offset)
            .take(usize::from(inner.height))
            .enumerate()
        {
            let focused = Some(section.id.as_str()) == active && self.focus != Focus::List;
            let count = section.items.len().to_string();
            let width = usize::from(inner.width).saturating_sub(count.len() + 1);
            let name = crate::ui::crop(&section.title, 0, width);
            let padding =
                width.saturating_sub(unicode_width::UnicodeWidthStr::width(name.as_str()));
            let row = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
            frame.render_widget(
                Paragraph::new(format!("{name}{} {count}", " ".repeat(padding))).style(
                    if focused {
                        crate::ui::user_message_style().fg(ACCENT)
                    } else {
                        Style::default().fg(TEXT)
                    },
                ),
                row,
            );
            if let Some(item) = section.items.first() {
                self.hits
                    .push((row, Action::InboxItem(item.key.clone(), None)));
            }
        }
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
        ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert_eq!(ui.clipboard.as_deref(), Some("First draft"));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(ui.selected.as_deref(), Some("two"));
        ui.paste("Second draft");
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("First draft")
        );
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::List);
        assert!(ui.focused_editor().is_none());
        ui.paste("Do not edit an unfocused question");
        ui.sync_inbox();
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("First draft")
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
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
    fn sidebar_arrows_reach_questions_and_tab_returns_without_editing_answers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        let first = ui.filtered().first().context("first session")?.id.clone();
        ui.select(first.clone());
        ui.focus = Focus::List;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(ui.inbox.visible);
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.paste("Keep this draft");
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(ui.inbox.visible);
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(!ui.inbox.visible);
        assert_eq!(ui.focus, Focus::List);
        assert_eq!(ui.selected.as_deref(), Some(first.as_str()));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("Keep this draft")
        );
        Ok(())
    }

    fn text_position(buffer: &ratatui::buffer::Buffer, text: &str) -> Result<(u16, u16)> {
        for y in buffer.area.top()..buffer.area.bottom() {
            let row = (buffer.area.left()..buffer.area.right())
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>();
            if let Some(index) = row.find(text) {
                let prefix = row.get(..index).context("text boundary")?;
                return Ok((
                    buffer.area.x + unicode_width::UnicodeWidthStr::width(prefix) as u16,
                    y,
                ));
            }
        }
        anyhow::bail!("Missing rendered text: {text}")
    }

    #[test]
    fn borders_and_minimap_follow_focus_and_questions_remain_neutral_elsewhere() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.preferences.motion.level = crate::settings::Motion::Off;
        ui.open_questions_inbox();
        ui.paste("Saved answer");
        let mut terminal = Terminal::new(TestBackend::new(120, 50))?;
        terminal.draw(|frame| {
            ui.draw_inbox(frame, frame.area());
        })?;
        let buffer = terminal.backend().buffer();
        let one = text_position(buffer, "Question for one")?;
        let two = text_position(buffer, "Question for two")?;
        assert_eq!(buffer.cell(one).context("active question")?.fg, ACCENT);
        assert_eq!(buffer.cell(two).context("inactive question")?.fg, TEXT);
        for (name, color) in [
            ("Session one · 1 pending", ACCENT),
            ("Session two · 1 pending", DIM),
        ] {
            let (_, y) = text_position(buffer, name)?;
            let border = buffer.cell((2, y)).context("section border")?;
            assert_eq!(border.symbol(), "┌");
            assert_eq!(border.fg, color);
        }
        let (map, action) = ui
            .hits
            .iter()
            .rev()
            .find(|(rect, action)| {
                rect.x > 80
                    && matches!(action, Action::InboxItem(key, None) if key.session == "two")
            })
            .cloned()
            .context("minimap session")?;
        assert_eq!(
            buffer
                .cell((map.right() - 1, map.y))
                .context("question count")?
                .symbol(),
            "1"
        );
        ui.action(action);
        assert_eq!(ui.selected.as_deref(), Some("two"));
        terminal.draw(|frame| {
            ui.draw_inbox(frame, frame.area());
        })?;
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((map.x, map.y))
                .context("focused map row")?
                .fg,
            ACCENT
        );
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        terminal.draw(|frame| {
            ui.draw_inbox(frame, frame.area());
        })?;
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((map.x, map.y))
                .context("unfocused map row")?
                .fg,
            TEXT
        );
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("Saved answer")
        );
        for width in [24, 40, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20))?;
            terminal.draw(|frame| {
                ui.draw_inbox(frame, frame.area());
            })?;
        }
        Ok(())
    }

    #[test]
    fn session_rows_align_with_the_questions_card_without_a_selection_chevron() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.preferences.motion.level = crate::settings::Motion::Off;
        ui.select("one".into());
        let mut terminal = Terminal::new(TestBackend::new(50, 30))?;
        terminal.draw(|frame| {
            ui.draw_list(frame, frame.area());
        })?;
        let buffer = terminal.backend().buffer();
        let (questions_x, _) = text_position(buffer, "Questions & requests")?;
        let (session_x, y) = text_position(buffer, "Session one")?;
        assert_eq!(session_x, questions_x);
        assert!(
            (buffer.area.left()..session_x)
                .all(|x| buffer.cell((x, y)).is_none_or(|cell| cell.symbol() != "›"))
        );
        assert_eq!(
            buffer.cell((session_x, y)).context("selected session")?.bg,
            crate::ui::user_message_style()
                .bg
                .context("selection background")?
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
        terminal.draw(|frame| {
            let area = frame.area();
            ui.draw_inbox(frame, area);
        })?;
        let session = ui.sessions.get_mut("two").context("session")?;
        session
            .pending
            .first_mut()
            .context("question")?
            .params
            .as_object_mut()
            .context("params")?
            .insert("difuAnswers".into(), json!({"a":"done"}));
        ui.sync_inbox();
        let section = ui.inbox.sections.get(1).context("section")?;
        assert_eq!(section.items.len(), 1);
        assert!(section.leaving.is_none());
        terminal.draw(|frame| {
            let area = frame.area();
            ui.draw_inbox(frame, area);
        })?;
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
        terminal.draw(|frame| {
            let area = frame.area();
            ui.draw_inbox(frame, area);
        })?;
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
        terminal.draw(|frame| {
            let area = frame.area();
            ui.draw_inbox(frame, area);
        })?;
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
        terminal.draw(|frame| {
            let area = frame.area();
            ui.draw_inbox(frame, area);
        })?;
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
    fn stale_item_click_cannot_change_another_sessions_answer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path());
        ui.open_questions_inbox();
        ui.paste("Keep the first draft");
        let target = ui
            .inbox
            .sections
            .get(1)
            .and_then(|s| s.items.first())
            .context("second item")?
            .key
            .clone();
        ui.sessions
            .get_mut("two")
            .context("second session")?
            .pending
            .clear();
        ui.inbox_item_action(target, Some(Box::new(Action::QuestionChoice(9))));
        assert_eq!(ui.selected.as_deref(), Some("one"));
        assert_eq!(
            ui.focused_editor().map(Editor::text).as_deref(),
            Some("Keep the first draft")
        );
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
