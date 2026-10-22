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
fn display(value: SettingValue, id: &SettingId, config: &Config) -> String {
    match value {
        SettingValue::Bool(value) => if value { "On" } else { "Off" }.into(),
        SettingValue::Number(value) => value.to_string(),
        SettingValue::Choice(value) => label(&value),
        SettingValue::Text(value) => value.unwrap_or_else(|| "Automatic".into()),
        SettingValue::Shortcut(_) => match id {
            SettingId::Shortcut(name) => crate::shortcuts::find(name)
                .map(|action| crate::shortcuts::Bindings::new(config).hint(action))
                .unwrap_or_else(|| "Unknown stored action".into()),
            _ => "Default".into(),
        },
        SettingValue::Action => "Open →".into(),
    }
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
            Constraint::Length(4),
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
                            .unwrap_or_else(|| {
                                display(self.draft.value(&spec.id), &spec.id, self.draft.config())
                            })
                    } else {
                        display(self.draft.value(&spec.id), &spec.id, self.draft.config())
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
            Paragraph::new(lines).block(block(format!(
                " {} · {}/{} ",
                self.category.label(),
                self.binding_hint(crate::shortcuts::ids::SETTINGS_PREVIOUS_SECTION),
                self.binding_hint(crate::shortcuts::ids::SETTINGS_NEXT_SECTION)
            ))),
            list,
        );
        if let Some(spec) = rows.get(self.selected) {
            self.draw_details(frame, details, spec);
        }
        let message = if self.confirm_discard {
            format!(
                "Unsaved preferences. {}: discard and close · any other key: keep editing",
                self.binding_hint(crate::shortcuts::ids::SETTINGS_DISCARD_CLOSE)
            )
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
        use crate::shortcuts::ids;
        let hint = |id| {
            let value = self
                .binding_hint(id)
                .split(" / ")
                .next()
                .unwrap_or_default()
                .to_owned();
            if full.width < 65 {
                value
                    .replace("Ctrl+", "^")
                    .replace("PageUp", "PgUp")
                    .replace("PageDown", "PgDn")
                    .replace("^Up", "^↑")
                    .replace("^Down", "^↓")
            } else {
                value
            }
        };
        let help=format!("{} Save {} Discard {} Reset\n{} Export {} Modified {} Replay\n{} Test {} Details {}/{} Scroll\n{} Focus {}/{} Sections Esc Close",hint(ids::SETTINGS_SAVE),hint(ids::SETTINGS_DISCARD),hint(ids::SETTINGS_RESET_SECTION),hint(ids::SETTINGS_EXPORT),hint(ids::SETTINGS_MODIFIED),hint(ids::SETTINGS_REPLAY),hint(ids::SETTINGS_TEST),hint(ids::SETTINGS_DETAILS),hint(ids::SETTINGS_DETAILS_UP),hint(ids::SETTINGS_DETAILS_DOWN),hint(ids::SETTINGS_FOCUS),hint(ids::SETTINGS_PREVIOUS_SECTION),hint(ids::SETTINGS_NEXT_SECTION)).replace("PgUp/PgDn","PgUp/Dn");
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
                format!(
                    "Current: {}",
                    display(self.draft.value(&spec.id), &spec.id, self.draft.config())
                )
            });
        let mut details = vec![
            Line::styled(value, Style::default().fg(ACCENT)),
            Line::styled(spec.label, Style::default().fg(ACCENT)),
            Line::from(spec.effect.label()),
            Line::from(spec.description),
            Line::default(),
            Line::styled(
                format!(
                    "Sample · {} Replay",
                    self.binding_hint(crate::shortcuts::ids::SETTINGS_REPLAY)
                ),
                Style::default().fg(DIM),
            ),
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

            Preview::Shortcut => {
                let mut lines = vec![
                    Line::from(format!(
                        "{} captures this key; {} tests draft bindings safely.",
                        self.binding_hint(crate::shortcuts::ids::SETTINGS_ACTIVATE),
                        self.binding_hint(crate::shortcuts::ids::SETTINGS_TEST)
                    )),
                    Line::from(format!(
                        "{} resets the selected binding. Esc cancels capture/test.",
                        self.binding_hint(crate::shortcuts::ids::SETTINGS_RESET_BINDING)
                    )),
                ];
                if let SettingId::Shortcut(name) = &spec.id
                    && let Some(action) =
                        crate::shortcuts::find(name).and_then(crate::shortcuts::spec)
                {
                    lines.push(Line::from(format!(
                        "Contexts: {}",
                        action
                            .contexts
                            .iter()
                            .map(|c| c.0)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                lines.push(Line::from("Shift+Left overrides composer text selection only with explicit consent. No session action runs in this preview."));
                lines
            }
            Preview::Notification => vec![
                Line::from(Span::styled(
                    "✓ Example session finished",
                    Style::default().fg(ACCENT),
                )),
                Line::from("Synthetic status; no notification is sent."),
            ],
            Preview::None => vec![
                Line::from(format!(
                    "{} edits or opens the selected control.",
                    self.binding_hint(crate::shortcuts::ids::SETTINGS_ACTIVATE)
                )),
                Line::from("Connection and rules actions apply separately."),
            ],
        };
        details.extend(lines);
        if spec.id == SettingId::SendKey {
            let bindings = crate::shortcuts::Bindings::new(self.draft.config());
            details.push(Line::from(format!(
                "Send: {} · Newline: {} · Explicit queue: {}",
                bindings.hint(crate::shortcuts::ids::SEND),
                bindings.hint(crate::shortcuts::ids::NEWLINE),
                bindings.hint(crate::shortcuts::ids::QUEUE)
            )));
        }
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
    use crate::diff::{DiffFile, DiffLine, Hunk, LineKind};
    let mut lines = (1..=11)
        .map(|n| DiffLine {
            kind: LineKind::Context,
            old: Some(n),
            new: Some(n),
            text: if n == 11 {
                "fn greeting() {".into()
            } else {
                format!("// Greeting context {n}")
            },
        })
        .collect::<Vec<_>>();
    lines.push(DiffLine {
        kind: LineKind::Remove,
        old: Some(12),
        new: None,
        text: "    println!(\"Hi\");".into(),
    });
    lines.push(DiffLine {
        kind: LineKind::Add,
        old: None,
        new: Some(12),
        text: "    println!(\"Hello, Difu!\");".into(),
    });
    lines.extend((13..=23).map(|n| DiffLine {
        kind: LineKind::Context,
        old: Some(n),
        new: Some(n),
        text: if n == 13 {
            "}".into()
        } else {
            format!("// Greeting context {n}")
        },
    }));
    let file = DiffFile {
        path: "src/greeting.ts".into(),
        old_path: "src/greeting.ts".into(),
        status: "M".into(),
        additions: 1,
        deletions: 1,
        hunks: vec![Hunk {
            id: "settings-example".into(),
            header: "@@ -9,7 +9,7 @@".into(),
            lines: lines.iter().skip(8).take(8).cloned().collect(),
        }],
    };
    let Ok(context) = crate::context::FileContext::new(&file, lines) else {
        return Vec::new();
    };
    let Some(hunk) = file.hunks.first() else {
        return Vec::new();
    };
    crate::context::project(
        hunk,
        Some(&context),
        Default::default(),
        config.preferences.code.context_lines,
    )
    .iter()
    .flat_map(|part| {
        crate::ui::code_rows_with_preferences(
            &file.path,
            part.lines,
            usize::from(width.max(1)),
            !config.unified,
            (0, config.wrap_diff),
            &config.preferences.code,
        )
    })
    .map(|row| {
        let selected = matches!(
            row.target,
            Some(crate::workflow::Target::Code { new: Some(12), .. })
        );
        Line::from(if selected {
            row.selected_spans(
                crate::review::Side::Right,
                !config.unified,
                usize::from(width.max(1)),
            )
        } else {
            row.spans
        })
    })
    .collect()
}
