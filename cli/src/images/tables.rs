//! Keep image previews in their Markdown or HTML table cells.
use super::{Reference, State, fragment_rows, references};
use crate::{
    model::PrDetail,
    ui::{TextRow, prose},
};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use ratatui::{style::Modifier, text::Span};
use scraper::{ElementRef, Html, Selector};
use std::ops::Range;

struct Cell {
    source: String,
    references: Vec<Reference>,
}
impl Cell {
    fn new(source: &str, range: Range<usize>, references: &[Reference]) -> Self {
        let value = source.get(range.clone()).unwrap_or_default();
        let start = range.start + value.len() - value.trim_start().len();
        let value = value.trim();
        Self {
            source: value.to_owned(),
            references: references
                .iter()
                .filter(|reference| {
                    reference.range.start >= start && reference.range.end <= start + value.len()
                })
                .cloned()
                .map(|mut reference| {
                    reference.range =
                        (reference.range.start - start)..(reference.range.end - start);
                    reference
                })
                .collect(),
        }
    }
    fn rows(&self, width: usize, pr: &PrDetail, images: &State) -> Vec<TextRow> {
        fragment_rows(
            &self.source,
            0..self.source.len(),
            &self.references,
            width,
            pr,
            images,
        )
        .into_iter()
        .flat_map(|row| {
            if row.images.is_empty() && row.spans.iter().map(Span::width).sum::<usize>() > width {
                crate::markdown::wrap(row.spans, width)
                    .into_iter()
                    .map(|mut wrapped| {
                        wrapped.action = row.action.clone();
                        wrapped
                    })
                    .collect()
            } else {
                vec![row]
            }
        })
        .collect()
    }
}

pub(super) struct Table {
    pub range: Range<usize>,
    cells: Vec<Vec<Cell>>,
    align: Vec<Alignment>,
    header: bool,
}
impl Table {
    fn has_images(&self) -> bool {
        self.cells
            .iter()
            .flatten()
            .any(|cell| !cell.references.is_empty())
    }
    pub fn rows(&self, width: usize, pr: &PrDetail, images: &State) -> Vec<TextRow> {
        let count = self.align.len();
        if count == 0 {
            return Vec::new();
        }
        let available = width.saturating_sub(count * 3 + 1).max(count);
        let mut sizes = (0..count)
            .map(|column| {
                self.cells
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| {
                        if !cell.references.is_empty() {
                            available
                        } else {
                            prose(&cell.source, available)
                                .iter()
                                .map(|row| row.spans.iter().map(Span::width).sum::<usize>())
                                .max()
                                .unwrap_or(1)
                        }
                    })
                    .max()
                    .unwrap_or(1)
                    .max(1)
            })
            .collect::<Vec<_>>();
        while sizes.iter().sum::<usize>() > available {
            let Some((column, size)) = sizes.iter().enumerate().max_by_key(|(_, size)| **size)
            else {
                break;
            };
            if *size <= 1 {
                break;
            }
            if let Some(size) = sizes.get_mut(column) {
                *size -= 1;
            }
        }
        let cells = self
            .cells
            .iter()
            .enumerate()
            .map(|(index, row)| {
                sizes
                    .iter()
                    .enumerate()
                    .map(|(column, size)| {
                        let mut rows = row
                            .get(column)
                            .map(|cell| cell.rows(*size, pr, images))
                            .unwrap_or_default();
                        if index == 0 && self.header {
                            for span in rows.iter_mut().flat_map(|row| &mut row.spans) {
                                span.style = span.style.add_modifier(Modifier::BOLD);
                            }
                        }
                        rows
                    })
                    .collect()
            })
            .collect();
        crate::markdown::table_rows(cells, &sizes, &self.align, width, self.header)
    }
}

pub(super) fn find(source: &str, references: &[Reference]) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut table = None;
    let mut cells = Vec::new();
    let mut html = Vec::new();
    for (event, range) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::Table(align)) => {
                table = Some(Table {
                    range,
                    cells: Vec::new(),
                    align,
                    header: true,
                })
            }
            Event::End(TagEnd::TableCell) if table.is_some() => {
                cells.push(Cell::new(source, range, references))
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(table) = &mut table {
                    table.cells.push(std::mem::take(&mut cells));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = table.take().filter(Table::has_images) {
                    tables.push(table);
                }
            }
            Event::Html(_) | Event::InlineHtml(_) => html.push(range),
            _ => {}
        }
    }
    for range in html_table_ranges(source, &html) {
        if let Some(table) = html_table(source, range).filter(Table::has_images) {
            tables.push(table);
        }
    }
    tables.sort_by_key(|table| table.range.start);
    // An HTML table can contain Markdown tables; the outer table owns its cells.
    let mut end = 0;
    tables.retain(|table| {
        if table.range.start < end {
            return false;
        }
        end = table.range.end;
        true
    });
    tables
}

fn html_table_ranges(source: &str, html: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut consumed = 0;
    let mut start = None;
    let mut depth = 0usize;
    let mut literal = None;
    for (offset, _) in source.match_indices('<') {
        if offset < consumed || !html.iter().any(|range| range.contains(&offset)) {
            continue;
        }
        let Some(rest) = source.get(offset..) else {
            continue;
        };
        if rest.starts_with("<!--") {
            consumed = rest
                .find("-->")
                .map(|end| offset + end + 3)
                .unwrap_or(source.len());
            continue;
        }
        let mut quote = None;
        let end = rest.char_indices().skip(1).find_map(|(index, ch)| {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() && matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if quote.is_none() && ch == '>' {
                return Some(index);
            }
            None
        });
        let Some(end) = end else { continue };
        consumed = offset + end + 1;
        let tag = rest.get(1..end).unwrap_or_default().trim();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if let Some(literal_name) = &literal {
            if closing && literal_name == &name {
                literal = None;
            }
            continue;
        }
        if !closing && matches!(name.as_str(), "pre" | "code" | "script" | "style") {
            literal = Some(name);
            continue;
        }
        if name != "table" {
            continue;
        }
        if closing {
            depth = depth.saturating_sub(1);
            if depth == 0
                && let Some(start) = start.take()
            {
                ranges.push(start..consumed);
            }
        } else {
            if depth == 0 {
                start = Some(offset);
            }
            depth += 1;
        }
    }
    ranges
}

fn html_table(source: &str, range: Range<usize>) -> Option<Table> {
    let fragment = Html::parse_fragment(source.get(range.clone())?);
    let table = fragment.select(&Selector::parse("table").ok()?).next()?;
    let mut cells = Vec::new();
    let mut align = Vec::new();
    let mut header = false;
    for row in table.select(&Selector::parse("tr").ok()?).filter(|row| {
        row.ancestors()
            .filter_map(ElementRef::wrap)
            .find(|node| node.value().name() == "table")
            .is_some_and(|parent| parent.id() == table.id())
    }) {
        let mut columns = Vec::new();
        for cell in row
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|node| matches!(node.value().name(), "td" | "th"))
        {
            if cells.is_empty() {
                header |= cell.value().name() == "th";
                align.push(match cell.value().attr("align") {
                    Some("right") => Alignment::Right,
                    Some("center") => Alignment::Center,
                    Some("left") => Alignment::Left,
                    _ => Alignment::None,
                });
            }
            let source = cell.inner_html();
            columns.push(Cell::new(&source, 0..source.len(), &references(&source)));
        }
        if !columns.is_empty() {
            cells.push(columns);
        }
    }
    align.resize(
        cells.iter().map(Vec::len).max().unwrap_or(0),
        Alignment::None,
    );
    Some(Table {
        range,
        cells,
        align,
        header,
    })
}
