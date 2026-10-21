use super::*;
use crate::ui::{ACCENT, BG, BORDER, DIM, INK, PANEL, RED, TEXT};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub(super) fn label(value: &str) -> String {
    if let Some(label) = match value {
        "ctrl_enter" => Some("Ctrl+Enter"),
        "in_app" => Some("In app"),
        "claude" => Some("Claude Code"),
        "codex" => Some("Codex"),
        "mcp" => Some("MCP"),
        _ => None,
    } {
        return label.into();
    }
    value
        .split('_')
        .enumerate()
        .map(|(i, word)| {
            if i == 0 {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn display(value: SettingValue, id: &SettingId) -> String {
    match value {
        SettingValue::Bool(value) => if value { "On" } else { "Off" }.into(),
        SettingValue::Number(value) => value.to_string(),
        SettingValue::Choice(value) => label(&value),
        SettingValue::Text(value) => value.unwrap_or_else(|| "Automatic".into()),
        SettingValue::Shortcut(value) => {
            value
                .map(|value| chord_label(&value))
                .unwrap_or_else(|| match id {
                    SettingId::Shortcut(action) if action == "question.open" => {
                        "Alt+↑ (default)".into()
                    }
                    SettingId::Shortcut(action) if action == "settings.open" => {
                        "Ctrl+, (default)".into()
                    }
                    _ => "Default".into(),
                })
        }
        SettingValue::Action => "Open →".into(),
    }
}
fn chord_label(chord: &KeyChord) -> String {
    let mut parts = Vec::new();
    for (on, name) in [
        (chord.modifiers.control, "Ctrl"),
        (chord.modifiers.alt, "Alt"),
        (chord.modifiers.shift, "Shift"),
        (chord.modifiers.super_key, "Super"),
    ] {
        if on {
            parts.push(name.to_owned());
        }
    }
    parts.push(match chord.key {
        BindingKey::Character(ch) => ch.to_string(),
        BindingKey::Function(n) => format!("F{n}"),
        ref key => format!("{key:?}"),
    });
    parts.join("+")
}
fn block(title: String) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(BG).fg(TEXT))
}
impl Panel {
    pub fn draw(&mut self, frame: &mut Frame) {
        let full = frame.area();
        if full.width == 0 || full.height == 0 {
            return;
        }
        frame.render_widget(Clear, full);
        frame.render_widget(
            block(format!(
                " Settings · {} unsaved ",
                self.draft.edited().len()
            )),
            full,
        );
        let inside = full.inner(ratatui::layout::Margin::new(1, 1));
        let [search, body, status, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(if full.width < 65 { 4 } else { 3 }),
        ])
        .areas(inside);
        frame.render_widget(
            Paragraph::new(format!(
                "{} Search: {}{} · {}",
                if self.search_focused { "›" } else { " " },
                self.query,
                if self.search_focused { "▏" } else { "" },
                if self.modified_only {
                    "Modified only"
                } else {
                    "All preferences"
                }
            ))
            .style(Style::default().fg(ACCENT))
            .wrap(Wrap { trim: false }),
            search,
        );
        let wide = full.width >= 110;
        let medium = full.width >= 70;
        let (list, details) = if self.details_expanded {
            (Rect::new(body.x, body.y, 0, 0), body)
        } else if wide {
            let [categories, list, details] = Layout::horizontal([
                Constraint::Length(23),
                Constraint::Percentage(44),
                Constraint::Min(24),
            ])
            .areas(body);
            let rows = Category::ALL
                .iter()
                .map(|category| {
                    Line::styled(
                        format!(
                            "{} {}",
                            if self.category == *category {
                                "›"
                            } else {
                                " "
                            },
                            category.label()
                        ),
                        Style::default().fg(if self.category == *category {
                            ACCENT
                        } else {
                            DIM
                        }),
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(
                Paragraph::new(rows).block(block(" Sections ".into())),
                categories,
            );
            (list, details)
        } else if medium {
            let [list, details] =
                Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
                    .areas(body);
            (list, details)
        } else {
            let [list, details] =
                Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
                    .areas(body);
            (list, details)
        };
        let rows = self.rows();
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        let narrow_list = full.width < 70;
        let row_height = if narrow_list { 2 } else { 1 };
        let visible = usize::from(list.height.saturating_sub(2) / row_height);
        let start = self.selected.saturating_sub(visible.saturating_sub(1));
        let lines = if rows.is_empty() {
            vec![Line::styled(
                "No matching preferences",
                Style::default().fg(DIM),
            )]
        } else {
            rows.iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .flat_map(|(index, spec)| {
                    let selected = index == self.selected;
                    let value = if selected {
                        self.edit
                            .as_ref()
                            .map(|text| format!("{text}▏"))
                            .unwrap_or_else(|| display(self.draft.value(&spec.id), &spec.id))
                    } else {
                        display(self.draft.value(&spec.id), &spec.id)
                    };
                    let style = if selected {
                        Style::default().fg(INK).bg(ACCENT)
                    } else {
                        Style::default().fg(TEXT)
                    };
                    let marker = format!(
                        "{}{}",
                        if selected { "›" } else { " " },
                        if self.draft.edited().contains(&spec.id) {
                            "●"
                        } else {
                            " "
                        }
                    );
                    if narrow_list {
                        let value_label = if selected && self.edit.is_some() {
                            "Editing"
                        } else {
                            "Current"
                        };
                        vec![
                            Line::styled(format!("{marker} {}", spec.label), style),
                            Line::styled(format!("   {value_label}: {value}"), style),
                        ]
                    } else {
                        vec![Line::styled(
                            format!("{marker} {}: {value}", spec.label),
                            style,
                        )]
                    }
                })
                .collect()
        };
        frame.render_widget(
            Paragraph::new(lines).block(block(format!(" {} · PgUp/PgDn ", self.category.label()))),
            list,
        );
        if let Some(spec) = rows.get(self.selected) {
            self.draw_details(frame, details, spec);
        }
        let message = if self.confirm_discard {
            "Unsaved preferences. Enter/D: discard and close · any other key: keep editing"
                .to_owned()
        } else {
            self.error.clone().or_else(|| self.notice.clone()).unwrap_or_else(|| "Visual previews are local. Save applies preferences; connection actions apply separately.".into())
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(
                    Style::default().fg(if self.error.is_some() || self.confirm_discard {
                        RED
                    } else {
                        DIM
                    }),
                )
                .wrap(Wrap { trim: false }),
            status,
        );
        let help = if full.width < 65 {
            "^S Save ^D Discard ^R Reset\n^E Export ^M Modified ^P Replay\n^T Test ^I Details ^↑/↓ Scroll\nTab Focus PgUp/Dn Sections Esc Close"
        } else {
            "Ctrl+S Save · Ctrl+D Discard · Ctrl+R Reset · Ctrl+E Export\nCtrl+M Modified · Ctrl+P Replay · Ctrl+T Test · Ctrl+I Details\nCtrl+↑/↓ Scroll · Tab Focus · PgUp/Dn Sections · Esc Close"
        };
        frame.render_widget(
            Paragraph::new(help)
                .style(Style::default().fg(DIM))
                .wrap(Wrap { trim: false }),
            footer,
        );
    }
    fn draw_details(&mut self, frame: &mut Frame, area: Rect, spec: &SettingSpec) {
        frame.render_widget(block(" Details & preview ".into()), area);
        let inner = area.inner(ratatui::layout::Margin::new(1, 1));
        let value = self
            .edit
            .as_ref()
            .map(|text| format!("Editing: {text}▏"))
            .unwrap_or_else(|| {
                format!("Current: {}", display(self.draft.value(&spec.id), &spec.id))
            });
        let mut details = vec![
            Line::styled(value, Style::default().fg(ACCENT)),
            Line::styled(spec.label, Style::default().fg(ACCENT)),
            Line::from(spec.effect.label()),
            Line::from(spec.description),
            Line::default(),
            Line::styled("Sample · Ctrl+P Replay", Style::default().fg(DIM)),
        ];
        let visual = self.preview_config().clone();
        let lines = match spec.preview {
            Preview::Diff => diff_preview(&visual, inner.width),
            Preview::Animation => {
                self.preview
                    .render_control(&visual, inner.width, self.replay, &spec.id)
            }
            Preview::Conversation | Preview::Layout | Preview::Input => self
                .preview
                .render_control(&visual, inner.width, self.replay, &spec.id),
            Preview::Appearance => self
                .preview
                .render(&visual, inner.width, self.replay, false),

            Preview::Shortcut => vec![
                Line::from("Safe binding editor and tester"),
                Line::from("Enter opens keyboard integration."),
                Line::from("No session action runs in this preview."),
            ],
            Preview::Notification => vec![
                Line::from(Span::styled(
                    "✓ Example session finished",
                    Style::default().fg(ACCENT),
                )),
                Line::from("Synthetic status; no notification is sent."),
            ],
            Preview::None => vec![
                Line::from("Enter edits or opens the selected control."),
                Line::from("Connection and rules actions apply separately."),
            ],
        };
        details.extend(lines);
        frame.render_widget(
            Paragraph::new(details)
                .style(Style::default().bg(PANEL))
                .wrap(Wrap { trim: false })
                .scroll((self.details_scroll, 0)),
            inner,
        );
    }
}
/// Real review diff formatting on isolated synthetic data.
pub(crate) fn diff_preview(config: &Config, width: u16) -> Vec<Line<'static>> {
    use crate::diff::{DiffLine, LineKind};
    let lines = vec![
        DiffLine {
            kind: LineKind::Context,
            old: Some(1),
            new: Some(1),
            text: "fn greeting() {".into(),
        },
        DiffLine {
            kind: LineKind::Remove,
            old: Some(2),
            new: None,
            text: "    println!(\"Hi\");".into(),
        },
        DiffLine {
            kind: LineKind::Add,
            old: None,
            new: Some(2),
            text: "    println!(\"Hello, Difu!\");".into(),
        },
        DiffLine {
            kind: LineKind::Context,
            old: Some(3),
            new: Some(3),
            text: "}".into(),
        },
    ];
    crate::ui::code_rows(
        "src/greeting.rs",
        &lines,
        usize::from(width.max(1)),
        !config.unified,
        (0, config.wrap_diff),
    )
    .into_iter()
    .map(|row| Line::from(row.spans))
    .collect()
}
