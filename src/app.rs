use crate::{
    codex::{self, Guide},
    context::{Direction, Expansion, FileContext, FileState},
    diff::Snapshot,
    github,
    model::*,
    process::Cancel,
    repo,
    storage::{Config, Storage},
};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Overview,
    Guide,
    Diff,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Navigation,
    Content,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoticeKind {
    #[default]
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug, Default)]
pub struct Notice {
    pub message: String,
    pub kind: NoticeKind,
}

impl Notice {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: NoticeKind::Info,
        }
    }
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: NoticeKind::Success,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: NoticeKind::Error,
        }
    }
}

impl std::fmt::Display for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub struct Generation {
    pub id: u64,
    pub cancel: Cancel,
    pub started: Instant,
    pub activity: String,
}
#[derive(Default)]
pub struct Review {
    pub interaction: crate::workflow::PrState,
    pub detail: Option<Arc<PrDetail>>,
    pub timeline: Vec<TimelineItem>,
    pub checks: Vec<Check>,
    pub check_report: Option<CheckReport>,
    pub failures: HashMap<String, crate::ci::Failures>,
    pub failures_loading: std::collections::HashSet<String>,
    pub detail_error: Option<String>,
    pub timeline_error: Option<String>,
    pub checks_error: Option<String>,
    pub root: Option<PathBuf>,
    pub snapshot: Option<Arc<Snapshot>>,
    pub guide: Option<Arc<Guide>>,
    pub context: HashMap<String, FileState>,
    pub bounds: HashMap<String, crate::bounds::State>,
    pub expanded: HashMap<String, Expansion>,
    pub guide_model: Option<ModelChoice>,
    pub generation: Option<Generation>,
    pub guide_error: Option<String>,
    pub newer: Option<PrDetail>,
    pub loading: bool,
    pub preparing: bool,
    pub preparation_failed: bool,
    pub preparation_started: Option<Instant>,
    pub preparation_progress: Option<repo::SnapshotProgress>,
    pub preparation: Option<Cancel>,
    pub preparing_detail: Option<Arc<PrDetail>>,
    pub polling: bool,
    pub revision_polling: bool,
    pub revision_poll_at: Option<Instant>,
    pub poll_at: Option<Instant>,
    pub snapshot_id: u64,
}

impl Review {
    /// Lifecycle metadata remains live even when the reviewed code is pinned.
    fn update_state(&mut self, state: &str) {
        let state = match state {
            "OPEN" | "open" => "open",
            "CLOSED" | "closed" => "closed",
            "MERGED" | "merged" => "merged",
            _ => return,
        };
        // A delayed response cannot reopen a merged PR; GitHub never does that.
        let state = if self.detail.as_ref().is_some_and(|pr| pr.state == "merged") {
            "merged"
        } else {
            state
        };
        for detail in [&mut self.detail, &mut self.preparing_detail]
            .into_iter()
            .flatten()
        {
            Arc::make_mut(detail).state = state.into();
        }
        if let Some(newer) = &mut self.newer {
            newer.state = state.into();
        }
    }
}

pub enum Message {
    Bounds(
        String,
        Arc<Snapshot>,
        String,
        Result<crate::bounds::Bounds, String>,
    ),
    Clipboard(u64, Result<String, String>),
    Image(
        crate::images::RenderKey,
        Result<ratatui_image::sliced::SlicedProtocol, String>,
    ),
    Failures(String, String, crate::ci::Failures),
    Definition(u64, Result<Arc<crate::navigation::Definition>, String>),
    Workflow(crate::workflow::Event),
    Inbox(u64, Result<Vec<PrSummary>, String>),
    InboxFinished(u64),
    InboxStats(u64, Vec<(String, Option<PrStats>)>),
    Repositories(Result<Vec<String>, String>),
    Detail(String, Result<PrDetail, String>),
    Timeline(String, Result<Vec<TimelineItem>, String>),
    Checks(String, Result<CheckReport, String>),
    Poll(String, u64, Result<Option<PrDetail>, String>),
    Snapshot(String, u64, Result<(PathBuf, Snapshot), String>),
    SnapshotProgress(String, u64, repo::SnapshotProgress),
    Progress(String, u64, String),
    Guide(String, u64, ModelChoice, Result<Guide, String>),
    Context(String, Arc<Snapshot>, String, Result<FileContext, String>),
    Models(Result<Vec<ModelInfo>, String>),
    Notice(String),
}

#[derive(Clone, Debug)]
pub enum Action {
    Copy,
    Filter,
    SelectRepository(String),
    OpenRepository,
    PinRepository(String),
    Image(crate::images::Request),
    Definition {
        path: String,
        line: u64,
        column: usize,
        old: bool,
    },
    CloseDefinition,
    Workflow(crate::workflow::WAction),
    SelectPr(usize),
    OpenPr,
    SetView(View),
    SetInbox(InboxTab),
    SetState(PrState),
    Back,
    SelectFile(usize),
    SelectDirectory(String),
    Jump(usize),
    Chapter(bool),
    GoToChapter(usize),
    ExpandHunk(String, Direction),
    Link(String),
    Models,
    Locate,
    ChooseModel(usize),
    ApplyModel(ModelChoice),
    Refresh,
    Regenerate,
    Cancel,
    Help,
    ToggleLayout,
    ToggleWrap,
    Scroll(i32),
    FastScroll(i32),
    Focus(Focus),
}

pub enum Modal {
    Image(crate::images::Request),
    Definition(crate::navigation::Viewer),
    Workflow(Box<crate::workflow::Wizard>),
    Clone {
        value: crate::editor::Editor,
        key: String,
    },
    Models {
        selected: usize,
        effort: usize,
        query: crate::editor::Editor,
    },
    Help(crate::help::State),
}

pub struct App {
    pub hover: crate::hover::State,
    pub preserve_diff_position: bool,
    pub(crate) clipboard_id: u64,
    pub(crate) clipboard: Option<String>,
    pub filters: crate::filter::State,
    pub repository: Option<String>,
    pub repo_selected: Option<String>,
    pub inbox_refreshed: Option<Instant>,
    pub images: crate::images::State,
    pub workflow: crate::workflow::State,
    pub storage: Storage,
    pub config: Config,
    pub inbox: Vec<PrSummary>,
    pub selected: usize,
    pub inbox_loading: bool,
    pub inbox_error: Option<String>,
    pub home: bool,
    pub inbox_tab: InboxTab,
    pub my_prs_state: PrState,
    pub repository_state: PrState,
    pub repository_options: Vec<String>,
    pub repositories_loading: bool,
    pub repositories_error: Option<String>,
    pub reviews: HashMap<String, Review>,
    pub view: View,
    pub focus: Focus,
    pub scroll: usize,
    pub nav_scroll: usize,
    pub horizontal: usize,
    pub file: usize,
    pub directory: Option<String>,
    pub tree_horizontal: usize,
    pub tree_max_horizontal: usize,
    pub modal: Option<Modal>,
    pub models: Vec<ModelInfo>,
    pub model_purpose: ModelPurpose,
    pub models_loading: bool,
    pub models_error: Option<String>,
    pub notice: Notice,
    pub quit: bool,
    pub epoch: u64,
    pub document: Option<crate::ui::Document>,
    pub chapter_target: Option<usize>,
    pub hits: Vec<(ratatui::layout::Rect, Action)>,
    pub viewport: usize,
    pub content_rect: ratatui::layout::Rect,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
    jobs: Vec<(Cancel, JoinHandle<()>)>,
    sequence: u64,
    pending_open: Option<String>,
    opened: Option<String>,
    inbox_id: u64,
    inbox_cancel: Option<Cancel>,
}

fn result<T>(value: Result<T>) -> Result<T, String> {
    value.map_err(|e| format!("{e:#}"))
}

fn progress_priority(activity: &str) -> u8 {
    if activity == "Validating chapter coverage" {
        3
    } else if activity.starts_with("Codex:") {
        2
    } else if activity.starts_with("Codex summary:") {
        1
    } else {
        0
    }
}

impl App {
    pub fn new(storage: Storage, config: Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            storage,
            config,
            inbox: Vec::new(),
            selected: 0,
            inbox_loading: false,
            inbox_error: None,
            workflow: Default::default(),
            images: Default::default(),
            filters: Default::default(),
            repository: None,
            repo_selected: None,
            hover: Default::default(),
            preserve_diff_position: false,
            clipboard_id: 0,
            clipboard: None,
            inbox_refreshed: None,
            home: true,
            inbox_tab: InboxTab::MyPrs,
            my_prs_state: PrState::Open,
            repository_state: PrState::Open,
            repository_options: Vec::new(),
            repositories_loading: false,
            repositories_error: None,
            reviews: HashMap::new(),
            view: View::Overview,
            focus: Focus::Navigation,
            scroll: 0,
            nav_scroll: 0,
            horizontal: 0,
            file: 0,
            directory: None,
            tree_horizontal: 0,
            tree_max_horizontal: 0,
            modal: None,
            models: Vec::new(),
            model_purpose: ModelPurpose::Guide,
            models_loading: false,
            models_error: None,
            notice: Notice::default(),
            quit: false,
            epoch: 0,
            document: None,
            chapter_target: None,
            hits: Vec::new(),
            viewport: 20,
            content_rect: Default::default(),
            sender,
            receiver,
            jobs: Vec::new(),
            sequence: 0,
            pending_open: None,
            opened: None,
            inbox_id: 0,
            inbox_cancel: None,
        }
    }
    pub fn start(&mut self, pr: Option<PrKey>) {
        if let Some(key) = pr {
            let id = key.id();
            self.inbox.push(PrSummary {
                key: key.clone(),
                title: format!("Loading {id}…"),
                author: String::new(),
                updated: String::new(),
                created: String::new(),
                stats: None,
                stats_error: false,
                draft: false,
            });
            self.pending_open = Some(id);
            self.select(0);
        } else {
            self.load_inbox();
        }
    }
    pub(crate) fn spawn(
        &mut self,
        work: impl FnOnce(Sender<Message>, Cancel) + Send + 'static,
    ) -> Cancel {
        let sender = self.sender.clone();
        let cancel = Cancel::default();
        let token = cancel.clone();
        match thread::Builder::new()
            .name("difu-worker".into())
            .spawn(move || work(sender, token))
        {
            Ok(handle) => self.jobs.push((cancel.clone(), handle)),
            Err(error) => {
                cancel.cancel();
                self.notice = Notice::error(format!("Could not start background work: {error}"));
            }
        }
        cancel
    }
    pub fn key(&self) -> Option<String> {
        if !self.home {
            return self.opened.clone();
        }
        if self.repository_directory() || !self.visible_prs().contains(&self.selected) {
            return None;
        }
        self.inbox.get(self.selected).map(|p| p.key.id())
    }
    pub fn review(&self) -> Option<&Review> {
        self.key().and_then(|key| self.reviews.get(&key))
    }
    pub fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }
    fn next_id(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
    fn save_config(&mut self) {
        if let Err(e) = self.storage.save_config(&self.config) {
            self.notice = Notice::error(format!("Could not save settings: {e:#}"));
        }
    }
    pub fn load_inbox(&mut self) {
        if self.repository_directory() {
            return;
        }
        self.inbox_refreshed = Some(Instant::now());
        if let Some(cancel) = self.inbox_cancel.take() {
            cancel.cancel();
        }
        let id = self.next_id();
        self.inbox_id = id;
        self.inbox_loading = true;
        self.inbox_error = None;
        let tab = self.inbox_tab;
        let state = self.state();
        let repositories: Vec<_> = self.repository.iter().cloned().collect();
        let cache_key = crate::storage::hash(format!("v2:{tab:?}:{state:?}:{repositories:?}"));
        match self.storage.load_inbox(&cache_key) {
            Ok(Some(inbox)) => {
                let previous = self.inbox.get(self.selected).map(|p| p.key.id());
                self.inbox = inbox;
                self.selected = previous
                    .and_then(|id| self.inbox.iter().position(|p| p.key.id() == id))
                    .unwrap_or(0);
                self.ensure_pr_selection();
            }
            Ok(None) => {}
            Err(error) => {
                self.notice = Notice::error(format!("Could not load cached PRs: {error:#}"))
            }
        }
        let cached: HashMap<_, _> = self
            .inbox
            .iter()
            .map(|pr| (pr.key.id(), pr.stats.clone()))
            .collect();
        let storage = self.storage.clone();
        self.inbox_cancel = Some(self.spawn(move |tx, cancel| {
            let mut inbox = match github::inbox(tab, state, &repositories, &cancel) {
                Ok(inbox) => inbox,
                Err(error) => {
                    let _ = tx.send(Message::Inbox(id, Err(format!("{error:#}"))));
                    return;
                }
            };
            if cancel.cancelled() {
                return;
            }
            for pr in &mut inbox {
                pr.stats = cached.get(&pr.key.id()).cloned().flatten();
            }
            let _ = tx.send(Message::Inbox(id, Ok(inbox.clone())));
            if let Err(error) = storage.save_inbox(&cache_key, &inbox) {
                let _ = tx.send(Message::Notice(format!(
                    "Could not cache PR list: {error:#}"
                )));
            }
            for batch in inbox.chunks_mut(25) {
                if cancel.cancelled() {
                    return;
                }
                let keys: Vec<_> = batch.iter().map(|pr| pr.key.clone()).collect();
                let stats = github::stats(&keys, &cancel).unwrap_or_default();
                if cancel.cancelled() {
                    return;
                }
                let mut updates = Vec::new();
                for (index, pr) in batch.iter_mut().enumerate() {
                    let value = stats.get(index).cloned().flatten();
                    pr.stats_error = value.is_none();
                    if value.is_some() {
                        pr.stats = value.clone();
                    }
                    updates.push((pr.key.id(), value));
                }
                let _ = tx.send(Message::InboxStats(id, updates));
            }
            if !cancel.cancelled()
                && let Err(error) = storage.save_inbox(&cache_key, &inbox)
            {
                let _ = tx.send(Message::Notice(format!(
                    "Could not cache PR counts: {error:#}"
                )));
            }
            let _ = tx.send(Message::InboxFinished(id));
        }));
    }

    pub fn state(&self) -> PrState {
        match self.inbox_tab {
            InboxTab::MyPrs => self.my_prs_state,
            InboxTab::Repositories => self.repository_state,
        }
    }
    fn change_inbox(&mut self, tab: InboxTab) {
        self.home = true;
        self.opened = None;
        self.pending_open = None;
        self.inbox_tab = tab;
        self.repository = None;
        self.filters.focused = None;
        self.view = View::Overview;
        self.inbox.clear();
        self.selected = 0;
        self.nav_scroll = 0;
        self.scroll = 0;
        self.horizontal = 0;
        self.focus = Focus::Navigation;
        self.notice = Notice::default();
        if tab == InboxTab::Repositories {
            self.load_repository_list();
        } else {
            self.load_inbox();
        }
        self.invalidate();
    }

    pub fn select(&mut self, index: usize) {
        if index >= self.inbox.len() || !self.visible_prs().contains(&index) {
            return;
        }
        if index != self.selected {
            self.scroll = 0;
            self.file = 0;
            self.directory = None;
            self.tree_horizontal = 0;
            self.horizontal = 0;
        }
        if index != self.selected {
            self.workflow.cursor = None;
            self.workflow.selection = None;
            self.workflow.nav = 0;
        }
        self.selected = index;
        self.invalidate();
        let Some(key) = self.inbox.get(index).map(|pr| pr.key.clone()) else {
            return;
        };
        let id = key.id();
        let entry = self.reviews.entry(id.clone()).or_default();
        if entry.detail.is_some() || entry.loading {
            return;
        }
        entry.loading = true;
        self.spawn(move |tx, cancel| {
            let _ = tx.send(Message::Detail(
                id.clone(),
                result(github::detail(&key, &cancel)),
            ));
            if cancel.cancelled() {
                return;
            }
            let _ = tx.send(Message::Timeline(
                id.clone(),
                result(github::timeline(&key, &cancel)),
            ));
            if cancel.cancelled() {
                return;
            }
            let _ = tx.send(Message::Checks(id, result(github::checks(&key, &cancel))));
        });
    }
    pub fn open(&mut self) {
        if self.repository_directory() {
            self.open_repository();
            return;
        }
        let Some(id) = self.key() else {
            return;
        };
        let Some(review) = self.reviews.get(&id) else {
            return;
        };
        let Some(pr) = review.detail.clone() else {
            self.pending_open = Some(id);
            return;
        };
        let has_snapshot = review.snapshot.is_some();
        let preparing = review.preparing;
        let root = review
            .root
            .clone()
            .or_else(|| self.config.repositories.get(&pr.key.repository()).cloned());
        self.workflow.cursor = None;
        self.workflow.selection = None;
        self.workflow.nav = 0;
        if self.home {
            self.workflow.side = crate::review::Side::Right;
        }
        self.opened = Some(id.clone());
        self.home = false;
        self.view = View::Guide;
        self.focus = Focus::Content;
        self.scroll = 0;
        self.horizontal = 0;
        self.invalidate();
        if has_snapshot {
            self.generate(false);
            return;
        }
        if preparing {
            return;
        }
        if let Some(root) = root {
            self.prepare(root);
        } else {
            self.modal = Some(Modal::Clone {
                value: std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
                    .into(),
                key: id,
            });
        }
    }
    fn prepare(&mut self, path: PathBuf) {
        let Some(id) = self.key() else {
            return;
        };
        let Some(pr) = self.reviews.get(&id).and_then(|r| r.detail.clone()) else {
            return;
        };
        self.prepare_revision(path, pr);
    }
    fn prepare_revision(&mut self, path: PathBuf, pr: Arc<PrDetail>) {
        let Some(id) = self.key() else {
            return;
        };
        let sequence = self.next_id();
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        review.preparing = true;
        review.preparation_started = Some(Instant::now());
        review.preparation_progress = Some(repo::SnapshotProgress {
            step: 1,
            activity: "Checking the local clone".into(),
        });
        review.preparation_failed = false;
        review.preparing_detail = Some(pr.clone());
        review.guide_error = None;
        review.snapshot_id = sequence;
        let job_id = id.clone();
        let cancel = self.spawn(move |tx, cancel| {
            let output = (|| {
                let root = repo::validate(&path, &pr.key, &cancel)?;
                let progress_tx = tx.clone();
                let progress_id = job_id.clone();
                let snapshot = repo::snapshot_with_progress(
                    &root,
                    &pr,
                    &cancel,
                    Arc::new(move |progress| {
                        let _ = progress_tx.send(Message::SnapshotProgress(
                            progress_id.clone(),
                            sequence,
                            progress,
                        ));
                    }),
                )?;
                Ok((root, snapshot))
            })();
            let _ = tx.send(Message::Snapshot(job_id, sequence, result(output)));
        });
        if let Some(review) = self.reviews.get_mut(&id) {
            review.preparation = Some(cancel);
        }
    }
    fn adjust_focused_hunk(&mut self, amount: i32) {
        if self.home || self.view == View::Overview {
            return;
        }
        let hunk = self
            .workflow
            .cursor
            .and_then(|cursor| self.document.as_ref()?.rows.get(cursor))
            .and_then(|row| row.right.hunk.clone());
        if let Some(hunk) = hunk {
            self.expand_hunk_by(hunk.clone(), Direction::Above, amount);
            self.expand_hunk_by(hunk, Direction::Below, amount);
        }
    }
    fn expand_hunk(&mut self, hunk_id: String, direction: Direction) {
        self.expand_hunk_by(hunk_id, direction, 10);
    }
    fn expand_hunk_by(&mut self, hunk_id: String, direction: Direction, amount: i32) {
        let Some(id) = self.key() else { return };
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        let (Some(root), Some(snapshot)) = (review.root.clone(), review.snapshot.clone()) else {
            return;
        };
        let Some((file, hunk)) = snapshot.find(&hunk_id) else {
            return;
        };
        if !hunk.header.starts_with("@@ ") {
            return;
        }
        let path = file.path.clone();
        let state = review.context.entry(path.clone()).or_default();
        if let Some(data) = &state.data {
            data.adjust(
                &hunk_id,
                review.expanded.entry(hunk_id.clone()).or_default(),
                direction,
                amount,
            );
            self.preserve_diff_position = true;
            self.invalidate();
            return;
        }
        if amount == 10
            && state
                .pending
                .contains(&(hunk_id.clone(), direction, amount))
        {
            return;
        }
        let loading = !state.pending.is_empty();
        if amount < 0 && state.pending.is_empty() {
            return;
        }
        state.pending.push((hunk_id.clone(), direction, amount));
        state.error = None;
        if !loading {
            self.spawn(move |tx, cancel| {
                let output = (|| {
                    let (file, _) = snapshot
                        .find(&hunk_id)
                        .ok_or_else(|| anyhow::anyhow!("Hunk no longer exists"))?;
                    FileContext::new(file, repo::file_context(&root, &snapshot, file, &cancel)?)
                })();
                let _ = tx.send(Message::Context(id, snapshot, path, result(output)));
            });
        }
        self.invalidate();
    }

    pub fn generate(&mut self, force: bool) {
        let Some(id) = self.key() else {
            return;
        };
        self.generate_for(&id, force);
    }
    fn generate_for(&mut self, id: &str, force: bool) {
        let Some(review) = self.reviews.get(id) else {
            return;
        };
        if review.generation.is_some() {
            return;
        }
        let (Some(pr), Some(snapshot), Some(root)) = (
            review.detail.clone(),
            review.snapshot.clone(),
            review.root.clone(),
        ) else {
            return;
        };
        let model = self.config.model.clone();
        if !force && review.guide_model.as_ref() == Some(&model) && review.guide.is_some() {
            return;
        }
        if !force {
            let cached = (|| -> Result<Option<Guide>> {
                let key = codex::cache_key(&pr, &snapshot, &model)?;
                let guide = match self.storage.load_guide(&key)? {
                    Some(guide) => Some(guide),
                    None => {
                        let legacy = self
                            .storage
                            .load_guide(&codex::legacy_cache_key(&pr, &snapshot, &model)?)?;
                        if let Some(guide) = &legacy {
                            guide.validate(&snapshot)?;
                            self.storage.save_guide(&key, guide)?;
                        }
                        legacy
                    }
                };
                if let Some(guide) = &guide {
                    guide.validate(&snapshot)?;
                }
                Ok(guide)
            })();
            match cached {
                Ok(Some(guide)) => {
                    let Some(r) = self.reviews.get_mut(id) else {
                        return;
                    };
                    r.guide = Some(Arc::new(guide));
                    r.guide_model = Some(model);
                    r.guide_error = None;
                    self.sync_progress(id);
                    self.notice = Notice::success("Loaded cached guide");
                    self.invalidate();
                    return;
                }
                Err(e) => {
                    if let Some(r) = self.reviews.get_mut(id) {
                        r.guide_error = Some(format!("{e:#}"));
                    }
                    self.invalidate();
                    return;
                }
                _ => {}
            }
        }
        let generation = self.next_id();
        let storage = self.storage.clone();
        let job_id = id.to_owned();
        let choice = model.clone();
        let cancel = Cancel::default();
        let remote_cancel = cancel.clone();
        self.spawn(move |tx, observer| {
            let progress_tx = tx.clone();
            let progress_id = job_id.clone();
            let output = crate::agents::client::review_job(
                &storage,
                crate::agents::Job::Guide {
                    root,
                    pr: Box::new((*pr).clone()),
                    snapshot: Box::new((*snapshot).clone()),
                    model: choice.clone(),
                },
                &observer,
                &remote_cancel,
                move |message| {
                    let _ = progress_tx.send(Message::Progress(
                        progress_id.clone(),
                        generation,
                        message,
                    ));
                },
            )
            .and_then(|session| {
                session
                    .guide
                    .ok_or_else(|| anyhow::anyhow!("Completed guide job has no guide"))
            });
            let _ = tx.send(Message::Guide(job_id, generation, choice, result(output)));
        });
        let Some(review) = self.reviews.get_mut(id) else {
            cancel.cancel();
            return;
        };
        review.guide = None;
        review.guide_error = None;
        review.generation = Some(Generation {
            id: generation,
            cancel,
            started: Instant::now(),
            activity: "Starting guide generation".into(),
        });
        self.invalidate();
    }
    pub fn cancel(&mut self) {
        if let Some(review) = self.key().and_then(|id| self.reviews.get_mut(&id)) {
            if let Some(job) = &mut review.generation {
                job.cancel.cancel();
                job.activity = "Cancelling and cleaning up…".into();
            }
            if let Some(job) = &review.preparation {
                job.cancel();
            }
        }
    }
    pub fn refresh(&mut self) {
        if self.repository_directory() {
            self.load_repository_list();
            return;
        }
        if !self.home && self.view == View::Diff {
            if let Some(id) = self.key()
                && let Some(r) = self.reviews.get_mut(&id)
            {
                r.interaction.github_loaded = false;
            }
            self.load_viewed();
        }
        let Some(id) = self.key() else {
            self.load_inbox();
            return;
        };
        if self.view == View::Overview {
            if let Some(review) = self.reviews.get_mut(&id) {
                review
                    .failures
                    .retain(|_, failures| !failures.tests.is_empty());
                review.poll_at = None;
            }
            if let Some(key) = self
                .review()
                .and_then(|r| r.detail.as_ref())
                .map(|p| p.key.clone())
            {
                self.spawn(move |tx, cancel| {
                    let _ = tx.send(Message::Detail(
                        id.clone(),
                        result(github::detail(&key, &cancel)),
                    ));
                    let _ = tx.send(Message::Timeline(
                        id,
                        result(github::timeline(&key, &cancel)),
                    ));
                });
            }
            self.load_inbox();
            return;
        }
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        review.bounds.retain(|_, state| state.error.is_none());
        if review.generation.is_some() {
            self.notice =
                Notice::info("Cancel the current generation before refreshing its snapshot");
            return;
        }
        if review.preparing {
            return;
        }
        if let Some(newer) = review.newer.clone() {
            let root = review.root.clone().or_else(|| {
                self.config
                    .repositories
                    .get(&newer.key.repository())
                    .cloned()
            });
            if let Some(root) = root {
                self.prepare_revision(root, Arc::new(newer));
            } else {
                self.notice =
                    Notice::info("Locate this repository's local clone before refreshing");
            }
        } else {
            self.notice = Notice::info(
                "This snapshot is current. Remote revisions are checked every 30 seconds.",
            );
        }
    }
    pub fn load_models(&mut self) {
        self.load_models_for(ModelPurpose::Guide);
    }
    pub fn load_models_for(&mut self, purpose: ModelPurpose) {
        self.model_purpose = purpose;
        self.modal = Some(Modal::Models {
            selected: 0,
            effort: 0,
            query: Default::default(),
        });
        if self.models_loading || !self.models.is_empty() {
            return;
        }
        self.models_loading = true;
        self.models_error = None;
        self.spawn(|tx, cancel| {
            let _ = tx.send(Message::Models(result(codex::models(&cancel))));
        });
    }
    pub fn model_options(&self, query: &str) -> Vec<ModelChoice> {
        let recommended = self.model_purpose.recommended();
        let mut choices = vec![recommended.clone()];
        for model in &self.models {
            for effort in &model.efforts {
                let choice = ModelChoice {
                    model: model.id.clone(),
                    effort: effort.clone(),
                };
                if !choices.contains(&choice) {
                    choices.push(choice);
                }
            }
        }
        // The pinned recommendation is shown only when actually supported.
        if !self
            .models
            .iter()
            .any(|m| m.id == recommended.model && m.efforts.contains(&recommended.effort))
        {
            choices.retain(|c| *c != recommended);
        }
        let query = query.to_lowercase();
        choices
            .into_iter()
            .filter(|c| {
                format!("{} {}", c.model, c.effort)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }
    fn apply_model(&mut self, choice: ModelChoice) {
        let mut config = self.config.clone();
        match self.model_purpose {
            ModelPurpose::Guide => config.model = choice,
            ModelPurpose::Conflicts => config.conflict_model = choice,
        }
        if let Err(error) = self.storage.save_config(&config) {
            self.notice = Notice::error(format!("Could not save model: {error:#}"));
            return;
        }
        self.config = config;
        self.modal = None;
        self.notice = Notice::success(format!("{} saved", self.model_purpose.label()));
        self.invalidate();
    }
    pub fn tick(&mut self) {
        self.tick_visible(true);
    }
    pub fn tick_visible(&mut self, visible: bool) {
        while let Ok(message) = self.receiver.try_recv() {
            self.receive(message);
        }
        let mut index = 0;
        while index < self.jobs.len() {
            if self
                .jobs
                .get(index)
                .is_some_and(|(_, job)| job.is_finished())
            {
                let (_, handle) = self.jobs.swap_remove(index);
                if handle.join().is_err() {
                    self.notice = Notice::error("A background worker stopped unexpectedly");
                }
            } else {
                index += 1;
            }
        }
        // Drain already-requested results in the background, but only poll GitHub
        // for the Reviews screen while it is visible.
        if !visible {
            return;
        }
        if self.home
            && !self.repository_directory()
            && !self.inbox_loading
            && self
                .inbox_refreshed
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(30))
        {
            self.load_inbox();
        }
        self.load_visible_bounds();
        self.poll_revisions();
        let Some(id) = self.key() else {
            return;
        };
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        if review.loading
            || review.polling
            || review.detail.is_none()
            || review
                .poll_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
        {
            return;
        }
        review.polling = true;
        review.poll_at = Some(Instant::now());
        let Some(key) = review.detail.as_ref().map(|pr| pr.key.clone()) else {
            return;
        };
        self.spawn(move |tx, cancel| {
            let _ = tx.send(Message::Checks(id, result(github::checks(&key, &cancel))));
        });
    }
    fn poll_revisions(&mut self) {
        if self.home {
            return;
        }
        let Some(id) = self.key() else {
            return;
        };
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        if review.loading
            || review.revision_polling
            || review
                .revision_poll_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        let Some(pr) = review.newer.as_ref().or(review.detail.as_deref()).cloned() else {
            return;
        };
        review.revision_polling = true;
        review.revision_poll_at = Some(Instant::now());
        let snapshot_id = review.snapshot_id;
        self.spawn(move |tx, cancel| {
            let _ = tx.send(Message::Poll(
                id,
                snapshot_id,
                result(github::updated_revision(&pr, &cancel)),
            ));
        });
    }
    fn receive(&mut self, message: Message) {
        match message {
            Message::Bounds(id, snapshot, path, output) => {
                let visible =
                    self.key().as_ref() == Some(&id) && !self.home && self.view != View::Overview;
                if let Some(review) = self.reviews.get_mut(&id)
                    && review
                        .snapshot
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &snapshot))
                {
                    self.preserve_diff_position |= visible;
                    let state = review.bounds.entry(path).or_default();
                    state.loading = false;
                    match output {
                        Ok(data) => {
                            state.data = Some(data);
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error),
                    }
                }
            }
            Message::Clipboard(id, output) => {
                if id == self.clipboard_id {
                    match output {
                        Ok(text) => self.clipboard = Some(text),
                        Err(error) => {
                            self.notice = Notice::error(format!("Could not copy: {error}"))
                        }
                    }
                }
            }
            Message::Image(key, output) => self.images.receive(key, output),
            Message::Definition(id, output) => {
                if let Some(Modal::Definition(viewer)) = &mut self.modal
                    && viewer.id == id
                {
                    viewer.output = Some(output);
                }
            }
            Message::Workflow(event) => {
                self.workflow_receive(event);
                return;
            }
            Message::InboxFinished(id) => {
                if id == self.inbox_id {
                    self.inbox_loading = false;
                    self.inbox_refreshed = Some(Instant::now());
                }
            }
            Message::Inbox(id, output) => {
                if id != self.inbox_id {
                    return;
                }
                match output {
                    Ok(inbox) => {
                        let previous = self.inbox.get(self.selected).map(|p| p.key.id());
                        self.inbox = inbox;
                        self.selected = previous
                            .and_then(|id| self.inbox.iter().position(|p| p.key.id() == id))
                            .unwrap_or(0);
                        self.inbox_error = None;
                        if self.inbox.len() == 1000 {
                            self.notice =
                                Notice::info("GitHub search returned its maximum 1,000 results");
                        }
                        self.ensure_pr_selection();
                    }
                    Err(error) => {
                        self.inbox_loading = false;
                        self.inbox_refreshed = Some(Instant::now());
                        self.notice = Notice::error(format!(
                            "Could not refresh {}: {error}",
                            self.inbox_tab.label()
                        ));
                        self.inbox_error = Some(error);
                    }
                }
            }
            Message::InboxStats(id, updates) => {
                if id != self.inbox_id {
                    return;
                }
                for (key, stats) in updates {
                    if let Some(pr) = self.inbox.iter_mut().find(|pr| pr.key.id() == key) {
                        pr.stats_error = stats.is_none();
                        if stats.is_some() {
                            pr.stats = stats;
                        }
                    }
                }
            }
            Message::Detail(id, output) => {
                let r = self.reviews.entry(id.clone()).or_default();
                r.loading = false;
                r.poll_at = Some(Instant::now());
                r.revision_poll_at = Some(Instant::now());
                match output {
                    Ok(mut pr) => {
                        r.update_state(&pr.state);
                        if r.detail.as_ref().is_some_and(|old| old.state == "merged") {
                            pr.state = "merged".into();
                        }
                        if let Some(summary) = self.inbox.iter_mut().find(|p| p.key.id() == id) {
                            summary.title = pr.title.clone();
                            summary.author = pr.author.clone();
                            summary.stats = Some(PrStats {
                                additions: pr.additions,
                                deletions: pr.deletions,
                                changed_files: pr.changed_files,
                            });
                            summary.stats_error = false;
                        }
                        if (r.snapshot.is_some() || r.preparing)
                            && r.detail.as_ref().is_some_and(|old| {
                                old.head != pr.head
                                    || old.base != pr.base
                                    || old.title != pr.title
                                    || old.body != pr.body
                            })
                        {
                            r.newer = Some(pr);
                        } else if r.snapshot.is_none() && !r.preparing {
                            r.detail = Some(Arc::new(pr));
                        }
                        r.detail_error = None;
                    }
                    Err(e) => r.detail_error = Some(e),
                }
                if self.pending_open.as_ref() == Some(&id) {
                    self.pending_open = None;
                    if self.key().as_ref() == Some(&id) {
                        self.open();
                    }
                }
            }
            Message::Timeline(id, output) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    match output {
                        Ok(items) => {
                            r.timeline = items;
                            r.timeline_error = None;
                        }
                        Err(e) => r.timeline_error = Some(e),
                    }
                }
            }
            Message::Failures(id, url, failures) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    r.failures_loading.remove(&url);
                    r.failures.insert(url, failures);
                }
            }
            Message::Checks(id, output) => {
                let mut failed = Vec::new();
                if let Some(r) = self.reviews.get_mut(&id) {
                    r.polling = false;
                    match output {
                        Ok(items) => {
                            r.update_state(&items.state);
                            for check in items.checks.iter().filter(|c| c.state == "fail") {
                                if let Some(pr) = &r.detail
                                    && !r.failures.contains_key(&check.url)
                                    && r.failures_loading.insert(check.url.clone())
                                {
                                    failed.push((pr.key.clone(), check.clone()));
                                }
                            }
                            r.checks = items.checks.clone();
                            r.checks_error = items.rules_error.clone();
                            r.check_report = Some(items);
                        }
                        Err(e) => r.checks_error = Some(e),
                    }
                }
                if !failed.is_empty() {
                    self.spawn(move |tx, cancel| {
                        for (key, check) in failed {
                            if cancel.cancelled() {
                                break;
                            }
                            let result = crate::ci::load(&key, &check, &cancel);
                            let _ = tx.send(Message::Failures(id.clone(), check.url, result));
                        }
                    });
                }
            }
            Message::Poll(id, snapshot_id, detail) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    r.revision_polling = false;
                    if r.snapshot_id != snapshot_id {
                        return;
                    }
                    match detail {
                        Ok(Some(mut pr)) => {
                            r.update_state(&pr.state);
                            if r.detail.as_ref().is_some_and(|old| old.state == "merged") {
                                pr.state = "merged".into();
                            }
                            if (r.snapshot.is_some() || r.preparing)
                                && r.detail.as_ref().is_some_and(|old| {
                                    old.head != pr.head
                                        || old.base != pr.base
                                        || old.title != pr.title
                                        || old.body != pr.body
                                })
                            {
                                r.newer = Some(pr);
                            } else {
                                r.newer = None;
                                if r.snapshot.is_none() && !r.preparing {
                                    r.detail = Some(Arc::new(pr));
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            self.notice = Notice::error(format!("Revision refresh failed: {e}"))
                        }
                    }
                }
            }
            Message::SnapshotProgress(id, sequence, progress) => {
                if let Some(r) = self.reviews.get_mut(&id)
                    && r.preparing
                    && r.snapshot_id == sequence
                {
                    r.preparation_progress = Some(progress);
                }
                return;
            }
            Message::Snapshot(id, sequence, output) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    if r.snapshot_id != sequence {
                        return;
                    }
                    r.preparing = false;
                    r.preparation = None;
                    match output {
                        Ok((root, snapshot)) => {
                            let candidate = r.preparing_detail.take();
                            if candidate.as_ref().is_none_or(|detail| {
                                detail.head != snapshot.head || detail.base != snapshot.base
                            }) {
                                r.guide_error = Some("PR details changed during preparation. Refresh and retry to load a matching snapshot.".into());
                                return;
                            }
                            if r.newer.as_ref().is_some_and(|newer| {
                                newer.head == snapshot.head && newer.base == snapshot.base
                            }) {
                                r.newer = None;
                            }
                            r.detail = candidate;
                            r.guide = None;
                            r.guide_model = None;
                            self.file = 0;
                            self.directory = None;
                            self.tree_horizontal = 0;
                            self.scroll = 0;
                            if let Some(detail) = &r.detail {
                                self.config
                                    .repositories
                                    .insert(detail.key.repository(), root.clone());
                            }
                            r.root = Some(root);
                            r.snapshot = Some(Arc::new(snapshot));
                            r.interaction = Default::default();
                            self.workflow.cursor = None;
                            self.workflow.selection = None;
                            self.workflow.nav = 0;
                            r.context.clear();
                            r.bounds.clear();
                            r.expanded.clear();
                            r.guide_error = None;
                            self.save_config();
                            self.generate_for(&id, false);
                        }
                        Err(error) => {
                            r.guide_error = Some(error);
                            r.preparation_failed = true;
                            r.preparing_detail = None;
                        }
                    }
                }
            }
            Message::Progress(id, sequence, activity) => {
                if let Some(job) = self
                    .reviews
                    .get_mut(&id)
                    .and_then(|r| r.generation.as_mut())
                    && job.id == sequence
                    && progress_priority(&activity) >= progress_priority(&job.activity)
                {
                    job.activity = activity;
                }
                return;
            }
            Message::Guide(id, sequence, model, output) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    if r.generation.as_ref().map(|j| j.id) != Some(sequence) {
                        return;
                    }
                    r.generation = None;
                    match output {
                        Ok(guide) => {
                            r.guide = Some(Arc::new(guide));
                            r.guide_model = Some(model);
                            r.guide_error = None;
                        }
                        Err(e) => r.guide_error = Some(e),
                    }
                }
            }
            Message::Context(id, snapshot, path, output) => {
                let Some(review) = self.reviews.get_mut(&id) else {
                    return;
                };
                if !review
                    .snapshot
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &snapshot))
                {
                    return;
                }
                let Some(state) = review.context.get_mut(&path) else {
                    return;
                };
                let pending = std::mem::take(&mut state.pending);
                match output {
                    Ok(data) => {
                        for (hunk, direction, amount) in pending {
                            data.adjust(
                                &hunk,
                                review.expanded.entry(hunk.clone()).or_default(),
                                direction,
                                amount,
                            );
                        }
                        self.preserve_diff_position = true;
                        state.data = Some(Arc::new(data));
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error),
                }
            }
            Message::Models(output) => {
                self.models_loading = false;
                match output {
                    Ok(models) => self.models = models,
                    Err(e) => self.models_error = Some(e),
                }
            }
            Message::Notice(message) => self.notice = Notice::error(message),
            Message::Repositories(output) => {
                self.repositories_loading = false;
                match output {
                    Ok(repositories) => {
                        self.repository_options = repositories;
                        self.ensure_repo_selection();
                    }
                    Err(error) => self.repositories_error = Some(error),
                }
            }
        }
        if let Some(id) = self.key() {
            self.sync_progress(&id);
        }
        self.invalidate();
    }
    fn open_definition(&mut self, path: String, line: u64, column: usize, old: bool) {
        if self.modal.is_some() || self.home || self.view == View::Overview {
            return;
        }
        let Some(review) = self.review() else {
            return;
        };
        let (Some(root), Some(snapshot)) = (&review.root, &review.snapshot) else {
            return;
        };
        let source_path = if old {
            snapshot
                .files
                .iter()
                .find(|f| f.path == path)
                .map(|f| f.old_path.clone())
                .unwrap_or(path)
        } else {
            path
        };
        let request = crate::navigation::Request {
            root: root.clone(),
            revision: if old {
                snapshot.merge_base.clone()
            } else {
                snapshot.head.clone()
            },
            path: source_path,
            line,
            column,
        };
        let id = self.next_id();
        let work = request.clone();
        let cancel = self.spawn(move |tx, cancel| {
            let output = result(crate::navigation::resolve(&work, &cancel).map(Arc::new));
            let _ = tx.send(Message::Definition(id, output));
        });
        let output = cancel.cancelled().then(|| {
            Err("Could not start definition resolution. Close this modal and try again.".into())
        });
        self.modal = Some(Modal::Definition(crate::navigation::Viewer {
            id,
            request,
            output,
            scroll: 0,
            horizontal: 0,
            viewport: 1,
            cancel,
        }));
    }
    pub fn action(&mut self, action: Action) {
        self.preserve_diff_position = false;
        if !matches!(action, Action::Filter) {
            self.filters.focused = None;
        }
        match action {
            Action::Copy => self.copy_diff(),
            Action::Filter => {
                if let Some(kind) = self.filter_kind() {
                    self.filters.focused = Some(kind);
                    self.focus = Focus::Navigation;
                }
            }
            Action::SelectRepository(name) => {
                self.repo_selected = Some(name);
                self.focus = Focus::Navigation;
                self.scroll = 0;
                self.invalidate();
            }
            Action::OpenRepository => self.open_repository(),
            Action::PinRepository(name) => self.pin_repository(name),
            Action::Image(request) => self.modal = Some(Modal::Image(request)),
            Action::Definition {
                path,
                line,
                column,
                old,
            } => self.open_definition(path, line, column, old),
            Action::CloseDefinition => {
                if matches!(self.modal, Some(Modal::Definition(_))) {
                    self.modal = None;
                }
            }
            Action::Workflow(action) => self.workflow_action(action),
            Action::SelectPr(index) => self.select(index),
            Action::OpenPr => self.open(),
            Action::SetView(view) => {
                if self.home {
                    self.workflow.side = crate::review::Side::Right;
                }
                self.workflow.cursor = None;
                self.workflow.selection = None;
                self.workflow.nav = 0;
                self.chapter_target = None;
                let Some(id) = self.key() else {
                    return;
                };
                self.opened = Some(id);
                self.home = false;
                if view != View::Overview && self.review().is_some_and(|r| r.snapshot.is_none()) {
                    self.open();
                }
                self.view = view;
                self.scroll = 0;
                self.horizontal = 0;
                self.focus = if view != View::Diff {
                    Focus::Content
                } else {
                    Focus::Navigation
                };
                if view == View::Diff {
                    self.load_viewed();
                }
                self.invalidate();
            }
            Action::Back => {
                if self.repository_directory() {
                    return;
                }
                if self.home {
                    self.change_inbox(InboxTab::Repositories);
                    return;
                }
                self.home = true;
                self.opened = None;
                self.pending_open = None;
                self.view = View::Overview;
                self.focus = Focus::Navigation;
                self.scroll = 0;
                self.horizontal = 0;
                self.load_inbox();
                self.invalidate();
            }
            Action::SetInbox(tab) => self.change_inbox(tab),
            Action::SetState(state) => {
                if self.repository_directory() {
                    return;
                }
                match self.inbox_tab {
                    InboxTab::MyPrs => self.my_prs_state = state,
                    InboxTab::Repositories => self.repository_state = state,
                }
                self.inbox.clear();
                self.selected = 0;
                self.nav_scroll = 0;
                self.scroll = 0;
                self.pending_open = None;
                self.load_inbox();
                self.invalidate();
            }
            Action::SelectDirectory(path) => {
                self.workflow.cursor = None;
                self.workflow.selection = None;
                self.focus = Focus::Navigation;
                self.directory = Some(path);
                self.scroll = 0;
                self.invalidate();
            }
            Action::SelectFile(file) => {
                self.directory = None;
                self.workflow.cursor = None;
                self.workflow.selection = None;
                self.focus = Focus::Navigation;
                self.file = file;
                self.scroll = 0;
                self.invalidate();
            }
            Action::Jump(row) => {
                self.workflow.cursor = Some(row);
                self.workflow.selection = None;
                self.scroll = row;
                self.focus = Focus::Content;
            }
            Action::ExpandHunk(id, direction) => self.expand_hunk(id, direction),
            Action::GoToChapter(index) => {
                if self
                    .review()
                    .and_then(|r| r.guide.as_ref())
                    .is_some_and(|g| index < g.chapters.len())
                {
                    self.action(Action::SetView(View::Guide));
                    self.chapter_target = Some(index);
                    self.focus = Focus::Content;
                }
            }
            Action::Chapter(next) => {
                if self.home || self.view != View::Guide {
                    return;
                }
                let Some(doc) = &self.document else {
                    return;
                };
                let current = doc
                    .sections
                    .iter()
                    .rposition(|s| s.start <= self.scroll)
                    .unwrap_or(0);
                let index = if next {
                    current.saturating_add(1)
                } else {
                    current.saturating_sub(1)
                };
                if let Some(section) = doc.sections.get(index) {
                    self.scroll = section.start;
                    self.workflow.cursor = Some(section.start);
                    self.workflow.selection = None;
                    self.workflow.nav = doc
                        .navigation
                        .iter()
                        .position(|item| item.chapter == index)
                        .unwrap_or(0);
                    self.focus = Focus::Content;
                }
            }
            Action::Link(url) => {
                self.spawn(move |tx, cancel| {
                    if let Err(e) = github::open_url(&url, &cancel) {
                        let _ = tx.send(Message::Notice(format!("Could not open link: {e:#}")));
                    }
                });
            }
            Action::Models => self.load_models(),
            Action::Locate => {
                if let Some(id) = self.key() {
                    self.modal = Some(Modal::Clone {
                        value: self
                            .review()
                            .and_then(|r| r.root.as_ref())
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                            .into(),
                        key: id,
                    });
                }
            }
            Action::ApplyModel(choice) => self.apply_model(choice),
            Action::ChooseModel(index) => {
                if let Some(Modal::Models { selected, .. }) = &mut self.modal {
                    *selected = index;
                }
            }
            Action::Refresh => self.refresh(),
            Action::Regenerate => {
                if self
                    .review()
                    .is_some_and(|r| r.preparation_failed && r.snapshot.is_some())
                {
                    self.refresh();
                } else if self.review().is_some_and(|r| r.snapshot.is_some()) {
                    self.generate(true);
                } else {
                    self.open();
                }
            }
            Action::Cancel => self.cancel(),
            Action::Help => self.modal = Some(Modal::Help(Default::default())),
            Action::ToggleWrap => {
                let mut config = self.config.clone();
                config.wrap_diff = !config.wrap_diff;
                if let Err(error) = self.storage.save_config(&config) {
                    self.notice =
                        Notice::error(format!("Could not save wrapping preference: {error:#}"));
                    return;
                }
                self.config = config;
                self.horizontal = 0;
                self.workflow.cursor = None;
                self.workflow.selection = None;
                self.invalidate();
            }
            Action::ToggleLayout => {
                self.config.unified = !self.config.unified;
                self.save_config();
                self.invalidate();
            }
            Action::Scroll(delta) => self.move_scroll(delta),
            Action::FastScroll(delta) => {
                self.focus = Focus::Content;
                self.move_scroll(delta);
            }
            Action::Focus(focus) => self.focus = focus,
        }
    }
    fn move_scroll(&mut self, delta: i32) {
        if self.focus == Focus::Navigation && self.home {
            if self.repository_directory() {
                let names = self.visible_repositories();
                let current = names
                    .iter()
                    .position(|n| Some(n) == self.repo_selected.as_ref())
                    .unwrap_or(0);
                let next = current
                    .saturating_add_signed(delta as isize)
                    .min(names.len().saturating_sub(1));
                if let Some(name) = names.get(next) {
                    self.repo_selected = Some(name.clone());
                    self.scroll = 0;
                    self.invalidate();
                }
            } else {
                let visible = self.visible_prs();
                let current = visible
                    .iter()
                    .position(|i| *i == self.selected)
                    .unwrap_or(0);
                let next = current
                    .saturating_add_signed(delta as isize)
                    .min(visible.len().saturating_sub(1));
                if let Some(index) = visible.get(next) {
                    self.select(*index);
                }
            }
        } else if self.focus == Focus::Navigation
            && self.view != View::Overview
            && (self.view == View::Diff || self.review().is_none_or(|r| r.guide.is_none()))
        {
            let entries = self
                .review()
                .and_then(|r| r.snapshot.as_ref())
                .map(|s| crate::tree::filtered(&s.files, &self.filters.files.text()))
                .unwrap_or_default();
            let current = entries
                .iter()
                .position(|entry| match &self.directory {
                    Some(path) => entry.file.is_none() && &entry.path == path,
                    None => entry.file == Some(self.file),
                })
                .unwrap_or(0);
            let next = current
                .saturating_add_signed(delta as isize)
                .min(entries.len().saturating_sub(1));
            if let Some(entry) = entries.get(next) {
                self.action(match entry.file {
                    Some(index) => Action::SelectFile(index),
                    None => Action::SelectDirectory(entry.path.clone()),
                });
            }
        } else if self.focus == Focus::Navigation && self.view == View::Guide {
            let count = self.document.as_ref().map_or(0, |d| d.navigation.len());
            let index = self
                .workflow
                .nav
                .saturating_add_signed(delta as isize)
                .min(count.saturating_sub(1));
            self.workflow_action(crate::workflow::WAction::Nav(index));
        } else if !self.home && self.view != View::Overview {
            self.move_diff(delta, false);
        } else {
            let max = self
                .document
                .as_ref()
                .map(|d| d.max_scroll(self.viewport))
                .unwrap_or(0);
            self.scroll = self.scroll.saturating_add_signed(delta as isize).min(max);
        }
    }
    pub fn key_event(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::SUPER) {
            let editor = match &self.modal {
                Some(Modal::Help(state)) => Some(&state.query),
                Some(Modal::Clone { value, .. }) => Some(value),
                Some(Modal::Models { query, .. }) => Some(query),
                Some(Modal::Workflow(wizard)) => match wizard.as_ref() {
                    crate::workflow::Wizard::Controls { query, .. } => Some(query),
                    crate::workflow::Wizard::Compose(draft) if draft.focus == 0 => {
                        Some(&draft.editor)
                    }
                    _ => None,
                },
                None => self.filters.focused.map(|kind| self.filters.editor(kind)),
                _ => None,
            };
            if let Some(editor) = editor {
                self.clipboard = editor.selected_text();
                return;
            }
        }

        if self.hover.key(key) {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if matches!(self.modal, Some(Modal::Workflow(_))) {
            self.workflow_key(key);
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        if self.filters.focused.is_some() {
            self.filter_key(key);
            return;
        }
        let plain = !key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        );
        match key.code {
            KeyCode::Char('{' | '}') if plain => {
                self.adjust_focused_hunk(if key.code == KeyCode::Char('}') {
                    1
                } else {
                    -1
                });
            }
            KeyCode::Char('[' | ']') if plain && key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.adjust_focused_hunk(if key.code == KeyCode::Char(']') {
                    1
                } else {
                    -1
                });
            }
            KeyCode::Char('c') if plain || key.modifiers == KeyModifiers::SUPER => {
                self.action(Action::Copy)
            }
            KeyCode::Char('/') => self.workflow_action(crate::workflow::WAction::Open),
            KeyCode::Up | KeyCode::Down
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    && self.focus == Focus::Content
                    && !self.home
                    && self.view != View::Overview =>
            {
                self.move_diff(if key.code == KeyCode::Up { -1 } else { 1 }, true);
            }
            KeyCode::Left | KeyCode::Right
                if self.focus == Focus::Content
                    && !self.home
                    && self.view != View::Overview
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.workflow.side = if key.code == KeyCode::Left {
                    crate::review::Side::Left
                } else {
                    crate::review::Side::Right
                };
                self.workflow.selection = None;
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                self.action(Action::Chapter(false))
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                self.action(Action::Chapter(true))
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SUPER) => {
                self.action(Action::FastScroll(-10))
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SUPER) => {
                self.action(Action::FastScroll(10))
            }
            KeyCode::Esc => {
                if self.home
                    && self.inbox_tab == InboxTab::Repositories
                    && self.repository.is_some()
                {
                    self.action(Action::Back);
                } else if self.home {
                    self.quit = true;
                } else {
                    self.action(Action::Back);
                }
            }
            KeyCode::Enter => {
                if self.home {
                    self.open();
                } else if self.view != View::Overview {
                    self.enter_diff();
                }
            }
            KeyCode::Up => self.move_scroll(-1),
            KeyCode::Down => self.move_scroll(1),
            KeyCode::PageUp => self.move_scroll(-(self.viewport as i32)),
            KeyCode::PageDown | KeyCode::Char(' ') => self.move_scroll(self.viewport as i32),
            KeyCode::Home => {
                self.workflow.cursor = Some(0);
                self.workflow.selection = None;
                self.scroll = 0;
                if self.focus == Focus::Navigation && self.home {
                    self.move_scroll(i32::MIN);
                }
            }
            KeyCode::End => {
                self.move_scroll(i32::MAX);
            }
            KeyCode::Left | KeyCode::Right if self.focus == Focus::Navigation => {
                self.tree_horizontal = if key.code == KeyCode::Left {
                    self.tree_horizontal.saturating_sub(4)
                } else {
                    self.tree_horizontal
                        .saturating_add(4)
                        .min(self.tree_max_horizontal)
                };
            }
            KeyCode::Left | KeyCode::Right
                if !self.home && self.view != View::Overview && self.config.wrap_diff => {}
            KeyCode::Left => self.horizontal = self.horizontal.saturating_sub(4),
            KeyCode::Right => self.horizontal = self.horizontal.saturating_add(4),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus =
                    if self.focus == Focus::Content && (self.home || self.view != View::Overview) {
                        Focus::Navigation
                    } else {
                        Focus::Content
                    }
            }
            KeyCode::Char('1') => self.action(if self.home {
                Action::SetInbox(InboxTab::MyPrs)
            } else {
                Action::SetView(View::Overview)
            }),
            KeyCode::Char('2') => self.action(if self.home {
                Action::SetInbox(InboxTab::Repositories)
            } else {
                Action::SetView(View::Guide)
            }),
            KeyCode::Char('3') if !self.home => self.action(Action::SetView(View::Diff)),
            KeyCode::Char('?') if plain => self.action(Action::Help),
            KeyCode::Char('m') if plain => self.load_models(),
            KeyCode::Char('s' | '[' | ']')
                if plain && self.home && !self.repository_directory() =>
            {
                let index = PrState::ALL
                    .iter()
                    .position(|s| *s == self.state())
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Char('[') {
                    (index + 3) % 4
                } else {
                    (index + 1) % 4
                };
                if let Some(state) = PrState::ALL.get(next) {
                    self.action(Action::SetState(*state));
                }
            }
            KeyCode::Char('f') if plain => self.action(Action::Filter),
            KeyCode::Char('*') if plain && self.repository_directory() => {
                if let Some(name) = self.repo_selected.clone() {
                    self.action(Action::PinRepository(name));
                }
            }
            KeyCode::Char('w') if plain => self.action(Action::ToggleWrap),
            KeyCode::Char('r') if plain => self.refresh(),
            KeyCode::Char('g') if plain => self.action(Action::Regenerate),
            KeyCode::Char('l') if plain => self.action(Action::Locate),
            KeyCode::Char('x') if plain => self.cancel(),
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.action(Action::ToggleLayout);
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(pr) = self.review().and_then(|r| r.detail.as_ref()) {
                    self.action(Action::Link(pr.key.url()));
                }
            }
            _ => {}
        }
    }
    fn modal_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Image(request) => {
                if key.code == KeyCode::Char('o')
                    && let Ok(url) = request.browser_url()
                {
                    self.action(Action::Link(url.into()));
                }
                self.modal = Some(Modal::Image(request));
            }
            Modal::Definition(mut viewer) => {
                let total = viewer
                    .output
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .map_or(0, |d| d.source.lines().count());
                let max = total.saturating_sub(viewer.viewport);
                let step = if key.modifiers.contains(KeyModifiers::SUPER) {
                    10
                } else {
                    1
                };
                match key.code {
                    KeyCode::Up => viewer.scroll = viewer.scroll.saturating_sub(step),
                    KeyCode::Down => viewer.scroll = viewer.scroll.saturating_add(step).min(max),
                    KeyCode::PageUp => {
                        viewer.scroll = viewer.scroll.saturating_sub(viewer.viewport)
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        viewer.scroll = viewer.scroll.saturating_add(viewer.viewport).min(max)
                    }
                    KeyCode::Home => viewer.scroll = 0,
                    KeyCode::End => viewer.scroll = max,
                    KeyCode::Left => viewer.horizontal = viewer.horizontal.saturating_sub(4),
                    KeyCode::Right => viewer.horizontal = viewer.horizontal.saturating_add(4),
                    _ => {}
                }
                self.modal = Some(Modal::Definition(viewer));
            }
            Modal::Workflow(modal) => self.modal = Some(Modal::Workflow(modal)),
            Modal::Help(mut state) => {
                let before = state.query.text();
                let max = state.rows.saturating_sub(state.viewport);
                match key.code {
                    KeyCode::Up if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        state.scroll = state.scroll.saturating_sub(1)
                    }
                    KeyCode::Down if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        state.scroll = state.scroll.saturating_add(1).min(max)
                    }
                    KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(state.viewport),
                    KeyCode::PageDown => {
                        state.scroll = state.scroll.saturating_add(state.viewport).min(max)
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.query.clear()
                    }
                    _ => state.query.key(key),
                }
                if before != state.query.text() {
                    state.scroll = 0;
                }
                self.modal = Some(Modal::Help(state));
            }
            Modal::Clone { mut value, key: id } => {
                match key.code {
                    KeyCode::Enter => {
                        if !value.text().trim().is_empty() {
                            self.prepare(PathBuf::from(value.text().trim()));
                        }
                        return;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear()
                    }
                    _ => value.key(key),
                }
                self.modal = Some(Modal::Clone { value, key: id });
            }
            Modal::Models {
                mut selected,
                effort,
                mut query,
            } => {
                let options = self.model_options(&query.text());
                match key.code {
                    KeyCode::Up if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        selected = selected.saturating_sub(1)
                    }
                    KeyCode::Down if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        selected = (selected + 1).min(options.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        if let Some(choice) = options.get(selected) {
                            self.apply_model(choice.clone());
                            return;
                        }
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.clear();
                        selected = 0;
                    }
                    _ => {
                        query.key(key);
                        selected = 0;
                    }
                }
                self.modal = Some(Modal::Models {
                    selected,
                    effort,
                    query,
                });
            }
        }
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        self.hover.mouse(event);
        let definitions = self.hover.active();
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, action)) = self.hits.iter().rev().find(|(rect, action)| {
                    rect.contains((event.column, event.row).into())
                        && (definitions || !matches!(action, Action::Definition { .. }))
                }) {
                    self.action(action.clone());
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if matches!(self.modal, Some(Modal::Definition(_) | Modal::Help(_))) =>
            {
                let code = if event.kind == MouseEventKind::ScrollDown {
                    KeyCode::Down
                } else {
                    KeyCode::Up
                };
                self.modal_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if self.modal.is_none() => {
                self.focus = self
                    .hits
                    .iter()
                    .rev()
                    .find_map(|(rect, action)| {
                        if rect.contains((event.column, event.row).into())
                            && let Action::Focus(focus) = action
                        {
                            Some(*focus)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(self.focus);
                self.move_scroll(if event.kind == MouseEventKind::ScrollDown {
                    3
                } else {
                    -3
                });
            }
            _ => {}
        }
    }
    pub fn paste(&mut self, text: String) {
        if self.modal.is_none()
            && let Some(kind) = self.filters.focused
        {
            self.filters
                .editor_mut(kind)
                .insert(&text.replace(['\n', '\r', '\t'], " "));
            self.filter_changed();
            return;
        }
        match &mut self.modal {
            Some(Modal::Help(state)) => {
                state.query.insert(&text.replace(['\n', '\r', '\t'], " "));
                state.scroll = 0;
            }
            Some(Modal::Workflow(modal)) => match modal.as_mut() {
                crate::workflow::Wizard::Compose(draft) if draft.focus == 0 => {
                    draft.editor.insert(&text);
                }
                crate::workflow::Wizard::Controls {
                    query, selected, ..
                } => {
                    query.insert(&text.replace(['\n', '\r', '\t'], " "));
                    *selected = 0;
                }
                _ => {}
            },
            Some(Modal::Clone { value, .. }) => value.insert(text.trim()),
            Some(Modal::Models {
                query, selected, ..
            }) => {
                query.insert(text.trim());
                *selected = 0;
            }
            _ => {}
        }
    }
    pub fn shutdown(&mut self) {
        for (cancel, _) in &self.jobs {
            cancel.cancel();
        }
        for (_, job) in self.jobs.drain(..) {
            let _ = job.join();
        }
    }
}
impl App {
    pub fn repository_directory(&self) -> bool {
        self.home && self.inbox_tab == InboxTab::Repositories && self.repository.is_none()
    }
    pub fn filter_kind(&self) -> Option<crate::filter::Kind> {
        use crate::filter::Kind;
        if self.repository_directory() {
            Some(Kind::Repositories)
        } else if self.home {
            Some(Kind::PullRequests)
        } else if self.view == View::Diff
            || (self.view == View::Guide && self.review().is_none_or(|r| r.guide.is_none()))
        {
            Some(Kind::Files)
        } else {
            None
        }
    }
    pub fn visible_prs(&self) -> Vec<usize> {
        crate::filter::prs(&self.inbox, &self.filters.prs.text())
    }
    pub fn visible_repositories(&self) -> Vec<String> {
        let query = self.filters.repositories.text();
        let mut names = self
            .repository_options
            .iter()
            .filter(|name| crate::filter::matches(&query, name))
            .cloned()
            .collect::<Vec<_>>();
        names.sort_by_key(|name| {
            (
                !self.config.pinned_repositories.contains(name),
                name.to_lowercase(),
                name.clone(),
            )
        });
        names.dedup();
        names
    }
    fn ensure_pr_selection(&mut self) {
        let visible = self.visible_prs();
        let index = if visible.contains(&self.selected) {
            Some(self.selected)
        } else {
            visible.first().copied()
        };
        if let Some(index) = index {
            if self.home {
                self.select(index);
            } else {
                self.selected = index;
            }
        }
        self.invalidate();
    }
    fn ensure_repo_selection(&mut self) {
        let names = self.visible_repositories();
        if !self
            .repo_selected
            .as_ref()
            .is_some_and(|name| names.contains(name))
        {
            self.repo_selected = names.first().cloned();
        }
        self.invalidate();
    }
    fn load_repository_list(&mut self) {
        if let Some(cancel) = self.inbox_cancel.take() {
            cancel.cancel();
        }
        self.inbox_id = self.next_id();
        self.inbox_loading = false;
        if self.repositories_loading {
            return;
        }
        match self.storage.load_repositories() {
            Ok(Some(names)) => self.repository_options = names,
            Ok(None) => {}
            Err(error) => {
                self.notice =
                    Notice::error(format!("Could not load cached repositories: {error:#}"))
            }
        }
        self.ensure_repo_selection();
        self.repositories_loading = true;
        self.repositories_error = None;
        let storage = self.storage.clone();
        self.spawn(move |tx, cancel| {
            let output = github::repositories(&cancel);
            if let Ok(names) = &output
                && let Err(error) = storage.save_repositories(names)
            {
                let _ = tx.send(Message::Notice(format!(
                    "Could not cache repositories: {error:#}"
                )));
            }
            let _ = tx.send(Message::Repositories(result(output)));
        });
    }
    fn open_repository(&mut self) {
        if !self.repository_directory() {
            return;
        }
        let Some(name) = self
            .repo_selected
            .clone()
            .filter(|n| self.visible_repositories().contains(n))
        else {
            return;
        };
        self.repository = Some(name);
        self.filters.focused = None;
        self.inbox.clear();
        self.selected = 0;
        self.nav_scroll = 0;
        self.scroll = 0;
        self.focus = Focus::Navigation;
        self.load_inbox();
        self.invalidate();
    }
    fn pin_repository(&mut self, name: String) {
        if !self.repository_options.contains(&name) {
            return;
        }
        let mut config = self.config.clone();
        if !config.pinned_repositories.remove(&name) {
            config.pinned_repositories.insert(name);
        }
        if let Err(error) = self.storage.save_config(&config) {
            self.notice = Notice::error(format!("Could not save repository pins: {error:#}"));
            return;
        }
        self.config = config;
        self.ensure_repo_selection();
    }
    fn filter_key(&mut self, key: KeyEvent) {
        let Some(kind) = self.filters.focused else {
            return;
        };
        if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
            self.filters.focused = None;
            self.focus = Focus::Navigation;
            return;
        }
        let before = self.filters.editor(kind).text();
        if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.filters.editor_mut(kind).clear();
        } else {
            self.filters.editor_mut(kind).key(key);
        }
        if before != self.filters.editor(kind).text() {
            self.filter_changed();
        }
    }
    fn filter_changed(&mut self) {
        let focus = self.filters.focused;
        self.nav_scroll = 0;
        match self.filter_kind() {
            Some(crate::filter::Kind::PullRequests) => {
                self.pending_open = None;
                self.ensure_pr_selection();
            }
            Some(crate::filter::Kind::Repositories) => self.ensure_repo_selection(),
            Some(crate::filter::Kind::Files) => {
                let entries = self
                    .review()
                    .and_then(|r| r.snapshot.as_ref())
                    .map(|s| crate::tree::filtered(&s.files, &self.filters.files.text()))
                    .unwrap_or_default();
                let selected = entries.iter().any(|e| match &self.directory {
                    Some(path) => e.file.is_none() && &e.path == path,
                    None => e.file == Some(self.file),
                });
                if !selected && let Some(entry) = entries.first() {
                    self.action(match entry.file {
                        Some(index) => Action::SelectFile(index),
                        None => Action::SelectDirectory(entry.path.clone()),
                    });
                }
                self.tree_horizontal = 0;
                self.invalidate();
            }
            None => {}
        }
        self.filters.focused = focus;
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    #[test]
    fn definition_navigation_cancels_on_close_and_ignores_obsolete_results() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Default::default(),
        );
        let cancel = Cancel::default();
        app.modal = Some(Modal::Definition(crate::navigation::Viewer {
            id: 42,
            request: crate::navigation::Request {
                root: dir.path().into(),
                revision: "a".repeat(40),
                path: "file.ts".into(),
                line: 1,
                column: 0,
            },
            output: None,
            scroll: 0,
            horizontal: 0,
            viewport: 1,
            cancel: cancel.clone(),
        }));
        app.receive(Message::Definition(41, Err("obsolete result".into())));
        assert!(matches!(&app.modal, Some(Modal::Definition(v)) if v.output.is_none()));
        app.receive(Message::Definition(
            42,
            Err("Cannot verify this object's method".into()),
        ));
        assert!(
            matches!(&app.modal, Some(Modal::Definition(v)) if matches!(&v.output, Some(Err(e)) if e.contains("method")))
        );
        for (width, height) in [(1, 1), (24, 8), (80, 24)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
            terminal.draw(|frame| crate::ui::draw(frame, &mut app))?;
        }
        app.key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(cancel.cancelled());
        app.modal = Some(Modal::Help(Default::default()));
        app.receive(Message::Definition(42, Err("late result".into())));
        assert!(matches!(app.modal, Some(Modal::Help(_))));
        Ok(())
    }

    #[test]
    fn context_results_ignore_old_snapshots_and_apply_only_requested_hunks() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Default::default(),
        );
        let files = crate::diff::parse(
            "M\0file\0",
            "diff --git a/file b/file\n@@ -2 +2 @@\n-old\n+new\n",
        )?;
        let snapshot = Arc::new(Snapshot {
            base: "base".into(),
            head: "head".into(),
            merge_base: "base".into(),
            head_tree: "head tree".into(),
            base_tree: "base tree".into(),
            files,
        });
        let old = Arc::new((*snapshot).clone());
        let mut state = FileState::default();
        state.pending.push(("f0-h0".into(), Direction::Below, 10));
        app.reviews.insert(
            "pr".into(),
            Review {
                snapshot: Some(snapshot.clone()),
                context: HashMap::from([("file".into(), state)]),
                ..Default::default()
            },
        );
        app.receive(Message::Context(
            "pr".into(),
            old,
            "file".into(),
            Err("stale failure".into()),
        ));
        let state = app
            .reviews
            .get("pr")
            .and_then(|r| r.context.get("file"))
            .ok_or_else(|| anyhow::anyhow!("Missing context state"))?;
        assert!(state.error.is_none());
        assert_eq!(state.pending.len(), 1);
        let file = snapshot
            .files
            .first()
            .ok_or_else(|| anyhow::anyhow!("Missing file"))?;
        let hunk = file
            .hunks
            .first()
            .ok_or_else(|| anyhow::anyhow!("Missing hunk"))?;
        let mut lines = hunk.lines.clone();
        lines.push(crate::diff::DiffLine {
            kind: crate::diff::LineKind::Context,
            old: Some(3),
            new: Some(3),
            text: "tail".into(),
        });
        let data = FileContext::new(file, lines)?;
        app.receive(Message::Context(
            "pr".into(),
            snapshot,
            "file".into(),
            Ok(data),
        ));
        let review = app
            .reviews
            .get("pr")
            .ok_or_else(|| anyhow::anyhow!("Missing review"))?;
        assert_eq!(review.expanded.len(), 1);
        assert_eq!(review.expanded.get("f0-h0").map(|e| e.below), Some(1));
        assert!(
            review
                .context
                .get("file")
                .is_some_and(|s| s.pending.is_empty() && s.data.is_some())
        );
        Ok(())
    }

    #[test]
    fn commentary_takes_priority_over_summary_and_tool_activity() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Config::default(),
        );
        app.reviews.insert(
            "pr".into(),
            Review {
                generation: Some(Generation {
                    id: 7,
                    cancel: Cancel::default(),
                    started: Instant::now(),
                    activity: "Starting Codex".into(),
                }),
                ..Review::default()
            },
        );
        for (sequence, incoming, expected) in [
            (
                7,
                "Codex summary: Reading types",
                "Codex summary: Reading types",
            ),
            (
                7,
                "Reading repository context",
                "Codex summary: Reading types",
            ),
            (
                7,
                "Codex summary: Checking callers",
                "Codex summary: Checking callers",
            ),
            (
                7,
                "Codex: Grouping the routing changes",
                "Codex: Grouping the routing changes",
            ),
            (
                7,
                "Codex summary: Reading tests",
                "Codex: Grouping the routing changes",
            ),
            (
                6,
                "Codex: Stale generation",
                "Codex: Grouping the routing changes",
            ),
            (
                7,
                "Validating chapter coverage",
                "Validating chapter coverage",
            ),
        ] {
            app.receive(Message::Progress("pr".into(), sequence, incoming.into()));
            assert_eq!(
                app.reviews
                    .get("pr")
                    .and_then(|r| r.generation.as_ref())
                    .map(|g| g.activity.as_str()),
                Some(expected)
            );
        }
        Ok(())
    }

    #[test]
    fn metadata_batches_ignore_stale_replies_and_keep_cached_counts_on_failure() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Config::default(),
        );
        let key = detail("head").key;
        let id = key.id();
        let stats = PrStats {
            additions: 42,
            deletions: 7,
            changed_files: 3,
        };
        app.inbox.push(PrSummary {
            key,
            title: "PR".into(),
            author: "author".into(),
            updated: String::new(),
            created: "2026-09-10T12:00:00Z".into(),
            stats: None,
            stats_error: false,
            draft: false,
        });
        app.inbox_id = 2;
        app.receive(Message::InboxStats(
            1,
            vec![(id.clone(), Some(stats.clone()))],
        ));
        assert!(app.inbox.first().is_some_and(|p| p.stats.is_none()));
        app.receive(Message::InboxStats(
            2,
            vec![(id.clone(), Some(stats.clone()))],
        ));
        assert_eq!(
            app.inbox.first().and_then(|p| p.stats.as_ref()),
            Some(&stats)
        );
        app.receive(Message::InboxStats(2, vec![(id, None)]));
        assert!(
            app.inbox
                .first()
                .is_some_and(|p| p.stats_error && p.stats.as_ref() == Some(&stats))
        );
        app.storage.save_inbox("fixture", &app.inbox)?;
        let restored = app
            .storage
            .load_inbox("fixture")?
            .context("Missing cache")?;
        assert_eq!(
            restored.first().and_then(|p| p.stats.as_ref()),
            Some(&stats)
        );
        assert_eq!(
            restored.first().map(|p| p.created.as_str()),
            Some("2026-09-10T12:00:00Z")
        );
        assert!(app.storage.load_inbox("other-filter")?.is_none());
        Ok(())
    }

    #[test]
    fn inbox_results_cannot_cross_requests_or_replace_an_opened_pr() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Config::default(),
        );
        app.inbox_id = 2;
        app.inbox_loading = true;
        app.receive(Message::Inbox(1, Err("old request".into())));
        assert!(app.inbox_loading);
        assert!(app.inbox_error.is_none());
        let pr = detail("original");
        let id = pr.key.id();
        app.opened = Some(id.clone());
        app.home = false;
        app.reviews.insert(
            id.clone(),
            Review {
                detail: Some(Arc::new(pr)),
                ..Review::default()
            },
        );
        app.receive(Message::Inbox(2, Ok(vec![])));
        app.receive(Message::InboxFinished(2));
        assert!(!app.inbox_loading);
        assert_eq!(app.key(), Some(id));
        assert!(app.review().is_some());
        Ok(())
    }
    fn detail(head: &str) -> PrDetail {
        PrDetail {
            key: PrKey {
                owner: "owner".into(),
                repo: "repo".into(),
                number: 1,
            },
            title: "Title".into(),
            body: "Body".into(),
            author: "Author".into(),
            head: head.into(),
            base: "base".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
            state: "open".into(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
        }
    }
    #[test]
    fn live_pr_state_updates_without_replacing_pinned_code_or_guide() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Config::default(),
        );
        let pr = detail("pinned");
        let id = pr.key.id();
        let snapshot = Arc::new(Snapshot {
            base: "base".into(),
            head: "pinned".into(),
            merge_base: "base".into(),
            head_tree: "tree".into(),
            base_tree: "base tree".into(),
            files: Vec::new(),
        });
        let guide = Arc::new(Guide {
            chapters: Vec::new(),
        });
        app.reviews.insert(
            id.clone(),
            Review {
                detail: Some(Arc::new(pr.clone())),
                snapshot: Some(snapshot.clone()),
                guide: Some(guide.clone()),
                ..Review::default()
            },
        );
        for state in ["CLOSED", "OPEN", "MERGED"] {
            app.receive(Message::Checks(
                id.clone(),
                Ok(CheckReport {
                    state: state.into(),
                    head: "new".into(),
                    base: "new base".into(),
                    ..CheckReport::default()
                }),
            ));
            let review = app.reviews.get(&id).context("Missing review")?;
            let pinned = review.detail.as_ref().context("Missing detail")?;
            assert_eq!(pinned.state, state.to_lowercase());
            assert_eq!(pinned.head, "pinned");
            assert!(Arc::ptr_eq(
                review.snapshot.as_ref().context("Missing snapshot")?,
                &snapshot
            ));
            assert!(Arc::ptr_eq(
                review.guide.as_ref().context("Missing guide")?,
                &guide
            ));
        }
        // A queued response from before merging cannot resurrect the Open badge.
        app.receive(Message::Detail(id.clone(), Ok(pr)));
        assert_eq!(
            app.reviews
                .get(&id)
                .and_then(|r| r.detail.as_ref())
                .map(|p| p.state.as_str()),
            Some("merged")
        );
        // Full-detail refreshes must also update a pinned review's lifecycle state.
        app.reviews.get_mut(&id).context("Missing review")?.detail =
            Some(Arc::new(detail("pinned")));
        let mut merged = detail("new");
        merged.state = "merged".into();
        app.receive(Message::Detail(id.clone(), Ok(merged)));
        let review = app.reviews.get(&id).context("Missing review")?;
        assert_eq!(
            review.detail.as_ref().context("Missing detail")?.state,
            "merged"
        );
        assert_eq!(
            review.detail.as_ref().context("Missing detail")?.head,
            "pinned"
        );
        assert_eq!(
            review.newer.as_ref().context("Missing new revision")?.head,
            "new"
        );
        Ok(())
    }

    #[test]
    fn polling_pins_inflight_snapshot_and_ignores_stale_results() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Config::default(),
        );
        let initial = detail("original");
        let id = initial.key.id();
        app.reviews.insert(
            id.clone(),
            Review {
                detail: Some(Arc::new(initial)),
                preparing: true,
                snapshot_id: 2,
                ..Review::default()
            },
        );
        app.receive(Message::Poll(id.clone(), 2, Ok(Some(detail("new")))));
        let review = app.reviews.get(&id).context("Missing review")?;
        assert_eq!(
            review.detail.as_ref().context("Missing detail")?.head,
            "original"
        );
        assert_eq!(review.newer.as_ref().context("Missing update")?.head, "new");
        app.receive(Message::Poll(id.clone(), 1, Ok(Some(detail("obsolete")))));
        let review = app.reviews.get(&id).context("Missing review")?;
        assert_eq!(review.newer.as_ref().context("Missing update")?.head, "new");
        app.receive(Message::Snapshot(
            id.clone(),
            1,
            Err("obsolete error".into()),
        ));
        assert!(
            app.reviews
                .get(&id)
                .is_some_and(|r| r.preparing && r.guide_error.is_none())
        );
        Ok(())
    }
    #[test]
    fn stale_generation_cannot_replace_the_active_guide() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().into(),
            },
            Config::default(),
        );
        let id = detail("original").key.id();
        app.reviews.insert(
            id.clone(),
            Review {
                generation: Some(Generation {
                    id: 3,
                    cancel: Cancel::default(),
                    started: Instant::now(),
                    activity: "active".into(),
                }),
                ..Review::default()
            },
        );
        app.receive(Message::Guide(
            id.clone(),
            2,
            ModelChoice::default(),
            Ok(Guide { chapters: vec![] }),
        ));
        assert!(
            app.reviews.get(&id).is_some_and(
                |r| r.guide.is_none() && r.generation.as_ref().is_some_and(|g| g.id == 3)
            )
        );
        Ok(())
    }
}
