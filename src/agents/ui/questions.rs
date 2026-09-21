use super::*;

#[derive(Clone)]
pub struct Draft {
    pub answers: Vec<Editor>,
    pub field: usize,
    pub selected: usize,
}
impl Ui {
    pub(super) fn remember_answers(&mut self) {
        if let Some(Modal::Approval {
            pending,
            answers,
            field,
            selected,
            ..
        }) = &self.modal
            && let Some(id) = &self.selected
        {
            self.positions
                .entry(id.clone())
                .or_default()
                .question_choices
                .insert((pending.id.to_string(), *field), *selected);
            self.positions
                .entry(id.clone())
                .or_default()
                .answers
                .insert(
                    pending.id.to_string(),
                    Draft {
                        answers: answers.clone(),
                        field: *field,
                        selected: *selected,
                    },
                );
        }
    }
    pub(super) fn edit_queued(&mut self, index: usize) {
        if let Some(prompt) = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .and_then(|s| s.queue.get(index))
            .cloned()
        {
            self.modal = Some(Modal::QueuedEdit {
                index,
                editor: prompt.text().into(),
                expected: prompt,
                focus: 0,
            });
        }
    }
    pub(super) fn save_queued(&mut self, delete: bool) {
        let Some(Modal::QueuedEdit {
            index,
            expected,
            editor,
            ..
        }) = &self.modal
        else {
            return;
        };
        if !delete && editor.text().trim().is_empty() {
            self.notice = Some(("Enter a message, or choose Remove".into(), true));
            return;
        }
        let replacement = (!delete).then(|| Prompt::WithSkills {
            text: editor.text(),
            skills: expected.skills().to_vec(),
            attachments: expected
                .attachments()
                .iter()
                .filter(|a| editor.text().contains(&a.token()))
                .cloned()
                .collect(),
        });
        self.control(Control::ReplaceQueued {
            index: *index,
            expected: expected.clone(),
            replacement,
        });
    }
    pub(super) fn question_or_queue_key(&mut self, key: KeyEvent) -> bool {
        let mut action = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let session = self.selected.as_ref().and_then(|id| self.sessions.get(id));
        let queue = matches!(self.modal, Some(Modal::Queue { .. }));
        match &mut self.modal {
            Some(Modal::Pending { selected }) | Some(Modal::Queue { selected }) => {
                let count = session.map_or(0, |s| {
                    if queue {
                        s.queue.len()
                    } else {
                        s.pending.len()
                    }
                });
                match key.code {
                    KeyCode::Esc => self.modal = None,
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        *selected = selected.saturating_add(1).min(count.saturating_sub(1))
                    }
                    KeyCode::Enter if count > 0 => {
                        action = Some(if queue {
                            Action::EditQueued(*selected)
                        } else {
                            Action::Approval(*selected)
                        })
                    }
                    _ => {}
                }
            }
            Some(Modal::QueuedEdit { editor, focus, .. }) => match key.code {
                KeyCode::Esc => self.modal = Some(Modal::Queue { selected: 0 }),
                KeyCode::Tab => *focus = (*focus + 1) % 3,
                KeyCode::BackTab => *focus = (*focus + 2) % 3,
                KeyCode::Enter if ctrl || *focus == 1 => action = Some(Action::SaveQueued(false)),
                KeyCode::Enter if *focus == 2 => action = Some(Action::SaveQueued(true)),
                _ if *focus == 0 => editor.key(key),
                _ => {}
            },
            Some(Modal::Approval {
                pending,
                answers,
                field,
                selected,
                scroll,
                ..
            }) if pending.method == "item/tool/requestUserInput" => {
                let questions = pending.params.get("questions").and_then(Value::as_array);
                let count = questions.map_or(0, Vec::len);
                let options = questions
                    .and_then(|q| q.get(*field))
                    .and_then(|q| q.get("options"))
                    .and_then(Value::as_array);
                let options_count = options.map_or(0, Vec::len);
                match key.code {
                    KeyCode::Esc => {
                        self.remember_answers();
                        self.modal = None;
                    }
                    KeyCode::Tab | KeyCode::BackTab if count > 0 => {
                        *field = if key.code == KeyCode::BackTab {
                            (*field + count - 1) % count
                        } else {
                            (*field + 1) % count
                        };
                        *selected = 0;
                    }
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(5),
                    KeyCode::PageDown => *scroll = scroll.saturating_add(5),
                    KeyCode::Up if !shift => *selected = selected.saturating_sub(1),
                    KeyCode::Down if !shift => {
                        *selected = selected.saturating_add(1).min(options_count)
                    }
                    KeyCode::Enter if ctrl => action = Some(Action::Approve(0)),
                    KeyCode::Enter if *selected < options_count => {
                        if let Some(label) = options
                            .and_then(|a| a.get(*selected))
                            .and_then(|o| o.get("label"))
                            .and_then(Value::as_str)
                            && let Some(answer) = answers.get_mut(*field)
                        {
                            *answer = label.into();
                        }
                        if *field + 1 < count {
                            *field += 1;
                            *selected = 0;
                        }
                    }
                    _ => {
                        *selected = options_count;
                        if let Some(answer) = answers.get_mut(*field) {
                            answer.key(key);
                        }
                    }
                }
            }
            _ => return false,
        }
        if let Some(action) = action {
            self.action(action);
        }
        true
    }
    pub(super) fn draw_questions_or_queue(&mut self, frame: &mut Frame, area: Rect) -> bool {
        let session = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .cloned();
        match &self.modal {
            Some(Modal::Pending { selected }) | Some(Modal::Queue { selected }) => {
                let queue = matches!(self.modal, Some(Modal::Queue { .. }));
                let selected = *selected;
                let labels = session
                    .as_ref()
                    .map(|s| {
                        if queue {
                            s.queue
                                .iter()
                                .map(|p| crate::model::clean(p.text()))
                                .collect::<Vec<_>>()
                        } else {
                            s.pending
                                .iter()
                                .map(|p| {
                                    if p.method == "item/tool/requestUserInput" {
                                        p.params
                                            .get("questions")
                                            .and_then(Value::as_array)
                                            .and_then(|q| q.first())
                                            .and_then(|q| q.get("question"))
                                            .and_then(Value::as_str)
                                            .map(crate::model::clean)
                                            .unwrap_or_else(|| "Question".into())
                                    } else {
                                        format!("Approval · {}", crate::model::clean(&p.method))
                                    }
                                })
                                .collect()
                        }
                    })
                    .unwrap_or_default();
                if labels.is_empty() {
                    frame.render_widget(
                        Paragraph::new(if queue {
                            "No queued messages"
                        } else {
                            "No unanswered requests"
                        }),
                        area,
                    );
                }
                let capacity = usize::from(area.height.saturating_sub(2)).max(1);
                let start = selected.saturating_sub(capacity.saturating_sub(1));
                for (index, label) in labels.iter().enumerate().skip(start).take(capacity) {
                    self.button(
                        frame,
                        Rect::new(area.x, area.y + (index - start) as u16, area.width, 1),
                        label,
                        if queue {
                            Action::EditQueued(index)
                        } else {
                            Action::Approval(index)
                        },
                        index == selected,
                    );
                }
                frame.render_widget(
                    Paragraph::new("↑↓ choose · Enter open · Esc close")
                        .style(Style::default().fg(DIM)),
                    Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                );
            }
            Some(Modal::QueuedEdit {
                editor: value,
                focus,
                ..
            }) => {
                let focus = *focus;
                self.hits.push((
                    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(4)),
                    Action::QueueEditorFocus,
                ));
                editor(
                    frame,
                    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(4)),
                    "Message",
                    value,
                    focus == 0,
                );
                self.button(
                    frame,
                    Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 1),
                    "Save changes · Ctrl+Enter",
                    Action::SaveQueued(false),
                    focus == 1,
                );
                self.button(
                    frame,
                    Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                    "Remove from queue",
                    Action::SaveQueued(true),
                    focus == 2,
                );
            }
            Some(Modal::Approval {
                pending,
                answers,
                field,
                selected,
                scroll,
                ..
            }) if pending.method == "item/tool/requestUserInput" => {
                let field = *field;
                let selected = *selected;
                let scroll = *scroll;
                let answers = answers.clone();
                let questions = pending
                    .params
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let Some(question) = questions.get(field) else {
                    return true;
                };
                let mut x = area.x;
                for (index, q) in questions.iter().enumerate() {
                    let label = format!(
                        " {}{} ",
                        if answers
                            .get(index)
                            .is_some_and(|a| !a.text().trim().is_empty())
                        {
                            "✓ "
                        } else {
                            ""
                        },
                        q.get("header")
                            .and_then(Value::as_str)
                            .unwrap_or("Question")
                    );
                    let width = (unicode_width::UnicodeWidthStr::width(label.as_str()) as u16)
                        .min(area.right().saturating_sub(x));
                    self.button(
                        frame,
                        Rect::new(x, area.y, width, 1),
                        &label,
                        Action::QuestionTab(index),
                        index == field,
                    );
                    x = x.saturating_add(width + 1);
                }
                let text = question
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let lines = wrapped(text, area.width);
                let count = lines
                    .len()
                    .min(usize::from(area.height.saturating_sub(15)).max(3));
                let scroll = scroll.min(lines.len().saturating_sub(count));
                frame.render_widget(
                    Paragraph::new(
                        lines
                            .into_iter()
                            .skip(scroll)
                            .take(count)
                            .map(Line::from)
                            .collect::<Vec<_>>(),
                    ),
                    Rect::new(area.x, area.y + 2, area.width, count as u16),
                );
                let mut y = area.y.saturating_add(3 + count as u16);
                let options = question
                    .get("options")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for (index, option) in options.iter().enumerate() {
                    if y >= area.bottom().saturating_sub(5) {
                        break;
                    }
                    let label = option
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.button(
                        frame,
                        Rect::new(area.x, y, area.width, 1),
                        label,
                        Action::Answer(field, label.into()),
                        selected == index,
                    );
                    y += 1;
                    let description = option
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !description.is_empty() && y < area.bottom().saturating_sub(5) {
                        frame.render_widget(
                            Paragraph::new(crate::model::clean(description))
                                .style(Style::default().fg(DIM)),
                            Rect::new(area.x + 2, y, area.width.saturating_sub(2), 1),
                        );
                        y += 1;
                    }
                }
                if let Some(answer) = answers.get(field) {
                    let mut display = answer.clone();
                    if question.get("isSecret").and_then(Value::as_bool) == Some(true) {
                        display.chars.fill('•');
                    }
                    self.hits.push((
                        Rect::new(area.x, y, area.width, area.bottom().saturating_sub(y + 2)),
                        Action::AnswerFocus,
                    ));
                    editor(
                        frame,
                        Rect::new(area.x, y, area.width, area.bottom().saturating_sub(y + 2)),
                        "Your answer · PgUp/PgDown scroll question",
                        &display,
                        selected >= options.len(),
                    );
                }
                self.button(
                    frame,
                    Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                    "Submit answers · Ctrl+Enter    Tab next question · Esc dismiss",
                    Action::Approve(0),
                    false,
                );
            }
            _ => return false,
        }
        true
    }
}
