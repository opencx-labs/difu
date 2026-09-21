//! Terminal Markdown using the same parser as attachment discovery.
use crate::{
    app::Action,
    model::clean,
    ui::{ACCENT, BORDER, DIM, TEXT, TextRow, link, text},
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};
use unicode_width::UnicodeWidthChar;

/// Wrap styled text without losing emphasis across terminal rows.
fn wrap(spans: Vec<Span<'static>>, width: usize) -> Vec<TextRow> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = TextRow::default();
    let mut used = 0;
    let mut word: Vec<(char, Style)> = Vec::new();
    let emit = |word: &mut Vec<(char, Style)>,
                row: &mut TextRow,
                rows: &mut Vec<TextRow>,
                used: &mut usize| {
        let size: usize = word.iter().map(|(c, _)| c.width().unwrap_or(0)).sum();
        if *used > 0 && *used + size > width && size <= width {
            rows.push(std::mem::take(row));
            *used = 0;
        }
        for (ch, style) in word.drain(..) {
            let size = ch.width().unwrap_or(0);
            if ch == '\n' || *used + size > width {
                rows.push(std::mem::take(row));
                *used = 0;
                if ch == '\n' {
                    continue;
                }
            }
            if let Some(last) = row.spans.last_mut().filter(|s| s.style == style) {
                last.content.to_mut().push(ch);
            } else {
                row.spans.push(Span::styled(ch.to_string(), style));
            }
            *used += size;
        }
    };
    for span in spans {
        for ch in span.content.chars() {
            if ch.is_whitespace() {
                emit(&mut word, &mut row, &mut rows, &mut used);
                if ch == '\n' || used < width {
                    word.push((ch, span.style));
                    emit(&mut word, &mut row, &mut rows, &mut used);
                }
            } else {
                word.push((ch, span.style));
            }
        }
    }
    emit(&mut word, &mut row, &mut rows, &mut used);
    if !row.spans.is_empty() {
        rows.push(row);
    }
    rows
}

fn table(cells: Vec<Vec<Vec<Span<'static>>>>, align: &[Alignment], width: usize) -> Vec<TextRow> {
    let count = align.len();
    if count == 0 {
        return Vec::new();
    }
    let available = width.saturating_sub(count * 3 + 1).max(count);
    let mut sizes = (0..count)
        .map(|col| {
            cells
                .iter()
                .filter_map(|r| r.get(col))
                .map(|c| c.iter().map(Span::width).sum::<usize>())
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect::<Vec<_>>();
    while sizes.iter().sum::<usize>() > available {
        let Some((index, value)) = sizes.iter().enumerate().max_by_key(|(_, n)| **n) else {
            break;
        };
        if *value <= 1 {
            break;
        }
        if let Some(value) = sizes.get_mut(index) {
            *value -= 1;
        }
    }
    let rule = |left: &str, mid: &str, right: &str| {
        text(
            format!(
                "{left}{}{right}",
                sizes
                    .iter()
                    .map(|s| "─".repeat(s + 2))
                    .collect::<Vec<_>>()
                    .join(mid)
            ),
            BORDER,
        )
    };
    let mut rows = vec![rule("╭", "┬", "╮")];
    for (index, cells) in cells.into_iter().enumerate() {
        let columns = sizes
            .iter()
            .enumerate()
            .map(|(i, size)| {
                let mut spans = cells.get(i).cloned().unwrap_or_default();
                if index == 0 {
                    for span in &mut spans {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                }
                wrap(spans, *size)
            })
            .collect::<Vec<_>>();
        let height = columns.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for line in 0..height {
            let mut spans = vec![Span::styled("│", Style::default().fg(BORDER))];
            for (col, size) in sizes.iter().enumerate() {
                let content = columns.get(col).and_then(|r| r.get(line));
                let used = content
                    .map(|r| r.spans.iter().map(Span::width).sum::<usize>())
                    .unwrap_or(0);
                let padding = size.saturating_sub(used);
                let left = match align.get(col) {
                    Some(Alignment::Right) => padding,
                    Some(Alignment::Center) => padding / 2,
                    _ => 0,
                };
                spans.push(Span::raw(" ".repeat(left + 1)));
                if let Some(content) = content {
                    spans.extend(content.spans.clone());
                }
                spans.push(Span::raw(" ".repeat(padding - left + 1)));
                spans.push(Span::styled("│", Style::default().fg(BORDER)));
            }
            rows.extend(wrap(spans, width));
        }
        if index == 0 {
            rows.push(rule("├", "┼", "┤"));
        }
    }
    rows.push(rule("╰", "┴", "╯"));
    rows
}

pub(crate) fn rows(source: &str, width: usize) -> Vec<TextRow> {
    let source = clean(source);
    let mut rows = Vec::new();
    let mut spans = Vec::new();
    let mut styles = vec![Style::default().fg(TEXT)];
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut links = Vec::new();
    let mut quote = 0usize;
    let mut code = false;
    let mut alignment = None;
    let mut cells = Vec::new();
    let mut table_row = Vec::new();
    let flush = |spans: &mut Vec<Span<'static>>, rows: &mut Vec<TextRow>| {
        rows.extend(wrap(std::mem::take(spans), width));
    };
    let blank = |rows: &mut Vec<TextRow>| {
        if rows.last().is_some_and(|r| !r.spans.is_empty()) {
            rows.push(TextRow::default());
        }
    };
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(&source, options) {
        let style = styles.last().copied().unwrap_or_default();
        match event {
            Event::Start(Tag::Table(align)) => {
                flush(&mut spans, &mut rows);
                blank(&mut rows);
                alignment = Some(align);
            }
            Event::End(TagEnd::Table) => {
                rows.extend(table(
                    std::mem::take(&mut cells),
                    &alignment.take().unwrap_or_default(),
                    width,
                ));
                blank(&mut rows);
            }
            Event::End(TagEnd::TableCell) => table_row.push(std::mem::take(&mut spans)),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                cells.push(std::mem::take(&mut table_row))
            }
            Event::Start(Tag::Strong) => styles.push(style.add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Emphasis) => styles.push(style.add_modifier(Modifier::ITALIC)),
            Event::Start(Tag::Strikethrough) => {
                styles.push(style.add_modifier(Modifier::CROSSED_OUT))
            }
            Event::Start(Tag::Heading { .. }) => {
                flush(&mut spans, &mut rows);
                blank(&mut rows);
                styles.push(style.add_modifier(Modifier::BOLD));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                if dest_url.starts_with("https://") || dest_url.starts_with("http://") {
                    links.push(dest_url.to_string());
                }
                styles.push(style.fg(ACCENT).add_modifier(Modifier::UNDERLINED));
            }
            Event::End(
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link,
            ) => {
                if styles.len() > 1 {
                    styles.pop();
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if styles.len() > 1 {
                    styles.pop();
                }
                flush(&mut spans, &mut rows);
                blank(&mut rows);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut spans, &mut rows);
                blank(&mut rows);
                code = true;
                let language = match kind {
                    CodeBlockKind::Fenced(value) => value.to_string(),
                    _ => String::new(),
                };
                rows.push(text(format!("┌ {language}"), DIM));
            }
            Event::End(TagEnd::CodeBlock) => {
                flush(&mut spans, &mut rows);
                code = false;
                rows.push(text("└────", DIM));
                blank(&mut rows);
            }
            Event::Start(Tag::List(start)) => {
                flush(&mut spans, &mut rows);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut spans, &mut rows);
                lists.pop();
                if lists.is_empty() {
                    blank(&mut rows);
                }
            }
            Event::Start(Tag::Item) => {
                flush(&mut spans, &mut rows);
                let marker = if let Some(Some(number)) = lists.last_mut() {
                    let marker = format!("{number}. ");
                    *number = number.saturating_add(1);
                    marker
                } else {
                    "• ".into()
                };
                spans.push(Span::styled(
                    format!("{}{marker}", "  ".repeat(lists.len().saturating_sub(1))),
                    style,
                ));
            }
            Event::End(TagEnd::Item) => flush(&mut spans, &mut rows),
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut spans, &mut rows);
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut spans, &mut rows);
                quote = quote.saturating_sub(1);
                blank(&mut rows);
            }
            Event::Start(Tag::Paragraph) if quote > 0 => {
                spans.push(Span::styled("│ ".repeat(quote), Style::default().fg(DIM)))
            }
            Event::End(TagEnd::Paragraph) => {
                flush(&mut spans, &mut rows);
                for url in links.drain(..) {
                    rows.push(link(format!("↗ {url}"), Action::Link(url)));
                }
                if lists.is_empty() {
                    blank(&mut rows);
                }
            }
            Event::Text(value) => spans.push(Span::styled(
                value.to_string(),
                if code { style.fg(DIM) } else { style },
            )),
            Event::Code(value) => spans.push(Span::styled(value.to_string(), style.fg(ACCENT))),
            Event::SoftBreak => spans.push(Span::raw(" ")),
            Event::HardBreak => flush(&mut spans, &mut rows),
            Event::TaskListMarker(done) => spans.push(Span::styled(
                if done { "☑ " } else { "☐ " },
                style.fg(ACCENT),
            )),
            Event::Rule => {
                flush(&mut spans, &mut rows);
                rows.push(text("─".repeat(width.max(1)), BORDER));
            }
            Event::Html(value) | Event::InlineHtml(value) => {
                // HTML comments and markup should not leak into the rendered document.
                let fragment = scraper::Html::parse_fragment(&value);
                let value = fragment.root_element().text().collect::<String>();
                if !value.trim().is_empty() {
                    spans.push(Span::styled(value, style));
                }
            }
            _ => {}
        }
    }
    flush(&mut spans, &mut rows);
    for url in links {
        rows.push(link(format!("↗ {url}"), Action::Link(url)));
    }
    while rows.last().is_some_and(|r| r.spans.is_empty()) {
        rows.pop();
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    fn content(rows: &[TextRow]) -> String {
        rows.iter()
            .map(|r| {
                r.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn renders_github_table_and_preserves_styling_when_wrapped() {
        let output = rows(
            "**Measured on prod**\n\n| | Before | After |\n|---|---:|---|\n| Time | 137 s | 0.37 s cold, 8–9 ms warm |\n| Rows discarded | 481281 | 381 |",
            64,
        );
        let rendered = content(&output);
        assert!(rendered.contains('┼'));
        assert!(!rendered.contains("|---"));
        assert!(rendered.contains("137 s"));
        assert!(
            output
                .iter()
                .all(|r| r.spans.iter().map(Span::width).sum::<usize>() <= 64)
        );
        assert!(output.iter().flat_map(|r| &r.spans).any(|s| {
            s.style.add_modifier.contains(Modifier::BOLD) && s.content.contains("Measured")
        }));
    }
    #[test]
    fn links_lists_code_and_html_comments_are_rendered() {
        let output = rows(
            "# Heading\n\n<!-- hidden -->\n\n- [x] Done with **bold** and `code`\n\n[Docs](https://example.com)\n\n```rs\n  let x = 1;\n```\n\n<details><summary>Summary</summary></details>",
            80,
        );
        let rendered = content(&output);
        assert!(rendered.contains("☑ Done"));
        assert!(rendered.contains("  let x = 1;"));
        assert!(rendered.contains("Summary"));
        assert!(!rendered.contains("hidden"));
        assert!(!rendered.contains("<details>"));
        assert!(
            output.iter().any(
                |r| matches!(&r.action, Some(Action::Link(url)) if url == "https://example.com")
            )
        );
    }
}
