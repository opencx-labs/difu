//! Cached, asynchronous PR discovery for visible coding sessions.
use super::*;
use crate::{github::SessionPr, process::Cancel};
use std::path::PathBuf;

struct Update {
    id: String,
    workspace: PathBuf,
    result: Result<SessionPr, String>,
}
use crate::agents::pr_cache::{SessionLink as Cached, SESSION_LINKS};
pub(super) struct State {
    cache: HashMap<String, Cached>,
    loaded_at: Option<Instant>,
    pub visible: HashSet<String>,
    refreshed: HashMap<String, Instant>,
    pending: HashMap<String, Cancel>,
    sender: mpsc::Sender<Update>,
    receiver: mpsc::Receiver<Update>,
}
impl State {
    pub fn load(storage: &Storage) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut cache: HashMap<String, Cached> =
            std::fs::read(storage.cache.join("agent-prs.json"))
                .ok()
                .and_then(|v| serde_json::from_slice(&v).ok())
                .unwrap_or_default();
        if let Some(links) = std::fs::read(storage.cache.join(SESSION_LINKS))
            .ok()
            .and_then(|v| serde_json::from_slice::<HashMap<String, Cached>>(&v).ok())
        {
            cache.extend(links);
        }
        Self {
            cache,
            loaded_at: None,
            visible: HashSet::new(),
            refreshed: HashMap::new(),
            pending: HashMap::new(),
            sender,
            receiver,
        }
    }
    pub fn forget(&mut self, id: &str, storage: &Storage) -> anyhow::Result<()> {
        if let Some(cancel) = self.pending.remove(id) {
            cancel.cancel();
        }
        self.cache.remove(id);
        self.refreshed.remove(id);
        self.save(storage)
    }
    pub fn get(&self, id: &str) -> Option<&SessionPr> {
        self.cache.get(id).map(|c| &c.pr)
    }
    fn due(&self, id: &str) -> bool {
        !self.pending.contains_key(id)
            && self
                .refreshed
                .get(id)
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
    }
    fn receive(&mut self, update: Update) -> bool {
        self.pending.remove(&update.id);
        self.refreshed.insert(update.id.clone(), Instant::now());
        if let Ok(pr) = update.result {
            self.cache.insert(
                update.id,
                Cached {
                    workspace: update.workspace,
                    pr,
                },
            );
            true
        } else {
            // A branch without a PR is normal; transient failures retain cached status.
            false
        }
    }
    fn save(&self, storage: &Storage) -> anyhow::Result<()> {
        std::fs::create_dir_all(&storage.cache)?;
        crate::storage::atomic_json(&storage.cache.join("agent-prs.json"), &self.cache)
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
    pub(super) fn tick_prs(&mut self, visible: bool) {
        let invalid = self
            .prs
            .cache
            .iter()
            .filter(|(id, cached)| {
                self.summaries
                    .iter()
                    .any(|s| &s.id == *id && s.workspace != cached.workspace)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in invalid {
            if let Err(error) = self.prs.forget(&id, &self.storage) {
                self.notice = Some((format!("Cannot clear cached PR: {error:#}"), true));
            }
        }
        let mut changed = false;
        if self
            .prs
            .loaded_at
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30))
        {
            self.prs.loaded_at = Some(Instant::now());
            if let Some(links) = std::fs::read(self.storage.cache.join(SESSION_LINKS))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<HashMap<String, Cached>>(&bytes).ok())
            {
                for (id, link) in links {
                    if self
                        .summaries
                        .iter()
                        .any(|s| s.id == id && s.workspace == link.workspace)
                    {
                        self.prs.cache.insert(id, link);
                    }
                }
            }
        }
        while let Ok(update) = self.prs.receiver.try_recv() {
            if self
                .summaries
                .iter()
                .any(|s| s.id == update.id && s.workspace == update.workspace)
            {
                changed |= self.prs.receive(update);
            }
        }
        if changed && let Err(error) = self.prs.save(&self.storage) {
            self.notice = Some((format!("Cannot cache session PRs: {error:#}"), true));
        }
        if !visible {
            return;
        }
        for summary in &self.summaries {
            if summary.kind != "Coding" && summary.kind != "coding" {
                continue;
            }
            if !self.prs.visible.contains(&summary.id)
                && self.selected.as_ref() != Some(&summary.id)
            {
                continue;
            }
            if self.prs.pending.len() >= 2 || !self.prs.due(&summary.id) {
                continue;
            }
            let id = summary.id.clone();
            let workspace = summary.workspace.clone();
            let branch = summary.branch.clone();
            let known = self.prs.get(&id).map(|p| p.key.clone());
            let cancel = Cancel::default();
            self.prs.pending.insert(id.clone(), cancel.clone());
            let sender = self.prs.sender.clone();
            thread::spawn(move || {
                let result = crate::github::session_pr(
                    &workspace,
                    branch.as_deref(),
                    known.as_ref(),
                    &cancel,
                )
                .map_err(|e| format!("{e:#}"));
                let _ = sender.send(Update {
                    id,
                    workspace,
                    result,
                });
            });
        }
    }
    pub(super) fn open_pr_panel(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(pr) = self.prs.get(&id).cloned() else {
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
        // Reuse the session checkout for reads without changing saved repository settings.
        if let Some(root) = self
            .prs
            .cache
            .get(&id)
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
            key: PrKey {
                owner: "example".into(),
                repo: "project".into(),
                number: 42,
            },
            state: "OPEN".into(),
            draft: false,
            conflicts: false,
        }
    }
    fn store(root: &std::path::Path) -> Storage {
        Storage {
            config: root.join("config.json"),
            cache: root.join("cache"),
        }
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
            result: Ok(badge),
        });
        assert!(!state.due("one"));
        state.refreshed.insert(
            "one".into(),
            Instant::now()
                .checked_sub(Duration::from_secs(31))
                .context("time")?,
        );
        assert!(state.due("one"));
        assert!(!state.receive(Update {
            id: "one".into(),
            workspace: temp.path().into(),
            result: Err("offline".into())
        }));
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
            result: Ok(pr()),
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
        assert!(ui.prs.visible.contains("one"));
        assert!(!screen.contains("Shells (0)"));
        ui.summaries.first_mut().context("summary")?.status = Status::Waiting;
        ui.sidebar.counts.insert(
            "one".into(),
            DiffStatistics {
                added: 12,
                removed: 3,
            },
        );
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
        assert_eq!(ui.focus, Focus::PullRequest);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
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
            ('2', PrView::Guide),
            ('3', PrView::Diff),
            ('1', PrView::Overview),
            (']', PrView::Guide),
            (']', PrView::Diff),
            (']', PrView::Overview),
            ('[', PrView::Diff),
            ('[', PrView::Guide),
            ('[', PrView::Overview),
        ] {
            ui.key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            let Some(panels::View::PullRequest { app }) = &ui.panels.view else {
                anyhow::bail!("PR pane missing");
            };
            assert_eq!(app.view, expected);
        }
        ui.key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE));
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
