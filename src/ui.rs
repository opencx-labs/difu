use crate::{
    app::{Action, App, Focus, Modal, Review, View},
    context::Direction,
    diff::{DiffFile, DiffLine, Hunk, LineKind, split_rows},
    model::{InboxTab, PrState, PrSummary, clean},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const BG: Color = Color::Reset;
pub(crate) const INK: Color = Color::Rgb(12, 14, 18);
pub(crate) const PANEL: Color = Color::Reset;
pub(crate) const TEXT: Color = Color::Rgb(220, 225, 232);
pub(crate) const DIM: Color = Color::Rgb(130, 140, 156);
pub(crate) const BORDER: Color = Color::Rgb(42, 48, 61);
pub(crate) const ACCENT: Color = Color::Rgb(0, 255, 65);
/// Dark theme tint with white text (approximately 12:1 contrast).
pub(crate) fn user_message_style() -> Style {
    let background = match ACCENT {
        Color::Rgb(r, g, b) => Color::Rgb(r / 4, g / 4, b / 4),
        _ => Color::Rgb(0, 63, 16),
    };
    Style::default().bg(background).fg(Color::White)
}
pub(crate) const GREEN: Color = Color::Rgb(114, 216, 163);
pub(crate) const PURPLE: Color = Color::Rgb(171, 125, 248);
pub(crate) const YELLOW: Color = Color::Rgb(229, 192, 100);
pub(crate) const RED: Color = Color::Rgb(247, 137, 145);
pub(crate) const ADD_BG: Color = Color::Rgb(18, 43, 32);
pub(crate) const REMOVE_BG: Color = Color::Rgb(49, 25, 31);

#[derive(Clone, Default)]
pub struct TextRow {
    pub hunk: Option<String>,
    pub image: Option<crate::images::PreviewRow>,
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
    pub category_start: usize,
    pub category: TextRow,
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

fn file_header(
    file: &DiffFile,
    width: usize,
    chapter: Option<usize>,
    review: &Review,
) -> Vec<TextRow> {
    let hint = match chapter {
        Some(chapter)
            if review
                .interaction
                .progress
                .completed
                .contains(&(chapter, file.path.clone())) =>
        {
            "Enter: reopen section"
        }
        Some(_) => "Enter: complete section",
        None if review.interaction.github.viewed.contains(&file.path)
            && review
                .snapshot
                .as_ref()
                .is_some_and(|s| s.head == review.interaction.github.head) =>
        {
            "Enter: mark unviewed"
        }
        None if review.local.is_some() => "Local diff",
        None => "Enter: mark viewed",
    };
    wrapped_text(
        &format!(
            "{}   +{} −{}  ·  {hint}",
            file.path, file.additions, file.deletions
        ),
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
        image: None,
        hunk: None,
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
        image: None,
        hunk: None,
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
        image: None,
        hunk: None,
    }
}
fn append(rows: &mut Vec<Row>, right: TextRow) {
    rows.push(Row {
        right,
        ..Default::default()
    });
}

pub(crate) fn prose(source: &str, width: usize) -> Vec<TextRow> {
    crate::markdown::rows(source, width)
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
    spans.extend(syntax_spans(&content).into_iter().map(|mut span| {
        span.style = span.style.bg(bg);
        span
    }));
    spans.push(Span::styled(
        " ".repeat(code_width.saturating_sub(content.width())),
        Style::default().bg(bg),
    ));
    spans
}

pub(crate) fn syntax_spans(content: &str) -> Vec<Span<'static>> {
    let bg = Color::Reset;
    let mut spans = Vec::new();
    // A small lexical highlighter keeps rendering independent of language parsers.
    // Diff colors remain meaningful for every file type.
    let mut token = String::new();
    let mut quoted = false;
    let mut quote = '\0';
    let comment = content.trim_start().starts_with("//")
        || content.trim_start().starts_with('#')
        || content.trim_start().starts_with("--");
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
            "for",
            "in",
            "use",
            "mut",
            "while",
            "loop",
            "break",
            "continue",
            "try",
            "except",
            "raise",
            "some",
            "none",
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
            "select",
            "update",
            "where",
            "set",
            "insert",
            "into",
            "delete",
            "with",
            "as",
            "and",
            "or",
            "not",
            "exists",
            "is",
            "distinct",
            "null",
            "true",
            "false",
            "join",
            "left",
            "right",
            "inner",
            "outer",
            "on",
            "group",
            "by",
            "order",
            "having",
            "limit",
            "offset",
            "union",
            "all",
            "case",
            "when",
            "then",
            "end",
            "returning",
            "conflict",
            "do",
            "nothing",
            "begin",
            "commit",
            "rollback",
            "timestamp",
        ]
        .contains(&token.to_ascii_lowercase().as_str())
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

// Offsets are terminal-cell positions in the original line. Keeping the source
// line intact preserves definition links and review anchors on every wrap row.
fn code_offsets(
    line: Option<&DiffLine>,
    old: bool,
    width: usize,
    layout: (usize, bool),
) -> Vec<usize> {
    let (horizontal, wrap) = layout;
    if !wrap {
        return vec![horizontal];
    }
    let Some(line) = line else { return Vec::new() };
    let number = if old { line.old } else { line.new };
    let prefix = format!(
        "{:>5}   ",
        number.map(|n| n.to_string()).unwrap_or_default()
    )
    .width();
    let available = width.saturating_sub(prefix);
    let mut offsets = vec![0];
    if available == 0 {
        return offsets;
    }
    let mut position = 0;
    let mut used = 0;
    for ch in clean(&line.text).replace('\t', "    ").chars() {
        let size = ch.width().unwrap_or(0);
        if used > 0 && used + size > available {
            offsets.push(position);
            used = 0;
        }
        used += size;
        position += size;
    }
    offsets
}

fn code_rows(
    path: &str,
    lines: &[DiffLine],
    width: usize,
    split: bool,
    layout: (usize, bool),
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
            let old_offsets = code_offsets(old, true, left, layout);
            let new_offsets = code_offsets(new, false, right, layout);
            for index in 0..old_offsets.len().max(new_offsets.len()) {
                let old_offset = old_offsets.get(index).copied();
                let new_offset = new_offsets.get(index).copied();
                let old = old.filter(|_| old_offset.is_some());
                let new = new.filter(|_| new_offset.is_some());
                let mut spans = code(old, true, left, old_offset.unwrap_or(0));
                spans.push(span("│", BORDER));
                spans.extend(code(new, false, right, new_offset.unwrap_or(0)));
                let mut links = code_links(path, old, true, left, old_offset.unwrap_or(0), 0);
                links.extend(code_links(
                    path,
                    new,
                    false,
                    right,
                    new_offset.unwrap_or(0),
                    left + 1,
                ));
                rows.push(TextRow {
                    spans,
                    code_links: links,
                    image: None,
                    hunk: None,
                    action: None,
                    target: Some(crate::workflow::Target::Code {
                        path: path.into(),
                        old: old.and_then(|l| l.old),
                        new: new.and_then(|l| l.new),
                    }),
                });
            }
        }
    } else {
        for line in lines {
            let old = line.kind == LineKind::Remove;
            for offset in code_offsets(Some(line), old, width, layout) {
                rows.push(TextRow {
                    spans: code(Some(line), old, width, offset),
                    code_links: code_links(path, Some(line), old, width, offset, 0),
                    image: None,
                    hunk: None,
                    action: None,
                    target: Some(crate::workflow::Target::Code {
                        path: path.into(),
                        old: line.old,
                        new: line.new,
                    }),
                });
            }
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
    } else {
        let bounds = review.bounds.get(&file.path)?.data?;
        if !bounds.can_expand(hunk, direction) {
            return None;
        }
    }
    if state.is_some_and(|s| {
        s.pending
            .iter()
            .any(|(id, d, _)| id == &hunk.id && *d == direction)
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
    horizontal: (usize, bool),
    with_title: (bool, Option<usize>),
    review: &Review,
) -> Vec<TextRow> {
    let mut rows = Vec::new();
    if with_title.0 {
        rows.extend(file_header(file, width, with_title.1, review));
    }
    rows.push(text(format!(" {}", hunk.header), DIM));
    if let Some(button) = expansion_button(review, file, hunk, Direction::Above) {
        rows.push(button);
    }
    let state = review.context.get(&file.path);
    if let Some(error) = review
        .bounds
        .get(&file.path)
        .and_then(|state| state.error.as_ref())
    {
        rows.extend(prose(
            &format!("Could not read file boundaries: {error}. Press r to retry."),
            width,
        ));
    }
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
    for row in &mut rows {
        row.hunk = Some(hunk.id.clone());
    }
    rows
}

pub(crate) fn build(app: &App, width: u16) -> Document {
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
    if app.repository_directory() {
        if let Some(name) = &app.repo_selected {
            append(&mut doc.rows, bold(name.clone(), TEXT));
            append(&mut doc.rows, TextRow::default());
            append(
                &mut doc.rows,
                link("Enter · Open pull requests", Action::OpenRepository),
            );
            append(
                &mut doc.rows,
                link(
                    if app.config.pinned_repositories.contains(name) {
                        "* · Unpin repository"
                    } else {
                        "* · Pin repository"
                    },
                    Action::PinRepository(name.clone()),
                ),
            );
        } else {
            append(
                &mut doc.rows,
                text("Select a repository to open its pull requests.", DIM),
            );
        }
        return doc;
    }
    let Some(review) = app.review() else {
        return doc;
    };
    if app.view == View::Overview {
        if let Some(local) = &review.local {
            for row in prose(
                &format!(
                    "{}\nBranch: {}\n{}\n\nGuide generation is manual: press g.",
                    local.root.display(),
                    local.branch,
                    if matches!(
                        local.comparison,
                        crate::local_diff::Comparison::WorkingTree { .. }
                    ) {
                        "Uncommitted changes against HEAD"
                    } else {
                        "HEAD against the merge base with main"
                    }
                ),
                width as usize,
            ) {
                append(&mut doc.rows, row);
            }
            return doc;
        }
        for row in crate::overview::rows_with_images(review, width, app.images.supported()) {
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
    if app.filter_kind() == Some(crate::filter::Kind::Files)
        && crate::tree::filtered(&snapshot.files, &app.filters.files.text()).is_empty()
    {
        append(&mut doc.rows, text("No matching files.", DIM));
        return doc;
    }
    if app.view == View::Guide
        && let Some(guide) = &review.guide
    {
        let wide = width >= 132;
        doc.guide_columns = wide;
        doc.left_width = if wide { (width / 4).clamp(30, 44) } else { 0 };
        let code_width = width.saturating_sub(if wide { doc.left_width + 3 } else { 0 }) as usize;
        let code_width = code_width.saturating_sub(2);
        let split = wide && !app.config.unified;
        let mut previous_category = None;
        let mut category_start = 0;
        let mut category_header = TextRow::default();
        for (chapter_index, chapter) in guide.chapters.iter().enumerate() {
            if previous_category != Some(chapter.category) {
                let label = match chapter.category {
                    crate::codex::ChapterCategory::Schema => Some("Manual schemas / DTOs"),
                    crate::codex::ChapterCategory::Migrations => Some("Database migrations"),
                    crate::codex::ChapterCategory::Regular => Some("Implementation"),
                    crate::codex::ChapterCategory::Generated => Some("Generated code"),
                    crate::codex::ChapterCategory::Tests => Some("Tests"),
                };
                if let Some(label) = label {
                    let divider = bold(format!("── {label} ──"), ACCENT);
                    category_start = doc.rows.len();
                    category_header = divider.clone();
                    if wide {
                        doc.rows.push(Row {
                            left: divider,
                            right: text("─".repeat(code_width), BORDER),
                        });
                    } else {
                        append(&mut doc.rows, divider);
                    }
                    doc.rows.push(Row::default());
                }
                previous_category = Some(chapter.category);
            }
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
                        headers.push((
                            right.len(),
                            file_header(file, code_width, Some(chapter_index), review),
                        ));
                    }
                    let collapsed = review
                        .interaction
                        .progress
                        .completed
                        .contains(&(chapter_index, file.path.clone()));
                    if collapsed {
                        if title {
                            right.extend(file_header(
                                file,
                                code_width,
                                Some(chapter_index),
                                review,
                            ));
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
                        (app.horizontal, app.config.wrap_diff),
                        (title, Some(chapter_index)),
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
                category_start,
                category: category_header.clone(),
                start,
                end: doc.rows.len(),
                left,
            });
            for _ in 0..2 {
                doc.rows.push(Row::default());
            }
        }
    } else {
        for (_, file) in snapshot.files.iter().enumerate().filter(|(index, file)| {
            app.directory
                .as_ref()
                .map_or(*index == app.file, |directory| {
                    file.path
                        .strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/'))
                })
        }) {
            let start = doc.rows.len();
            let collapsed = review.interaction.github.viewed.contains(&file.path)
                && review.interaction.github.head == snapshot.head;
            if collapsed {
                for row in file_header(file, width.saturating_sub(2) as usize, None, review) {
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
                    (app.horizontal, app.config.wrap_diff),
                    (i == 0, None),
                    review,
                ) {
                    append(&mut doc.rows, row);
                }
            }
            doc.files.push(FileSection {
                start,
                end: doc.rows.len(),
                header: file_header(file, width.saturating_sub(2) as usize, None, review),
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
                    Color::Rgb(18, 64, 34)
                } else {
                    Color::Rgb(18, 21, 27)
                });
            }
        }
    }
    if app.modal.is_none() {
        for link in &row.code_links {
            if let (Ok(column), Ok(width)) = (u16::try_from(link.column), u16::try_from(link.width))
                && column < code.width
            {
                let rect = Rect::new(code.x + column, code.y, width.min(code.width - column), 1);
                if app
                    .hover
                    .position
                    .is_some_and(|position| rect.contains(position))
                {
                    app.hover.rect = Some(rect);
                    for x in rect.x..rect.right() {
                        if let Some(cell) = frame.buffer_mut().cell_mut((x, rect.y)) {
                            cell.set_style(Style::default().add_modifier(Modifier::UNDERLINED));
                        }
                    }
                }
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
                .fg(if active { INK } else { DIM })
                .bg(if active { ACCENT } else { PANEL }),
        ),
        rect,
    );
    app.hits.push((rect, action));
    x + width + 1
}

// Map a code/header anchor through metadata-only layout changes, including
// repeated chapter appearances and continuation rows of wrapped source lines.
fn relocated_row(old: &Document, new: &Document, position: usize) -> Option<usize> {
    let section = old
        .sections
        .iter()
        .position(|s| position >= s.start && position < s.end);
    let old_start = section
        .and_then(|index| old.sections.get(index))
        .map_or(0, |s| s.start);
    let (start, end) = section
        .and_then(|index| new.sections.get(index))
        .map_or((0, new.rows.len()), |s| (s.start, s.end));
    let (offset, target) = old
        .rows
        .iter()
        .skip(position)
        .enumerate()
        .find_map(|(offset, row)| row.right.target.as_ref().map(|target| (offset, target)))?;
    let occurrence = old
        .rows
        .iter()
        .take(position + offset)
        .skip(old_start)
        .filter(|row| row.right.target.as_ref() == Some(target))
        .count();
    new.rows
        .iter()
        .enumerate()
        .take(end)
        .skip(start)
        .filter(|(_, row)| row.right.target.as_ref() == Some(target))
        .nth(occurrence)
        .map(|(index, _)| index.saturating_sub(offset))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    if app.home && app.inbox_tab == InboxTab::Diffs {
        crate::app::local::draw(frame, app);
        return;
    }
    let area = frame.area();
    app.hits.clear();
    app.hover.rect = None;
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    if area.width < 24 || area.height < 8 {
        frame.render_widget(Paragraph::new("difu · resize to at least 24 × 8"), area);
        return;
    }
    let title = Rect::new(2, 1, area.width.saturating_sub(4), 1);
    let identity = app
        .review()
        .and_then(|r| r.local.as_ref())
        .map(|c| format!("{} · {}", c.root.display(), c.branch))
        .unwrap_or_else(|| app.key().unwrap_or_else(|| app.inbox_tab.label().into()));
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
                "1 My PRs",
                app.inbox_tab == InboxTab::MyPrs,
                Action::SetInbox(InboxTab::MyPrs),
            ),
            (
                "2 Repositories",
                app.inbox_tab == InboxTab::Repositories,
                Action::SetInbox(InboxTab::Repositories),
            ),
            (
                "3 Diffs",
                app.inbox_tab == InboxTab::Diffs,
                Action::SetInbox(InboxTab::Diffs),
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
    let filters = app.home && !app.repository_directory() && area.height >= 14;
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
    }
    let content_y = if filters { 7 } else { 5 };
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
                .title(if app.repository_directory() {
                    " Repository "
                } else if app.view == View::Overview {
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
        if app.repository_directory() {
            draw_repositories(frame, app, nav);
        } else if app.home {
            draw_inbox(frame, app, nav);
        } else {
            draw_files(frame, app, nav);
        }
    }
    if app.document.as_ref().is_none_or(|d| {
        d.epoch != app.epoch || d.width != main.width || d.horizontal != app.horizontal
    }) {
        let next = build(app, main.width);
        if app.preserve_diff_position
            && let Some(previous) = &app.document
        {
            if let Some(cursor) = app.workflow.cursor {
                app.workflow.cursor = relocated_row(previous, &next, cursor).or(Some(cursor));
            }
            app.scroll = relocated_row(previous, &next, app.scroll).unwrap_or(app.scroll);
        }
        app.preserve_diff_position = false;
        app.document = Some(next);
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
    let main = if let Some(section) = doc
        .sections
        .iter()
        .rev()
        .find(|s| s.category_start <= app.scroll)
        && app.scroll > section.category_start
        && main.height > 2
    {
        paint(
            frame,
            Rect::new(main.x, main.y, main.width, 1),
            &section.category,
            app,
        );
        Rect::new(
            main.x,
            main.y + 1,
            main.width,
            main.height.saturating_sub(1),
        )
    } else {
        main
    };
    app.content_rect = main;
    app.viewport = usize::from(main.height);
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
    crate::images::draw_inline(frame, app, &doc, main);
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
    let mut status = if app.repository_directory() {
        if let Some(error) = &app.repositories_error {
            format!("Could not refresh repositories: {} · r retry", clean(error))
        } else if app.repositories_loading {
            "Showing cached repositories · refreshing…".into()
        } else {
            format!(
                "{} repositories · * pin/unpin",
                app.repository_options.len()
            )
        }
    } else if app.home && app.inbox_loading && !app.inbox.is_empty() {
        "Showing cached PRs · Refreshing…".into()
    } else if app.home && app.inbox_error.is_some() && !app.inbox.is_empty() {
        "Showing cached PRs · Refresh failed · r retry".into()
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
                "{}: {} · g retry",
                if review.preparation_failed {
                    "Snapshot"
                } else {
                    "Guide"
                },
                clean(error).replace('\n', " ")
            )
        } else if review.newer.is_some() {
            "● Remote PR updated · r to sync and refresh".into()
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
            && review.local.is_none()
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
            ("? Help", Action::Help),
            (
                "[ / ] State",
                Action::SetState(match app.state() {
                    PrState::Open => PrState::Merged,
                    PrState::Merged => PrState::Closed,
                    PrState::Closed => PrState::All,
                    PrState::All => PrState::Open,
                }),
            ),
            ("f Filter", Action::Filter),
            (
                "* Pin",
                Action::PinRepository(app.repo_selected.clone().unwrap_or_default()),
            ),
            ("Esc Repositories", Action::Back),
            ("r Refresh", Action::Refresh),
            ("Enter Open", Action::OpenPr),
            ("Cmd+↑/↓ 10 lines", Action::FastScroll(10)),
        ]
    } else {
        vec![
            (
                "/ Actions",
                Action::Workflow(crate::workflow::WAction::Open),
            ),
            ("c / Cmd+C Copy", Action::Copy),
            ("Cmd/Ctrl+click Definition", Action::Help),
            ("Alt+↑/↓ Chapters", Action::Chapter(true)),
            ("f Filter", Action::Filter),
            (
                if app.config.wrap_diff {
                    "w Unwrap"
                } else {
                    "w Wrap"
                },
                Action::ToggleWrap,
            ),
            ("Cmd+↑/↓ 10 lines", Action::FastScroll(10)),
            ("? Help", Action::Help),
            ("m Model", Action::Models),
            ("r Refresh", Action::Refresh),
            ("g Generate", Action::Regenerate),
            ("x Cancel", Action::Cancel),
            ("Ctrl+B Split", Action::ToggleLayout),
            ("Esc Home", Action::Back),
        ]
    };
    for (label, action) in footer {
        if matches!(label, "c / Cmd+C Copy" | "Cmd/Ctrl+click Definition")
            && app.view == View::Overview
        {
            continue;
        }
        if label == "Alt+↑/↓ Chapters" && app.view != View::Guide {
            continue;
        }
        if (label == "[ / ] State" && app.repository_directory())
            || (label == "* Pin" && !app.repository_directory())
            || (label == "Esc Repositories"
                && (app.inbox_tab != InboxTab::Repositories || app.repository.is_none()))
            || (label == "f Filter" && app.filter_kind().is_none())
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
        (Focus::Navigation, true, _, _) if app.repository_directory() => "Repositories",
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
            rows.push(text("Stats stale · r retry", DIM));
        }
    } else {
        rows.extend(
            wrapped_text(
                if pr.stats_error {
                    "Stats unavailable · r retry"
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

fn draw_filter(frame: &mut Frame, app: &mut App, rect: Rect, kind: crate::filter::Kind) {
    if rect.height < 3 {
        return;
    }
    let rect = Rect::new(rect.x, rect.y + 2, rect.width, 1);
    let focused = app.filters.focused == Some(kind) && app.modal.is_none();
    let width = rect.width.saturating_sub(3) as usize;
    let editor = app.filters.editor(kind);
    let (lines, (x, y)) =
        editor.styled_layout(width.max(1), Style::default().bg(ACCENT).fg(crate::ui::INK));
    let mut spans = vec![Span::raw("f ")];
    if editor.chars.is_empty() && !focused {
        spans.push(Span::raw("Filter…"));
    } else if let Some(line) = lines.get(y) {
        spans.extend(line.spans.clone());
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(
            Style::default()
                .fg(if focused { ACCENT } else { DIM })
                .bg(PANEL),
        ),
        rect,
    );
    app.hits.push((rect, Action::Filter));
    if focused && rect.width >= 3 {
        frame.set_cursor_position((rect.x + 2 + (x as u16).min(rect.width - 3), rect.y));
    }
}
fn draw_repositories(frame: &mut Frame, app: &mut App, rect: Rect) {
    paint(
        frame,
        Rect::new(rect.x, rect.y, rect.width, 1),
        &bold("REPOSITORIES", DIM),
        app,
    );
    draw_filter(frame, app, rect, crate::filter::Kind::Repositories);
    let names = app.visible_repositories();
    let mut rows = Vec::new();
    for (pinned, title) in [(true, "PINNED"), (false, "REST")] {
        rows.push((bold(title, DIM), None));
        for name in names
            .iter()
            .filter(|n| app.config.pinned_repositories.contains(*n) == pinned)
        {
            let selected = app.repo_selected.as_ref() == Some(name);
            rows.push((
                text(
                    format!(
                        "{} {} {}",
                        if selected { "▸" } else { " " },
                        if pinned { "◆" } else { "◇" },
                        name
                    ),
                    if selected { ACCENT } else { TEXT },
                ),
                Some(name.clone()),
            ));
        }
    }
    let selected = rows
        .iter()
        .position(|(_, name)| name.is_some() && *name == app.repo_selected)
        .unwrap_or(0);
    let available = rect.height.saturating_sub(4) as usize;
    let start = selected.saturating_sub(available.saturating_sub(1));
    for (offset, (row, name)) in rows.iter().skip(start).take(available).enumerate() {
        let hit = Rect::new(rect.x, rect.y + 4 + offset as u16, rect.width, 1);
        paint(frame, hit, row, app);
        if let Some(name) = name {
            app.hits.push((hit, Action::SelectRepository(name.clone())));
            app.hits.push((
                Rect::new(hit.x + 2, hit.y, hit.width.saturating_sub(2).min(1), 1),
                Action::PinRepository(name.clone()),
            ));
        }
    }
    if names.is_empty() && rect.height > 4 {
        let message = app
            .repositories_error
            .as_deref()
            .unwrap_or(if app.repositories_loading {
                "Loading repositories…"
            } else if app.filters.repositories.chars.is_empty() {
                "No repositories available."
            } else {
                "No matching repositories."
            });
        frame.render_widget(
            Paragraph::new(clean(message))
                .style(Style::default().fg(DIM))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            Rect::new(rect.x, rect.y + 4, rect.width, rect.height - 4),
        );
    }
}
fn draw_inbox(frame: &mut Frame, app: &mut App, rect: Rect) {
    let visible = app.visible_prs();
    let title = app
        .repository
        .clone()
        .unwrap_or_else(|| app.inbox_tab.label().to_uppercase());
    paint(
        frame,
        Rect::new(rect.x, rect.y, rect.width, 1),
        &bold(
            format!("{title}  {}/{}", visible.len(), app.inbox.len()),
            DIM,
        ),
        app,
    );
    draw_filter(frame, app, rect, crate::filter::Kind::PullRequests);
    if visible.is_empty() {
        let message = if !app.inbox.is_empty() {
            "No matching PRs.".into()
        } else {
            app.inbox_error.clone().unwrap_or_else(|| {
                if app.inbox_loading {
                    "Loading GitHub…".into()
                } else {
                    format!("No {} PRs.", app.state().label().to_lowercase())
                }
            })
        };
        for (i, row) in prose(&message, rect.width as usize)
            .iter()
            .take(rect.height.saturating_sub(4) as usize)
            .enumerate()
        {
            paint(
                frame,
                Rect::new(rect.x, rect.y + 4 + i as u16, rect.width, 1),
                row,
                app,
            );
        }
        return;
    }
    let width = usize::from(rect.width.saturating_sub(1));
    let available = usize::from(rect.height.saturating_sub(4));
    let selected = visible.iter().position(|i| *i == app.selected).unwrap_or(0);
    app.nav_scroll = app.nav_scroll.min(selected);
    let mut start = selected;
    let mut needed = app
        .inbox
        .get(app.selected)
        .map_or(0, |pr| inbox_rows(pr, width, true).len());
    while start > app.nav_scroll {
        let Some(pr) = visible.get(start - 1).and_then(|i| app.inbox.get(*i)) else {
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
    let mut y = rect.y.saturating_add(4);
    for index in visible.into_iter().skip(start) {
        if y >= rect.bottom() {
            break;
        }
        let Some(pr) = app.inbox.get(index) else {
            break;
        };
        let selected = index == app.selected;
        let rows = inbox_rows(pr, width, selected);
        let height = rows.len().min(usize::from(rect.bottom() - y)) as u16;
        let hit = Rect::new(rect.x, y, rect.width.saturating_sub(1), height);
        frame.render_widget(
            Block::default().style(Style::default().bg(if selected { PANEL } else { BG })),
            hit,
        );
        for (offset, row) in rows.iter().take(height as usize).enumerate() {
            paint(
                frame,
                Rect::new(rect.x, y + offset as u16, hit.width, 1),
                row,
                app,
            );
        }
        app.hits.push((hit, Action::SelectPr(index)));
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
    draw_filter(frame, app, rect, crate::filter::Kind::Files);
    let Some(snapshot) = app.review().and_then(|r| r.snapshot.clone()) else {
        return;
    };
    let entries = crate::tree::filtered(&snapshot.files, &app.filters.files.text());
    if entries.is_empty() {
        paint(
            frame,
            Rect::new(rect.x, rect.y.saturating_add(4), rect.width, 1),
            &text("No matching files.", DIM),
            app,
        );
    }
    let selected = |entry: &crate::tree::Entry| match &app.directory {
        Some(path) => entry.file.is_none() && &entry.path == path,
        None => entry.file == Some(app.file),
    };
    let selected_row = entries.iter().position(selected).unwrap_or(0);
    let width = rect.width.saturating_sub(2) as usize;
    app.tree_max_horizontal = entries
        .iter()
        .map(|entry| entry.label().width().saturating_sub(width))
        .max()
        .unwrap_or(0);
    app.tree_horizontal = app.tree_horizontal.min(app.tree_max_horizontal);
    let height = rect.height.saturating_sub(4) as usize;
    let start = selected_row.saturating_sub(height.saturating_sub(1));
    for (offset, entry) in entries.iter().enumerate().skip(start).take(height) {
        let y = rect.y + 4 + (offset - start) as u16;
        let active = offset == selected_row;
        let color = if active {
            ACCENT
        } else if entry.file.is_some() {
            TEXT
        } else {
            DIM
        };
        let row = text(
            format!(
                "{} {}",
                if active { "▸" } else { " " },
                crop(
                    &format!(
                        "{}{}",
                        entry
                            .file
                            .and_then(|i| snapshot.files.get(i))
                            .map(|f| format!("{} ", crate::local_diff::status_letter(&f.status)))
                            .unwrap_or_default(),
                        entry.label()
                    ),
                    app.tree_horizontal,
                    width
                )
            ),
            color,
        );
        let hit = Rect::new(rect.x, y, rect.width, 1);
        paint(frame, hit, &row, app);
        app.hits.push((
            hit,
            match entry.file {
                Some(index) => Action::SelectFile(index),
                None => Action::SelectDirectory(entry.path.clone()),
            },
        ));
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
    let mut links = Vec::new();
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
                for mut link in code_links(
                    &definition.path,
                    Some(&line),
                    false,
                    body.width as usize,
                    viewer.horizontal,
                    0,
                ) {
                    if offset == 0
                        && let Action::Definition { column, .. } = &mut link.action
                    {
                        *column += definition.column;
                    }
                    links.push((body.y + i as u16, link));
                }
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
    for (y, link) in links {
        if let (Ok(column), Ok(width)) = (u16::try_from(link.column), u16::try_from(link.width))
            && column < body.width
        {
            let rect = Rect::new(body.x + column, y, width.min(body.width - column), 1);
            app.hits.push((rect, link.action));
            if app
                .hover
                .position
                .is_some_and(|position| rect.contains(position))
            {
                app.hover.rect = Some(rect);
                for x in rect.x..rect.right() {
                    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                        cell.set_style(Style::default().add_modifier(Modifier::UNDERLINED));
                    }
                }
            }
        }
    }
    if inner.height > 0 {
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        frame.render_widget(
            Paragraph::new(
                "Esc Close   Cmd/Ctrl+click Definition   ↑/↓ Scroll   ←/→ Pan   PgUp/PgDn Page",
            )
            .style(Style::default().fg(ACCENT)),
            footer,
        );
        app.hits.push((
            Rect::new(footer.x, footer.y, footer.width.min(9), 1),
            Action::CloseDefinition,
        ));
    }
}

fn draw_help(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.saturating_sub(4).min(38);
    let rect = Rect::new(
        (area.width - width) / 2,
        (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" difu · keyboard & mouse ")
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    app.hits.clear();
    let Some(Modal::Help(state)) = &mut app.modal else {
        return;
    };
    let input_width = inner.width.saturating_sub(8).max(1) as usize;
    let (input, (x, y)) = state
        .query
        .styled_layout(input_width, Style::default().bg(ACCENT).fg(crate::ui::INK));
    let mut spans = vec![Span::raw("Search: ")];
    if let Some(line) = input.get(y) {
        spans.extend(line.spans.clone());
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().fg(ACCENT)),
        Rect::new(inner.x, inner.y, inner.width, inner.height.min(1)),
    );
    let cursor_x = inner.x.saturating_add(8).saturating_add(x as u16);
    if cursor_x < inner.right() && inner.height > 0 {
        frame.set_cursor_position((cursor_x, inner.y));
    }
    let matches = crate::help::entries(&state.query.text());
    let mut rows = Vec::new();
    for (key, description) in &matches {
        rows.extend(prose(
            &format!("{key:18} {description}"),
            inner.width as usize,
        ));
    }
    if rows.is_empty() {
        rows.push(text("No matching shortcuts.", DIM));
    }
    state.viewport = inner.height.saturating_sub(3) as usize;
    state.rows = rows.len();
    state.scroll = state.scroll.min(rows.len().saturating_sub(state.viewport));
    let scroll = state.scroll;
    let viewport = state.viewport;
    if inner.height > 1 {
        frame.render_widget(
            Paragraph::new(format!(
                "{} matches · ↑↓ scroll · Ctrl+U clear · Esc close",
                matches.len()
            ))
            .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
    for (index, row) in rows.iter().skip(scroll).take(viewport).enumerate() {
        paint(
            frame,
            Rect::new(inner.x, inner.y + 3 + index as u16, inner.width, 1),
            row,
            app,
        );
    }
}

fn draw_modal(frame: &mut Frame, app: &mut App) {
    if matches!(app.modal, Some(Modal::Help(_))) {
        draw_help(frame, app);
        return;
    }
    if matches!(app.modal, Some(Modal::Image(_))) {
        crate::images::draw_modal(frame, app);
        return;
    }
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
        Modal::Definition(_) | Modal::Image(_) => Vec::new(),
        Modal::Workflow(_) => Vec::new(),
        Modal::Help(_) => Vec::new(),
        Modal::Clone { value, key } => {
            let mut rows = vec![bold("Locate your repository", ACCENT), text("", DIM)];
            rows.extend(prose(&format!("Choose the existing local clone for {key}. Difu remembers it for future reviews."),inner.width as usize));
            rows.push(text("", DIM));
            let (lines, (x, y)) = value.styled_layout(
                inner.width.saturating_sub(4) as usize,
                Style::default().bg(ACCENT).fg(crate::ui::INK),
            );
            input_cursor = Some((inner.x + 2 + x as u16, inner.y + rows.len() as u16));
            let mut spans = vec![Span::raw("> ")];
            if let Some(line) = lines.get(y) {
                spans.extend(line.spans.clone());
            }
            rows.push(TextRow {
                spans,
                ..Default::default()
            });
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
            let (lines, (x, y)) = query.styled_layout(
                inner.width.saturating_sub(9) as usize,
                Style::default().bg(ACCENT).fg(crate::ui::INK),
            );
            input_cursor = Some((inner.x + 8 + x as u16, inner.y + 1));
            let options = app.model_options(&query.text());
            let selected = *selected;
            let mut spans = vec![Span::raw("Filter: ")];
            if let Some(line) = lines.get(y) {
                spans.extend(line.spans.clone());
            }
            let mut rows = vec![
                bold(app.model_purpose.label(), ACCENT),
                TextRow {
                    spans,
                    ..Default::default()
                },
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
                let recommended = *choice == app.model_purpose.recommended();
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
                            category: crate::codex::ChapterCategory::Regular,
                            title: "First chapter".into(),
                            explanation: "Changes across two files.".into(),
                            hunks: vec!["h0".into(), "h1".into()],
                        },
                        Chapter {
                            category: crate::codex::ChapterCategory::Regular,
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
    fn pr_command_filter_keeps_keyboard_mouse_and_cursor_on_matching_commands() -> Result<()> {
        use crate::{
            editor::Editor,
            review::Operation,
            workflow::{Kind, WAction, Wizard},
        };
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let key = app.inbox.first().context("Missing PR")?.key.clone();
        // Opening a composer in this UI test must not start a live GitHub lookup.
        app.workflow.mentions_loading.insert(key.id());
        let open_menu = |app: &mut App| {
            app.wizard(Wizard::Controls {
                draft: Some(false),
                key: key.clone(),
                head: "head".into(),
                selected: 0,
                query: Editor::default(),
            })
        };
        open_menu(&mut app);
        for ch in "ADMIN merge".chars() {
            app.key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(
            crate::workflow::control_commands("ADMIN merge", Some(false))
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 35))?;
        terminal.draw(|f| draw(f, &mut app))?;
        let choices = app
            .hits
            .iter()
            .filter_map(|(_, action)| match action {
                Action::Workflow(WAction::Choose(id)) => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(choices, [3, 4]);
        let cursor = terminal.get_cursor_position()?;
        assert!(cursor.x > 0 && cursor.x < 120 && cursor.y > 0 && cursor.y < 35);
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Confirm {operation: Operation::Merge{squash:true, admin:true},..}))
        );
        assert!(!app.workflow.busy);
        open_menu(&mut app);
        app.paste("resolve\nconflicts".into());
        terminal.draw(|f| draw(f, &mut app))?;
        let action = app
            .hits
            .iter()
            .find_map(|(_, a)| match a {
                Action::Workflow(WAction::Choose(6)) => Some(a.clone()),
                _ => None,
            })
            .context("Missing filtered mouse action")?;
        app.action(action);
        assert!(
            matches!(&app.modal,Some(Modal::Workflow(w)) if matches!(w.as_ref(),Wizard::Resolve {..}))
        );
        assert!(!app.workflow.busy);
        open_menu(&mut app);
        app.paste("no matching command 界".into());
        app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&app.modal,Some(Modal::Workflow(w)) if matches!(w.as_ref(),Wizard::Controls {..}))
        );
        terminal.draw(|f| draw(f, &mut app))?;
        assert!(
            !app.hits
                .iter()
                .any(|(_, a)| matches!(a, Action::Workflow(WAction::Choose(_))))
        );
        app.key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            matches!(&app.modal,Some(Modal::Workflow(w)) if matches!(w.as_ref(),Wizard::Controls {query,..} if query.text().ends_with(' ')))
        );
        app.key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste("close".into());
        app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&app.modal,Some(Modal::Workflow(w)) if matches!(w.as_ref(),Wizard::Compose(draft) if matches!(draft.kind, Kind::Close)))
        );
        for (draft, label) in [(false, "Convert to draft"), (true, "Mark ready for review")] {
            assert_eq!(
                crate::workflow::control_commands(label, Some(draft)).len(),
                1
            );
            assert!(crate::workflow::control_commands(label, Some(!draft)).is_empty());
            assert!(crate::workflow::control_commands(label, None).is_empty());
            app.wizard(Wizard::Controls {
                key: key.clone(),
                head: "head".into(),
                draft: Some(draft),
                selected: 0,
                query: Editor::from(label),
            });
            app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(
                matches!(&app.modal, Some(Modal::Workflow(w)) if matches!(w.as_ref(), Wizard::Confirm { operation: Operation::Draft { draft: next }, .. } if *next != draft))
            );
            assert!(!app.workflow.busy);
        }
        open_menu(&mut app);
        app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());
        Ok(())
    }

    #[test]
    fn section_dividers_render_once_without_becoming_chapters_or_navigation_targets() -> Result<()>
    {
        use crate::codex::{ChapterCategory, Guide};
        let dir = tempfile::tempdir()?;
        for width in [80, 180] {
            let mut app = guide_app(dir.path());
            let id = app.key().context("Missing key")?;
            let r = app.reviews.get_mut(&id).context("Missing review")?;
            let mut chapter = r
                .guide
                .as_ref()
                .and_then(|g| g.chapters.first())
                .cloned()
                .context("Missing chapter")?;
            chapter.hunks = vec!["h0".into()];
            let chapters = [
                ChapterCategory::Schema,
                ChapterCategory::Migrations,
                ChapterCategory::Regular,
                ChapterCategory::Generated,
                ChapterCategory::Generated,
                ChapterCategory::Tests,
                ChapterCategory::Tests,
            ]
            .into_iter()
            .map(|category| {
                let mut c = chapter.clone();
                c.category = category;
                c
            })
            .collect();
            r.guide = Some(std::sync::Arc::new(Guide { chapters }));
            let doc = build(&app, width);
            assert_eq!(doc.sections.len(), 7);
            assert_eq!(doc.navigation.len(), 7);
            let mut positions = Vec::new();
            for label in [
                "── Manual schemas / DTOs ──",
                "── Database migrations ──",
                "── Implementation ──",
                "── Generated code ──",
                "── Tests ──",
            ] {
                let hits = doc
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.left
                            .spans
                            .iter()
                            .chain(&row.right.spans)
                            .any(|s| s.content == label)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(hits.len(), 1);
                let (index, row) = hits.first().context("Missing divider")?;
                assert!(row.left.action.is_none() && row.right.action.is_none());
                assert!(row.right.target.is_none());
                positions.push(*index);
            }
            assert!(
                positions
                    .windows(2)
                    .all(|pair| matches!(pair, [a, b] if a < b))
            );
            for item in &doc.navigation {
                assert!(doc.rows.get(item.row).is_some_and(|row| matches!(&row.right.target,
                    Some(crate::workflow::Target::Header {chapter: Some(index), ..}) if *index == item.chapter)));
            }
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30))?;
            terminal.draw(|frame| draw(frame, &mut app))?;
            let sections = app
                .document
                .as_ref()
                .context("Missing document")?
                .sections
                .iter()
                .map(|section| {
                    (
                        section.start + 10,
                        section
                            .category
                            .spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>(),
                    )
                })
                .collect::<Vec<_>>();
            for (position, category) in sections {
                app.scroll = position;
                terminal.draw(|frame| draw(frame, &mut app))?;
                let header = (app.content_rect.x..app.content_rect.right())
                    .filter_map(|x| {
                        terminal
                            .backend()
                            .buffer()
                            .cell((x, app.content_rect.y - 1))
                    })
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(
                    header.contains(&category),
                    "Missing sticky {category}: {header}"
                );
            }
            let regular_only = guide_app(dir.path());
            let doc = build(&regular_only, width);
            assert!(
                !doc.rows
                    .iter()
                    .flat_map(|row| row.left.spans.iter().chain(&row.right.spans))
                    .any(|span| span.content == "── Generated code ──"
                        || span.content == "── Tests ──")
            );
        }
        Ok(())
    }

    #[test]
    fn directories_select_descendants_and_horizontal_scroll_stays_in_tree() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let id = app.key().context("Missing key")?;
        let snapshot = std::sync::Arc::make_mut(
            app.reviews
                .get_mut(&id)
                .and_then(|r| r.snapshot.as_mut())
                .context("Missing snapshot")?,
        );
        for (file, path) in snapshot.files.iter_mut().zip([
            "src/nested/long-child-file-name.ts",
            "src/root.ts",
            "src-extra/outside.ts",
        ]) {
            file.path = path.into();
        }
        app.action(Action::SetView(View::Diff));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 35))?;
        terminal.draw(|f| draw(f, &mut app))?;
        let directory = app
            .hits
            .iter()
            .find_map(|(_, action)| match action {
                Action::SelectDirectory(path) if path == "src" => Some(action.clone()),
                _ => None,
            })
            .context("Directory must be clickable")?;
        app.action(directory);
        terminal.draw(|f| draw(f, &mut app))?;
        let doc = app.document.as_ref().context("Missing document")?;
        assert_eq!(doc.files.len(), 2);
        let paths = doc
            .rows
            .iter()
            .filter_map(|row| match &row.right.target {
                Some(crate::workflow::Target::Code { path, .. }) => Some(path.as_str()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            paths,
            std::collections::BTreeSet::from(["src/nested/long-child-file-name.ts", "src/root.ts"])
        );
        assert!(
            doc.files
                .windows(2)
                .all(|pair| matches!(pair, [a, b] if a.end == b.start && b.start > a.start))
        );
        let rows = doc.rows.len();
        let epoch = app.epoch;
        for _ in 0..3 {
            app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        }
        assert!(app.tree_horizontal > 0);
        assert_eq!(app.horizontal, 0);
        assert_eq!(app.epoch, epoch);
        terminal.draw(|f| draw(f, &mut app))?;
        assert_eq!(app.document.as_ref().map(|d| d.rows.len()), Some(rows));
        assert!(
            app.hits
                .iter()
                .filter(|(_, a)| matches!(a, Action::SelectFile(_) | Action::SelectDirectory(_)))
                .all(|(r, _)| r.height == 1)
        );
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.directory.as_deref(), Some("src/nested"));
        terminal.draw(|f| draw(f, &mut app))?;
        assert_eq!(app.document.as_ref().map(|d| d.files.len()), Some(1));
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.directory.is_none());
        assert_eq!(app.file, 0);
        app.focus = Focus::Content;
        let tree_offset = app.tree_horizontal;
        app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.horizontal, 4);
        assert_eq!(app.tree_horizontal, tree_offset);
        Ok(())
    }

    #[test]
    fn diff_backgrounds_cover_syntax_and_padding_in_both_layouts() -> Result<()> {
        let lines = [
            DiffLine {
                kind: LineKind::Remove,
                old: Some(1),
                new: None,
                text: "const old = 'value';".into(),
            },
            DiffLine {
                kind: LineKind::Add,
                old: None,
                new: Some(1),
                text: "const new = 'value';".into(),
            },
        ];
        for split in [false, true] {
            for layout in [(0, false), (5, false), (0, true)] {
                let width = 35;
                let rows = code_rows("file.ts", &lines, width, split, layout);
                for row in rows {
                    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
                        width as u16,
                        1,
                    ))?;
                    terminal.draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(Line::from(row.spans.clone())),
                            frame.area(),
                        )
                    })?;
                    let unified_background = if matches!(
                        row.target,
                        Some(crate::workflow::Target::Code { new: None, .. })
                    ) {
                        REMOVE_BG
                    } else {
                        ADD_BG
                    };
                    for x in 0..width as u16 {
                        let expected = if split {
                            match x {
                                0..=16 => REMOVE_BG,
                                17 => Color::Reset,
                                _ => ADD_BG,
                            }
                        } else {
                            unified_background
                        };
                        assert_eq!(
                            terminal.backend().buffer().cell((x, 0)).context("cell")?.bg,
                            expected
                        );
                    }
                }
            }
        }
        assert!(
            code(
                Some(&DiffLine {
                    kind: LineKind::Context,
                    old: Some(2),
                    new: Some(2),
                    text: "unchanged".into()
                }),
                false,
                35,
                0
            )
            .iter()
            .all(|span| span.style.bg == Some(Color::Reset))
        );
        Ok(())
    }
    #[test]
    fn wrapped_code_preserves_complete_text_side_anchors_and_definition_columns() -> Result<()> {
        use crate::workflow::Target;
        let source = "\tconst 界界 = longFunctionName(argumentOne, argumentTwo);";
        let old = DiffLine {
            kind: LineKind::Remove,
            old: Some(42),
            new: None,
            text: source.into(),
        };
        let new = DiffLine {
            kind: LineKind::Add,
            old: None,
            new: Some(50),
            text: "short();".into(),
        };
        let width = 26;
        let offsets = code_offsets(Some(&old), true, width, (0, true));
        let reconstructed: String = offsets
            .iter()
            .map(|offset| crop(source, *offset, width - 8))
            .collect();
        assert_eq!(reconstructed, source.replace('\t', "    "));
        let rows = code_rows("file.ts", &[old.clone(), new], 53, true, (0, true));
        assert!(rows.len() > 1);
        assert!(matches!(
            rows.first().and_then(|r| r.target.as_ref()),
            Some(Target::Code {
                old: Some(42),
                new: Some(50),
                ..
            })
        ));
        assert!(rows.iter().skip(1).all(|r| matches!(
            r.target,
            Some(Target::Code {
                old: Some(42),
                new: None,
                ..
            })
        )));
        let expected_column = source
            .find("longFunctionName")
            .context("Missing function")?;
        assert!(rows.iter().skip(1).flat_map(|r| &r.code_links).any(|link| matches!(link.action, Action::Definition {line:42, column, old:true, ..} if column == expected_column)));
        assert!(
            rows.iter()
                .all(|r| r.spans.iter().map(|s| s.width()).sum::<usize>() <= 53)
        );
        let unified = code_rows("file.ts", &[old], width, false, (0, true));
        assert_eq!(unified.len(), offsets.len());
        assert!(unified.iter().all(|r| matches!(
            r.target,
            Some(Target::Code {
                old: Some(42),
                new: None,
                ..
            })
        )));
        Ok(())
    }

    #[test]
    fn wrapping_toggle_applies_to_both_views_and_persists_without_stealing_input() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        for view in [View::Guide, View::Diff] {
            for width in [80, 180] {
                let mut app = guide_app(dir.path());
                let id = app.key().context("Missing key")?;
                let snapshot = std::sync::Arc::make_mut(
                    app.reviews
                        .get_mut(&id)
                        .and_then(|r| r.snapshot.as_mut())
                        .context("Missing snapshot")?,
                );
                let line = snapshot
                    .files
                    .first_mut()
                    .and_then(|f| f.hunks.first_mut())
                    .and_then(|h| h.lines.first_mut())
                    .context("Missing line")?;
                line.text = "someLongFunctionName(argument); ".repeat(20);
                app.action(Action::SetView(view));
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 35))?;
                terminal.draw(|f| draw(f, &mut app))?;
                let before = app.document.as_ref().context("Missing doc")?.rows.len();
                app.key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
                terminal.draw(|f| draw(f, &mut app))?;
                assert!(
                    app.document
                        .as_ref()
                        .context("Missing wrapped doc")?
                        .rows
                        .len()
                        > before
                );
                assert!(app.storage.load_config()?.wrap_diff);
                let restarted = App::new(app.storage.clone(), app.storage.load_config()?);
                assert!(restarted.config.wrap_diff);
                app.key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
                terminal.draw(|f| draw(f, &mut app))?;
                assert_eq!(app.document.as_ref().map(|d| d.rows.len()), Some(before));
                assert!(!app.storage.load_config()?.wrap_diff);
                app.modal = Some(Modal::Clone {
                    value: Default::default(),
                    key: id,
                });
                app.key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
                assert!(
                    matches!(&app.modal, Some(Modal::Clone {value, ..}) if value.text() == "w")
                );
                assert!(!app.config.wrap_diff);
            }
        }
        Ok(())
    }

    #[test]
    fn default_focus_and_fast_scroll_preserve_shortcut_meanings() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 30))?;
        app.action(Action::SetView(View::Guide));
        assert_eq!(app.focus, Focus::Content);
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
        assert!(output.contains("Focus: Diff"));
        app.key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
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
        let first_chapter = app
            .document
            .as_ref()
            .and_then(|d| d.sections.first())
            .context("Missing chapter")?
            .start;
        assert_eq!(app.scroll, first_chapter);
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(app.workflow.cursor, Some(first_chapter + 10));
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
        assert_eq!(app.scroll, 0);
        assert_eq!(app.file, 1);
        app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(app.file, 0);
        assert_eq!(app.focus, Focus::Content);
        assert_eq!(
            app.workflow.cursor,
            Some(
                app.document
                    .as_ref()
                    .context("Missing previous file")?
                    .rows
                    .len()
                    - 1
            )
        );
        Ok(())
    }

    #[test]
    fn home_model_settings_save_independent_defaults() -> Result<()> {
        use crate::{
            model::{ModelChoice, ModelInfo, ModelPurpose},
            workflow::{WAction, Wizard},
        };
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        app.home = true;
        app.models = vec![ModelInfo {
            id: "gpt-6-astra".into(),
            name: "Astra".into(),
            efforts: vec!["high".into()],
        }];
        app.wizard(Wizard::Home(3));
        app.workflow_action(WAction::Choose(3));
        assert_eq!(app.model_purpose, ModelPurpose::Conflicts);
        let conflict = ModelChoice {
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
        };
        app.action(Action::ApplyModel(conflict.clone()));
        assert_eq!(app.config.model, ModelChoice::default());
        assert_eq!(app.storage.load_config()?.conflict_model, conflict);
        app.wizard(Wizard::Home(2));
        app.workflow_action(WAction::Choose(2));
        assert_eq!(app.model_purpose, ModelPurpose::Guide);
        app.action(Action::ApplyModel(ModelChoice {
            model: "gpt-5.6-sol".into(),
            effort: "low".into(),
        }));
        assert_eq!(app.storage.load_config()?.conflict_model, conflict);
        assert_eq!(app.storage.load_config()?.model.model, "gpt-5.6-sol");
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
                    {"category": "regular", "title": "First", "explanation": "First use", "hunks": ["h0", "h1", "h0"]},
                    {"category": "regular", "title": "Second", "explanation": "Second use", "hunks": ["h0", "h2", "h0"]}
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
            assert_eq!(
                app.scroll,
                app.document
                    .as_ref()
                    .and_then(|d| d.sections.first())
                    .context("Missing chapter")?
                    .start
            );
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
    fn diff_arrows_cross_file_edges_without_leaving_code_or_extending_selection() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        for unified in [false, true] {
            for wrap in [false, true] {
                let mut app = guide_app(dir.path());
                app.view = View::Diff;
                app.focus = Focus::Content;
                app.config.unified = unified;
                app.config.wrap_diff = wrap;
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30))?;
                terminal.draw(|frame| draw(frame, &mut app))?;
                app.workflow.cursor = Some(0);
                app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
                assert_eq!(app.file, 0);
                let last = app.document.as_ref().context("document")?.rows.len() - 1;
                app.workflow.cursor = Some(last - 1);
                app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                assert_eq!(app.file, 0); // Reach the boundary before crossing it.
                assert_eq!(app.workflow.cursor, Some(last));
                app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
                assert_eq!(app.file, 0); // Selection never leaks to another file.
                app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                terminal.draw(|frame| draw(frame, &mut app))?;
                assert_eq!(app.file, 1);
                assert_eq!(app.focus, Focus::Content);
                assert_eq!(app.workflow.cursor, Some(0));
                assert_eq!(app.scroll, 0);
                assert!(app.workflow.selection.is_none());
                app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
                terminal.draw(|frame| draw(frame, &mut app))?;
                assert_eq!(app.file, 0);
                assert_eq!(app.workflow.cursor, Some(last));
                assert_eq!(app.scroll, (last + 1).saturating_sub(app.viewport));
                app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
                assert_eq!(app.file, 1);
                assert_eq!(app.workflow.cursor, Some(0));
                app.action(Action::SelectFile(2));
                app.focus = Focus::Content;
                terminal.draw(|frame| draw(frame, &mut app))?;
                let last = app.document.as_ref().context("document")?.rows.len() - 1;
                app.workflow.cursor = Some(last);
                app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                assert_eq!(app.file, 2);
                assert_eq!(app.workflow.cursor, Some(last));
            }
        }
        Ok(())
    }

    #[test]
    fn diff_file_crossing_follows_filtered_tree_order() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let snapshot = app
            .reviews
            .get_mut("example/repo#1")
            .context("review")?
            .snapshot
            .as_mut()
            .context("snapshot")?;
        std::sync::Arc::make_mut(snapshot)
            .files
            .get_mut(1)
            .context("file")?
            .path = "hidden.rs".into();
        app.filters.files = "distinctive".into();
        app.view = View::Diff;
        app.focus = Focus::Content;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30))?;
        terminal.draw(|frame| draw(frame, &mut app))?;
        app.workflow.cursor = app
            .document
            .as_ref()
            .context("document")?
            .rows
            .len()
            .checked_sub(1);
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.file, 2);
        app.key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.file, 0);
        Ok(())
    }

    #[test]
    fn code_cursor_tracks_the_viewport_center_and_clamps_at_document_ends() -> Result<()> {
        use crate::{review::Side, workflow::Target};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
        let dir = tempfile::tempdir()?;
        for width in [80, 180] {
            for view in [View::Guide, View::Diff] {
                let mut app = guide_app(dir.path());
                app.view = view;
                app.focus = Focus::Content;
                app.workflow.side = Side::Right;
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30))?;
                terminal.draw(|f| draw(f, &mut app))?;
                let row = app
                    .document
                    .as_ref()
                    .context("Missing document")?
                    .rows
                    .iter()
                    .position(|r| {
                        matches!(r.right.target, Some(Target::Code { new: Some(30), .. }))
                    })
                    .context("Missing code line")?;
                app.workflow.cursor = Some(row);
                for (code, modifiers) in [
                    (KeyCode::Down, KeyModifiers::NONE),
                    (KeyCode::Down, KeyModifiers::SUPER),
                    (KeyCode::Up, KeyModifiers::NONE),
                    (KeyCode::Down, KeyModifiers::SHIFT),
                ] {
                    app.key_event(KeyEvent::new(code, modifiers));
                    terminal.draw(|f| draw(f, &mut app))?;
                    assert_eq!(
                        app.workflow.cursor.context("Missing cursor")? - app.scroll,
                        app.viewport / 2
                    );
                }
                assert!(app.workflow.selection.is_some());
                app.mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: app.content_rect.right().saturating_sub(2),
                    row: app.content_rect.y + 2,
                    modifiers: KeyModifiers::NONE,
                });
                terminal.draw(|f| draw(f, &mut app))?;
                assert_eq!(
                    app.workflow.cursor.context("Missing cursor")? - app.scroll,
                    app.viewport / 2
                );
                app.move_diff(-i32::MAX, false);
                assert_eq!(app.scroll, 0);
                app.move_diff(i32::MAX, false);
                let length = app
                    .document
                    .as_ref()
                    .context("Missing document")?
                    .rows
                    .len();
                assert_eq!(app.scroll, length.saturating_sub(app.viewport));
                assert_eq!(app.workflow.cursor, length.checked_sub(1));
            }
        }
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
        app.key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.workflow.side, Side::Left);
        assert_eq!(app.horizontal, 0);
        app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.horizontal, 4);
        let current = app.workflow.cursor;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.workflow.cursor, current);
        Ok(())
    }
    #[test]
    fn arrows_scroll_unwrapped_code_and_alt_arrows_select_sides() -> Result<()> {
        use crate::review::{Anchor, Side};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        for view in [View::Guide, View::Diff] {
            for unified in [false, true] {
                let mut app = guide_app(dir.path());
                assert_eq!(app.workflow.side, Side::Right);
                app.config.unified = unified;
                app.action(Action::SetView(view));
                app.focus = Focus::Content;
                app.workflow.selection = Some(Anchor {
                    path: "file".into(),
                    side: Side::Right,
                    start: 1,
                    end: 2,
                });
                app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
                assert_eq!(app.horizontal, 4);
                assert_eq!(app.workflow.side, Side::Right);
                assert!(app.workflow.selection.is_some());
                app.key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
                assert_eq!(app.workflow.side, Side::Left);
                assert_eq!(app.horizontal, 4);
                assert!(app.workflow.selection.is_none());
                app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
                assert_eq!(app.workflow.side, Side::Right);
                assert_eq!(app.horizontal, 4);
                app.key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
                assert_eq!(app.horizontal, 0);
                app.config.wrap_diff = true;
                app.key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
                assert_eq!(app.horizontal, 0);
                app.key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
                assert_eq!(app.workflow.side, Side::Left);
            }
        }
        Ok(())
    }

    #[test]
    fn searchable_help_filters_keys_and_descriptions_without_triggering_actions() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        app.action(Action::Help);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))?;
        terminal.draw(|frame| draw(frame, &mut app))?;
        app.paste("COPY".into());
        terminal.draw(|frame| draw(frame, &mut app))?;
        assert_eq!(crate::help::entries("COPY").len(), 1);
        assert_eq!(crate::help::entries("Cmd+C").len(), 1);
        assert!(app.clipboard.is_none());
        app.key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        terminal.draw(|frame| draw(frame, &mut app))?;
        app.key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(&app.modal, Some(Modal::Help(state)) if state.scroll == 1 && state.query.text().is_empty())
        );
        app.paste("no such shortcut".into());
        terminal.draw(|frame| draw(frame, &mut app))?;
        assert!(matches!(&app.modal, Some(Modal::Help(state)) if state.scroll == 0));
        app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());
        assert!(!app.home);
        Ok(())
    }

    #[test]
    fn copy_shortcuts_preserve_review_position_and_copy_whole_wrapped_source_lines() -> Result<()> {
        use crate::{
            review::{Anchor, Side},
            workflow::Target,
        };
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        for view in [View::Guide, View::Diff] {
            for unified in [false, true] {
                let mut app = guide_app(dir.path());
                let id = app.key().context("Missing PR")?;
                let snapshot = std::sync::Arc::make_mut(
                    app.reviews
                        .get_mut(&id)
                        .and_then(|r| r.snapshot.as_mut())
                        .context("Missing snapshot")?,
                );
                let source = format!("\t界 {}  ", "long_source_word ".repeat(40));
                snapshot
                    .files
                    .first_mut()
                    .and_then(|f| f.hunks.first_mut())
                    .and_then(|h| h.lines.first_mut())
                    .context("Missing source")?
                    .text = source.clone();
                app.config.unified = unified;
                app.config.wrap_diff = true;
                app.action(Action::SetView(view));
                app.focus = Focus::Content;
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 35))?;
                terminal.draw(|frame| draw(frame, &mut app))?;
                let doc = app.document.as_ref().context("Missing document")?;
                let (index, path) = doc
                    .rows
                    .iter()
                    .enumerate()
                    .find_map(|(index, row)| match &row.right.target {
                        Some(Target::Code {
                            path, new: Some(3), ..
                        }) => Some((index, path.clone())),
                        _ => None,
                    })
                    .context("Missing code")?;
                app.workflow.cursor = Some(index);
                app.workflow.side = Side::Right;
                app.workflow.selection = Some(Anchor {
                    path: path.clone(),
                    side: Side::Right,
                    start: 1,
                    end: 1,
                });
                let scroll = app.scroll;
                for modifier in [KeyModifiers::NONE, KeyModifiers::SUPER] {
                    app.key_event(KeyEvent::new(KeyCode::Char('c'), modifier));
                    assert_eq!(
                        app.clipboard.as_deref(),
                        Some(format!("{source}\nline_2\nline_3").as_str())
                    );
                    assert_eq!(app.workflow.cursor, Some(index));
                    assert_eq!(app.workflow.selection.as_ref().map(|a| a.start), Some(1));
                    assert_eq!(app.scroll, scroll);
                    assert_eq!(app.focus, Focus::Content);
                }
                app.workflow.selection = None;
                app.key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
                assert_eq!(app.clipboard.as_deref(), Some("line_3"));
                app.workflow.cursor = app.document.as_ref().and_then(|doc| {
                    doc.rows
                        .iter()
                        .position(|row| matches!(row.right.target, Some(Target::Header { .. })))
                });
                app.key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
                assert_eq!(app.clipboard.as_deref(), Some(path.as_str()));
            }
        }
        Ok(())
    }

    #[test]
    fn sidebar_filters_keep_shortcuts_as_text_and_hide_nonmatching_file_content() -> Result<()> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        app.action(Action::SetView(View::Diff));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 35))?;
        app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        for c in "r[]*界".chars() {
            app.key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.filters.files.text(), "r[]*界");
        assert!(app.modal.is_none());
        terminal.draw(|frame| draw(frame, &mut app))?;
        assert!(
            app.document
                .as_ref()
                .context("Missing filtered document")?
                .files
                .is_empty()
        );
        assert_eq!(app.filters.focused, Some(crate::filter::Kind::Files));
        app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.home);
        assert_eq!(app.filters.files.text(), "r[]*界");
        app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app))?;
        assert!(app.filters.files.text().is_empty());
        assert!(
            !app.document
                .as_ref()
                .context("Missing unfiltered document")?
                .files
                .is_empty()
        );
        assert_eq!(app.filters.focused, None);
        app.action(Action::SetView(View::Guide));
        app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.filters.focused, None);
        Ok(())
    }

    #[test]
    fn character_shortcuts_keep_text_inputs_isolated() -> Result<()> {
        use crate::{
            editor::Editor,
            model::ModelInfo,
            process::Cancel,
            workflow::{Compose, Kind, Wizard},
        };
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir()?;
        let mut app = guide_app(dir.path());
        let key = app.inbox.first().context("Missing PR")?.key.clone();
        let cancel = Cancel::default();
        app.reviews
            .get_mut(&key.id())
            .context("Missing review")?
            .preparation = Some(cancel.clone());
        app.models = vec![ModelInfo {
            id: "gpt-5.6-luna".into(),
            name: "Luna".into(),
            efforts: vec!["high".into()],
        }];
        for code in [KeyCode::Char('?'), KeyCode::Char('m'), KeyCode::Char('l')] {
            app.key_event(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(match code {
                KeyCode::Char('?') => matches!(app.modal, Some(Modal::Help(_))),
                KeyCode::Char('m') => matches!(app.modal, Some(Modal::Models { .. })),
                _ => matches!(app.modal, Some(Modal::Clone { .. })),
            });
            app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        app.home = true;
        app.inbox_tab = InboxTab::Repositories;
        app.key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.filters.focused, Some(crate::filter::Kind::Repositories));
        app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.inbox_tab = InboxTab::MyPrs;
        let letters = "?msfFrglx/";
        for modal in [
            Modal::Clone {
                value: Default::default(),
                key: key.id(),
            },
            Modal::Models {
                selected: 0,
                effort: 0,
                query: Default::default(),
            },
            Modal::Workflow(Box::new(Wizard::Compose(Compose {
                key: key.clone(),
                head: "head".into(),
                kind: Kind::Review,
                editor: Editor::default(),
                choice: 0,
                focus: 0,
                mention: 0,
            }))),
        ] {
            app.modal = Some(modal);
            for c in letters.chars() {
                app.key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            let text = match app
                .modal
                .as_ref()
                .context("Input was replaced by a shortcut")?
            {
                Modal::Clone { value, .. } => value.text(),
                Modal::Models { query, .. } => query.text(),
                Modal::Workflow(w) => match w.as_ref() {
                    Wizard::Compose(draft) => draft.editor.text(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            assert_eq!(text, letters);
            assert!(!cancel.cancelled());
        }
        app.modal = None;
        app.key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(!cancel.cancelled());
        app.key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(cancel.cancelled());
        let conflict = Cancel::default();
        app.workflow.busy = true;
        app.workflow.conflict_cancel = Some(conflict.clone());
        app.wizard(Wizard::Resolving {
            activity: "Resolving".into(),
        });
        app.key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(conflict.cancelled());
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
        ] {
            app.modal = Some(modal);
            terminal.draw(|f| draw(f, &mut app))?;
            let cursor = terminal.get_cursor_position()?;
            assert!(cursor.x > 0 && cursor.y > 3);
        }
        Ok(())
    }
}
