//! Interactive review state and explicit, asynchronous wizard actions.
use crate::{
    app::{App, Focus, Message, Modal, Notice},
    editor::Editor,
    model::{ModelChoice, ModelPurpose, PrKey},
    review::{self, Anchor, Operation, Side},
    storage, worktrees,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
};

#[derive(Clone, Debug)]
pub enum Target {
    Header {
        path: String,
        chapter: Option<usize>,
    },
    Code {
        path: String,
        old: Option<u64>,
        new: Option<u64>,
    },
}
impl Target {
    pub fn line(&self, side: Side) -> Option<Anchor> {
        match self {
            Self::Code { path, old, new } => {
                let line = match side {
                    Side::Left => old,
                    Side::Right => new,
                };
                line.map(|n| Anchor {
                    path: path.clone(),
                    side,
                    start: n,
                    end: n,
                })
            }
            _ => None,
        }
    }
}
#[derive(Default)]
pub struct State {
    pub cursor: Option<usize>,
    pub selection: Option<Anchor>,
    pub side: Side,
    pub nav: usize,
    pub drafts: HashMap<String, Compose>,
    pub mentions: HashMap<String, review::Mentions>,
    pub mentions_loading: BTreeSet<String>,
    pub busy: bool,
    pub conflict_cancel: Option<crate::process::Cancel>,
}
#[derive(Default)]
pub struct PrState {
    pub github: review::State,
    pub github_loading: bool,
    pub github_loaded: bool,
    pub progress: Progress,
    pub progress_key: String,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub completed: BTreeSet<(usize, String)>,
}
#[derive(Clone, Debug)]
pub enum Kind {
    Review,
    Comment(Anchor),
    Close,
}
#[derive(Clone, Debug)]
pub struct Compose {
    pub key: PrKey,
    pub head: String,
    pub kind: Kind,
    pub editor: Editor,
    pub choice: usize,
    pub focus: usize,
    pub mention: usize,
}
impl Compose {
    pub fn id(&self) -> String {
        match &self.kind {
            Kind::Comment(anchor) => format!("{}:{anchor:?}", self.key.id()),
            Kind::Review => format!("{}:review", self.key.id()),
            Kind::Close => format!("{}:close", self.key.id()),
        }
    }
    pub fn choices(&self) -> Vec<&'static str> {
        match self.kind {
            Kind::Review => vec!["Comment", "Approve", "Request changes"],
            Kind::Comment(_) => vec!["Standalone comment", "Add to pending review"],
            Kind::Close => vec!["Close PR"],
        }
    }
    pub fn operation(&self) -> Operation {
        let body = self.editor.text();
        match &self.kind {
            Kind::Review => Operation::Review {
                body,
                event: match self.choice {
                    1 => "APPROVE",
                    2 => "REQUEST_CHANGES",
                    _ => "COMMENT",
                }
                .into(),
            },
            Kind::Comment(anchor) => Operation::Comment {
                anchor: anchor.clone(),
                body,
                pending: self.choice == 1,
            },
            Kind::Close => Operation::Close { body },
        }
    }
}
#[derive(Clone, Debug)]
pub enum Wizard {
    Resolve {
        key: PrKey,
        head: String,
        model: ModelChoice,
    },
    Resolving {
        activity: String,
    },
    Home(usize),
    Controls {
        key: PrKey,
        head: String,
        selected: usize,
    },
    Compose(Compose),
    Confirm {
        key: PrKey,
        head: String,
        operation: Operation,
        draft: Option<Compose>,
    },
    Result {
        notice: Notice,
    },
    Trees {
        entries: Vec<worktrees::Entry>,
        selected: usize,
        loading: bool,
    },
    Delete {
        directories: Vec<PathBuf>,
    },
}
#[derive(Clone, Debug)]
pub enum WAction {
    Resolve,
    CancelResolution,
    Open,
    Controls,
    Choose(usize),
    Next,
    Back,
    Submit,
    RefreshMentions,
    Complete(String),
    Nav(usize),
    Cursor(usize, Side),
    Trees,
    DeleteOne,
    DeleteStale,
    ConfirmDelete,
    FocusEditor,
    FocusChoice,
}
pub enum Event {
    ResolutionProgress(String),
    Resolved(PrKey, Result<String, String>),
    Written(PrKey, Operation, Result<String, String>),
    Viewed(PrKey, String, Result<review::State, String>),
    Mentions(PrKey, Result<review::Mentions, String>),
    Trees(Result<Vec<worktrees::Entry>, String>),
    Deleted(Result<(), String>),
}
fn result<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{e:#}"))
}

impl App {
    pub fn wizard(&mut self, wizard: Wizard) {
        self.modal = Some(Modal::Workflow(Box::new(wizard)));
    }
    pub fn workflow_action(&mut self, action: WAction) {
        if self.workflow.busy && !matches!(action, WAction::CancelResolution) {
            return;
        }
        match action {
            WAction::CancelResolution => {
                if let Some(cancel) = &self.workflow.conflict_cancel {
                    cancel.cancel();
                }
            }
            WAction::Resolve => {
                let Some(Modal::Workflow(modal)) = &self.modal else {
                    return;
                };
                let Wizard::Resolve { key, head, model } = modal.as_ref() else {
                    return;
                };
                let (key, head, model) = (key.clone(), head.clone(), model.clone());
                let root = self
                    .reviews
                    .get(&key.id())
                    .and_then(|r| r.root.clone())
                    .or_else(|| self.config.repositories.get(&key.repository()).cloned())
                    .or_else(|| std::env::current_dir().ok());
                let Some(root) = root else {
                    self.notice =
                        Notice::error("Locate the PR's local clone before resolving conflicts");
                    return;
                };
                self.workflow.busy = true;
                self.wizard(Wizard::Resolving {
                    activity: "Preparing conflict resolution…".into(),
                });
                let cancel = self.spawn(move |tx, cancel| {
                    let progress_tx = tx.clone();
                    let progress = std::sync::Arc::new(move |activity| {
                        let _ = progress_tx
                            .send(Message::Workflow(Event::ResolutionProgress(activity)));
                    });
                    let output = result(crate::conflicts::resolve(
                        &root, &key, &head, &model, &cancel, progress,
                    ));
                    let _ = tx.send(Message::Workflow(Event::Resolved(key, output)));
                });
                self.workflow.conflict_cancel = Some(cancel);
            }
            WAction::Open => {
                if self.home {
                    self.wizard(Wizard::Home(0));
                } else {
                    self.workflow_action(WAction::Controls);
                }
            }
            WAction::Controls => {
                if let Some(pr) = self.review().and_then(|r| r.detail.clone()) {
                    self.wizard(Wizard::Controls {
                        key: pr.key.clone(),
                        head: pr.head.clone(),
                        selected: 0,
                    });
                    self.load_viewed();
                } else {
                    self.notice = Notice::info("Wait for PR details before opening its controls");
                }
            }
            WAction::Choose(index) => {
                let Some(Modal::Workflow(modal)) = self.modal.take() else {
                    return;
                };
                match *modal {
                    Wizard::Home(_) => match index {
                        0 => self.workflow_action(WAction::Controls),
                        1 => self.workflow_action(WAction::Trees),
                        2 => self.load_models_for(ModelPurpose::Guide),
                        3 => self.load_models_for(ModelPurpose::Conflicts),
                        _ => {}
                    },
                    Wizard::Controls { key, head, .. } => match index {
                        0 => self.compose(key, head, Kind::Review),
                        1..=4 => self.wizard(Wizard::Confirm {
                            key,
                            head,
                            operation: Operation::Merge {
                                squash: index == 2 || index == 4,
                                admin: index >= 3,
                            },
                            draft: None,
                        }),
                        5 => self.compose(key, head, Kind::Close),
                        6 => self.wizard(Wizard::Resolve {
                            key,
                            head,
                            model: self.config.conflict_model.clone(),
                        }),
                        _ => {}
                    },
                    Wizard::Compose(mut draft) => {
                        draft.choice = index.min(draft.choices().len().saturating_sub(1));
                        draft.focus = 1;
                        self.wizard(Wizard::Compose(draft));
                    }
                    Wizard::Trees {
                        entries, loading, ..
                    } => self.wizard(Wizard::Trees {
                        entries,
                        selected: index,
                        loading,
                    }),
                    other => self.wizard(other),
                }
            }
            WAction::Next => {
                if let Some(Modal::Workflow(modal)) = self.modal.take() {
                    if let Wizard::Compose(draft) = *modal {
                        self.workflow.drafts.insert(draft.id(), draft.clone());
                        self.wizard(Wizard::Confirm {
                            key: draft.key.clone(),
                            head: draft.head.clone(),
                            operation: draft.operation(),
                            draft: Some(draft),
                        });
                    } else {
                        self.modal = Some(Modal::Workflow(modal));
                    }
                }
            }
            WAction::Submit => {
                if let Some(Modal::Workflow(modal)) = &self.modal
                    && let Wizard::Confirm {
                        key,
                        head,
                        operation,
                        ..
                    } = modal.as_ref()
                {
                    let (key, head, operation) = (key.clone(), head.clone(), operation.clone());
                    self.workflow.busy = true;
                    self.spawn(move |tx, cancel| {
                        let output = result(review::execute(&key, &head, &operation, &cancel));
                        let _ = tx.send(Message::Workflow(Event::Written(key, operation, output)));
                    });
                }
            }
            WAction::Back => self.close_wizard(),
            WAction::RefreshMentions => {
                if let Some(Modal::Workflow(modal)) = &self.modal
                    && let Wizard::Compose(draft) = modal.as_ref()
                {
                    self.load_mentions(draft.key.clone(), true);
                }
            }
            WAction::Complete(login) => {
                if let Some(Modal::Workflow(modal)) = &mut self.modal
                    && let Wizard::Compose(draft) = modal.as_mut()
                {
                    draft.editor.complete(&login);
                    draft.mention = 0;
                }
            }
            WAction::FocusEditor | WAction::FocusChoice => {
                if let Some(Modal::Workflow(modal)) = &mut self.modal
                    && let Wizard::Compose(draft) = modal.as_mut()
                {
                    draft.focus = usize::from(matches!(action, WAction::FocusChoice));
                }
            }
            WAction::Nav(index) => {
                self.workflow.nav = index;
                self.focus = Focus::Navigation;
                self.workflow.selection = None;
                if let Some(item) = self.document.as_ref().and_then(|d| d.navigation.get(index)) {
                    self.scroll = item.row;
                    self.workflow.cursor = Some(item.row);
                }
            }
            WAction::Cursor(row, side) => {
                self.focus = Focus::Content;
                self.workflow.cursor = Some(row);
                self.workflow.side = side;
                self.workflow.selection = None;
            }
            WAction::Trees => {
                self.wizard(Wizard::Trees {
                    entries: Vec::new(),
                    selected: 0,
                    loading: true,
                });
                self.spawn(|tx, cancel| {
                    let _ = tx.send(Message::Workflow(Event::Trees(result(worktrees::list(
                        &cancel,
                    )))));
                });
            }
            WAction::DeleteOne | WAction::DeleteStale => {
                if let Some(Modal::Workflow(modal)) = &self.modal
                    && let Wizard::Trees {
                        entries,
                        selected,
                        loading: false,
                    } = modal.as_ref()
                {
                    let paths = entries
                        .iter()
                        .enumerate()
                        .filter(|(i, e)| {
                            e.reason.is_none()
                                && (matches!(action, WAction::DeleteStale) || *i == *selected)
                        })
                        .map(|(_, e)| e.directory.clone())
                        .collect::<Vec<_>>();
                    if !paths.is_empty() {
                        self.wizard(Wizard::Delete { directories: paths });
                    } else {
                        self.notice = Notice::info("No eligible inactive worktrees selected");
                    }
                }
            }
            WAction::ConfirmDelete => {
                if let Some(Modal::Workflow(modal)) = &self.modal
                    && let Wizard::Delete { directories } = modal.as_ref()
                {
                    let paths = directories.clone();
                    self.workflow.busy = true;
                    self.spawn(move |tx, cancel| {
                        let output = (|| -> anyhow::Result<()> {
                            for path in paths {
                                worktrees::delete(&path, &cancel)?;
                            }
                            Ok(())
                        })();
                        let _ = tx.send(Message::Workflow(Event::Deleted(result(output))));
                    });
                }
            }
        }
    }
    pub fn close_wizard(&mut self) {
        if self.workflow.busy {
            self.notice = Notice::info("Waiting for the active operation to finish…");
            return;
        }
        if let Some(Modal::Workflow(modal)) = self.modal.take() {
            match *modal {
                Wizard::Compose(draft) => {
                    self.workflow.drafts.insert(draft.id(), draft);
                }
                Wizard::Confirm {
                    draft: Some(draft), ..
                } => self.wizard(Wizard::Compose(draft)),
                _ => {}
            }
        }
    }
    fn compose(&mut self, key: PrKey, head: String, kind: Kind) {
        let mut draft = Compose {
            key: key.clone(),
            head,
            kind,
            editor: Editor::default(),
            choice: 0,
            focus: 0,
            mention: 0,
        };
        if let Some(saved) = self.workflow.drafts.get(&draft.id()) {
            draft.editor = saved.editor.clone();
            draft.choice = saved.choice;
        }
        self.wizard(Wizard::Compose(draft));
        self.load_mentions(key, false);
    }
    pub fn mention_options(&self, draft: &Compose) -> Vec<String> {
        let Some((_, query)) = draft.editor.mention() else {
            return Vec::new();
        };
        self.workflow
            .mentions
            .get(&draft.key.id())
            .map(|m| {
                m.users
                    .iter()
                    .filter(|login| login.to_lowercase().starts_with(&query))
                    .take(8)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    fn load_mentions(&mut self, key: PrKey, force: bool) {
        let id = key.id();
        if self.workflow.mentions_loading.contains(&id) {
            return;
        }
        if !self.workflow.mentions.contains_key(&id) {
            match review::cached_mentions(&self.storage, &key) {
                Ok(cached) => {
                    self.workflow.mentions.insert(id.clone(), cached);
                }
                Err(e) => {
                    self.notice = Notice::error(format!("Could not read mention cache: {e:#}"))
                }
            }
        }
        if !force
            && self
                .workflow
                .mentions
                .get(&id)
                .is_some_and(|m| chrono::Utc::now().timestamp() - m.fetched < 86400)
        {
            return;
        }
        self.workflow.mentions_loading.insert(id);
        let storage = self.storage.clone();
        self.spawn(move |tx, cancel| {
            let output = review::mentions(&key, &cancel).and_then(|m| {
                review::save_mentions(&storage, &key, &m)?;
                Ok(m)
            });
            let _ = tx.send(Message::Workflow(Event::Mentions(key, result(output))));
        });
    }
    pub fn load_viewed(&mut self) {
        let Some(id) = self.key() else {
            return;
        };
        let Some(r) = self.reviews.get_mut(&id) else {
            return;
        };
        if r.interaction.github_loading || r.interaction.github_loaded {
            return;
        }
        let Some(pr) = r.detail.clone() else {
            return;
        };
        r.interaction.github_loading = true;
        self.spawn(move |tx, cancel| {
            let _ = tx.send(Message::Workflow(Event::Viewed(
                pr.key.clone(),
                pr.head.clone(),
                result(review::state(&pr.key, &cancel)),
            )));
        });
    }
    pub fn workflow_receive(&mut self, event: Event) {
        match event {
            Event::ResolutionProgress(activity) => {
                if let Some(Modal::Workflow(modal)) = &mut self.modal
                    && let Wizard::Resolving { activity: current } = modal.as_mut()
                {
                    *current = activity;
                }
            }
            Event::Resolved(key, output) => {
                self.workflow.busy = false;
                self.workflow.conflict_cancel = None;
                let notice = match output {
                    Ok(message) => Notice::success(message),
                    Err(error) => Notice::error(error),
                };
                self.notice = notice.clone();
                self.wizard(Wizard::Result { notice });
                self.spawn(move |tx, cancel| {
                    let _ = tx.send(Message::Detail(
                        key.id(),
                        result(crate::github::detail(&key, &cancel)),
                    ));
                    let _ = tx.send(Message::Checks(
                        key.id(),
                        result(crate::github::checks(&key, &cancel)),
                    ));
                });
            }
            Event::Mentions(key, output) => {
                self.workflow.mentions_loading.remove(&key.id());
                match output {
                    Ok(m) => {
                        self.workflow.mentions.insert(key.id(), m);
                    }
                    Err(e) => self.notice = Notice::error(format!("Mentions refresh failed: {e}")),
                }
            }
            Event::Viewed(key, head, output) => {
                if let Some(r) = self.reviews.get_mut(&key.id()) {
                    r.interaction.github_loading = false;
                    match output {
                        Ok(state)
                            if r.detail.as_ref().is_some_and(|p| p.head == head)
                                && state.head == head =>
                        {
                            r.interaction.github = state;
                            r.interaction.github_loaded = true;
                        }
                        Ok(_) => {
                            self.notice =
                                Notice::info("PR changed while loading Viewed state; refresh it");
                        }
                        Err(e) => self.notice = Notice::error(e),
                    }
                }
            }
            Event::Written(key, operation, output) => {
                self.workflow.busy = false;
                match output {
                    Ok(message) => {
                        if let Some(Modal::Workflow(modal)) = &self.modal
                            && let Wizard::Confirm {
                                draft: Some(draft), ..
                            } = modal.as_ref()
                        {
                            self.workflow.drafts.remove(&draft.id());
                        }
                        if let Some(r) = self.reviews.get_mut(&key.id()) {
                            r.interaction.github_loaded = false;
                            if let Operation::Viewed { path, viewed } = &operation {
                                if *viewed {
                                    r.interaction.github.viewed.insert(path.clone());
                                } else {
                                    r.interaction.github.viewed.remove(path);
                                }
                            }
                        }
                        if !matches!(operation, Operation::Viewed { .. }) {
                            self.wizard(Wizard::Result {
                                notice: Notice::success(message.clone()),
                            });
                        }
                        self.notice = Notice::success(message);
                        let id = key.id();
                        self.spawn(move |tx, cancel| {
                            let _ = tx.send(Message::Detail(
                                id.clone(),
                                result(crate::github::detail(&key, &cancel)),
                            ));
                            let _ = tx.send(Message::Timeline(
                                id,
                                result(crate::github::timeline(&key, &cancel)),
                            ));
                        });
                    }
                    Err(error) => {
                        // Preserve the composer and require an explicit new confirmation. No automatic retries.
                        if let Some(Modal::Workflow(modal)) = self.modal.take() {
                            match *modal {
                                Wizard::Confirm {
                                    draft: Some(draft), ..
                                } => self.wizard(Wizard::Compose(draft)),
                                other => self.wizard(other),
                            }
                        }
                        self.notice = Notice::error(format!(
                            "{error} · Check GitHub before retrying if the result is uncertain."
                        ));
                    }
                }
            }
            Event::Trees(output) => match output {
                Ok(entries) => {
                    if matches!(&self.modal,Some(Modal::Workflow(m)) if matches!(m.as_ref(),Wizard::Trees{..}))
                    {
                        self.wizard(Wizard::Trees {
                            entries,
                            selected: 0,
                            loading: false,
                        });
                    }
                }
                Err(error) => self.wizard(Wizard::Result {
                    notice: Notice::error(error),
                }),
            },
            Event::Deleted(output) => {
                self.workflow.busy = false;
                match output {
                    Ok(()) => self.workflow_action(WAction::Trees),
                    Err(error) => self.wizard(Wizard::Result {
                        notice: Notice::error(error),
                    }),
                }
            }
        }
        self.invalidate();
    }
    pub fn workflow_key(&mut self, key: KeyEvent) {
        if self.workflow.busy {
            if (key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('x') && key.modifiers.is_empty()))
                && self.workflow.conflict_cancel.is_some()
            {
                self.workflow_action(WAction::CancelResolution);
            }
            return;
        }
        if key.code == KeyCode::Esc {
            self.close_wizard();
            return;
        }
        let Some(Modal::Workflow(modal)) = self.modal.take() else {
            return;
        };
        let mut wizard = *modal;
        let mut action = None;
        match &mut wizard {
            Wizard::Resolve { .. } => {
                if key.code == KeyCode::Enter {
                    action = Some(WAction::Resolve);
                }
            }
            Wizard::Resolving { .. } => {}
            Wizard::Home(_) | Wizard::Controls { .. } => {
                let count = if self.home && matches!(wizard, Wizard::Home(_)) {
                    4
                } else {
                    7
                };
                // Restore below; choosing a menu entry goes through the same mouse action path.
                let selected = match &mut wizard {
                    Wizard::Home(i) | Wizard::Controls { selected: i, .. } => i,
                    _ => return,
                };
                match key.code {
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => *selected = (*selected + 1).min(count - 1),
                    KeyCode::Enter => action = Some(WAction::Choose(*selected)),
                    _ => {}
                }
            }
            Wizard::Compose(draft) => {
                let options = self.mention_options(draft);
                if key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
                    action = Some(WAction::RefreshMentions);
                } else if key.code == KeyCode::Enter
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    action = Some(WAction::Next);
                } else if draft.focus == 0
                    && !options.is_empty()
                    && matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Tab)
                {
                    match key.code {
                        KeyCode::Up => draft.mention = draft.mention.saturating_sub(1),
                        KeyCode::Down => draft.mention = (draft.mention + 1).min(options.len() - 1),
                        KeyCode::Tab => {
                            if let Some(login) = options.get(draft.mention) {
                                draft.editor.complete(login);
                                draft.mention = 0;
                            }
                        }
                        _ => {}
                    }
                } else if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    draft.focus = (draft.focus + 1) % 3;
                } else if draft.focus == 0 {
                    draft.editor.key(key);
                    draft.mention = 0;
                } else if draft.focus == 1 {
                    match key.code {
                        KeyCode::Left | KeyCode::Up => {
                            draft.choice = draft.choice.saturating_sub(1)
                        }
                        KeyCode::Right | KeyCode::Down => {
                            draft.choice = (draft.choice + 1).min(draft.choices().len() - 1)
                        }
                        KeyCode::Enter => draft.focus = 2,
                        _ => {}
                    }
                } else if key.code == KeyCode::Enter {
                    action = Some(WAction::Next);
                }
                self.workflow.drafts.insert(draft.id(), draft.clone());
            }
            Wizard::Confirm { .. } => {
                if key.code == KeyCode::Enter {
                    action = Some(WAction::Submit);
                }
            }
            Wizard::Result { .. } => {
                if key.code == KeyCode::Enter {
                    action = Some(WAction::Back);
                }
            }
            Wizard::Trees {
                entries, selected, ..
            } => match key.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(entries.len().saturating_sub(1)),
                KeyCode::Delete | KeyCode::Enter => action = Some(WAction::DeleteOne),
                KeyCode::Char('a') => action = Some(WAction::DeleteStale),
                KeyCode::Char('r') if key.modifiers.is_empty() => action = Some(WAction::Trees),
                _ => {}
            },
            Wizard::Delete { .. } => {
                if key.code == KeyCode::Enter {
                    action = Some(WAction::ConfirmDelete);
                }
            }
        }
        self.wizard(wizard);
        if let Some(action) = action {
            self.workflow_action(action);
        }
    }
    pub fn sync_progress(&mut self, id: &str) {
        let Some(r) = self.reviews.get_mut(id) else {
            return;
        };
        let (Some(guide), Some(snapshot)) = (&r.guide, &r.snapshot) else {
            return;
        };
        let encoded = serde_json::to_vec(&(id, guide, snapshot));
        match encoded {
            Ok(bytes) => {
                let key = storage::hash(bytes);
                if key != r.interaction.progress_key {
                    r.interaction.progress_key = key.clone();
                    r.interaction.progress = Progress::default();
                    let path = self.storage.cache.join(format!("progress-{key}.json"));
                    if path.exists() {
                        match std::fs::read(path)
                            .map_err(anyhow::Error::from)
                            .and_then(|bytes| Ok(serde_json::from_slice(&bytes)?))
                        {
                            Ok(progress) => r.interaction.progress = progress,
                            Err(e) => {
                                self.notice = Notice::error(format!(
                                    "Could not restore chapter progress: {e:#}"
                                ))
                            }
                        }
                    }
                }
            }
            Err(e) => {
                self.notice = Notice::error(format!("Could not identify chapter progress: {e}"))
            }
        }
    }
    pub fn enter_diff(&mut self) {
        if self.workflow.busy {
            return;
        }
        if self.focus == Focus::Navigation {
            self.focus = Focus::Content;
            self.workflow.cursor = Some(self.scroll);
            return;
        }
        let target = self
            .document
            .as_ref()
            .and_then(|d| d.rows.get(self.workflow.cursor.unwrap_or(self.scroll)))
            .and_then(|row| row.right.target.clone());
        match target {
            Some(Target::Header {
                path,
                chapter: Some(chapter),
            }) => {
                let Some(id) = self.key() else {
                    return;
                };
                self.sync_progress(&id);
                if let Some(r) = self.reviews.get_mut(&id) {
                    let item = (chapter, path);
                    if !r.interaction.progress.completed.remove(&item) {
                        r.interaction.progress.completed.insert(item);
                    }
                    if let Err(e) = storage::atomic_json(
                        &self
                            .storage
                            .cache
                            .join(format!("progress-{}.json", r.interaction.progress_key)),
                        &r.interaction.progress,
                    ) {
                        self.notice =
                            Notice::error(format!("Could not save chapter progress: {e:#}"));
                    }
                }
                self.invalidate();
            }
            Some(Target::Header {
                path,
                chapter: None,
            }) => {
                let Some(r) = self.review() else {
                    return;
                };
                let Some(pr) = r.detail.clone() else {
                    return;
                };
                if !r.interaction.github_loaded {
                    self.load_viewed();
                    self.notice =
                        Notice::info("Loading GitHub Viewed state; press Enter again when ready");
                    return;
                }
                let operation = Operation::Viewed {
                    viewed: !r.interaction.github.viewed.contains(&path),
                    path,
                };
                self.workflow.busy = true;
                self.spawn(move |tx, cancel| {
                    let output = result(review::execute(&pr.key, &pr.head, &operation, &cancel));
                    let _ = tx.send(Message::Workflow(Event::Written(
                        pr.key.clone(),
                        operation,
                        output,
                    )));
                });
            }
            Some(target) => {
                let Some(mut anchor) = target.line(self.workflow.side) else {
                    self.notice = Notice::info("Choose a side with a code line using Left/Right");
                    return;
                };
                if let Some(start) = &self.workflow.selection
                    && start.path == anchor.path
                    && start.side == anchor.side
                {
                    anchor.start = start.start.min(anchor.end);
                    anchor.end = start.start.max(anchor.end);
                }
                if let Some(pr) = self.review().and_then(|r| r.detail.clone()) {
                    self.compose(pr.key.clone(), pr.head.clone(), Kind::Comment(anchor));
                }
            }
            None => {
                if let Some(action) = self
                    .document
                    .as_ref()
                    .and_then(|d| d.rows.get(self.workflow.cursor.unwrap_or(self.scroll)))
                    .and_then(|r| r.right.action.clone())
                {
                    self.action(action);
                }
            }
        }
    }
    pub fn move_diff(&mut self, delta: i32, select: bool) {
        let Some(doc) = &self.document else {
            return;
        };
        let current = self
            .workflow
            .cursor
            .unwrap_or(self.scroll)
            .min(doc.rows.len().saturating_sub(1));
        let mut target =
            (current.saturating_add_signed(delta as isize)).min(doc.rows.len().saturating_sub(1));
        if select {
            let anchor = self.workflow.selection.clone().or_else(|| {
                doc.rows
                    .get(current)?
                    .right
                    .target
                    .as_ref()?
                    .line(self.workflow.side)
            });
            let Some(anchor) = anchor else {
                return;
            };
            while target != current {
                let row = doc.rows.get(target);
                if let Some(Target::Header { path, .. }) = row.and_then(|r| r.right.target.as_ref())
                    && path != &anchor.path
                {
                    return;
                }
                if row
                    .and_then(|r| r.right.target.as_ref())
                    .and_then(|t| t.line(self.workflow.side))
                    .is_some()
                {
                    break;
                }
                let next = target
                    .saturating_add_signed(delta.signum() as isize)
                    .min(doc.rows.len().saturating_sub(1));
                if next == target {
                    return;
                }
                target = next;
            }
            let target_line = doc
                .rows
                .get(target)
                .and_then(|r| r.right.target.as_ref())
                .and_then(|t| t.line(self.workflow.side));
            if !target_line.is_some_and(|line| line.path == anchor.path && line.side == anchor.side)
            {
                return;
            }
            self.workflow.selection = Some(anchor);
        } else {
            self.workflow.selection = None;
        }
        self.workflow.cursor = Some(target);
        let sticky = doc
            .files
            .iter()
            .find(|f| target > f.start && target < f.end)
            .map_or(0, |f| f.header.len());
        let height = self.viewport.max(1);
        // Track the cursor around the middle immediately, instead of waiting for
        // it to reach a viewport edge. Keep wrapped sticky headings above it.
        let offset = (height / 2).max(sticky.min(height.saturating_sub(1)));
        self.scroll = target
            .saturating_sub(offset)
            .min(doc.rows.len().saturating_sub(height));
    }
}
