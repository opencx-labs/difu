//! GitHub-style overview cards in a centered, readable column.
use crate::{
    app::{Action, Review},
    model::clean,
    ui::{ACCENT, BG, BORDER, DIM, GREEN, PANEL, RED, TEXT, TextRow, bold, link, prose, text},
};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Span,
};
use unicode_width::UnicodeWidthChar;

const MAX_WIDTH: u16 = 110;

pub(crate) fn column(area: Rect) -> Rect {
    let width = area.width.min(MAX_WIDTH);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y,
        width,
        area.height,
    )
}

/// Preserve span styling and link actions when an unbroken URL or branch name
/// needs more rows. All widths are terminal cells, not UTF-8 bytes.
fn flow(row: TextRow, width: usize) -> Vec<TextRow> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut spans = Vec::new();
    let mut used = 0;
    for span in row.spans {
        let mut chunk = String::new();
        for ch in clean(&span.content).replace('\t', "    ").chars() {
            let cells = ch.width().unwrap_or(0);
            if ch == '\n' || used + cells > width {
                if !chunk.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                rows.push(TextRow {
                    spans: std::mem::take(&mut spans),
                    action: row.action.clone(),
                    target: None,
                    code_links: Vec::new(),
                });
                used = 0;
                if ch == '\n' {
                    continue;
                }
            }
            if cells > width {
                chunk.push('?');
                used += 1;
            } else {
                chunk.push(ch);
                used += cells;
            }
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, span.style));
        }
    }
    rows.push(TextRow {
        spans,
        action: row.action,
        target: None,
        code_links: Vec::new(),
    });
    rows
}

fn rule(width: usize, left: &str, right: &str) -> TextRow {
    text(
        format!("{left}{}{right}", "─".repeat(width.saturating_sub(2))),
        BORDER,
    )
}
fn padded(mut row: TextRow, width: usize, background: Color) -> TextRow {
    let used: usize = row.spans.iter().map(Span::width).sum();
    let mut spans = vec![
        Span::styled("│", Style::default().fg(BORDER)),
        Span::styled(" ", Style::default().bg(background)),
    ];
    for span in &mut row.spans {
        span.style = span.style.bg(background);
    }
    spans.extend(row.spans);
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(3 + used)),
        Style::default().bg(background),
    ));
    spans.push(Span::styled("│", Style::default().fg(BORDER)));
    TextRow {
        spans,
        action: row.action,
        target: None,
        code_links: Vec::new(),
    }
}
fn card(width: usize, header: Vec<TextRow>, body: Vec<TextRow>) -> Vec<TextRow> {
    if width < 6 {
        return header
            .into_iter()
            .chain(body)
            .flat_map(|row| flow(row, width))
            .collect();
    }
    let mut rows = vec![rule(width, "╭", "╮")];
    for row in header.into_iter().flat_map(|row| flow(row, width - 4)) {
        rows.push(padded(row, width, PANEL));
    }
    rows.push(rule(width, "├", "┤"));
    for row in body.into_iter().flat_map(|row| flow(row, width - 4)) {
        rows.push(padded(row, width, BG));
    }
    rows.push(rule(width, "╰", "╯"));
    rows
}
fn date(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.format("%b %-d, %Y · %H:%M %:z").to_string())
        .unwrap_or_else(|_| clean(value))
}
fn state_color(state: &str) -> Color {
    match state {
        "open" => GREEN,
        "merged" => ACCENT,
        "closed" => RED,
        _ => DIM,
    }
}

pub(crate) fn rows(review: &Review, width: u16) -> Vec<TextRow> {
    let width = width as usize;
    let inner = width.saturating_sub(4).max(1);
    let Some(pr) = review.detail.as_ref() else {
        return card(
            width,
            vec![bold("Pull request", TEXT)],
            prose(
                review
                    .detail_error
                    .as_deref()
                    .unwrap_or("Loading PR details…"),
                inner,
            ),
        );
    };
    let mut rows = vec![TextRow::default()];
    rows.extend(prose(&pr.title, width).into_iter().map(|mut row| {
        for span in &mut row.spans {
            span.style = span.style.add_modifier(ratatui::style::Modifier::BOLD);
        }
        row
    }));
    rows.extend(flow(text(pr.key.id(), DIM), width));
    rows.push(TextRow::default());
    rows.extend(flow(
        TextRow {
            spans: vec![
                Span::styled(
                    format!(" {} ", clean(&pr.state)),
                    Style::default()
                        .fg(BG)
                        .bg(state_color(&pr.state))
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  {}  ·  {} ← {}",
                        clean(&pr.author),
                        clean(&pr.base_branch),
                        clean(&pr.head_branch)
                    ),
                    Style::default().fg(DIM),
                ),
            ],
            ..Default::default()
        },
        width,
    ));
    rows.extend(flow(
        TextRow {
            spans: vec![
                Span::styled(
                    format!("{} files changed  ", pr.changed_files),
                    Style::default().fg(DIM),
                ),
                Span::styled(format!("+{}  ", pr.additions), Style::default().fg(GREEN)),
                Span::styled(format!("−{}", pr.deletions), Style::default().fg(RED)),
            ],
            ..Default::default()
        },
        width,
    ));
    rows.push(TextRow::default());
    let body = if pr.body.trim().is_empty() {
        vec![text("No description provided.", DIM)]
    } else {
        prose(&pr.body, inner)
    };
    rows.extend(card(
        width,
        vec![bold(format!("{} · description", clean(&pr.author)), TEXT)],
        body,
    ));
    rows.push(TextRow::default());
    rows.push(bold("ACTIVITY", DIM));
    if let Some(error) = &review.timeline_error {
        rows.extend(card(
            width,
            vec![bold("Activity could not be loaded", RED)],
            prose(error, inner),
        ));
    }
    let inset = usize::from(width >= 12) * 2;
    let activity_width = width.saturating_sub(inset);
    for item in &review.timeline {
        rows.push(text("  │", BORDER));
        let header = vec![
            bold(
                format!("{} · {}", clean(&item.author), clean(&item.kind)),
                TEXT,
            ),
            text(date(&item.date), DIM),
        ];
        let mut body = prose(&item.body, activity_width.saturating_sub(4).max(1));
        if !item.url.is_empty() {
            if !body.is_empty() {
                body.push(TextRow::default());
            }
            body.push(link("↗ View on GitHub", Action::Link(item.url.clone())));
        }
        for mut row in card(activity_width, header, body) {
            row.spans.insert(0, Span::raw(" ".repeat(inset)));
            rows.push(row);
        }
    }
    rows.push(TextRow::default());
    let mut checks = Vec::new();
    if let Some(error) = &review.checks_error {
        checks.extend(prose(error, inner));
    }
    if review.checks.is_empty() && review.checks_error.is_none() {
        checks.push(text("No checks reported", DIM));
    }
    for check in &review.checks {
        let (symbol, color) = match check.state.as_str() {
            "pass" => ("✓", GREEN),
            "fail" => ("×", RED),
            "pending" => ("◌", ACCENT),
            _ => ("−", DIM),
        };
        let duration = chrono::DateTime::parse_from_rfc3339(&check.started)
            .ok()
            .map(|start| {
                let end = chrono::DateTime::parse_from_rfc3339(&check.completed)
                    .unwrap_or_else(|_| chrono::Utc::now().fixed_offset());
                format!(" · {}s", (end - start).num_seconds().max(0))
            })
            .unwrap_or_default();
        let mut row = text(
            format!(
                "{symbol} {} · {}{duration}",
                clean(&check.name),
                clean(&check.state)
            ),
            color,
        );
        if !check.url.is_empty() {
            row.action = Some(Action::Link(check.url.clone()));
        }
        checks.push(row);
    }
    rows.extend(card(
        width,
        vec![bold(
            format!("CHECKS · {} · live every 10s", review.checks.len()),
            TEXT,
        )],
        checks,
    ));
    rows.push(TextRow::default());
    rows
}
