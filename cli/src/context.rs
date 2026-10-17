use crate::diff::{DiffFile, DiffLine};
use anyhow::{Context, Result, ensure};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Expansion {
    pub above: usize,
    pub below: usize,
}

#[derive(Default)]
pub struct FileState {
    pub data: Option<Arc<FileContext>>,
    pub pending: Vec<(String, Direction, i32)>,
    pub error: Option<String>,
}

pub struct FileContext {
    pub lines: Vec<DiffLine>,
    ranges: Vec<(String, Range<usize>)>,
}

pub struct Part<'a> {
    pub owner: Option<&'a str>,
    pub lines: &'a [DiffLine],
}

impl FileContext {
    pub fn new(file: &DiffFile, lines: Vec<DiffLine>) -> Result<Self> {
        let mut ranges = Vec::new();
        let mut cursor = 0;
        for hunk in file.hunks.iter().filter(|h| h.header.starts_with("@@ ")) {
            ensure!(!hunk.lines.is_empty(), "Cannot expand an empty hunk");
            let start = lines
                .windows(hunk.lines.len())
                .enumerate()
                .skip(cursor)
                .find_map(|(index, candidate)| (candidate == hunk.lines).then_some(index))
                .context("Expanded context does not match the pinned hunk")?;
            let end = start + hunk.lines.len();
            ranges.push((hunk.id.clone(), start..end));
            cursor = end;
        }
        Ok(Self { lines, ranges })
    }

    fn original(&self, id: &str) -> Option<Range<usize>> {
        self.ranges
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, range)| range.clone())
    }

    pub fn visible(&self, id: &str, expansion: Expansion) -> Option<Range<usize>> {
        let original = self.original(id)?;
        Some(
            original.start.saturating_sub(expansion.above)
                ..original
                    .end
                    .saturating_add(expansion.below)
                    .min(self.lines.len()),
        )
    }

    pub fn can_expand(&self, id: &str, expansion: Expansion, direction: Direction) -> bool {
        self.visible(id, expansion)
            .is_some_and(|range| match direction {
                Direction::Above => range.start > 0,
                Direction::Below => range.end < self.lines.len(),
            })
    }

    pub fn expand(&self, id: &str, expansion: &mut Expansion, direction: Direction) {
        self.adjust(id, expansion, direction, 10);
    }

    pub fn adjust(&self, id: &str, expansion: &mut Expansion, direction: Direction, amount: i32) {
        if let Some(original) = self.original(id) {
            let (value, limit) = match direction {
                Direction::Above => (&mut expansion.above, original.start),
                Direction::Below => (
                    &mut expansion.below,
                    self.lines.len().saturating_sub(original.end),
                ),
            };
            *value = value.saturating_add_signed(amount as isize).min(limit);
        }
    }

    pub fn parts(&self, id: &str, expansion: Expansion) -> Vec<Part<'_>> {
        let Some(visible) = self.visible(id, expansion) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        let mut start = visible.start;
        while start < visible.end {
            let owner = self.ranges.iter().find(|(_, range)| range.contains(&start));
            let end = owner
                .map(|(_, range)| range.end)
                .unwrap_or_else(|| {
                    self.ranges
                        .iter()
                        .find(|(_, range)| range.start > start)
                        .map(|(_, range)| range.start)
                        .unwrap_or(visible.end)
                })
                .min(visible.end);
            if let Some(lines) = self.lines.get(start..end) {
                result.push(Part {
                    owner: owner.map(|(id, _)| id.as_str()),
                    lines,
                });
            }
            start = end;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Hunk, LineKind};

    fn fixture() -> Result<(DiffFile, FileContext)> {
        let lines = (1..=60)
            .map(|n| DiffLine {
                kind: LineKind::Context,
                old: Some(n),
                new: Some(n),
                text: format!("line {n}"),
            })
            .collect::<Vec<_>>();
        let mut file = DiffFile {
            path: "file".into(),
            old_path: "file".into(),
            status: "M".into(),
            additions: 0,
            deletions: 0,
            hunks: Vec::new(),
        };
        for (id, range) in [("first", 10..15), ("second", 22..27)] {
            file.hunks.push(Hunk {
                id: id.into(),
                header: "@@ context @@".into(),
                lines: lines.get(range).context("Missing fixture range")?.to_vec(),
            });
        }
        let context = FileContext::new(&file, lines)?;
        Ok((file, context))
    }

    #[test]
    fn expansion_is_per_hunk_in_steps_of_ten_and_crosses_neighbors() -> Result<()> {
        let (_, context) = fixture()?;
        let mut first = Expansion::default();
        context.expand("first", &mut first, Direction::Below);
        assert_eq!(context.visible("first", first), Some(10..25));
        assert_eq!(
            context.visible("second", Expansion::default()),
            Some(22..27)
        );
        let parts = context.parts("first", first);
        assert_eq!(
            parts.iter().map(|p| p.owner).collect::<Vec<_>>(),
            [Some("first"), None, Some("second")]
        );
        assert_eq!(parts.last().context("Missing neighbor")?.lines.len(), 3);
        context.expand("first", &mut first, Direction::Below);
        assert_eq!(context.visible("first", first), Some(10..35));
        let mut second = Expansion::default();
        context.expand("second", &mut second, Direction::Above);
        assert_eq!(
            context
                .parts("second", second)
                .first()
                .context("Missing previous hunk")?
                .owner,
            Some("first")
        );
        assert_eq!(context.visible("first", first), Some(10..35));
        Ok(())
    }

    #[test]
    fn expansion_clamps_to_file_edges_and_rejects_mismatched_context() -> Result<()> {
        let (file, context) = fixture()?;
        let mut expanded = Expansion::default();
        for _ in 0..10 {
            context.expand("first", &mut expanded, Direction::Above);
            context.expand("first", &mut expanded, Direction::Below);
        }
        assert_eq!(context.visible("first", expanded), Some(0..60));
        assert!(!context.can_expand("first", expanded, Direction::Above));
        assert!(!context.can_expand("first", expanded, Direction::Below));
        assert!(FileContext::new(&file, Vec::new()).is_err());
        assert!(context.parts("missing", expanded).is_empty());
        Ok(())
    }
}
