//! Local repository discovery and review entry points; never performs GitHub writes.
use super::*;
use crate::{
    editor::Editor,
    local_diff::{self, Checkout, Comparison},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Paragraph},
};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Default)]
pub struct State {
    pub entries: Vec<PathBuf>,
    pub selected: usize,
    pub setup: Option<Setup>,
    pub loading: bool,
    pub error: Option<String>,
    pub sequence: u64,
    pub cancel: Option<Cancel>,
}
pub enum Setup {
    Roots(Editor),
    Select {
        roots: Vec<PathBuf>,
        repos: Vec<PathBuf>,
        chosen: BTreeSet<PathBuf>,
        selected: usize,
    },
}
#[derive(Clone, Debug)]
pub enum Action {
    Configure,
    Scan,
    Toggle(usize),
    Save,
    Open(usize),
    Refresh,
}
pub enum Event {
    Scanned(u64, Vec<PathBuf>, Result<Vec<PathBuf>, String>),
    Listed(u64, Result<Vec<PathBuf>, String>),
    Opened(u64, bool, Result<(Checkout, Snapshot), String>),
}
fn outcome<T>(value: anyhow::Result<T>) -> Result<T, String> {
    value.map_err(|e| format!("{e:#}"))
}
fn id(path: &std::path::Path) -> String {
    format!(
        "local:{}",
        crate::storage::hash(path.to_string_lossy().as_bytes())
    )
}

impl App {
    pub fn cancel_local_load(&mut self) {
        if let Some(cancel) = self.local.cancel.take() {
            cancel.cancel();
        }
        self.local.sequence += 1;
        self.local.loading = false;
    }
    pub fn load_local_repositories(&mut self) {
        if self.config.local_diff_roots.is_empty() && self.config.local_diff_repositories.is_empty()
        {
            self.local_action(Action::Configure);
            return;
        }
        self.cancel_local_load();
        let sequence = self.local.sequence;
        self.local.loading = true;
        self.local.error = None;
        let roots = self.config.local_diff_repositories.clone();
        let cancel = self.spawn(move |tx, cancel| {
            let output = (|| -> anyhow::Result<_> {
                let mut paths = BTreeSet::new();
                for root in roots {
                    paths.extend(local_diff::worktrees(&root, &cancel)?);
                }
                Ok(paths.into_iter().collect())
            })();
            let _ = tx.send(Message::Local(Event::Listed(sequence, outcome(output))));
        });
        self.local.cancel = Some(cancel);
    }
    pub fn open_local(&mut self, path: PathBuf, generate: bool) {
        self.inbox_tab = InboxTab::Diffs;
        self.cancel_local_load();
        let sequence = self.local.sequence;
        self.local.loading = true;
        self.local.error = None;
        let cancel = self.spawn(move |tx, cancel| {
            let output = (|| -> anyhow::Result<_> {
                let checkout = Checkout::resolve(&path, &cancel)?;
                let snapshot = local_diff::snapshot(&checkout, &cancel)?;
                Ok((checkout, snapshot))
            })();
            let _ = tx.send(Message::Local(Event::Opened(
                sequence,
                generate,
                outcome(output),
            )));
        });
        self.local.cancel = Some(cancel);
    }
    pub fn local_action(&mut self, action: Action) {
        match action {
            Action::Configure => {
                self.local.loading = false;
                self.cancel_local_load();
                let mut editor = Editor::default();
                editor.insert(
                    &self
                        .config
                        .local_diff_roots
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                self.local.setup = Some(Setup::Roots(editor));
                self.local.error = None;
            }
            Action::Scan => {
                let Some(Setup::Roots(editor)) = &self.local.setup else {
                    return;
                };
                let roots = editor
                    .text()
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        s.strip_prefix("~/")
                            .and_then(|suffix| dirs::home_dir().map(|home| home.join(suffix)))
                            .unwrap_or_else(|| PathBuf::from(s))
                    })
                    .collect::<Vec<_>>();
                if roots.is_empty() {
                    self.local.error = Some("Enter at least one base directory.".into());
                    return;
                }
                self.cancel_local_load();
                let sequence = self.local.sequence;
                self.local.loading = true;
                self.local.error = None;
                let cancel = self.spawn(move |tx, cancel| {
                    let output = local_diff::discover(&roots, &cancel);
                    let _ = tx.send(Message::Local(Event::Scanned(
                        sequence,
                        roots,
                        outcome(output),
                    )));
                });
                self.local.cancel = Some(cancel);
            }
            Action::Toggle(index) => {
                if let Some(Setup::Select {
                    repos,
                    chosen,
                    selected,
                    ..
                }) = &mut self.local.setup
                    && let Some(path) = repos.get(index)
                {
                    *selected = index;
                    if !chosen.remove(path) {
                        chosen.insert(path.clone());
                    }
                }
            }
            Action::Save => {
                if let Some(Setup::Select { roots, chosen, .. }) = &self.local.setup {
                    let mut config = self.config.clone();
                    config.local_diff_roots = roots.clone();
                    config.local_diff_repositories = chosen.clone();
                    if let Err(error) = self.storage.save_config(&config) {
                        self.local.error =
                            Some(format!("Could not save tracked repositories: {error:#}"));
                        return;
                    }
                    self.config = config;
                    self.local.setup = None;
                    self.load_local_repositories();
                }
            }
            Action::Open(index) => {
                if let Some(path) = self.local.entries.get(index).cloned() {
                    self.local.selected = index;
                    self.open_local(path, false);
                }
            }
            Action::Refresh => self.load_local_repositories(),
        }
    }
    pub fn local_receive(&mut self, event: Event) {
        let sequence = match &event {
            Event::Scanned(s, ..) | Event::Listed(s, ..) | Event::Opened(s, ..) => *s,
        };
        if sequence != self.local.sequence {
            return;
        }
        self.local.loading = false;
        match event {
            Event::Scanned(_, roots, output) => match output {
                Ok(repos) => {
                    let chosen = self
                        .config
                        .local_diff_repositories
                        .intersection(&repos.iter().cloned().collect())
                        .cloned()
                        .collect();
                    self.local.setup = Some(Setup::Select {
                        roots,
                        repos,
                        chosen,
                        selected: 0,
                    });
                }
                Err(error) => self.local.error = Some(error),
            },
            Event::Listed(_, output) => match output {
                Ok(entries) => {
                    self.local.entries = entries;
                    self.local.selected = self
                        .local
                        .selected
                        .min(self.local.entries.len().saturating_sub(1));
                }
                Err(error) => self.local.error = Some(error),
            },
            Event::Opened(_, generate, output) => match output {
                Ok((checkout, snapshot)) => {
                    let key = id(&checkout.root);
                    let working = matches!(checkout.comparison, Comparison::WorkingTree { .. });
                    let pr = PrDetail {
                        draft: false,
                        requested_reviewers: Vec::new(), requested_teams: Vec::new(),
                        key: PrKey { owner: "local".into(), repo: crate::storage::hash(checkout.root.to_string_lossy().as_bytes()), number: 1 },
                        title: format!("{} · {}", checkout.root.display(), checkout.branch),
                        body: if working { "Uncommitted local changes against HEAD. The supplied diff includes staged, unstaged, and untracked files. The isolated review worktree contains HEAD before these changes; use the supplied patch as the source of changed code." } else { "Local branch changes from the merge base with main to HEAD." }.into(),
                        author: String::new(), head: snapshot.head.clone(), base: snapshot.base.clone(),
                        head_branch: checkout.branch.clone(), base_branch: "main".into(), state: "local".into(),
                        additions: snapshot.files.iter().map(|f| f.additions as u64).sum(),
                        deletions: snapshot.files.iter().map(|f| f.deletions as u64).sum(), changed_files: snapshot.files.len() as u64,
                    };
                    if let Some(generation) =
                        self.reviews.get(&key).and_then(|r| r.generation.as_ref())
                    {
                        generation.cancel.cancel();
                    }
                    let snapshot_id = self.next_id();
                    self.reviews.insert(
                        key.clone(),
                        Review {
                            local: Some(checkout.clone()),
                            root: Some(checkout.root),
                            detail: Some(Arc::new(pr)),
                            snapshot: Some(Arc::new(snapshot)),
                            snapshot_id,
                            ..Review::default()
                        },
                    );
                    self.home = false;
                    self.opened = Some(key.clone());
                    self.pending_open = None;
                    self.view = View::Diff;
                    self.focus = Focus::Navigation;
                    self.file = 0;
                    self.directory = None;
                    self.scroll = 0;
                    self.horizontal = 0;
                    self.workflow.cursor = None;
                    self.workflow.selection = None;
                    self.notice = Notice::default();
                    if generate {
                        self.view = View::Guide;
                        self.focus = Focus::Content;
                        self.generate_for(&key, true);
                    }
                }
                Err(error) => {
                    self.notice = Notice::error(&error);
                    self.local.error = Some(error);
                }
            },
        }
        self.invalidate();
    }
    pub fn local_key(&mut self, key: KeyEvent) -> bool {
        if !self.home || self.inbox_tab != InboxTab::Diffs || self.modal.is_some() {
            return false;
        }
        if key.kind == crossterm::event::KeyEventKind::Release {
            return true;
        }
        let mut action = None;
        if let Some(setup) = &mut self.local.setup {
            if key.code == KeyCode::Esc {
                self.cancel_local_load();
                self.local.loading = false;
                self.local.setup = None;
                return true;
            }
            if self.local.loading {
                return true;
            }
            match setup {
                Setup::Roots(editor) => {
                    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
                        action = Some(Action::Scan);
                    } else {
                        editor.key(key);
                    }
                }
                Setup::Select {
                    repos, selected, ..
                } => match key.code {
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => *selected = (*selected + 1).min(repos.len().saturating_sub(1)),
                    KeyCode::Char(' ') => action = Some(Action::Toggle(*selected)),
                    KeyCode::Enter => action = Some(Action::Save),
                    KeyCode::Char('e') => action = Some(Action::Configure),
                    _ => {}
                },
            }
        } else {
            match key.code {
                KeyCode::Up => self.local.selected = self.local.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.local.selected =
                        (self.local.selected + 1).min(self.local.entries.len().saturating_sub(1))
                }
                KeyCode::Enter => action = Some(Action::Open(self.local.selected)),
                KeyCode::Char('r') => action = Some(Action::Refresh),
                KeyCode::Char('e') => action = Some(Action::Configure),
                _ => return false,
            }
        }
        if let Some(action) = action {
            self.local_action(action);
        }
        true
    }
    pub fn local_paste(&mut self, text: &str) -> bool {
        if self.home
            && self.inbox_tab == InboxTab::Diffs
            && let Some(Setup::Roots(editor)) = &mut self.local.setup
        {
            editor.insert(text);
            return true;
        }
        false
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    use crate::ui::{ACCENT, BG, DIM, TEXT};
    let area = frame.area();
    app.hits.clear();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    if area.width < 24 || area.height < 10 {
        return;
    }
    let mut x = 2;
    for (label, tab) in [
        ("1 My PRs", InboxTab::MyPrs),
        ("2 Repositories", InboxTab::Repositories),
        ("3 Diffs", InboxTab::Diffs),
    ] {
        let width = (label.len() as u16 + 2).min(area.width.saturating_sub(x));
        let rect = Rect::new(x, 3, width, 1);
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(if tab == InboxTab::Diffs {
                ACCENT
            } else {
                DIM
            })),
            rect,
        );
        if app.local.setup.is_none() {
            app.hits.push((rect, super::Action::SetInbox(tab)));
        }
        x += width;
    }
    let inner = Rect::new(
        2,
        5,
        area.width.saturating_sub(4),
        area.height.saturating_sub(7),
    );
    let mut rows: Vec<(String, Option<Action>)> = Vec::new();
    let mut cursor = None;
    match &app.local.setup {
        Some(Setup::Roots(editor)) => {
            rows.push((
                "Base directories · one path per line · Ctrl+Enter scan · Esc cancel".into(),
                None,
            ));
            let (lines, (cx, cy)) = editor.styled_layout(
                inner.width as usize,
                Style::default().bg(ACCENT).fg(crate::ui::INK),
            );
            let available = inner.height.saturating_sub(4) as usize;
            let start = cy.saturating_sub(available.saturating_sub(1));
            for (i, line) in lines.into_iter().skip(start).take(available).enumerate() {
                frame.render_widget(
                    Paragraph::new(line),
                    Rect::new(inner.x, inner.y + 1 + i as u16, inner.width, 1),
                );
            }
            if available > 0 {
                cursor = Some((
                    inner.x + (cx as u16).min(inner.width.saturating_sub(1)),
                    inner.y + 1 + (cy - start) as u16,
                ));
            }
            rows.resize_with(available + 1, || (String::new(), None));
            rows.push(("[ Scan repositories ]".into(), Some(Action::Scan)));
        }
        Some(Setup::Select {
            repos,
            chosen,
            selected,
            ..
        }) => {
            rows.push((
                "Space toggle · Enter track selected repos and their worktrees · e edit roots"
                    .into(),
                None,
            ));
            let height = inner.height.saturating_sub(4) as usize;
            for (index, repo) in repos
                .iter()
                .enumerate()
                .skip(selected.saturating_sub(height.saturating_sub(1)))
                .take(height)
            {
                rows.push((
                    format!(
                        "{} [{}] {}",
                        if index == *selected { ">" } else { " " },
                        if chosen.contains(repo) { "x" } else { " " },
                        repo.display()
                    ),
                    Some(Action::Toggle(index)),
                ));
            }
            if repos.is_empty() {
                rows.push(("No repositories found.".into(), None));
            }
            rows.push((
                format!("[ Track {} selected repositories ]", chosen.len()),
                Some(Action::Save),
            ));
        }
        None => {
            rows.push((
                "Local repositories and worktrees · Enter open · r refresh · e configure/rescan"
                    .into(),
                Some(Action::Configure),
            ));
            let height = inner.height.saturating_sub(3) as usize;
            for (index, path) in app
                .local
                .entries
                .iter()
                .enumerate()
                .skip(app.local.selected.saturating_sub(height.saturating_sub(1)))
                .take(height)
            {
                rows.push((
                    format!(
                        "{} {}",
                        if index == app.local.selected {
                            ">"
                        } else {
                            " "
                        },
                        path.display()
                    ),
                    Some(Action::Open(index)),
                ));
            }
            if app.local.entries.is_empty() && !app.local.loading {
                rows.push((
                    "No repositories tracked. Press e to choose directories.".into(),
                    Some(Action::Configure),
                ));
            }
        }
    }
    if app.local.loading {
        rows.push(("Loading local repositories…".into(), None));
    }
    if let Some(error) = &app.local.error {
        rows.push((error.clone(), None));
    }
    for (index, (text, action)) in rows.into_iter().take(inner.height as usize).enumerate() {
        let rect = Rect::new(inner.x, inner.y + index as u16, inner.width, 1);
        if !text.is_empty() {
            frame.render_widget(
                Paragraph::new(crate::model::clean(&text)).style(Style::default().fg(TEXT)),
                rect,
            );
        }
        if let Some(action) = action {
            app.hits.push((rect, super::Action::Local(action)));
        }
    }
    if let Some(cursor) = cursor {
        frame.set_cursor_position(cursor);
    }
}
