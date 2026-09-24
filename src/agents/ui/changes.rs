use super::*;

pub struct File {
    pub path: String,
    pub patch: String,
}
pub struct Document {
    pub files: Vec<File>,
    pub tree: Vec<crate::tree::Entry>,
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
            .map(|patch| File {
                path: file_path(&patch),
                patch,
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
        Self { files, tree }
    }
}
impl From<&str> for Document {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl Ui {
    pub(super) fn receive_changes(&mut self, id: String, document: Document) {
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
        position.change_tree = document
            .tree
            .iter()
            .position(|entry| entry.file == Some(position.change_file))
            .unwrap_or(0);
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
        if let Some(file) = entry.file
            && p.change_file != file
        {
            p.change_file = file;
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
        let count = self
            .changes
            .get(id)
            .map_or(0, |document| document.tree.len());
        let index = self
            .positions
            .get(id)
            .map_or(0, |p| p.change_tree)
            .saturating_add_signed(delta as isize)
            .min(count.saturating_sub(1));
        self.select_change(index);
    }
    pub(super) fn draw_changes(&mut self, frame: &mut Frame, rect: Rect) {
        let width = (rect.width / 4)
            .clamp(16, 34)
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
            "Changes since session start",
            !self.panels.focused && self.focus == Focus::Changes,
        );
        self.hits.push((tree, Action::Focus(Focus::ChangeTree)));
        self.hits.push((content, Action::Focus(Focus::Changes)));
        self.change_rows.clear();
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(document) = self.changes.get(&id) else {
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
            frame.render_widget(Paragraph::new("No changes since session start"), content);
            return;
        }
        let p = self.positions.entry(id).or_default();
        p.change_file = p.change_file.min(document.files.len().saturating_sub(1));
        p.change_tree = p.change_tree.min(document.tree.len().saturating_sub(1));
        let top = p
            .change_tree
            .saturating_sub(usize::from(tree.height) / 2)
            .min(document.tree.len().saturating_sub(usize::from(tree.height)));
        for (index, entry) in document
            .tree
            .iter()
            .enumerate()
            .skip(top)
            .take(usize::from(tree.height))
        {
            let row = Rect::new(tree.x, tree.y + (index - top) as u16, tree.width, 1);
            frame.render_widget(
                Paragraph::new(crate::ui::crop(
                    &crate::model::clean(&entry.label()),
                    0,
                    usize::from(tree.width),
                ))
                .style(
                    Style::default()
                        .fg(if entry.file == Some(p.change_file) {
                            ACCENT
                        } else {
                            TEXT
                        })
                        .bg(if index == p.change_tree { PANEL } else { BG }),
                ),
                row,
            );
            self.hits.push((row, Action::ChangeFile(index)));
        }
        let Some(file) = document.files.get(p.change_file) else {
            return;
        };
        let longest = file
            .patch
            .lines()
            .map(unicode_width::UnicodeWidthStr::width)
            .max()
            .unwrap_or(0);
        p.horizontal = p
            .horizontal
            .min(longest.saturating_sub(usize::from(content.width.saturating_sub(9))));
        let render_width = content
            .width
            .saturating_add(u16::try_from(p.horizontal).unwrap_or(u16::MAX));
        let (rows, added, removed) = super::patch::render_full(&file.patch, render_width);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(crate::model::clean(&file.path), Style::default().fg(TEXT)),
                Span::styled(format!(" +{added}"), Style::default().fg(GREEN)),
                Span::styled(format!(" -{removed}"), Style::default().fg(RED)),
            ])),
            Rect::new(content.x, content.y, content.width, 1),
        );
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
                if p.horizontal > 0 {
                    let mut offset = p.horizontal;
                    let mut remaining = usize::from(content.width);
                    let mut spans = Vec::new();
                    for (index, span) in line.spans.into_iter().enumerate() {
                        let skip = if index == 0 && row.source.is_some() {
                            0
                        } else {
                            offset.min(span.width())
                        };
                        let text = crate::ui::crop(&span.content, skip, remaining);
                        if index != 0 || row.source.is_none() {
                            offset = offset.saturating_sub(span.width());
                        }
                        remaining = remaining
                            .saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
                        spans.push(Span::styled(text, span.style));
                    }
                    line.spans = spans;
                }
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
