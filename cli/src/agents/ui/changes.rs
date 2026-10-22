#[path = "changes_reading.rs"]
mod reading;
use super::*;
pub(super) fn copy_rows(rows: &[super::patch::SourceRow], position: &Position) -> Option<String> {
    reading::copy(rows, position)
}
use ratatui::style::Modifier;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    pub id: String,
    pub revision: u64,
    pub pr: Option<crate::model::PrKey>,
}

pub struct File {
    pub path: String,
    pub patch: String,
    pub diff: Result<crate::diff::DiffFile, String>,
}
pub struct Document {
    pub files: Vec<File>,
    pub tree: Vec<crate::tree::Entry>,
    cache: Option<Rendered>,
    context: HashMap<String, crate::context::FileState>,
    original_patch: String,
    context_request: Option<u64>,
    context_error: Option<String>,
    reading: Option<reading::Reading>,
    last_rows: Arc<Vec<super::patch::SourceRow>>,
}
struct Rendered {
    key: (
        usize,
        Option<String>,
        usize,
        u16,
        bool,
        bool,
        crate::settings::Code,
    ),
    rows: Arc<Vec<super::patch::SourceRow>>,
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
                let path = crate::diff::patch_path(&patch);
                let diff = crate::diff::parse_patch(&patch)
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
            context: HashMap::new(),
            original_patch: source,
            context_request: None,
            context_error: None,
            reading: None,
            last_rows: Default::default(),
        }
    }
}
impl From<&str> for Document {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl Document {
    pub(super) fn clear_reading(&mut self) {
        self.reading = None;
    }
    pub(super) fn copied_source(&self, position: &Position) -> Option<String> {
        self.reading
            .as_ref()
            .filter(|r| r.projected == (position.changes, position.selection))
            .and_then(|r| r.copy.clone())
            .or_else(|| reading::copy(&self.last_rows, position))
    }
    fn reflow(
        &mut self,
        position: &mut Position,
        width: u16,
        wrap: bool,
        unified: bool,
        code: &crate::settings::Code,
    ) -> Arc<Vec<super::patch::SourceRow>> {
        if self
            .reading
            .as_ref()
            .is_some_and(|r| r.projected != (position.changes, position.selection))
        {
            self.reading = None;
        }
        if self.reading.is_none() && !self.last_rows.is_empty() {
            self.reading = Some(reading::Reading::capture(&self.last_rows, position));
        }
        let rows = self.rows_with_preferences(position, width, wrap, unified, code);
        if let Some(reading) = &mut self.reading {
            reading.project(&rows, position);
        }
        position.changes = position.changes.min(rows.len().saturating_sub(1));
        self.last_rows = Arc::clone(&rows);
        rows
    }
    #[cfg(test)]
    fn rows(
        &mut self,
        position: &Position,
        width: u16,
        wrap: bool,
    ) -> Arc<Vec<super::patch::SourceRow>> {
        self.rows_with_preferences(
            position,
            width,
            wrap,
            true,
            &crate::settings::Code::default(),
        )
    }
    fn rows_with_preferences(
        &mut self,
        position: &Position,
        width: u16,
        wrap: bool,
        unified: bool,
        code: &crate::settings::Code,
    ) -> Arc<Vec<super::patch::SourceRow>> {
        let key = (
            position.change_file,
            position.change_directory.clone(),
            position.horizontal,
            width,
            wrap,
            unified,
            code.clone(),
        );
        if self.cache.as_ref().is_none_or(|cache| cache.key != key) {
            let mut rows = Vec::new();

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
                        paired_source: None,
                        marked_line: None,
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
                                paired_source: None,
                                marked_line: None,
                            });
                            let state = self.context.get(&file.path);
                            if code.context_lines > 3
                                && state.and_then(|s| s.data.as_ref()).is_none()
                            {
                                rows.push(super::patch::SourceRow {
                                    line: Line::from(self.context_error.as_ref().map_or_else(
                                        || "Loading extra context…".to_owned(),
                                        |e| {
                                            format!(
                                                "Could not load context: {e}. Refresh to retry."
                                            )
                                        },
                                    )),
                                    source: None,
                                    paired_source: None,
                                    marked_line: None,
                                });
                            }
                            for part in crate::context::project(
                                hunk,
                                state.and_then(|s| s.data.as_deref()),
                                Default::default(),
                                code.context_lines,
                            ) {
                                for row in crate::ui::code_rows_with_preferences(
                                    &file.path,
                                    part.lines,
                                    usize::from(width),
                                    !unified && width >= 80,
                                    (position.horizontal, wrap),
                                    code,
                                ) {
                                    let (old, new) = match row.target {
                                        Some(crate::workflow::Target::Code {
                                            old, new, ..
                                        }) => (old, new),
                                        _ => (None, None),
                                    };
                                    let sources = |number: Option<u64>, old_side: bool| {
                                        number
                                            .and_then(|n| {
                                                part.lines.iter().find(|line| {
                                                    if old_side {
                                                        line.old == Some(n)
                                                    } else {
                                                        line.new == Some(n)
                                                    }
                                                })
                                            })
                                            .map(|line| {
                                                use std::hash::{Hash, Hasher};
                                                let mut hash =
                                                    std::collections::hash_map::DefaultHasher::new(
                                                    );
                                                file.path.hash(&mut hash);
                                                line.old.hash(&mut hash);
                                                line.new.hash(&mut hash);
                                                let key = hash.finish() as usize;
                                                (key, line.text.clone())
                                            })
                                    };
                                    let source = sources(new, false).or_else(|| sources(old, true));
                                    let paired_source = sources(old, true)
                                        .filter(|old| source.as_ref() != Some(old));
                                    rows.push(super::patch::SourceRow {
                                        line: Line::from(row.spans),
                                        marked_line: row.marked_spans.map(Line::from),
                                        source,
                                        paired_source,
                                    });
                                }
                            }
                        }
                    }
                    Err(error) => rows.push(super::patch::SourceRow {
                        line: Line::from(Span::styled(
                            format!("Cannot read diff: {error}"),
                            Style::default().fg(RED),
                        )),
                        source: None,
                        paired_source: None,
                        marked_line: None,
                    }),
                }
                if position.change_directory.is_some() {
                    rows.push(super::patch::SourceRow {
                        line: Line::default(),
                        source: None,
                        paired_source: None,
                        marked_line: None,
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
    pub(super) fn request_change_context(&mut self) {
        if self.preferences.code.context_lines <= 3 || !self.drilled || !self.changes_visible {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let source = self.change_source(&id);
        let Some(document) = self
            .changes
            .get_mut(&id)
            .filter(|d| d.context_request.is_none())
        else {
            return;
        };
        let files = document
            .files
            .iter()
            .filter_map(|f| f.diff.as_ref().ok().cloned())
            .collect::<Vec<_>>();
        if files.is_empty() {
            return;
        }
        document.context_request = Some(self.change_request);
        document.cache = None;
        let patch = document.original_patch.clone();
        self.task(
            Task::ChangesContext(source.clone(), self.change_request, patch.clone()),
            Request::ChangesContext {
                id,
                pr: source.pr,
                patch,
                files,
            },
            false,
        );
    }
    pub(super) fn receive_change_context(
        &mut self,
        source: &Source,
        request: u64,
        patch: &str,
        result: Result<Reply, String>,
    ) {
        if self.change_source(&source.id) != *source {
            return;
        }
        let Some(document) = self
            .changes
            .get_mut(&source.id)
            .filter(|d| d.original_patch == patch && d.context_request == Some(request))
        else {
            return;
        };
        match result {
            Ok(Reply::ChangesContext(files)) => {
                for (path, lines) in files {
                    if let Some(file) = document
                        .files
                        .iter()
                        .find(|f| f.path == path)
                        .and_then(|f| f.diff.as_ref().ok())
                    {
                        match crate::context::FileContext::new(file, lines) {
                            Ok(data) => {
                                document.context.entry(path).or_default().data =
                                    Some(Arc::new(data));
                            }
                            Err(error) => document.context_error = Some(error.to_string()),
                        }
                    }
                }
            }
            Err(error) => document.context_error = Some(error),
            _ => document.context_error = Some("Unexpected context response".into()),
        }
        if document.context_error.is_none()
            && document
                .files
                .iter()
                .filter_map(|f| f.diff.as_ref().ok())
                .any(|f| {
                    f.hunks.iter().any(|h| h.header.starts_with("@@ "))
                        && document
                            .context
                            .get(&f.path)
                            .is_none_or(|s| s.data.is_none())
                })
        {
            document.context_error =
                Some("Context response did not include the pinned file".into());
        }
        document.cache = None;
    }
    pub(super) fn change_source(&self, id: &str) -> Source {
        let links = self.prs.all(id);
        let selected = self.positions.get(id).and_then(|p| p.change_pr.as_ref());
        Source {
            id: id.into(),
            revision: self.sessions.get(id).map_or(0, |s| s.workspace_revision),
            pr: links
                .iter()
                .find(|link| Some(&link.pr.key) == selected)
                .or_else(|| links.first())
                .map(|link| link.pr.key.clone()),
        }
    }
    pub(super) fn sync_change_pr(&mut self, id: &str) {
        let source = self.change_source(id);
        if self.positions.get(id).and_then(|p| p.change_pr.as_ref()) != source.pr.as_ref() {
            self.reset_change_source(id, source.pr);
        }
    }
    fn reset_change_source(&mut self, id: &str, pr: Option<crate::model::PrKey>) {
        let cached = pr
            .as_ref()
            .and_then(|key| crate::github::store::load(&self.storage, key))
            .and_then(|pr| pr.diff);
        let position = self.positions.entry(id.into()).or_default();
        position.change_pr = pr;
        position.change_file = 0;
        position.change_tree = 0;
        position.change_directory = None;
        position.change_tree_horizontal = 0;
        position.changes = 0;
        position.horizontal = 0;
        position.selection = None;
        self.changes.remove(id);
        if let Some(cached) = cached {
            self.changes.insert(id.into(), cached.value.into());
        }
        self.change_errors.remove(id);
        self.changes_at = None;
        self.changing = None;
        self.change_rows = Default::default();
        self.change_lines = 0;
    }
    pub(super) fn select_change_pr(&mut self, index: usize) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(key) = self.prs.all(&id).get(index).map(|link| link.pr.key.clone()) else {
            return;
        };
        if self.positions.get(&id).and_then(|p| p.change_pr.as_ref()) != Some(&key) {
            self.reset_change_source(&id, Some(key));
        }
        self.focus = Focus::ChangeTree;
        self.panels.focused = false;
    }
    pub(super) fn cycle_change_pr(&mut self, forward: bool) {
        let Some(id) = self.selected.as_ref() else {
            return;
        };
        let links = self.prs.all(id);
        if links.len() < 2 {
            return;
        }
        let source = self.change_source(id);
        let index = links
            .iter()
            .position(|link| Some(&link.pr.key) == source.pr.as_ref())
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % links.len()
        } else {
            (index + links.len() - 1) % links.len()
        };
        self.select_change_pr(next);
    }
    pub(super) fn receive_change_result(
        &mut self,
        source: &Source,
        request: u64,
        result: Result<Reply, String>,
    ) {
        if self
            .changing
            .as_ref()
            .is_none_or(|(pending, serial)| pending != source || *serial != request)
            || self.selected.as_deref() != Some(source.id.as_str())
            || self.change_source(&source.id) != *source
        {
            return;
        }
        self.changing = None;
        self.changes_at = Some(Instant::now());
        match result {
            Ok(Reply::Changes(patch)) => {
                self.change_errors.remove(&source.id);
                self.receive_changes(source.id.clone(), patch.into());
            }
            Err(error) => {
                self.change_errors.insert(source.id.clone(), error);
            }
            _ => {}
        }
    }
    pub(super) fn receive_changes(&mut self, id: String, document: Document) {
        if self.changes.get(&id).is_some_and(|old| {
            old.files.len() == document.files.len()
                && old
                    .files
                    .iter()
                    .zip(&document.files)
                    .all(|(a, b)| a.patch == b.patch)
        }) {
            if let Some(old) = self.changes.get_mut(&id)
                && old.context_error.take().is_some()
            {
                old.context_request = None;
                old.cache = None;
            }
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
        let Some(document) = self.changes.get_mut(&id) else {
            return;
        };
        let Some(entry) = document.tree.get(index) else {
            return;
        };
        let p = self.positions.entry(id).or_default();
        p.change_tree = index;
        let directory = entry.file.is_none().then(|| entry.path.clone());
        if p.change_directory != directory || entry.file.is_some_and(|file| p.change_file != file) {
            document.reading = None;
            document.last_rows = Default::default();
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
        document.reading = None;
        document.last_rows = Default::default();
        p.change_file = file;
        p.change_tree = crate::tree::selected(&document.tree, file, None);
        p.selection = None;
        p.horizontal = 0;
        self.change_lines = document
            .rows_with_preferences(
                p,
                self.change_width,
                self.change_wrap,
                self.change_unified,
                &self.preferences.code,
            )
            .len();
        p.changes = if forward {
            0
        } else {
            self.change_lines.saturating_sub(1)
        };
        true
    }
    pub(super) fn draw_changes(&mut self, frame: &mut Frame, mut rect: Rect) {
        let pr = self
            .selected
            .as_ref()
            .and_then(|id| self.change_source(id).pr);
        if let Some(id) = self.selected.clone() {
            let keys = self
                .prs
                .all(&id)
                .iter()
                .map(|link| link.pr.key.clone())
                .collect::<Vec<_>>();
            if keys.len() > 1 && rect.height > 0 {
                let selected = keys
                    .iter()
                    .position(|key| Some(key) == pr.as_ref())
                    .unwrap_or(0);
                // Scroll the tab strip to keep the selected PR visible on narrow terminals.
                let labels = keys
                    .iter()
                    .map(|key| format!(" {}#{} ", key.repository(), key.number))
                    .collect::<Vec<_>>();
                let mut start = 0;
                let mut used = labels
                    .iter()
                    .take(selected + 1)
                    .map(|s| s.len())
                    .sum::<usize>();
                while used > usize::from(rect.width.saturating_sub(10)) && start < selected {
                    used = used.saturating_sub(labels.get(start).map_or(0, String::len));
                    start += 1;
                }
                let mut x = rect.x;
                for (index, label) in labels.iter().enumerate().skip(start) {
                    let width = (label.len() as u16).min(rect.right().saturating_sub(x));
                    if width == 0 {
                        break;
                    }
                    self.button(
                        frame,
                        Rect::new(x, rect.y, width, 1),
                        label,
                        Action::ChangePr(index),
                        index == selected,
                    );
                    x = x.saturating_add(width);
                }
                rect.y = rect.y.saturating_add(1);
                rect.height = rect.height.saturating_sub(1);
            }
        }
        let width = (rect.width / 4)
            .clamp(20, 34)
            .min(rect.width.saturating_sub(12));
        let tree = panel(
            frame,
            Rect::new(rect.x, rect.y, width, rect.height),
            "Files",
            !self.panels.focused && self.focus == Focus::ChangeTree,
        );
        let mut content = panel(
            frame,
            Rect::new(
                rect.x + width,
                rect.y,
                rect.width.saturating_sub(width),
                rect.height,
            ),
            &pr.as_ref().map_or_else(
                || "Current worktree changes".into(),
                |key| {
                    let multiple = self
                        .selected
                        .as_ref()
                        .is_some_and(|id| self.prs.all(id).len() > 1);
                    format!(
                        "PR {}{}",
                        key.id(),
                        if multiple {
                            let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
                            format!(
                                " · {} / {} switch PR",
                                bindings.hint(crate::shortcuts::ids::CHANGES_PREVIOUS_PR),
                                bindings.hint(crate::shortcuts::ids::CHANGES_NEXT_PR)
                            )
                        } else {
                            String::new()
                        }
                    )
                },
            ),
            !self.panels.focused && self.focus == Focus::Changes,
        );
        self.hits.push((tree, Action::Focus(Focus::ChangeTree)));
        self.hits.push((content, Action::Focus(Focus::Changes)));
        self.change_rows = Default::default();
        self.change_lines = 0;
        let Some(id) = self.selected.clone() else {
            return;
        };
        if let Some(error) = self.change_errors.get(&id) {
            frame.render_widget(
                Paragraph::new(if self.changes.contains_key(&id) {
                    format!("Refresh failed; cached diff may be stale: {error}")
                } else {
                    format!("Cannot load diff: {error}")
                })
                .style(Style::default().fg(RED)),
                Rect::new(content.x, content.y, content.width, content.height.min(1)),
            );
            if !self.changes.contains_key(&id) {
                return;
            }
            content.y = content.y.saturating_add(1);
            content.height = content.height.saturating_sub(1);
        }
        let Some(document) = self.changes.get_mut(&id) else {
            frame.render_widget(
                Paragraph::new(if pr.is_some() {
                    "Loading PR diff…"
                } else if self.changing.is_some() {
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
                Paragraph::new(if pr.is_some() {
                    "This PR has no diff changes"
                } else {
                    "No changes in the current worktree"
                }),
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
        let rows = document.reflow(
            p,
            content.width,
            self.change_wrap,
            self.change_unified,
            &self.preferences.code,
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
                if p.selection.is_some_and(|anchor| {
                    (anchor.min(p.changes)..=anchor.max(p.changes)).contains(&index)
                }) {
                    if let Some(marked) = &row.marked_line {
                        line = marked.clone();
                    }
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
    fn changes_native_painter_applies_all_modes_without_modifying_copy() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut config = Config::default();
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            &config,
        );
        ui.selected = Some("one".into());
        ui.focus = Focus::Changes;
        let patch = "diff --git a/file.ts b/file.ts\n--- a/file.ts\n+++ b/file.ts\n@@ -13,6 +13,7 @@\n line 13\n line 14\n line 15\n+\tconst value = longFunctionName(argumentOne, argumentTwo);  \n line 17\n line 18\n line 19\n";
        let mut document = Document::from(patch);
        let canonical = document
            .files
            .first()
            .and_then(|f| f.diff.as_ref().ok())
            .context("file")?;
        let mut full = Vec::new();
        for n in 1..=31 {
            full.push(crate::diff::DiffLine {
                kind: if n == 16 {
                    crate::diff::LineKind::Add
                } else {
                    crate::diff::LineKind::Context
                },
                old: if n == 16 {
                    None
                } else {
                    Some(if n > 16 { n - 1 } else { n })
                },
                new: Some(n),
                text: if n == 16 {
                    "\tconst value = longFunctionName(argumentOne, argumentTwo);  ".into()
                } else {
                    format!("line {n}")
                },
            });
        }
        document.context.entry("file.ts".into()).or_default().data =
            Some(Arc::new(crate::context::FileContext::new(canonical, full)?));
        ui.changes.insert("one".into(), document);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 45))?;
        let screen = |t: &ratatui::Terminal<ratatui::backend::TestBackend>| {
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
        for unified in [false, true] {
            config.unified = unified;
            for (context, count) in [(0, 1), (3, 7), (5, 11), (10, 21)] {
                config.preferences.code.context_lines = context;
                ui.apply_settings(&config);
                terminal.draw(|f| ui.draw_changes(f, f.area()))?;
                assert_eq!(
                    ui.change_rows.iter().filter(|r| r.source.is_some()).count(),
                    count
                );
                let code = ui
                    .change_rows
                    .iter()
                    .find(|r| {
                        r.source
                            .as_ref()
                            .is_some_and(|(_, s)| s.contains("longFunctionName"))
                    })
                    .context("code")?
                    .line
                    .to_string();
                assert_eq!(code.contains('│'), !unified);
            }
        }
        config.preferences.code.context_lines = 0;
        config.preferences.code.line_numbers = false;
        config.preferences.code.whitespace = crate::settings::Whitespace::SelectedLines;
        ui.apply_settings(&config);
        terminal.draw(|f| ui.draw_changes(f, f.area()))?;
        assert!(!screen(&terminal).contains("→···const·value"));
        let row = ui
            .change_rows
            .iter()
            .position(|r| r.source.is_some())
            .context("source")?;
        let position = ui.positions.get_mut("one").context("position")?;
        position.changes = row;
        position.selection = Some(row);
        terminal.draw(|f| ui.draw_changes(f, f.area()))?;
        assert!(screen(&terminal).contains("→···const·value"));
        assert!(
            !ui.change_rows
                .get(row)
                .context("row")?
                .line
                .to_string()
                .contains("   16")
        );
        ui.copy_change();
        assert_eq!(
            ui.clipboard.as_deref(),
            Some("\tconst value = longFunctionName(argumentOne, argumentTwo);  ")
        );
        ui.positions.get_mut("one").context("position")?.selection = None;
        config.preferences.code.whitespace = crate::settings::Whitespace::AllLines;
        ui.apply_settings(&config);
        terminal.draw(|f| ui.draw_changes(f, f.area()))?;
        assert!(screen(&terminal).contains("→···const·value"));
        config.preferences.code.whitespace = crate::settings::Whitespace::Off;
        config.wrap_diff = false;
        ui.apply_settings(&config);
        let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 20))?;
        narrow.draw(|f| ui.draw_changes(f, f.area()))?;
        let clipped = ui.change_rows.len();
        config.wrap_diff = true;
        ui.apply_settings(&config);
        narrow.draw(|f| ui.draw_changes(f, f.area()))?;
        assert!(ui.change_rows.len() > clipped);
        assert!(!screen(&narrow).contains("→···const·value"));
        Ok(())
    }

    #[test]
    fn asynchronous_context_rejects_stale_results_and_preserves_preview_source_intent() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            &Config::default(),
        );
        ui.selected = Some("one".into());
        let patch = "diff --git a/file.ts b/file.ts\n--- a/file.ts\n+++ b/file.ts\n@@ -13,7 +13,7 @@\n line 13\n line 14\n line 15\n-old 16\n+new 16\n line 17\n line 18\n line 19\n";
        let mut document = Document::from(patch);
        let canonical = document
            .files
            .first()
            .context("file")?
            .diff
            .as_ref()
            .map_err(|e| anyhow::anyhow!(e.clone()))?
            .hunks
            .first()
            .context("hunk")?
            .id
            .clone();
        document.context_request = Some(7);
        ui.changes.insert("one".into(), document);
        let source = ui.change_source("one");
        let mut code = crate::settings::Code {
            context_lines: 10,
            ..Default::default()
        };
        let mut position = Position::default();
        let document = ui.changes.get_mut("one").context("document")?;
        let pending = document.reflow(&mut position, 100, true, true, &code);
        assert!(
            pending
                .iter()
                .any(|r| r.line.to_string().contains("Loading extra context"))
        );
        position.changes = pending
            .iter()
            .position(|r| r.source.as_ref().is_some_and(|(_, s)| s == "line 13"))
            .context("source")?;
        position.selection = Some(position.changes);
        document.reflow(&mut position, 100, true, true, &code);
        code.context_lines = 0;
        document.reflow(&mut position, 100, true, true, &code);
        let mut full = Vec::new();
        for n in 1..=31 {
            if n == 16 {
                full.push(crate::diff::DiffLine {
                    kind: crate::diff::LineKind::Remove,
                    old: Some(n),
                    new: None,
                    text: "old 16".into(),
                });
                full.push(crate::diff::DiffLine {
                    kind: crate::diff::LineKind::Add,
                    old: None,
                    new: Some(n),
                    text: "new 16".into(),
                });
            } else {
                full.push(crate::diff::DiffLine {
                    kind: crate::diff::LineKind::Context,
                    old: Some(n),
                    new: Some(n),
                    text: format!("line {n}"),
                });
            }
        }
        ui.receive_change_context(
            &source,
            6,
            patch,
            Ok(Reply::ChangesContext(vec![(
                "file.ts".into(),
                full.clone(),
            )])),
        );
        assert!(
            ui.changes
                .get("one")
                .context("document")?
                .context
                .is_empty()
        );
        ui.receive_change_context(
            &source,
            7,
            "stale patch",
            Ok(Reply::ChangesContext(vec![(
                "file.ts".into(),
                full.clone(),
            )])),
        );
        assert!(
            ui.changes
                .get("one")
                .context("document")?
                .context
                .is_empty()
        );
        ui.change_request = 99; // An unchanged background refresh must not invalidate this result.
        ui.selected = Some("two".into()); // Reply arrives while another session is selected.
        let mut stale_source = source.clone();
        stale_source.revision += 1;
        ui.receive_change_context(
            &stale_source,
            7,
            patch,
            Ok(Reply::ChangesContext(vec![(
                "file.ts".into(),
                full.clone(),
            )])),
        );
        assert!(
            ui.changes
                .get("one")
                .context("document")?
                .context
                .is_empty()
        );
        ui.receive_change_context(
            &source,
            7,
            patch,
            Ok(Reply::ChangesContext(vec![("file.ts".into(), full)])),
        );
        ui.selected = Some("one".into());
        ui.receive_changes("one".into(), Document::from(patch));
        let document = ui.changes.get_mut("one").context("document")?;
        code.context_lines = 10;
        let rows = document.reflow(&mut position, 100, true, true, &code);
        assert_eq!(
            rows.get(position.changes)
                .and_then(|r| r.source.as_ref())
                .map(|(_, s)| s.as_str()),
            Some("line 13")
        );
        assert_eq!(
            document.copied_source(&position).as_deref(),
            Some("line 13")
        );
        assert!(
            rows.iter()
                .any(|r| r.source.as_ref().is_some_and(|(_, s)| s == "line 6"))
        );
        assert!(
            rows.iter()
                .any(|r| r.source.as_ref().is_some_and(|(_, s)| s == "line 26"))
        );
        assert_eq!(
            document
                .files
                .first()
                .and_then(|f| f.diff.as_ref().ok())
                .and_then(|f| f.hunks.first())
                .map(|h| h.id.as_str()),
            Some(canonical.as_str())
        );
        document.context.clear();
        ui.receive_change_context(&source, 7, patch, Ok(Reply::ChangesContext(Vec::new())));
        let document = ui.changes.get_mut("one").context("document")?;
        let rows = document.reflow(&mut position, 100, true, true, &code);
        assert!(
            rows.iter()
                .any(|r| r.line.to_string().contains("Could not load context"))
        );
        Ok(())
    }

    #[test]
    fn changes_reading_and_selection_survive_context_and_wrap_clamps() -> Result<()> {
        let patch = format!(
            "diff --git a/file.ts b/file.ts\n--- a/file.ts\n+++ b/file.ts\n@@ -1,3 +1,3 @@\n {}\n-old\n+new\n after\n",
            "long context ".repeat(30)
        );
        let mut document = Document::from(patch);
        let mut position = Position::default();
        let mut code = crate::settings::Code::default();
        let rows = document.reflow(&mut position, 50, true, true, &code);
        let first = rows
            .iter()
            .position(|r| {
                r.source
                    .as_ref()
                    .is_some_and(|(_, s)| s.starts_with("long context"))
            })
            .context("context")?;
        position.changes = first + 4;
        position.selection = Some(first);
        let initial = position.changes;
        document.reflow(&mut position, 50, true, true, &code);
        code.context_lines = 0;
        document.reflow(&mut position, 50, false, false, &code);
        assert!(position.changes < initial);
        assert_eq!(
            document.copied_source(&position).as_deref(),
            Some("long context ".repeat(30).as_str())
        );
        code.context_lines = 3;
        document.reflow(&mut position, 50, true, true, &code);
        assert_eq!(position.changes, initial);
        assert_eq!(position.selection, Some(first));
        Ok(())
    }

    #[test]
    fn settings_changes_split_context_and_whitespace_preserve_copy_sources() -> Result<()> {
        let mut document = Document::from(
            "diff --git a/file.ts b/file.ts\n--- a/file.ts\n+++ b/file.ts\n@@ -1,3 +1,3 @@\n before\n- old value\n+ new value\n after\n",
        );
        let code = crate::settings::Code {
            line_numbers: false,
            context_lines: 0,
            whitespace: crate::settings::Whitespace::AllLines,
        };
        let rows = document.rows_with_preferences(&Position::default(), 100, true, false, &code);
        let text = rows
            .iter()
            .map(|r| r.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("│"), "{text}");
        assert!(
            text.contains("- ·old·value") && text.contains("+ ·new·value"),
            "{text}"
        );
        assert!(
            !text.contains("before") && !text.contains("after"),
            "{text}"
        );
        assert!(
            rows.iter()
                .any(|r| r.source.as_ref().is_some_and(|(_, s)| s == " new value"))
        );
        Ok(())
    }

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

#[cfg(test)]
mod shortcut_tests {
    use super::*;
    use crate::{
        settings::BindingKey,
        shortcuts::{ids, key},
    };
    use anyhow::{Context, Result};
    #[test]
    fn remapped_navigation_replaces_clamped_source_intent_while_copy_retains_it() -> Result<()> {
        for action in [
            ids::NAVIGATION_FIRST,
            ids::NAVIGATION_UP,
            ids::SELECTION_COPY,
        ] {
            let (mut ui, _dir, mut config) = super::super::shortcut_tests::fixture(None)?;
            config
                .preferences
                .keyboard
                .bindings
                .insert(action.0.into(), key(BindingKey::Function(12), 0));
            ui.apply_settings(&config);
            ui.focus = Focus::Changes;
            ui.changes_visible = true;
            let source = "long context ".repeat(30);
            let mut document = Document::from(format!(
                "diff --git a/file.ts b/file.ts\n--- a/file.ts\n+++ b/file.ts\n@@ -1,3 +1,3 @@\n {source}\n-old\n+new\n after\n"
            ));
            let position = ui.positions.get_mut("one").context("position")?;
            let mut code = crate::settings::Code::default();
            let rows = document.reflow(position, 50, true, true, &code);
            let first = rows
                .iter()
                .position(|r| {
                    r.source
                        .as_ref()
                        .is_some_and(|(_, s)| s.starts_with("long context"))
                })
                .context("context")?;
            position.changes = first + 4;
            position.selection = Some(first);
            let initial = position.changes;
            document.reflow(position, 50, true, true, &code);
            code.context_lines = 0;
            let rows = document.reflow(position, 50, false, false, &code);
            assert!(position.changes < initial);
            ui.change_lines = rows.len();
            ui.change_rows = rows;
            ui.changes.insert("one".into(), document);
            ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
            let document = ui.changes.get_mut("one").context("document")?;
            if action == ids::SELECTION_COPY {
                assert_eq!(ui.clipboard.as_deref(), Some(source.as_str()));
                assert!(document.reading.is_some());
            } else {
                assert!(
                    document.reading.is_none(),
                    "semantic navigation must discard old reading intent"
                );
            }
            code.context_lines = 3;
            let position = ui.positions.get_mut("one").context("position")?;
            document.reflow(position, 50, true, true, &code);
            if action == ids::SELECTION_COPY {
                assert_eq!(position.changes, initial);
                assert_eq!(position.selection, Some(first));
            } else {
                assert_ne!(position.changes, initial);
            }
        }
        Ok(())
    }
}
