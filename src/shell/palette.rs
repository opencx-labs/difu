use super::*;
use crate::{
    agents::pr_cache,
    app::{Action as ReviewAction, Modal as ReviewModal, View},
    editor::Editor,
    model::{ModelPurpose, PrSummary},
    process::Cancel,
    ui::{BORDER, INK, TEXT},
    workflow::{WAction, Wizard},
};
use anyhow::{Context, Result};
use ratatui::{
    text::{Line, Span},
    widgets::{Block, Borders, Clear},
};
use std::{
    collections::BTreeMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
enum Action {
    Session(String),
    Preview(PrSummary),
    AgentCommand(String),
    AgentMenu(usize),
    ReviewHome(usize),
    ReviewCommand(usize),
}
impl Action {
    fn key(&self) -> String {
        match self {
            Self::Session(id) => format!("session:{id}"),
            Self::Preview(pr) => format!("pr:{}", pr.key.id()),
            Self::AgentCommand(name) => format!("command:{name}"),
            Self::AgentMenu(index) => format!("agent:{index}"),
            Self::ReviewHome(index) => format!("review-home:{index}"),
            Self::ReviewCommand(index) => format!("review:{index}"),
        }
    }
}
struct Item {
    title: String,
    detail: String,
    search: String,
    category: &'static str,
    action: Action,
    prs: Vec<PrKey>,
    pinned: bool,
}
enum Update {
    Cache(Result<Vec<PrSummary>, String>),
    Lookup(String, Result<Vec<PrSummary>, String>),
}
pub(super) struct Palette {
    query: Editor,
    selected: usize,
    prs: Vec<PrSummary>,
    storage: Storage,
    sender: mpsc::Sender<Update>,
    receiver: mpsc::Receiver<Update>,
    loaded: bool,
    refreshed: Instant,
    changed: Instant,
    attempted: Option<String>,
    lookup: Option<Cancel>,
    error: Option<String>,
    hits: Vec<(Rect, usize)>,
    viewport: usize,
}
impl Palette {
    pub fn new(storage: Storage) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut palette = Self {
            query: Editor::default(),
            selected: 0,
            prs: Vec::new(),
            storage,
            sender,
            receiver,
            loaded: false,
            refreshed: Instant::now(),
            changed: Instant::now(),
            attempted: None,
            lookup: None,
            error: None,
            hits: Vec::new(),
            viewport: 1,
        };
        palette.load_cache();
        palette
    }
    fn load_cache(&mut self) {
        self.refreshed = Instant::now();
        let storage = self.storage.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = (|| -> Result<Vec<PrSummary>> {
                let mut prs = storage.load_inbox(pr_cache::PERSONAL)?.unwrap_or_default();
                prs.extend(storage.load_inbox(pr_cache::LOOKUPS)?.unwrap_or_default());
                Ok(prs)
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = sender.send(Update::Cache(result));
        });
    }
    fn changed(&mut self) {
        self.selected = 0;
        self.changed = Instant::now();
        self.attempted = None;
        self.error = None;
        if let Some(cancel) = self.lookup.take() {
            cancel.cancel();
        }
    }
    pub fn paste(&mut self, text: &str) {
        self.query.insert(&text.replace(['\n', '\r', '\t'], " "));
        self.changed();
    }
}
impl Drop for Palette {
    fn drop(&mut self) {
        if let Some(cancel) = &self.lookup {
            cancel.cancel();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Reference {
    repository: Option<String>,
    number: u64,
}
impl Reference {
    fn parse(query: &str) -> Option<Self> {
        let query = query.trim();
        let (repository, number) = if let Some((repository, number)) = query.rsplit_once('#') {
            (
                if repository.is_empty() {
                    None
                } else {
                    Some(repository.to_owned())
                },
                number,
            )
        } else {
            (None, query)
        };
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number = number.parse().ok().filter(|n| *n > 0)?;
        if let Some(repository) = &repository {
            let full = if repository.contains('/') {
                repository.clone()
            } else {
                format!("owner/{repository}")
            };
            crate::model::validate_repository(&full).ok()?;
        }
        Some(Self { repository, number })
    }
    fn matches(&self, key: &PrKey) -> bool {
        self.number == key.number
            && self.repository.as_ref().is_none_or(|repo| {
                repo.eq_ignore_ascii_case(&key.repo) || repo.eq_ignore_ascii_case(&key.repository())
            })
    }
}

fn score(query: &str, title: &str, search: &str) -> Option<usize> {
    let query = query.trim().trim_start_matches('/').to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let title = title.to_lowercase();
    let search = search.to_lowercase();
    if title.trim_start_matches('/').eq(&query) {
        return Some(0);
    }
    let mut score = 0;
    for term in query.split_whitespace() {
        if title.starts_with(term) {
            score += 1;
        } else if title.contains(term) {
            score += 3;
        } else if search.contains(term) {
            score += 6;
        } else {
            let mut chars = search.chars();
            if !term
                .chars()
                .all(|ch| chars.by_ref().any(|candidate| ch == candidate))
            {
                return None;
            }
            score += 20;
        }
    }
    Some(score)
}

fn lookup(reference: &Reference, storage: &Storage, cancel: &Cancel) -> Result<Vec<PrSummary>> {
    let name = reference
        .repository
        .as_ref()
        .context("Use repo#number for GitHub lookup")?;
    let repositories = if name.contains('/') {
        vec![name.clone()]
    } else {
        // Resolve short names without guessing an owner; multiple matches remain separate results.
        let matches = |repos: Vec<String>| {
            repos
                .into_iter()
                .filter(|repo| {
                    repo.rsplit('/')
                        .next()
                        .is_some_and(|short| short.eq_ignore_ascii_case(name))
                })
                .collect::<Vec<_>>()
        };
        let mut known = storage.load_repositories()?.unwrap_or_default();
        for cache in [pr_cache::PERSONAL, pr_cache::LOOKUPS] {
            known.extend(
                storage
                    .load_inbox(cache)?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|pr| pr.key.repository()),
            );
        }
        known.sort();
        known.dedup();
        let cached = matches(known);
        if cached.is_empty() {
            let repositories = crate::github::repositories(cancel)?;
            storage.save_repositories(&repositories)?;
            matches(repositories)
        } else {
            cached
        }
    };
    anyhow::ensure!(
        !repositories.is_empty(),
        "No accessible repository named {name}; try owner/repo#{}",
        reference.number
    );
    let mut prs = Vec::new();
    let mut errors = Vec::new();
    for repository in repositories {
        cancel.check()?;
        let (owner, repo) = repository.split_once('/').context("Invalid repository")?;
        let key = PrKey {
            owner: owner.into(),
            repo: repo.into(),
            number: reference.number,
        };
        match crate::github::pr_summary(&key, cancel) {
            Ok(pr) => prs.push(pr),
            Err(error) => errors.push(format!("{}: {error:#}", key.id())),
        }
    }
    anyhow::ensure!(!prs.is_empty(), "{}", errors.join("\n"));
    cancel.check()?;
    let mut cached = storage.load_inbox(pr_cache::LOOKUPS)?.unwrap_or_default();
    cached.retain(|old| !prs.iter().any(|pr| pr.key == old.key));
    cached.extend(prs.clone());
    storage.save_inbox(pr_cache::LOOKUPS, &cached)?;
    Ok(prs)
}

impl Shell {
    pub(super) fn palette_selection(&self) -> Option<String> {
        let palette = self.palette.as_ref()?;
        self.palette_items()
            .get(palette.selected)
            .map(|item| item.action.key())
    }
    pub(super) fn restore_palette_selection(&mut self, key: Option<String>) {
        if let Some(key) = key {
            let items = self.palette_items();
            if let Some(palette) = &mut self.palette {
                palette.selected = items
                    .iter()
                    .position(|item| item.action.key() == key)
                    .unwrap_or_else(|| palette.selected.min(items.len().saturating_sub(1)));
            }
        }
    }
    fn palette_prs(&self) -> Vec<PrSummary> {
        let mut prs = BTreeMap::new();
        for pr in self
            .palette
            .iter()
            .flat_map(|p| &p.prs)
            .chain(&self.reviews.inbox)
        {
            prs.entry(pr.key.id()).or_insert_with(|| pr.clone());
        }
        for session in &self.agents.summaries {
            for pr in self.agents.session_prs(&session.id) {
                prs.entry(pr.key.id()).or_insert_with(|| PrSummary {
                    key: pr.key.clone(),
                    title: session.title.clone(),
                    author: String::new(),
                    updated: pr.updated.clone(),
                    created: String::new(),
                    stats: None,
                    stats_error: false,
                    draft: pr.draft,
                });
            }
        }
        prs.into_values().collect()
    }
    fn palette_review(&self) -> &App {
        if self.agents_active
            && let Some(app) = self.agents.palette_review()
        {
            return app;
        }
        &self.reviews
    }
    fn palette_items(&self) -> Vec<Item> {
        let Some(palette) = &self.palette else {
            return Vec::new();
        };
        let query = palette.query.text();
        let reference = Reference::parse(&query);
        let mut items = Vec::new();
        for session in self.agents.summaries.iter().filter(|s| s.kind != "Guide") {
            let prs = self
                .agents
                .session_prs(&session.id)
                .iter()
                .map(|pr| pr.key.clone())
                .collect::<Vec<_>>();
            let repository = session.repository.as_ref().unwrap_or(&session.workspace);
            let detail = format!(
                "{} · {} · {}{}{}",
                repository.file_name().unwrap_or_default().to_string_lossy(),
                session.branch.as_deref().unwrap_or("no branch"),
                session
                    .workspace
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                prs.iter()
                    .map(|pr| format!(" · {}", pr.id()))
                    .collect::<String>(),
                if session.archived { " · archived" } else { "" }
            );
            items.push(Item {
                title: session.title.clone(),
                search: format!(
                    "{} {detail} {} {}",
                    session.title,
                    repository.display(),
                    session.workspace.display()
                ),
                detail,
                category: "Session",
                action: Action::Session(session.id.clone()),
                prs,
                pinned: self.agents.pinned_sessions.contains(&session.id),
            });
        }
        for pr in self.palette_prs() {
            items.push(Item {
                title: format!("{} · {}", pr.key.id(), pr.title),
                detail: "Open pull request preview".into(),
                search: format!("{} {}", pr.key.id(), pr.title),
                category: "PR",
                prs: vec![pr.key.clone()],
                action: Action::Preview(pr),
                pinned: false,
            });
        }
        for (name, description, _) in self.agents.palette_commands() {
            items.push(Item {
                title: name.clone(),
                search: format!("{name} {description}"),
                detail: description,
                category: "Agents",
                action: Action::AgentCommand(name),
                prs: Vec::new(),
                pinned: false,
            });
        }
        for (index, label) in self.agents.menu_entries().into_iter().enumerate() {
            items.push(Item {
                title: label.into(),
                detail: self
                    .agents
                    .selected
                    .as_ref()
                    .and_then(|id| self.agents.summaries.iter().find(|s| &s.id == id))
                    .map(|session| session.title.clone())
                    .unwrap_or_else(|| "Select an agent session first".into()),
                search: label.into(),
                category: "Agents",
                action: Action::AgentMenu(index),
                prs: Vec::new(),
                pinned: false,
            });
        }
        for (index, label) in [
            "PR controls",
            "Memory management · worktrees",
            "Default guide model",
            "Default conflict resolve model",
        ]
        .iter()
        .enumerate()
        {
            items.push(Item {
                title: (*label).into(),
                detail: "Review action".into(),
                search: (*label).into(),
                category: "Reviews",
                action: Action::ReviewHome(index),
                prs: Vec::new(),
                pinned: false,
            });
        }
        let review = self.palette_review();
        let detail = review.review().and_then(|r| r.detail.as_ref());
        let draft = detail.filter(|pr| pr.state == "open").map(|pr| pr.draft);
        for (index, label) in crate::workflow::control_commands("", draft) {
            items.push(Item {
                title: label.into(),
                detail: detail
                    .map(|pr| pr.key.id())
                    .unwrap_or_else(|| "Open a PR first".into()),
                search: label.into(),
                category: "Reviews",
                action: Action::ReviewCommand(index),
                prs: Vec::new(),
                pinned: false,
            });
        }
        let mut matched = items.into_iter().filter_map(|item| {
            let rank = if let Some(reference) = &reference {
                if !item.prs.iter().any(|pr| reference.matches(pr)) { return None; }
                0
            } else if query.trim_start().starts_with('/') {
                let name = query.split_whitespace().next().unwrap_or_default();
                if matches!(&item.action, Action::AgentCommand(command) if command.eq_ignore_ascii_case(name)) {
                    0
                } else { score(&query, &item.title, &item.search)? }
            } else { score(&query, &item.title, &item.search)? };
            let category = match item.category {
                "Session" => 0,
                "PR" if !query.is_empty() => 1,
                "PR" => 3,
                _ => 2,
            };
            Some((rank, category, item))
        }).collect::<Vec<_>>();
        matched.sort_by_key(|(score, category, item)| (*score, *category, !item.pinned));
        matched.into_iter().map(|(_, _, item)| item).collect()
    }
    pub(super) fn tick_palette(&mut self) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        while let Ok(update) = palette.receiver.try_recv() {
            match update {
                Update::Cache(result) => {
                    palette.loaded = true;
                    match result {
                        Ok(prs) => {
                            for pr in prs {
                                if let Some(old) =
                                    palette.prs.iter_mut().find(|old| old.key == pr.key)
                                {
                                    if pr.updated >= old.updated {
                                        *old = pr;
                                    }
                                } else {
                                    palette.prs.push(pr);
                                }
                            }
                        }
                        Err(error) => palette.error = Some(error),
                    }
                }
                Update::Lookup(query, result) if palette.query.text().trim() == query => {
                    palette.lookup = None;
                    match result {
                        Ok(prs) => {
                            palette
                                .prs
                                .retain(|old| !prs.iter().any(|pr| pr.key == old.key));
                            palette.prs.extend(prs);
                        }
                        Err(error) => palette.error = Some(error),
                    }
                }
                _ => {}
            }
        }
        if palette.refreshed.elapsed() >= Duration::from_secs(30) {
            palette.load_cache();
        }
        let query = palette.query.text().trim().to_owned();
        if !palette.loaded
            || palette.changed.elapsed() < Duration::from_millis(300)
            || palette.attempted.as_ref() == Some(&query)
        {
            return;
        }
        let Some(reference) = Reference::parse(&query).filter(|r| r.repository.is_some()) else {
            return;
        };
        if self
            .palette_prs()
            .iter()
            .any(|pr| reference.matches(&pr.key))
        {
            return;
        }
        let Some(palette) = &mut self.palette else {
            return;
        };
        palette.attempted = Some(query.clone());
        let cancel = Cancel::default();
        palette.lookup = Some(cancel.clone());
        let storage = palette.storage.clone();
        let sender = palette.sender.clone();
        thread::spawn(move || {
            let result = lookup(&reference, &storage, &cancel).map_err(|e| format!("{e:#}"));
            if !cancel.cancelled() {
                let _ = sender.send(Update::Lookup(query, result));
            }
        });
    }
    pub(super) fn palette_key(&mut self, key: KeyEvent) {
        let items = self.palette_items();
        let Some(palette) = &mut self.palette else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Enter => {
                let selected = palette.selected;
                self.activate_palette(selected);
            }
            KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                palette.selected = (palette.selected + 1).min(items.len().saturating_sub(1))
            }
            KeyCode::BackTab => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::PageDown => {
                palette.selected =
                    (palette.selected + palette.viewport).min(items.len().saturating_sub(1))
            }
            KeyCode::PageUp => palette.selected = palette.selected.saturating_sub(palette.viewport),
            KeyCode::Char('c')
                if key
                    .modifiers
                    .intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL) =>
            {
                self.agents.clipboard = palette.query.selected_text();
            }
            KeyCode::Char('p') if key.modifiers == KeyModifiers::SUPER => {
                if let Some(Item {
                    action: Action::Session(id),
                    ..
                }) = items.get(palette.selected)
                {
                    self.agents.toggle_pin(id);
                }
            }
            _ => {
                let before = palette.query.text();
                palette.query.key(key);
                if before != palette.query.text() {
                    palette.changed();
                }
            }
        }
    }
    fn activate_palette(&mut self, index: usize) {
        let Some(item) = self.palette_items().into_iter().nth(index) else {
            return;
        };
        let query = self
            .palette
            .as_ref()
            .map(|p| p.query.text())
            .unwrap_or_default();
        self.palette = None;
        match item.action {
            Action::Session(id) => {
                self.switch(true);
                self.agents.open_session(id);
            }
            Action::Preview(pr) => {
                self.reviews_started = true;
                self.switch(false);
                let app = &mut self.reviews;
                app.modal = None;
                app.home = true;
                app.filters = Default::default();
                app.inbox_tab = crate::model::InboxTab::MyPrs;
                let index = app
                    .inbox
                    .iter()
                    .position(|p| p.key == pr.key)
                    .unwrap_or_else(|| {
                        app.inbox.push(pr);
                        app.inbox.len() - 1
                    });
                app.select(index);
                app.action(ReviewAction::SetView(View::Overview));
            }
            Action::AgentCommand(name) => {
                self.switch(true);
                self.agents.run_palette_command(&name, &query);
            }
            Action::AgentMenu(index) => {
                self.switch(true);
                self.agents.menu_action(index);
            }
            action @ (Action::ReviewHome(_) | Action::ReviewCommand(_)) => {
                let embedded = self.agents_active && self.agents.palette_review().is_some();
                if !embedded {
                    self.switch(false);
                }
                let app = if embedded {
                    let Some(app) = self.agents.palette_review_mut() else {
                        return;
                    };
                    app
                } else {
                    &mut self.reviews
                };
                match action {
                    Action::ReviewHome(0) => app.workflow_action(WAction::Controls),
                    Action::ReviewHome(1) => app.workflow_action(WAction::Trees),
                    Action::ReviewHome(2) => app.load_models_for(ModelPurpose::Guide),
                    Action::ReviewHome(3) => app.load_models_for(ModelPurpose::Conflicts),
                    Action::ReviewCommand(index) => {
                        app.workflow_action(WAction::Controls);
                        if matches!(&app.modal, Some(ReviewModal::Workflow(w)) if matches!(w.as_ref(), Wizard::Controls { .. }))
                        {
                            app.workflow_action(WAction::Choose(index));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    pub(super) fn palette_mouse(&mut self, mouse: MouseEvent) {
        let count = self.palette_items().len();
        let Some(palette) = &mut self.palette else {
            return;
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, index)) = palette
                    .hits
                    .iter()
                    .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
                {
                    let index = *index;
                    self.activate_palette(index);
                }
            }
            MouseEventKind::ScrollDown => {
                palette.selected = (palette.selected + 3).min(count.saturating_sub(1))
            }
            MouseEventKind::ScrollUp => palette.selected = palette.selected.saturating_sub(3),
            _ => {}
        }
    }
    pub(super) fn draw_palette(&mut self, frame: &mut Frame) {
        let items = self.palette_items();
        let Some(palette) = &mut self.palette else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(6).min(112);
        let height = area.height.saturating_sub(4).min(30);
        if width < 12 || height < 8 {
            return;
        }
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 3,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Command palette · ⌘K ")
                .style(Style::default().bg(INK).fg(TEXT))
                .border_style(Style::default().fg(ACCENT)),
            rect,
        );
        let input = Rect::new(rect.x + 2, rect.y + 2, rect.width - 4, 1);
        let (lines, (x, y)) = palette
            .query
            .styled_layout(input.width as usize, Style::default().bg(ACCENT).fg(INK));
        frame.render_widget(
            Paragraph::new(lines.get(y).cloned().unwrap_or_default())
                .style(Style::default().fg(TEXT).bg(INK)),
            input,
        );
        if palette.query.chars.is_empty() {
            frame.render_widget(
                Paragraph::new("Search sessions, repos, branches, PRs or commands…")
                    .style(Style::default().fg(DIM)),
                input,
            );
        }
        frame.set_cursor_position((input.x + (x as u16).min(input.width - 1), input.y));
        frame.render_widget(
            Paragraph::new("─".repeat((rect.width - 4) as usize))
                .style(Style::default().fg(BORDER)),
            Rect::new(input.x, input.y + 2, input.width, 1),
        );
        palette.viewport = usize::from(height.saturating_sub(8) / 2).max(1);
        palette.selected = palette.selected.min(items.len().saturating_sub(1));
        let offset = palette.selected.saturating_sub(palette.viewport - 1);
        palette.hits.clear();
        for (row, (index, item)) in items
            .iter()
            .enumerate()
            .skip(offset)
            .take(palette.viewport)
            .enumerate()
        {
            let selected = index == palette.selected;
            let style = if selected {
                Style::default().bg(ACCENT).fg(INK)
            } else {
                Style::default().bg(INK).fg(TEXT)
            };
            let detail_style = if selected { style } else { style.fg(DIM) };
            let item_rect = Rect::new(input.x, rect.y + 5 + row as u16 * 2, input.width, 2);
            let title = format!(
                "{}{}{}  {}",
                if selected { "› " } else { "  " },
                if item.pinned { "◆ " } else { "" },
                item.category,
                item.title
            );
            let lines = vec![
                Line::from(Span::styled(
                    crate::ui::crop(&crate::model::clean(&title), 0, input.width as usize),
                    style,
                )),
                Line::from(Span::styled(
                    crate::ui::crop(
                        &format!("  {}", crate::model::clean(&item.detail)),
                        0,
                        input.width as usize,
                    ),
                    detail_style,
                )),
            ];
            frame.render_widget(Paragraph::new(lines).style(style), item_rect);
            palette.hits.push((item_rect, index));
        }
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(if !palette.loaded {
                    "Loading cached PRs…"
                } else {
                    "No matching results"
                })
                .style(Style::default().fg(DIM)),
                Rect::new(input.x, rect.y + 5, input.width, 1),
            );
        }
        let status = if let Some(error) = &palette.error {
            crate::model::clean(error)
        } else if palette.lookup.is_some() {
            "Searching GitHub…".into()
        } else {
            format!(
                "{} results · ↑↓ Navigate · Enter Open · ⌘P Pin session · Esc Close",
                items.len()
            )
        };
        frame.render_widget(
            Paragraph::new(crate::ui::crop(&status, 0, input.width as usize))
                .style(Style::default().fg(DIM)),
            Rect::new(input.x, rect.bottom() - 2, input.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Job, Launch, Session, Status};

    fn fixture(storage: Storage) -> Result<Shell> {
        let pr = PrSummary {
            key: PrKey {
                owner: "example".into(),
                repo: "opencx".into(),
                number: 1524,
            },
            title: "Billing improvements".into(),
            author: "me".into(),
            updated: "2026-09-25".into(),
            created: String::new(),
            stats: None,
            stats_error: false,
            draft: false,
        };
        crate::storage::atomic_json(
            &storage.cache.join("agent-prs.json"),
            &serde_json::json!({
                "one": {"workspace":"/tmp/worktrees/billing-edit", "pr":{
                    "key":pr.key, "state":"OPEN", "draft":false, "conflicts":false
                }}
            }),
        )?;
        let mut shell = Shell::new(storage.clone(), Config::default(), None);
        shell.reviews_started = true;
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: "/tmp/opencx".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Invoice fixes".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.workspace = Some("/tmp/worktrees/billing-edit".into());
        session.branch = Some("fix/invoices".into());
        shell.agents.summaries.push(session.summary());
        shell.agents.sessions.insert(session.id.clone(), session);
        let mut palette = Palette::new(storage);
        palette.prs.push(pr);
        palette.loaded = true;
        shell.palette = Some(palette);
        Ok(shell)
    }
    fn query(shell: &mut Shell, text: &str) {
        if let Some(palette) = &mut shell.palette {
            palette.query = Editor::from(text);
            palette.changed();
        }
    }
    #[test]
    fn search_matches_session_metadata_and_returns_pr_with_connected_sessions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut shell = fixture(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        })?;
        for text in ["Invoice fixes", "opencx", "fix/invoices", "billing-edit"] {
            query(&mut shell, text);
            assert!(
                shell
                    .palette_items()
                    .iter()
                    .any(|item| matches!(&item.action, Action::Session(id) if id == "one")),
                "{text}"
            );
        }
        for text in ["1524", "#1524", "opencx#1524", "example/opencx#1524"] {
            query(&mut shell, text);
            let items = shell.palette_items();
            assert_eq!(items.len(), 2, "{text}");
            assert!(
                items.iter().any(
                    |item| matches!(&item.action, Action::Preview(pr) if pr.key.number == 1524)
                )
            );
            assert!(
                items
                    .iter()
                    .any(|item| matches!(&item.action, Action::Session(id) if id == "one"))
            );
            shell.palette.as_mut().context("palette")?.changed =
                Instant::now() - Duration::from_secs(1);
            shell.tick_palette();
            assert!(shell.palette.as_ref().context("palette")?.lookup.is_none());
        }
        query(&mut shell, "987654");
        shell.palette.as_mut().context("palette")?.changed =
            Instant::now() - Duration::from_secs(1);
        shell.tick_palette();
        assert!(
            shell
                .palette
                .as_ref()
                .context("palette")?
                .attempted
                .is_none()
        );
        assert!(shell.palette_items().is_empty());
        Ok(())
    }
    #[test]
    fn commands_from_both_tabs_and_session_switch_preserve_composer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut shell = fixture(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        })?;
        shell
            .agents
            .positions
            .entry("one".into())
            .or_default()
            .draft = Editor::from("Unsent draft");
        for (name, _, _) in shell.agents.palette_commands() {
            query(&mut shell, &name);
            assert!(shell.palette_items().iter().any(
                |item| matches!(&item.action, Action::AgentCommand(command) if command == &name)
            ));
        }
        query(&mut shell, "squash merge");
        assert!(
            shell
                .palette_items()
                .iter()
                .any(|item| matches!(item.action, Action::ReviewCommand(2)))
        );
        query(&mut shell, "/rename New title");
        assert!(
            matches!(shell.palette_items().first().map(|i| &i.action), Some(Action::AgentCommand(name)) if name == "/rename")
        );
        query(&mut shell, "Invoice fixes");
        shell.agents_active = false;
        let index = shell
            .palette_items()
            .iter()
            .position(|item| matches!(item.action, Action::Session(_)))
            .context("session")?;
        shell.activate_palette(index);
        assert!(shell.agents_active && shell.agents.drilled);
        assert_eq!(shell.agents.selected.as_deref(), Some("one"));
        assert_eq!(
            shell
                .agents
                .positions
                .get("one")
                .context("position")?
                .draft
                .text(),
            "Unsent draft"
        );
        // Escape closes the global overlay without changing the active tab or draft.
        shell.agents_active = false;
        shell.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SUPER));
        assert!(shell.palette.is_some());
        shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(shell.palette.is_none() && !shell.agents_active);
        Ok(())
    }
    #[test]
    fn palette_preview_opens_cached_pr_despite_previous_review_filters() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut shell = fixture(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        })?;
        let key = shell
            .palette
            .as_ref()
            .context("palette")?
            .prs
            .first()
            .context("pr")?
            .key
            .clone();
        // Already loading avoids network work in this rendering/navigation test.
        shell.reviews.reviews.insert(
            key.id(),
            crate::app::Review {
                loading: true,
                ..Default::default()
            },
        );
        shell.reviews.filters.prs = Editor::from("does not match");
        query(&mut shell, "1524");
        let index = shell
            .palette_items()
            .iter()
            .position(|item| matches!(item.action, Action::Preview(_)))
            .context("preview")?;
        shell.activate_palette(index);
        assert!(!shell.agents_active);
        assert!(!shell.reviews.home);
        assert_eq!(shell.reviews.view, View::Overview);
        assert_eq!(shell.reviews.key(), Some(key.id()));
        Ok(())
    }
}
