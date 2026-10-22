//! Shared production toast appearance for delivered notices and local Settings samples.
use crate::ui::{BG, GREEN, RED};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

pub(crate) const LIFETIME: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn draw(frame: &mut Frame, area: Rect, notice: &str, error: bool) -> Rect {
    let width = area.width.saturating_sub(2).min(64);
    if width < 4 {
        return Rect::default();
    }
    let text = crate::model::clean(notice);
    let rows = super::wrapped(&text, width.saturating_sub(2));
    let height = (rows.len() as u16)
        .saturating_add(2)
        .min(area.height.saturating_sub(2));
    if height < 3 {
        return Rect::default();
    }
    let rect = Rect::new(
        area.right().saturating_sub(width + 1),
        area.bottom().saturating_sub(height + 1),
        width,
        height,
    );
    let color = if error { RED } else { GREEN };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color)),
            )
            .style(Style::default().fg(color).bg(BG)),
        rect,
    );
    rect
}
