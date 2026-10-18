//! Symbol hover and terminal underline styling. Command depends on enhanced
//! keyboard reports; Ctrl also works through ordinary mouse reports.
use crossterm::{
    cursor::{MoveTo, RestorePosition, SavePosition},
    event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseEvent},
    queue,
    style::{Attribute, Print, SetAttribute, SetStyle},
};
use ratatui::{
    backend::IntoCrossterm,
    buffer::Buffer,
    layout::{Position, Rect},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub struct State {
    pub position: Option<Position>,
    pub rect: Option<Rect>,
    command: bool,
    control: bool,
    mouse_control: bool,
}
impl State {
    pub fn active(&self) -> bool {
        self.command || self.control || self.mouse_control
    }
    /// Returns true for events which must never trigger an application action.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let pressed = key.kind != KeyEventKind::Release;
        match key.code {
            KeyCode::Modifier(ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper) => {
                self.command = pressed
            }
            KeyCode::Modifier(ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl) => {
                self.control = pressed;
                self.mouse_control = pressed;
            }
            KeyCode::Modifier(_) => {}
            _ => {
                self.command = key.modifiers.contains(KeyModifiers::SUPER);
                self.control = key.modifiers.contains(KeyModifiers::CONTROL);
                self.mouse_control = self.control;
            }
        }
        !pressed || matches!(key.code, KeyCode::Modifier(_))
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        self.position = Some(Position::new(event.column, event.row));
        self.control = event.modifiers.contains(KeyModifiers::CONTROL);
        self.mouse_control = event.modifiers.contains(KeyModifiers::SUPER);
    }
    /// Ratatui tracks the underline in its buffer so it erases on hover exit.
    /// Its style type has no dotted variant; refine the hovered cells after the
    /// frame using Crossterm, retaining their colors and the input cursor.
    pub fn render(&self, output: &mut impl Write, buffer: &Buffer) -> io::Result<()> {
        let Some(rect) = self.rect else { return Ok(()) };
        queue!(output, SavePosition)?;
        let mut x = rect.x;
        while x < rect.right() {
            let Some(cell) = buffer.cell((x, rect.y)) else {
                break;
            };
            queue!(
                output,
                MoveTo(x, rect.y),
                SetAttribute(Attribute::Reset),
                SetStyle(cell.style().into_crossterm()),
                SetAttribute(if self.active() {
                    Attribute::Underlined
                } else {
                    Attribute::Underdotted
                }),
                Print(cell.symbol())
            )?;
            let width = u16::try_from(cell.symbol().width().max(1)).unwrap_or(1);
            x = x.saturating_add(width);
        }
        queue!(output, SetAttribute(Attribute::Reset), RestorePosition)?;
        output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_release_and_control_mouse_reports_update_hover() {
        let mut state = State::default();
        assert!(state.key(KeyEvent::new(
            KeyCode::Modifier(ModifierKeyCode::LeftSuper),
            KeyModifiers::SUPER
        )));
        assert!(state.active());
        state.key(KeyEvent::new_with_kind(
            KeyCode::Modifier(ModifierKeyCode::LeftSuper),
            KeyModifiers::SUPER,
            KeyEventKind::Release,
        ));
        assert!(!state.active());
        state.mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 2,
            row: 3,
            modifiers: KeyModifiers::CONTROL,
        });
        assert!(state.active());
        state.mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!state.active());
    }
    #[test]
    fn overlay_emits_dotted_or_solid_underlines_and_restores_cursor() -> io::Result<()> {
        let mut state = State {
            rect: Some(Rect::new(0, 0, 3, 1)),
            ..Default::default()
        };
        let buffer = Buffer::with_lines(["foo"]);
        let mut output = Vec::new();
        state.render(&mut output, &buffer)?;
        assert!(String::from_utf8_lossy(&output).contains("\x1b[4:4m"));
        state.control = true;
        output.clear();
        state.render(&mut output, &buffer)?;
        let sequence = String::from_utf8_lossy(&output);
        assert!(sequence.contains("\x1b[4m"));
        assert!(!sequence.contains("\x1b[4:4m"));
        assert!(sequence.ends_with("\x1b8"));
        Ok(())
    }
}
