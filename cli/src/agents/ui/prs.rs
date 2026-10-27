//! Cached, asynchronous PR discovery for visible coding sessions.
use super::*;
use crate::{github::SessionPr, process::Cancel};
use std::path::PathBuf;

struct Update {
    id: String,
    workspace: PathBuf,
    branch: Option<String>,
    revision: u64,
    result: Result<Vec<Cached>, String>,
}
use crate::agents::pr_cache::{self, SESSION_LINKS, SessionLink as Cached};
pub(super) struct State {
    cache: pr_cache::SessionLinks,
    loaded_at: Option<Instant>,
    loading: Option<mpsc::Receiver<Vec<(Summary, pr_cache::Snapshot)>>>,
    pub visible: HashSet<String>,
    refreshed: HashMap<String, Instant>,
    revisions: HashMap<String, u64>,
    branches: HashMap<String, Option<String>>,
    pending: HashMap<String, Cancel>,
    stale: HashSet<String>,
    opened: Option<String>,
    history: HashSet<String>,
    sender: mpsc::Sender<Update>,
    receiver: mpsc::Receiver<Update>,
}
impl State {
    pub fn load(storage: &Storage) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut cache = pr_cache::load_links(&storage.cache.join("agent-prs.json"));
        for (id, links) in pr_cache::load_links(&storage.cache.join(SESSION_LINKS)) {
            pr_cache::merge_links(cache.entry(id).or_default(), links);
        }
        let stale = cache.keys().cloned().collect();
        Self {
            cache,
            loaded_at: None,
            loading: None,
            visible: HashSet::new(),
            refreshed: HashMap::new(),
            revisions: HashMap::new(),
            branches: HashMap::new(),
            pending: HashMap::new(),
            stale,
            opened: None,
            history: HashSet::new(),
            sender,
            receiver,
        }
    }
    pub fn forget(
        &mut self,
        id: &str,
        storage: &Storage,
        writer: &cache_io::Writer,
    ) -> anyhow::Result<()> {
        if let Some(cancel) = self.pending.remove(id) {
            cancel.cancel();
        }
        self.cache.remove(id);
        self.refreshed.remove(id);
        self.revisions.remove(id);
        self.branches.remove(id);
        self.stale.remove(id);
        self.history.remove(id);
        self.queue_save(storage, writer)
    }
    #[cfg(test)]
    pub fn get(&self, id: &str) -> Option<&SessionPr> {
        self.all(id).first().map(|c| &c.pr)
    }
    pub fn all(&self, id: &str) -> &[Cached] {
        self.cache.get(id).map(Vec::as_slice).unwrap_or_default()
    }
    pub fn stale(&self, id: &str) -> bool {
        self.stale.contains(id)
    }
    pub fn refresh(&mut self, id: &str) {
        self.history.insert(id.to_owned());
        self.refreshed.remove(id);
    }
    fn due(&self, id: &str) -> bool {
        !self.pending.contains_key(id)
            && self.refreshed.get(id).is_none_or(|t| {
                t.elapsed() >= Duration::from_secs(crate::github::PR_REFRESH_SECONDS)
            })
    }
    fn receive(&mut self, update: Update) -> bool {
        self.pending.remove(&update.id);
        self.refreshed.insert(update.id.clone(), Instant::now());
        if let Ok(links) = update.result {
            self.stale.remove(&update.id);
            self.branches
                .insert(update.id.clone(), update.branch.clone());
            let cached = self.cache.entry(update.id).or_default();
            pr_cache::merge_links(cached, links);
            pr_cache::sort_links(cached, &update.workspace, update.branch.as_deref());
            true
        } else {
            self.stale.insert(update.id);
            false
        }
    }
    fn queue_save(&self, storage: &Storage, writer: &cache_io::Writer) -> anyhow::Result<()> {
        let storage = storage.clone();
        let cache = self.cache.clone();
        writer.save("session PRs".into(), move || {
            std::fs::create_dir_all(&storage.cache)?;
            pr_cache::save_links(&storage, &storage.cache.join("agent-prs.json"), &cache)
        })
    }
    #[cfg(test)]
    fn save(&self, storage: &Storage) -> anyhow::Result<()> {
        std::fs::create_dir_all(&storage.cache)?;
        pr_cache::save_links(storage, &storage.cache.join("agent-prs.json"), &self.cache)
    }
}
impl Drop for State {
    fn drop(&mut self) {
        for token in self.pending.values() {
            token.cancel();
        }
    }
}
pub(super) fn color(pr: &SessionPr) -> ratatui::style::Color {
    match pr.label() {
        "Merged" => crate::ui::PURPLE,
        "Closed" | "Has conflicts" => RED,
        "Open" => GREEN,
        _ => DIM,
    }
}
impl Ui {
    pub(crate) fn configure_images(&mut self, images: &crate::images::State, obscured: bool) {
        self.status_icons
            .configure(images.picker.as_ref(), obscured);
        if let Some(panels::View::PullRequest { app }) = &mut self.panels.view
            && app.images.supported() != images.supported()
        {
            app.images.picker = images.picker.clone();
            app.invalidate();
        }
    }
    pub(super) fn tick_prs(&mut self, visible: bool) {
        for session in &self.summaries {
            if self
                .prs
                .revisions
                .insert(session.id.clone(), session.workspace_revision)
                .is_some_and(|old| old != session.workspace_revision)
            {
                if let Some(cancel) = self.prs.pending.remove(&session.id) {
                    cancel.cancel();
                }
                self.prs.refreshed.remove(&session.id);
            }
        }
        if visible
            && self.prs.loading.is_none()
            && self
                .prs
                .loaded_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
        {
            self.prs.loaded_at = Some(Instant::now());
            let storage = self.storage.clone();
            let summaries = self.summaries.clone();
            let mut cache = self.prs.cache.clone();
            let (sender, receiver) = mpsc::channel();
            self.prs.loading = Some(receiver);
            thread::spawn(move || {
                for (id, links) in pr_cache::load_links(&storage.cache.join(SESSION_LINKS)) {
                    pr_cache::merge_links(cache.entry(id).or_default(), links);
                }
                let snapshots = summaries
                    .into_iter()
                    .map(|session| {
                        let mut saved = pr_cache::snapshot(&storage, &session.id);
                        let mut links = cache.remove(&session.id).unwrap_or_default();
                        pr_cache::hydrate_links(&storage, &mut links);
                        saved.retain_connected(&mut links);
                        pr_cache::merge_links(&mut links, saved.links);
                        saved.links = links;
                        (session, saved)
                    })
                    .collect();
                let _ = sender.send(snapshots);
                // tick_prs compares the result before requesting a redraw; an
                // unchanged disk refresh must leave an idle view quiet.
            });
        }
        let disk = self
            .prs
            .loading
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if disk.is_some() {
            self.prs.loading = None;
        }
        let updates = self.prs.receiver.try_iter().collect::<Vec<_>>();
        if disk.is_some() || !updates.is_empty() {
            let previous = self.prs.cache.clone();
            let previous_stale = self.prs.stale.clone();
            for (session, saved) in disk.into_iter().flatten() {
                // A checkout switch or deletion invalidates in-flight disk reads.
                if !self.summaries.iter().any(|current| {
                    current.id == session.id
                        && current.workspace_revision == session.workspace_revision
                }) || self.deleting.contains(&session.id)
                {
                    continue;
                }
                let links = self.prs.cache.entry(session.id.clone()).or_default();
                saved.retain_connected(links);
                self.prs
                    .branches
                    .insert(session.id.clone(), saved.branch.clone());
                let stale = saved.error.is_some()
                    || saved.checked_at == 0
                    || (saved.links.iter().any(|link| link.pr.state == "OPEN")
                        && crate::github::polling::now().saturating_sub(saved.checked_at) > 600);
                if stale {
                    self.prs.stale.insert(session.id.clone());
                } else {
                    self.prs.stale.remove(&session.id);
                }
                pr_cache::merge_links(links, saved.links);
            }
            for update in updates {
                if self.summaries.iter().any(|s| {
                    s.id == update.id
                        && s.workspace == update.workspace
                        && s.workspace_revision == update.revision
                }) && !self.deleting.contains(&update.id)
                {
                    self.prs.receive(update);
                }
            }
            for session in &self.summaries {
                if let Some(links) = self.prs.cache.get_mut(&session.id) {
                    let branch = self
                        .prs
                        .branches
                        .get(&session.id)
                        .map(|b| b.as_deref())
                        .unwrap_or(session.branch.as_deref());
                    pr_cache::sort_links(links, &session.workspace, branch);
                }
                if previous.get(&session.id) != self.prs.cache.get(&session.id)
                    || previous_stale.contains(&session.id) != self.prs.stale(&session.id)
                {
                    crate::repaint::request();
                    self.sidebar.invalidate(&session.id);
                    if self.selected.as_ref() == Some(&session.id) {
                        self.changes_at = None;
                    }
                }
            }
            if previous != self.prs.cache
                && let Err(error) = self.prs.queue_save(&self.storage, &self.cache_writer)
            {
                self.notice = Some((format!("Cannot cache session PRs: {error:#}"), true));
            }
        }
        let opened = if visible && self.drilled {
            self.selected.clone()
        } else {
            None
        };
        if self.prs.opened != opened {
            if let Some(id) = &opened {
                self.prs.refresh(id);
            }
            self.prs.opened = opened;
        }
        if !visible {
            return;
        }
        for summary in &self.summaries {
            if summary.kind != "Coding" && summary.kind != "coding" {
                continue;
            }
            if summary.status == Status::Starting && !summary.worktree {
                continue;
            }
            if !self.prs.visible.contains(&summary.id)
                && self.selected.as_ref() != Some(&summary.id)
            {
                continue;
            }
            let history = self.prs.history.contains(&summary.id);
            if !history && !pr_cache::should_poll(summary, self.prs.all(&summary.id)) {
                continue;
            }
            if self.prs.pending.len() >= 2 || !self.prs.due(&summary.id) {
                continue;
            }
            let id = summary.id.clone();
            let workspace = summary.workspace.clone();
            let summary = summary.clone();
            let known = self.prs.all(&id).to_vec();
            self.prs.history.remove(&id);
            let storage = self.storage.clone();
            let refresh = if history {
                pr_cache::Refresh::Open
            } else {
                pr_cache::Refresh::Visible
            };
            let cancel = Cancel::default();
            self.prs.pending.insert(id.clone(), cancel.clone());
            let sender = self.prs.sender.clone();
            thread::spawn(move || {
                let mut result =
                    pr_cache::refresh_session(&storage, &summary, known, refresh, &cancel)
                        .map_err(|e| format!("{e:#}"));
                let saved = pr_cache::snapshot(&storage, &summary.id);
                if summary.workspace_revision < saved.disconnected_before {
                    return;
                }
                if let Ok(links) = &mut result {
                    saved.retain_connected(links);
                }
                let _ = sender.send(Update {
                    id,
                    workspace,
                    branch: saved.branch,
                    revision: summary.workspace_revision,
                    result,
                });
            });
        }
    }
    pub(super) fn open_pr_panel(&mut self, index: usize) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(pr) = self.prs.all(&id).get(index).map(|link| link.pr.clone()) else {
            return;
        };
        let config = match self.storage.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some((format!("Cannot open PR: {error:#}"), true));
                return;
            }
        };
        let mut app = crate::app::App::new(self.storage.clone(), config);
        let key = pr.key.clone();
        app.start_embedded(key.clone());
        // Seed discovery from the session checkout. PR preparation resolves and
        // remembers its main clone so deleting this worktree cannot break reviews.
        if let Some(root) = self
            .prs
            .cache
            .get(&id)
            .and_then(|links| links.iter().find(|link| link.pr.key == key))
            .filter(|link| !link.attached)
            .map(|c| c.workspace.clone())
            .filter(|p| p.is_dir())
            && let Some(review) = app.reviews.get_mut(&key.id())
        {
            review.root = Some(root);
        }
        self.panels.session = Some(id);
        self.panels.view = Some(panels::View::PullRequest { app: Box::new(app) });
        self.panels.hidden = false;
        self.panels.focused = true;
        self.drilled = true;
        self.modal = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, Review, View as PrView},
        model::PrKey,
    };
    use anyhow::{Context, Result};

    fn pr() -> SessionPr {
        SessionPr {
            summary: None,
            key: PrKey {
                owner: "example".into(),
                repo: "project".into(),
                number: 42,
            },
            state: "OPEN".into(),
            draft: false,
            conflicts: false,
            head_branch: String::new(),
            head: String::new(),
            updated: String::new(),
        }
    }
    fn store(root: &std::path::Path) -> Storage {
        Storage {
            config: root.join("config.json"),
            cache: root.join("cache"),
        }
    }
    #[test]
    fn published_diffs_have_stable_tabs_and_reject_late_responses() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = store(temp.path());
        let mut ui = Ui::new(storage.clone(), &Config::default());
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: temp.path().into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.workspace_removed = true;
        ui.summaries.push(session.summary());
        ui.sessions.insert("one".into(), session);
        ui.selected = Some("one".into());
        ui.drilled = true;
        ui.changes_visible = true;
        ui.focus = Focus::ChangeTree;
        let first = pr();
        let mut second = pr();
        second.key.owner = "zother".into();
        second.key.repo = "repository".into();
        let patch = "diff --git a/published.rs b/published.rs\n--- a/published.rs\n+++ b/published.rs\n@@ -1 +1 @@\n-old\n+published\n";
        crate::github::store::diff(&storage, &first.key, patch)?;
        ui.prs.receive(Update {
            id: "one".into(),
            workspace: temp.path().into(),
            branch: None,
            revision: 0,
            result: Ok(vec![first.clone(), second.clone()]
                .into_iter()
                .map(|pr| Cached {
                    attached: pr.key == second.key,
                    workspace_revision: 0,
                    workspace: temp.path().into(),
                    pr,
                })
                .collect()),
        });
        ui.sync_change_pr("one");
        assert_eq!(ui.change_source("one").pr, Some(first.key.clone()));
        assert_eq!(
            ui.changes
                .get("one")
                .and_then(|d| d.files.first())
                .map(|f| f.path.as_str()),
            Some("published.rs")
        );
        for (key, expected) in [(']', &second.key), (']', &first.key), ('[', &second.key)] {
            ui.key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            assert_eq!(ui.change_source("one").pr.as_ref(), Some(expected));
        }
        ui.prs.cache.get_mut("one").context("links")?.reverse();
        ui.sync_change_pr("one");
        assert_eq!(
            ui.change_source("one").pr,
            Some(second.key.clone()),
            "sorting must not change the selected PR"
        );
        let source = ui.change_source("one");
        ui.changing = Some((source.clone(), 2));
        ui.receive_change_result(&source, 1, Ok(Reply::Changes("obsolete".into())));
        assert!(!ui.changes.contains_key("one"));
        let mut obsolete = source.clone();
        obsolete.pr = Some(first.key.clone());
        ui.receive_change_result(&obsolete, 2, Ok(Reply::Changes("wrong PR".into())));
        assert!(!ui.changes.contains_key("one"));
        ui.receive_change_result(&source, 2, Ok(Reply::Changes(patch.into())));
        assert!(ui.changes.contains_key("one"));
        ui.changing = Some((source.clone(), 3));
        ui.receive_change_result(&source, 3, Err("offline".into()));
        assert!(
            ui.changes.contains_key("one"),
            "failed refresh retains the published patch"
        );
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30))?;
        terminal.draw(|frame| {
            let area = frame.area();
            ui.draw_changes(frame, area);
        })?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("example/project#42") && screen.contains("zother/repository#42"));
        assert!(screen.contains("cached diff may be stale"));
        assert!(
            ui.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::ChangePr(1)))
        );
        ui.focus = Focus::Composer;
        ui.key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "[]"
        );
        ui.prs.forget("one", &storage, &ui.cache_writer)?;
        ui.sync_change_pr("one");
        assert!(ui.change_source("one").pr.is_none());
        assert!(!ui.changes.contains_key("one"));
        Ok(())
    }
    #[test]
    fn statuses_persist_refresh_and_survive_network_failures() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = store(temp.path());
        let mut state = State::load(&storage);
        assert!(state.due("one"));
        state.pending.insert("one".into(), Cancel::default());
        assert!(!state.due("one"));
        let mut badge = pr();
        for (status, draft, conflict, label) in [
            ("OPEN", false, false, "Open"),
            ("OPEN", true, false, "Draft"),
            ("OPEN", true, true, "Has conflicts"),
            ("MERGED", false, true, "Merged"),
            ("CLOSED", false, true, "Closed"),
        ] {
            badge.state = status.into();
            badge.draft = draft;
            badge.conflicts = conflict;
            assert_eq!(badge.label(), label);
        }
        badge.state = "MERGED".into();
        state.receive(Update {
            id: "one".into(),
            workspace: temp.path().into(),
            branch: None,
            revision: 0,
            result: Ok(vec![Cached {
                attached: false,
                workspace_revision: 0,
                workspace: temp.path().into(),
                pr: badge,
            }]),
        });
        assert!(!state.due("one"));
        state.refreshed.insert(
            "one".into(),
            Instant::now()
                .checked_sub(Duration::from_secs(121))
                .context("time")?,
        );
        assert!(state.due("one"));
        assert!(!state.receive(Update {
            id: "one".into(),
            workspace: temp.path().into(),
            branch: None,
            revision: 0,
            result: Err("offline".into())
        }));
        assert!(state.stale("one"));
        assert_eq!(state.get("one").context("cached")?.label(), "Merged");
        assert_eq!(
            color(state.get("one").context("cached")?),
            crate::ui::PURPLE
        );
        state.save(&storage)?;
        let restored = State::load(&storage);
        assert_eq!(restored.get("one"), state.get("one"));
        assert!(restored.due("one"));
        Ok(())
    }
    #[test]
    fn pr_badges_pane_shortcuts_and_modals_preserve_chat() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = store(temp.path());
        let mut ui = Ui::new(storage.clone(), &Config::default());
        let session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: temp.path().into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        ui.summaries.push(session.summary());
        ui.sessions.insert("one".into(), session);
        ui.selected = Some("one".into());
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.positions
            .entry("one".into())
            .or_default()
            .draft
            .insert("Preserve this draft");
        ui.prs.receive(Update {
            id: "one".into(),
            workspace: temp.path().into(),
            branch: None,
            revision: 0,
            result: Ok(vec![Cached {
                attached: false,
                workspace_revision: 0,
                workspace: temp.path().into(),
                pr: pr(),
            }]),
        });
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 45))?;
        terminal.draw(|f| ui.draw(f))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert_eq!(screen.matches("PR #42 · Open").count(), 2);
        let conversation = ui
            .hits
            .iter()
            .find_map(|(rect, action)| {
                matches!(action, Action::Focus(Focus::Conversation)).then_some(*rect)
            })
            .context("conversation border")?;
        let pill = ui
            .hits
            .iter()
            .find_map(|(rect, action)| matches!(action, Action::PullRequest(0)).then_some(*rect))
            .context("PR pill")?;
        assert_eq!(pill.y, conversation.bottom() - 2);
        assert!(pill.x > conversation.x && pill.right() < conversation.right());
        let status_row = (conversation.x..conversation.right())
            .filter_map(|x| terminal.backend().buffer().cell((x, pill.y)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            status_row.contains("Inherited · Inherited · Starting")
                && status_row.contains("PR #42 · Open"),
            "{status_row}"
        );
        assert!(!status_row.contains("/permissions") && !status_row.contains("Shift+Tab"));
        assert!(ui.prs.visible.contains("one"));
        assert!(!screen.contains("Shells (0)"));
        ui.sessions
            .get_mut("one")
            .context("session")?
            .artifacts
            .push(super::super::super::artifacts::Artifact {
                workspace: None,
                path: "preview.html".into(),
                title: "Preview".into(),
            });
        ui.panels
            .shells
            .insert("one".into(), vec![serde_json::json!({"itemId":"shell"})]);
        terminal.draw(|f| ui.draw(f))?;
        let controls = ui
            .hits
            .iter()
            .filter_map(|(rect, action)| match action {
                Action::Resources(true) => Some(("artifacts", *rect)),
                Action::PullRequest(0) => Some(("pr", *rect)),
                Action::Resources(false) => Some(("shells", *rect)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            controls.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["artifacts", "pr", "shells"]
        );
        assert!(controls.iter().all(|(_, rect)| rect.y == pill.y));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Resources(true));
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::PullRequest(0));
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Resources(false));
        let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(48, 20))?;
        narrow.draw(|f| ui.draw(f))?;
        let shell = ui
            .hits
            .iter()
            .find_map(|(rect, action)| matches!(action, Action::Resources(false)).then_some(*rect))
            .context("focused shell stays reachable")?;
        let label = (shell.x..shell.right())
            .filter_map(|x| narrow.backend().buffer().cell((x, shell.y)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(label.contains("Shells (1)"), "{label}");
        ui.sessions
            .get_mut("one")
            .context("session")?
            .artifacts
            .clear();
        ui.panels.shells.clear();
        ui.focus = Focus::Composer;
        ui.summaries.first_mut().context("summary")?.status = Status::Waiting;
        ui.sidebar.counts.insert(
            "one".into(),
            DiffStatistics {
                added: 12,
                removed: 3,
            },
        );
        terminal.draw(|f| ui.draw(f))?;
        ui.settle_flips();
        terminal.draw(|f| ui.draw(f))?;
        let sidebar = (0..45)
            .map(|y| {
                (0..38)
                    .filter_map(|x| terminal.backend().buffer().cell((x, y)))
                    .map(|c| c.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let diff_row = sidebar
            .iter()
            .position(|s| s.contains("+12 -3"))
            .context("counts")?;
        let waiting_row = sidebar
            .iter()
            .position(|s| s.contains("Needs input"))
            .context("needs input")?;
        let pr_row = sidebar
            .iter()
            .position(|s| s.contains("PR #42"))
            .context("PR badge")?;
        assert_eq!(waiting_row, diff_row + 1);
        assert_eq!(pr_row, waiting_row + 1);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::PullRequest(0));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        ui.prs.receive(Update {
            id: "one".into(),
            workspace: temp.path().into(),
            branch: None,
            revision: 0,
            result: Ok([41, 40]
                .into_iter()
                .map(|number| {
                    let mut pr = pr();
                    pr.key.number = number;
                    Cached {
                        attached: false,
                        workspace_revision: 0,
                        workspace: temp.path().into(),
                        pr,
                    }
                })
                .collect()),
        });
        terminal.draw(|f| ui.draw(f))?;
        let badges = |ui: &Ui| {
            ui.hits
                .iter()
                .filter_map(|(rect, action)| {
                    if let Action::PullRequest(index) = action {
                        Some((*index, *rect))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let original = badges(&ui);
        assert_eq!(original.len(), 3);
        ui.focus = Focus::PullRequest(0);
        for key in [
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Left,
            KeyCode::Left,
        ] {
            ui.key(KeyEvent::new(key, KeyModifiers::NONE));
            terminal.draw(|f| ui.draw(f))?;
            assert_eq!(badges(&ui), original);
            for (index, rect) in &original {
                let cell = terminal
                    .backend()
                    .buffer()
                    .cell((rect.x, rect.y))
                    .context("badge cell")?;
                let expected = if ui.focus == Focus::PullRequest(*index) {
                    color(&pr())
                } else {
                    BG
                };
                assert_eq!(cell.bg, expected);
            }
        }
        ui.focus = Focus::Composer;
        // A loading fixture prevents any network or model work in this UI test.
        let mut app = App::new(storage.clone(), Config::default());
        app.reviews.insert(
            pr().key.id(),
            Review {
                loading: true,
                ..Default::default()
            },
        );
        app.start_embedded(pr().key);
        assert!(!app.home);
        assert_eq!(app.view, PrView::Overview);
        ui.panels.view = Some(panels::View::PullRequest { app: Box::new(app) });
        ui.panels.focused = true;
        let mut reviews = App::new(storage.clone(), Config::default());
        reviews.images.picker = Some(ratatui_image::picker::Picker::halfblocks());
        ui.configure_images(&reviews.images, false);
        let Some(panels::View::PullRequest { app }) = &ui.panels.view else {
            anyhow::bail!("Missing embedded PR");
        };
        assert!(app.images.supported());
        terminal.draw(|f| ui.draw(f))?;
        let right = ui.panels.rect;
        assert!(right.x > 80);
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(screen.contains("Preserve this draft") && screen.contains("1 Preview"));
        for (key, expected) in [
            ('2', PrView::Diff),
            ('3', PrView::Guide),
            ('1', PrView::Overview),
            (']', PrView::Diff),
            (']', PrView::Guide),
            (']', PrView::Overview),
            ('[', PrView::Guide),
            ('[', PrView::Diff),
            ('[', PrView::Overview),
        ] {
            ui.key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            let Some(panels::View::PullRequest { app }) = &ui.panels.view else {
                anyhow::bail!("PR pane missing");
            };
            assert_eq!(app.view, expected);
        }
        ui.key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(ui.panels.focused);
        let Some(panels::View::PullRequest { app }) = &ui.panels.view else {
            anyhow::bail!("PR pane missing");
        };
        assert_eq!(app.focus, crate::app::Focus::Content);
        ui.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        terminal.draw(|f| ui.draw(f))?;
        assert!(!ui.panels.right && ui.panels.rect.width > right.width);
        assert!(!storage.load_config()?.agent_panel_right);
        // PR dialogs receive typing and Esc before the helper's close handler.
        ui.key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        ui.paste("review");
        terminal.draw(|f| ui.draw(f))?;
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.panels.view.is_some());
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!ui.panels.focused);
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(ui.panels.focused);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.panels.view.is_none());
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Preserve this draft"
        );
        Ok(())
    }
    #[test]
    fn embedded_pr_draw_and_dialog_stay_inside_their_rectangle() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut app = App::new(store(temp.path()), Config::default());
        app.reviews.insert(
            pr().key.id(),
            Review {
                loading: true,
                ..Default::default()
            },
        );
        app.start_embedded(pr().key);
        let area = Rect::new(30, 4, 75, 28);
        app.render_area = Some(area);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 40))?;
        for modal in [false, true] {
            if modal {
                app.key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
            }
            terminal.draw(|f| {
                for cell in &mut f.buffer_mut().content {
                    cell.set_symbol(".");
                }
                crate::ui::draw(f, &mut app);
            })?;
            for y in 0..40 {
                for x in 0..130 {
                    if !area.contains((x, y).into()) {
                        assert_eq!(
                            terminal
                                .backend()
                                .buffer()
                                .cell((x, y))
                                .context("cell")?
                                .symbol(),
                            ".",
                            "leak at {x},{y}"
                        );
                    }
                }
            }
            assert!(app.hits.iter().all(|(r, _)| r.intersection(area) == *r));
        }
        Ok(())
    }
}
