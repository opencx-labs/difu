use super::*;

type QuestionLines = (
    Vec<Line<'static>>,
    Option<(usize, usize)>,
    Vec<(usize, usize, Action)>,
);

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
        let text = if let Some(label) = question
            .get("options")
            .and_then(Value::as_array)
            .and_then(|options| options.get(*selected))
            .and_then(|option| option.get("label"))
            .and_then(Value::as_str)
        {
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
    fn question_index(&self) -> Option<usize> {
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
        if !self.inline_question() {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            let index = self.question_index().unwrap_or(0);
            if key.code == KeyCode::Down && index == 0 {
                self.remember_answers();
                self.modal = None;
                self.focus = Focus::Composer;
            } else {
                let next = if key.code == KeyCode::Up {
                    (index + 1).min(self.question_slots().len().saturating_sub(1))
                } else {
                    index - 1
                };
                self.open_question(next);
            }
            return true;
        }
        if key.code == KeyCode::Char(']') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            let count = self.question_slots().len().max(1);
            let index = self.question_index().unwrap_or(0);
            self.open_question(if key.code == KeyCode::Tab {
                (index + 1) % count
            } else {
                (index + count - 1) % count
            });
            return true;
        }
        if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
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
        if key.code == KeyCode::Char('n')
            && key.modifiers.is_empty()
            && self.question_note_key().is_some()
        {
            self.open_question_note();
            return true;
        }
        self.question_reveal = !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown);
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
            match key.code {
                KeyCode::Up if key.modifiers.is_empty() => *selected = selected.saturating_sub(1),
                KeyCode::Down if key.modifiers.is_empty() => *selected = (*selected + 1).min(count),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(3),
                KeyCode::PageDown => *scroll = scroll.saturating_add(3),
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
        if self.busy {
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
        self.busy = true;
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
        self.busy = false;
        self.question_send = None;
        if unresolved {
            self.notice = Some((
                session.error.clone().unwrap_or_else(|| {
                    "Question was not delivered; your answer is retained".into()
                }),
                true,
            ));
        } else if self.selected.as_deref() == Some(id)
            && matches!(&self.modal, Some(Modal::Approval {pending,field,..}) if pending.id == request && pending.params.get("questions").and_then(Value::as_array).and_then(|q|q.get(*field)).and_then(|q|q.get("id")).and_then(Value::as_str) == Some(question.as_str()))
        {
            let count = self.question_slots().len();
            self.open_question(if count == 0 { 0 } else { index.min(count - 1) });
        }
    }
    pub(super) fn question_height(&self, width: u16) -> u16 {
        self.question_lines(width)
            .map(|(lines, _, _)| u16::try_from(lines.len().saturating_add(4)).unwrap_or(u16::MAX))
            .unwrap_or(0)
    }
    fn question_lines(&self, width: u16) -> Option<QuestionLines> {
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
        if !self.inline_question() {
            return None;
        }
        let question = pending
            .params
            .get("questions")
            .and_then(Value::as_array)?
            .get(*field)?;
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
                "{} {}. ",
                if index == *selected { "›" } else { " " },
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
                && let Some(note) = self.question_note()
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
                if self.question_note_focused {
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
        Some((lines, cursor, hits))
    }
    pub(super) fn draw_inline_question(&mut self, frame: &mut Frame, area: Rect) {
        let Some((lines, cursor, hits)) = self.question_lines(area.width) else {
            return;
        };
        let slots = self.question_slots();
        let index = self.question_index().unwrap_or(0);
        frame.render_widget(
            Paragraph::new(format!("{} of {}", index + 1, slots.len()))
                .style(Style::default().fg(DIM)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let mut x = area.x.saturating_add(10);
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
        let available = area.height.saturating_sub(3) as usize;
        let scroll = match &mut self.modal {
            Some(Modal::Approval {
                scroll, selected, ..
            }) => {
                if self.question_reveal && available > 0 {
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
                self.question_reveal = false;
                *scroll = (*scroll).min(lines.len().saturating_sub(available));
                *scroll
            }
            _ => 0,
        };
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(scroll)
                    .take(available)
                    .collect::<Vec<_>>(),
            ),
            Rect::new(area.x, area.y + 1, area.width, available as u16),
        );
        for (start, height, action) in hits {
            let top = start.max(scroll);
            let bottom = (start + height).min(scroll + available);
            if bottom > top {
                self.hits.push((
                    Rect::new(
                        area.x,
                        area.y + 1 + (top - scroll) as u16,
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
            frame.set_cursor_position((area.x + x as u16, area.y + 1 + (y - scroll) as u16));
        }
        self.button(
            frame,
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
            if self.busy {
                "Sending answer…"
            } else {
                if self.question_note_focused {
                    "Enter submit with note · Esc choices · Alt+↑↓ questions"
                } else {
                    "n add note · Enter submit · Ctrl+] skip · Alt+↓ previous/input · Alt+↑ next"
                }
            },
            Action::SubmitQuestion(false),
            false,
        );
    }
}
