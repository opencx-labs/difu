use super::*;
pub(super) struct History {
    drafts: Vec<Editor>,
    index: usize,
}
impl Ui {
    pub(super) fn history_key(&mut self, key: KeyEvent) -> bool {
        if !key.modifiers.is_empty() || !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            return false;
        }
        let Some(id) = self.selected.as_ref() else {
            return false;
        };
        let Some(position) = self.positions.get_mut(id) else {
            return false;
        };
        if position.history.is_none() {
            if key.code != KeyCode::Up || !position.draft.chars.is_empty() {
                return false;
            }
            let mut drafts = self
                .sessions
                .get(id)
                .map(|s| {
                    s.entries
                        .iter()
                        .filter(|e| e.kind == "userMessage")
                        .map(|e| Editor::from(e.text.as_str()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if drafts.is_empty() {
                return true;
            }
            let index = drafts.len();
            drafts.push(position.draft.clone());
            position.history = Some(History { drafts, index });
        }
        let Some(history) = &mut position.history else {
            return false;
        };
        if let Some(draft) = history.drafts.get_mut(history.index) {
            *draft = position.draft.clone();
        }
        history.index = if key.code == KeyCode::Up {
            history.index.saturating_sub(1)
        } else {
            history
                .index
                .saturating_add(1)
                .min(history.drafts.len().saturating_sub(1))
        };
        if let Some(draft) = history.drafts.get(history.index) {
            position.draft = draft.clone();
        }
        if history.index == history.drafts.len().saturating_sub(1) {
            position.history = None;
        }
        true
    }

    pub(super) fn latest_prompt(&self) -> Option<String> {
        self.selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))?
            .entries
            .iter()
            .rev()
            .find(|e| e.kind == "userMessage")
            .map(|e| e.text.clone())
    }
    pub(super) fn prompt_header(&mut self, frame: &mut Frame, body: Rect, text: &str) -> Rect {
        if body.height < 3 || body.width < 4 {
            return body;
        }
        let style = if self.focus == Focus::Prompt {
            crate::ui::user_message_style().bg(ratatui::style::Color::Rgb(16, 39, 25))
        } else {
            crate::ui::user_message_style()
        };
        let rows = wrapped(text, body.width.saturating_sub(2));
        let height = rows
            .len()
            .clamp(1, 3)
            .min(usize::from(body.height / 3).max(1));
        let rect = Rect::new(body.x, body.y, body.width, height as u16);
        let mut lines = rows
            .into_iter()
            .take(height)
            .enumerate()
            .map(|(i, line)| {
                Line::from(format!("{}{line}", if i == 0 { "› " } else { "  " })).style(style)
            })
            .collect::<Vec<_>>();
        if let Some(line) = lines.last_mut() {
            let hint = if self.focus == Focus::Prompt {
                " ↵ Expand"
            } else {
                " · Latest prompt"
            };
            let text = line.to_string();
            *line = Line::from(format!(
                "{}{hint}",
                crate::ui::crop(
                    &text,
                    0,
                    usize::from(body.width)
                        .saturating_sub(unicode_width::UnicodeWidthStr::width(hint))
                )
            ))
            .style(style);
        }
        self.text_selection.register(0, rect, 0, &lines);
        frame.render_widget(Paragraph::new(lines).style(style), rect);
        self.hits.push((rect, Action::ShowPrompt(text.to_owned())));
        Rect::new(
            body.x,
            body.y + height as u16 + 1,
            body.width,
            body.height.saturating_sub(height as u16 + 1),
        )
    }
    pub(super) fn prompt_key(&mut self, key: KeyEvent) -> bool {
        if let Some(Modal::Prompt { scroll, .. }) = &mut self.modal {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.modal = None;
                    self.text_selection.clear();
                }
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = scroll.saturating_add(1),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                KeyCode::Home => *scroll = 0,
                KeyCode::End => *scroll = usize::MAX,
                _ => {}
            }
            return true;
        }
        false
    }
    pub(super) fn draw_prompt(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(Modal::Prompt { text, scroll }) = &mut self.modal {
            let rows = wrapped(text, area.width);
            *scroll = (*scroll).min(rows.len().saturating_sub(area.height as usize));
            self.text_selection.frame();
            let lines = rows
                .iter()
                .map(|row| Line::from(row.clone()))
                .collect::<Vec<_>>();
            self.text_selection.register(2, area, *scroll, &lines);
            frame.render_widget(
                Paragraph::new(
                    rows.into_iter()
                        .skip(*scroll)
                        .take(area.height as usize)
                        .map(Line::from)
                        .collect::<Vec<_>>(),
                ),
                area,
            );
            self.text_selection.highlight(frame);
        }
    }
}
