use crate::{
    app::{Action, App, Focus, Modal, View},
    diff::{DiffFile, DiffLine, Hunk, LineKind, split_rows},
    model::{InboxTab, ModelChoice, PrState, clean},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BG: Color = Color::Rgb(12, 14, 18);
const PANEL: Color = Color::Rgb(18, 21, 27);
const TEXT: Color = Color::Rgb(220, 225, 232);
const DIM: Color = Color::Rgb(130, 140, 156);
const BORDER: Color = Color::Rgb(42, 48, 61);
const ACCENT: Color = Color::Rgb(183, 161, 255);
const GREEN: Color = Color::Rgb(114, 216, 163);
const RED: Color = Color::Rgb(247, 137, 145);
const ADD_BG: Color = Color::Rgb(18, 43, 32);
const REMOVE_BG: Color = Color::Rgb(49, 25, 31);

#[derive(Clone, Default)]
pub struct TextRow {
    pub spans: Vec<Span<'static>>,
    pub action: Option<Action>,
}
#[derive(Clone, Default)]
pub struct Row {
    pub left: TextRow,
    pub right: TextRow,
}
pub struct Section {
    pub start: usize,
    pub end: usize,
    pub left: Vec<TextRow>,
}
pub struct Document {
    pub epoch: u64,
    pub width: u16,
    pub horizontal: usize,
    pub rows: Vec<Row>,
    pub sections: Vec<Section>,
    pub guide_columns: bool,
    pub left_width: u16,
}

fn span(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(color))
}
fn text(value: impl Into<String>, color: Color) -> TextRow {
    TextRow {
        spans: vec![span(value, color)],
        action: None,
    }
}
fn bold(value: impl Into<String>, color: Color) -> TextRow {
    TextRow {
        spans: vec![Span::styled(
            value.into(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )],
        action: None,
    }
}
fn link(value: impl Into<String>, action: Action) -> TextRow {
    TextRow {
        spans: vec![Span::styled(
            value.into(),
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::UNDERLINED),
        )],
        action: Some(action),
    }
}
fn append(rows: &mut Vec<Row>, right: TextRow) {
    rows.push(Row {
        right,
        ..Default::default()
    });
}

fn inline(source: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut code = false;
    let mut strong = false;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        let toggle = c == '`' || (c == '*' && chars.peek() == Some(&'*'));
        if toggle {
            if !buffer.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut buffer),
                    Style::default()
                        .fg(if code { ACCENT } else { TEXT })
                        .add_modifier(if strong {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ));
            }
            if c == '`' {
                code = !code;
            } else {
                chars.next();
                strong = !strong;
            }
        } else {
            buffer.push(c);
        }
    }
    if !buffer.is_empty() {
        spans.push(Span::styled(
            buffer,
            Style::default()
                .fg(if code { ACCENT } else { TEXT })
                .add_modifier(if strong {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
    }
    spans
}
fn prose(source: &str, width: usize) -> Vec<TextRow> {
    let mut rows = Vec::new();
    let source = clean(source);
    let mut fence = false;
    for line in source.lines() {
        if line.starts_with("```") {
            fence = !fence;
            rows.push(text(if fence { "┌ code" } else { "└────" }, DIM));
            continue;
        }
        let heading = line.starts_with('#');
        let line = if heading {
            line.trim_start_matches('#').trim()
        } else {
            line
        };
        if line.is_empty() {
            rows.push(TextRow::default());
            continue;
        }
        for wrapped in textwrap::wrap(line, width.max(1)) {
            rows.push(if heading {
                bold(wrapped.into_owned(), TEXT)
            } else if fence {
                text(wrapped.into_owned(), DIM)
            } else {
                TextRow {
                    spans: inline(&wrapped),
                    action: None,
                }
            });
        }
        // Retain clickable web destinations in Markdown without trusting terminal
        // escape sequences or allowing file/command URI schemes.
        let mut rest = line;
        while let Some(start) = rest.find("](") {
            let Some(suffix) = rest.get(start.saturating_add(2)..) else {
                break;
            };
            rest = suffix;
            let Some(end) = rest.find(')') else {
                break;
            };
            let Some(url) = rest.get(..end) else {
                break;
            };
            let url = url.trim_matches(|c| c == '<' || c == '>');
            if url.starts_with("https://") || url.starts_with("http://") {
                rows.push(link(format!("↗ {url}"), Action::Link(url.into())));
            }
            let Some(suffix) = rest.get(end.saturating_add(1)..) else {
                break;
            };
            rest = suffix;
        }
    }
    rows
}

/// Crop by terminal cells, never UTF-8 bytes. A clipped wide glyph becomes a
/// space so the next character stays in the correct column.
fn crop(value: &str, offset: usize, width: usize) -> String {
    let mut output = String::new();
    let mut position = 0;
    let mut written = 0;
    for ch in clean(value).replace('\t', "    ").chars() {
        let size = ch.width().unwrap_or(0);
        if position + size <= offset {
            position += size;
            continue;
        }
        if position < offset {
            if written < width {
                output.push(' ');
                written += 1;
            }
            position += size;
            continue;
        }
        if written + size > width {
            break;
        }
        output.push(ch);
        written += size;
        position += size;
    }
    output
}

fn code(line: Option<&DiffLine>, old: bool, width: usize, horizontal: usize) -> Vec<Span<'static>> {
    let Some(line) = line else {
        return vec![span(" ".repeat(width), DIM)];
    };
    let bg = match line.kind {
        LineKind::Add => ADD_BG,
        LineKind::Remove => REMOVE_BG,
        _ => BG,
    };
    let number = if old { line.old } else { line.new };
    let prefix = format!(
        "{:>5} {} ",
        number.map(|n| n.to_string()).unwrap_or_default(),
        match line.kind {
            LineKind::Add => '+',
            LineKind::Remove => '-',
            _ => ' ',
        }
    );
    let prefix = crop(&prefix, 0, width);
    let code_width = width.saturating_sub(prefix.width());
    let content = crop(&line.text, horizontal, code_width);
    let mut spans = vec![Span::styled(
        prefix,
        Style::default()
            .fg(match line.kind {
                LineKind::Add => GREEN,
                LineKind::Remove => RED,
                _ => DIM,
            })
            .bg(bg),
    )];
    // A small lexical highlighter keeps rendering independent of language parsers.
    // Diff colors remain meaningful for every file type.
    let mut token = String::new();
    let mut quoted = false;
    let mut quote = '\0';
    let comment = content.trim_start().starts_with("//") || content.trim_start().starts_with('#');
    let emit = |token: &mut String, spans: &mut Vec<Span<'static>>, quoted: bool| {
        if token.is_empty() {
            return;
        }
        let color = if comment {
            DIM
        } else if quoted {
            Color::Rgb(221, 194, 139)
        } else if [
            "fn",
            "pub",
            "let",
            "const",
            "return",
            "if",
            "else",
            "async",
            "await",
            "function",
            "export",
            "import",
            "type",
            "interface",
            "struct",
            "impl",
            "match",
            "class",
            "def",
            "from",
            "SELECT",
            "UPDATE",
            "WHERE",
            "SET",
        ]
        .contains(&token.as_str())
        {
            ACCENT
        } else if token.chars().all(|c| c.is_ascii_digit()) {
            Color::Rgb(224, 171, 129)
        } else {
            TEXT
        };
        spans.push(Span::styled(
            std::mem::take(token),
            Style::default().fg(color).bg(bg),
        ));
    };
    for c in content.chars() {
        if c == '\'' || c == '"' || c == '`' {
            emit(&mut token, &mut spans, quoted);
            if !quoted {
                quoted = true;
                quote = c;
            } else if quote == c {
                quoted = false;
            }
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::Rgb(221, 194, 139)).bg(bg),
            ));
        } else if !quoted && !c.is_alphanumeric() && c != '_' {
            emit(&mut token, &mut spans, false);
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(TEXT).bg(bg),
            ));
        } else {
            token.push(c);
        }
    }
    emit(&mut token, &mut spans, quoted);
    spans.push(Span::styled(
        " ".repeat(code_width.saturating_sub(content.width())),
        Style::default().bg(bg),
    ));
    spans
}

fn hunk_rows(
    file: &DiffFile,
    hunk: &Hunk,
    width: usize,
    split: bool,
    horizontal: usize,
    with_title: bool,
) -> Vec<TextRow> {
    let mut rows = Vec::new();
    if with_title {
        rows.push(bold(
            format!(
                " {}   +{} −{}",
                clean(&file.path),
                file.additions,
                file.deletions
            ),
            TEXT,
        ));
    }
    rows.push(text(format!(" {}", hunk.header), DIM));
    if split {
        let left = width.saturating_sub(1) / 2;
        let right = width.saturating_sub(left + 1);
        for (old, new) in split_rows(hunk) {
            let mut spans = code(old, true, left, horizontal);
            spans.push(span("│", BORDER));
            spans.extend(code(new, false, right, horizontal));
            rows.push(TextRow {
                spans,
                action: None,
            });
        }
    } else {
        for line in &hunk.lines {
            rows.push(TextRow {
                spans: code(Some(line), line.kind == LineKind::Remove, width, horizontal),
                action: None,
            });
        }
    }
    rows.push(TextRow::default());
    rows
}

fn build(app: &App, width: u16) -> Document {
    let mut doc = Document {
        epoch: app.epoch,
        width,
        horizontal: app.horizontal,
        rows: Vec::new(),
        sections: Vec::new(),
        guide_columns: false,
        left_width: 0,
    };
    let Some(review) = app.review() else {
        return doc;
    };
    if app.view == View::Overview {
        let Some(pr) = review.detail.as_ref() else {
            append(
                &mut doc.rows,
                text(
                    review
                        .detail_error
                        .as_deref()
                        .unwrap_or("Loading PR details…"),
                    DIM,
                ),
            );
            return doc;
        };
        let width = width.saturating_sub(4) as usize;
        for row in prose(&pr.title, width) {
            append(&mut doc.rows, row);
        }
        append(
            &mut doc.rows,
            text(
                format!(
                    "{} · {} · {} ← {}",
                    pr.key.id(),
                    pr.author,
                    pr.base_branch,
                    pr.head_branch
                ),
                DIM,
            ),
        );
        append(
            &mut doc.rows,
            text(
                format!(
                    "{} files changed   +{} −{}   {}",
                    pr.changed_files, pr.additions, pr.deletions, pr.state
                ),
                GREEN,
            ),
        );
        append(&mut doc.rows, TextRow::default());
        for row in prose(&pr.body, width) {
            append(&mut doc.rows, row);
        }
        append(&mut doc.rows, TextRow::default());
        append(&mut doc.rows, bold("ACTIVITY", ACCENT));
        append(&mut doc.rows, TextRow::default());
        if let Some(error) = &review.timeline_error {
            for row in prose(error, width) {
                append(&mut doc.rows, row);
            }
        }
        for item in &review.timeline {
            append(
                &mut doc.rows,
                bold(
                    format!("{} · {}", clean(&item.author), clean(&item.kind)),
                    TEXT,
                ),
            );
            append(&mut doc.rows, text(&item.date, DIM));
            for row in prose(&item.body, width) {
                append(&mut doc.rows, row);
            }
            if !item.url.is_empty() {
                append(
                    &mut doc.rows,
                    link("↗ View on GitHub", Action::Link(item.url.clone())),
                );
            }
            append(&mut doc.rows, text("─".repeat(width.min(70)), BORDER));
            append(&mut doc.rows, TextRow::default());
        }
        append(&mut doc.rows, bold("CHECKS · live every 10s", ACCENT));
        if let Some(error) = &review.checks_error {
            for row in prose(error, width) {
                append(&mut doc.rows, row);
            }
        }
        if review.checks.is_empty() && review.checks_error.is_none() {
            append(&mut doc.rows, text("No checks reported", DIM));
        }
        for check in &review.checks {
            let color = match check.state.as_str() {
                "pass" => GREEN,
                "fail" => RED,
                _ => DIM,
            };
            let duration = chrono::DateTime::parse_from_rfc3339(&check.started)
                .ok()
                .map(|start| {
                    let end = chrono::DateTime::parse_from_rfc3339(&check.completed)
                        .unwrap_or_else(|_| chrono::Utc::now().fixed_offset());
                    format!("{}s", (end - start).num_seconds().max(0))
                })
                .unwrap_or_default();
            let mut row = text(
                format!("{}  {}  {}", check.state, clean(&check.name), duration),
                color,
            );
            if !check.url.is_empty() {
                row.action = Some(Action::Link(check.url.clone()));
            }
            append(&mut doc.rows, row);
        }
        return doc;
    }
    let Some(snapshot) = review.snapshot.as_ref() else {
        append(
            &mut doc.rows,
            text(
                if review.preparing {
                    "Fetching the PR revisions and preparing the diff…"
                } else {
                    "Open a PR to load its diff."
                },
                DIM,
            ),
        );
        if let Some(error) = &review.guide_error {
            for row in prose(error, width.saturating_sub(4) as usize) {
                append(&mut doc.rows, row);
            }
        }
        return doc;
    };
    if app.view == View::Guide
        && let Some(guide) = &review.guide
    {
        let wide = width >= 132;
        doc.guide_columns = wide;
        doc.left_width = if wide { (width / 4).clamp(30, 44) } else { 0 };
        let code_width = width.saturating_sub(if wide { doc.left_width + 3 } else { 0 }) as usize;
        let split = wide && !app.config.unified;
        for (chapter_index, chapter) in guide.chapters.iter().enumerate() {
            let start = doc.rows.len();
            let prose_width = if wide {
                doc.left_width as usize
            } else {
                code_width
            };
            let mut left = prose(&chapter.title, prose_width);
            for row in &mut left {
                for span in &mut row.spans {
                    span.style = span.style.fg(TEXT).add_modifier(Modifier::BOLD);
                }
            }
            left.push(text(
                format!("{:02} / {:02}", chapter_index + 1, guide.chapters.len()),
                DIM,
            ));
            left.push(TextRow::default());
            left.extend(prose(&chapter.explanation, prose_width));
            left.push(TextRow::default());
            let mut right = Vec::new();
            let mut last_file = String::new();
            let mut seen = std::collections::HashSet::new();
            let prose_length = left.len();
            // Compact chapters place explanation before all code. File links
            // below it point to actual document rows, calculated after layout.
            let mut links = Vec::new();
            for id in &chapter.hunks {
                if let Some((file, hunk)) = snapshot.find(id) {
                    let title = last_file != file.path;
                    last_file = file.path.clone();
                    if seen.insert(file.path.clone()) {
                        links.push((file.path.clone(), right.len()));
                    }
                    right.extend(hunk_rows(
                        file,
                        hunk,
                        code_width,
                        split,
                        app.horizontal,
                        title,
                    ));
                }
            }
            let offset = if wide {
                start
            } else {
                start + prose_length + links.len() + 1
            };
            for (path, row) in links {
                left.push(link(
                    format!("↳ {}", clean(&path)),
                    Action::Jump(offset + row),
                ));
            }
            left.push(TextRow::default());
            if wide {
                let count = left.len().max(right.len());
                for i in 0..count {
                    doc.rows.push(Row {
                        left: left.get(i).cloned().unwrap_or_default(),
                        right: right.get(i).cloned().unwrap_or_default(),
                    });
                }
            } else {
                for row in &left {
                    append(&mut doc.rows, row.clone());
                }
                for row in right {
                    append(&mut doc.rows, row);
                }
            }
            doc.sections.push(Section {
                start,
                end: doc.rows.len(),
                left,
            });
            for _ in 0..2 {
                doc.rows.push(Row::default());
            }
        }
    } else {
        if let Some(file) = snapshot.files.get(app.file) {
            for (i, hunk) in file.hunks.iter().enumerate() {
                for row in hunk_rows(
                    file,
                    hunk,
                    width as usize,
                    width >= 80 && !app.config.unified,
                    app.horizontal,
                    i == 0,
                ) {
                    append(&mut doc.rows, row);
                }
            }
        }
    }
    doc
}

fn paint(frame: &mut Frame, rect: Rect, row: &TextRow, app: &mut App) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(Line::from(row.spans.clone())), rect);
    if let Some(action) = &row.action {
        app.hits.push((rect, action.clone()));
    }
}
fn button(
    frame: &mut Frame,
    app: &mut App,
    x: u16,
    y: u16,
    label: &str,
    active: bool,
    action: Action,
) -> u16 {
    let width = (label.width() + 4) as u16;
    let rect = Rect::new(x, y, width.min(frame.area().right().saturating_sub(x)), 1);
    frame.render_widget(
        Paragraph::new(format!("  {label}  ")).style(
            Style::default()
                .fg(if active { BG } else { DIM })
                .bg(if active { ACCENT } else { PANEL }),
        ),
        rect,
    );
    app.hits.push((rect, action));
    x + width + 1
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.hits.clear();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    if area.width < 24 || area.height < 8 {
        frame.render_widget(Paragraph::new("difu · resize to at least 24 × 8"), area);
        return;
    }
    let title = Rect::new(2, 1, area.width.saturating_sub(4), 1);
    let identity = app.key().unwrap_or_else(|| app.inbox_tab.label().into());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "difu",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            span(format!("  /  {identity}"), DIM),
        ])),
        title,
    );
    let mut x = 2;
    let tabs = if app.home {
        vec![
            (
                if area.width < 70 {
                    "1 Reviews"
                } else {
                    "1 Review requests"
                },
                app.inbox_tab == InboxTab::ReviewRequests,
                Action::SetInbox(InboxTab::ReviewRequests),
            ),
            (
                "2 Authored",
                app.inbox_tab == InboxTab::Authored,
                Action::SetInbox(InboxTab::Authored),
            ),
            (
                if area.width < 70 {
                    "3 Repos"
                } else {
                    "3 Repositories"
                },
                app.inbox_tab == InboxTab::Repositories,
                Action::SetInbox(InboxTab::Repositories),
            ),
        ]
    } else {
        vec![
            (
                "1 Overview",
                app.view == View::Overview,
                Action::SetView(View::Overview),
            ),
            (
                "2 Guide",
                app.view == View::Guide,
                Action::SetView(View::Guide),
            ),
            (
                "3 Diff",
                app.view == View::Diff,
                Action::SetView(View::Diff),
            ),
            ("Esc Home", false, Action::Back),
        ]
    };
    for (label, selected, action) in tabs {
        if x + label.len() as u16 + 4 < area.width {
            x = button(frame, app, x, 3, label, selected, action);
        }
    }
    let filters = app.home && app.inbox_tab != InboxTab::ReviewRequests && area.height >= 14;
    if filters {
        let mut x = 2;
        for state in PrState::ALL {
            if x + state.label().len() as u16 + 4 < area.width {
                x = button(
                    frame,
                    app,
                    x,
                    5,
                    state.label(),
                    app.state() == state,
                    Action::SetState(state),
                );
            }
        }
        if app.inbox_tab == InboxTab::Repositories {
            let enabled = app
                .config
                .review_repositories
                .values()
                .filter(|v| **v)
                .count();
            let label = format!(
                "F4 Repos {enabled}/{}",
                app.config.review_repositories.len()
            );
            let x = button(frame, app, 2, 6, &label, false, Action::Repositories(false));
            if x + 24 < area.width {
                button(
                    frame,
                    app,
                    x,
                    6,
                    "Shift+F4 Whitelist",
                    false,
                    Action::Repositories(true),
                );
            }
        }
    }
    let content_y = if filters {
        if app.inbox_tab == InboxTab::Repositories {
            8
        } else {
            7
        }
    } else {
        5
    };
    let content = Rect::new(
        2,
        content_y,
        area.width.saturating_sub(4),
        area.height.saturating_sub(content_y + 4),
    );
    let has_guide = app.view == View::Guide && app.review().is_some_and(|r| r.guide.is_some());
    let navigation = app.home || (app.view != View::Overview && !has_guide);
    let nav_width = if navigation {
        (content.width / 4)
            .clamp(18, 42)
            .min(content.width.saturating_sub(12))
    } else {
        0
    };
    let main = if navigation {
        Rect::new(
            content.x + nav_width + 2,
            content.y,
            content.width.saturating_sub(nav_width + 2),
            content.height,
        )
    } else {
        content
    };
    app.content_rect = main;
    app.viewport = main.height as usize;
    app.hits.push((main, Action::Focus(Focus::Content)));
    if navigation {
        let nav = Rect::new(content.x, content.y, nav_width, content.height);
        frame.render_widget(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(if app.focus == Focus::Navigation {
                    ACCENT
                } else {
                    BORDER
                })),
            Rect::new(nav.x, nav.y, nav.width + 1, nav.height),
        );
        app.hits.push((nav, Action::Focus(Focus::Navigation)));
        if app.home {
            draw_inbox(frame, app, nav);
        } else {
            draw_files(frame, app, nav);
        }
    }
    if app.document.as_ref().is_none_or(|d| {
        d.epoch != app.epoch || d.width != main.width || d.horizontal != app.horizontal
    }) {
        app.document = Some(build(app, main.width));
    }
    let Some(doc) = app.document.take() else {
        return;
    };
    app.scroll = app
        .scroll
        .min(doc.rows.len().saturating_sub(main.height as usize));
    for y in 0..main.height {
        let index = app.scroll + y as usize;
        let Some(row) = doc.rows.get(index) else {
            break;
        };
        if doc.guide_columns {
            let mut left = &row.left;
            if let Some(section) = doc
                .sections
                .iter()
                .find(|section| index >= section.start && index < section.end)
            {
                let sticky = section
                    .start
                    .max(app.scroll)
                    .min(section.end.saturating_sub(section.left.len()));
                left = section
                    .left
                    .get(index.saturating_sub(sticky))
                    .unwrap_or(&row.left);
                if index < sticky {
                    left = &row.left;
                }
            }
            paint(
                frame,
                Rect::new(main.x, main.y + y, doc.left_width, 1),
                left,
                app,
            );
            paint(
                frame,
                Rect::new(
                    main.x + doc.left_width + 3,
                    main.y + y,
                    main.width.saturating_sub(doc.left_width + 3),
                    1,
                ),
                &row.right,
                app,
            );
        } else {
            paint(
                frame,
                Rect::new(main.x, main.y + y, main.width, 1),
                &row.right,
                app,
            );
        }
    }
    if doc.rows.len() > main.height as usize && main.height > 0 {
        let thumb = ((app.scroll * main.height as usize) / doc.rows.len())
            .min(main.height as usize - 1) as u16;
        frame.render_widget(
            Paragraph::new("┃").style(Style::default().fg(ACCENT)),
            Rect::new(area.width - 1, main.y + thumb, 1, 1),
        );
    }
    app.document = Some(doc);
    let status = if let Some(review) = app.review() {
        if let Some(job) = &review.generation {
            format!(
                "◌ Generating guide · {}s · {}",
                job.started.elapsed().as_secs(),
                job.activity
            )
        } else if review.preparing {
            "◌ Preparing PR snapshot…".into()
        } else if review.newer.is_some() {
            "● PR updated · F5 to refresh this snapshot".into()
        } else if let Some(error) = &review.guide_error {
            format!("Guide: {} · F6 retry", clean(error).replace('\n', " "))
        } else if let Some(model) = &review.guide_model {
            format!("Guide ready · {model}")
        } else {
            format!("{}", app.config.model)
        }
    } else if app.inbox_loading {
        format!("Loading {}…", app.inbox_tab.label().to_lowercase())
    } else {
        app.notice.clone()
    };
    frame.render_widget(
        Paragraph::new(crop(&status, 0, area.width.saturating_sub(4) as usize)).style(
            Style::default().fg(if app.review().is_some_and(|r| r.guide_error.is_some()) {
                RED
            } else {
                DIM
            }),
        ),
        Rect::new(2, area.height - 3, area.width - 4, 1),
    );
    let mut footer_x = 2;
    let footer = if app.home {
        vec![
            ("F1 Help", Action::Help),
            (
                "F3 State",
                Action::SetState(match app.state() {
                    PrState::Open => PrState::Merged,
                    PrState::Merged => PrState::Closed,
                    PrState::Closed => PrState::All,
                    PrState::All => PrState::Open,
                }),
            ),
            ("F4 Repos", Action::Repositories(false)),
            ("F5 Refresh", Action::Refresh),
            ("Enter Open", Action::OpenPr),
        ]
    } else {
        vec![
            ("F1 Help", Action::Help),
            ("F2 Model", Action::Models),
            ("F5 Refresh", Action::Refresh),
            ("F6 Generate", Action::Regenerate),
            ("F8 Cancel", Action::Cancel),
            ("Ctrl+B Split", Action::ToggleLayout),
            ("Esc Home", Action::Back),
        ]
    };
    for (label, action) in footer {
        if app.home
            && ((label == "F3 State" && app.inbox_tab == InboxTab::ReviewRequests)
                || (label == "F4 Repos" && app.inbox_tab != InboxTab::Repositories))
        {
            continue;
        }
        let width = label.len() as u16;
        if footer_x + width > area.width.saturating_sub(2) {
            break;
        }
        let rect = Rect::new(footer_x, area.height - 2, width, 1);
        frame.render_widget(Paragraph::new(label).style(Style::default().fg(DIM)), rect);
        app.hits.push((rect, action));
        footer_x += width + 2;
    }
    if !app.notice.is_empty() {
        frame.render_widget(
            Paragraph::new(crop(
                &clean(&app.notice).replace('\n', " "),
                0,
                area.width.saturating_sub(4) as usize,
            ))
            .style(Style::default().fg(ACCENT)),
            Rect::new(2, area.height - 1, area.width - 4, 1),
        );
    }
    if app.modal.is_some() {
        draw_modal(frame, app);
    }
}

fn draw_inbox(frame: &mut Frame, app: &mut App, rect: Rect) {
    paint(
        frame,
        Rect::new(rect.x, rect.y, rect.width, 1),
        &bold(
            format!(
                "{}  {}",
                app.inbox_tab.label().to_uppercase(),
                app.inbox.len()
            ),
            DIM,
        ),
        app,
    );
    if app.inbox.is_empty() {
        let message = app.inbox_error.clone().unwrap_or_else(|| {
            if app.inbox_loading {
                "Loading GitHub…".into()
            } else {
                match app.inbox_tab {
                    InboxTab::ReviewRequests => "No open PRs requesting your review.".into(),
                    InboxTab::Authored => {
                        format!("No {} authored PRs.", app.state().label().to_lowercase())
                    }
                    InboxTab::Repositories if app.config.review_repositories.is_empty() => {
                        "Choose repositories first. Shift+F4 opens the whitelist.".into()
                    }
                    InboxTab::Repositories
                        if !app
                            .config
                            .review_repositories
                            .values()
                            .any(|enabled| *enabled) =>
                    {
                        "No repositories enabled. F4 opens filters.".into()
                    }
                    InboxTab::Repositories => format!(
                        "No {} PRs in the enabled repositories.",
                        app.state().label().to_lowercase()
                    ),
                }
            }
        });
        for (i, row) in prose(&message, rect.width as usize)
            .iter()
            .take(rect.height.saturating_sub(2) as usize)
            .enumerate()
        {
            paint(
                frame,
                Rect::new(rect.x, rect.y + 2 + i as u16, rect.width, 1),
                row,
                app,
            );
        }
        return;
    }
    let visible = (rect.height.saturating_sub(2) / 4).max(1) as usize;
    if app.selected < app.nav_scroll {
        app.nav_scroll = app.selected;
    }
    if app.selected >= app.nav_scroll + visible {
        app.nav_scroll = app.selected + 1 - visible;
    }
    for index in app.nav_scroll..(app.nav_scroll + visible).min(app.inbox.len()) {
        let Some(pr) = app.inbox.get(index) else {
            continue;
        };
        let y = rect.y + 2 + ((index - app.nav_scroll) * 4) as u16;
        if y >= rect.bottom() {
            break;
        }
        let selected = index == app.selected;
        let row_rect = Rect::new(
            rect.x,
            y,
            rect.width.saturating_sub(1),
            3.min(rect.bottom() - y),
        );
        frame.render_widget(
            Block::default().style(Style::default().bg(if selected { PANEL } else { BG })),
            row_rect,
        );
        frame.render_widget(
            Paragraph::new(crop(
                &format!(
                    "{} #{}  {}",
                    if selected { "▸" } else { " " },
                    pr.key.number,
                    pr.key.repo
                ),
                0,
                rect.width.saturating_sub(2) as usize,
            ))
            .style(Style::default().fg(if selected { ACCENT } else { DIM })),
            Rect::new(rect.x, y, rect.width - 1, 1),
        );
        let title = crop(&pr.title, 0, rect.width.saturating_sub(3) as usize);
        frame.render_widget(
            Paragraph::new(format!(" {title}")).style(Style::default().fg(TEXT)),
            Rect::new(rect.x, y + 1, rect.width - 1, 1),
        );
        if y + 2 < rect.bottom() {
            frame.render_widget(
                Paragraph::new(crop(
                    &format!(" {}{}", pr.author, if pr.draft { " · draft" } else { "" }),
                    0,
                    rect.width.saturating_sub(2) as usize,
                ))
                .style(Style::default().fg(DIM)),
                Rect::new(rect.x, y + 2, rect.width - 1, 1),
            );
        }
        app.hits.push((row_rect, Action::SelectPr(index)));
    }
}

fn draw_files(frame: &mut Frame, app: &mut App, rect: Rect) {
    paint(
        frame,
        Rect::new(rect.x, rect.y, rect.width, 1),
        &bold("FILES", DIM),
        app,
    );
    let files = app.review().and_then(|r| r.snapshot.clone());
    let Some(snapshot) = files else {
        return;
    };
    let mut entries = Vec::<(String, Option<usize>)>::new();
    let mut previous = Vec::<String>::new();
    let mut selected_row = 0;
    for (index, file) in snapshot.files.iter().enumerate() {
        let parts = file.path.split('/').collect::<Vec<_>>();
        let Some((_, parents)) = parts.split_last() else {
            continue;
        };
        let common = parents
            .iter()
            .zip(&previous)
            .take_while(|(a, b)| **a == b.as_str())
            .count();
        for (depth, parent) in parents.iter().enumerate().skip(common) {
            entries.push((format!("{}{parent}/", "  ".repeat(depth.min(5))), None));
        }
        previous = parents.iter().map(|s| s.to_string()).collect();
        if index == app.file {
            selected_row = entries.len();
        }
        entries.push((
            format!(
                "{}{} {}",
                "  ".repeat(parents.len().min(5)),
                if index == app.file { "▸" } else { " " },
                parts.last().unwrap_or(&"")
            ),
            Some(index),
        ));
    }
    let height = rect.height.saturating_sub(2) as usize;
    let start = selected_row.saturating_sub(height.saturating_sub(1));
    for (offset, (label, index)) in entries.iter().skip(start).take(height).enumerate() {
        let y = rect.y + 2 + offset as u16;
        let color = if *index == Some(app.file) {
            ACCENT
        } else if index.is_some() {
            TEXT
        } else {
            DIM
        };
        let row = text(crop(label, 0, rect.width.saturating_sub(1) as usize), color);
        paint(frame, Rect::new(rect.x, y, rect.width - 1, 1), &row, app);
        if let Some(index) = index {
            app.hits.push((
                Rect::new(rect.x, y, rect.width - 1, 1),
                Action::SelectFile(*index),
            ));
        }
    }
}

fn draw_modal(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(88);
    let height = area.height.saturating_sub(4).min(26);
    let rect = Rect::new(
        (area.width - width) / 2,
        (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT))
            .style(Style::default().bg(PANEL)),
        rect,
    );
    app.hits.clear();
    let inner = Rect::new(
        rect.x + 2,
        rect.y + 1,
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(2),
    );
    let Some(modal) = &app.modal else {
        return;
    };
    let rows = match modal {
        Modal::Repositories {
            manage,
            query,
            selected,
            choices,
        } => {
            let options = app.repository_choices(*manage, query, choices);
            let mut rows = vec![
                bold(
                    if *manage {
                        "Choose your repository whitelist"
                    } else {
                        "Filter whitelisted repositories"
                    },
                    ACCENT,
                ),
                text(format!("Search: {query}"), TEXT),
                text(
                    "↑↓ select · Space/click toggle · Enter save · Esc cancel",
                    DIM,
                ),
            ];
            let mut save = bold("[ Save selection ]", ACCENT);
            save.action = Some(Action::SaveRepositories);
            rows.push(save);
            if !manage {
                let enabled = choices.values().filter(|v| **v).count();
                let mut all = text(
                    format!(
                        "[ All whitelisted · Ctrl+A ]  {enabled}/{} enabled",
                        choices.len()
                    ),
                    TEXT,
                );
                all.action = Some(Action::AllRepositories);
                rows.push(all);
                let mut edit = text("[ Edit whitelist ]", ACCENT);
                edit.action = Some(Action::Repositories(true));
                rows.push(edit);
            }
            if *manage && app.repositories_loading {
                rows.push(text(
                    "Loading your personal and organization repositories…",
                    DIM,
                ));
            }
            if *manage && let Some(error) = &app.repositories_error {
                rows.extend(prose(error, inner.width as usize));
            }
            if options.is_empty() {
                rows.push(text("No matching repositories.", DIM));
            }
            let count = (inner.height as usize).saturating_sub(rows.len());
            let selected = (*selected).min(options.len().saturating_sub(1));
            let start = selected.saturating_sub(count.saturating_sub(1));
            for (index, name) in options.iter().enumerate().skip(start).take(count) {
                let checked = if *manage {
                    choices.contains_key(name)
                } else {
                    choices.get(name).copied().unwrap_or(false)
                };
                let mut row = text(
                    format!(
                        "{} [{}] {}",
                        if index == selected { "▸" } else { " " },
                        if checked { "x" } else { " " },
                        name
                    ),
                    if index == selected { ACCENT } else { TEXT },
                );
                row.action = Some(Action::ToggleRepository(name.clone()));
                rows.push(row);
            }
            rows
        }
        Modal::Help => vec![
            bold("difu · keyboard & mouse", ACCENT),
            text("", DIM),
            text("↑ ↓           Navigate PRs, files, or code", TEXT),
            text("Tab           Switch navigation / content focus", TEXT),
            text("Enter         Open the selected PR", TEXT),
            text("Page Up/Down  Scroll a page · Space scrolls down", TEXT),
            text("Home / End    Jump to start / end", TEXT),
            text("← →           Scroll code horizontally", TEXT),
            text(
                "1 / 2 / 3     Home: Reviews / Authored / Repositories",
                TEXT,
            ),
            text("              Inside PR: Overview / Guide / Diff", TEXT),
            text("F3            Cycle Open / Merged / Closed / All", TEXT),
            text("F4            Filter whitelisted repositories", TEXT),
            text("Shift+F4      Edit the repository whitelist", TEXT),
            text("F2            Choose model and reasoning", TEXT),
            text("F5            Refresh / load the new PR revision", TEXT),
            text("F6            Generate again / retry", TEXT),
            text("F7            Choose another local clone path", TEXT),
            text("F8            Cancel generation or snapshot loading", TEXT),
            text("Ctrl+B        Toggle side-by-side / unified", TEXT),
            text("Ctrl+O        Open PR on GitHub", TEXT),
            text("Mouse         Click items & links; wheel to scroll", TEXT),
            text("Esc           Close dialog / back / quit", TEXT),
            text("Ctrl+C        Quit and clean up running work", TEXT),
        ],
        Modal::Clone { value, key } => {
            let mut rows = vec![bold("Locate your repository", ACCENT), text("", DIM)];
            rows.extend(prose(&format!("Choose the existing local clone for {key}. Difu remembers it for future reviews."),inner.width as usize));
            rows.push(text("", DIM));
            rows.push(bold(
                crop(
                    &format!("> {value}"),
                    value
                        .width()
                        .saturating_sub(inner.width.saturating_sub(4) as usize),
                    inner.width as usize,
                ),
                TEXT,
            ));
            rows.push(text("", DIM));
            rows.extend(prose(
                "Enter to open · Ctrl+U to clear · Esc to cancel",
                inner.width as usize,
            ));
            rows
        }
        Modal::Models {
            selected, query, ..
        } => {
            let options = app.model_options(query);
            let selected = *selected;
            let mut rows = vec![
                bold("Model & reasoning", ACCENT),
                text(format!("Filter: {query}"), DIM),
                text("↑↓ select · Enter apply · Esc close", DIM),
                text("", DIM),
            ];
            if app.models_loading {
                rows.push(text("Loading models from Codex…", DIM));
            }
            if let Some(error) = &app.models_error {
                rows.extend(prose(error, inner.width as usize));
            }
            let count = inner.height.saturating_sub(4) as usize;
            let start = selected.saturating_sub(count.saturating_sub(1));
            for (index, choice) in options.iter().enumerate().skip(start).take(count) {
                let recommended = *choice == ModelChoice::default();
                let label = format!(
                    "{} {}  {}",
                    if index == selected { "▸" } else { " " },
                    choice,
                    if recommended { "★ Recommended" } else { "" }
                );
                let mut row = text(label, if index == selected { ACCENT } else { TEXT });
                row.action = Some(Action::ApplyModel(choice.clone()));
                rows.push(row);
            }
            rows
        }
    };
    for (i, row) in rows.iter().take(inner.height as usize).enumerate() {
        paint(
            frame,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            row,
            app,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_crop_preserves_cell_boundaries() {
        assert_eq!(crop("a界b", 1, 2), "界");
        assert_eq!(crop("a界b", 2, 2), " b");
        assert_eq!(crop("éx", 0, 1), "é");
    }
    #[test]
    fn control_characters_never_reach_terminal() {
        assert!(!crop("hi\x1b[2J\x07", 0, 100).contains('\x1b'));
    }
    #[test]
    fn small_terminal_layout_does_not_panic() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = crate::storage::Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().to_owned(),
        };
        let mut app = App::new(storage, Default::default());
        for (w, h) in [(1, 1), (24, 8), (80, 24), (180, 50)] {
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut terminal = ratatui::Terminal::new(backend)?;
            terminal.draw(|f| draw(f, &mut app))?;
        }
        Ok(())
    }
}
