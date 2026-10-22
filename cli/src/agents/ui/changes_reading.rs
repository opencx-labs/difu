use super::super::{Position, patch::SourceRow};
use std::collections::HashSet;

pub(super) struct Reading {
    cursor: Option<Anchor>,
    selection: Option<Anchor>,
    pub(super) copy: Option<String>,
    pub(super) projected: (usize, Option<usize>),
}
struct Anchor {
    source: usize,
    continuation: usize,
}
fn has_source(row: &SourceRow, source: usize) -> bool {
    row.source
        .iter()
        .chain(&row.paired_source)
        .any(|(id, _)| *id == source)
}
impl Anchor {
    fn capture(rows: &[SourceRow], index: usize) -> Option<Self> {
        let source = rows.get(index)?.source.as_ref()?.0;
        Some(Self {
            source,
            continuation: rows
                .iter()
                .take(index)
                .rev()
                .take_while(|row| has_source(row, source))
                .count(),
        })
    }
    fn project(&self, rows: &[SourceRow]) -> Option<usize> {
        let matches = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| has_source(row, self.source))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        matches
            .get(self.continuation)
            .or_else(|| matches.last())
            .copied()
    }
}
pub(super) fn copy(rows: &[SourceRow], position: &Position) -> Option<String> {
    let start = position
        .selection
        .unwrap_or(position.changes)
        .min(position.changes);
    let end = position
        .selection
        .unwrap_or(position.changes)
        .max(position.changes);
    let mut seen = HashSet::new();
    let text = rows
        .iter()
        .skip(start)
        .take(end.saturating_sub(start) + 1)
        .flat_map(|row| row.paired_source.iter().chain(&row.source))
        .filter(|(id, _)| seen.insert(*id))
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>();
    (!text.is_empty()).then(|| text.join("\n"))
}
impl Reading {
    pub(super) fn capture(rows: &[SourceRow], position: &Position) -> Self {
        Self {
            cursor: Anchor::capture(rows, position.changes),
            selection: position.selection.and_then(|i| Anchor::capture(rows, i)),
            copy: copy(rows, position),
            projected: (position.changes, position.selection),
        }
    }
    pub(super) fn project(&mut self, rows: &[SourceRow], position: &mut Position) {
        position.changes = self
            .cursor
            .as_ref()
            .and_then(|a| a.project(rows))
            .unwrap_or(position.changes)
            .min(rows.len().saturating_sub(1));
        if position.selection.is_some() {
            position.selection = Some(
                self.selection
                    .as_ref()
                    .and_then(|a| a.project(rows))
                    .unwrap_or(position.changes),
            );
        }
        self.projected = (position.changes, position.selection);
    }
}
