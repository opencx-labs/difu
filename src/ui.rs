use crate::{
    app::{Action, App, Focus, Modal, Review, View},
    context::Direction,
    diff::{DiffFile, DiffLine, Hunk, LineKind, split_rows},
    model::{InboxTab, ModelChoice, PrState, PrSummary, clean},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const BG: Color = Color::Rgb(12, 14, 18);
pub(crate) const PANEL: Color = Color::Rgb(18, 21, 27);
pub(crate) const TEXT: Color = Color::Rgb(220, 225, 232);
pub(crate) const DIM: Color = Color::Rgb(130, 140, 156);
pub(crate) const BORDER: Color = Color::Rgb(42, 48, 61);
pub(crate) const ACCENT: Color = Color::Rgb(183, 161, 255);
pub(crate) const GREEN: Color = Color::Rgb(114, 216, 163);
pub(crate) const RED: Color = Color::Rgb(247, 137, 145);
const ADD_BG: Color = Color::Rgb(18, 43, 32);
const REMOVE_BG: Color = Color::Rgb(49, 25, 31);

#[derive(Clone, Default)]
pub struct TextRow {
    pub spans: Vec<Span<'static>>,
    pub action: Option<Action>,
    pub target: Option<crate::workflow::Target>,
    pub code_links: Vec<CodeLink>,
}
#[derive(Clone)]
pub struct CodeLink {
    pub column: usize,
    pub width: usize,
    pub action: Action,
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
pub struct FileSection {
    pub start: usize,
    pub end: usize,
    pub header: Vec<TextRow>,
}
pub struct NavItem {
    pub chapter: usize,
    pub path: String,
    pub row: usize,
}
pub struct Document {
    pub navigation: Vec<NavItem>,
    pub epoch: u64,
    pub width: u16,
    pub horizontal: usize,
    pub rows: Vec<Row>,
    pub sections: Vec<Section>,
    pub files: Vec<FileSection>,
    pub guide_columns: bool,
    pub left_width: u16,
}
impl Document {
    pub fn max_scroll(&self, height: usize) -> usize {
        self.rows
            .len()
            .saturating_sub(height)
            .max(self.sections.last().map(|s| s.start).unwrap_or(0))
    }
}

fn wrapped_text(value: &str, width: usize) -> Vec<String> {
    textwrap::wrap(&clean(value), width.max(1))
        .into_iter()
        .map(|line| line.into_owned())
        .collect()
}

fn file_header(file: &DiffFile, width: usize) -> Vec<TextRow> {
    wrapped_text(
        &format!("{}   +{} −{}", file.path, file.additions, file.deletions),
        width,
    )
    .into_iter()
    .map(|line| {
        let mut row = bold(line, TEXT);
        row.target = Some(crate::workflow::Target::Header {
            path: file.path.clone(),
            chapter: None,
        });
        row
    })
    .collect()
}

fn span(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(color))
}
pub(crate) fn text(value: impl Into<String>, color: Color) -> TextRow {
    TextRow {
        spans: vec![span(value, color)],
        action: None,
        target: None,
        code_links: Vec::new(),
    }
}
pub(crate) fn bold(value: impl Into<String>, color: Color) -> TextRow {
    TextRow {
        spans: vec![Span::styled(
            value.into(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )],
        action: None,
        target: None,
        code_links: Vec::new(),
    }
}
pub(crate) fn link(value: impl Into<String>, action: Action) -> TextRow {
    TextRow {
        spans: vec![Span::styled(
            value.into(),
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::UNDERLINED),
        )],
        action: Some(action),
        target: None,
        code_links: Vec::new(),
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
pub(crate) fn prose(source: &str, width: usize) -> Vec<TextRow> {
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
                    target: None,
                    code_links: Vec::new(),
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
pub(crate) fn crop(value: &str, offset: usize, width: usize) -> String {
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

fn code_links(
    path: &str,
    line: Option<&DiffLine>,
    old: bool,
    width: usize,
    horizontal: usize,
    offset: usize,
) -> Vec<CodeLink> {
    if !crate::navigation::supported(path) {
        return Vec::new();
    }
    let Some(line) = line else {
        return Vec::new();
    };
    let Some(number) = (if old { line.old } else { line.new }) else {
        return Vec::new();
    };
    let prefix = format!("{number:>5}   ").width();
    let available = width.saturating_sub(prefix);
    let mut links = Vec::new();
    let mut chars = line.text.char_indices().peekable();
    let mut cells: usize = 0;
    let mut quote = None;
    let mut escaped = false;
    while let Some((byte, ch)) = chars.next() {
        let cell_width = |c: char| {
            if c == '\t' {
                4
            } else if c.is_control() {
                0
            } else {
                c.width().unwrap_or(0)
            }
        };
        if let Some(end) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == end {
                quote = None;
            }
            cells += cell_width(ch);
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            cells += cell_width(ch);
            continue;
        }
        if ch == '/'
            && chars
                .peek()
                .is_some_and(|(_, next)| matches!(next, '/' | '*'))
        {
            break;
        }
        if ch.is_alphabetic() || matches!(ch, '_' | '$') {
            let start = cells;
            cells += cell_width(ch);
            let mut end = byte + ch.len_utf8();
            while chars.peek().is_some_and(|(_, c)| {
                c.is_alphanumeric() || matches!(c, '_' | '$') || c.width() == Some(0)
            }) {
                if let Some((index, next)) = chars.next() {
                    cells += cell_width(next);
                    end = index + next.len_utf8();
                }
            }
            let name = line.text.get(byte..end).unwrap_or_default();
            if [
                "function",
                "const",
                "let",
                "var",
                "return",
                "import",
                "export",
                "from",
                "as",
                "async",
                "await",
                "if",
                "else",
                "for",
                "while",
                "switch",
                "catch",
                "new",
                "class",
                "type",
                "interface",
                "true",
                "false",
                "null",
                "throw",
            ]
            .contains(&name)
            {
                continue;
            }
            let visible_start = start.max(horizontal);
            let visible_end = cells.min(horizontal.saturating_add(available));
            if visible_end > visible_start {
                links.push(CodeLink {
                    column: offset + prefix + visible_start.saturating_sub(horizontal),
                    width: visible_end - visible_start,
                    action: Action::Definition {
                        path: path.into(),
                        line: number,
                        column: byte,
                        old,
                    },
                });
            }
        } else {
            cells += cell_width(ch);
        }
    }
    links
}

fn code_rows(
    path: &str,
    lines: &[DiffLine],
    width: usize,
    split: bool,
    horizontal: usize,
) -> Vec<TextRow> {
    let mut rows = Vec::new();
    if split {
        let left = width.saturating_sub(1) / 2;
        let right = width.saturating_sub(left + 1);
        let hunk = Hunk {
            id: String::new(),
            header: String::new(),
            lines: lines.to_vec(),
        };
        for (old, new) in split_rows(&hunk) {
            let mut spans = code(old, true, left, horizontal);
            spans.push(span("│", BORDER));
            spans.extend(code(new, false, right, horizontal));
            let mut links = code_links(path, old, true, left, horizontal, 0);
            links.extend(code_links(path, new, false, right, horizontal, left + 1));
            rows.push(TextRow {
                spans,
                code_links: links,
                action: None,
                target: Some(crate::workflow::Target::Code {
                    path: path.into(),
                    old: old.and_then(|l| l.old),
                    new: new.and_then(|l| l.new),
                }),
            });
        }
    } else {
        for line in lines {
            rows.push(TextRow {
                spans: code(Some(line), line.kind == LineKind::Remove, width, horizontal),
                code_links: code_links(
                    path,
                    Some(line),
                    line.kind == LineKind::Remove,
                    width,
                    horizontal,
                    0,
                ),
                action: None,
                target: Some(crate::workflow::Target::Code {
                    path: path.into(),
                    old: line.old,
                    new: line.new,
                }),
            });
        }
    }
    rows
}

fn expansion_button(
    review: &Review,
    file: &DiffFile,
    hunk: &Hunk,
    direction: Direction,
) -> Option<TextRow> {
    if review.root.is_none() || !hunk.header.starts_with("@@ ") {
        return None;
    }
    let state = review.context.get(&file.path);
    let expanded = review.expanded.get(&hunk.id).copied().unwrap_or_default();
    if let Some(data) = state.and_then(|s| s.data.as_ref()) {
        if !data.can_expand(&hunk.id, expanded, direction) {
            return None;
        }
    } else if direction == Direction::Above
        && !hunk
            .lines
            .iter()
            .find(|line| line.old.is_some() || line.new.is_some())
            .is_some_and(|line| line.old.is_some_and(|n| n > 1) && line.new.is_some_and(|n| n > 1))
    {
        return None;
    }
    if state.is_some_and(|s| {
        s.pending
            .iter()
            .any(|(id, d)| id == &hunk.id && *d == direction)
    }) {
        return Some(text("  Loading context…", DIM));
    }
    Some(link(
        match direction {
            Direction::Above => "  [ ↑ 10 lines above ]",
            Direction::Below => "  [ ↓ 10 lines below ]",
        },
        Action::ExpandHunk(hunk.id.clone(), direction),
    ))
}

fn hunk_rows(
    file: &DiffFile,
    hunk: &Hunk,
    width: usize,
    split: bool,
    horizontal: usize,
    with_title: bool,
    review: &Review,
) -> Vec<TextRow> {
    let mut rows = Vec::new();
    if with_title {
        rows.extend(file_header(file, width));
    }
    rows.push(text(format!(" {}", hunk.header), DIM));
    if let Some(button) = expansion_button(review, file, hunk, Direction::Above) {
        rows.push(button);
    }
    let state = review.context.get(&file.path);
    if let Some(error) = state.and_then(|s| s.error.as_ref()) {
        rows.extend(prose(
            &format!("Could not load context: {error}. Click an expansion button to retry."),
            width,
        ));
    }
    let data = state.and_then(|s| s.data.as_ref());
    let expanded = review.expanded.get(&hunk.id).copied().unwrap_or_default();
    if let Some(data) = data.filter(|_| hunk.header.starts_with("@@ ")) {
        let expanded_view = expanded.above > 0 || expanded.below > 0;
        for part in data.parts(&hunk.id, expanded) {
            if expanded_view {
                match part.owner {
                    Some(id) if id != hunk.id => {
                        rows.push(text("── Neighboring hunk ──", DIM));
                        if let Some(guide) = &review.guide {
                            for (index, chapter) in guide
                                .chapters
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| c.hunks.iter().any(|h| h == id))
                            {
                                for line in wrapped_text(
                                    &format!(
                                        "Explained in Chapter {}: {} →",
                                        index + 1,
                                        chapter.title
                                    ),
                                    width,
                                ) {
                                    rows.push(link(line, Action::GoToChapter(index)));
                                }
                            }
                        }
                    }
                    Some(_) => rows.push(text("── This hunk ──", DIM)),
                    None => rows.push(text("── Context ──", DIM)),
                }
            }
            rows.extend(code_rows(&file.path, part.lines, width, split, horizontal));
        }
    } else {
        rows.extend(code_rows(&file.path, &hunk.lines, width, split, horizontal));
    }
    if let Some(button) = expansion_button(review, file, hunk, Direction::Below) {
        rows.push(button);
    }
    rows.push(TextRow::default());
    rows
}

fn build(app: &App, width: u16) -> Document {
    let mut doc = Document {
        navigation: Vec::new(),
        epoch: app.epoch,
        width,
        horizontal: app.horizontal,
        rows: Vec::new(),
        sections: Vec::new(),
        files: Vec::new(),
        guide_columns: false,
        left_width: 0,
    };
    let Some(review) = app.review() else {
        return doc;
    };
    if app.view == View::Overview {
        for row in crate::overview::rows(review, width) {
            append(&mut doc.rows, row);
        }
        return doc;
    }
    let Some(snapshot) = review.snapshot.as_ref() else {
        append(
            &mut doc.rows,
            text(
                if review.preparing {
                    "Preparing PR revisions and the diff…"
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
        let code_width = code_width.saturating_sub(2);
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
            let chapter_files = chapter
                .hunks
                .iter()
                .filter_map(|id| snapshot.find(id).map(|(f, _)| f.path.clone()))
                .collect::<std::collections::BTreeSet<_>>();
            let completed = chapter_files
                .iter()
                .filter(|path| {
                    review
                        .interaction
                        .progress
                        .completed
                        .contains(&(chapter_index, (*path).clone()))
                })
                .count();
            left.push(text(
                format!("{completed}/{} sections completed", chapter_files.len()),
                DIM,
            ));
            let mut right = Vec::new();
            let mut last_file = String::new();
            let mut seen = std::collections::HashSet::new();
            let prose_length = left.len();
            // Compact chapters place explanation before all code. File links
            // below it point to actual document rows, calculated after layout.
            let mut links = Vec::new();
            let mut headers = Vec::new();
            for id in &chapter.hunks {
                if let Some((file, hunk)) = snapshot.find(id) {
                    let title = last_file != file.path;
                    last_file = file.path.clone();
                    if seen.insert(file.path.clone()) {
                        links.push((file.path.clone(), right.len()));
                    }
                    if title {
                        headers.push((right.len(), file_header(file, code_width)));
                    }
                    let collapsed = review
                        .interaction
                        .progress
                        .completed
                        .contains(&(chapter_index, file.path.clone()));
                    if collapsed {
                        if title {
                            right.extend(file_header(file, code_width));
                            right
                                .push(text("✓ Chapter section completed · Enter to reopen", GREEN));
                        }
                        continue;
                    }
                    right.extend(hunk_rows(
                        file,
                        hunk,
                        code_width,
                        split,
                        app.horizontal,
                        title,
                        review,
                    ));
                }
            }
            for row in &mut right {
                if let Some(crate::workflow::Target::Header { chapter, .. }) = &mut row.target {
                    *chapter = Some(chapter_index);
                }
            }
            for (_, header) in &mut headers {
                for row in header {
                    if let Some(crate::workflow::Target::Header { chapter, .. }) = &mut row.target {
                        *chapter = Some(chapter_index);
                    }
                }
            }
            let links = links
                .into_iter()
                .map(|(path, row)| {
                    (
                        path.clone(),
                        wrapped_text(
                            &format!(
                                "{} {}",
                                if review
                                    .interaction
                                    .progress
                                    .completed
                                    .contains(&(chapter_index, path.clone()))
                                {
                                    "✓"
                                } else {
                                    "↳"
                                },
                                path
                            ),
                            prose_width,
                        ),
                        row,
                    )
                })
                .collect::<Vec<_>>();
            let link_rows: usize = links.iter().map(|(_, lines, _)| lines.len()).sum();
            let offset = if wide {
                start
            } else {
                start + prose_length + link_rows + 1
            };
            for (path, lines, row) in links {
                let index = doc.navigation.len();
                doc.navigation.push(NavItem {
                    chapter: chapter_index,
                    path,
                    row: offset + row,
                });
                for line in lines {
                    left.push(link(
                        line,
                        Action::Workflow(crate::workflow::WAction::Nav(index)),
                    ));
                }
            }
            for (index, (row, header)) in headers.iter().enumerate() {
                doc.files.push(FileSection {
                    start: offset + row,
                    end: offset
                        + headers
                            .get(index + 1)
                            .map(|(start, _)| *start)
                            .unwrap_or(right.len()),
                    header: header.clone(),
                });
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
            let collapsed = review.interaction.github.viewed.contains(&file.path)
                && review.interaction.github.head == snapshot.head;
            if collapsed {
                for row in file_header(file, width.saturating_sub(2) as usize) {
                    append(&mut doc.rows, row);
                }
                append(
                    &mut doc.rows,
                    text("✓ Viewed on GitHub · Enter to reopen", GREEN),
                );
            }
            for (i, hunk) in file.hunks.iter().enumerate().filter(|_| !collapsed) {
                for row in hunk_rows(
                    file,
                    hunk,
                    width.saturating_sub(2) as usize,
                    width >= 80 && !app.config.unified,
                    app.horizontal,
                    i == 0,
                    review,
                ) {
                    append(&mut doc.rows, row);
                }
            }
            doc.files.push(FileSection {
                start: 0,
                end: doc.rows.len(),
                header: file_header(file, width.saturating_sub(2) as usize),
            });
        }
    }
    doc
}

pub(crate) fn paint(frame: &mut Frame, rect: Rect, row: &TextRow, app: &mut App) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let mut spans = row.spans.clone();
    if matches!(&row.action,Some(Action::Workflow(crate::workflow::WAction::Nav(i))) if *i==app.workflow.nav)
    {
        for span in &mut spans {
            span.style = span.style.bg(PANEL).add_modifier(Modifier::BOLD);
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    if let Some(action) = &row.action {
        app.hits.push((rect, action.clone()));
    }
}
fn paint_diff(
    frame: &mut Frame,
    rect: Rect,
    row: &TextRow,
    index: usize,
    doc: &Document,
    app: &mut App,
) {
    if app.view == View::Overview {
        paint(frame, rect, row, app);
        return;
    }
    let code = Rect::new(rect.x + 2, rect.y, rect.width.saturating_sub(2), 1);
    paint(frame, code, row, app);
    let split =
        !app.config.unified && (doc.guide_columns || (app.view == View::Diff && doc.width >= 80));
    if row.target.is_some() {
        let left = if split { code.width / 2 } else { code.width };
        let side = if !split
            && matches!(
                &row.target,
                Some(crate::workflow::Target::Code { new: None, .. })
            ) {
            crate::review::Side::Left
        } else {
            crate::review::Side::Right
        };
        app.hits.push((
            Rect::new(code.x, code.y, left, 1),
            Action::Workflow(crate::workflow::WAction::Cursor(
                index,
                if split {
                    crate::review::Side::Left
                } else {
                    side
                },
            )),
        ));
        if split {
            app.hits.push((
                Rect::new(code.x + left, code.y, code.width.saturating_sub(left), 1),
                Action::Workflow(crate::workflow::WAction::Cursor(
                    index,
                    crate::review::Side::Right,
                )),
            ));
        }
    }
    // Symbol regions take precedence over the row's selection regions. The
    // resolver checks the exact AST binding before presenting any definition.
    for link in &row.code_links {
        if let (Ok(column), Ok(width)) = (u16::try_from(link.column), u16::try_from(link.width))
            && column < code.width
        {
            app.hits.push((
                Rect::new(code.x + column, code.y, width.min(code.width - column), 1),
                link.action.clone(),
            ));
        }
    }
    let cursor = app.workflow.cursor.unwrap_or(app.scroll);
    let focused = app.focus == Focus::Content && cursor == index;
    let selected = app.workflow.selection.as_ref().is_some_and(|start| {
        let end = doc
            .rows
            .get(cursor)
            .and_then(|r| r.right.target.as_ref())
            .and_then(|t| t.line(app.workflow.side));
        let line = row.target.as_ref().and_then(|t| t.line(app.workflow.side));
        end.zip(line).is_some_and(|(end, line)| {
            start.path == line.path
                && end.path == line.path
                && (start.start.min(end.end)..=start.start.max(end.end)).contains(&line.start)
        })
    });
    if focused || selected {
        let offset = if split && app.workflow.side == crate::review::Side::Right {
            code.width / 2 + 1
        } else {
            0
        };
        let width = if split { code.width / 2 } else { code.width };
        for x in code.x + offset..(code.x + offset + width).min(code.right()) {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, code.y)) {
                cell.set_bg(if selected {
                    Color::Rgb(55, 48, 78)
                } else {
                    PANEL
                });
            }
        }
    }
    if focused {
        frame.render_widget(
            Paragraph::new(">").style(Style::default().fg(ACCENT)),
            Rect::new(rect.x, rect.y, 1, 1),
        );
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
    let outer_main = main;
    let main = Rect::new(
        main.x + 1,
        main.y + 1,
        main.width.saturating_sub(2),
        main.height.saturating_sub(2),
    );
    if !has_guide || main.width < 132 {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(if app.view == View::Overview {
                    " PR preview "
                } else {
                    " Diff "
                })
                .border_style(Style::default().fg(if app.focus == Focus::Content {
                    ACCENT
                } else {
                    BORDER
                })),
            outer_main,
        );
    }
    // Center the overview column inside the available pane, including the home preview.
    app.hits.push((outer_main, Action::Focus(Focus::Content)));
    let main = if app.view == View::Overview {
        crate::overview::column(main)
    } else {
        main
    };
    app.content_rect = main;
    app.viewport = main.height as usize;
    app.hits.push((main, Action::Focus(Focus::Content)));
    if navigation {
        let nav = Rect::new(content.x, content.y, nav_width, content.height);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if app.focus == Focus::Navigation {
                    ACCENT
                } else {
                    BORDER
                })),
            Rect::new(nav.x, nav.y, nav.width, nav.height),
        );
        app.hits.push((nav, Action::Focus(Focus::Navigation)));
        let nav = Rect::new(
            nav.x + 1,
            nav.y + 1,
            nav.width.saturating_sub(2),
            nav.height.saturating_sub(2),
        );
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
    if doc.guide_columns {
        for (rect, label, focus) in [
            (
                Rect::new(main.x - 1, main.y - 1, doc.left_width + 2, main.height + 2),
                " Chapter ",
                Focus::Navigation,
            ),
            (
                Rect::new(
                    main.x + doc.left_width + 2,
                    main.y - 1,
                    main.width.saturating_sub(doc.left_width + 1),
                    main.height + 2,
                ),
                " Diff ",
                Focus::Content,
            ),
        ] {
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .title(label)
                    .border_style(Style::default().fg(if app.focus == focus {
                        ACCENT
                    } else {
                        BORDER
                    })),
                rect,
            );
        }
        app.hits.push((
            Rect::new(main.x, main.y, doc.left_width, main.height),
            Action::Focus(Focus::Navigation),
        ));
    }
    if let Some(index) = app.chapter_target.take()
        && let Some(section) = doc.sections.get(index)
    {
        app.scroll = section.start;
        app.workflow.cursor = Some(section.start);
        app.workflow.selection = None;
        app.workflow.nav = doc
            .navigation
            .iter()
            .position(|item| item.chapter == index)
            .unwrap_or(0);
    }
    app.scroll = app.scroll.min(doc.max_scroll(main.height as usize));
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
            paint_diff(
                frame,
                Rect::new(
                    main.x + doc.left_width + 3,
                    main.y + y,
                    main.width.saturating_sub(doc.left_width + 3),
                    1,
                ),
                &row.right,
                index,
                &doc,
                app,
            );
        } else {
            paint_diff(
                frame,
                Rect::new(main.x, main.y + y, main.width, 1),
                &row.right,
                index,
                &doc,
                app,
            );
        }
    }
    if let Some(file) = doc
        .files
        .iter()
        .find(|file| app.scroll > file.start && app.scroll < file.end)
        && file.header.len() < main.height as usize
    {
        let offset = if doc.guide_columns {
            doc.left_width + 3
        } else {
            0
        };
        // Stop at the next file's natural header instead of covering it.
        let count = file.header.len().min(file.end.saturating_sub(app.scroll));
        for (index, row) in file.header.iter().take(count).enumerate() {
            let rect = Rect::new(
                main.x + offset,
                main.y + index as u16,
                main.width.saturating_sub(offset),
                1,
            );
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Block::default().style(Style::default().bg(BG).fg(TEXT)),
                rect,
            );
            paint_diff(frame, rect, row, file.start + index, &doc, app);
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
    let mut status = if app.home && app.inbox_loading && !app.inbox.is_empty() {
        "Showing cached PRs · Refreshing…".into()
    } else if app.home && app.inbox_error.is_some() && !app.inbox.is_empty() {
        "Showing cached PRs · Refresh failed · F5 retry".into()
    } else if let Some(review) = app.review() {
        if let Some(job) = &review.generation {
            format!(
                "◌ Generating guide · {}s · {}",
                job.started.elapsed().as_secs(),
                job.activity
            )
        } else if review.preparing {
            let step = review
                .preparation_progress
                .as_ref()
                .map_or(1, |p| p.step)
                .clamp(1, 5);
            let completed = usize::from(step.saturating_sub(1)) * 2;
            let bar = format!("{}{}", "■".repeat(completed), "□".repeat(10 - completed));
            let activity = review
                .preparation_progress
                .as_ref()
                .map_or("Checking the local clone", |p| p.activity.as_str());
            let elapsed = review
                .preparation_started
                .map_or(0, |t| t.elapsed().as_secs());
            format!("[{bar}] {step}/5 · {elapsed}s · {}", clean(activity))
        } else if let Some(error) = &review.guide_error {
            format!(
                "{}: {} · F6 retry",
                if review.preparation_failed {
                    "Snapshot"
                } else {
                    "Guide"
                },
                clean(error).replace('\n', " ")
            )
        } else if review.newer.is_some() {
            "● Remote PR updated · F5 to sync and refresh".into()
        } else if let Some(model) = &review.guide_model {
            format!("Guide ready · {model}")
        } else {
            format!("{}", app.config.model)
        }
    } else if app.inbox_loading {
        format!("Loading {}…", app.inbox_tab.label().to_lowercase())
    } else {
        String::new()
    };
    if !app.home
        && let Some(review) = app.review()
    {
        if app.view == View::Guide
            && let (Some(guide), Some(snapshot)) = (&review.guide, &review.snapshot)
        {
            let total: usize = guide
                .chapters
                .iter()
                .map(|c| {
                    c.hunks
                        .iter()
                        .filter_map(|id| snapshot.find(id).map(|(f, _)| f.path.as_str()))
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                })
                .sum();
            status = format!(
                "{} · {}/{} chapter sections complete",
                status,
                review.interaction.progress.completed.len(),
                total
            );
        } else if app.view == View::Diff
            && let Some(snapshot) = &review.snapshot
        {
            status = format!(
                "{} · {}/{} files Viewed on GitHub",
                status,
                review.interaction.github.viewed.len(),
                snapshot.files.len()
            );
        }
    }
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
            (
                "/ Actions",
                Action::Workflow(crate::workflow::WAction::Open),
            ),
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
            ("Cmd+↑/↓ 10 lines", Action::FastScroll(10)),
        ]
    } else {
        vec![
            (
                "/ Actions",
                Action::Workflow(crate::workflow::WAction::Open),
            ),
            ("Alt+↑/↓ Chapters", Action::Chapter(true)),
            ("Cmd+↑/↓ 10 lines", Action::FastScroll(10)),
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
        if label == "Alt+↑/↓ Chapters" && app.view != View::Guide {
            continue;
        }
        if app.home
            && ((label == "F3 State" && app.inbox_tab == InboxTab::ReviewRequests)
                || (label == "F4 Repos" && app.inbox_tab != InboxTab::Repositories))
        {
            continue;
        }
        let width = label.width() as u16;
        if footer_x + width > area.width.saturating_sub(2) {
            break;
        }
        let rect = Rect::new(footer_x, area.height - 2, width, 1);
        frame.render_widget(Paragraph::new(label).style(Style::default().fg(DIM)), rect);
        app.hits.push((rect, action));
        footer_x += width + 2;
    }
    let focused = match (app.focus, app.home, app.view, has_guide) {
        (Focus::Navigation, true, _, _) => "PR list",
        (Focus::Navigation, false, View::Guide, true) => "Chapters",
        (Focus::Navigation, false, _, _) => "Files",
        (_, _, View::Overview, _) => "Overview",
        _ => "Diff",
    };
    let mut focus_line = vec![span(format!("Focus: {focused} · Tab to switch"), ACCENT)];
    let notice_in_modal = matches!(
        &app.modal,
        Some(Modal::Workflow(wizard))
            if matches!(wizard.as_ref(), crate::workflow::Wizard::Result { notice }
                if notice.message == app.notice.message && notice.kind == app.notice.kind)
    );
    if !app.notice.message.is_empty() && !notice_in_modal {
        focus_line.push(span(
            format!(" · {}", clean(&app.notice.message).replace('\n', " ")),
            notice_color(app.notice.kind),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(focus_line)),
        Rect::new(2, area.height - 1, area.width - 4, 1),
    );
    if app.modal.is_some() {
        draw_modal(frame, app);
    }
}

pub(crate) fn notice_color(kind: crate::app::NoticeKind) -> Color {
    match kind {
        crate::app::NoticeKind::Info => DIM,
        crate::app::NoticeKind::Success => GREEN,
        crate::app::NoticeKind::Error => RED,
    }
}

fn inbox_rows(pr: &PrSummary, width: usize, selected: bool) -> Vec<TextRow> {
    let width = width.max(1);
    let mut rows = vec![
        text(
            crop(
                &format!(
                    "{} #{}  {}",
                    if selected { "▸" } else { " " },
                    pr.key.number,
                    pr.key.repo
                ),
                0,
                width,
            ),
            if selected { ACCENT } else { DIM },
        ),
        text(crop(&format!(" {}", pr.title), 0, width), TEXT),
        text(
            crop(
                &format!(" {}{}", pr.author, if pr.draft { " · draft" } else { "" }),
                0,
                width,
            ),
            DIM,
        ),
    ];
    let date = chrono::DateTime::parse_from_rfc3339(&pr.created)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| "unknown".into());
    rows.extend(
        wrapped_text(&format!("Opened {date}"), width)
            .into_iter()
            .map(|s| text(s, DIM)),
    );
    if let Some(stats) = &pr.stats {
        // Wrap colored tokens without losing digits in a narrow navigation pane.
        let mut line = TextRow::default();
        let mut used = 0;
        for (token, color) in [
            (
                format!(
                    "{} {}",
                    stats.changed_files,
                    if stats.changed_files == 1 {
                        "file"
                    } else {
                        "files"
                    }
                ),
                DIM,
            ),
            (format!("+{}", stats.additions), GREEN),
            (format!("−{}", stats.deletions), RED),
        ] {
            if used > 0 && used + 2 + token.width() > width {
                rows.push(line);
                line = TextRow::default();
                used = 0;
            }
            if used > 0 {
                line.spans.push(span("  ", DIM));
                used += 2;
            }
            for ch in token.chars() {
                let cells = ch.width().unwrap_or(0);
                if used + cells > width {
                    rows.push(line);
                    line = TextRow::default();
                    used = 0;
                }
                line.spans.push(span(ch.to_string(), color));
                used += cells;
            }
        }
        rows.push(line);
        if pr.stats_error {
            rows.push(text("Stats stale · F5 retry", DIM));
        }
    } else {
        rows.extend(
            wrapped_text(
                if pr.stats_error {
                    "Stats unavailable · F5 retry"
                } else {
                    "Loading stats…"
                },
                width,
            )
            .into_iter()
            .map(|s| text(s, DIM)),
        );
    }
    rows
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
    let width = usize::from(rect.width.saturating_sub(1));
    let available = usize::from(rect.height.saturating_sub(2));
    app.nav_scroll = app.nav_scroll.min(app.selected);
    let mut start = app.selected;
    let mut needed = app
        .inbox
        .get(start)
        .map_or(0, |pr| inbox_rows(pr, width, true).len());
    while start > app.nav_scroll {
        let Some(pr) = app.inbox.get(start - 1) else {
            break;
        };
        let height = inbox_rows(pr, width, false).len() + 1;
        if needed + height > available {
            break;
        }
        needed += height;
        start -= 1;
    }
    app.nav_scroll = start;
    let mut y = rect.y.saturating_add(2);
    for index in app.nav_scroll..app.inbox.len() {
        if y >= rect.bottom() {
            break;
        }
        let Some(pr) = app.inbox.get(index) else {
            break;
        };
        let selected = index == app.selected;
        let rows = inbox_rows(pr, width, selected);
        let height = rows.len().min(usize::from(rect.bottom() - y)) as u16;
        let row_rect = Rect::new(rect.x, y, rect.width.saturating_sub(1), height);
        frame.render_widget(
            Block::default().style(Style::default().bg(if selected { PANEL } else { BG })),
            row_rect,
        );
        for (offset, row) in rows.iter().take(usize::from(height)).enumerate() {
            paint(
                frame,
                Rect::new(rect.x, y + offset as u16, row_rect.width, 1),
                row,
                app,
            );
        }
        app.hits.push((row_rect, Action::SelectPr(index)));
        y = y.saturating_add(height).saturating_add(1);
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
            entries.extend(
                wrapped_text(
                    &format!("{}{parent}/", "  ".repeat(depth.min(5))),
                    rect.width.saturating_sub(1) as usize,
                )
                .into_iter()
                .map(|line| (line, None)),
            );
        }
        previous = parents.iter().map(|s| s.to_string()).collect();
        let label = format!(
            "{}{} {}",
            "  ".repeat(parents.len().min(5)),
            if index == app.file { "▸" } else { " " },
            parts.last().unwrap_or(&"")
        );
        entries.extend(
            wrapped_text(&label, rect.width.saturating_sub(1) as usize)
                .into_iter()
                .map(|line| (line, Some(index))),
        );
        if index == app.file {
            selected_row = entries.len().saturating_sub(1);
        }
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

fn draw_definition(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(120);
    let height = area.height.saturating_sub(4).min(40);
    let rect = Rect::new(
        (area.width - width) / 2,
        (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" Function definition ")
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL).fg(TEXT));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    app.hits.clear();
    let Some(Modal::Definition(viewer)) = &mut app.modal else {
        return;
    };
    let definition = viewer.output.as_ref().and_then(|r| r.as_ref().ok());
    let path = definition.map_or(viewer.request.path.as_str(), |d| d.path.as_str());
    let line = definition.map_or(viewer.request.line, |d| d.line as u64);
    let short_revision: String = viewer.request.revision.chars().take(8).collect();
    let header = wrapped_text(
        &format!("{path}:{line} · {short_revision}"),
        inner.width as usize,
    );
    let header_height = header.len().min(inner.height.saturating_sub(3) as usize);
    for (i, row) in header.iter().take(header_height).enumerate() {
        frame.render_widget(
            Paragraph::new(row.as_str()).style(Style::default().fg(ACCENT)),
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
        );
    }
    let body = Rect::new(
        inner.x,
        inner.y + header_height as u16 + 1,
        inner.width,
        inner.height.saturating_sub(header_height as u16 + 2),
    );
    viewer.viewport = body.height as usize;
    match &viewer.output {
        None => frame.render_widget(
            Paragraph::new("Resolving definition from the pinned local revision…")
                .style(Style::default().fg(DIM)),
            body,
        ),
        Some(Err(error)) => {
            let rows = wrapped_text(
                &format!("Definition unavailable\n\n{error}"),
                body.width as usize,
            );
            for (i, row) in rows.iter().take(body.height as usize).enumerate() {
                frame.render_widget(
                    Paragraph::new(row.as_str()).style(Style::default().fg(DIM)),
                    Rect::new(body.x, body.y + i as u16, body.width, 1),
                );
            }
        }
        Some(Ok(definition)) => {
            viewer.scroll = viewer.scroll.min(
                definition
                    .source
                    .lines()
                    .count()
                    .saturating_sub(viewer.viewport),
            );
            for (i, (offset, value)) in definition
                .source
                .lines()
                .enumerate()
                .skip(viewer.scroll)
                .take(body.height as usize)
                .enumerate()
            {
                let line = DiffLine {
                    kind: LineKind::Context,
                    old: None,
                    new: Some(definition.line.saturating_add(offset) as u64),
                    text: value.into(),
                };
                frame.render_widget(
                    Paragraph::new(Line::from(code(
                        Some(&line),
                        false,
                        body.width as usize,
                        viewer.horizontal,
                    ))),
                    Rect::new(body.x, body.y + i as u16, body.width, 1),
                );
            }
        }
    }
    if inner.height > 0 {
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        frame.render_widget(
            Paragraph::new("Esc Close   ↑/↓ Scroll   ←/→ Pan   PgUp/PgDn Page")
                .style(Style::default().fg(ACCENT)),
            footer,
        );
        app.hits.push((
            Rect::new(footer.x, footer.y, footer.width.min(9), 1),
            Action::CloseDefinition,
        ));
    }
}

fn draw_modal(frame: &mut Frame, app: &mut App) {
    if matches!(app.modal, Some(Modal::Definition(_))) {
        draw_definition(frame, app);
        return;
    }
    if matches!(app.modal, Some(Modal::Workflow(_))) {
        crate::workflow_ui::draw(frame, app);
        return;
    }
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
    let mut input_cursor = None;
    let rows = match modal {
        Modal::Definition(_) => Vec::new(),
        Modal::Workflow(_) => Vec::new(),
        Modal::Repositories {
            manage,
            query,
            selected,
            choices,
        } => {
            input_cursor = Some((
                inner.x + 7 + (query.width().min(inner.width.saturating_sub(9) as usize) as u16),
                inner.y + 1,
            ));
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
                text(
                    format!(
                        "Search: {}",
                        crop(
                            query,
                            query
                                .width()
                                .saturating_sub(inner.width.saturating_sub(9) as usize),
                            inner.width.saturating_sub(9) as usize
                        )
                    ),
                    TEXT,
                ),
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
            text("Alt+↑ / ↓     Previous / next guide chapter", TEXT),
            text("Tab           Switch navigation / content focus", TEXT),
            text("Cmd+Up/Down   Scroll content by ten lines", TEXT),
            text("Enter         Open PR / comment / toggle completion", TEXT),
            text("Click symbol  JS/TS function definition · Esc closes", TEXT),
            text("/             PR actions / worktree management", TEXT),
            text("Page Up/Down  Scroll a page · Space scrolls down", TEXT),
            text("Home / End    Jump to start / end", TEXT),
            text("← → side · Alt+← → horizontal · Shift+↑↓ select", TEXT),
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
            input_cursor = Some((
                inner.x + 2 + (value.width().min(inner.width.saturating_sub(4) as usize) as u16),
                inner.y + rows.len() as u16,
            ));
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
            input_cursor = Some((
                inner.x + 8 + (query.width().min(inner.width.saturating_sub(9) as usize) as u16),
                inner.y + 1,
            ));
            let options = app.model_options(query);
            let selected = *selected;
            let mut rows = vec![
                bold("Model & reasoning", ACCENT),
                text(
                    format!(
                        "Filter: {}",
                        crop(
                            query,
                            query
                                .width()
                                .saturating_sub(inner.width.saturating_sub(9) as usize),
                            inner.width.saturating_sub(9) as usize
                        )
                    ),
                    DIM,
                ),
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
    if let Some((x, y)) = input_cursor
        && x < inner.right()
        && y < inner.bottom()
    {
        frame.set_cursor_position((x, y));
    }
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
    use anyhow::{Context, Result};

    fn guide_app(directory: &std::path::Path) -> App {
        use crate::{
            app::Review,
            codex::{Chapter, Guide},
            diff::Snapshot,
            model::{PrKey, PrSummary},
        };
        use std::sync::Arc;
        let key = PrKey {
            owner: "example".into(),
            repo: "repo".into(),
            number: 1,
        };
        let files = (0..3)
            .map(|index| {
                let path = format!(
                    "directory/with/a/very/long/path/to/a-distinctive-file-name-{index}.rs"
                );
                DiffFile {
                    path: path.clone(),
                    old_path: path,
                    status: "M".into(),
                    additions: 80,
                    deletions: 0,
                    hunks: vec![Hunk {
                        id: format!("h{index}"),
                        header: "@@ -0,0 +1,80 @@".into(),
                        lines: (1..=80)
                            .map(|line| DiffLine {
                                kind: LineKind::Add,
                                old: None,
                                new: Some(line),
                                text: format!("line_{line}"),
                            })
                            .collect(),
                    }],
                }
            })
            .collect();
        let mut app = App::new(
            crate::storage::Storage {
                config: directory.join("config.json"),
                cache: directory.into(),
            },
            Default::default(),
        );
        app.inbox.push(PrSummary {
            key: key.clone(),
            title: "Test".into(),
            author: "author".into(),
            updated: String::new(),
            created: String::new(),
            stats: None,
            stats_error: false,
            draft: false,
        });
        app.reviews.insert(
            key.id(),
            Review {
                snapshot: Some(Arc::new(Snapshot {
                    base: "base".into(),
                    head: "head".into(),
                    merge_base: "base".into(),
                    head_tree: "head tree".into(),
                    base_tree: "base tree".into(),
                    files,
                })),
                guide: Some(Arc::new(Guide {
                    chapters: vec![
                        Chapter {
                            title: "First chapter".into(),
                            explanation: "Changes across two files.".into(),
                            hunks: vec!["h0".into(), "h1".into()],
                        },
                        Chapter {
                            title: "Second chapter".into(),
                            explanation: "The final file.".into(),
                            hunks: vec!["h2".into()],
                        },
                    ],
                })),
                ..Review::default()
            },
        );
        app.action(Action::SetView(View::Guide));
        app
    }

    #[test]
    fn inbox_metadata_wraps_keeps_colors_and_selected_row_clickable() -> Result<()> {
        let dir = tempfile::tempdir()?;
        for width in [18, 42] {
            let mut app = guide_app(dir.path());
            let mut pr = app.inbox.first().context("Missing PR")?.clone();
            pr.created = "2026-09-10T12:30:00Z".into();
            pr.stats = Some(crate::model::PrStats {
                additions: 6582,
                deletions: 181,
                changed_files: 97,
            });
            app.inbox = (1..=8)
                .map(|number| {
                    let mut p = pr.clone();
                    p.key.number = number;
                    p
                })
                .collect();
            app.selected = 7;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24))?;
            terminal.draw(|f| draw_inbox(f, &mut app, Rect::new(0, 0, width, 24)))?;
            let buffer = terminal.backend().buffer();
            let contents: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(contents.contains("2026-09-10"));
            assert!(contents.contains("97 files"));
            assert!(contents.contains("+6582"));
            assert!(contents.contains("−181"));
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|c| c.symbol() == "+" && c.fg == GREEN)
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|c| c.symbol() == "−" && c.fg == RED)
            );
            let (hit, _) = app
                .hits
                .iter()
                .find(|(_, action)| matches!(action, Action::SelectPr(7)))
                .context("Selected PR clipped out")?;
            assert!(hit.bottom() <= 24);
            assert!(hit.height >= 5);
            assert!(app.nav_scroll > 0);
        }
        Ok(())
    }

    #[test]
    fn default_navigation_focus_and_fast_scroll_preserve_shortcut_meanings() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 30))?;
        app.action(Action::SetView(View::Guide));
        assert_eq!(app.focus, Focus::Navigation);
        terminal.draw(|f| draw(f, &mut app))?;
        let output: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(output.contains("Alt+↑/↓ Chapters"));
        assert!(output.contains("Cmd+↑/↓ 10 lines"));
        assert!(output.contains("Focus: Chapters"));
        let next_file = app
            .document
            .as_ref()
            .and_then(|d| d.navigation.get(1))
            .context("Missing next file")?
            .row;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.scroll, next_file);
        assert_eq!(app.focus, Focus::Navigation);
        app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(app.workflow.cursor, Some(10));
        assert_eq!(app.focus, Focus::Content);
        app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(app.scroll, 0);
        app.action(Action::SetView(View::Diff));
        assert_eq!(app.focus, Focus::Navigation);
        terminal.draw(|f| draw(f, &mut app))?;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.file, 1);
        terminal.draw(|f| draw(f, &mut app))?;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(app.file, 1);
        assert_eq!(app.workflow.cursor, Some(10));
        app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(app.scroll, 0);
        Ok(())
    }

    #[test]
    fn action_notices_render_once_with_outcome_colors() -> Result<()> {
        use crate::{app::Notice, workflow::Wizard};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))?;
        for (notice, color) in [
            (Notice::success("merged"), GREEN),
            (Notice::error("merge failed"), RED),
            (Notice::info("Waiting for GitHub"), DIM),
        ] {
            app.notice = notice;
            // An open wizard used to overwrite the footer with a second, red copy.
            for modal in [
                None,
                Some(Modal::Workflow(Box::new(Wizard::Home(0)))),
                Some(Modal::Workflow(Box::new(Wizard::Result {
                    notice: app.notice.clone(),
                }))),
            ] {
                app.modal = modal;
                terminal.draw(|f| draw(f, &mut app))?;
                let buffer = terminal.backend().buffer();
                let mut matches = 0;
                for cells in buffer.content.chunks(120) {
                    let line: String = cells.iter().map(|c| c.symbol()).collect();
                    if let Some(start) = line.find(&app.notice.message) {
                        matches += line.matches(&app.notice.message).count();
                        let column = line.get(..start).context("Notice prefix")?.width();
                        for cell in cells.iter().skip(column).take(app.notice.message.width()) {
                            assert_eq!(cell.fg, color);
                        }
                    }
                }
                assert_eq!(matches, 1, "notice must appear once: {}", app.notice);
            }
        }
        // With no selected PR, the general status must not repeat the footer notice.
        app.modal = None;
        app.home = true;
        app.inbox.clear();
        terminal.draw(|f| draw(f, &mut app))?;
        let output: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert_eq!(output.matches(&app.notice.message).count(), 1);
        Ok(())
    }

    #[test]
    fn snapshot_footer_shows_steps_and_live_transfer_details() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let review = app.reviews.values_mut().next().context("Missing review")?;
        review.preparing = true;
        review.preparation_started = Some(std::time::Instant::now());
        review.preparation_progress = Some(crate::repo::SnapshotProgress {
            step: 2,
            activity: "Receiving objects: 50%, 12 MiB | 2 MiB/s".into(),
        });
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))?;
        terminal.draw(|f| draw(f, &mut app))?;
        let output: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(output.contains("[■■□□□□□□□□] 2/5"));
        assert!(output.contains("Receiving objects: 50%, 12 MiB | 2 MiB/s"));
        app.reviews
            .values_mut()
            .next()
            .context("Missing review")?
            .preparation_progress = Some(crate::repo::SnapshotProgress {
            step: 5,
            activity: "Building and validating the diff".into(),
        });
        terminal.draw(|f| draw(f, &mut app))?;
        let output: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(output.contains("[■■■■■■■■□□] 5/5"));
        assert!(output.contains("Building and validating the diff"));
        assert!(!output.contains("Receiving objects:"));
        Ok(())
    }

    #[test]
    fn reused_hunks_render_once_per_chapter_and_keep_navigation_in_both_layouts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        for width in [80, 180] {
            let mut app = guide_app(dir.path());
            let review = app.reviews.values_mut().next().context("Missing review")?;
            let snapshot =
                std::sync::Arc::make_mut(review.snapshot.as_mut().context("Missing snapshot")?);
            let line = snapshot
                .files
                .first_mut()
                .and_then(|f| f.hunks.first_mut())
                .and_then(|h| h.lines.first_mut())
                .context("Missing shared hunk line")?;
            line.text = "shared_hunk_line".into();
            let guide: crate::codex::Guide = serde_json::from_value(serde_json::json!({
                "chapters": [
                    {"title": "First", "explanation": "First use", "hunks": ["h0", "h1", "h0"]},
                    {"title": "Second", "explanation": "Second use", "hunks": ["h0", "h2", "h0"]}
                ]
            }))?;
            guide.validate(snapshot)?;
            review.guide = Some(std::sync::Arc::new(guide));
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30))?;
            terminal.draw(|frame| draw(frame, &mut app))?;
            let doc = app.document.as_ref().context("Missing document")?;
            assert_eq!(doc.sections.len(), 2);
            let second = doc.sections.get(1).context("Missing second chapter")?.start;
            for (start, end) in [(0, second), (second, doc.rows.len())] {
                let rows = doc.rows.iter().skip(start).take(end - start);
                let code = rows
                    .flat_map(|row| row.right.spans.iter())
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                assert_eq!(code.matches("shared_hunk_line").count(), 1);
                assert_eq!(
                    doc.files
                        .iter()
                        .filter(|f| f.start >= start && f.start < end)
                        .count(),
                    2
                );
            }
            app.key_event(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::ALT,
            ));
            assert_eq!(app.scroll, second);
            terminal.draw(|frame| draw(frame, &mut app))?;
            let output: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(output.contains("shared_hunk_line"));
        }
        Ok(())
    }

    #[test]
    fn wrapped_links_and_sticky_headers_follow_each_file_in_both_layouts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        for width in [80, 180] {
            let mut app = guide_app(dir.path());
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30))?;
            terminal.draw(|frame| draw(frame, &mut app))?;
            let doc = app.document.as_ref().context("Missing document")?;
            let first = doc.files.first().context("Missing file")?;
            let target = first.start;
            let destination = doc
                .rows
                .get(target)
                .context("Missing link destination")?
                .right
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(destination.starts_with("directory/with/"));
            let links = doc
                .rows
                .iter()
                .flat_map(|row| [&row.left, &row.right])
                .filter(|row| {
                    matches!(
                        row.action,
                        Some(Action::Workflow(crate::workflow::WAction::Nav(0)))
                    )
                })
                .collect::<Vec<_>>();
            let complete = links
                .iter()
                .flat_map(|row| row.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(complete.contains("a-distinctive-file-name-0.rs"));
            if width == 180 {
                assert!(links.len() > 1);
            }
            let starts = doc.files.iter().map(|file| file.start).collect::<Vec<_>>();
            for (index, start) in starts.iter().enumerate() {
                app.scroll = start + 10;
                terminal.draw(|frame| draw(frame, &mut app))?;
                let doc = app.document.as_ref().context("Missing document")?;
                let file = doc.files.get(index).context("Missing file")?;
                let x = app.content_rect.x
                    + if doc.guide_columns {
                        doc.left_width + 3
                    } else {
                        0
                    };
                let y = app.content_rect.y;
                let header = (0..file.header.len())
                    .flat_map(|line| {
                        (x..app.content_rect.right()).map(move |column| (column, y + line as u16))
                    })
                    .filter_map(|position| terminal.backend().buffer().cell(position))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>();
                assert!(header.contains(&format!("a-distinctive-file-name-{index}.rs")));
            }
            app.action(Action::SetView(View::Diff));
            app.scroll = 12;
            terminal.draw(|frame| draw(frame, &mut app))?;
            let first_row = (app.content_rect.x..app.content_rect.right())
                .filter_map(|x| terminal.backend().buffer().cell((x, app.content_rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(first_row.contains("directory/with/a/very"));
        }
        Ok(())
    }

    #[test]
    fn alt_arrows_reach_and_align_a_short_final_chapter() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        for width in [80, 180] {
            let mut app = guide_app(dir.path());
            let snapshot = app
                .reviews
                .get_mut("example/repo#1")
                .and_then(|r| r.snapshot.as_mut())
                .context("Missing snapshot")?;
            let snapshot = std::sync::Arc::make_mut(snapshot);
            snapshot
                .files
                .last_mut()
                .and_then(|f| f.hunks.first_mut())
                .context("Missing hunk")?
                .lines
                .truncate(1);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30))?;
            terminal.draw(|frame| draw(frame, &mut app))?;
            let last = app
                .document
                .as_ref()
                .and_then(|d| d.sections.last())
                .context("Missing chapter")?
                .start;
            app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
            terminal.draw(|frame| draw(frame, &mut app))?;
            assert_eq!(app.scroll, last);
            app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
            assert_eq!(app.scroll, last);
            app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            assert_eq!(app.scroll, 0);
        }
        Ok(())
    }
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
    #[test]
    fn chapter_completion_is_local_per_section_and_persists_for_exact_content() -> Result<()> {
        use crate::workflow::Target;
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let r = app
            .reviews
            .get_mut("example/repo#1")
            .context("Missing review")?;
        std::sync::Arc::make_mut(r.guide.as_mut().context("Missing guide")?)
            .chapters
            .get_mut(1)
            .context("Missing chapter")?
            .hunks
            .push("h0".into());
        app.sync_progress("example/repo#1");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 36))?;
        terminal.draw(|frame| draw(frame, &mut app))?;
        let first = app
            .document
            .as_ref()
            .context("Missing document")?
            .rows
            .iter()
            .position(|r| {
                matches!(
                    &r.right.target,
                    Some(Target::Header {
                        chapter: Some(0),
                        ..
                    })
                )
            })
            .context("Missing first header")?;
        app.workflow.cursor = Some(first);
        app.focus = Focus::Content;
        app.enter_diff();
        terminal.draw(|frame| draw(frame, &mut app))?;
        let r = app.review().context("Missing review")?;
        assert_eq!(r.interaction.progress.completed.len(), 1);
        assert!(r.interaction.github.viewed.is_empty());
        let doc = app.document.as_ref().context("Missing doc")?;
        let second = doc.sections.get(1).context("Missing chapter")?;
        assert!(doc.rows.iter().skip(second.start).take(second.end-second.start).any(|r|matches!(&r.right.target,Some(Target::Code{path,..})if path.ends_with("file-name-0.rs"))));
        let saved = app
            .review()
            .context("Missing review")?
            .interaction
            .progress_key
            .clone();
        app.reviews
            .get_mut("example/repo#1")
            .context("Missing review")?
            .interaction = Default::default();
        app.sync_progress("example/repo#1");
        assert_eq!(
            app.review()
                .context("Missing review")?
                .interaction
                .progress
                .completed
                .len(),
            1
        );
        let r = app
            .reviews
            .get_mut("example/repo#1")
            .context("Missing review")?;
        std::sync::Arc::make_mut(r.guide.as_mut().context("Missing guide")?)
            .chapters
            .get_mut(0)
            .context("Missing chapter")?
            .explanation
            .push_str(" Updated.");
        app.sync_progress("example/repo#1");
        let r = app.review().context("Missing review")?;
        assert_ne!(r.interaction.progress_key, saved);
        assert!(r.interaction.progress.completed.is_empty());
        Ok(())
    }
    #[test]
    fn line_focus_and_selection_keep_correct_side_and_file() -> Result<()> {
        use crate::{review::Side, workflow::Target};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 30))?;
        terminal.draw(|f| draw(f, &mut app))?;
        let first = app
            .document
            .as_ref()
            .context("Missing doc")?
            .rows
            .iter()
            .position(|r| matches!(r.right.target, Some(Target::Code { new: Some(1), .. })))
            .context("Missing line")?;
        app.focus = Focus::Content;
        app.workflow.cursor = Some(first);
        app.workflow.side = Side::Right;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.workflow.cursor, Some(first + 1));
        assert_eq!(app.workflow.selection.as_ref().map(|a| a.start), Some(1));
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(app.workflow.cursor, Some(first + 11));
        assert!(app.workflow.selection.is_none());
        terminal.draw(|f| draw(f, &mut app))?;
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|c| c.symbol() == ">" && c.fg == ACCENT)
        );
        app.key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.workflow.side, Side::Left);
        assert_eq!(app.horizontal, 0);
        app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(app.horizontal, 4);
        let current = app.workflow.cursor;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.workflow.cursor, current);
        Ok(())
    }
    #[test]
    fn all_text_inputs_position_a_cursor_and_wizard_keeps_drafts() -> Result<()> {
        use crate::{
            editor::Editor,
            workflow::{Compose, Kind, WAction, Wizard},
        };
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let key = app.inbox.first().context("Missing PR")?.key.clone();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 36))?;
        let draft = Compose {
            key,
            head: "head".into(),
            kind: Kind::Review,
            editor: Editor::default(),
            choice: 0,
            focus: 0,
            mention: 0,
        };
        let draft_id = draft.id();
        app.wizard(Wizard::Compose(draft));
        app.paste("A unicode review 🦀\nsecond line".into());
        terminal.draw(|f| draw(f, &mut app))?;
        let position = terminal.get_cursor_position()?;
        assert!(position.x > 0 && position.y > 4);
        app.workflow_action(WAction::Next);
        assert!(
            matches!(&app.modal,Some(Modal::Workflow(m))if matches!(m.as_ref(),Wizard::Confirm{..}))
        );
        app.workflow_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(&app.modal,Some(Modal::Workflow(m))if matches!(m.as_ref(),Wizard::Compose(_)))
        );
        app.workflow_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());
        assert_eq!(
            app.workflow
                .drafts
                .get(&draft_id)
                .context("Missing saved draft")?
                .editor
                .text(),
            "A unicode review 🦀\nsecond line"
        );
        for modal in [
            Modal::Clone {
                value: "/tmp/clone".into(),
                key: "example/repo".into(),
            },
            Modal::Models {
                selected: 0,
                effort: 0,
                query: "luna".into(),
            },
            Modal::Repositories {
                manage: true,
                query: "example".into(),
                selected: 0,
                choices: Default::default(),
            },
        ] {
            app.modal = Some(modal);
            terminal.draw(|f| draw(f, &mut app))?;
            let cursor = terminal.get_cursor_position()?;
            assert!(cursor.x > 0 && cursor.y > 3);
        }
        Ok(())
    }
}
