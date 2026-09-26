use super::*;
use ratatui::style::Color;

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
#[derive(Clone)]
pub struct Link {
    pub row: usize,
    pub column: usize,
    pub width: usize,
    pub url: String,
}
pub(super) fn link_hits(links: &[Link], area: Rect, scroll: usize) -> Vec<(Rect, Action)> {
    links
        .iter()
        .filter_map(|link| {
            let row = link.row.checked_sub(scroll)?;
            let width = link
                .width
                .min(usize::from(area.width).saturating_sub(link.column));
            if row >= usize::from(area.height) || width == 0 {
                return None;
            }
            Some((
                Rect::new(
                    area.x + link.column as u16,
                    area.y + row as u16,
                    width as u16,
                    1,
                ),
                Action::Link(link.url.clone()),
            ))
        })
        .collect()
}
#[cfg(test)]
fn prose(text: &str, width: u16) -> Vec<Line<'static>> {
    prose_links(text, width).0
}
fn append_prose(lines: &mut Vec<Line<'static>>, links: &mut Vec<Link>, text: &str, width: u16) {
    let (rows, targets) = prose_links(text, width);
    links.extend(targets.into_iter().map(|mut link| {
        link.row += lines.len();
        link
    }));
    lines.extend(rows);
}
fn prose_links(text: &str, width: u16) -> (Vec<Line<'static>>, Vec<Link>) {
    let mut links = Vec::new();
    let mut rows = Vec::new();
    let mut normal = String::new();
    let mut fence: Option<String> = None;
    let mut flush = |normal: &mut String, rows: &mut Vec<Line<'static>>| {
        for row in crate::ui::prose(normal, width.max(1) as usize) {
            if let Some(crate::app::Action::Link(url)) = row.action {
                links.push(Link {
                    row: rows.len(),
                    column: 0,
                    width: row.spans.iter().map(Span::width).sum(),
                    url,
                });
            }
            for link in row.code_links {
                if let crate::app::Action::Link(url) = link.action {
                    links.push(Link {
                        row: rows.len(),
                        column: link.column,
                        width: link.width,
                        url,
                    });
                }
            }
            rows.push(Line::from(row.spans));
        }
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
            let content = crate::model::clean(line).replace('\t', "    ");
            let spans = if language == "diff" && (line.starts_with('+') || line.starts_with('-')) {
                vec![Span::styled(
                    content,
                    Style::default().fg(if line.starts_with('+') { GREEN } else { RED }),
                )]
            } else {
                crate::ui::syntax_spans(&content)
            };
            let highlighted = crate::markdown::wrap(spans, usize::from(width).max(1));
            if highlighted.is_empty() {
                rows.push(Line::default());
            } else {
                rows.extend(highlighted.into_iter().map(|row| Line::from(row.spans)));
            }
        } else {
            normal.push_str(line);
            normal.push('\n');
        }
    }
    flush(&mut normal, &mut rows);
    (rows, links)
}

pub(super) fn tool(entry: &Entry) -> bool {
    entry.is_tool()
}
pub(super) fn visible(session: &Session, entry: &Entry) -> bool {
    !(entry.text.is_empty() && entry.kind == "reasoning")
        && !(session.is_pending_steering(entry) && session.tool_running())
}

fn label(entry: &Entry, running: bool) -> String {
    match entry.kind.as_str() {
        "commandExecution" => format!(
            "{} {}",
            if running { "Running" } else { "Ran" },
            field(&entry.data, "command")
        ),
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
            format!("{} {paths}", if running { "Editing" } else { "Edited" })
        }
        "mcpToolCall" => format!(
            "{} · {}",
            field(&entry.data, "server"),
            field(&entry.data, "tool")
        ),
        "dynamicToolCall" => format!("Tool {}", field(&entry.data, "tool")),
        "webSearch" => format!(
            "{} {}",
            if running { "Searching" } else { "Searched" },
            field(&entry.data, "query")
        ),
        "imageView" => format!("Read {}", field(&entry.data, "path")),
        "autoApprovalReview" => "Reviewing approval request".into(),
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
fn activity_text(session: &Session) -> (String, Option<String>, Option<i64>) {
    if session
        .pending
        .iter()
        .any(|p| p.id == "difu-missing-guidance")
    {
        return (
            "Waiting for repository guidance".into(),
            None,
            session.turn_started_at,
        );
    }
    if let Some(review) = session
        .entries
        .iter()
        .rev()
        .find(|e| e.kind == "autoApprovalReview" && e.finished_at.is_none())
    {
        let target = review.data.get("targetItemId").and_then(Value::as_str);
        let detail = session
            .entries
            .iter()
            .find(|e| Some(e.id.as_str()) == target)
            .map(|e| label(e, true));
        return (
            "Reviewing approval request".into(),
            detail,
            review.started_at,
        );
    }
    if session.pending.iter().any(|p| !p.is_async_question()) {
        return (
            "Waiting for your approval or answer".into(),
            None,
            session.turn_started_at,
        );
    }
    if let Some(entry) = session.entries.iter().rev().find(|e| {
        e.started_at.is_some()
            && e.finished_at.is_none()
            && !matches!(
                e.kind.as_str(),
                "userMessage" | "sending" | "sending_context" | "system"
            )
    }) {
        let state = match entry.kind.as_str() {
            "agentMessage" => "Responding".into(),
            "reasoning" => "Thinking".into(),
            "contextCompaction" => "Compacting context".into(),
            _ => label(entry, true),
        };
        return (state, None, entry.started_at);
    }
    let state = if session.status == Status::Starting {
        "Preparing session"
    } else if session.status == Status::Waiting {
        "Waiting for your answer"
    } else {
        "Working"
    };
    (state.into(), None, session.turn_started_at)
}
fn shimmer(text: &str, now: i64) -> Line<'static> {
    let length = text.chars().count();
    let phase = (now.max(0) as usize / 80) % length.max(1);
    Line::from(
        text.chars()
            .enumerate()
            .map(|(index, ch)| {
                let distance = index.abs_diff(phase);
                let color = match distance {
                    0..=1 => Color::Rgb(235, 240, 235),
                    2..=3 => Color::Rgb(190, 200, 190),
                    4..=5 => Color::Rgb(150, 165, 150),
                    _ => DIM,
                };
                Span::styled(ch.to_string(), Style::default().fg(color))
            })
            .collect::<Vec<_>>(),
    )
}
pub(super) fn activity(session: &Session, width: u16, now: i64) -> Vec<Line<'static>> {
    if !session.status.active() {
        return Vec::new();
    }
    let (state, detail, started) = activity_text(session);
    let seconds = started.map_or(0, |at| now.saturating_sub(at).max(0) / 1000);
    let elapsed = if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    };
    let mut lines = wrapped(&format!("• {state} ({elapsed})"), width)
        .into_iter()
        .map(|text| shimmer(&text, now))
        .collect::<Vec<_>>();
    if let Some(detail) = detail {
        lines.extend(
            wrapped(
                &format!("  └ {}", detail.lines().next().unwrap_or_default()),
                width,
            )
            .into_iter()
            .map(|text| Line::from(Span::styled(text, Style::default().fg(DIM)))),
        );
    }
    lines
}
pub(super) fn waiting_messages(
    session: &Session,
    width: u16,
    outgoing: &[PendingSend],
) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut group = |heading: &str, messages: Vec<&str>| {
        if messages.is_empty() {
            return;
        }
        rows.push(Line::default());
        let mut spans = vec![Span::styled(
            format!("• {heading}"),
            Style::default().fg(TEXT),
        )];
        if session.can_send_waiting() {
            spans.push(Span::styled(
                " (press esc to interrupt and send immediately)",
                Style::default().fg(DIM),
            ));
        }
        rows.extend(
            crate::markdown::wrap(spans, usize::from(width.max(1)))
                .into_iter()
                .map(|row| Line::from(row.spans)),
        );
        for text in messages {
            use unicode_width::UnicodeWidthStr;
            let available = usize::from(width.saturating_sub(4));
            let mut chars = text.chars();
            let prefix = chars
                .by_ref()
                .take(available + 1)
                .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
                .collect::<String>();
            let prefix = crate::model::clean(&prefix);
            let shortened = chars.next().is_some() || prefix.width() > available;
            let preview = if shortened && available > 0 {
                format!("{}…", crate::ui::crop(&prefix, 0, available - 1))
            } else {
                crate::ui::crop(&prefix, 0, available)
            };
            rows.push(Line::from(Span::styled(
                crate::ui::crop(&format!("  ↳ {preview}"), 0, usize::from(width)),
                Style::default().fg(DIM),
            )));
        }
    };
    let mut steering = session
        .pending_steering()
        .filter(|_| session.tool_running())
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    let mut queue = session.queue.iter().map(Prompt::text).collect::<Vec<_>>();
    for outgoing in outgoing {
        if outgoing.queued {
            queue.push(&outgoing.text);
        } else if !outgoing.in_chat(session) {
            steering.push(&outgoing.text);
        }
    }
    group(
        "Messages to be submitted after the next tool call",
        steering,
    );
    group(
        if session
            .pending
            .iter()
            .any(|p| p.id == "difu-missing-guidance")
        {
            "Messages queued until repository guidance is answered"
        } else {
            "Messages queued for the next turn"
        },
        queue,
    );
    rows
}

// Bound input before markdown/syntax parsing, then cap terminal rows after wrapping.
fn prefix(text: &str, budget: usize) -> String {
    text.chars().take(budget).collect()
}
fn preview(
    session: &Session,
    entry: &Entry,
    width: u16,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Link>) {
    let budget = usize::from(width.max(1)).saturating_mul(16);
    let mut data = serde_json::Map::new();
    for key in [
        "command", "server", "tool", "query", "path", "status", "exitCode", "review",
    ] {
        if let Some(value) = entry.data.get(key) {
            if let Some(text) = value.as_str() {
                data.insert(key.into(), Value::String(prefix(text, budget)));
            } else if key == "exitCode" && value.is_i64() {
                data.insert(key.into(), value.clone());
            }
        }
    }
    if let Some(status) = entry.data.pointer("/review/status").and_then(Value::as_str) {
        data.insert(
            "review".into(),
            serde_json::json!({"status":prefix(status,64)}),
        );
    }
    if let Some(changes) = entry.data.get("changes").and_then(Value::as_array) {
        let changes = changes.iter().take(1).map(|change| {
            let diff = change.get("diff").and_then(Value::as_str).unwrap_or_default();
            let (added,removed) = super::patch::counts(diff);
            serde_json::json!({
                "path":prefix(change.get("path").and_then(Value::as_str).unwrap_or_default(),budget),
                "diff":prefix(diff,budget),
                "previewAdded":added,
                "previewRemoved":removed,
            })
        }).collect();
        data.insert("changes".into(), Value::Array(changes));
    }
    let bounded = Entry {
        id: entry.id.clone(),
        kind: entry.kind.clone(),
        text: prefix(&entry.text, budget),
        data: Value::Object(data),
        started_at: entry.started_at,
        finished_at: entry.finished_at,
    };
    let mut position = Position::default();
    position.expanded.insert(entry.id.clone());
    if focused {
        position.focused_entry = Some(entry.id.clone());
    }
    let (mut rows, _, mut links) = render_entries(
        session,
        std::slice::from_ref(&bounded),
        &position,
        width,
        focused,
    );
    // A multiline command or very long JSON response shares the same preview budget.
    rows.truncate(4);
    links.retain(|link| link.row < rows.len());
    rows.push(Line::from(Span::styled(
        "    Enter/click to expand",
        Style::default().fg(DIM),
    )));
    rows.push(Line::default());
    (rows, links)
}

#[cfg(test)]
pub(super) fn render(
    session: &Session,
    position: &Position,
    width: u16,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Section>) {
    let (lines, sections, _) = render_entries(session, &session.entries, position, width, focused);
    (lines, sections)
}
pub(super) fn render_entries(
    session: &Session,
    entries: &[Entry],
    position: &Position,
    width: u16,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Section>, Vec<Link>) {
    let mut links = Vec::new();
    let mut lines = Vec::new();
    let mut sections = Vec::new();
    for entry in entries {
        if !visible(session, entry) {
            continue;
        }
        let is_tool = tool(entry);
        sections.push(Section {
            id: entry.id.clone(),
            row: lines.len(),
            tool: is_tool,
        });
        if is_tool && !position.expanded.contains(&entry.id) {
            let (rows, targets) = preview(
                session,
                entry,
                width,
                focused && position.focused_entry.as_ref() == Some(&entry.id),
            );
            links.extend(targets.into_iter().map(|mut link| {
                link.row += lines.len();
                link
            }));
            lines.extend(rows);
            continue;
        }
        match entry.kind.as_str() {
            "userMessage"
            | "awaiting connection"
            | "sending"
            | "unsent"
            | "unsent or unacknowledged" => {
                if entry.kind.starts_with("unsent") {
                    lines.push(Line::from(Span::styled(
                        "Not sent",
                        Style::default().fg(RED),
                    )));
                }
                let padding = || {
                    Line::from(" ".repeat(usize::from(width)))
                        .style(crate::ui::user_message_style())
                };
                lines.push(padding());
                for (index, text) in wrapped(&entry.text, width.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                {
                    let style = crate::ui::user_message_style();
                    let mut line = Line::from(vec![
                        Span::styled(if index == 0 { "› " } else { "  " }, style),
                        Span::styled(text, style),
                    ])
                    .style(style);
                    let padding = usize::from(width).saturating_sub(line.width());
                    line.spans.push(Span::styled(" ".repeat(padding), style));
                    lines.push(line);
                }
                lines.push(padding());
            }
            "agentMessage" | "result" => append_prose(&mut lines, &mut links, &entry.text, width),
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
                        append_prose(
                            &mut lines,
                            &mut links,
                            &format!("{marker} {}", field(step, "step")),
                            width,
                        );
                    }
                } else {
                    append_prose(&mut lines, &mut links, &entry.text, width);
                }
            }
            "error" | "system" => lines.extend(wrapped(&entry.text, width).into_iter().map(|s| {
                Line::from(Span::styled(
                    s,
                    Style::default().fg(if entry.kind == "error" { RED } else { DIM }),
                ))
            })),
            _ => {
                let review_status = entry.data.pointer("/review/status").and_then(Value::as_str);
                let failed = matches!(review_status, Some("denied" | "timedOut"))
                    || matches!(field(&entry.data, "status").as_str(), "failed" | "declined")
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
                } else if field(&entry.data, "status") == "interrupted"
                    || review_status == Some("aborted")
                {
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
                let color = if failed {
                    RED
                } else if running || focus {
                    ACCENT
                } else {
                    TEXT
                };
                let heading = format!(
                    "{} {} · {state}{elapsed}",
                    if focus {
                        "›"
                    } else if failed {
                        "×"
                    } else {
                        "•"
                    },
                    label(entry, running)
                );
                if entry.kind == "commandExecution" {
                    for (index, source) in field(&entry.data, "command").lines().enumerate() {
                        let mut spans = if index == 0 {
                            vec![Span::styled(
                                format!(
                                    "{} {} ",
                                    if focus {
                                        "›"
                                    } else if failed {
                                        "×"
                                    } else {
                                        "•"
                                    },
                                    if running { "Running" } else { "Ran" }
                                ),
                                Style::default().fg(color),
                            )]
                        } else {
                            vec![Span::raw("  │ ")]
                        };
                        spans.extend(crate::ui::syntax_spans(&source.replace('\t', "    ")));
                        if index == 0 {
                            spans.push(Span::styled(
                                format!(" · {state}{elapsed}"),
                                Style::default().fg(DIM),
                            ));
                        }
                        lines.extend(
                            crate::markdown::wrap(spans, usize::from(width).max(1))
                                .into_iter()
                                .map(|row| Line::from(row.spans)),
                        );
                    }
                } else if entry.kind == "fileChange" {
                    if let Some(changes) = entry.data.get("changes").and_then(Value::as_array) {
                        for change in changes {
                            let (patch, added, removed) =
                                super::patch::render(&field(change, "diff"), width);
                            let added = change
                                .get("previewAdded")
                                .and_then(Value::as_u64)
                                .map_or(added, |n| n as usize);
                            let removed = change
                                .get("previewRemoved")
                                .and_then(Value::as_u64)
                                .map_or(removed, |n| n as usize);
                            let title = vec![
                                Span::styled(
                                    format!(
                                        "{} {} {} ",
                                        if focus {
                                            "›"
                                        } else if failed {
                                            "×"
                                        } else {
                                            "•"
                                        },
                                        if running { "Editing" } else { "Edited" },
                                        field(change, "path")
                                    ),
                                    Style::default().fg(color),
                                ),
                                Span::styled(format!("+{added}"), Style::default().fg(GREEN)),
                                Span::raw(" "),
                                Span::styled(format!("-{removed}"), Style::default().fg(RED)),
                                Span::styled(
                                    format!(" · {state}{elapsed}"),
                                    Style::default().fg(DIM),
                                ),
                            ];
                            lines.extend(
                                crate::markdown::wrap(title, usize::from(width).max(1))
                                    .into_iter()
                                    .map(|row| Line::from(row.spans)),
                            );
                            lines.extend(patch);
                        }
                        lines.push(Line::default());
                        continue;
                    }
                } else {
                    lines.extend(
                        wrapped(&heading, width)
                            .into_iter()
                            .map(|line| Line::from(Span::styled(line, Style::default().fg(color)))),
                    );
                }
                let output = if entry.kind == "commandExecution" {
                    entry
                        .text
                        .strip_prefix(&format!("$ {}\n", field(&entry.data, "command")))
                        .unwrap_or(&entry.text)
                        .to_owned()
                } else {
                    entry.text.clone()
                };
                let shown = output;
                let rows = if entry.kind == "commandExecution" {
                    shown
                        .lines()
                        .flat_map(|line| {
                            let rows = crate::markdown::wrap(
                                crate::ui::syntax_spans(
                                    &crate::model::clean(line).replace('\t', "    "),
                                ),
                                usize::from(width.saturating_sub(4)).max(1),
                            );
                            if rows.is_empty() {
                                vec![Line::default()]
                            } else {
                                rows.into_iter().map(|row| Line::from(row.spans)).collect()
                            }
                        })
                        .collect::<Vec<_>>()
                } else {
                    let (rows, targets) = prose_links(&shown, width.saturating_sub(4));
                    links.extend(targets.into_iter().map(|mut link| {
                        link.row += lines.len();
                        link.column += 4;
                        link
                    }));
                    rows
                };
                if !shown.is_empty() {
                    for (index, mut row) in rows.into_iter().enumerate() {
                        row.spans.insert(
                            0,
                            Span::styled(
                                if index == 0 { "  └ " } else { "    " },
                                Style::default().fg(DIM),
                            ),
                        );
                        lines.push(row);
                    }
                }
            }
        }
        if focused
            && !is_tool
            && position.focused_entry.as_ref() == Some(&entry.id)
            && let Some(section) = sections.last()
            && let Some(line) = lines.get_mut(section.row.saturating_add(usize::from(matches!(
                entry.kind.as_str(),
                "userMessage" | "awaiting connection" | "sending"
            ))))
        {
            if matches!(
                entry.kind.as_str(),
                "userMessage"
                    | "awaiting connection"
                    | "sending"
                    | "unsent"
                    | "unsent or unacknowledged"
            ) {
                if let Some(marker) = line.spans.first_mut() {
                    marker.content = "▸ ".into();
                }
            } else {
                for link in links.iter_mut().filter(|link| link.row == section.row) {
                    link.column += 2;
                }
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
    (lines, sections, links)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_keep_targets_and_viewport_coordinates() {
        let (rows, links) = prose_links("Open [the invoice](https://example.com/invoice)", 12);
        assert!(links.iter().any(|link| {
            link.url == "https://example.com/invoice"
                && crate::ui::crop(
                    &rows.get(link.row).map(Line::to_string).unwrap_or_default(),
                    link.column,
                    link.width,
                )
                .contains("invoice")
        }));
        let url_row = links
            .iter()
            .find(|link| {
                rows.get(link.row)
                    .is_some_and(|row| row.to_string().starts_with("↗ "))
            })
            .map(|link| link.row)
            .unwrap_or_default();
        let hits = link_hits(&links, Rect::new(40, 8, 12, 2), url_row);
        assert!(
            hits.iter()
                .any(|(rect, action)| *rect == Rect::new(40, 8, 12, 1)
                    && matches!(action, Action::Link(url) if url == "https://example.com/invoice"))
        );
        assert!(link_hits(&links, Rect::new(40, 8, 12, 2), rows.len()).is_empty());
    }
    #[test]
    fn shimmer_changes_only_color_without_moving_text_or_painting_background() {
        let first = shimmer("Reviewing approval request", 480);
        let next = shimmer("Reviewing approval request", 960);
        assert_eq!(first.to_string(), next.to_string());
        assert_eq!(first.width(), next.width());
        assert_ne!(first, next);
        assert!(first.spans.iter().all(|span| span.style.bg.is_none()));
        let text = "0123456789abcdefghijk";
        let cycle = text.chars().count() as i64 * 80;
        assert_eq!(shimmer(text, 0), shimmer(text, 79));
        assert_ne!(shimmer(text, 0), shimmer(text, 80));
        assert_eq!(shimmer(text, 0), shimmer(text, cycle));
        assert_ne!(shimmer(text, cycle - 240), shimmer(text, cycle + 240));
    }
    #[test]
    fn fenced_code_keeps_syntax_colors_when_wrapped() {
        let rows = prose(
            "```sql\nSELECT 'abcdefghijklmno'\n-- comment\nLIMIT 10;\n```",
            12,
        );
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.content == "SELECT" && span.style.fg == Some(ACCENT))
        );
        let quoted: String = rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| span.style.fg == Some(ratatui::style::Color::Rgb(221, 194, 139)))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(quoted, "'abcdefghijklmno'");
        assert!(rows.iter().all(|row| row.width() <= 12));
        let diff = prose("```diff\n+added\n-removed\n```", 40);
        assert!(
            diff.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.content == "+added" && span.style.fg == Some(GREEN))
        );
        assert!(
            diff.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.content == "-removed" && span.style.fg == Some(RED))
        );
    }
}
