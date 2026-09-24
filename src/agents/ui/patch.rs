//! Render Codex file-change patches without flattening their source coordinates.
use super::*;
use crate::ui::{ADD_BG, REMOVE_BG};

fn starts(header: &str) -> Option<(u64, u64)> {
    let mut parts = header.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old = parts
        .next()?
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new = parts
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    (parts.next()? == "@@").then_some((old, new))
}

pub(super) struct SourceRow {
    pub line: Line<'static>,
    pub source: Option<(usize, String)>,
}
pub(super) fn render(diff: &str, width: u16) -> (Vec<Line<'static>>, usize, usize) {
    let (rows, added, removed) = render_full(diff, width);
    (
        rows.into_iter().map(|row| row.line).collect(),
        added,
        removed,
    )
}
pub(super) fn render_full(diff: &str, width: u16) -> (Vec<SourceRow>, usize, usize) {
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    let mut added = 0;
    let mut removed = 0;
    let mut numbers = None;
    for (source_index, source) in diff.lines().enumerate() {
        let before = rows.len();
        if let Some((old, new)) = starts(source) {
            if numbers.is_some() {
                rows.push(Line::from(Span::styled("    ⋮", Style::default().fg(DIM))));
            }
            numbers = Some((old, new));
            sources.resize_with(rows.len(), || None);
            continue;
        }
        if source.starts_with("@@") {
            numbers = None;
        }
        let parsed = numbers.as_mut().and_then(|(old, new)| {
            let (content, number, marker, background) = match source.chars().next()? {
                '+' => {
                    let number = *new;
                    *new = new.saturating_add(1);
                    added += 1;
                    (source.strip_prefix('+')?, number, '+', ADD_BG)
                }
                '-' => {
                    let number = *old;
                    *old = old.saturating_add(1);
                    removed += 1;
                    (source.strip_prefix('-')?, number, '-', REMOVE_BG)
                }
                ' ' => {
                    let number = *new;
                    *old = old.saturating_add(1);
                    *new = new.saturating_add(1);
                    (source.strip_prefix(' ')?, number, ' ', BG)
                }
                _ => return None,
            };
            Some((content, number, marker, background))
        });
        if let Some((content, number, marker, background)) = parsed {
            let prefix = format!(" {number:>5} {marker} ");
            let prefix = crate::ui::crop(&prefix, 0, usize::from(width).saturating_sub(1));
            let gutter = unicode_width::UnicodeWidthStr::width(prefix.as_str());
            let content = crate::model::clean(content).replace('\t', "    ");
            let wrapped = crate::markdown::wrap(
                crate::ui::syntax_spans(&content),
                usize::from(width).saturating_sub(gutter).max(1),
            );
            let wrapped = if wrapped.is_empty() {
                vec![crate::ui::TextRow::default()]
            } else {
                wrapped
            };
            for (index, row) in wrapped.into_iter().enumerate() {
                let mut spans = vec![Span::styled(
                    if index == 0 {
                        prefix.clone()
                    } else {
                        " ".repeat(gutter)
                    },
                    Style::default()
                        .fg(match marker {
                            '+' => GREEN,
                            '-' => RED,
                            _ => DIM,
                        })
                        .bg(background),
                )];
                spans.extend(row.spans.into_iter().map(|mut span| {
                    span.style = span.style.bg(background);
                    span
                }));
                let used = spans.iter().map(Span::width).sum::<usize>();
                spans.push(Span::styled(
                    " ".repeat(usize::from(width).saturating_sub(used)),
                    Style::default().bg(background),
                ));
                rows.push(Line::from(spans).style(Style::default().bg(background)));
            }
        } else if numbers.is_none()
            && (source.starts_with("diff --git ")
                || source.starts_with("index ")
                || source.starts_with("--- ")
                || source.starts_with("+++ "))
        {
            // The enclosing tool heading already identifies this file.
        } else {
            rows.extend(
                wrapped(source, width)
                    .into_iter()
                    .map(|text| Line::from(Span::styled(text, Style::default().fg(DIM)))),
            );
        }
        sources.extend(
            (before..rows.len())
                .map(|_| parsed.map(|(text, _, _, _)| (source_index, text.to_owned()))),
        );
    }
    (
        rows.into_iter()
            .zip(sources)
            .map(|(line, source)| SourceRow { line, source })
            .collect(),
        added,
        removed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hunk_coordinates_and_wrapped_change_backgrounds_are_preserved() {
        let diff = "--- a/code.rs\n+++ b/code.rs\n@@ -10,2 +20,2 @@\n context\n-old\n+let greeting = \"hello 世界, a long string\";\n@@ -40 +50 @@\n-before\n+after\n\\ No newline at end of file\n";
        let (rows, added, removed) = render(diff, 32);
        assert_eq!((added, removed), (2, 2));
        let text = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "20   context",
            "11 - old",
            "21 + let",
            "40 - before",
            "50 + after",
            "⋮",
            "No newline",
        ] {
            assert!(text.contains(expected), "Missing {expected}: {text}");
        }
        assert!(!text.contains("--- a/code.rs"));
        let additions = rows
            .iter()
            .filter(|row| row.style.bg == Some(ADD_BG))
            .collect::<Vec<_>>();
        assert!(additions.len() > 2);
        for row in additions {
            assert_eq!(row.width(), 32);
            assert!(row.spans.iter().all(|span| span.style.bg == Some(ADD_BG)));
        }
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.content == "let" && span.style.fg == Some(ACCENT))
        );
        assert!(
            rows.iter()
                .filter(|row| row.style.bg == Some(REMOVE_BG))
                .all(|row| row.width() == 32)
        );
    }
    #[test]
    fn added_deleted_empty_and_metadata_only_patches_are_readable() {
        let (added, plus, minus) = render("@@ -0,0 +1,2 @@\n+first\n+\n", 24);
        assert_eq!((plus, minus), (2, 0));
        assert_eq!(added.len(), 2);
        assert!(
            added
                .last()
                .is_some_and(|row| row.to_string().contains("2 +"))
        );
        let (deleted, plus, minus) = render("@@ -1 +0,0 @@\n-gone", 24);
        assert_eq!((plus, minus), (0, 1));
        assert!(
            deleted
                .first()
                .is_some_and(|row| row.to_string().contains("1 - gone"))
        );
        let (metadata, plus, minus) = render("Binary files differ", 24);
        assert_eq!((plus, minus), (0, 0));
        assert_eq!(
            metadata.first().map(ToString::to_string).as_deref(),
            Some("Binary files differ")
        );
        assert!(render("", 24).0.is_empty());
        for width in 0..10 {
            let _ = render("@@ -1 +1 @@\n-世界\n+new", width);
        }
    }
}
