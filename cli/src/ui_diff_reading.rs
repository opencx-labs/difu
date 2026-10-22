//! Source reading intent survives presentation clamps until explicit navigation.
use super::{Document, TextRow};
use crate::{review::Side, workflow::Target};

pub(crate) struct Reading {
    cursor: Option<Anchor>,
    scroll: Option<Anchor>,
}
struct Anchor {
    target: Target,
    section: Option<usize>,
    hunk: Option<String>,
    occurrence: usize,
    before: usize,
    side: Side,
}
impl Anchor {
    fn capture(doc: &Document, position: usize, side: Side) -> Option<Self> {
        let (index, row) = doc
            .rows
            .iter()
            .enumerate()
            .skip(position)
            .find(|(_, r)| r.right.target.is_some())?;
        let section = doc
            .sections
            .iter()
            .position(|s| position >= s.start && position < s.end);
        let start = section
            .and_then(|i| doc.sections.get(i))
            .map_or(0, |s| s.start);
        let target = row.right.target.clone()?;
        let occurrence = doc
            .rows
            .iter()
            .take(index)
            .skip(start)
            .filter(|r| r.right.target.as_ref() == Some(&target))
            .count();
        Some(Self {
            target,
            section,
            hunk: row.right.hunk.clone(),
            occurrence,
            before: index - position,
            side,
        })
    }
    fn matches(&self, row: &TextRow) -> bool {
        if row.hunk != self.hunk {
            return false;
        }
        if row.target.as_ref() == Some(&self.target) {
            return true;
        }
        self.target
            .line(self.side)
            .zip(row.target.as_ref().and_then(|t| t.line(self.side)))
            .is_some_and(|(a, b)| a.path == b.path && a.start == b.start)
    }
    fn project(&self, doc: &Document) -> Option<usize> {
        let (start, end) = self
            .section
            .and_then(|i| doc.sections.get(i))
            .map_or((0, doc.rows.len()), |s| (s.start, s.end));
        let matches = doc
            .rows
            .iter()
            .enumerate()
            .take(end)
            .skip(start)
            .filter(|(_, r)| self.matches(&r.right))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        matches
            .get(self.occurrence)
            .or_else(|| matches.last())
            .copied()
            .map(|i| i.saturating_sub(self.before))
            .or_else(|| {
                // A hidden context line stays the intent. Only its current display
                // falls back to the closest visible line in the same canonical hunk.
                let line = self.target.line(self.side)?;
                doc.rows
                    .iter()
                    .enumerate()
                    .take(end)
                    .skip(start)
                    .filter(|(_, r)| r.right.hunk == self.hunk)
                    .filter_map(|(i, r)| {
                        r.right
                            .target
                            .as_ref()?
                            .line(self.side)
                            .filter(|a| a.path == line.path)
                            .map(|a| (i, a.start.abs_diff(line.start)))
                    })
                    .min_by_key(|(_, distance)| *distance)
                    .map(|(i, _)| i)
            })
    }
}
impl Reading {
    pub(super) fn capture(
        doc: &Document,
        cursor: Option<usize>,
        scroll: usize,
        side: Side,
    ) -> Self {
        Self {
            cursor: cursor.and_then(|i| Anchor::capture(doc, i, side)),
            scroll: Anchor::capture(doc, scroll, side),
        }
    }
    pub(super) fn project(&self, doc: &Document) -> (Option<usize>, Option<usize>) {
        (
            self.cursor.as_ref().and_then(|a| a.project(doc)),
            self.scroll.as_ref().and_then(|a| a.project(doc)),
        )
    }
    pub(crate) fn target(&self) -> Option<Target> {
        self.cursor
            .as_ref()
            .or(self.scroll.as_ref())
            .map(|a| a.target.clone())
    }
}
