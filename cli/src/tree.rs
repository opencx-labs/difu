use crate::diff::DiffFile;

/// A visible entry in the fully expanded changed-files tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub depth: usize,
    pub file: Option<usize>,
}
impl Entry {
    pub fn label(&self) -> String {
        format!(
            "{}{}{}",
            "  ".repeat(self.depth),
            self.path.rsplit('/').next().unwrap_or_default(),
            if self.file.is_none() { "/" } else { "" }
        )
    }
}

pub fn entries(files: &[DiffFile]) -> Vec<Entry> {
    let mut paths = files
        .iter()
        .enumerate()
        .map(|(index, file)| (index, file.path.split('/').collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    // Compare components so a directory's descendants stay together even when
    // similarly named siblings (e.g. src.ts and src/) are present.
    paths.sort_by(|(_, a), (_, b)| a.cmp(b));
    let mut previous = Vec::<&str>::new();
    let mut entries = Vec::new();
    for (index, parts) in paths {
        let Some((_, parents)) = parts.split_last() else {
            continue;
        };
        let common = parents
            .iter()
            .zip(&previous)
            .take_while(|(a, b)| a == b)
            .count();
        let mut path = String::new();
        for (depth, part) in parents.iter().enumerate() {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            if depth >= common {
                entries.push(Entry {
                    path: path.clone(),
                    depth,
                    file: None,
                });
            }
        }
        entries.push(Entry {
            path: parts.join("/"),
            depth: parents.len(),
            file: Some(index),
        });
        previous = parents.to_vec();
    }
    entries
}

/// Include every matching path and the ancestors needed to keep its hierarchy.
/// Matching a directory also matches the full paths of all its descendants.
pub fn filtered(files: &[DiffFile], query: &str) -> Vec<Entry> {
    let entries = entries(files);
    if query.is_empty() {
        return entries;
    }
    let mut visible = std::collections::HashSet::new();
    for entry in &entries {
        if crate::filter::matches(query, &entry.path) {
            visible.insert(entry.path.as_str());
            let mut path = entry.path.as_str();
            while let Some((parent, _)) = path.rsplit_once('/') {
                visible.insert(parent);
                path = parent;
            }
        }
    }
    entries
        .iter()
        .filter(|entry| visible.contains(entry.path.as_str()))
        .cloned()
        .collect()
}

/// Resolve focus by identity, so refreshes and reordered files do not move it.
pub(crate) fn selected(entries: &[Entry], file: usize, directory: Option<&str>) -> usize {
    entries
        .iter()
        .position(|entry| match directory {
            Some(path) => entry.file.is_none() && entry.path == path,
            None => entry.file == Some(file),
        })
        .unwrap_or(0)
}
pub(crate) fn step(entries: &[Entry], current: usize, delta: i32) -> usize {
    current
        .saturating_add_signed(delta as isize)
        .min(entries.len().saturating_sub(1))
}
pub(crate) fn next_file(entries: &[Entry], file: usize, forward: bool) -> Option<usize> {
    let files = entries
        .iter()
        .filter_map(|entry| entry.file)
        .collect::<Vec<_>>();
    let current = files.iter().position(|index| *index == file)?;
    let next = if forward {
        current.checked_add(1)?
    } else {
        current.checked_sub(1)?
    };
    files.get(next).copied()
}
pub(crate) fn contains(directory: &str, path: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Shared changed-files tree used by PR reviews and live agent changes.
pub(crate) fn draw(
    frame: &mut ratatui::Frame,
    rect: ratatui::layout::Rect,
    entries: &[Entry],
    statuses: &[String],
    selected: usize,
    horizontal: &mut usize,
) -> Vec<(ratatui::layout::Rect, usize)> {
    use crate::ui::{ACCENT, DIM, TEXT};
    use ratatui::{style::Style, widgets::Paragraph};
    use unicode_width::UnicodeWidthStr;
    let width = usize::from(rect.width.saturating_sub(2));
    let labels = entries
        .iter()
        .map(|entry| {
            format!(
                "{}{}",
                entry
                    .file
                    .and_then(|i| statuses.get(i))
                    .map(|s| format!("{} ", crate::local_diff::status_letter(s)))
                    .unwrap_or_default(),
                entry.label()
            )
        })
        .collect::<Vec<_>>();
    let max = labels
        .iter()
        .map(|label| label.width().saturating_sub(width))
        .max()
        .unwrap_or(0);
    *horizontal = (*horizontal).min(max);
    let height = usize::from(rect.height);
    let start = selected.saturating_sub(height.saturating_sub(1));
    let mut hits = Vec::new();
    for (index, (entry, label)) in entries
        .iter()
        .zip(labels)
        .enumerate()
        .skip(start)
        .take(height)
    {
        let active = index == selected;
        let row =
            ratatui::layout::Rect::new(rect.x, rect.y + (index - start) as u16, rect.width, 1);
        let text = format!(
            "{} {}",
            if active { "▸" } else { " " },
            crate::ui::crop(&crate::model::clean(&label), *horizontal, width)
        );
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(if active {
                ACCENT
            } else if entry.file.is_some() {
                TEXT
            } else {
                DIM
            })),
            row,
        );
        hits.push((row, index));
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtering_retains_ancestors_and_directory_children_without_sibling_prefixes() {
        let files = [
            "src/nested/one.ts",
            "src/nested/two.ts",
            "src-other/three.ts",
            "界/four.ts",
        ]
        .into_iter()
        .map(|path| DiffFile {
            path: path.into(),
            old_path: path.into(),
            status: "modified".into(),
            hunks: vec![],
            additions: 0,
            deletions: 0,
        })
        .collect::<Vec<_>>();
        let paths = |query| {
            filtered(&files, query)
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(paths("ONE.TS"), ["src", "src/nested", "src/nested/one.ts"]);
        assert_eq!(
            paths("NESTED"),
            [
                "src",
                "src/nested",
                "src/nested/one.ts",
                "src/nested/two.ts"
            ]
        );
        assert_eq!(paths("界"), ["界", "界/four.ts"]);
        assert!(paths("absent").is_empty());
        assert_eq!(filtered(&files, ""), entries(&files));
    }
}
