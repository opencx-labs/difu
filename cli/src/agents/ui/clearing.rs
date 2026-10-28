//! Clearing feedback follows the session, including background worktree preparation.
use super::*;
use ratatui::{buffer::Buffer, style::Color};

pub(super) struct State {
    pub requests: HashSet<String>,
    started: Instant,
}

impl Default for State {
    fn default() -> Self {
        Self {
            requests: HashSet::new(),
            started: Instant::now(),
        }
    }
}

impl Ui {
    pub(super) fn is_clearing(&self, id: &str) -> bool {
        if self.clearing.requests.contains(id) {
            return true;
        }
        let summary = self.summaries.iter().find(|s| s.id == id);
        self.sessions
            .get(id)
            .filter(|s| summary.is_none_or(|summary| s.version >= summary.version))
            .map(|s| s.clearing)
            .or_else(|| summary.map(|s| s.clearing))
            .unwrap_or(false)
    }

    pub(super) fn clearing_selected(&self) -> bool {
        !self.inbox.visible
            && self
                .selected
                .as_deref()
                .is_some_and(|id| self.is_clearing(id))
    }

    pub(super) fn draw_clearing(&self, frame: &mut Frame, area: Rect) {
        if self.clearing_selected() {
            pulse(
                frame.buffer_mut(),
                area,
                self.clearing.started.elapsed(),
                self.preferences.motion.level == crate::settings::Motion::Full,
            );
        }
    }
}

pub(super) fn icon(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("🧹").style(Style::default().fg(ACCENT)),
        area,
    );
}

/// Terminals have no per-cell opacity. Gently blend text toward its background,
/// leaving the sidebar, dialogs, and terminal background untouched.
fn pulse(buffer: &mut Buffer, area: Rect, elapsed: Duration, animated: bool) {
    let wave = (elapsed.as_secs_f64() * std::f64::consts::TAU / 2.4).cos();
    let opacity = if animated { 0.78 + 0.10 * wave } else { 0.78 };
    for position in area.intersection(buffer.area).positions() {
        let Some(cell) = buffer.cell_mut(position) else {
            continue;
        };
        let foreground = match crate::theme::resolve(cell.fg) {
            Color::Reset => crate::theme::resolve(TEXT),
            color => color,
        };
        let background = match crate::theme::resolve(cell.bg) {
            // The terminal owns transparent backgrounds; use the theme's
            // muted text color instead of guessing whether they are light/dark.
            Color::Reset => crate::theme::resolve(DIM),
            color => color,
        };
        let [r, g, b] = status_icons::rgb(foreground);
        let [br, bg, bb] = status_icons::rgb(background);
        let blend = |from: u8, to: u8| {
            (f64::from(to) + f64::from(i16::from(from) - i16::from(to)) * opacity).round() as u8
        };
        cell.fg = Color::Rgb(blend(r, br), blend(g, bg), blend(b, bb));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_keeps_text_backgrounds_and_surrounding_ui_intact() -> Result<()> {
        let mut original = Buffer::empty(Rect::new(0, 0, 4, 2));
        for cell in &mut original.content {
            cell.set_symbol("x").set_fg(TEXT).set_bg(BG);
        }
        let area = Rect::new(2, 0, 2, 2);
        let render = |elapsed, animated| {
            let mut buffer = original.clone();
            pulse(&mut buffer, area, elapsed, animated);
            buffer
        };
        let bright = render(Duration::ZERO, true);
        let dim = render(Duration::from_millis(1200), true);
        assert_ne!(bright.cell((2, 0)), dim.cell((2, 0)));
        for position in original.area.positions() {
            let cell = dim.cell(position).context("cell")?;
            let before = original.cell(position).context("original")?;
            assert_eq!(cell.symbol(), before.symbol());
            assert_eq!(cell.bg, before.bg);
            if !area.contains(position) {
                assert_eq!(cell, before);
            }
        }
        assert_eq!(
            render(Duration::ZERO, false),
            render(Duration::from_secs(1), false)
        );
        Ok(())
    }
}
