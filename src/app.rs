use crate::{
    codex::{self, Guide},
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
    collections::{BTreeMap, HashMap},
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

pub struct Generation {
    pub id: u64,
    pub cancel: Cancel,
    pub started: Instant,
    pub activity: String,
}
#[derive(Default)]
pub struct Review {
    pub detail: Option<Arc<PrDetail>>,
    pub timeline: Vec<TimelineItem>,
    pub checks: Vec<Check>,
    pub detail_error: Option<String>,
    pub timeline_error: Option<String>,
    pub checks_error: Option<String>,
    pub root: Option<PathBuf>,
    pub snapshot: Option<Arc<Snapshot>>,
    pub guide: Option<Arc<Guide>>,
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

pub enum Message {
    Inbox(u64, Result<Vec<PrSummary>, String>),
    InboxStats(u64, Vec<(String, Option<PrStats>)>),
    Repositories(Result<Vec<String>, String>),
    Detail(String, Result<PrDetail, String>),
    Timeline(String, Result<Vec<TimelineItem>, String>),
    Checks(String, Result<Vec<Check>, String>),
    Poll(String, u64, Result<Option<PrDetail>, String>),
    Snapshot(String, u64, Result<(PathBuf, Snapshot), String>),
    SnapshotProgress(String, u64, repo::SnapshotProgress),
    Progress(String, u64, String),
    Guide(String, u64, ModelChoice, Result<Guide, String>),
    Models(Result<Vec<ModelInfo>, String>),
    Notice(String),
}

#[derive(Clone, Debug)]
pub enum Action {
    SelectPr(usize),
    OpenPr,
    SetView(View),
    SetInbox(InboxTab),
    SetState(PrState),
    Back,
    Repositories(bool),
    ToggleRepository(String),
    AllRepositories,
    SaveRepositories,
    SelectFile(usize),
    Jump(usize),
    Chapter(bool),
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
    Scroll(i32),
    FastScroll(i32),
    Focus(Focus),
}

pub enum Modal {
    Clone {
        value: String,
        key: String,
    },
    Models {
        selected: usize,
        effort: usize,
        query: String,
    },
    Help,
    Repositories {
        manage: bool,
        query: String,
        selected: usize,
        choices: BTreeMap<String, bool>,
    },
}

pub struct App {
    pub storage: Storage,
    pub config: Config,
    pub inbox: Vec<PrSummary>,
    pub selected: usize,
    pub inbox_loading: bool,
    pub inbox_error: Option<String>,
    pub home: bool,
    pub inbox_tab: InboxTab,
    pub authored_state: PrState,
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
    pub modal: Option<Modal>,
    pub models: Vec<ModelInfo>,
    pub models_loading: bool,
    pub models_error: Option<String>,
    pub notice: String,
    pub quit: bool,
    pub epoch: u64,
    pub document: Option<crate::ui::Document>,
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
            home: true,
            inbox_tab: InboxTab::ReviewRequests,
            authored_state: PrState::Open,
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
            modal: None,
            models: Vec::new(),
            models_loading: false,
            models_error: None,
            notice: String::new(),
            quit: false,
            epoch: 0,
            document: None,
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
    fn spawn(&mut self, work: impl FnOnce(Sender<Message>, Cancel) + Send + 'static) -> Cancel {
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
                self.notice = format!("Could not start background work: {error}");
            }
        }
        cancel
    }
    pub fn key(&self) -> Option<String> {
        if !self.home {
            return self.opened.clone();
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
            self.notice = format!("Could not save settings: {e:#}");
        }
    }
    pub fn load_inbox(&mut self) {
        if let Some(cancel) = self.inbox_cancel.take() {
            cancel.cancel();
        }
        let id = self.next_id();
        self.inbox_id = id;
        self.inbox_loading = true;
        self.inbox_error = None;
        let tab = self.inbox_tab;
        let state = self.state();
        let repositories = self
            .config
            .review_repositories
            .iter()
            .filter(|(_, enabled)| **enabled)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let cache_key = crate::storage::hash(format!("v1:{tab:?}:{state:?}:{repositories:?}"));
        match self.storage.load_inbox(&cache_key) {
            Ok(Some(inbox)) => {
                let previous = self.inbox.get(self.selected).map(|p| p.key.id());
                self.inbox = inbox;
                self.selected = previous
                    .and_then(|id| self.inbox.iter().position(|p| p.key.id() == id))
                    .unwrap_or(0);
                self.select(self.selected);
            }
            Ok(None) => {}
            Err(error) => self.notice = format!("Could not load cached PRs: {error:#}"),
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
        }));
    }

    pub fn state(&self) -> PrState {
        match self.inbox_tab {
            InboxTab::ReviewRequests => PrState::Open,
            InboxTab::Authored => self.authored_state,
            InboxTab::Repositories => self.repository_state,
        }
    }
    fn change_inbox(&mut self, tab: InboxTab) {
        self.home = true;
        self.opened = None;
        self.pending_open = None;
        self.inbox_tab = tab;
        self.view = View::Overview;
        self.inbox.clear();
        self.selected = 0;
        self.nav_scroll = 0;
        self.scroll = 0;
        self.horizontal = 0;
        self.focus = Focus::Navigation;
        self.notice.clear();
        self.load_inbox();
        self.invalidate();
        if tab == InboxTab::Repositories && self.config.review_repositories.is_empty() {
            self.choose_repositories(true);
        }
    }
    fn choose_repositories(&mut self, manage: bool) {
        let choices = match &self.modal {
            Some(Modal::Repositories { choices, .. }) => choices.clone(),
            _ => self.config.review_repositories.clone(),
        };
        self.modal = Some(Modal::Repositories {
            manage,
            query: String::new(),
            selected: 0,
            choices,
        });
        if manage && !self.repositories_loading {
            self.repositories_loading = true;
            self.repositories_error = None;
            self.spawn(|tx, cancel| {
                let _ = tx.send(Message::Repositories(result(github::repositories(&cancel))));
            });
        }
    }
    pub fn repository_choices(
        &self,
        manage: bool,
        query: &str,
        choices: &BTreeMap<String, bool>,
    ) -> Vec<String> {
        let mut names = choices.keys().cloned().collect::<Vec<_>>();
        if manage {
            names.extend(self.repository_options.iter().cloned());
        }
        names.sort_by_key(|r| r.to_lowercase());
        names.dedup();
        let query = query.to_lowercase();
        names.retain(|r| r.to_lowercase().contains(&query));
        names
    }
    fn toggle_repository(&mut self, name: String) {
        if validate_repository(&name).is_err() {
            return;
        }
        if let Some(Modal::Repositories {
            manage, choices, ..
        }) = &mut self.modal
        {
            if *manage {
                if choices.remove(&name).is_none() {
                    choices.insert(name, true);
                }
            } else if let Some(enabled) = choices.get_mut(&name) {
                *enabled = !*enabled;
            }
        }
    }
    fn save_repositories(&mut self) {
        if let Some(Modal::Repositories { choices, .. }) = self.modal.take() {
            self.config.review_repositories = choices;
            self.save_config();
            // An explicitly empty whitelist stays empty, without reopening the picker.
            self.inbox.clear();
            self.selected = 0;
            self.scroll = 0;
            self.nav_scroll = 0;
            self.pending_open = None;
            self.load_inbox();
            self.invalidate();
        }
    }
    pub fn select(&mut self, index: usize) {
        if index >= self.inbox.len() {
            return;
        }
        if index != self.selected {
            self.scroll = 0;
            self.file = 0;
            self.horizontal = 0;
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
        self.opened = Some(id.clone());
        self.home = false;
        self.view = View::Guide;
        self.focus = Focus::Navigation;
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
                    .unwrap_or_default(),
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
                    self.notice = "Loaded cached guide".into();
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
        let cancel = self.spawn(move |tx, cancel| {
            let progress_tx = tx.clone();
            let progress_id = job_id.clone();
            let output = codex::generate(
                &root,
                &pr,
                &snapshot,
                &choice,
                &storage,
                &cancel,
                move |message| {
                    let _ = progress_tx.send(Message::Progress(
                        progress_id.clone(),
                        generation,
                        message,
                    ));
                },
            );
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
        let Some(id) = self.key() else {
            self.load_inbox();
            return;
        };
        if self.view == View::Overview {
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
        if review.generation.is_some() {
            self.notice = "Cancel the current generation before refreshing its snapshot".into();
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
                self.notice = "Locate this repository's local clone before refreshing".into();
            }
        } else {
            self.notice =
                "This snapshot is current. Remote revisions are checked every 30 seconds.".into();
        }
    }
    pub fn load_models(&mut self) {
        self.modal = Some(Modal::Models {
            selected: 0,
            effort: 0,
            query: String::new(),
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
        let mut choices = vec![ModelChoice::default()];
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
            .any(|m| m.id == "gpt-5.6-luna" && m.efforts.iter().any(|e| e == "high"))
        {
            choices.retain(|c| *c != ModelChoice::default());
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
        self.config.model = choice;
        self.save_config();
        self.modal = None;
        self.notice = "Model saved. Open a PR or choose Regenerate to use it.".into();
        self.invalidate();
    }
    pub fn tick(&mut self) {
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
                    self.notice = "A background worker stopped unexpectedly".into();
                }
            } else {
                index += 1;
            }
        }
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
            Message::Inbox(id, output) => {
                if id != self.inbox_id {
                    return;
                }
                self.inbox_loading = false;
                match output {
                    Ok(inbox) => {
                        let previous = self.inbox.get(self.selected).map(|p| p.key.id());
                        self.inbox = inbox;
                        self.selected = previous
                            .and_then(|id| self.inbox.iter().position(|p| p.key.id() == id))
                            .unwrap_or(0);
                        self.inbox_error = None;
                        if self.inbox.len() == 1000 {
                            self.notice = "GitHub search returned its maximum 1,000 results".into();
                        }
                        self.select(self.selected);
                    }
                    Err(error) => {
                        self.notice =
                            format!("Could not refresh {}: {error}", self.inbox_tab.label());
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
                    Ok(pr) => {
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
            Message::Checks(id, output) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    r.polling = false;
                    match output {
                        Ok(items) => {
                            r.checks = items;
                            r.checks_error = None;
                        }
                        Err(e) => r.checks_error = Some(e),
                    }
                }
            }
            Message::Poll(id, snapshot_id, detail) => {
                if let Some(r) = self.reviews.get_mut(&id) {
                    r.revision_polling = false;
                    if r.snapshot_id != snapshot_id {
                        return;
                    }
                    match detail {
                        Ok(Some(pr)) => {
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
                        Err(e) => self.notice = format!("Revision refresh failed: {e}"),
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
                            self.scroll = 0;
                            if let Some(detail) = &r.detail {
                                self.config
                                    .repositories
                                    .insert(detail.key.repository(), root.clone());
                            }
                            r.root = Some(root);
                            r.snapshot = Some(Arc::new(snapshot));
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
            Message::Models(output) => {
                self.models_loading = false;
                match output {
                    Ok(models) => self.models = models,
                    Err(e) => self.models_error = Some(e),
                }
            }
            Message::Notice(message) => self.notice = message,
            Message::Repositories(output) => {
                self.repositories_loading = false;
                match output {
                    Ok(repositories) => self.repository_options = repositories,
                    Err(error) => self.repositories_error = Some(error),
                }
            }
        }
        self.invalidate();
    }
    pub fn action(&mut self, action: Action) {
        match action {
            Action::SelectPr(index) => self.select(index),
            Action::OpenPr => self.open(),
            Action::SetView(view) => {
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
                self.focus = if view == View::Overview {
                    Focus::Content
                } else {
                    Focus::Navigation
                };
                self.invalidate();
            }
            Action::Back => {
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
                match self.inbox_tab {
                    InboxTab::Authored => self.authored_state = state,
                    InboxTab::Repositories => self.repository_state = state,
                    InboxTab::ReviewRequests => return,
                }
                self.change_inbox(self.inbox_tab);
            }
            Action::Repositories(manage) => self.choose_repositories(manage),
            Action::ToggleRepository(name) => self.toggle_repository(name),
            Action::AllRepositories => {
                if let Some(Modal::Repositories { choices, .. }) = &mut self.modal {
                    for enabled in choices.values_mut() {
                        *enabled = true;
                    }
                }
            }
            Action::SaveRepositories => self.save_repositories(),
            Action::SelectFile(file) => {
                self.focus = Focus::Navigation;
                self.file = file;
                self.scroll = 0;
                self.invalidate();
            }
            Action::Jump(row) => {
                self.scroll = row;
                self.focus = Focus::Content;
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
                            .unwrap_or_default(),
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
            Action::Help => self.modal = Some(Modal::Help),
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
            let index = self
                .selected
                .saturating_add_signed(delta as isize)
                .min(self.inbox.len().saturating_sub(1));
            self.select(index);
        } else if self.focus == Focus::Navigation
            && self.view != View::Overview
            && (self.view == View::Diff || self.review().is_none_or(|r| r.guide.is_none()))
        {
            let count = self
                .review()
                .and_then(|r| r.snapshot.as_ref())
                .map(|s| s.files.len())
                .unwrap_or(0);
            self.file = self
                .file
                .saturating_add_signed(delta as isize)
                .min(count.saturating_sub(1));
            self.scroll = 0;
            self.invalidate();
        } else if self.focus == Focus::Navigation && self.view == View::Guide {
            if let Some(doc) = &self.document {
                let current = doc
                    .sections
                    .iter()
                    .rposition(|s| s.start <= self.scroll)
                    .unwrap_or(0);
                let index = current
                    .saturating_add_signed(delta as isize)
                    .min(doc.sections.len().saturating_sub(1));
                if let Some(section) = doc.sections.get(index) {
                    self.scroll = section.start;
                }
            }
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
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        match key.code {
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
                if self.home {
                    self.quit = true;
                } else {
                    self.action(Action::Back);
                }
            }
            KeyCode::Enter => {
                if self.home {
                    self.open();
                } else {
                    self.focus = Focus::Content;
                }
            }
            KeyCode::Up => self.move_scroll(-1),
            KeyCode::Down => self.move_scroll(1),
            KeyCode::PageUp => self.move_scroll(-(self.viewport as i32)),
            KeyCode::PageDown | KeyCode::Char(' ') => self.move_scroll(self.viewport as i32),
            KeyCode::Home => {
                self.scroll = 0;
                if self.focus == Focus::Navigation && self.home {
                    self.select(0);
                }
            }
            KeyCode::End => {
                self.move_scroll(i32::MAX);
            }
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
                Action::SetInbox(InboxTab::ReviewRequests)
            } else {
                Action::SetView(View::Overview)
            }),
            KeyCode::Char('2') => self.action(if self.home {
                Action::SetInbox(InboxTab::Authored)
            } else {
                Action::SetView(View::Guide)
            }),
            KeyCode::Char('3') => self.action(if self.home {
                Action::SetInbox(InboxTab::Repositories)
            } else {
                Action::SetView(View::Diff)
            }),
            KeyCode::F(1) => self.action(Action::Help),
            KeyCode::F(2) => self.load_models(),
            KeyCode::F(3) if self.home && self.inbox_tab != InboxTab::ReviewRequests => {
                let state = match self.state() {
                    PrState::Open => PrState::Merged,
                    PrState::Merged => PrState::Closed,
                    PrState::Closed => PrState::All,
                    PrState::All => PrState::Open,
                };
                self.action(Action::SetState(state));
            }
            KeyCode::F(4) if self.home && self.inbox_tab == InboxTab::Repositories => {
                self.choose_repositories(key.modifiers.contains(KeyModifiers::SHIFT));
            }
            KeyCode::F(5) => self.refresh(),
            KeyCode::F(6) => self.action(Action::Regenerate),
            KeyCode::F(7) => self.action(Action::Locate),
            KeyCode::F(8) => self.cancel(),
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
            Modal::Repositories {
                manage,
                mut query,
                mut selected,
                choices,
            } => {
                let options = self.repository_choices(manage, &query, &choices);
                match key.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        selected = selected
                            .saturating_add(1)
                            .min(options.len().saturating_sub(1))
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    KeyCode::Char(' ') => {
                        self.modal = Some(Modal::Repositories {
                            manage,
                            query,
                            selected,
                            choices,
                        });
                        if let Some(name) = options.get(selected) {
                            self.toggle_repository(name.clone());
                        }
                        return;
                    }
                    KeyCode::Enter => {
                        self.modal = Some(Modal::Repositories {
                            manage,
                            query,
                            selected,
                            choices,
                        });
                        self.save_repositories();
                        return;
                    }
                    KeyCode::Char('a')
                        if key.modifiers.contains(KeyModifiers::CONTROL) && !manage =>
                    {
                        self.modal = Some(Modal::Repositories {
                            manage,
                            query,
                            selected,
                            choices,
                        });
                        self.action(Action::AllRepositories);
                        return;
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(c);
                        selected = 0;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Repositories {
                    manage,
                    query,
                    selected,
                    choices,
                });
            }
            Modal::Help => {
                self.modal = Some(Modal::Help);
            }
            Modal::Clone { mut value, key: id } => {
                match key.code {
                    KeyCode::Enter => {
                        if !value.trim().is_empty() {
                            self.prepare(PathBuf::from(value.trim()));
                        }
                        return;
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear()
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.push(c)
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Clone { value, key: id });
            }
            Modal::Models {
                mut selected,
                effort,
                mut query,
            } => {
                let options = self.model_options(&query);
                match key.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(options.len().saturating_sub(1)),
                    KeyCode::Enter => {
                        if let Some(choice) = options.get(selected) {
                            self.apply_model(choice.clone());
                            return;
                        }
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(c);
                        selected = 0;
                    }
                    _ => {}
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
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, action)) = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains((event.column, event.row).into()))
                {
                    self.action(action.clone());
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if matches!(self.modal, Some(Modal::Repositories { .. })) =>
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
        match &mut self.modal {
            Some(Modal::Clone { value, .. }) => value.push_str(text.trim()),
            Some(Modal::Repositories {
                query, selected, ..
            }) => {
                query.push_str(text.trim());
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
