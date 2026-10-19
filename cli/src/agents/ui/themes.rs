//! `/theme`: pick a colour theme with a live preview. Default restores difu's
//! original colours.
use super::*;
use crate::theme::{self, Role};

/// Swatches show a theme's own colours, which the frame passes through unchanged.
const SWATCHES: [Role; 8] = [
    Role::Accent,
    Role::Text,
    Role::Keyword,
    Role::String,
    Role::Number,
    Role::Green,
    Role::Red,
    Role::Purple,
];

/// Longest theme name, so swatches line up.
pub(super) const NAME_WIDTH: usize = 16;

/// A compact panel on the right leaves most of the screen visible, so moving
/// through the list previews each theme on the real conversation.
pub(super) fn rect(full: Rect) -> Rect {
    // Two borders, a cursor, name, swatches and the saved mark.
    let width = full.width.saturating_sub(2).min(NAME_WIDTH as u16 + 25);
    let height = full
        .height
        .saturating_sub(2)
        .min(theme::themes().len() as u16 + 5);
    Rect::new(
        full.x + full.width.saturating_sub(width + 1),
        full.y + 1,
        width,
        height,
    )
}

impl Ui {
    pub(super) fn open_theme(&mut self) {
        self.modal = Some(Modal::Theme {
            selected: self.theme,
        });
    }
    /// Previews last only while the picker is open.
    pub(super) fn tick_theme(&mut self) {
        if !matches!(self.modal, Some(Modal::Theme { .. })) && theme::active_index() != self.theme {
            theme::select(self.theme);
        }
    }
    pub(super) fn preview_theme(&mut self, index: usize) {
        if let Some(Modal::Theme { selected }) = &mut self.modal {
            *selected = index.min(theme::themes().len().saturating_sub(1));
            theme::select(*selected);
        }
    }
    pub(super) fn theme_key(&mut self, key: KeyEvent) {
        let Some(Modal::Theme { selected }) = &self.modal else {
            return;
        };
        let (selected, count) = (*selected, theme::themes().len());
        match key.code {
            KeyCode::Esc => {
                self.modal = None;
                self.tick_theme();
            }
            KeyCode::Up | KeyCode::BackTab => self.preview_theme((selected + count - 1) % count),
            KeyCode::Down | KeyCode::Tab => self.preview_theme((selected + 1) % count),
            KeyCode::Home => self.preview_theme(0),
            KeyCode::End => self.preview_theme(count.saturating_sub(1)),
            KeyCode::Enter => self.save_theme(),
            _ => {}
        }
    }
    pub(super) fn save_theme(&mut self) {
        let Some(Modal::Theme { selected }) = &self.modal else {
            return;
        };
        let selected = *selected;
        let Some(choice) = theme::themes().get(selected) else {
            return;
        };
        let saved = self
            .storage
            .update_config(|config| config.theme = (selected != 0).then(|| choice.id.to_owned()));
        match saved {
            Ok(_) => {
                self.theme = selected;
                theme::select(selected);
                self.modal = None;
                self.notice = Some((format!("Theme: {}", choice.name), false));
            }
            Err(error) => {
                self.notice = Some((format!("Could not save theme: {error:#}"), true));
            }
        }
    }
    pub(super) fn draw_theme(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::Theme { selected }) = &self.modal else {
            return;
        };
        let selected = *selected;
        // Below the list: the highlighted theme's description and the keys.
        let footer = 3;
        let rows = usize::from(area.height.saturating_sub(footer)).max(1);
        let first = selected.saturating_sub(rows - 1);
        for (offset, (index, choice)) in theme::themes()
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .enumerate()
        {
            let row = Rect::new(area.x, area.y.saturating_add(offset as u16), area.width, 1);
            let active = index == selected;
            let mut line = vec![Span::styled(
                format!(
                    "{} {:NAME_WIDTH$} ",
                    if active { "›" } else { " " },
                    choice.name
                ),
                Style::default().fg(if active { ACCENT } else { TEXT }),
            )];
            let background = choice.color(Role::Background);
            line.extend(SWATCHES.iter().map(|role| {
                Span::styled(
                    "██",
                    Style::default().fg(choice.color(*role)).bg(background),
                )
            }));
            if index == self.theme {
                line.push(Span::styled(" ✓", Style::default().fg(DIM)));
            }
            frame.render_widget(Paragraph::new(Line::from(line)), row);
            self.hits.push((row, Action::ThemeChoice(index)));
        }
        if let Some(choice) = theme::themes().get(selected) {
            frame.render_widget(
                Paragraph::new(format!(
                    "{}{}",
                    choice.description,
                    if selected == self.theme {
                        " · saved"
                    } else {
                        ""
                    }
                ))
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(DIM)),
                Rect::new(
                    area.x,
                    area.bottom().saturating_sub(footer),
                    area.width,
                    footer - 1,
                ),
            );
        }
        frame.render_widget(
            Paragraph::new("↑↓ preview · Enter apply · Esc cancel").style(Style::default().fg(DIM)),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }
}
