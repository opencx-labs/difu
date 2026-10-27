use super::*;

type QuestionLines = (
    Vec<Line<'static>>,
    Option<(usize, usize)>,
    Vec<(usize, usize, Action)>,
);

/// The inline question's split-flap: each question flips in as it opens, and
/// an answered one flips away like the composer's text after a send.
#[derive(Default)]
pub(super) struct Flip {
    /// The question on show: session, request and question.
    shown: Option<(String, String, usize)>,
    board: flap::Board,
    /// The rows on show, kept to flip away once answered.
    drawn: Vec<Line<'static>>,
    leaving: Option<Leaving>,
}

/// An answered question's rows flipping away, last character first.
struct Leaving {
    session: String,
    rows: Vec<Line<'static>>,
    started: Instant,
    duration: Duration,
    /// Keep the answered rows suppressed until delivery resolves, even when
    /// a preference refresh stops their visual effect immediately.
    settled: bool,
}
impl Leaving {
    fn done(&self, now: Instant) -> bool {
        self.settled || now >= self.started + self.duration + flap::LIFETIME
    }
    fn frame(&self, now: Instant) -> Vec<Line<'static>> {
        if self.settled {
            return Vec::new();
        }
        let mut rows = self.rows.clone();
        let total = flap::glyphs(&rows).len();
        flap::vanish(
            &mut rows,
            |index| {
                let turn = total.saturating_sub(index + 1) as f64 / total.max(1) as f64;
                self.started + self.duration.mul_f64(turn)
            },
            now,
            self.started,
        );
        rows
    }
}
impl Flip {
    pub(super) fn settle_opening(&mut self) {
        self.board = flap::Board::default();
    }
    pub(super) fn settle_leaving(&mut self) {
        if let Some(leaving) = &mut self.leaving {
            leaving.settled = true;
        }
    }
    pub(super) fn animating(&self, now: Instant) -> bool {
        self.leaving
            .as_ref()
            .is_some_and(|leaving| !leaving.done(now))
            || self.board.animating(now)
    }
    /// A key press finishes the flip away, so input is never blocked.
    pub(super) fn interrupt(&mut self) {
        if self.leaving.take().is_some() {
            self.board = flap::Board::default();
        }
    }
}

#[cfg(test)]
impl Flip {
    /// Moves the flips back, as if `by` had passed.
    pub(super) fn age(&mut self, by: Duration) {
        self.board.age(by);
        if let Some(leaving) = &mut self.leaving {
            leaving.started = leaving.started.checked_sub(by).unwrap_or(leaving.started);
        }
    }
}

impl Ui {
    fn question_note_key(&self) -> Option<(String, usize, usize)> {
        let Some(Modal::Approval {
            pending,
            field,
            selected,
            ..
        }) = &self.modal
        else {
            return None;
        };
        if !self.inline_question() || pending.id == "difu-missing-guidance" {
            return None;
        }
        pending
            .params
            .get("questions")?
            .as_array()?
            .get(*field)?
            .get("options")?
            .as_array()?
            .get(*selected)?;
        Some((pending.id.to_string(), *field, *selected))
    }
    pub(super) fn question_note(&self) -> Option<&Editor> {
        let key = self.question_note_key()?;
        self.positions
            .get(self.selected.as_ref()?)?
            .question_notes
            .get(&key)
    }
    pub(super) fn question_note_mut(&mut self) -> Option<&mut Editor> {
        let key = self.question_note_key()?;
        self.positions
            .get_mut(self.selected.as_ref()?)?
            .question_notes
            .get_mut(&key)
    }
    pub(super) fn open_question_note(&mut self) {
        let Some(key) = self.question_note_key() else {
            return;
        };
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.positions
            .entry(id)
            .or_default()
            .question_notes
            .entry(key)
            .or_default();
        self.question_note_focused = true;
        self.question_reveal = true;
    }
    /// Claude marks questions that accept several options with `multiSelect`.
    fn multi_select_key(&self) -> Option<(String, usize)> {
        let Some(Modal::Approval { pending, field, .. }) = &self.modal else {
            return None;
        };
        let question = pending.params.get("questions")?.as_array()?.get(*field)?;
        (self.inline_question()
            && question.get("multiSelect").and_then(Value::as_bool) == Some(true))
        .then(|| (pending.id.to_string(), *field))
    }
    fn question_checks(&self) -> Option<&std::collections::BTreeSet<usize>> {
        let key = self.multi_select_key()?;
        self.positions
            .get(self.selected.as_ref()?)?
            .question_checks
            .get(&key)
    }
    /// Toggle the highlighted option of a multi-select question. "Other" remains
    /// a text field, so this reports whether an option was toggled.
    pub(super) fn toggle_question_check(&mut self) -> bool {
        let Some(key) = self.multi_select_key() else {
            return false;
        };
        let Some(Modal::Approval {
            pending,
            field,
            selected,
            ..
        }) = &self.modal
        else {
            return false;
        };
        let choice = *selected;
        let count = pending
            .params
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|q| q.get(*field))
            .and_then(|q| q.get("options"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let Some(id) = self.selected.clone().filter(|_| choice < count) else {
            return false;
        };
        let checks = self
            .positions
            .entry(id)
            .or_default()
            .question_checks
            .entry(key)
            .or_default();
        if !checks.remove(&choice) {
            checks.insert(choice);
        }
        true
    }
    pub(super) fn selected_question_answer(&self) -> Option<String> {
        let Some(Modal::Approval {
            pending,
            field,
            selected,
            answers,
            ..
        }) = &self.modal
        else {
            return None;
        };
        let question = pending.params.get("questions")?.as_array()?.get(*field)?;
        let label = |index: usize| {
            question
                .get("options")
                .and_then(Value::as_array)
                .and_then(|options| options.get(index))
                .and_then(|option| option.get("label"))
                .and_then(Value::as_str)
        };
        let text = if self.multi_select_key().is_some() {
            let other = answers
                .get(*field)
                .map(Editor::text)
                .filter(|text| !text.trim().is_empty());
            let mut chosen = self.question_checks().cloned().unwrap_or_default();
            // With nothing checked, Enter answers with the highlighted option.
            if chosen.is_empty() && other.is_none() {
                chosen.insert(*selected);
            }
            let chosen = chosen
                .into_iter()
                .filter_map(|index| Some((index, label(index)?)))
                .collect::<Vec<_>>();
            // Claude reads several choices as comma-separated labels.
            let mut text = chosen
                .iter()
                .map(|(_, label)| *label)
                .chain(other.as_deref())
                .collect::<Vec<_>>()
                .join(", ");
            for (index, label) in chosen {
                if let Some(note) = self
                    .positions
                    .get(self.selected.as_ref()?)
                    .and_then(|p| {
                        p.question_notes
                            .get(&(pending.id.to_string(), *field, index))
                    })
                    .map(Editor::text)
                    .filter(|note| !note.trim().is_empty())
                {
                    text.push_str(&format!("\n\nNote on {label}: {note}"));
                }
            }
            text
        } else if let Some(label) = label(*selected) {
            let mut text = label.to_owned();
            if let Some(note) = self
                .question_note()
                .map(Editor::text)
                .filter(|note| !note.trim().is_empty())
            {
                text.push_str("\n\nNote: ");
                text.push_str(&note);
            }
            text
        } else {
            answers.get(*field).map(Editor::text).unwrap_or_default()
        };
        if text.trim().is_empty() {
            return None;
        }
        Some(text)
    }
    pub(super) fn inline_question(&self) -> bool {
        matches!(&self.modal,Some(Modal::Approval {pending,..}) if pending.method == "item/tool/requestUserInput")
    }
    fn question_slots(&self) -> Vec<(usize, usize)> {
        self.selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(|session| {
                session
                    .pending
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.method == "item/tool/requestUserInput")
                    .flat_map(|(index, p)| {
                        p.unanswered_questions()
                            .into_iter()
                            .map(move |(field, _)| (index, field))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(super) fn question_index(&self) -> Option<usize> {
        let Some(Modal::Approval { pending, field, .. }) = &self.modal else {
            return None;
        };
        let session = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))?;
        self.question_slots().iter().position(|(index, f)| {
            *f == *field
                && session
                    .pending
                    .get(*index)
                    .is_some_and(|p| p.id == pending.id)
        })
    }
    pub(super) fn open_question(&mut self, index: usize) {
        self.question_reveal = true;
        self.question_note_focused = false;
        self.remember_answers();
        let Some((request, field)) = self.question_slots().get(index).copied() else {
            self.modal = None;
            self.focus = Focus::Composer;
            return;
        };
        self.pending(request);
        if let Some(Modal::Approval {
            pending,
            field: active,
            selected,
            ..
        }) = &mut self.modal
        {
            *active = field;
            *selected = self
                .selected
                .as_ref()
                .and_then(|id| self.positions.get(id))
                .and_then(|p| p.question_choices.get(&(pending.id.to_string(), field)))
                .copied()
                .unwrap_or(0);
        }
        self.drilled = true;
        self.focus = Focus::Composer;
    }
    pub(super) fn open_questions(&mut self) {
        if self.question_slots().is_empty() {
            self.remember_answers();
            self.modal = Some(Modal::Pending { selected: 0 });
        } else {
            self.open_question(0);
        }
    }
    pub(super) fn question_key(&mut self, key: KeyEvent) -> bool {
        use crate::shortcuts::ids::*;
        let action = self.shortcut(key);
        if !self.inline_question() {
            return false;
        }
        if matches!(action, Some(QUESTION_PREVIOUS | QUESTION_NEXT)) {
            let index = self.question_index().unwrap_or(0);
            if action == Some(QUESTION_NEXT) && index == 0 {
                self.remember_answers();
                self.modal = None;
                self.focus = Focus::Composer;
            } else {
                let next = if action == Some(QUESTION_PREVIOUS) {
                    (index + 1).min(self.question_slots().len().saturating_sub(1))
                } else {
                    index - 1
                };
                self.open_question(next);
            }
            return true;
        }
        if action == Some(QUESTION_SKIP) {
            self.submit_question(true);
            return true;
        }
        if key.code == KeyCode::Esc && self.question_note_focused {
            self.question_note_focused = false;
            return true;
        }
        if key.code == KeyCode::Esc {
            self.remember_answers();
            self.modal = None;
            self.focus = Focus::Composer;
            return true;
        }
        if matches!(action, Some(QUESTION_NEXT_FIELD | QUESTION_PREVIOUS_FIELD)) {
            let count = self.question_slots().len().max(1);
            let index = self.question_index().unwrap_or(0);
            self.open_question(if action == Some(QUESTION_NEXT_FIELD) {
                (index + 1) % count
            } else {
                (index + count - 1) % count
            });
            return true;
        }
        if action == Some(QUESTION_SUBMIT) {
            self.submit_question(false);
            return true;
        }
        if self.question_note_focused
            && let Some(note) = self.question_note_mut()
        {
            note.key(key);
            self.question_reveal = true;
            return true;
        }
        if action == Some(QUESTION_TOGGLE) && self.toggle_question_check() {
            self.remember_answers();
            return true;
        }
        if action == Some(QUESTION_NOTE) && self.question_note_key().is_some() {
            self.open_question_note();
            return true;
        }
        self.question_reveal = !matches!(action, Some(NAVIGATION_PAGE_UP | NAVIGATION_PAGE_DOWN));
        if let Some(Modal::Approval {
            pending,
            field,
            selected,
            answers,
            scroll,
            ..
        }) = &mut self.modal
        {
            let count = pending
                .params
                .get("questions")
                .and_then(Value::as_array)
                .and_then(|q| q.get(*field))
                .and_then(|q| q.get("options"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            match action {
                Some(NAVIGATION_UP) => *selected = selected.saturating_sub(1),
                Some(NAVIGATION_DOWN) => *selected = (*selected + 1).min(count),
                Some(NAVIGATION_PAGE_UP) => *scroll = scroll.saturating_sub(3),
                Some(NAVIGATION_PAGE_DOWN) => *scroll = scroll.saturating_add(3),
                _ => {
                    *selected = count;
                    if let Some(answer) = answers.get_mut(*field) {
                        answer.key(key);
                    }
                }
            }
        }
        self.remember_answers();
        true
    }
    pub(super) fn submit_question(&mut self, skip: bool) {
        if self.question_send.is_some() {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(Modal::Approval { pending, field, .. }) = &self.modal else {
            return;
        };
        let Some(question) = pending
            .params
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|q| q.get(*field))
        else {
            return;
        };
        let Some(question_id) = question.get("id").and_then(Value::as_str) else {
            return;
        };
        let answer = if skip {
            None
        } else {
            let Some(text) = self.selected_question_answer() else {
                return;
            };
            Some(text)
        };
        let request = pending.id.clone();
        let question_id = question_id.to_owned();
        let index = self.question_index().unwrap_or(0);
        self.remember_answers();
        let tweaks = self.preferences.motion.tweaks(self.tweaks);
        let clear = tweaks.capped(tweaks.input_clear_ms);
        if !self.inbox.visible && clear > 0 && !self.question_flip.drawn.is_empty() {
            self.question_flip.leaving = Some(Leaving {
                session: id.clone(),
                rows: self.question_flip.drawn.clone(),
                started: Instant::now(),
                duration: Duration::from_millis(u64::from(clear)),
                settled: false,
            });
        }
        self.question_send = Some((id.clone(), request.clone(), question_id.clone(), index));
        self.task(
            Task::Question,
            Request::Control {
                id,
                control: Control::AnswerQuestion {
                    request,
                    question: question_id,
                    answer,
                },
            },
            false,
        );
    }
    pub(super) fn question_received(&mut self, id: &str) {
        let Some((sent_session, request, question, index)) = self.question_send.clone() else {
            return;
        };
        if sent_session != id {
            return;
        }
        let Some(session) = self.sessions.get(id) else {
            return;
        };
        let unresolved = session.pending.iter().any(|p| {
            p.id == request
                && p.unanswered_questions()
                    .iter()
                    .any(|(_, q)| q.get("id").and_then(Value::as_str) == Some(question.as_str()))
        });
        if unresolved && !matches!(session.status, Status::Failed | Status::Interrupted) {
            return;
        }
        self.question_send = None;
        if unresolved {
            self.notice = Some((
                session.error.clone().unwrap_or_else(|| {
                    "Question was not delivered; your answer is retained".into()
                }),
                true,
            ));
        } else if !self.inbox.visible
            && self.selected.as_deref() == Some(id)
            && matches!(&self.modal, Some(Modal::Approval {pending,field,..}) if pending.id == request && pending.params.get("questions").and_then(Value::as_array).and_then(|q|q.get(*field)).and_then(|q|q.get("id")).and_then(Value::as_str) == Some(question.as_str()))
        {
            let count = self.question_slots().len();
            self.open_question(if count == 0 { 0 } else { index.min(count - 1) });
        }
    }
    /// The answered question still flipping away from the selected session.
    fn question_leaving(&self) -> Option<&Leaving> {
        self.question_flip
            .leaving
            .as_ref()
            .filter(|leaving| self.selected.as_deref() == Some(leaving.session.as_str()))
    }
    /// Whether the question panel replaces the composer: a question is open,
    /// or the last one answered is still flipping away.
    pub(super) fn question_panel(&self) -> bool {
        self.inline_question() || self.question_leaving().is_some()
    }
    pub(super) fn question_height(&self, width: u16) -> u16 {
        self.question_lines(width)
            .map(|(lines, _, _)| lines.len())
            .or_else(|| self.question_leaving().map(|leaving| leaving.rows.len()))
            .map(|rows| u16::try_from(rows.saturating_add(3)).unwrap_or(u16::MAX))
            .unwrap_or(0)
    }
    fn question_lines(&self, width: u16) -> Option<QuestionLines> {
        let Some(Modal::Approval { pending, field, .. }) = &self.modal else {
            return None;
        };
        self.inbox_question_lines(self.selected.as_deref()?, pending, *field, width)
    }
    pub(super) fn inbox_question_lines(
        &self,
        id: &str,
        pending: &Pending,
        field: usize,
        width: u16,
    ) -> Option<QuestionLines> {
        let questions = pending.params.get("questions")?.as_array()?;
        let question = questions.get(field)?;
        let position = self.positions.get(id);
        let request = pending.id.to_string();
        let active = self.selected.as_deref() == Some(id)
            && matches!(&self.modal, Some(Modal::Approval { pending: shown, field: shown_field, .. })
                if shown.id == pending.id && *shown_field == field);
        let saved = position.and_then(|p| p.answers.get(&request));
        let (answers, choice) = match &self.modal {
            Some(Modal::Approval {
                answers, selected, ..
            }) if active => (answers.clone(), *selected),
            _ => (
                saved
                    .map(|draft| draft.answers.clone())
                    .unwrap_or_else(|| vec![Editor::default(); questions.len()]),
                position
                    .and_then(|p| p.question_choices.get(&(request.clone(), field)))
                    .copied()
                    .unwrap_or(0),
            ),
        };
        let checks = position.and_then(|p| p.question_checks.get(&(request.clone(), field)));
        let note = position.and_then(|p| p.question_notes.get(&(request, field, choice)));
        let field = &field;
        let selected = &choice;
        let mut lines: Vec<_> = wrapped(
            question
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            width,
        )
        .into_iter()
        .map(|s| {
            Line::from(Span::styled(
                s,
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ))
        })
        .collect();
        let multi = question.get("multiSelect").and_then(Value::as_bool) == Some(true);
        if multi {
            lines.push(Line::from(Span::styled(
                "Choose one or more",
                Style::default().fg(DIM),
            )));
        }
        lines.push(Line::from(""));
        let options = question
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut hits = Vec::new();
        let mut cursor = None;
        for (index, option) in options.iter().enumerate() {
            let start = lines.len();
            let label = option
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let prefix = format!(
                "{} {}{}. ",
                if index == *selected { "›" } else { " " },
                if !multi {
                    ""
                } else if checks.is_some_and(|checks| checks.contains(&index)) {
                    "[x] "
                } else {
                    "[ ] "
                },
                index + 1
            );
            lines.extend(
                wrapped(&format!("{prefix}{label}"), width)
                    .into_iter()
                    .map(|s| {
                        Line::from(Span::styled(
                            s,
                            Style::default().fg(if index == *selected { ACCENT } else { TEXT }),
                        ))
                    }),
            );
            if let Some(description) = option
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                lines.extend(
                    wrapped(&format!("     {description}"), width)
                        .into_iter()
                        .map(|s| Line::from(Span::styled(s, Style::default().fg(DIM)))),
                );
            }
            hits.push((start, lines.len() - start, Action::QuestionChoice(index)));
            if index == *selected
                && let Some(note) = note
            {
                let start = lines.len();
                let prefix = "     Note: ";
                let indent = prefix.len().min(usize::from(width).saturating_sub(1));
                let mut display = note.clone();
                if question.get("isSecret").and_then(Value::as_bool) == Some(true) {
                    display.chars.fill('•');
                }
                let (rows, (x, y)) = display.styled_layout(
                    usize::from(width).saturating_sub(indent).max(1),
                    Style::default().bg(ACCENT).fg(crate::ui::INK),
                );
                if active && self.question_note_focused {
                    cursor = Some((indent + x, start + y));
                }
                for (row_index, mut row) in rows.into_iter().enumerate() {
                    row.spans.insert(
                        0,
                        Span::styled(
                            if row_index == 0 {
                                prefix.chars().take(indent).collect::<String>()
                            } else {
                                " ".repeat(indent)
                            },
                            Style::default().fg(DIM),
                        ),
                    );
                    if display.chars.is_empty() {
                        row.spans
                            .push(Span::styled("Add a note…", Style::default().fg(DIM)));
                    }
                    lines.push(row);
                }
                hits.push((start, lines.len() - start, Action::QuestionNote(index)));
            }
        }
        let start = lines.len();
        let prefix = if options.is_empty() {
            "› ".into()
        } else {
            format!(
                "{} {}. ",
                if *selected == options.len() {
                    "›"
                } else {
                    " "
                },
                options.len() + 1
            )
        };
        let prefix_width = unicode_width::UnicodeWidthStr::width(prefix.as_str());
        if let Some(answer) = answers.get(*field) {
            let mut answer = answer.clone();
            if question.get("isSecret").and_then(Value::as_bool) == Some(true) {
                answer.chars.fill('•');
            }
            let (rows, (x, y)) = answer.styled_layout(
                usize::from(width).saturating_sub(prefix_width).max(1),
                Style::default().bg(ACCENT).fg(crate::ui::INK),
            );
            if *selected == options.len() {
                cursor = Some((prefix_width + x, start + y));
            }
            if answer.chars.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default().fg(if *selected == options.len() {
                            ACCENT
                        } else {
                            TEXT
                        }),
                    ),
                    Span::styled(
                        if options.is_empty() {
                            "Type your answer"
                        } else {
                            "Other"
                        },
                        Style::default().fg(DIM),
                    ),
                ]));
            } else {
                for (index, mut row) in rows.into_iter().enumerate() {
                    row.spans.insert(
                        0,
                        Span::raw(if index == 0 {
                            prefix.clone()
                        } else {
                            " ".repeat(prefix_width)
                        }),
                    );
                    lines.push(row);
                }
            }
        }
        hits.push((
            start,
            lines.len().saturating_sub(start),
            Action::QuestionChoice(options.len()),
        ));
        Some((lines, cursor.filter(|_| active), hits))
    }
    pub(super) fn draw_question_tabs(&mut self, frame: &mut Frame, area: Rect) {
        if !self.inline_question() {
            return;
        }
        let slots = self.question_slots();
        if slots.len() <= 1 {
            return;
        }
        let index = self.question_index().unwrap_or(0);
        let mut x = area.x;
        for tab in 0..slots.len() {
            let label = format!(" {} ", tab + 1);
            let width = u16::try_from(label.len()).unwrap_or(0);
            if x.saturating_add(width) > area.right() {
                break;
            }
            self.button(
                frame,
                Rect::new(x, area.y, width, 1),
                &label,
                Action::InlineQuestion(tab),
                tab == index,
            );
            x = x.saturating_add(width);
        }
    }
    pub(super) fn draw_inline_question(&mut self, frame: &mut Frame, area: Rect) {
        let now = Instant::now();
        let available = area.height.saturating_sub(2) as usize;
        let rows_area = Rect::new(area.x, area.y, area.width, available as u16);
        let shown = self
            .question_lines(area.width)
            .map(|(lines, cursor, hits)| {
                let scroll = self.question_scroll(lines.len(), cursor, &hits, available);
                let rows: Vec<_> = lines.into_iter().skip(scroll).take(available).collect();
                (rows, cursor, hits, scroll)
            });
        let key = match (&self.modal, &self.selected) {
            (Some(Modal::Approval { pending, field, .. }), Some(session)) => {
                Some((session.clone(), pending.id.to_string(), *field))
            }
            _ => None,
        };
        let sending = self.question_send.is_some();
        let tweaks = self.preferences.motion.tweaks(self.tweaks);
        let flip = &mut self.question_flip;
        if let Some((rows, ..)) = &shown {
            flip.drawn.clone_from(rows);
        }
        // The answered question stays away until its answer is through.
        if flip.leaving.as_ref().is_some_and(|leaving| {
            (leaving.done(now) && !sending)
                || self.selected.as_deref() != Some(leaving.session.as_str())
        }) {
            flip.leaving = None;
            flip.board = flap::Board::default();
        }
        if let Some(leaving) = &flip.leaving {
            frame.render_widget(Paragraph::new(leaving.frame(now)), rows_area);
            if shown.is_some() {
                self.draw_question_bar(frame, area);
            }
            return;
        }
        let Some((mut rows, cursor, hits, scroll)) = shown else {
            return;
        };
        if flip.shown != key {
            flip.shown = key;
            flip.board = flap::Board::default();
        }
        if tweaks.agent_reply_cps > 0 && (flip.board.is_empty() || flip.board.animating(now)) {
            flip.board
                .show_over(&mut rows, now, crate::ui::PURPLE, area.width, |changed| {
                    flap::paced(tweaks, changed)
                });
        } else {
            flip.board.settle(&rows, now);
        }
        frame.render_widget(Paragraph::new(rows), rows_area);
        for (start, height, action) in hits {
            let top = start.max(scroll);
            let bottom = (start + height).min(scroll + available);
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
        if let Some((x, y)) = cursor
            && y >= scroll
            && y < scroll + available
            && x < usize::from(area.width)
        {
            frame.set_cursor_position((area.x + x as u16, area.y + (y - scroll) as u16));
        }
        self.draw_question_bar(frame, area);
    }
    /// Keeps the cursor or highlighted choice in view after it moves.
    fn question_scroll(
        &mut self,
        rows: usize,
        cursor: Option<(usize, usize)>,
        hits: &[(usize, usize, Action)],
        available: usize,
    ) -> usize {
        let reveal = std::mem::replace(&mut self.question_reveal, false);
        let Some(Modal::Approval {
            scroll, selected, ..
        }) = &mut self.modal
        else {
            return 0;
        };
        if reveal && available > 0 {
            let target = cursor.map(|(_, row)| row).or_else(|| {
                hits.iter()
                    .find(|(_, _, action)| matches!(action, Action::QuestionChoice(choice) if choice == selected))
                    .map(|(row, _, _)| *row)
            });
            if let Some(target) = target {
                if target < *scroll {
                    *scroll = target;
                } else if target >= scroll.saturating_add(available) {
                    *scroll = target.saturating_sub(available - 1);
                }
            }
        }
        *scroll = (*scroll).min(rows.saturating_sub(available));
        *scroll
    }
    fn draw_question_bar(&mut self, frame: &mut Frame, area: Rect) {
        use crate::shortcuts::{Bindings, ids::*};
        let bindings = Bindings::new(&self.shortcut_config);
        let active_hint = |id| bindings.primary_hint_in(id, self.shortcut_context());
        let hint = if self.question_send.is_some() {
            "Sending answer…".into()
        } else if self.question_note_focused {
            format!(
                "{} submit with note · Esc choices · {}/{} questions",
                active_hint(QUESTION_SUBMIT),
                active_hint(QUESTION_PREVIOUS),
                active_hint(QUESTION_NEXT)
            )
        } else if area.width < 80 {
            format!(
                "{} submit · {} note · Esc back",
                active_hint(QUESTION_SUBMIT),
                active_hint(QUESTION_NOTE)
            )
        } else {
            format!(
                "{}{} add note · {} submit · {} skip · {} previous/input · {} next",
                if self.multi_select_key().is_some() {
                    format!("{} choose · ", active_hint(QUESTION_TOGGLE))
                } else {
                    String::new()
                },
                active_hint(QUESTION_NOTE),
                active_hint(QUESTION_SUBMIT),
                active_hint(QUESTION_SKIP),
                active_hint(QUESTION_NEXT),
                active_hint(QUESTION_PREVIOUS)
            )
        };
        self.button(
            frame,
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
            &hint,
            Action::SubmitQuestion(false),
            false,
        );
    }
}

#[cfg(test)]
mod settings_refresh_tests {
    use super::*;
    use anyhow::Context;
    #[test]
    fn committed_refresh_settles_clearing_without_redisplaying_an_inflight_answer()
    -> anyhow::Result<()> {
        for change in 0..5 {
            let dir = tempfile::tempdir()?;
            let storage = Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            };
            let mut ui = Ui::new(storage.clone(), &Config::default());
            let mut session = Session::new(
                "one".into(),
                super::super::super::Job::Coding(super::super::super::Launch {
                    repository: "/synthetic/settings-preview".into(),
                    isolated: false,
                    base: "HEAD".into(),
                    prompt: "Sample".into(),
                    model: None,
                    effort: None,
                }),
            );
            session.status = Status::Idle;
            session.pending.push(Pending {id: serde_json::json!("request"), method: "item/tool/requestUserInput".into(), responded: true, params: serde_json::json!({"difuAsync":true,"questions":[{"id":"q","question":"Choose scope","options":[{"label":"Small"}]}]})});
            ui.sessions.insert("one".into(), session);
            ui.selected = Some("one".into());
            ui.drilled = true;
            ui.open_question(0);
            ui.positions.entry("one".into()).or_default().draft.chars =
                "Unsent draft".chars().collect();
            let now = Instant::now();
            ui.question_flip.leaving = Some(Leaving {
                session: "one".into(),
                rows: vec![Line::from("Choose scope")],
                started: now,
                duration: Duration::from_secs(10),
                settled: false,
            });
            let pending = Some(("one".into(), serde_json::json!("request"), "q".into(), 0));
            ui.question_send = pending.clone();
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 12))?;
            terminal.draw(|f| ui.draw_inline_question(f, f.area()))?;
            assert!(ui.question_flip.animating(Instant::now()));
            storage.update_config(|config| match change {
                0 => config.preferences.motion.sent_message = false,
                1 => config.tweaks.input_clear_ms = 0,
                2 => config.preferences.motion.level = crate::settings::Motion::Off,
                3 => config.preferences.motion.level = crate::settings::Motion::Reduced,
                _ => config.tweaks.agent_reply_cps = 0,
            })?;
            ui.apply_settings(&storage.refresh_settings()?);
            terminal.draw(|f| ui.draw_inline_question(f, f.area()))?;
            let screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(
                !ui.question_flip.animating(Instant::now()),
                "refresh {change} must finish clearing"
            );
            assert!(
                !screen.contains("Choose scope"),
                "refresh {change} must keep the answered question hidden"
            );
            assert!(screen.contains("Sending answer"));
            assert_eq!(ui.question_send, pending);
            assert_eq!(
                ui.positions
                    .get("one")
                    .context("position")?
                    .draft
                    .chars
                    .iter()
                    .collect::<String>(),
                "Unsent draft"
            );
            // A failed delivery makes the retained answer available for retry.
            ui.question_send = None;
            terminal.draw(|f| ui.draw_inline_question(f, f.area()))?;
            ui.question_flip.age(Duration::from_secs(2));
            terminal.draw(|f| ui.draw_inline_question(f, f.area()))?;
            let screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(screen.contains("Choose scope"));
            assert_eq!(ui.selected_question_answer().as_deref(), Some("Small"));
        }
        Ok(())
    }
}
