use super::*;
use ratatui::style::Modifier;
use std::sync::Arc;

pub struct File {
    pub path: String,
    pub patch: String,
    pub diff: Result<crate::diff::DiffFile, String>,
}
pub struct Document {
    pub files: Vec<File>,
    pub tree: Vec<crate::tree::Entry>,
    cache: Option<Rendered>,
}
struct Rendered {
    key: (usize, Option<String>, usize, u16, bool),
    rows: Arc<Vec<super::patch::SourceRow>>,
}
fn unquote(path: &str) -> String {
    let path = path.trim_end_matches('\t');
    let Some(quoted) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_owned();
    };
    let mut bytes = quoted.bytes().peekable();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let Some(escaped) = bytes.next() else {
            break;
        };
        if (b'0'..=b'7').contains(&escaped) {
            let mut value = u16::from(escaped - b'0');
            for _ in 0..2 {
                if let Some(digit) = bytes
                    .peek()
                    .copied()
                    .filter(|digit| (b'0'..=b'7').contains(digit))
                {
                    bytes.next();
                    value = value * 8 + u16::from(digit - b'0');
                }
            }
            decoded.push(value as u8);
        } else {
            decoded.push(match escaped {
                b'n' => b'\n',
                b't' => b'\t',
                b'r' => b'\r',
                b'b' => 8,
                b'f' => 12,
                b'v' => 11,
                b'a' => 7,
                other => other,
            });
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
fn file_path(section: &str) -> String {
    let metadata = section
        .lines()
        .take_while(|line| !line.starts_with("@@"))
        .collect::<Vec<_>>();
    for prefix in ["rename to ", "+++ ", "--- "] {
        if let Some(path) = metadata.iter().find_map(|line| line.strip_prefix(prefix)) {
            let path = unquote(path);
            if path == "/dev/null" {
                continue;
            }
            return if prefix == "rename to " {
                path
            } else {
                path.strip_prefix("a/")
                    .or_else(|| path.strip_prefix("b/"))
                    .unwrap_or(&path)
                    .to_owned()
            };
        }
    }
    let header = metadata
        .first()
        .and_then(|line| line.strip_prefix("diff --git "))
        .unwrap_or("Changes");
    // Mode-only and binary changes have no +++ header. Their source/destination
    // paths are identical; use equality to disambiguate spaces inside a path.
    for (index, _) in header.match_indices(' ') {
        let left = unquote(header.get(..index).unwrap_or_default());
        let right = unquote(header.get(index + 1..).unwrap_or_default());
        if let (Some(left), Some(right)) = (left.strip_prefix("a/"), right.strip_prefix("b/"))
            && left == right
        {
            return right.to_owned();
        }
    }
    header.to_owned()
}
impl From<String> for Document {
    fn from(source: String) -> Self {
        let mut sections = Vec::<String>::new();
        for line in source.lines() {
            if line.starts_with("diff --git ") || sections.is_empty() {
                sections.push(String::new());
            }
            if let Some(section) = sections.last_mut() {
                section.push_str(line);
                section.push('\n');
            }
        }
        let files = sections
            .into_iter()
            .map(|patch| {
                let path = file_path(&patch);
                let status = if patch.lines().any(|line| line.starts_with("new file mode ")) {
                    "A"
                } else if patch
                    .lines()
                    .any(|line| line.starts_with("deleted file mode "))
                {
                    "D"
                } else {
                    "M"
                };
                let names = if let Some(old) = patch
                    .lines()
                    .find_map(|line| line.strip_prefix("rename from "))
                {
                    format!("R\0{}\0{path}\0", unquote(old))
                } else if let Some(old) = patch
                    .lines()
                    .find_map(|line| line.strip_prefix("copy from "))
                {
                    format!("C\0{}\0{path}\0", unquote(old))
                } else {
                    format!("{status}\0{path}\0")
                };
                let diff = crate::diff::parse(&names, &patch)
                    .and_then(|files| {
                        files
                            .into_iter()
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("Empty diff"))
                    })
                    .map_err(|error| format!("{error:#}"));
                File { path, patch, diff }
            })
            .collect::<Vec<_>>();
        let tree = crate::tree::entries(
            &files
                .iter()
                .map(|file| crate::diff::DiffFile {
                    path: file.path.clone(),
                    old_path: file.path.clone(),
                    status: "M".into(),
                    hunks: Vec::new(),
                    additions: 0,
                    deletions: 0,
                })
                .collect::<Vec<_>>(),
        );
        Self {
            files,
            tree,
            cache: None,
        }
    }
}
impl From<&str> for Document {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl Document {
    fn rows(
        &mut self,
        position: &Position,
        width: u16,
        wrap: bool,
    ) -> Arc<Vec<super::patch::SourceRow>> {
        let key = (
            position.change_file,
            position.change_directory.clone(),
            position.horizontal,
            width,
            wrap,
        );
        if self.cache.as_ref().is_none_or(|cache| cache.key != key) {
            let mut rows = Vec::new();
            let mut source_index = 0;
            for (_, file) in self.files.iter().enumerate().filter(|(index, file)| {
                position
                    .change_directory
                    .as_ref()
                    .map_or(*index == position.change_file, |dir| {
                        crate::tree::contains(dir, &file.path)
                    })
            }) {
                if position.change_directory.is_some() {
                    rows.push(super::patch::SourceRow {
                        line: Line::from(Span::styled(
                            crate::model::clean(&file.path),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        )),
                        source: None,
                    });
                }
                match &file.diff {
                    Ok(diff) => {
                        for hunk in &diff.hunks {
                            rows.push(super::patch::SourceRow {
                                line: Line::from(Span::styled(
                                    crate::model::clean(&hunk.header),
                                    Style::default().fg(DIM),
                                )),
                                source: None,
                            });
                            for line in &hunk.lines {
                                // Use the same source-coordinate and syntax renderer as PR diffs.
                                for row in crate::ui::code_rows(
                                    &file.path,
                                    std::slice::from_ref(line),
                                    usize::from(width),
                                    false,
                                    (position.horizontal, wrap),
                                ) {
                                    rows.push(super::patch::SourceRow {
                                        line: Line::from(row.spans),
                                        source: (line.old.is_some() || line.new.is_some())
                                            .then(|| (source_index, line.text.clone())),
                                    });
                                }
                                source_index += 1;
                            }
                        }
                    }
                    Err(error) => rows.push(super::patch::SourceRow {
                        line: Line::from(Span::styled(
                            format!("Cannot read diff: {error}"),
                            Style::default().fg(RED),
                        )),
                        source: None,
                    }),
                }
                if position.change_directory.is_some() {
                    rows.push(super::patch::SourceRow {
                        line: Line::default(),
                        source: None,
                    });
                }
            }
            self.cache = Some(Rendered {
                key,
                rows: Arc::new(rows),
            });
        }
        self.cache
            .as_ref()
            .map(|cache| Arc::clone(&cache.rows))
            .unwrap_or_default()
    }
}

impl Ui {
    pub(super) fn receive_changes(&mut self, id: String, document: Document) {
        if self.changes.get(&id).is_some_and(|old| {
            old.files.len() == document.files.len()
                && old
                    .files
                    .iter()
                    .zip(&document.files)
                    .all(|(a, b)| a.patch == b.patch)
        }) {
            return; // Keep navigation, scroll, and cached rows on unchanged background refreshes.
        }
        let position = self.positions.entry(id.clone()).or_default();
        let old_path = self
            .changes
            .get(&id)
            .and_then(|old| old.files.get(position.change_file))
            .map(|file| file.path.as_str());
        if let Some(path) = old_path {
            if let Some(index) = document.files.iter().position(|file| file.path == path) {
                position.change_file = index;
            } else {
                position.change_file = 0;
                position.changes = 0;
                position.selection = None;
            }
        }
        if position.change_directory.as_ref().is_some_and(|path| {
            !document
                .tree
                .iter()
                .any(|entry| entry.file.is_none() && &entry.path == path)
        }) {
            position.change_directory = None;
        }
        position.change_tree = crate::tree::selected(
            &document.tree,
            position.change_file,
            position.change_directory.as_deref(),
        );
        self.changes.insert(id, document);
    }
    pub(super) fn select_change(&mut self, index: usize) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(document) = self.changes.get(&id) else {
            return;
        };
        let Some(entry) = document.tree.get(index) else {
            return;
        };
        let p = self.positions.entry(id).or_default();
        p.change_tree = index;
        let directory = entry.file.is_none().then(|| entry.path.clone());
        if p.change_directory != directory || entry.file.is_some_and(|file| p.change_file != file) {
            p.change_directory = directory;
            if let Some(file) = entry.file {
                p.change_file = file;
            }
            p.changes = 0;
            p.selection = None;
            p.horizontal = 0;
        }
        self.focus = Focus::ChangeTree;
        self.panels.focused = false;
    }
    pub(super) fn move_change_tree(&mut self, delta: i32) {
        let Some(id) = self.selected.as_ref() else {
            return;
        };
        let Some(document) = self.changes.get(id) else {
            return;
        };
        let current = self.positions.get(id).map_or(0, |p| p.change_tree);
        let index = crate::tree::step(&document.tree, current, delta);
        self.select_change(index);
    }
    pub(super) fn step_change_file(&mut self, forward: bool) -> bool {
        if self.focus != Focus::Changes || !self.changes_visible {
            return false;
        }
        let Some(id) = self.selected.as_ref() else {
            return false;
        };
        let Some(p) = self.positions.get_mut(id) else {
            return false;
        };
        if p.change_directory.is_some()
            || (forward && p.changes < self.change_lines.saturating_sub(1))
            || (!forward && p.changes != 0)
        {
            return false;
        }
        let Some(document) = self.changes.get_mut(id) else {
            return false;
        };
        let Some(file) = crate::tree::next_file(&document.tree, p.change_file, forward) else {
            return false;
        };
        p.change_file = file;
        p.change_tree = crate::tree::selected(&document.tree, file, None);
        p.selection = None;
        p.horizontal = 0;
        self.change_lines = document.rows(p, self.change_width, self.change_wrap).len();
        p.changes = if forward {
            0
        } else {
            self.change_lines.saturating_sub(1)
        };
        true
    }
    pub(super) fn draw_changes(&mut self, frame: &mut Frame, rect: Rect) {
        let width = (rect.width / 4)
            .clamp(20, 34)
            .min(rect.width.saturating_sub(12));
        let tree = panel(
            frame,
            Rect::new(rect.x, rect.y, width, rect.height),
            "Files",
            !self.panels.focused && self.focus == Focus::ChangeTree,
        );
        let content = panel(
            frame,
            Rect::new(
                rect.x + width,
                rect.y,
                rect.width.saturating_sub(width),
                rect.height,
            ),
            "Current worktree changes",
            !self.panels.focused && self.focus == Focus::Changes,
        );
        self.hits.push((tree, Action::Focus(Focus::ChangeTree)));
        self.hits.push((content, Action::Focus(Focus::Changes)));
        self.change_rows = Default::default();
        self.change_lines = 0;
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(document) = self.changes.get_mut(&id) else {
            frame.render_widget(
                Paragraph::new(if self.changing {
                    "Reading local changes…"
                } else {
                    "Changes are available for coding sessions."
                })
                .wrap(Wrap { trim: false }),
                content,
            );
            return;
        };
        if document.files.is_empty() {
            frame.render_widget(
                Paragraph::new("No changes in the current worktree"),
                content,
            );
            return;
        }
        let p = self.positions.entry(id).or_default();
        p.change_file = p.change_file.min(document.files.len().saturating_sub(1));
        p.change_tree = p.change_tree.min(document.tree.len().saturating_sub(1));
        let statuses = document
            .files
            .iter()
            .map(|file| {
                file.diff
                    .as_ref()
                    .map(|d| d.status.clone())
                    .unwrap_or_else(|_| "M".into())
            })
            .collect::<Vec<_>>();
        for (row, index) in crate::tree::draw(
            frame,
            tree,
            &document.tree,
            &statuses,
            p.change_tree,
            &mut p.change_tree_horizontal,
        ) {
            self.hits.push((row, Action::ChangeFile(index)));
        }
        let selected = document.files.iter().enumerate().filter(|(index, file)| {
            p.change_directory
                .as_ref()
                .map_or(*index == p.change_file, |dir| {
                    crate::tree::contains(dir, &file.path)
                })
        });
        let mut added = 0;
        let mut removed = 0;
        let mut longest = 0;
        for (_, file) in selected {
            if let Ok(diff) = &file.diff {
                added += diff.additions;
                removed += diff.deletions;
                longest = longest.max(
                    diff.hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .map(|l| unicode_width::UnicodeWidthStr::width(l.text.as_str()))
                        .max()
                        .unwrap_or(0),
                );
            }
        }
        p.horizontal = p
            .horizontal
            .min(longest.saturating_sub(usize::from(content.width.saturating_sub(8))));
        let title = p
            .change_directory
            .as_deref()
            .or_else(|| {
                document
                    .files
                    .get(p.change_file)
                    .map(|file| file.path.as_str())
            })
            .unwrap_or("Changes");
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(crate::model::clean(title), Style::default().fg(TEXT)),
                Span::styled(format!(" +{added}"), Style::default().fg(GREEN)),
                Span::styled(format!(" -{removed}"), Style::default().fg(RED)),
            ])),
            Rect::new(content.x, content.y, content.width, 1),
        );
        self.change_width = content.width;
        let rows = document.rows(p, content.width, self.change_wrap);
        let body = Rect::new(
            content.x,
            content.y + 1,
            content.width,
            content.height.saturating_sub(1),
        );
        self.change_lines = rows.len();
        p.changes = p.changes.min(rows.len().saturating_sub(1));
        let top = p
            .changes
            .saturating_sub(usize::from(body.height) / 2)
            .min(rows.len().saturating_sub(usize::from(body.height)));
        let lines = rows
            .iter()
            .enumerate()
            .skip(top)
            .take(usize::from(body.height))
            .map(|(index, row)| {
                let mut line = row.line.clone();
                if p.selection.is_some_and(|anchor| {
                    (anchor.min(p.changes)..=anchor.max(p.changes)).contains(&index)
                }) {
                    line.style = line.style.bg(PANEL);
                    for span in &mut line.spans {
                        span.style = span.style.bg(PANEL);
                    }
                }
                if index == p.changes
                    && self.focus == Focus::Changes
                    && !self.panels.focused
                    && let Some(span) = line.spans.first_mut()
                {
                    span.content =
                        format!("›{}", span.content.chars().skip(1).collect::<String>()).into();
                }
                line
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), body);
        self.change_rows = rows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    #[test]
    fn shared_code_rows_cache_wrap_source_coordinates_and_folder_boundaries() -> Result<()> {
        let mut document = Document::from(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+let a_long_variable_name = 123;\ndiff --git a/src-other/b.rs b/src-other/b.rs\n--- a/src-other/b.rs\n+++ b/src-other/b.rs\n@@ -1 +1 @@\n-old\n+unrelated\n",
        );
        let position = Position {
            change_directory: Some("src".into()),
            ..Default::default()
        };
        let rows = document.rows(&position, 20, true);
        assert!(Arc::ptr_eq(&rows, &document.rows(&position, 20, true)));
        assert!(
            !rows
                .iter()
                .any(|r| r.line.to_string().contains("unrelated"))
        );
        let wrapped = rows
            .iter()
            .filter(|r| {
                r.source
                    .as_ref()
                    .is_some_and(|(_, text)| text == "let a_long_variable_name = 123;")
            })
            .collect::<Vec<_>>();
        assert!(wrapped.len() > 1);
        assert!(
            wrapped
                .windows(2)
                .all(|pair| pair.first().and_then(|r| r.source.as_ref())
                    == pair.get(1).and_then(|r| r.source.as_ref()))
        );
        assert!(
            wrapped
                .first()
                .context("wrapped row")?
                .line
                .spans
                .iter()
                .any(|span| span.style.bg == Some(crate::ui::ADD_BG))
        );
        assert!(!Arc::ptr_eq(&rows, &document.rows(&position, 80, true)));
        Ok(())
    }
    #[test]
    fn git_paths_and_file_tree_cover_spaces_unicode_deletions_and_renames() {
        let source = "diff --git a/src/old name.rs b/src/new name.rs\nsimilarity index 100%\nrename from src/old name.rs\nrename to src/new name.rs\ndiff --git a/deleted b/deleted\ndeleted file mode 100644\n--- a/deleted\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\ndiff --git \"a/\\347\\225\\214.txt\" \"b/\\347\\225\\214.txt\"\nnew file mode 100644\n--- /dev/null\n+++ \"b/\\347\\225\\214.txt\"\n@@ -0,0 +1 @@\n+new\ndiff --git a/a b/name.bin b/a b/name.bin\nBinary files a/a b/name.bin and b/a b/name.bin differ\n";
        let document = Document::from(source);
        assert_eq!(
            document
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["src/new name.rs", "deleted", "界.txt", "a b/name.bin"]
        );
        assert!(
            document
                .files
                .first()
                .and_then(|file| file.diff.as_ref().ok())
                .is_some_and(
                    |renamed| renamed.old_path == "src/old name.rs" && renamed.status == "R"
                )
        );
        assert!(document.files.iter().all(|file| file.diff.is_ok()));
        assert!(
            document
                .tree
                .iter()
                .any(|entry| entry.file.is_none() && entry.path == "src")
        );
        assert_eq!(
            document
                .tree
                .iter()
                .filter(|entry| entry.file.is_some())
                .count(),
            4
        );
        assert!(
            document
                .files
                .iter()
                .any(|file| file.patch.contains("rename from"))
        );
    }
}
