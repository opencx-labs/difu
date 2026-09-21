use super::*;

pub(super) struct Section {
    pub id: String,
    pub row: usize,
    pub tool: bool,
}
fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(crate::model::clean)
        .unwrap_or_default()
}
fn prose(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut normal = String::new();
    let mut fence: Option<String> = None;
    let flush = |normal: &mut String, rows: &mut Vec<Line<'static>>| {
        rows.extend(
            crate::ui::prose(normal, width.max(1) as usize)
                .into_iter()
                .map(|row| Line::from(row.spans)),
        );
        normal.clear();
    };
    for line in text.lines() {
        if let Some(language) = line.trim_start().strip_prefix("```") {
            flush(&mut normal, &mut rows);
            if fence.take().is_some() {
                rows.push(Line::from(Span::styled("└────", Style::default().fg(DIM))));
            } else {
                fence = Some(language.into());
                rows.push(Line::from(Span::styled(
                    format!("┌ {}", crate::model::clean(language)),
                    Style::default().fg(DIM),
                )));
            }
        } else if let Some(language) = &fence {
            let color = if language == "diff" && line.starts_with('+') {
                GREEN
            } else if language == "diff" && line.starts_with('-') {
                RED
            } else {
                TEXT
            };
            rows.extend(
                wrapped(line, width)
                    .into_iter()
                    .map(|s| Line::from(Span::styled(s, Style::default().fg(color).bg(PANEL)))),
            );
        } else {
            normal.push_str(line);
            normal.push('\n');
        }
    }
    flush(&mut normal, &mut rows);
    rows
}

fn tool(entry: &Entry) -> bool {
    !matches!(
        entry.kind.as_str(),
        "agentMessage"
            | "userMessage"
            | "sending"
            | "sending_context"
            | "awaiting connection"
            | "result"
            | "plan"
            | "error"
            | "system"
            | "unsent"
            | "unsent or unacknowledged"
            | "reasoning"
    )
}
fn label(entry: &Entry) -> String {
    match entry.kind.as_str() {
        "commandExecution" => format!("Run {}", field(&entry.data, "command")),
        "fileChange" => {
            let paths = entry
                .data
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .map(|c| field(c, "path"))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!("Edit {paths}")
        }
        "mcpToolCall" => format!(
            "{} · {}",
            field(&entry.data, "server"),
            field(&entry.data, "tool")
        ),
        "dynamicToolCall" => format!("Tool {}", field(&entry.data, "tool")),
        "webSearch" => format!("Search {}", field(&entry.data, "query")),
        "imageView" => format!("View {}", field(&entry.data, "path")),
        "contextCompaction" => "Compact conversation".into(),
        "collabAgentToolCall" => "Agent activity".into(),
        "progress" => entry
            .text
            .lines()
            .next()
            .map(crate::model::clean)
            .unwrap_or_else(|| "Working".into()),
        _ => "Tool activity".into(),
    }
}
pub(super) fn activity(session: &Session) -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let elapsed = session
        .turn_started_at
        .map(|at| format!(" · {}s", now.saturating_sub(at).max(0) / 1000))
        .unwrap_or_default();
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        .get(((now.max(0) / 100) % 10) as usize)
        .copied()
        .unwrap_or("·");
    let state = if !session.pending.is_empty() {
        "Waiting for your answer"
    } else if session
        .entries
        .last()
        .is_some_and(|e| e.kind == "contextCompaction" && e.finished_at.is_none())
    {
        "Compacting context"
    } else if session
        .entries
        .last()
        .is_some_and(|e| e.kind == "agentMessage" && e.finished_at.is_none())
    {
        "Responding"
    } else if session.status == Status::Starting {
        "Preparing session"
    } else {
        "Working"
    };
    format!("{spinner} {state}{elapsed}")
}
pub(super) fn render(
    session: &Session,
    position: &Position,
    width: u16,
    pinned: Option<&str>,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Section>) {
    let mut lines = Vec::new();
    let mut sections = Vec::new();
    for entry in &session.entries {
        if pinned == Some(entry.id.as_str()) {
            continue;
        }
        if entry.text.is_empty() && entry.kind == "reasoning" {
            continue;
        }
        let is_tool = tool(entry);
        sections.push(Section {
            id: entry.id.clone(),
            row: lines.len(),
            tool: is_tool,
        });
        match entry.kind.as_str() {
            "userMessage" | "sending" | "unsent" | "unsent or unacknowledged" => {
                if entry.kind.starts_with("unsent") {
                    lines.push(Line::from(Span::styled(
                        "Not sent",
                        Style::default().fg(RED),
                    )));
                }
                for (index, text) in wrapped(&entry.text, width.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                {
                    let style = crate::ui::user_message_style();
                    lines.push(
                        Line::from(vec![
                            Span::styled(if index == 0 { "› " } else { "  " }, style),
                            Span::styled(text, style),
                        ])
                        .style(style),
                    );
                }
            }
            "agentMessage" | "result" => lines.extend(prose(&entry.text, width)),
            "reasoning" => {
                let heading = entry
                    .text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_matches('*');
                lines.push(Line::from(Span::styled(
                    format!("· {}", crate::model::clean(heading)),
                    Style::default().fg(DIM),
                )));
            }
            "plan" => {
                lines.push(Line::from(Span::styled(
                    "Plan",
                    Style::default().fg(ACCENT),
                )));
                if let Some(steps) = entry.data.get("plan").and_then(Value::as_array) {
                    for step in steps {
                        let marker = match field(step, "status").as_str() {
                            "completed" => "✓",
                            "inProgress" | "in_progress" => "›",
                            _ => "○",
                        };
                        lines.extend(prose(&format!("{marker} {}", field(step, "step")), width));
                    }
                } else {
                    lines.extend(prose(&entry.text, width));
                }
            }
            "error" | "system" => lines.extend(wrapped(&entry.text, width).into_iter().map(|s| {
                Line::from(Span::styled(
                    s,
                    Style::default().fg(if entry.kind == "error" { RED } else { DIM }),
                ))
            })),
            _ => {
                let expanded = position.expanded.contains(&entry.id);
                let failed = matches!(field(&entry.data, "status").as_str(), "failed" | "declined")
                    || entry
                        .data
                        .get("exitCode")
                        .and_then(Value::as_i64)
                        .is_some_and(|n| n != 0);
                let running = entry.started_at.is_some()
                    && entry.finished_at.is_none()
                    && session.status.active();
                let state = if failed {
                    "failed"
                } else if running {
                    "running"
                } else if field(&entry.data, "status") == "interrupted" {
                    "interrupted"
                } else if entry.finished_at.is_some() {
                    "done"
                } else if entry.started_at.is_some() {
                    "interrupted"
                } else {
                    ""
                };
                let elapsed = entry
                    .started_at
                    .map(|start| {
                        format!(
                            " · {:.1}s",
                            (entry
                                .finished_at
                                .unwrap_or_else(|| if running {
                                    chrono::Utc::now().timestamp_millis()
                                } else {
                                    session.updated
                                })
                                .saturating_sub(start)
                                .max(0) as f64)
                                / 1000.0
                        )
                    })
                    .unwrap_or_default();
                let focus = focused && position.focused_entry.as_ref() == Some(&entry.id);
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} {} {} · {state}{elapsed}",
                        if focus { "›" } else { " " },
                        if expanded { "▾" } else { "▸" },
                        label(entry)
                    ),
                    Style::default().fg(if failed {
                        RED
                    } else if running || focus {
                        ACCENT
                    } else {
                        DIM
                    }),
                )));
                if expanded {
                    let output = if entry.kind == "fileChange" {
                        entry
                            .data
                            .get("changes")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .map(|v| {
                                        format!(
                                            "{}\n```diff\n{}\n```",
                                            field(v, "path"),
                                            field(v, "diff")
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_else(|| entry.text.clone())
                    } else {
                        entry.text.clone()
                    };
                    if entry.kind == "commandExecution" {
                        lines.extend(wrapped(&output, width).into_iter().map(|s| {
                            Line::from(Span::styled(s, Style::default().fg(TEXT).bg(PANEL)))
                        }));
                    } else {
                        lines.extend(prose(&output, width));
                    }
                }
            }
        }
        if focused
            && !is_tool
            && position.focused_entry.as_ref() == Some(&entry.id)
            && let Some(section) = sections.last()
            && let Some(line) = lines.get_mut(section.row)
        {
            if matches!(
                entry.kind.as_str(),
                "userMessage" | "sending" | "unsent" | "unsent or unacknowledged"
            ) {
                if let Some(marker) = line.spans.first_mut() {
                    marker.content = "▸ ".into();
                }
            } else {
                line.spans
                    .insert(0, Span::styled("› ", Style::default().fg(ACCENT)));
                line.style = Style::default().bg(PANEL);
            }
        }
        if focused
            && position.focused_entry.as_ref() == Some(&entry.id)
            && let Some(section) = sections.last()
        {
            let background = ratatui::style::Color::Rgb(16, 39, 25);
            for line in lines.iter_mut().skip(section.row) {
                line.style = line.style.bg(background);
                for span in &mut line.spans {
                    span.style = span.style.bg(background);
                }
                let padding = usize::from(width).saturating_sub(line.width());
                line.spans.push(Span::styled(
                    " ".repeat(padding),
                    Style::default().bg(background),
                ));
            }
        }
        lines.push(Line::default());
    }
    (lines, sections)
}
