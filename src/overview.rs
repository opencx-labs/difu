//! GitHub-style overview cards in a centered, readable column.
use crate::{
    app::{Action, Review},
    model::clean,
    ui::{BG, BORDER, DIM, GREEN, PANEL, RED, TEXT, TextRow, bold, link, prose, text},
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
    if row.image.is_some() {
        return vec![row];
    }
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
                    image: None,
                    hunk: None,
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
        image: None,
        hunk: None,
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
    if let Some(image) = &mut row.image {
        image.inset += 2;
    }
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
        image: row.image,
        hunk: row.hunk,
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
        "merged" => crate::ui::PURPLE,
        "closed" => RED,
        _ => DIM,
    }
}

#[cfg(test)]
pub(crate) fn rows(review: &Review, width: u16) -> Vec<TextRow> {
    rows_with_images(review, width, false)
}
pub(crate) fn rows_with_images(review: &Review, width: u16, images: bool) -> Vec<TextRow> {
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
                        .fg(crate::ui::INK)
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
    let mut reviewers = std::collections::BTreeMap::new();
    for item in &review.timeline {
        if let Some(state) = item.kind.strip_prefix("review · ") {
            reviewers.insert(item.author.clone(), state.replace('_', " "));
        }
    }
    for login in &pr.requested_reviewers {
        reviewers.insert(login.clone(), "requested".into());
    }
    for team in &pr.requested_teams {
        reviewers.insert(
            format!("{}/{} (team)", pr.key.owner, team),
            "requested".into(),
        );
    }
    let reviewer_rows = if reviewers.is_empty() {
        vec![text("No reviewers requested.", DIM)]
    } else {
        reviewers
            .into_iter()
            .flat_map(|(name, state)| {
                let color = match state.as_str() {
                    "approved" => GREEN,
                    "changes requested" => RED,
                    _ => DIM,
                };
                flow(
                    text(format!("{} · {}", clean(&name), clean(&state)), color),
                    inner,
                )
            })
            .collect()
    };
    rows.extend(card(width, vec![bold("REVIEWERS", TEXT)], reviewer_rows));
    rows.push(TextRow::default());
    let body = if pr.body.trim().is_empty() {
        vec![text("No description provided.", DIM)]
    } else {
        crate::images::rows(&pr.body, inner, pr, images)
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
        let mut body = crate::images::rows(
            &item.body,
            activity_width.saturating_sub(4).max(1),
            pr,
            images,
        );
        if !item.url.is_empty() {
            if !body.is_empty() {
                body.push(TextRow::default());
            }
            body.push(link("↗ View on GitHub", Action::Link(item.url.clone())));
        }
        for mut row in card(activity_width, header, body) {
            row.spans.insert(0, Span::raw(" ".repeat(inset)));
            if let Some(image) = &mut row.image {
                image.inset += inset as u16;
            }
            rows.push(row);
        }
    }
    rows.push(TextRow::default());
    if let Some(status) = &review.check_report
        && status.state == "OPEN"
        && pr.state == "open"
    {
        let (title, color, explanation) = match status.mergeable.as_str() {
            "CONFLICTING" => (
                "MERGE CONFLICTS",
                RED,
                "This branch has conflicts that must be resolved. Checks may be waiting for conflict resolution. Open / → PR controls → Resolve conflicts.",
            ),
            "MERGEABLE" => (
                "MERGE STATUS",
                GREEN,
                "No merge conflicts reported by GitHub.",
            ),
            _ => (
                "MERGE STATUS",
                DIM,
                "GitHub is calculating whether this branch has conflicts…",
            ),
        };
        let mut body = prose(explanation, inner);
        if review
            .detail
            .as_ref()
            .is_some_and(|pr| pr.head != status.head || pr.base != status.base)
        {
            body.extend(prose("This is the latest GitHub status; your review snapshot is older. Refresh to load the new revision.", inner));
        }
        rows.extend(card(width, vec![bold(title, color)], body));
        rows.push(TextRow::default());
    }
    if let Some(report) = &review.check_report {
        if !report.awaiting_workflows.is_empty() {
            let mut body = prose("Workflows will not run until approved by a user with write permission. Use / → PR controls → Approve workflows to run.", inner);
            for run in &report.awaiting_workflows {
                body.push(link(
                    format!("{} · run {}", run.name, run.id),
                    Action::Link(run.url.clone()),
                ));
            }
            rows.extend(card(
                width,
                vec![bold(
                    &format!(
                        "{} WORKFLOWS AWAITING APPROVAL",
                        report.awaiting_workflows.len()
                    ),
                    crate::ui::YELLOW,
                )],
                body,
            ));
            rows.push(TextRow::default());
        }
        if let Some(error) = &report.workflows_error {
            rows.extend(prose(error, inner));
        }
    }
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
            "pending" | "expected" => ("◌", crate::ui::YELLOW),
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
    let failed = review
        .checks
        .iter()
        .filter(|c| c.state == "fail")
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        rows.push(TextRow::default());
        let mut body = Vec::new();
        for check in failed {
            body.push(bold(clean(&check.name), RED));
            if let Some(failures) = review.failures.get(&check.url) {
                for test in &failures.tests {
                    body.extend(prose(&format!("× {}", clean(&test.name)), inner));
                    for line in &test.excerpt {
                        body.extend(prose(&format!("  {}", clean(line)), inner));
                    }
                    body.push(TextRow::default());
                }
                if let Some(explanation) = &failures.explanation {
                    body.extend(prose(explanation, inner));
                }
            } else {
                body.push(text("Loading failed test details…", DIM));
            }
            if !check.url.is_empty() {
                body.push(link(
                    "↗ Open failed check / full log",
                    Action::Link(check.url.clone()),
                ));
            }
            body.push(TextRow::default());
        }
        rows.extend(card(width, vec![bold("FAILED TESTS", RED)], body));
    }
    rows.push(TextRow::default());
    rows
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use crate::model::{Check, CheckReport, PrDetail, PrKey};
    use std::sync::Arc;
    #[test]
    fn reviewers_show_users_teams_and_latest_review_outcomes() -> anyhow::Result<()> {
        use anyhow::Context;
        let key = PrKey {
            owner: "example".into(),
            repo: "repo".into(),
            number: 1,
        };
        let pr = crate::github::parse_detail(
            &key,
            &serde_json::json!({
                "requested_reviewers": [{"login": "alice"}],
                "requested_teams": [{"slug": "platform", "name": "Platform team"}]
            }),
        )?;
        let review = Review {
            detail: Some(Arc::new(pr)),
            timeline: vec![
                crate::model::TimelineItem {
                    author: "bob".into(),
                    kind: "review · changes_requested".into(),
                    ..Default::default()
                },
                crate::model::TimelineItem {
                    author: "bob".into(),
                    kind: "review · approved".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let output = rows(&review, 110)
            .into_iter()
            .map(|row| {
                row.spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let card = output
            .split("description")
            .next()
            .context("reviewer card")?;
        assert!(card.contains("alice · requested"));
        assert!(card.contains("example/platform (team) · requested"));
        assert!(card.contains("bob · approved"));
        assert!(!card.contains("bob · changes requested"));
        // Old cached PR details remain readable after adding reviewer metadata.
        let mut cached = serde_json::to_value(review.detail.as_ref().context("detail")?.as_ref())?;
        cached
            .as_object_mut()
            .context("object")?
            .remove("requested_reviewers");
        cached
            .as_object_mut()
            .context("object")?
            .remove("requested_teams");
        let old: PrDetail = serde_json::from_value(cached)?;
        assert!(old.requested_reviewers.is_empty());
        assert!(old.requested_teams.is_empty());
        Ok(())
    }
    #[test]
    fn conflicts_and_empty_checks_are_not_parsing_errors_and_failures_follow_checks() {
        let mut review = Review {
            detail: Some(Arc::new(PrDetail {
                draft: false,
                requested_reviewers: Vec::new(),
                requested_teams: Vec::new(),
                key: PrKey {
                    owner: "example".into(),
                    repo: "repo".into(),
                    number: 1,
                },
                title: "PR".into(),
                body: String::new(),
                author: "author".into(),
                head: "head".into(),
                base: "base".into(),
                head_branch: "feature".into(),
                base_branch: "main".into(),
                state: "open".into(),
                additions: 1,
                deletions: 1,
                changed_files: 1,
            })),
            check_report: Some(CheckReport {
                state: "OPEN".into(),
                mergeable: "CONFLICTING".into(),
                head: "head".into(),
                base: "base".into(),
                ..CheckReport::default()
            }),
            checks: vec![Check {
                name: "lint".into(),
                state: "expected".into(),
                ..Check::default()
            }],
            ..Review::default()
        };
        let output = |r: &Review| {
            rows(r, 110)
                .iter()
                .map(|r| {
                    r.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = output(&review);
        assert!(text.contains("MERGE CONFLICTS"));
        assert!(text.contains("lint · expected"));
        assert!(!text.contains("EOF"));
        review.check_report = Some(CheckReport {
            state: "OPEN".into(),
            mergeable: "UNKNOWN".into(),
            ..CheckReport::default()
        });
        assert!(output(&review).contains("calculating"));
        assert!(!output(&review).contains("MERGE CONFLICTS"));
        review.checks = vec![Check {
            name: "unit tests".into(),
            state: "fail".into(),
            url: "https://github.com/example/repo/actions/runs/1/job/2".into(),
            ..Check::default()
        }];
        review.failures.insert(
            "https://github.com/example/repo/actions/runs/1/job/2".into(),
            crate::ci::parse("test mail::reply ... FAILED\nassertion failed"),
        );
        let text = output(&review);
        assert!(
            text.find("CHECKS")
                .zip(text.find("FAILED TESTS"))
                .is_some_and(|(a, b)| a < b)
        );
        assert!(text.contains("mail::reply"));
        assert!(text.contains("assertion failed"));
    }
}

#[cfg(test)]
mod status_color_tests {
    use super::*;
    #[test]
    fn lifecycle_colors_are_independent_of_theme_accent() {
        assert_eq!(state_color("merged"), crate::ui::PURPLE);
        assert_eq!(state_color("open"), GREEN);
        assert_eq!(state_color("closed"), RED);
        assert_ne!(state_color("merged"), crate::ui::ACCENT);
    }
}
