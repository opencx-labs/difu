use super::*;
use crate::shortcuts::ids;
use crate::ui::{ACCENT, DIM, PANEL, TEXT};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

impl Panel {
    /// Keep each shortcut and label together, wrapping whole commands as needed.
    pub(super) fn footer_lines(&self, width: u16) -> Vec<Line<'static>> {
        let compact = width < 65;
        let hint = |id| {
            let hint = self
                .binding_hint(id)
                .split(" / ")
                .next()
                .unwrap_or_default()
                .to_owned();
            if compact {
                hint.replace("Ctrl+", "^")
                    .replace("Shift+", "⇧")
                    .replace("PageUp", "PgUp")
                    .replace("PageDown", "PgDn")
                    .replace("Up", "↑")
                    .replace("Down", "↓")
            } else {
                hint
            }
        };
        let mut commands = vec![
            (hint(ids::SETTINGS_SAVE), "Save"),
            (hint(ids::SETTINGS_DISCARD), "Discard"),
        ];
        match self.rows().get(self.selected).map(|spec| spec.preview) {
            Some(Preview::Animation) => commands.push((hint(ids::SETTINGS_REPLAY), "Replay")),
            Some(Preview::Shortcut | Preview::Notification) => {
                commands.push((hint(ids::SETTINGS_TEST), "Test"))
            }
            _ => {}
        }
        commands.extend([
            (hint(ids::SETTINGS_DETAILS), "Details"),
            ("Esc".into(), "Close"),
            (hint(ids::SETTINGS_FOCUS), "Focus"),
            (
                format!("{}/{}", hint(ids::SETTINGS_UP), hint(ids::SETTINGS_DOWN)),
                "Move",
            ),
            (hint(ids::SETTINGS_ACTIVATE), "Edit"),
            (hint(ids::SETTINGS_SEARCH), "Search"),
            (hint(ids::SETTINGS_RESET_SECTION), "Reset"),
            (hint(ids::SETTINGS_EXPORT), "Export"),
            (hint(ids::SETTINGS_MODIFIED), "Modified"),
            (
                format!(
                    "{}/{}",
                    hint(ids::SETTINGS_DETAILS_UP),
                    hint(ids::SETTINGS_DETAILS_DOWN)
                ),
                "Scroll",
            ),
        ]);
        if self.edit.is_some() {
            commands = vec![
                (hint(ids::SETTINGS_EDIT_ACCEPT), "Apply"),
                ("Esc".into(), "Cancel"),
            ];
        }
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        let mut used = 0;
        let width = usize::from(width);
        for (key, label) in commands {
            let key = if compact { key } else { format!(" {key} ") };
            let key = Span::styled(
                key,
                Style::default()
                    .fg(ACCENT)
                    .bg(PANEL)
                    .add_modifier(Modifier::BOLD),
            );
            let label = Span::styled(format!(" {label}"), Style::default().fg(TEXT));
            let size = key.width() + label.width();
            let gap = if compact { 2 } else { 3 };
            if used > 0 && used + gap + size > width {
                lines.push(Line::from(std::mem::take(&mut spans)));
                used = 0;
            }
            if used > 0 {
                spans.push(Span::styled(" ".repeat(gap), Style::default().fg(DIM)));
                used += gap;
            }
            spans.extend([key, label]);
            used += size;
        }
        if !spans.is_empty() {
            lines.push(Line::from(spans));
        }
        lines
    }
}
