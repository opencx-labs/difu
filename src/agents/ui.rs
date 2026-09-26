//! Native session UI. Network/process work is performed off the terminal thread.
use super::*;
mod browser;
mod changes;
mod commands;
mod defaults;
mod inline_questions;
mod media;
mod models;
mod panels;
mod patch;
mod prompt;
mod prs;
mod questions;
mod selection;
mod sidebar;
mod transcript;
mod transcript_window;
mod usage;
mod voice;
use crate::{
    editor::Editor,
    storage::{Config, Storage},
    ui::{ACCENT, BG, BORDER, DIM, GREEN, PANEL, RED, TEXT},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Conversation,
    Composer,
    Changes,
    ChangeTree,
    Resources(bool),
    PullRequest(usize),
}
pub(super) struct PendingSend {
    text: String,
    queued: bool,
    observed_before: usize,
}
impl PendingSend {
    fn in_chat(&self, session: &Session) -> bool {
        !self.queued && !session.tool_running()
    }
    fn matching(session: &Session, text: &str) -> usize {
        session
            .entries
            .iter()
            .filter(|e| {
                e.text == text
                    && matches!(
                        e.kind.as_str(),
                        "sending"
                            | "userMessage"
                            | "awaiting connection"
                            | "unsent"
                            | "unsent or unacknowledged"
                    )
            })
            .count()
            + session.queue.iter().filter(|p| p.text() == text).count()
    }
    fn observed(&self, session: &Session) -> bool {
        Self::matching(session, &self.text) > self.observed_before
    }
}
#[derive(Default)]
pub struct Position {
    pub conversation: usize,
    pub follow: bool,
    pub changes: usize,
    pub change_file: usize,
    pub change_tree: usize,
    pub change_directory: Option<String>,
    pub change_tree_horizontal: usize,
    pub horizontal: usize,
    pub selection: Option<usize>,
    pub draft: Editor,
    outgoing: Vec<PendingSend>,
    history: Option<prompt::History>,
    pub skills: Vec<super::Skill>,
    pub attachments: Vec<super::media::Attachment>,
    pub image_count: usize,
    pub video_count: usize,
    media_loaded: bool,
    saved_attachments: Vec<super::media::Attachment>,
    pub expanded: HashSet<String>,
    pub focused_entry: Option<String>,
    pub scroll_anchor: Option<(String, usize)>,
    transcript_viewport: Option<(u16, u16, u64)>,
    keep_transcript_position: bool,
    pub answers: HashMap<String, questions::Draft>,
    question_choices: HashMap<(String, usize), usize>,
    question_notes: HashMap<(String, usize, usize), Editor>,
}
#[derive(Clone)]
enum Action {
    Select(String),
    Open,
    Focus(Focus),
    Menu,
    Resources(bool),
    PullRequest(usize),
    Resource(bool, usize),
    RefreshArtifact,
    ExternalArtifact,
    BrowserChoice(usize),
    New,
    ToggleList,
    ToggleChanges,
    ChangeFile(usize),
    Approval(usize),
    MenuItem(usize),
    Command(usize),
    ToggleEntry(String),
    Link(String),
    Pending,
    Queue,
    EditQueued(usize),
    SaveQueued(bool),
    QuestionTab(usize),
    VoiceField,
    VoiceSave,
    VoiceToggle,
    AnswerFocus,
    QueueEditorFocus,
    ChooseRepository,
    ChangeRepository,
    DefaultField(usize),
    DefaultOption(usize),
    ModelField(usize),
    ModelOption(usize),
    DefaultSave,
    Approve(usize),
    Answer(usize, String),
    InlineQuestion(usize),
    QuestionChoice(usize),
    QuestionNote(usize),
    SubmitQuestion(bool),
    DismissNotice,
}
pub enum Modal {
    Transcript {
        entry: String,
        scroll: usize,
        rows: Vec<Line<'static>>,
        links: Vec<transcript::Link>,
        layout: Option<(u16, u64)>,
        height: usize,
    },
    InstallBrowser {
        path: std::path::PathBuf,
        title: String,
        selected: usize,
    },
    Resources {
        artifacts: bool,
        selected: usize,
    },
    AgentDefaults(Box<defaults::Settings>),
    Voice {
        key: Editor,
        field: usize,
    },
    Repository(Editor),
    ChangeRepository(Editor),
    Commands {
        query: Editor,
        selected: usize,
        skills_only: bool,
        files_only: bool,
    },
    Status,
    Usage {
        id: String,
        provider: provider::Provider,
        data: Option<Value>,
        error: Option<String>,
        scroll: usize,
        maximum: usize,
    },
    Pending {
        selected: usize,
    },
    Queue {
        selected: usize,
    },
    QueuedEdit {
        index: usize,
        expected: Prompt,
        editor: Editor,
        focus: usize,
    },
    Menu {
        query: Editor,
        selected: usize,
    },
    Rename(Editor),
    Model {
        model: Editor,
        effort: Editor,
        field: usize,
    },
    Approval {
        pending: Pending,
        selected: usize,
        answers: Vec<Editor>,
        field: usize,
        scroll: usize,
    },
    Cleanup,
    Delete,
    Help(crate::help::State),
}
struct ResultMessage {
    kind: Task,
    result: std::result::Result<Reply, String>,
}
#[derive(Clone)]
enum Task {
    List,
    Read(String),
    Changes(String, u64),
    Statistics(String, u64),
    Shells(String),
    OpenArtifact,
    OpenLink,
    InstallBrowser(String, std::path::PathBuf, String),
    WorkspacePaths(String, std::path::PathBuf),
    Defaults(String),
    Skills(String, std::path::PathBuf, provider::Provider),
    Launch,
    Repository(String),
    Worktree(String),
    Usage(String),
    Action,
    Interrupt(String),
    Question,
    Delete(String),
    Send(String, String, Vec<super::media::Attachment>),
}

pub struct Ui {
    panels: panels::Panels,
    prs: prs::State,
    pub defaults: crate::storage::AgentDefaults,
    pub selected: Option<String>,
    pub summaries: Vec<Summary>,
    pub sessions: HashMap<String, Session>,
    pub positions: HashMap<String, Position>,
    pub changes: HashMap<String, changes::Document>,
    pub focus: Focus,
    pub drilled: bool,
    pub list_visible: bool,
    pub changes_visible: bool,
    pub pinned_sessions: std::collections::BTreeSet<String>,
    pub archived: bool,
    pub modal: Option<Modal>,
    pub notice: Option<(String, bool)>,
    pub clipboard: Option<String>,
    pub review_requested: bool,
    storage: Storage,
    sender: mpsc::Sender<ResultMessage>,
    receiver: mpsc::Receiver<ResultMessage>,
    refreshed: Option<Instant>,
    changes_at: Option<Instant>,
    listing: bool,
    reading: bool,
    changing: bool,
    pub busy: bool,
    interrupting: HashSet<String>,
    question_send: Option<(String, Value, String, usize)>,
    question_reveal: bool,
    question_note_focused: bool,
    hits: Vec<(Rect, Action)>,
    pub viewport: usize,
    list_scroll: usize,
    conversation_lines: usize,
    conversation_sections: Vec<transcript::Section>,
    conversation_height: usize,
    transcript_window: transcript_window::Window,
    toast_started: Option<(String, bool, Instant)>,
    text_selection: selection::Selection,
    change_lines: usize,
    change_width: u16,
    change_wrap: bool,
    change_rows: std::sync::Arc<Vec<patch::SourceRow>>,
    skills: HashMap<String, (Vec<Skill>, Vec<String>)>,
    skills_loading: Option<String>,
    workspace_paths: HashMap<String, Vec<String>>,
    paths_loading: HashSet<String>,
    sidebar: sidebar::State,
    voice: voice::State,
    model_completion: models::State,
    toast_rect: Rect,
    media_pending: Option<(String, mpsc::Receiver<Result<super::media::Paste, String>>)>,
}
impl Ui {
    pub fn new(storage: Storage, config: &Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            panels: panels::Panels::new(config.agent_panel_right),
            prs: prs::State::load(&storage),
            defaults: config.agent_defaults.clone(),
            selected: None,
            summaries: Vec::new(),
            sessions: HashMap::new(),
            positions: HashMap::new(),
            changes: HashMap::new(),
            focus: Focus::List,
            drilled: false,
            list_visible: config.agent_list_visible,
            changes_visible: config.agent_changes_visible,
            pinned_sessions: config.pinned_sessions.clone(),
            archived: false,
            modal: None,
            notice: None,
            clipboard: None,
            review_requested: false,
            sidebar: sidebar::State::load(&storage),
            storage,
            sender,
            receiver,
            refreshed: None,
            changes_at: None,
            listing: false,
            reading: false,
            changing: false,
            busy: false,
            interrupting: HashSet::new(),
            question_send: None,
            question_reveal: true,
            question_note_focused: false,
            hits: Vec::new(),
            viewport: 20,
            list_scroll: 0,
            conversation_lines: 0,
            conversation_sections: Vec::new(),
            conversation_height: 0,
            transcript_window: Default::default(),
            toast_started: None,
            text_selection: selection::Selection::default(),
            change_lines: 0,
            change_width: 0,
            change_wrap: config.wrap_diff,
            change_rows: Default::default(),
            skills: HashMap::new(),
            skills_loading: None,
            workspace_paths: HashMap::new(),
            paths_loading: HashSet::new(),
            voice: voice::State::new(config.voice_enabled),
            model_completion: models::State::default(),
            toast_rect: Rect::default(),
            media_pending: None,
        }
    }
    fn task(&mut self, kind: Task, request: Request, connect: bool) {
        let storage = self.storage.clone();
        let tx = self.sender.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<Reply> {
                if connect {
                    client::ensure_running(&storage)?;
                }
                client::request(&storage, request)
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ResultMessage { kind, result });
        });
    }
    pub fn tick(&mut self, visible: bool) {
        self.tick_selection_click();
        self.tick_panels(visible);
        self.tick_prs(visible);
        self.tick_models();
        self.tick_defaults();
        self.tick_media();
        self.tick_voice(visible);
        while let Ok(message) = self.receiver.try_recv() {
            match &message.kind {
                Task::Shells(_) => {
                    self.panels.loading = false;
                    self.panels.checked = Some(Instant::now());
                }
                Task::List => {
                    self.listing = false;
                    self.refreshed = Some(Instant::now());
                }
                Task::Read(_) => self.reading = false,
                Task::Changes(..) => {
                    self.changing = false;
                    self.changes_at = Some(Instant::now());
                }
                Task::Launch
                | Task::Repository(_)
                | Task::Worktree(_)
                | Task::Action
                | Task::Delete(_)
                | Task::Send(..) => self.busy = false,
                Task::Interrupt(id) => {
                    self.interrupting.remove(id);
                }
                Task::Defaults(_)
                | Task::Question
                | Task::OpenArtifact
                | Task::OpenLink
                | Task::InstallBrowser(..)
                | Task::Usage(_) => {}
                Task::WorkspacePaths(id, _) => {
                    self.paths_loading.remove(id);
                }
                Task::Statistics(id, _) => {
                    self.sidebar.finished(id);
                }
                Task::Skills(..) => self.skills_loading = None,
            }
            if let Task::Usage(id) = &message.kind {
                if let Some(Modal::Usage {
                    id: shown,
                    data,
                    error,
                    ..
                }) = &mut self.modal
                    && shown == id
                {
                    match message.result {
                        Ok(Reply::Usage(value)) => *data = Some(value),
                        Err(message) => {
                            *error = Some(format!("Could not read provider usage: {message}"))
                        }
                        _ => *error = Some("Provider returned an unexpected usage response".into()),
                    }
                }
                continue;
            }
            if let Task::Shells(id) = &message.kind {
                match &message.result {
                    Ok(Reply::Shells(shells)) => {
                        self.panels.shells.insert(id.clone(), shells.clone());
                        self.panels.error = None;
                    }
                    Err(e) => self.panels.error = Some(e.clone()),
                    _ => {}
                }
                continue;
            }
            if let Task::InstallBrowser(id, path, title) = &message.kind {
                self.panels.installing = false;
                match &message.result {
                    Ok(_) => {
                        self.notice = Some(("terminal-browser installed".into(), false));
                        if self.selected.as_ref() == Some(id) && self.panels.view.as_ref().is_some_and(|v| matches!(v, panels::View::Artifact { path: current, .. } if current == path)) {
                            self.open_artifact(path.clone(), title.clone());
                        }
                    }
                    Err(error) => {
                        self.notice = Some((error.clone(), true));
                        if self.selected.as_ref() == Some(id)
                            && let Some(panels::View::Artifact {
                                path: current,
                                error: shown,
                                ..
                            }) = &mut self.panels.view
                            && current == path
                        {
                            *shown = Some(error.clone());
                        }
                    }
                }
                continue;
            }
            match message.result {
                Err(error) => {
                    if let Task::Send(id, text, ..) = &message.kind
                        && let Some(position) = self.positions.get_mut(id)
                        && let Some(index) = position.outgoing.iter().rposition(|p| &p.text == text)
                    {
                        position.outgoing.remove(index);
                    }
                    if matches!(message.kind, Task::Question) {
                        self.busy = false;
                        self.question_send = None;
                    }
                    self.notice = Some((error, true));
                }
                Ok(reply) => match (message.kind, reply) {
                    (Task::WorkspacePaths(id, root), Reply::WorkspacePaths(paths)) => {
                        if self
                            .sessions
                            .get(&id)
                            .is_some_and(|s| s.workspace.as_ref().unwrap_or(s.job.root()) == &root)
                        {
                            self.workspace_paths.insert(id, paths);
                        }
                    }
                    (Task::Statistics(id, revision), Reply::Statistics(stats)) => {
                        if self
                            .summaries
                            .iter()
                            .any(|s| s.id == id && s.workspace_revision != revision)
                        {
                            continue;
                        }
                        self.sidebar.counts.insert(id, stats);
                        if let Err(error) = self.sidebar.save(&self.storage) {
                            self.notice =
                                Some((format!("Cannot cache session statistics: {error:#}"), true));
                        }
                    }
                    (Task::Skills(id, root, provider), Reply::Skills { skills, errors }) => {
                        if self.sessions.get(&id).is_some_and(|s| {
                            s.workspace.as_ref().unwrap_or(s.job.root()) == &root
                                && s.provider == provider
                        }) {
                            self.skills.insert(id, (skills, errors));
                        }
                    }
                    (Task::List, Reply::Sessions(sessions)) => {
                        self.sidebar.observe(&sessions);
                        self.summaries = sessions;
                        self.ensure_selected();
                    }
                    (Task::Repository(id), Reply::Session(session)) => {
                        self.skills.remove(&id);
                        self.workspace_paths.remove(&id);
                        self.changes.remove(&id);
                        self.sidebar = sidebar::State::load(&self.storage);
                        self.sidebar.counts.remove(&id);
                        if let Err(error) = self.prs.forget(&id, &self.storage) {
                            self.notice =
                                Some((format!("Cannot clear cached PR: {error:#}"), true));
                        }
                        self.panels = panels::Panels::new(self.panels.right);
                        self.summaries.retain(|s| s.id != id);
                        self.summaries.push(session.summary());
                        self.sessions.insert(id, *session);
                        self.modal = None;
                        self.refreshed = None;
                    }
                    (Task::Read(id) | Task::Worktree(id), Reply::Session(session)) => {
                        if self
                            .sessions
                            .get(&id)
                            .is_some_and(|current| current.version > session.version)
                        {
                            continue;
                        }
                        if let Some(position) = self.positions.get_mut(&id) {
                            position
                                .outgoing
                                .retain(|pending| !pending.observed(&session));
                        }
                        if !self.sessions.contains_key(&id)
                            && session.thread_id.is_none()
                            && session.provider == provider::Provider::Codex
                        {
                            self.task(
                                Task::Defaults(id.clone()),
                                Request::Defaults {
                                    cwd: session.job.root().clone(),
                                },
                                true,
                            );
                        }
                        if session.workspace_removed
                            || self.sessions.get(&id).is_some_and(|old| {
                                old.workspace_revision != session.workspace_revision
                            })
                        {
                            self.changes.remove(&id);
                            self.changes_at = None;
                            self.workspace_paths.remove(&id);
                            self.skills.remove(&id);
                        }
                        if Some(&id) == self.selected.as_ref()
                            && let Some(Modal::Approval { pending, .. }) = &self.modal
                            && self.question_send.is_none()
                            && !session.pending.iter().any(|p| p.id == pending.id)
                        {
                            self.modal = None;
                        }
                        self.sessions.insert(id.clone(), *session);
                        self.question_received(&id);
                    }
                    (Task::Changes(id, revision), Reply::Changes(patch)) => {
                        if self
                            .sessions
                            .get(&id)
                            .is_some_and(|s| s.workspace_revision != revision)
                        {
                            continue;
                        }
                        self.receive_changes(id, patch.into());
                    }
                    (
                        Task::Defaults(id),
                        Reply::Defaults {
                            model,
                            effort,
                            permissions,
                        },
                    ) => {
                        if let Some(session) = self.sessions.get_mut(&id)
                            && session.thread_id.is_none()
                            && session.provider == provider::Provider::Codex
                        {
                            let Job::Coding(launch) = &session.job else {
                                continue;
                            };
                            session.model = launch.model.clone().or(Some(model));
                            session.effort = launch.effort.clone().or(Some(effort));
                            session.permissions =
                                serde_json::from_str(&permissions).unwrap_or_default();
                        }
                    }
                    (Task::Launch, Reply::ChooseRepository) => {
                        self.modal = Some(Modal::Repository(Editor::default()));
                    }
                    (Task::Launch, Reply::Launched(id)) => {
                        self.selected = Some(id.clone());
                        self.changes_visible = false;
                        self.positions.entry(id).or_default().follow = true;
                        self.drilled = true;
                        self.focus = Focus::Composer;
                        self.modal = None;
                        self.refreshed = None;
                        if let Ok(config) = self.storage.load_config() {
                            self.defaults = config.agent_defaults;
                        }
                    }
                    (Task::Send(id, text, _attachments), Reply::Ok) => {
                        if let Some(position) = self.positions.get_mut(&id) {
                            if position.draft.text() == text {
                                position.draft = Editor::default();
                                position.history = None;
                                position.skills.clear();
                                position.attachments.clear();
                                position.saved_attachments.clear();
                                if let Err(error) = super::media::save_draft(
                                    &self.storage,
                                    &id,
                                    &position.attachments,
                                ) {
                                    self.notice = Some((
                                        format!("Cannot save attachment draft: {error:#}"),
                                        true,
                                    ));
                                }
                            }
                            position.follow = true;
                            position.keep_transcript_position = false;
                            position.transcript_viewport = None;
                        }
                        self.refreshed = None;
                    }
                    (Task::Question, Reply::Ok) => {
                        self.refreshed = None;
                    }
                    (Task::OpenArtifact, Reply::Ok) => {
                        self.notice = Some(("Opened artifact in browser".into(), false));
                    }
                    (Task::Delete(id), Reply::Ok) => {
                        self.panels.shells.remove(&id);
                        self.sidebar.counts.remove(&id);
                        let _ = self.sidebar.save(&self.storage);
                        self.sessions.remove(&id);
                        self.positions.remove(&id);
                        self.changes.remove(&id);
                        self.summaries.retain(|s| s.id != id);
                        self.selected = None;
                        self.drilled = false;
                        self.modal = None;
                        self.busy = false;
                        self.refreshed = None;
                        self.ensure_selected();
                        self.notice = Some(("Chat deleted".into(), false));
                    }
                    (Task::Action, Reply::Ok) => {
                        if let Some(id) = &self.selected {
                            self.skills.remove(id);
                        }
                        self.modal = None;
                        self.notice = Some(("Action accepted".into(), false));
                        self.refreshed = None;
                    }
                    (Task::Interrupt(_), Reply::Ok) => self.refreshed = None,
                    _ => {}
                },
            }
        }
        if visible
            && !self.listing
            && self
                .refreshed
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            self.listing = true;
            self.task(Task::List, Request::List, true);
        }
        if visible {
            self.refresh_statistics();
        }
        if visible && let Some(id) = self.selected.clone() {
            let wanted = self
                .summaries
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.version);
            let have = self.sessions.get(&id).map(|s| s.version);
            if !self.reading && (have.is_none() || wanted != have) {
                self.reading = true;
                self.task(
                    Task::Read(id.clone()),
                    Request::Read {
                        id: id.clone(),
                        version: have,
                    },
                    false,
                );
            }
            if self.drilled
                && self.changes_visible
                && !self.changing
                && self
                    .changes_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
                && self.sessions.get(&id).is_some_and(|s| {
                    matches!(s.job, Job::Coding(_)) && s.baseline.is_some() && !s.workspace_removed
                })
            {
                self.changing = true;
                let revision = self.sessions.get(&id).map_or(0, |s| s.workspace_revision);
                self.task(
                    Task::Changes(id.clone(), revision),
                    Request::Changes { id },
                    false,
                );
            }
        }
    }
    fn filtered(&self) -> Vec<Summary> {
        let mut sessions = self
            .summaries
            .iter()
            .filter(|s| s.kind != "Guide" && s.archived == self.archived)
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by_key(|s| !self.pinned_sessions.contains(&s.id));
        sessions
    }
    fn ensure_selected(&mut self) {
        let list = self.filtered();
        if !list.iter().any(|s| Some(&s.id) == self.selected.as_ref()) && !self.drilled {
            self.selected = list.first().map(|s| s.id.clone());
        }
    }
    fn select(&mut self, id: String) {
        let changed = self.selected.as_ref() != Some(&id);
        self.selected = Some(id.clone());
        self.positions.entry(id.clone()).or_default();
        if changed {
            self.follow_latest();
        }
        self.restore_media(&id);
        self.conversation_sections.clear();
        self.change_rows = Default::default();
        self.text_selection.clear();
        self.changes_at = None;
    }
    fn follow_latest(&mut self) {
        if let Some(id) = self.selected.clone() {
            let position = self.positions.entry(id).or_default();
            position.follow = true;
            position.focused_entry = None;
            position.scroll_anchor = None;
            position.keep_transcript_position = false;
            position.transcript_viewport = None;
        }
        self.text_selection.clear();
    }
    fn move_list(&mut self, delta: i32) {
        let list = self.filtered();
        let current = list
            .iter()
            .position(|s| Some(&s.id) == self.selected.as_ref())
            .unwrap_or(0);
        let index = current
            .saturating_add_signed(delta as isize)
            .min(list.len().saturating_sub(1));
        if let Some(session) = list.get(index) {
            self.select(session.id.clone());
        }
    }
    fn interrupt_chat(&mut self, control: Control) {
        if let Some(id) = self.selected.clone()
            && self.interrupting.insert(id.clone())
        {
            self.task(
                Task::Interrupt(id.clone()),
                Request::Control { id, control },
                false,
            );
        }
    }
    fn control(&mut self, control: Control) {
        if self.busy {
            return;
        }
        if let Some(id) = self.selected.clone() {
            self.busy = true;
            self.task(Task::Action, Request::Control { id, control }, false);
        }
    }
    pub fn launch(&mut self) {
        self.launch_in(None);
    }
    fn launch_in(&mut self, repository: Option<std::path::PathBuf>) {
        if self.busy {
            return;
        }
        let cwd = match std::env::current_dir() {
            Ok(path) => path,
            Err(error) => {
                self.notice = Some((format!("Cannot read current directory: {error}"), true));
                return;
            }
        };
        let mut defaults = self.defaults.clone();
        let remember_repository = repository.is_some();
        if let Some(repository) = repository {
            defaults.repository = Some(repository);
        }
        self.busy = true;
        self.task(
            Task::Launch,
            Request::NewAgent {
                defaults,
                cwd,
                remember_repository,
            },
            true,
        );
    }
    pub(crate) fn menu_entries(&self) -> Vec<&'static str> {
        let archived = self
            .sessions
            .get(self.selected.as_deref().unwrap_or_default())
            .is_some_and(|s| s.archived);
        vec![
            "New coding agent",
            "Continue session",
            "Interrupt / cancel job",
            "Rename session",
            "Change model / reasoning",
            if archived {
                "Unarchive session"
            } else {
                "Archive session"
            },
            "Show active / archived sessions",
            "Toggle agents list · Alt+[",
            "Toggle Changes · Alt+D",
            "Delete clean inactive worktree",
            "Open Reviews",
            "Queued outgoing messages",
            "Pending questions · Alt+↑",
            "Default repository, model, reasoning and worktree",
            "Delete chat and clean up worktree",
            "Open shells",
            "HTML artifacts",
            "Move shell / artifact pane · Alt+P",
            if self
                .selected
                .as_ref()
                .is_some_and(|id| self.pinned_sessions.contains(id))
            {
                "Unpin session"
            } else {
                "Pin session"
            },
        ]
    }
    pub(crate) fn menu_action(&mut self, index: usize) {
        match index {
            18 => {
                if let Some(id) = self.selected.clone() {
                    self.toggle_pin(&id);
                    self.modal = None;
                }
            }
            15 => self.open_resources(false),
            16 => self.open_resources(true),
            17 => {
                self.modal = None;
                self.panel_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
            }
            14 => self.modal = Some(Modal::Delete),
            13 => self.open_defaults(),
            0 => self.launch(),
            1 => self.control(Control::Resume),
            2 => self.control(Control::Interrupt),
            3 => {
                let mut editor = Editor::default();
                if let Some(s) = self
                    .sessions
                    .get(self.selected.as_deref().unwrap_or_default())
                {
                    editor.insert(&s.title);
                }
                self.modal = Some(Modal::Rename(editor));
            }
            4 => self.open_model(0),
            5 => {
                if let Some(id) = self.selected.clone() {
                    let archived = !self.sessions.get(&id).is_some_and(|s| s.archived);
                    self.busy = true;
                    self.task(Task::Action, Request::Archive { id, archived }, false);
                }
            }
            6 => {
                self.archived = !self.archived;
                self.drilled = false;
                self.ensure_selected();
                self.modal = None;
            }
            7 => {
                self.toggle_list();
                self.modal = None;
            }
            8 => {
                self.toggle_changes();
                self.modal = None;
            }
            11 => self.modal = Some(Modal::Queue { selected: 0 }),
            12 => self.modal = Some(Modal::Pending { selected: 0 }),
            9 => self.modal = Some(Modal::Cleanup),
            10 => {
                self.review_requested = true;
                self.modal = None;
            }
            _ => {}
        }
    }
    pub(crate) fn toggle_pin(&mut self, id: &str) {
        let result = self.storage.load_config().and_then(|mut config| {
            if !config.pinned_sessions.remove(id) {
                config.pinned_sessions.insert(id.to_owned());
            }
            self.storage.save_config(&config)?;
            Ok(config.pinned_sessions)
        });
        match result {
            Ok(pins) => self.pinned_sessions = pins,
            Err(error) => {
                self.notice = Some((format!("Could not save session pin: {error:#}"), true))
            }
        }
    }
    pub(crate) fn open_session(&mut self, id: String) {
        self.archived = self
            .summaries
            .iter()
            .find(|s| s.id == id)
            .is_some_and(|s| s.archived);
        self.select(id);
        self.tick_panels(false);
        self.panels.focused = false;
        self.changes_visible = false;
        self.modal = None;
        self.action(Action::Open);
    }
    pub(crate) fn session_prs(&self, id: &str) -> Vec<&crate::github::SessionPr> {
        self.prs.all(id).iter().map(|link| &link.pr).collect()
    }
    pub(crate) fn palette_review(&self) -> Option<&crate::app::App> {
        match &self.panels.view {
            Some(panels::View::PullRequest { app }) if self.panels.visible() => Some(app),
            _ => None,
        }
    }
    pub(crate) fn palette_review_mut(&mut self) -> Option<&mut crate::app::App> {
        if !self.panels.visible() {
            return None;
        }
        match &mut self.panels.view {
            Some(panels::View::PullRequest { app }) => Some(app),
            _ => None,
        }
    }
    fn save_visibility(&mut self) {
        let result = self.storage.load_config().and_then(|mut config| {
            config.agent_list_visible = self.list_visible;
            config.agent_changes_visible = self.changes_visible;
            self.storage.save_config(&config)
        });
        if let Err(error) = result {
            self.notice = Some((format!("Could not save pane visibility: {error:#}"), true));
        }
    }
    pub fn toggle_list(&mut self) {
        self.list_visible = !self.list_visible;
        if !self.list_visible && self.focus == Focus::List {
            self.focus = if self.changes_visible && self.drilled {
                Focus::ChangeTree
            } else {
                Focus::Conversation
            };
        }
        self.save_visibility();
    }
    pub fn toggle_changes(&mut self) {
        self.panels.focused = false;
        self.changes_visible = !self.changes_visible;
        if self.changes_visible && self.panels.view.is_some() {
            self.panels.right = true;
        }
        self.drilled = true;
        self.focus = if self.changes_visible {
            Focus::ChangeTree
        } else {
            Focus::Composer
        };
        self.changes_at = None;
        self.save_visibility();
    }
    fn composer_suggestion(&self) -> Option<&str> {
        let id = self.selected.as_ref()?;
        let position = self.positions.get(id)?;
        if !position.draft.chars.is_empty() || !position.active_attachments().is_empty() {
            return None;
        }
        let session = self.sessions.get(id)?;
        session
            .suggestion
            .as_ref()
            .filter(|suggestion| suggestion.current(session))
            .map(|suggestion| suggestion.text.as_str())
    }
    fn send(&mut self, queue: bool) {
        if self.busy || self.media_pending.is_some() {
            return;
        }
        if self.command_draft() {
            return;
        }
        if let Some(id) = self.selected.clone() {
            let draft = self.positions.entry(id.clone()).or_default().draft.text();
            let attachments = self
                .positions
                .get(&id)
                .map(Position::active_attachments)
                .unwrap_or_default();
            let text = draft.clone();
            if !text.trim().is_empty() {
                if let Some(session) = self.sessions.get(&id) {
                    let position = self.positions.entry(id.clone()).or_default();
                    let earlier = position.outgoing.iter().filter(|p| p.text == text).count();
                    position.outgoing.push(PendingSend {
                        text: text.clone(),
                        queued: (queue && session.turn_id.is_some())
                            || session
                                .pending
                                .iter()
                                .any(|p| p.id == "difu-missing-guidance"),
                        observed_before: PendingSend::matching(session, &text) + earlier,
                    });
                    position.follow = true;
                    position.keep_transcript_position = false;
                    position.transcript_viewport = None;
                }
                self.busy = true;
                self.task(
                    Task::Send(id.clone(), draft, attachments.clone()),
                    Request::Control {
                        id: id.clone(),
                        control: if attachments.is_empty() {
                            Control::Message {
                                text,
                                queue,
                                skills: self
                                    .positions
                                    .get(&id)
                                    .map(|p| p.skills.clone())
                                    .unwrap_or_default(),
                                attachments,
                            }
                        } else {
                            Control::MessageWithAttachments {
                                text,
                                queue,
                                skills: self
                                    .positions
                                    .get(&id)
                                    .map(|p| p.skills.clone())
                                    .unwrap_or_default(),
                                attachments,
                            }
                        },
                    },
                    false,
                );
            }
        }
    }
    fn pending(&mut self, index: usize) {
        self.remember_answers();
        if let Some(pending) = self
            .sessions
            .get(self.selected.as_deref().unwrap_or_default())
            .and_then(|s| s.pending.get(index))
            .cloned()
        {
            let count = pending
                .params
                .get("questions")
                .and_then(Value::as_array)
                .map_or(1, Vec::len)
                .max(1);
            let saved = self
                .selected
                .as_ref()
                .and_then(|id| self.positions.get(id))
                .and_then(|p| p.answers.get(&pending.id.to_string()))
                .cloned();
            self.modal = Some(Modal::Approval {
                pending,
                selected: saved.as_ref().map_or(0, |s| s.selected),
                answers: saved
                    .as_ref()
                    .map(|s| s.answers.clone())
                    .filter(|a| a.len() == count)
                    .unwrap_or_else(|| vec![Editor::default(); count]),
                field: saved
                    .as_ref()
                    .map_or(0, |s| s.field.min(count.saturating_sub(1))),
                scroll: 0,
            });
        }
    }
    fn respond(&mut self, choice: usize) {
        let Some(Modal::Approval {
            pending, answers, ..
        }) = &self.modal
        else {
            return;
        };
        let response = match pending.method.as_str() {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                serde_json::json!({"decision":match choice { 0 => "accept", 1 => "acceptForSession", 2 => "decline", _ => "cancel" }})
            }
            "item/permissions/requestApproval" => {
                serde_json::json!({"permissions":if choice == 0 { pending.params.get("permissions").cloned().unwrap_or_else(|| serde_json::json!({})) } else { serde_json::json!({}) },"scope":"turn"})
            }
            "item/tool/requestUserInput" => {
                let mut map = serde_json::Map::new();
                if let Some(questions) = pending.params.get("questions").and_then(Value::as_array) {
                    for (question, answer) in questions.iter().zip(answers) {
                        if answer.text().trim().is_empty() {
                            self.notice =
                                Some(("Answer each question before submitting".into(), true));
                            return;
                        }
                        if let Some(id) = question.get("id").and_then(Value::as_str) {
                            map.insert(id.into(), serde_json::json!({"answers":[answer.text()]}));
                        }
                    }
                }
                serde_json::json!({"answers":map})
            }
            "mcpServer/elicitation/request" => {
                let content = answers.first().map(Editor::text).unwrap_or_default();
                let content = if choice == 0 && !content.trim().is_empty() {
                    match serde_json::from_str::<Value>(&content) {
                        Ok(value) => value,
                        Err(error) => {
                            self.notice = Some((format!("Invalid JSON: {error}"), true));
                            return;
                        }
                    }
                } else {
                    Value::Null
                };
                serde_json::json!({"action":if choice == 0 { "accept" } else { "decline" },"content":content})
            }
            _ => match serde_json::from_str::<Value>(
                &answers.first().map(Editor::text).unwrap_or_default(),
            ) {
                Ok(value) => value,
                Err(error) => {
                    self.notice = Some((format!("Enter a JSON response: {error}"), true));
                    return;
                }
            },
        };
        self.control(Control::Respond {
            request: pending.id.clone(),
            response,
        });
    }
    fn focused_editor(&self) -> Option<&Editor> {
        if self.question_note_focused
            && let Some(note) = self.question_note()
        {
            return Some(note);
        }
        match &self.modal {
            Some(Modal::Repository(editor) | Modal::ChangeRepository(editor)) => Some(editor),
            Some(Modal::AgentDefaults(form)) => form.fields.get(form.field),
            Some(Modal::Menu { query, .. } | Modal::Commands { query, .. }) => Some(query),
            Some(Modal::Help(state)) => Some(&state.query),
            Some(Modal::Rename(editor) | Modal::QueuedEdit { editor, .. }) => Some(editor),
            Some(Modal::Model {
                model,
                effort,
                field,
            }) => Some(if *field == 0 { model } else { effort }),
            Some(Modal::Approval { answers, field, .. }) => answers.get(*field),
            None if self.focus == Focus::Composer && self.drilled => self
                .selected
                .as_ref()
                .and_then(|id| self.positions.get(id))
                .map(|p| &p.draft),
            _ => None,
        }
    }
    fn typing_focus(&mut self) {
        if self.modal.is_none()
            && self.drilled
            && matches!(self.focus, Focus::Conversation)
            && self
                .selected
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .is_some_and(|s| matches!(s.job, Job::Coding(_)))
        {
            self.focus = Focus::Composer;
            self.text_selection.clear();
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        // Release events terminate hold-to-dictate, but never navigate or submit.
        if key.kind == crossterm::event::KeyEventKind::Release {
            self.voice_key(key);
            return;
        }
        if self.resource_key(key) || self.panel_key(key) {
            return;
        }
        if matches!(key.code, KeyCode::Char(_))
            && !key.modifiers.intersects(
                KeyModifiers::CONTROL
                    | KeyModifiers::ALT
                    | KeyModifiers::SUPER
                    | KeyModifiers::HYPER
                    | KeyModifiers::META,
            )
        {
            self.typing_focus();
        }
        if key.code == KeyCode::Char('c')
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && self.copy_chat_selection()
        {
            return;
        }

        if key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::SUPER)
            && let Some(editor) = self.focused_editor()
        {
            self.clipboard = editor.selected_text();
            return;
        }
        if self.question_key(key) {
            return;
        }
        if self.voice_key(key) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('[') {
            self.toggle_list();
            return;
        }
        if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('d') {
            self.toggle_changes();
            return;
        }
        if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::ALT) {
            self.open_questions();
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            self.remember_answers();
            return;
        }
        if self.drilled
            && key.modifiers == KeyModifiers::SUPER
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && matches!(self.focus, Focus::Composer | Focus::Conversation)
        {
            if self.focus == Focus::Composer {
                if key.code == KeyCode::Up {
                    self.focus = Focus::Conversation;
                    if let Some(p) = self
                        .selected
                        .as_ref()
                        .and_then(|id| self.positions.get_mut(id))
                    {
                        p.focused_entry = self.conversation_sections.last().map(|s| s.id.clone());
                        p.follow = false;
                        p.scroll_anchor = None;
                        p.keep_transcript_position = false;
                        p.transcript_viewport = None;
                        if let Some(section) = self.conversation_sections.last() {
                            p.conversation =
                                section.row.saturating_sub(self.conversation_height / 2);
                        }
                    }
                }
            } else {
                self.move_transcript(key.code == KeyCode::Down);
            }
            return;
        }
        if self.changes_visible && self.drilled && key.code == KeyCode::Esc {
            self.toggle_changes();
            return;
        }
        if key.code == KeyCode::Esc
            && self.drilled
            && matches!(self.focus, Focus::Composer | Focus::Conversation)
            && let Some(session) = self.selected.as_ref().and_then(|id| self.sessions.get(id))
        {
            if session.can_send_waiting() {
                self.interrupt_chat(Control::InterruptAndSend);
                return;
            }
            if session.status.active() {
                self.interrupt_chat(Control::Interrupt);
                return;
            }
        }
        if matches!(key.code, KeyCode::Up | KeyCode::Down)
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SUPER)
            && self.step_change_file(key.code == KeyCode::Down)
        {
            return;
        }
        if self.focus == Focus::ChangeTree && self.changes_visible {
            match key.code {
                KeyCode::Enter => {
                    self.focus = Focus::Changes;
                    return;
                }
                KeyCode::Left | KeyCode::Right => {
                    if let Some(p) = self
                        .selected
                        .as_ref()
                        .and_then(|id| self.positions.get_mut(id))
                    {
                        p.change_tree_horizontal = p
                            .change_tree_horizontal
                            .saturating_add_signed(if key.code == KeyCode::Left { -4 } else { 4 });
                    }
                    return;
                }
                KeyCode::Home | KeyCode::End => {
                    self.move_change_tree(if key.code == KeyCode::Home {
                        i32::MIN
                    } else {
                        i32::MAX
                    });
                    return;
                }
                _ => {}
            }
        }
        if self.focus == Focus::Composer && self.drilled {
            if key.modifiers.is_empty()
                && key.code == KeyCode::Right
                && let Some(suggestion) = self.composer_suggestion().map(str::to_owned)
                && let Some(id) = self.selected.clone()
            {
                self.positions
                    .entry(id)
                    .or_default()
                    .draft
                    .insert(&suggestion);
                return;
            }
            if self.history_key(key) {
                return;
            }
            if key.code == KeyCode::Char('v')
                && key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            {
                if self
                    .selected
                    .as_ref()
                    .and_then(|id| self.positions.get_mut(id))
                    .is_some_and(|p| p.draft.expand_paste())
                {
                    return;
                }
                self.attach(None);
                return;
            }
            if matches!(key.code, KeyCode::Char('$' | '@'))
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                && self
                    .selected
                    .as_ref()
                    .and_then(|id| self.positions.get(id))
                    .is_some_and(|p| {
                        p.draft.cursor == 0
                            || p.draft
                                .chars
                                .get(p.draft.cursor.saturating_sub(1))
                                .is_some_and(|c| c.is_whitespace())
                    })
            {
                if key.code == KeyCode::Char('@') {
                    self.open_paths();
                } else {
                    self.open_commands(true);
                }
                return;
            }
            match key.code {
                KeyCode::Char('/')
                    if self
                        .selected
                        .as_ref()
                        .and_then(|id| self.positions.get(id))
                        .is_none_or(|p| p.draft.chars.is_empty()) =>
                {
                    self.open_commands(false)
                }
                KeyCode::Esc => {
                    self.drilled = false;
                    self.focus = Focus::List;
                }
                KeyCode::Tab | KeyCode::BackTab => self.cycle_focus(key.code == KeyCode::BackTab),
                KeyCode::Enter if ctrl => self.send(true),
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => self.send(false),
                _ => {
                    if let Some(id) = self.selected.clone() {
                        self.positions.entry(id).or_default().draft.key(key);
                    }
                }
            }
            return;
        }
        match key.code {
            KeyCode::Char('n') => self.launch(),
            KeyCode::Char('/') if self.drilled && self.focus != Focus::List => {
                self.open_commands(false)
            }
            KeyCode::Char('/') => {
                self.modal = Some(Modal::Menu {
                    query: Editor::default(),
                    selected: 0,
                })
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help(Default::default())),
            KeyCode::Char('r') => {
                self.refreshed = None;
                self.changes_at = None;
            }
            KeyCode::Enter
                if self.drilled
                    && self.focus == Focus::Conversation
                    && self
                        .selected
                        .as_ref()
                        .and_then(|id| self.positions.get(id))
                        .is_some_and(|p| p.focused_entry.is_some()) =>
            {
                if let Some(entry) = self
                    .selected
                    .as_ref()
                    .and_then(|id| self.positions.get(id))
                    .and_then(|p| p.focused_entry.clone())
                {
                    self.action(Action::ToggleEntry(entry));
                }
            }
            KeyCode::Enter if self.changes_visible && self.focus == Focus::List => {
                if self.selected.is_some() {
                    self.follow_latest();
                    self.drilled = true;
                    self.focus = Focus::ChangeTree;
                    self.changes_at = None;
                }
            }
            KeyCode::Char('i') | KeyCode::Enter if self.drilled => {
                if self.focus == Focus::List {
                    self.follow_latest();
                }
                if self
                    .sessions
                    .get(self.selected.as_deref().unwrap_or_default())
                    .is_some_and(|s| !s.pending.is_empty())
                {
                    self.pending(0);
                } else {
                    self.focus = Focus::Composer;
                }
            }
            KeyCode::Enter => {
                if self.selected.is_some() {
                    self.follow_latest();
                    self.drilled = true;
                    self.focus = Focus::Composer;
                    self.changes_at = None;
                }
            }
            KeyCode::Esc => {
                self.drilled = false;
                self.focus = Focus::List;
            }
            KeyCode::Tab | KeyCode::BackTab => self.cycle_focus(key.code == KeyCode::BackTab),
            KeyCode::Up | KeyCode::Down if self.focus == Focus::Conversation => {
                self.move_transcript(key.code == KeyCode::Down);
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let down = matches!(key.code, KeyCode::Down | KeyCode::PageDown);
                let amount = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                    self.viewport.max(1)
                } else if key.modifiers.contains(KeyModifiers::SUPER) {
                    10
                } else {
                    1
                };
                self.scroll(
                    if down {
                        amount as i32
                    } else {
                        -(amount as i32)
                    },
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
            }
            KeyCode::Left | KeyCode::Right if self.focus == Focus::Changes => {
                if let Some(id) = self.selected.clone() {
                    let p = self.positions.entry(id).or_default();
                    p.horizontal = p
                        .horizontal
                        .saturating_add_signed(if key.code == KeyCode::Left { -4 } else { 4 });
                }
            }
            KeyCode::End => {
                self.text_selection.clear();
                if let Some(id) = self.selected.clone() {
                    let p = self.positions.entry(id).or_default();
                    if self.focus == Focus::Changes {
                        p.changes = self.change_lines.saturating_sub(1);
                    } else {
                        p.follow = true;
                        p.focused_entry = None;
                        p.scroll_anchor = None;
                        p.keep_transcript_position = false;
                        p.transcript_viewport = None;
                    }
                }
            }
            KeyCode::Home => {
                if let Some(id) = self.selected.clone() {
                    let p = self.positions.entry(id).or_default();
                    if self.focus == Focus::Changes {
                        p.changes = 0;
                    } else {
                        p.follow = false;
                        p.conversation = 0;
                        p.scroll_anchor = None;
                        p.keep_transcript_position = false;
                        p.transcript_viewport = None;
                    }
                }
            }
            KeyCode::Char('c') if self.focus == Focus::Changes => self.copy_change(),
            _ => {}
        }
    }
    fn cycle_focus(&mut self, backwards: bool) {
        let helper = self.drilled && self.panels.right && self.panels.visible();
        // Visible panes cycle in screen order: sessions, composer, helper canvas.
        let mut targets = Vec::new();
        if self.list_visible {
            targets.push(0);
        }
        if self.changes_visible && self.drilled {
            targets.extend([3, 4]);
        } else {
            targets.push(1);
        }
        if helper {
            targets.push(2);
        }
        let current = if self.panels.focused && helper {
            2
        } else if self.focus == Focus::List {
            0
        } else if self.changes_visible && self.drilled {
            if self.focus == Focus::Changes { 4 } else { 3 }
        } else {
            1
        };
        let index = targets
            .iter()
            .position(|target| *target == current)
            .unwrap_or(0);
        let next = if backwards {
            (index + targets.len() - 1) % targets.len()
        } else {
            (index + 1) % targets.len()
        };
        match targets.get(next) {
            Some(3) => {
                self.panels.focused = false;
                self.focus = Focus::ChangeTree;
            }
            Some(4) => {
                self.panels.focused = false;
                self.focus = Focus::Changes;
            }
            Some(0) => {
                self.panels.focused = false;
                self.focus = Focus::List;
            }
            Some(2) => {
                self.panels.focused = true;
                self.focus = Focus::Composer;
            }
            _ => {
                if self
                    .selected
                    .as_ref()
                    .and_then(|id| self.sessions.get(id))
                    .is_some_and(|s| matches!(s.job, Job::Coding(_)))
                {
                    if self.focus == Focus::List || !self.drilled {
                        self.follow_latest();
                    }
                    self.drilled = true;
                    self.panels.focused = false;
                    self.focus = if self.changes_visible {
                        Focus::ChangeTree
                    } else {
                        Focus::Composer
                    };
                    self.changes_at = None;
                }
            }
        }
    }
    fn move_transcript(&mut self, down: bool) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let position = self.positions.entry(id.clone()).or_default();
        let current = self
            .conversation_sections
            .iter()
            .position(|s| position.focused_entry.as_ref() == Some(&s.id));
        if down
            && self.drilled
            && (self.conversation_sections.is_empty()
                || current.is_some_and(|i| i + 1 == self.conversation_sections.len()))
            && self
                .sessions
                .get(&id)
                .is_some_and(|s| matches!(s.job, Job::Coding(_)))
        {
            self.focus = Focus::Composer;
            return;
        }
        let index = current
            .map(|i| {
                if down {
                    i.saturating_add(1)
                } else {
                    i.saturating_sub(1)
                }
            })
            .unwrap_or_else(|| {
                self.conversation_sections
                    .iter()
                    .position(|s| s.row >= position.conversation)
                    .unwrap_or_default()
            })
            .min(self.conversation_sections.len().saturating_sub(1));
        if let Some(section) = self.conversation_sections.get(index) {
            position.focused_entry = Some(section.id.clone());
            position.follow = false;
            position.scroll_anchor = None;
            position.keep_transcript_position = false;
            position.transcript_viewport = None;
            if section.row < position.conversation {
                position.conversation = section.row;
                position.scroll_anchor = Some((section.id.clone(), 0));
            } else if section.row
                >= position
                    .conversation
                    .saturating_add(self.conversation_height)
            {
                position.conversation = section
                    .row
                    .saturating_sub(self.conversation_height.saturating_sub(1));
            }
        }
    }
    fn scroll(&mut self, delta: i32, select: bool) {
        if self.focus == Focus::ChangeTree {
            self.move_change_tree(delta);
            return;
        }
        if self.focus == Focus::List {
            self.move_list(delta);
            return;
        }
        if let Some(id) = self.selected.clone() {
            let p = self.positions.entry(id).or_default();
            if self.focus == Focus::Changes {
                if select {
                    p.selection.get_or_insert(p.changes);
                } else {
                    p.selection = None;
                }
                p.changes = p
                    .changes
                    .saturating_add_signed(delta as isize)
                    .min(self.change_lines.saturating_sub(1));
            } else {
                p.follow = false;
                p.scroll_anchor = None;
                p.keep_transcript_position = false;
                p.transcript_viewport = None;
                p.conversation = p.conversation.saturating_add_signed(delta as isize).min(
                    self.conversation_lines
                        .saturating_sub(self.conversation_height),
                );
            }
        }
    }
    fn copy_change(&mut self) {
        if let Some(p) = self.selected.as_ref().and_then(|id| self.positions.get(id)) {
            let start = p.selection.unwrap_or(p.changes).min(p.changes);
            let end = p.selection.unwrap_or(p.changes).max(p.changes);
            let mut previous = None;
            let mut text = Vec::new();
            for row in self
                .change_rows
                .iter()
                .skip(start)
                .take(end.saturating_sub(start) + 1)
            {
                if let Some((index, source)) = &row.source
                    && previous != Some(*index)
                {
                    text.push(source.clone());
                    previous = Some(*index);
                }
            }
            if !text.is_empty() {
                self.clipboard = Some(text.join("\n"));
            }
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.modal.is_none()
            && self.panels.focused
            && let Some(panels::View::PullRequest { app }) = &mut self.panels.view
        {
            app.paste(text.to_owned());
            return;
        }
        if self.browser_input() {
            if let Some(panels::View::Artifact {
                browser: Some(browser),
                ..
            }) = &mut self.panels.view
            {
                browser.paste(text);
            }
            return;
        }
        self.question_reveal = true;
        self.typing_focus();
        self.cancel_voice();
        if self.question_note_focused
            && let Some(note) = self.question_note_mut()
        {
            note.insert(text);
            return;
        }
        if self.modal.is_none()
            && self.focus == Focus::Composer
            && self.drilled
            && let Some(cwd) = self
                .selected
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .and_then(|s| s.workspace.as_deref())
            && let Some(paths) = super::media::paths(text, cwd)
        {
            self.attach(Some(paths));
            return;
        }
        match &mut self.modal {
            Some(Modal::AgentDefaults(form)) => {
                if let Some(editor) = form.fields.get_mut(form.field) {
                    editor.insert(text);
                    form.changed();
                }
            }
            Some(Modal::Repository(editor) | Modal::ChangeRepository(editor)) => {
                editor.insert(text)
            }
            Some(
                Modal::Rename(e)
                | Modal::QueuedEdit { editor: e, .. }
                | Modal::Voice { key: e, .. },
            ) => e.insert(text),
            Some(Modal::Model { .. }) => self.model_paste(text),
            Some(Modal::Approval {
                answers,
                field,
                pending,
                selected,
                ..
            }) => {
                *selected = pending
                    .params
                    .get("questions")
                    .and_then(Value::as_array)
                    .and_then(|q| q.get(*field))
                    .and_then(|q| q.get("options"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                if let Some(e) = answers.get_mut(*field) {
                    e.insert(text);
                }
            }
            Some(Modal::Menu { query, .. } | Modal::Commands { query, .. }) => query.insert(text),
            Some(Modal::Help(state)) => state.query.insert(text),
            None if self.focus == Focus::Composer => {
                if let Some(id) = self.selected.clone() {
                    self.positions.entry(id).or_default().draft.paste(text);
                }
            }
            _ => {}
        }
    }
    fn modal_key(&mut self, key: KeyEvent) {
        if matches!(self.modal, Some(Modal::Usage { .. })) {
            self.usage_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Model { .. })) {
            self.model_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::AgentDefaults(_))) {
            self.defaults_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Voice { .. })) {
            self.voice_modal_key(key);
            return;
        }
        if self.question_or_queue_key(key) {
            return;
        }
        self.remember_answers();
        if matches!(self.modal, Some(Modal::Commands { .. })) {
            self.command_key(key);
            return;
        }
        if key.code == KeyCode::Esc {
            self.text_selection.clear();
            self.modal = None;
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let menu = self.menu_entries();
        let mut action = None;
        match &mut self.modal {
            Some(Modal::Transcript {
                scroll,
                rows,
                height,
                ..
            }) => {
                let maximum = rows.len().saturating_sub(*height);
                *scroll = match key.code {
                    KeyCode::Up => scroll.saturating_sub(1),
                    KeyCode::Down => scroll.saturating_add(1).min(maximum),
                    KeyCode::PageUp => scroll.saturating_sub(*height),
                    KeyCode::PageDown => scroll.saturating_add(*height).min(maximum),
                    KeyCode::Home => 0,
                    KeyCode::End => maximum,
                    _ => *scroll,
                };
            }
            Some(Modal::ChangeRepository(editor)) => match key.code {
                KeyCode::Enter => action = Some(Action::ChangeRepository),
                KeyCode::Esc => self.modal = None,
                _ => {
                    editor.key(key);
                }
            },
            Some(Modal::Repository(editor)) => match key.code {
                KeyCode::Enter => action = Some(Action::ChooseRepository),
                KeyCode::Char('u') if ctrl => editor.clear(),
                _ => editor.key(key),
            },
            Some(Modal::Menu { query, selected }) => {
                let matching: Vec<_> = menu
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| {
                        label.to_lowercase().contains(&query.text().to_lowercase())
                    })
                    .collect();
                match key.code {
                    KeyCode::Up if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        *selected = selected.saturating_sub(1)
                    }
                    KeyCode::Down if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        *selected = (*selected + 1).min(matching.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        action = matching
                            .get(*selected)
                            .map(|(index, _)| Action::MenuItem(*index))
                    }
                    KeyCode::Char('u') if ctrl => {
                        *query = Editor::default();
                        *selected = 0;
                    }
                    _ => {
                        query.key(key);
                        *selected = 0;
                    }
                }
            }
            Some(Modal::Rename(editor)) => {
                if key.code == KeyCode::Enter {
                    if let Some(id) = self.selected.clone() {
                        let title = editor.text();
                        self.busy = true;
                        self.task(Task::Action, Request::Rename { id, title }, false);
                    }
                } else {
                    editor.key(key);
                }
            }
            Some(Modal::Delete) if key.code == KeyCode::Enter => {
                if let Some(id) = self.selected.clone() {
                    self.busy = true;
                    self.task(Task::Delete(id.clone()), Request::Delete { id }, false);
                }
            }
            Some(Modal::Cleanup) if key.code == KeyCode::Enter => {
                if let Some(id) = self.selected.clone() {
                    self.busy = true;
                    self.task(Task::Action, Request::Cleanup { id }, false);
                }
            }
            Some(Modal::Approval {
                pending,
                selected,
                answers,
                field,
                scroll,
            }) => {
                let text_input = pending.method == "item/tool/requestUserInput"
                    || !matches!(
                        pending.method.as_str(),
                        "item/commandExecution/requestApproval"
                            | "item/fileChange/requestApproval"
                            | "item/permissions/requestApproval"
                    );
                match key.code {
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                    KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                    KeyCode::Enter if ctrl || !text_input => {
                        action = Some(Action::Approve(*selected))
                    }
                    KeyCode::Tab | KeyCode::BackTab if text_input => {
                        *field = if key.code == KeyCode::BackTab {
                            (*field + answers.len().saturating_sub(1)) % answers.len().max(1)
                        } else {
                            (*field + 1) % answers.len().max(1)
                        };
                        *scroll = 0
                    }
                    KeyCode::Up if !text_input => *selected = selected.saturating_sub(1),
                    KeyCode::Down if !text_input => {
                        *selected = (*selected + 1).min(
                            if pending.method == "item/permissions/requestApproval" {
                                1
                            } else {
                                3
                            },
                        )
                    }
                    _ if text_input => {
                        if let Some(e) = answers.get_mut(*field) {
                            e.key(key);
                        }
                    }
                    _ => {}
                }
            }
            Some(Modal::Help(state)) => match key.code {
                KeyCode::Up if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    state.scroll = state.scroll.saturating_sub(1)
                }
                KeyCode::Down if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    state.scroll = state.scroll.saturating_add(1)
                }
                KeyCode::Char('u') if ctrl => {
                    state.query.clear();
                    state.scroll = 0;
                }
                _ => {
                    state.query.key(key);
                    state.scroll = 0;
                }
            },
            _ => {}
        }
        if let Some(action) = action {
            self.action(action);
        }
    }
    fn action(&mut self, action: Action) {
        match action {
            Action::DismissNotice => self.notice = None,
            Action::ModelField(index) => self.model_field(index),
            Action::ModelOption(index) => self.model_option(index),
            Action::DefaultOption(index) => self.default_option(index),
            Action::DefaultField(index) => {
                if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
                    form.field = index;
                    form.changed();
                }
            }
            Action::DefaultSave => self.save_defaults(),
            Action::Select(id) => {
                self.select(id);
                self.focus = Focus::List;
            }
            Action::Open => {
                self.follow_latest();
                self.drilled = true;
                self.focus = Focus::Composer;
            }
            Action::Focus(focus) => self.focus = focus,
            Action::Menu => {
                self.modal = Some(Modal::Menu {
                    query: Editor::default(),
                    selected: 0,
                })
            }
            Action::New => self.launch(),
            Action::ToggleList => self.toggle_list(),
            Action::ToggleChanges => self.toggle_changes(),
            Action::ChangeFile(index) => self.select_change(index),
            Action::Approval(index) => self.pending(index),
            Action::Pending => self.open_questions(),
            Action::InlineQuestion(index) => self.open_question(index),
            Action::SubmitQuestion(skip) => self.submit_question(skip),
            Action::QuestionNote(choice) => {
                if let Some(Modal::Approval { selected, .. }) = &mut self.modal {
                    *selected = choice;
                }
                self.open_question_note();
            }
            Action::QuestionChoice(choice) => {
                self.question_note_focused = false;
                self.question_reveal = true;
                if let Some(Modal::Approval { selected, .. }) = &mut self.modal {
                    *selected = choice;
                }
                self.remember_answers();
            }
            Action::Queue => self.modal = Some(Modal::Queue { selected: 0 }),
            Action::EditQueued(index) => self.edit_queued(index),
            Action::SaveQueued(delete) => self.save_queued(delete),
            Action::AnswerFocus => {
                if let Some(Modal::Approval {
                    pending,
                    field,
                    selected,
                    ..
                }) = &mut self.modal
                {
                    *selected = pending
                        .params
                        .get("questions")
                        .and_then(Value::as_array)
                        .and_then(|q| q.get(*field))
                        .and_then(|q| q.get("options"))
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                }
            }
            Action::QueueEditorFocus => {
                if let Some(Modal::QueuedEdit { focus, .. }) = &mut self.modal {
                    *focus = 0;
                }
            }
            Action::VoiceSave => self.save_voice_key(),
            Action::VoiceToggle => self.toggle_voice(),
            Action::VoiceField => {
                if let Some(Modal::Voice { field, .. }) = &mut self.modal {
                    *field = 0;
                }
            }
            Action::QuestionTab(field) => {
                if let Some(Modal::Approval {
                    field: active,
                    selected,
                    ..
                }) = &mut self.modal
                {
                    *active = field;
                    *selected = 0;
                }
            }
            Action::PullRequest(index) => self.open_pr_panel(index),
            Action::Resources(artifacts) => self.open_resources(artifacts),
            Action::Resource(artifacts, index) => self.select_resource(artifacts, index),
            Action::RefreshArtifact => self.refresh_artifact(),
            Action::ExternalArtifact => self.external_artifact(),
            Action::BrowserChoice(index) => self.browser_choice(index),
            Action::MenuItem(index) => self.menu_action(index),
            Action::Command(index) => self.run_command(index),
            Action::Link(url) => {
                let sender = self.sender.clone();
                thread::spawn(move || {
                    let result = crate::github::open_url(&url, &crate::process::Cancel::default())
                        .map(|()| Reply::Ok)
                        .map_err(|error| format!("{error:#}"));
                    let _ = sender.send(ResultMessage {
                        kind: Task::OpenLink,
                        result,
                    });
                });
            }
            Action::ToggleEntry(entry) => {
                self.text_selection.clear();
                self.modal = Some(Modal::Transcript {
                    entry,
                    scroll: 0,
                    rows: Vec::new(),
                    links: Vec::new(),
                    layout: None,
                    height: 0,
                });
            }
            Action::ChangeRepository => {
                if let Some(Modal::ChangeRepository(editor)) = &self.modal {
                    let path = editor.text();
                    self.change_repository(&path);
                }
            }
            Action::ChooseRepository => {
                if let Some(Modal::Repository(editor)) = &self.modal {
                    let path = editor.text();
                    if !path.trim().is_empty() {
                        self.launch_in(Some(path.trim().into()));
                    }
                }
            }
            Action::Approve(choice) => self.respond(choice),
            Action::Answer(field, text) => {
                if let Some(Modal::Approval {
                    answers,
                    field: selected,
                    ..
                }) = &mut self.modal
                    && let Some(editor) = answers.get_mut(field)
                {
                    *editor = Editor::default();
                    editor.insert(&text);
                    *selected = field;
                }
            }
        }
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        if self.notice.is_some() && self.toast_rect.contains((event.column, event.row).into()) {
            if event.kind == MouseEventKind::Down(MouseButton::Left) {
                self.action(Action::DismissNotice);
            }
            return;
        }
        if self.panel_mouse(event) {
            return;
        }
        if self.selection_mouse(event) {
            return;
        }
        if matches!(event.kind, MouseEventKind::Down(_)) {
            self.cancel_voice();
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, action)) = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains((event.column, event.row).into()))
                    .cloned()
                {
                    self.action(action);
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if let Some(Modal::Help(state)) = &mut self.modal {
                    state.scroll = state.scroll.saturating_add_signed(
                        if event.kind == MouseEventKind::ScrollDown {
                            3
                        } else {
                            -3
                        },
                    );
                } else if let Some(Modal::Transcript {
                    scroll,
                    rows,
                    height,
                    ..
                }) = &mut self.modal
                {
                    *scroll = scroll
                        .saturating_add_signed(if event.kind == MouseEventKind::ScrollDown {
                            3
                        } else {
                            -3
                        })
                        .min(rows.len().saturating_sub(*height));
                } else if let Some(Modal::Approval { scroll, .. }) = &mut self.modal {
                    *scroll =
                        scroll.saturating_add_signed(if event.kind == MouseEventKind::ScrollDown {
                            3
                        } else {
                            -3
                        });
                } else if self.modal.is_none() {
                    if let Some((_, Action::Focus(focus))) =
                        self.hits.iter().rev().find(|(r, a)| {
                            r.contains((event.column, event.row).into())
                                && matches!(a, Action::Focus(_))
                        })
                    {
                        self.focus = *focus;
                    }
                    self.scroll(
                        if event.kind == MouseEventKind::ScrollDown {
                            3
                        } else {
                            -3
                        },
                        false,
                    );
                }
            }
            _ => {}
        }
    }
}

fn inner(rect: Rect) -> Rect {
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}
fn panel(frame: &mut Frame, rect: Rect, title: &str, focused: bool) -> Rect {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }));
    if !title.is_empty() {
        block = block.title(format!(" {title} "));
    }
    frame.render_widget(block, rect);
    inner(rect)
}
fn join_bottom_border(frame: &mut Frame, rect: Rect) {
    if rect.width > 1 && rect.height > 1 {
        for x in [rect.x, rect.right().saturating_sub(1)] {
            if let Some(cell) = frame
                .buffer_mut()
                .cell_mut((x, rect.bottom().saturating_sub(1)))
            {
                cell.set_symbol("┴");
            }
        }
    }
}
fn editor(frame: &mut Frame, rect: Rect, label: &str, value: &Editor, focused: bool) {
    let area = panel(frame, rect, label, focused);
    if area.is_empty() {
        return;
    }
    let (lines, (x, y)) = value.styled_layout(
        area.width as usize,
        Style::default().bg(ACCENT).fg(crate::ui::INK),
    );
    let offset = y.saturating_sub(area.height.saturating_sub(1) as usize);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(offset)
                .take(area.height as usize)
                .collect::<Vec<_>>(),
        )
        .style(Style::default().fg(TEXT)),
        area,
    );
    if focused {
        frame.set_cursor_position((
            area.x
                .saturating_add(x.min(area.width.saturating_sub(1) as usize) as u16),
            area.y.saturating_add(
                y.saturating_sub(offset)
                    .min(area.height.saturating_sub(1) as usize) as u16,
            ),
        ));
    }
}
fn wrapped(text: &str, width: u16) -> Vec<String> {
    let clean = crate::model::clean(text);
    clean
        .split('\n')
        .flat_map(|line| {
            if line.is_empty() {
                vec![String::new()]
            } else {
                textwrap::wrap(line, usize::from(width).max(1))
                    .into_iter()
                    .map(|s| s.into_owned())
                    .collect()
            }
        })
        .collect()
}
impl Ui {
    fn button(
        &mut self,
        frame: &mut Frame,
        rect: Rect,
        label: &str,
        action: Action,
        selected: bool,
    ) {
        frame.render_widget(
            Paragraph::new(label.to_owned()).style(if selected {
                Style::default().bg(ACCENT).fg(crate::ui::INK)
            } else {
                Style::default().fg(ACCENT).bg(PANEL)
            }),
            rect,
        );
        self.hits.push((rect, action));
    }
    pub fn draw(&mut self, frame: &mut Frame) {
        self.text_selection.frame();
        self.hits.clear();
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 40 || area.height < 12 {
            frame.render_widget(Paragraph::new("Resize to at least 40 × 12"), area);
            return;
        }
        let toolbar = Rect::new(1, 2, area.width.saturating_sub(2), 1);
        self.button(
            frame,
            Rect::new(toolbar.x, toolbar.y, 14, 1),
            " n New agent ",
            Action::New,
            false,
        );
        self.button(
            frame,
            Rect::new(toolbar.x + 15, toolbar.y, 12, 1),
            " / Actions ",
            Action::Menu,
            false,
        );
        if toolbar.width >= 65 {
            self.button(
                frame,
                Rect::new(toolbar.x + 28, toolbar.y, 17, 1),
                " Alt+[ Agents ",
                Action::ToggleList,
                self.list_visible,
            );
            self.button(
                frame,
                Rect::new(toolbar.x + 46, toolbar.y, 18, 1),
                " Alt+D Changes ",
                Action::ToggleChanges,
                self.changes_visible,
            );
        }
        let resources = self.visible_resources();
        let session_prs = self
            .selected
            .as_ref()
            .map(|id| self.prs.all(id).to_vec())
            .unwrap_or_default();
        let resource_height = u16::from(!resources.is_empty() || !session_prs.is_empty());
        let content = Rect::new(
            1,
            4,
            area.width.saturating_sub(2),
            area.height.saturating_sub(4 + resource_height),
        );
        self.viewport = content.height.saturating_sub(2) as usize;
        let list_width = if self.list_visible {
            (content.width / 4)
                .clamp(20, 38)
                .min(content.width.saturating_sub(20))
        } else {
            0
        };
        let panel = self.panels.visible();
        let changes_width = if self.drilled && panel && self.panels.right {
            (content.width.saturating_sub(list_width) / 2).max(12)
        } else {
            0
        };
        let list = Rect::new(content.x, content.y, list_width, content.height);
        let conversation = Rect::new(
            content.x + list_width,
            content.y,
            content.width.saturating_sub(list_width + changes_width),
            content.height,
        );
        let changes = Rect::new(
            conversation.right(),
            content.y,
            changes_width,
            content.height,
        );
        self.prs.visible.clear();
        if list_width > 0 {
            self.draw_list(frame, list);
        }
        if panel && !self.panels.right {
            self.draw_panel(frame, conversation);
        } else if self.drilled && self.changes_visible {
            self.draw_changes(frame, conversation);
        } else {
            self.draw_conversation(frame, conversation);
        }
        if changes_width > 0 {
            self.draw_panel(frame, changes);
        }
        let mut x = conversation.x;
        for (artifacts, count) in resources {
            let label = format!(
                " {} ({count}) ",
                if artifacts { "Artifacts" } else { "Shells" }
            );
            let width = (label.len() as u16).min(conversation.right().saturating_sub(x));
            self.button(
                frame,
                Rect::new(x, content.bottom(), width, 1),
                &label,
                Action::Resources(artifacts),
                self.focus == Focus::Resources(artifacts),
            );
            x = x.saturating_add(width + 1);
        }
        let start = match self.focus {
            Focus::PullRequest(index) => index.min(session_prs.len().saturating_sub(1)),
            _ => 0,
        };
        for (index, link) in session_prs.iter().enumerate().skip(start) {
            let pr = &link.pr;
            let label = format!(" PR #{} · {} ", pr.key.number, pr.label());
            let width = (unicode_width::UnicodeWidthStr::width(label.as_str()) as u16)
                .min(conversation.right().saturating_sub(x));
            if width == 0 {
                break;
            }
            let rect = Rect::new(x, content.bottom(), width, 1);
            self.hits.push((rect, Action::PullRequest(index)));
            frame.render_widget(
                Paragraph::new(label).style(if self.focus == Focus::PullRequest(index) {
                    Style::default().bg(prs::color(pr)).fg(crate::ui::INK)
                } else {
                    Style::default().fg(prs::color(pr)).bg(BG)
                }),
                rect,
            );
            x = x.saturating_add(width + 1);
        }
        self.text_selection.highlight(frame);
        self.draw_modal(frame);
        self.draw_toast(frame);
    }
    fn expire_toast(&mut self, now: Instant) {
        match (&self.notice, &self.toast_started) {
            (Some((text, error)), Some((previous, was_error, started)))
                if text == previous && error == was_error =>
            {
                if now.saturating_duration_since(*started) >= Duration::from_secs(5) {
                    self.notice = None;
                    self.toast_started = None;
                }
            }
            (Some((text, error)), _) => self.toast_started = Some((text.clone(), *error, now)),
            (None, _) => self.toast_started = None,
        }
    }
    fn draw_toast(&mut self, frame: &mut Frame) {
        self.expire_toast(Instant::now());
        self.toast_rect = Rect::default();
        let Some((notice, error)) = &self.notice else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(2).min(64);
        if width < 4 {
            return;
        }
        let text = crate::model::clean(notice);
        let rows = wrapped(&text, width.saturating_sub(2));
        let height = (rows.len() as u16)
            .saturating_add(2)
            .min(area.height.saturating_sub(2));
        if height < 3 {
            return;
        }
        let rect = Rect::new(
            area.right().saturating_sub(width + 1),
            area.bottom().saturating_sub(height + 1),
            width,
            height,
        );
        let color = if *error { RED } else { GREEN };
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(color)),
                )
                .style(Style::default().fg(color).bg(BG)),
            rect,
        );
        self.toast_rect = rect;
    }
    fn draw_list(&mut self, frame: &mut Frame, rect: Rect) {
        let area = panel(
            frame,
            rect,
            if self.archived {
                "Archived agents"
            } else {
                "Agents"
            },
            !self.panels.focused && self.focus == Focus::List,
        );
        self.hits.push((rect, Action::Focus(Focus::List)));
        let items = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        let list = self.filtered();
        let heights = list
            .iter()
            .map(|session| {
                4 + usize::from(self.sidebar.counts.contains_key(&session.id))
                    + self.prs.all(&session.id).len()
            })
            .collect::<Vec<_>>();
        let selected = list
            .iter()
            .position(|s| Some(&s.id) == self.selected.as_ref())
            .unwrap_or(0);
        self.list_scroll = self.list_scroll.min(selected);
        while self.list_scroll < selected
            && heights
                .iter()
                .take(selected + 1)
                .skip(self.list_scroll)
                .sum::<usize>()
                > usize::from(items.height)
        {
            self.list_scroll += 1;
        }
        let mut visible_pr_sessions = Vec::new();
        let mut offset = 0;
        for (index, session) in list.iter().enumerate().skip(self.list_scroll) {
            if offset >= usize::from(items.height) {
                break;
            }
            let height = heights.get(index).copied().unwrap_or(4);
            let selected = Some(&session.id) == self.selected.as_ref();
            let row = Rect::new(
                items.x,
                items.y.saturating_add(offset as u16),
                items.width,
                (height as u16 - 1).min(items.height.saturating_sub(offset as u16)),
            );
            offset += height;
            visible_pr_sessions.push(session.id.clone());
            let color = match session.status {
                Status::Failed => RED,
                Status::Waiting => ACCENT,
                Status::Completed | Status::Idle => GREEN,
                _ => DIM,
            };
            let elapsed = if session.status.active() {
                session
                    .turn_started_at
                    .map(|start| {
                        format!(
                            " {}s",
                            chrono::Utc::now()
                                .timestamp_millis()
                                .saturating_sub(start)
                                .max(0)
                                / 1000
                        )
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let status = format!(
                "{}{}{}",
                session.status.label(),
                elapsed,
                if session.archived { " · archived" } else { "" }
            );
            let pin = if self.pinned_sessions.contains(&session.id) {
                "◆ "
            } else {
                ""
            };
            let title_width = usize::from(row.width)
                .saturating_sub(2 + unicode_width::UnicodeWidthStr::width(pin));
            let mut lines = vec![
                Line::from(Span::styled(
                    format!(
                        "{} {pin}{}",
                        if selected { "›" } else { " " },
                        crate::ui::crop(&session.title, 0, title_width)
                    ),
                    Style::default().fg(if selected { ACCENT } else { TEXT }),
                )),
                Line::from(Span::styled(
                    format!(
                        "  {}{}{}",
                        session
                            .repository
                            .as_ref()
                            .unwrap_or(&session.workspace)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        if session.worktree { " 🌳" } else { "" },
                        if session.pending > 0 {
                            format!(" · {} requests", session.pending)
                        } else if session.queued > 0 {
                            format!(" · {} queued", session.queued)
                        } else {
                            String::new()
                        }
                    ),
                    Style::default().fg(DIM),
                )),
            ];
            if let Some(stats) = self.sidebar.counts.get(&session.id) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("+{}", stats.added), Style::default().fg(GREEN)),
                    Span::styled(format!(" -{}", stats.removed), Style::default().fg(RED)),
                ]));
            }
            lines.push(Line::from(Span::styled(
                format!("  {status}"),
                Style::default().fg(color),
            )));
            for link in self.prs.all(&session.id) {
                let pr = &link.pr;
                lines.push(Line::from(Span::styled(
                    format!("  PR #{} · {}", pr.key.number, pr.label()),
                    Style::default().fg(prs::color(pr)),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).style(if selected {
                    crate::ui::user_message_style()
                } else {
                    Style::default().bg(BG)
                }),
                row,
            );
            self.hits.push((row, Action::Select(session.id.clone())));
        }
        if list.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.listing {
                    "Connecting…"
                } else {
                    "No matching sessions\n\nn · New agent"
                })
                .style(Style::default().fg(DIM)),
                items,
            );
        }
        self.prs.visible.extend(visible_pr_sessions);
    }
    fn draw_conversation(&mut self, frame: &mut Frame, rect: Rect) {
        let title = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(|s| format!("{} · {}", s.title, s.status.label()))
            .unwrap_or_else(|| "Conversation".into());
        let mut area = panel(
            frame,
            rect,
            &crate::model::clean(&title),
            !self.panels.focused && matches!(self.focus, Focus::Conversation | Focus::Composer),
        );
        self.hits.push((rect, Action::Focus(Focus::Conversation)));
        let Some(id) = self.selected.clone() else {
            frame.render_widget(Paragraph::new("Launch a coding agent with n.\n\nSessions and review jobs keep running when difu closes.").wrap(Wrap { trim:false }).style(Style::default().fg(DIM)), area);
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            frame.render_widget(Paragraph::new("Loading session…"), area);
            return;
        };
        let coding = matches!(session.job, Job::Coding(_));
        if coding {
            self.restore_media(&id);
        }
        let voice_status = self.voice_status();
        let composer_height = if self.inline_question() {
            self.question_height(area.width)
                .min(area.height.saturating_sub(5))
        } else if matches!(self.modal, Some(Modal::Commands { .. })) {
            self.commands_height().min(area.height.saturating_sub(5))
        } else if self.drilled && coding {
            let lines = self
                .positions
                .entry(id.clone())
                .or_default()
                .draft
                .layout(usize::from(area.width.saturating_sub(2)))
                .0
                .len()
                .clamp(1, 10) as u16;
            (lines + 2 + if voice_status.is_some() { 2 } else { 0 })
                .min(area.height.saturating_sub(3))
        } else {
            0
        };
        let dock_composer = composer_height > 0 && !self.inline_question() && !area.is_empty();
        if dock_composer {
            // The input's last row shares the pane border; reclaim the old inset.
            area.height = area
                .height
                .saturating_add(1)
                .min(rect.bottom().saturating_sub(area.y));
        }
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let mut activity_rows =
            transcript::activity(session, area.width, chrono::Utc::now().timestamp_millis());
        activity_rows.extend(transcript::waiting_messages(
            session,
            area.width,
            self.positions
                .get(&id)
                .map(|p| p.outgoing.as_slice())
                .unwrap_or_default(),
        ));
        let activity_height = (activity_rows.len().min(usize::from(area.height / 3))) as u16;
        let requests_height = u16::from(!session.pending.is_empty()) + activity_height;
        let (model, effort) = match &session.job {
            Job::Guide { model, .. } | Job::Conflict { model, .. } => {
                (model.model.as_str(), model.effort.as_str())
            }
            Job::Coding(_) => (
                session.model.as_deref().unwrap_or("Provider defaults"),
                session.effort.as_deref().unwrap_or("default reasoning"),
            ),
        };
        let meta = format!("{} · {model} · {effort}", session.job.kind());
        frame.render_widget(
            Paragraph::new(meta).style(Style::default().fg(DIM)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let body = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height
                .saturating_sub(composer_height + requests_height + 1),
        );
        self.conversation_height = usize::from(body.height);
        let position = self.positions.entry(id.clone()).or_default();
        if let Some((old_width, old_height, version)) = position.transcript_viewport {
            if old_width != body.width || version != session.version {
                position.keep_transcript_position = false;
            } else if old_height != body.height {
                position.keep_transcript_position = true;
            }
        }
        position.transcript_viewport = Some((body.width, body.height, session.version));
        let before_measure = position.conversation;
        let view = self.transcript_window.render(
            session,
            position,
            body.width,
            body.height,
            self.focus == Focus::Conversation,
        );
        self.text_selection
            .rebase(1, position.conversation as isize - before_measure as isize);
        let transcript_window::View {
            mut lines,
            links,
            start,
            mut total,
            sections,
        } = view;
        if session.entries.is_empty() && total == 0 {
            lines.push(Line::from("Preparing session…"));
            total = 1;
        }
        if let Some(error) = &session.error
            && !session
                .entries
                .iter()
                .any(|e| e.kind == "error" && &e.text == error)
            && start + lines.len() == total
        {
            let extra = wrapped(error, body.width)
                .into_iter()
                .map(|text| Line::from(Span::styled(text, Style::default().fg(RED))))
                .collect::<Vec<_>>();
            total += extra.len();
            lines.extend(extra);
        }
        self.conversation_lines = total;
        for (index, section) in sections.iter().enumerate() {
            let first = section.row.max(position.conversation);
            let last = sections.get(index + 1).map_or(total, |next| next.row).min(
                position
                    .conversation
                    .saturating_add(usize::from(body.height)),
            );
            if section.tool && first < last {
                self.hits.push((
                    Rect::new(
                        body.x,
                        body.y + first.saturating_sub(position.conversation) as u16,
                        body.width,
                        (last - first) as u16,
                    ),
                    Action::ToggleEntry(section.id.clone()),
                ));
            }
        }
        self.hits
            .extend(transcript::link_hits(&links, body, position.conversation));
        self.conversation_sections = sections;
        self.text_selection
            .register_window(1, body, position.conversation, start, &lines);
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(position.conversation.saturating_sub(start))
                    .take(body.height as usize)
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        let pending = !session.pending.is_empty();
        let queued = !session.queue.is_empty();
        let question_count = session.pending_question_count();
        let approval_count = session
            .pending
            .iter()
            .filter(|p| p.method != "item/tool/requestUserInput")
            .count();
        if pending {
            let questions = self.question_index().map_or_else(
                || format!("{question_count} question(s)"),
                |index| format!("{} of {question_count} questions", index + 1),
            );
            let header = format!("{questions} · {approval_count} approval(s) · Alt+↑ Questions");
            self.button(
                frame,
                Rect::new(area.x, body.bottom() + activity_height, area.width, 1),
                &header,
                Action::Pending,
                true,
            );
            let offset = (unicode_width::UnicodeWidthStr::width(header.as_str()) as u16)
                .saturating_add(1)
                .min(area.width);
            self.draw_question_tabs(
                frame,
                Rect::new(
                    area.x + offset,
                    body.bottom() + activity_height,
                    area.width - offset,
                    1,
                ),
            );
        }
        if activity_height > 0 {
            let activity_area = Rect::new(area.x, body.bottom(), area.width, activity_height);
            frame.render_widget(
                Paragraph::new(
                    activity_rows
                        .into_iter()
                        .take(usize::from(activity_height))
                        .collect::<Vec<_>>(),
                ),
                activity_area,
            );
            if queued {
                self.hits.push((activity_area, Action::Queue));
            }
        }
        if composer_height > 0 {
            let composer = Rect::new(
                area.x,
                area.bottom().saturating_sub(composer_height),
                area.width,
                composer_height,
            );
            if self.inline_question() {
                self.draw_inline_question(frame, composer);
                return;
            }
            if matches!(self.modal, Some(Modal::Commands { .. })) {
                self.draw_commands(frame, composer);
                if dock_composer {
                    join_bottom_border(frame, composer);
                }
                return;
            }
            let composer = if voice_status.is_some() {
                self.draw_voice_preview(
                    frame,
                    Rect::new(composer.x, composer.y, composer.width, 2),
                );
                Rect::new(
                    composer.x,
                    composer.y + 2,
                    composer.width,
                    composer.height.saturating_sub(2),
                )
            } else {
                composer
            };
            let placeholder = self
                .composer_suggestion()
                .unwrap_or("Ask anything…")
                .to_owned();
            let position = self.positions.entry(id.clone()).or_default();
            editor(
                frame,
                composer,
                "",
                &position.draft,
                self.focus == Focus::Composer && !self.panels.focused && self.modal.is_none(),
            );
            if position.draft.chars.is_empty() {
                frame.render_widget(
                    Paragraph::new(placeholder).style(Style::default().fg(DIM)),
                    inner(composer),
                );
            }
            if dock_composer {
                join_bottom_border(frame, composer);
            }
            self.hits.push((composer, Action::Focus(Focus::Composer)));
        } else if !self.drilled {
            self.button(
                frame,
                Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                "Enter · Open session",
                Action::Open,
                false,
            );
        }
    }
    fn draw_modal(&mut self, frame: &mut Frame) {
        if self.modal.is_none()
            || self.inline_question()
            || matches!(self.modal, Some(Modal::Commands { .. }))
        {
            return;
        }
        if !matches!(self.modal, Some(Modal::Transcript { .. })) {
            self.text_selection.clear();
        }
        self.text_selection.frame();
        // Underlying panes must not receive modal mouse events.
        self.hits.clear();
        let full = frame.area();
        let width = full.width.saturating_sub(4).min(110);
        let height = full.height.saturating_sub(4).min(38);
        let rect = Rect::new(
            full.width.saturating_sub(width) / 2,
            full.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default().style(Style::default().bg(PANEL).fg(TEXT)),
            rect,
        );
        let menu_entries = self.menu_entries();
        let title = match self.modal {
            Some(Modal::Transcript { .. }) => "Message · ↑/↓ Scroll · PgUp/PgDn Page · Esc Close",
            Some(Modal::InstallBrowser { .. }) => "Optional HTML preview · Esc cancels",
            Some(Modal::AgentDefaults(_)) => "New agent defaults · Tab fields · Esc cancels",
            Some(Modal::Voice { .. }) => "Voice settings",
            Some(Modal::Repository(_)) => "Choose and remember your default repository",
            Some(Modal::ChangeRepository(_)) => "Change session repository",
            Some(Modal::Menu { .. }) => "Agent actions",
            Some(Modal::Commands { .. }) => "Session commands",
            Some(Modal::Status) => "Session status",
            Some(Modal::Usage { .. }) => "Provider usage · ↑/↓ Scroll · r Refresh · Esc Close",
            Some(Modal::Pending { .. }) => "Pending questions and approvals",
            Some(Modal::Queue { .. }) => "Queued outgoing messages",
            Some(Modal::QueuedEdit { .. }) => "Edit queued message",
            Some(Modal::Rename(_)) => "Rename session",
            Some(Modal::Model { .. }) => "Session model",
            Some(Modal::Approval { .. }) => "Agent needs your input",
            Some(Modal::Resources {
                artifacts: true, ..
            }) => "HTML artifacts · Enter open",
            Some(Modal::Resources { .. }) => "Open shells · Enter view",
            Some(Modal::Cleanup) => "Delete worktree",
            Some(Modal::Delete) => "Stop and delete chat",
            Some(Modal::Help(_)) => "Search agent shortcuts",
            None => "",
        };
        let area = panel(frame, rect, title, true);
        if let Some(Modal::Transcript {
            entry,
            scroll,
            rows,
            links,
            layout,
            height,
        }) = &mut self.modal
        {
            if let Some(session) = self.selected.as_ref().and_then(|id| self.sessions.get(id)) {
                let key = (area.width, session.version);
                if *layout != Some(key) {
                    if let Some(source) = session.entries.iter().find(|e| e.id == *entry) {
                        let mut position = Position::default();
                        position.expanded.insert(entry.clone());
                        let (rendered, _, targets) = transcript::render_entries(
                            session,
                            std::slice::from_ref(source),
                            &position,
                            area.width,
                            false,
                        );
                        *rows = rendered;
                        *links = targets;
                    }
                    *layout = Some(key);
                }
            }
            *height = usize::from(area.height);
            *scroll = (*scroll).min(rows.len().saturating_sub(*height));
            let visible = rows
                .iter()
                .skip(*scroll)
                .take(*height)
                .cloned()
                .collect::<Vec<_>>();
            self.hits
                .extend(transcript::link_hits(links, area, *scroll));
            self.text_selection
                .register_window(3, area, *scroll, *scroll, &visible);
            frame.render_widget(Paragraph::new(visible), area);
            self.text_selection.highlight(frame);
            return;
        }
        if matches!(self.modal, Some(Modal::Model { .. })) {
            self.draw_model(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::AgentDefaults(_))) {
            self.draw_defaults(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Commands { .. })) {
            self.draw_commands(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Voice { .. })) {
            self.draw_voice_settings(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Usage { .. })) {
            self.draw_usage(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Status)) {
            self.draw_status(frame, area);
            return;
        }
        if self.draw_questions_or_queue(frame, area) {
            return;
        }
        if matches!(self.modal, Some(Modal::InstallBrowser { .. })) {
            self.draw_browser_install(frame, area);
            return;
        }
        let mut deferred = Vec::new();
        if let Some(Modal::Resources {
            artifacts,
            selected,
        }) = self.modal.as_ref()
        {
            let (artifacts, selected) = (*artifacts, *selected);
            self.draw_resources(frame, area, artifacts, selected);
            return;
        }
        match &mut self.modal {
            Some(
                Modal::Transcript { .. }
                | Modal::Model { .. }
                | Modal::Resources { .. }
                | Modal::InstallBrowser { .. },
            ) => {}
            Some(Modal::ChangeRepository(value)) => {
                let input = Rect::new(area.x, area.y, area.width, area.height.min(3));
                editor(frame, input, "Local repository", value, true);
                deferred.push((
                    Rect::new(area.x, area.y.saturating_add(4), area.width, 1).intersection(area),
                    "[ Enter · Change repository ]".into(),
                    Action::ChangeRepository,
                    true,
                ));
            }
            Some(Modal::Repository(value)) => {
                let input = Rect::new(area.x, area.y, area.width, area.height.min(3));
                editor(frame, input, "Local repository", value, true);
                let button =
                    Rect::new(area.x, area.y.saturating_add(4), area.width, 1).intersection(area);
                deferred.push((
                    button,
                    "[ Enter · Save and open session ]".into(),
                    Action::ChooseRepository,
                    true,
                ));
            }
            Some(Modal::Menu { query, selected }) => {
                let search = Rect::new(area.x, area.y, area.width, 3.min(area.height));
                editor(frame, search, "Filter commands", query, true);
                let matching: Vec<_> = menu_entries
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| {
                        label.to_lowercase().contains(&query.text().to_lowercase())
                    })
                    .collect();
                *selected = (*selected).min(matching.len().saturating_sub(1));
                for (offset, (index, label)) in matching
                    .iter()
                    .enumerate()
                    .take(area.height.saturating_sub(3) as usize)
                {
                    deferred.push((
                        Rect::new(area.x, search.bottom() + offset as u16, area.width, 1),
                        format!("{} {label}", if offset == *selected { ">" } else { " " }),
                        Action::MenuItem(*index),
                        offset == *selected,
                    ));
                }
            }
            Some(Modal::Rename(value)) => {
                editor(
                    frame,
                    Rect::new(area.x, area.y, area.width, 3.min(area.height)),
                    "Name · Enter save",
                    value,
                    true,
                );
            }
            Some(Modal::Delete) => {
                frame.render_widget(Paragraph::new("Stop this agent and permanently delete its difu chat, saved questions, queue, and attachments? Its clean, unlocked difu-owned worktree will also be removed.\n\nModified worktrees remain protected: deletion stops with an error and retains the chat. Existing directories, named branches, commits, and native provider history are not deleted.\n\nEnter confirms · Esc cancels").wrap(Wrap { trim:false }), area);
            }
            Some(Modal::Cleanup) => {
                frame.render_widget(Paragraph::new("Delete this session’s clean, inactive worktree?\n\nModified, untracked, ignored, active and Git-locked worktrees are protected. Existing directories are never deleted. The named branch and its commits are retained.\n\nEnter confirms · Esc cancels").wrap(Wrap { trim:false }), area);
            }
            Some(Modal::Help(state)) => {
                let search = Rect::new(area.x, area.y, area.width, 3.min(area.height));
                editor(
                    frame,
                    search,
                    "Type to filter · Ctrl+U clear",
                    &state.query,
                    true,
                );
                let help = agent_help(&state.query.text());
                let lines = help
                    .into_iter()
                    .flat_map(|(key, desc)| wrapped(&format!("{key:22} {desc}"), area.width))
                    .map(Line::from)
                    .collect::<Vec<_>>();
                let height = area.height.saturating_sub(3);
                state.scroll = state
                    .scroll
                    .min(lines.len().saturating_sub(height as usize));
                frame.render_widget(
                    Paragraph::new(
                        lines
                            .into_iter()
                            .skip(state.scroll)
                            .take(height as usize)
                            .collect::<Vec<_>>(),
                    ),
                    Rect::new(area.x, search.bottom(), area.width, height),
                );
            }
            Some(Modal::Approval {
                pending,
                selected,
                answers,
                field,
                scroll,
            }) => {
                let questions = pending.params.get("questions").and_then(Value::as_array);
                if pending.method == "item/tool/requestUserInput" {
                    if let Some(questions) = questions
                        && let Some(question) = questions.get(*field)
                    {
                        let prompt = question
                            .get("question")
                            .and_then(Value::as_str)
                            .unwrap_or("Answer");
                        let prompt = format!(
                            "Question {} / {} · Tab next · Shift+Tab previous\n\n{prompt}",
                            *field + 1,
                            questions.len()
                        );
                        let prompt_lines = wrapped(&prompt, area.width);
                        let max_prompt = area.height.saturating_sub(9) as usize;
                        *scroll = (*scroll).min(prompt_lines.len().saturating_sub(max_prompt));
                        let shown = prompt_lines
                            .into_iter()
                            .skip(*scroll)
                            .take(max_prompt)
                            .map(Line::from)
                            .collect::<Vec<_>>();
                        let mut y = area.y + shown.len() as u16;
                        frame.render_widget(
                            Paragraph::new(shown),
                            Rect::new(area.x, area.y, area.width, y - area.y),
                        );
                        if let Some(options) = question.get("options").and_then(Value::as_array) {
                            for option in options {
                                if y >= area.bottom().saturating_sub(5) {
                                    break;
                                }
                                let label = option
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let description = option
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                deferred.push((
                                    Rect::new(area.x, y, area.width, 1),
                                    format!("  {label} — {description}"),
                                    Action::Answer(*field, label.into()),
                                    false,
                                ));
                                y += 1;
                            }
                        }
                        if let Some(answer) = answers.get(*field) {
                            let masked;
                            let visible = if question.get("isSecret").and_then(Value::as_bool)
                                == Some(true)
                            {
                                masked = {
                                    let mut masked = answer.clone();
                                    masked.chars = answer
                                        .chars
                                        .iter()
                                        .map(|c| if *c == '\n' { '\n' } else { '*' })
                                        .collect();
                                    masked
                                };
                                &masked
                            } else {
                                answer
                            };
                            editor(
                                frame,
                                Rect::new(area.x, area.bottom().saturating_sub(5), area.width, 4),
                                "Your answer · Ctrl+Enter submits all",
                                visible,
                                true,
                            );
                        }
                    }
                    deferred.push((
                        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                        "[ Submit answers · Ctrl+Enter ]".into(),
                        Action::Approve(0),
                        false,
                    ));
                } else {
                    let content = serde_json::to_string_pretty(&pending.params)
                        .unwrap_or_else(|_| "Cannot display request".into());
                    let related = self
                        .selected
                        .as_ref()
                        .and_then(|id| self.sessions.get(id))
                        .and_then(|session| {
                            pending
                                .params
                                .get("itemId")
                                .and_then(Value::as_str)
                                .and_then(|id| session.entries.iter().find(|entry| entry.id == id))
                        })
                        .map(|entry| entry.text.as_str())
                        .unwrap_or_default();
                    let description = format!(
                        "{} · PageUp/Down scroll\n\n{}\n\n{}",
                        pending.method, content, related
                    );
                    let text_input = !matches!(
                        pending.method.as_str(),
                        "item/commandExecution/requestApproval"
                            | "item/fileChange/requestApproval"
                            | "item/permissions/requestApproval"
                    );
                    let bottom = if text_input { 7 } else { 5 };
                    let rows = wrapped(&description, area.width);
                    let height = area.height.saturating_sub(bottom);
                    *scroll = (*scroll).min(rows.len().saturating_sub(height as usize));
                    frame.render_widget(
                        Paragraph::new(
                            rows.into_iter()
                                .skip(*scroll)
                                .take(height as usize)
                                .map(Line::from)
                                .collect::<Vec<_>>(),
                        ),
                        Rect::new(area.x, area.y, area.width, height),
                    );
                    if text_input && let Some(answer) = answers.first() {
                        editor(
                            frame,
                            Rect::new(area.x, area.bottom().saturating_sub(7), area.width, 5),
                            "Structured response JSON",
                            answer,
                            true,
                        );
                    }
                    let choices = if pending.method == "item/permissions/requestApproval" {
                        vec!["Allow requested permissions for this turn", "Deny"]
                    } else if text_input {
                        vec!["Submit · Ctrl+Enter"]
                    } else {
                        vec!["Allow once", "Allow for session", "Decline", "Cancel"]
                    };
                    for (index, label) in choices.iter().enumerate() {
                        deferred.push((
                            Rect::new(
                                area.x,
                                area.bottom().saturating_sub(choices.len() as u16) + index as u16,
                                area.width,
                                1,
                            ),
                            format!("{} {label}", if *selected == index { ">" } else { " " }),
                            Action::Approve(index),
                            *selected == index,
                        ));
                    }
                }
            }
            Some(
                Modal::AgentDefaults(_)
                | Modal::Voice { .. }
                | Modal::Commands { .. }
                | Modal::Status
                | Modal::Usage { .. }
                | Modal::Pending { .. }
                | Modal::Queue { .. }
                | Modal::QueuedEdit { .. },
            )
            | None => {}
        }
        for (rect, label, action, selected) in deferred {
            if rect.y < area.bottom() {
                self.button(frame, rect, &label, action, selected);
            }
        }
        if let Some((notice, error)) = &self.notice
            && *error
        {
            frame.render_widget(
                Paragraph::new(crate::model::clean(notice)).style(Style::default().fg(RED)),
                Rect::new(
                    rect.x + 1,
                    rect.bottom().saturating_sub(1),
                    rect.width.saturating_sub(2),
                    1,
                ),
            );
        }
    }
}
fn agent_help(query: &str) -> Vec<(&'static str, &'static str)> {
    let query = query.to_lowercase();
    [
        ("⌥+1 / ⌥+2", "Switch Agents / Reviews"),
        ("n", "Launch a new coding agent"),
        (
            "↑ / ↓",
            "Select sessions, transcript messages, tools or changed lines",
        ),
        ("Enter on a tool", "Expand or collapse arguments and output"),
        (
            "PageUp / PageDown / wheel",
            "Scroll transcript freely; End returns to live output",
        ),
        (
            "Enter / Esc",
            "Open session / interrupt agent and send waiting messages; go back when idle",
        ),
        ("Cmd+K", "Search sessions, pull requests and commands"),
        (
            "Tab / Shift+Tab",
            "Cycle sessions, chat or tree/diff, and helper canvas",
        ),
        ("Drag · Ctrl+C / Cmd+C", "Select and copy conversation text"),
        (
            "Ctrl+V / Cmd+V",
            "Paste text or media; expand a pasted-content token at the cursor",
        ),
        ("Alt+[", "Show or hide agents list (saved)"),
        ("Alt+]", "Hide or reopen the selected helper canvas"),
        (
            "Alt+P",
            "Move shell / artifact between side pane and main area",
        ),
        ("Alt+D", "Open or close Changes with file tree"),
        (
            "Enter in composer",
            "Send message; steer active turn immediately",
        ),
        (
            "Ctrl+Enter",
            "Queue message for the next turn / submit launch or answers",
        ),
        (
            "Alt+↑ / ↓",
            "Next / previous question; previous from first returns to input",
        ),
        ("Ctrl+]", "Skip the current question without answering"),
        (
            "n (question choice)",
            "Add a note beneath the selected answer; Enter submits both",
        ),
        (
            "/ · $ · @",
            "Commands, workspace skills, and file suggestions above input",
        ),
        ("Shift+Enter", "Insert a newline in the composer"),
        (
            "Hold Space",
            "Voice dictation when enabled with /voice (macOS)",
        ),
        (
            "Shift+arrows",
            "Select text in inputs; type or delete to replace",
        ),
        ("Cmd+A / Cmd+C", "Select all / copy selected input text"),
        ("Cmd+Z", "Undo the last edit in any text input"),
        ("Alt+Backspace", "Delete the previous word in a text input"),
        (
            "Cmd+Backspace / Ctrl+U",
            "Delete input line; Ctrl+U clears filters",
        ),
        (
            "Cmd+← / → · Ctrl+A/E",
            "Move to the start / end of the input line",
        ),
        ("Shift+Alt+← / →", "Extend input selection by a word"),
        ("i", "Focus message composer"),
        (
            "/",
            "Search session commands / skills; /actions opens session controls",
        ),
        ("r", "Reconnect / refresh sessions and changes"),
        ("← / →", "Scroll Changes horizontally"),
        (
            "Cmd+↑ / ↓",
            "Focus and navigate message blocks; scroll ten lines in Changes",
        ),
        ("Shift+↑ / ↓", "Select lines in Changes"),
        ("c / Cmd+C", "Copy Changes line or selection"),
        ("Home / End", "Jump to start / follow latest conversation"),
        (
            "↑ / ↓",
            "Up in empty input browses prompts; Cmd+Up enters messages",
        ),
        ("?", "Search this shortcut list"),
        (
            "Ctrl+C",
            "Copy chat selection; otherwise close difu (agents keep running)",
        ),
    ]
    .into_iter()
    .filter(|(key, description)| {
        format!("{key} {description}")
            .to_lowercase()
            .contains(&query)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use ratatui::{Terminal, backend::TestBackend};
    fn state(storage: Storage) -> Ui {
        let mut ui = Ui::new(storage, &Config::default());
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Implement task".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.note("agentMessage", "A streamed response with useful context");
        ui.summaries.push(session.summary());
        ui.sessions.insert(session.id.clone(), session);
        ui.select("one".into());
        ui
    }
    #[test]
    fn session_pins_persist_sort_first_and_obey_archive_filter() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut ui = state(storage.clone());
        let mut other = ui.summaries.first().context("session")?.clone();
        other.id = "two".into();
        other.title = "Pinned task".into();
        ui.summaries.push(other);
        ui.toggle_pin("two");
        assert_eq!(ui.filtered().first().map(|s| s.id.as_str()), Some("two"));
        let restored = Ui::new(storage.clone(), &storage.load_config()?);
        assert!(restored.pinned_sessions.contains("two"));
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(screen.contains("◆ Pinned task"));
        assert!(!screen.contains("f Filter"));
        ui.summaries.last_mut().context("other")?.archived = true;
        assert!(ui.filtered().iter().all(|s| s.id != "two"));
        ui.archived = true;
        assert_eq!(ui.filtered().first().map(|s| s.id.as_str()), Some("two"));
        ui.toggle_pin("two");
        assert!(!storage.load_config()?.pinned_sessions.contains("two"));
        Ok(())
    }

    #[test]
    fn entering_sessions_follows_latest_without_resetting_in_session_scrolling() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        for index in 0..60 {
            session.note("agentMessage", format!("History message {index:02}"));
        }
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert("unsent draft");
        for entry in 0..5 {
            ui.drilled = entry == 1 || entry == 3;
            ui.focus = Focus::List;
            let p = ui.positions.get_mut("one").context("position")?;
            p.follow = false;
            p.conversation = 0;
            p.focused_entry = ui
                .sessions
                .get("one")
                .and_then(|s| s.entries.first())
                .map(|e| e.id.clone());
            p.scroll_anchor = p.focused_entry.clone().map(|id| (id, 0));
            p.keep_transcript_position = true;
            match entry {
                0 | 1 => ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                2 => ui.action(Action::Open),
                3 => ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
                _ => {
                    ui.select("other".into());
                    ui.action(Action::Select("one".into()));
                }
            }
            assert!(ui.positions.get("one").context("position")?.follow);
            let (screen, _) = draw(&mut ui, 120, 25)?;
            assert!(screen.contains("History message 59"), "entry path {entry}");
            assert!(!screen.contains("History message 00"));
        }
        ui.drilled = true;
        ui.focus = Focus::Conversation;
        ui.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 120, 25)?;
        assert!(screen.contains("History message 00"));
        // New transcript data and returning from the composer must not override reading position.
        ui.sessions
            .get_mut("one")
            .context("session")?
            .note("agentMessage", "Newest background update");
        let (screen, _) = draw(&mut ui, 120, 25)?;
        assert!(screen.contains("History message 00"));
        assert!(!screen.contains("Newest background update"));
        assert!(!ui.positions.get("one").context("position")?.follow);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "unsent draft"
        );
        Ok(())
    }
    #[test]
    fn message_is_queued_before_reply_and_only_enters_chat_after_tool_completion() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (release, hold) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            let request: Request = serde_json::from_str(&line)?;
            assert!(matches!(
                request,
                Request::Control {
                    control: Control::Message { queue: false, .. },
                    ..
                }
            ));
            hold.recv_timeout(Duration::from_secs(5))?;
            serde_json::to_writer(&mut stream, &Reply::Ok)?;
            stream.write_all(b"\n")?;
            Ok(())
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.turn_id = Some("t".into());
        session.status = Status::Running;
        session.entries.push(Entry {
            id: "running-tool".into(),
            kind: "commandExecution".into(),
            started_at: Some(1),
            ..Entry::default()
        });
        ui.positions.get_mut("one").context("position")?.draft =
            Editor::from("follow up immediately");
        ui.send(false);
        // No service reply has been permitted yet.
        let position = ui.positions.get("one").context("position")?;
        assert!(!position.outgoing.is_empty());
        let session = ui.sessions.get("one").context("session")?;
        let rows = transcript::waiting_messages(session, 150, &position.outgoing);
        assert_eq!(
            rows.iter()
                .filter(|l| l.to_string().contains("follow up immediately"))
                .count(),
            1
        );
        assert!(
            !session
                .entries
                .iter()
                .any(|e| e.text == "follow up immediately")
        );
        // A canonical snapshot can arrive before the request acknowledgement.
        let mut canonical = session.clone();
        canonical.note("sending", "follow up immediately");
        canonical.entries.last_mut().context("message")?.data =
            serde_json::json!({"difuSteeringTurn":"t"});
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(canonical.clone()))),
        })?;
        ui.tick(false);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .outgoing
                .is_empty()
        );
        release.send(())?;
        let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        ui.sender.send(response)?;
        ui.tick(false);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .text()
                .is_empty()
        );
        canonical.entries.last_mut().context("message")?.kind = "userMessage".into();
        canonical.finish_steering_wait();
        canonical.touch();
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(canonical.clone()))),
        })?;
        ui.tick(false);
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert_eq!(screen.matches("follow up immediately").count(), 1);
        assert!(screen.contains("Messages to be submitted after the next tool call"));
        let message_id = &canonical.entries.last().context("message")?.id;
        assert!(!ui.conversation_sections.iter().any(|s| &s.id == message_id));
        super::super::engine::apply_event(
            &mut canonical,
            &serde_json::json!({"method":"item/completed","params":{"item":{
                "id":"running-tool","type":"commandExecution"
            }}}),
        );
        canonical.touch();
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(canonical))),
        })?;
        ui.tick(false);
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert_eq!(screen.matches("follow up immediately").count(), 1);
        assert!(!screen.contains("Messages to be submitted after the next tool call"));
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }

    #[test]
    fn sends_without_tools_appear_before_acknowledgement_and_reconcile_once() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (release, hold) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            hold.recv_timeout(Duration::from_secs(5))?;
            serde_json::to_writer(&mut stream, &Reply::Ok)?;
            stream.write_all(b"\n")?;
            Ok(())
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        session.status = Status::Idle;
        session.turn_id = None;
        ui.positions.get_mut("one").context("position")?.draft =
            Editor::from("Show this immediately");
        ui.send(false);
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert!(screen.contains("› Show this immediately"));
        assert!(!screen.contains("Preparing session…"));
        assert!(!screen.contains("Messages to be submitted"));
        assert!(
            ui.conversation_sections
                .iter()
                .any(|s| s.id == "difu-outgoing-0")
        );

        release.send(())?;
        let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        ui.sender.send(response)?;
        ui.tick(false);
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert_eq!(screen.matches("Show this immediately").count(), 1);

        // A canonical send replaces the placeholder even before Codex acknowledges it.
        let mut canonical = ui.sessions.get("one").context("session")?.clone();
        canonical.status = Status::Running;
        canonical.turn_id = Some("t".into());
        canonical.note("sending", "Show this immediately");
        canonical.entries.last_mut().context("message")?.data =
            serde_json::json!({"difuSteeringTurn":"t"});
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(canonical))),
        })?;
        ui.tick(false);
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert_eq!(screen.matches("Show this immediately").count(), 1);
        assert!(screen.contains("› Show this immediately"));
        assert!(!screen.contains("Messages to be submitted"));
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .outgoing
                .is_empty()
        );
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }

    #[test]
    fn optimistic_steering_moves_to_chat_when_tools_finish_before_send_ack() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.status = Status::Running;
        session.turn_id = Some("t".into());
        session.entries = vec![Entry {
            id: "tool".into(),
            kind: "commandExecution".into(),
            started_at: Some(1),
            ..Entry::default()
        }];
        let position = ui.positions.get_mut("one").context("position")?;
        position.follow = true;
        position.outgoing = vec![
            PendingSend {
                text: "Follow up".into(),
                queued: false,
                observed_before: 0,
            },
            PendingSend {
                text: "Next turn".into(),
                queued: true,
                observed_before: 0,
            },
        ];
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert!(screen.contains("↳ Follow up"));
        assert!(!screen.contains("› Follow up"));
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.first_mut().context("tool")?.finished_at = Some(2);
        session.touch();
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert_eq!(screen.matches("Follow up").count(), 1);
        assert!(screen.contains("› Follow up"));
        assert!(screen.contains("↳ Next turn"));
        assert!(!screen.contains("Messages to be submitted after the next tool call"));
        // A reused optimistic slot must not display cached text from the previous send.
        ui.positions
            .get_mut("one")
            .context("position")?
            .outgoing
            .first_mut()
            .context("outgoing")?
            .text = "Another follow up".into();
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert!(screen.contains("› Another follow up"));
        assert!(!screen.contains("› Follow up"));
        Ok(())
    }

    #[test]
    fn queue_snapshots_reconcile_repeated_messages_and_failed_sends_keep_the_draft() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let p = ui.positions.get_mut("one").context("position")?;
        p.outgoing = vec![
            PendingSend {
                text: "again".into(),
                queued: true,
                observed_before: 0,
            },
            PendingSend {
                text: "again".into(),
                queued: false,
                observed_before: 1,
            },
        ];
        let mut snapshot = ui.sessions.get("one").context("session")?.clone();
        snapshot.queue.push("again".into());
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(snapshot.clone()))),
        })?;
        ui.tick(false);
        assert_eq!(
            ui.positions.get("one").context("position")?.outgoing.len(),
            1
        );
        snapshot.note("userMessage", "again");
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(snapshot))),
        })?;
        ui.tick(false);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .outgoing
                .is_empty()
        );
        let p = ui.positions.get_mut("one").context("position")?;
        p.draft = Editor::from("retry this");
        p.outgoing.push(PendingSend {
            text: "retry this".into(),
            queued: false,
            observed_before: 0,
        });
        ui.busy = true;
        ui.sender.send(ResultMessage {
            kind: Task::Send("one".into(), "retry this".into(), vec![]),
            result: Err("Disconnected".into()),
        })?;
        ui.tick(false);
        let p = ui.positions.get("one").context("position")?;
        assert!(p.outgoing.is_empty());
        assert_eq!(p.draft.text(), "retry this");
        assert!(!ui.busy);
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| *error && text == "Disconnected")
        );
        Ok(())
    }

    #[test]
    fn escape_interrupts_active_chat_even_while_busy_and_preserves_draft() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request: Request = serde_json::from_str(&line)?;
                serde_json::to_writer(&mut stream, &Reply::Ok)?;
                stream.write_all(b"\n")?;
                tx.send(request)?;
            }
            Ok(())
        });
        for status in [Status::Starting, Status::Running, Status::Waiting] {
            for focus in [Focus::Composer, Focus::Conversation] {
                let mut ui = state(storage.clone());
                ui.drilled = true;
                ui.focus = focus;
                ui.busy = true;
                ui.sessions.get_mut("one").context("session")?.status = status;
                ui.positions
                    .get_mut("one")
                    .context("position")?
                    .draft
                    .insert("Keep this draft");
                // Dismissing a dialog still takes precedence over interruption.
                ui.modal = Some(Modal::Status);
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(ui.modal.is_none());
                assert!(ui.interrupting.is_empty());
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(matches!(
                    rx.recv_timeout(Duration::from_secs(5))?,
                    Request::Control { id, control: Control::Interrupt } if id == "one"
                ));
                assert!(ui.drilled);
                assert_eq!(ui.focus, focus);
                assert_eq!(
                    ui.positions.get("one").context("position")?.draft.text(),
                    "Keep this draft"
                );
                assert!(ui.interrupting.contains("one"));
                let reply = ui.receiver.recv_timeout(Duration::from_secs(5))?;
                assert!(matches!(reply.kind, Task::Interrupt(_)));
                // Completion must not close a newly opened dialog or clear another request's busy flag.
                ui.modal = Some(Modal::Status);
                ui.sender.send(reply)?;
                ui.tick(false);
                assert!(ui.interrupting.is_empty());
                assert!(ui.busy);
                assert!(matches!(ui.modal, Some(Modal::Status)));
            }
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }

    #[test]
    fn escape_sends_waiting_messages_without_leaving_chat() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            let request: Request = serde_json::from_str(&line)?;
            serde_json::to_writer(&mut stream, &Reply::Ok)?;
            stream.write_all(b"\n")?;
            tx.send(request)?;
            Ok(())
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.sessions
            .get_mut("one")
            .context("session")?
            .queue
            .push("Next task".into());
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5))?,
            Request::Control {
                control: Control::InterruptAndSend,
                ..
            }
        ));
        assert!(ui.drilled);
        assert_eq!(ui.focus, Focus::Composer);
        ui.sessions.get_mut("one").context("session")?.queue.clear();
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.drilled);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }

    #[test]
    fn inline_attachment_send_preserves_text_order_without_appending_duplicate_labels() -> Result<()>
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request: Request = serde_json::from_str(&line)?;
                serde_json::to_writer(&mut stream, &Reply::Ok)?;
                stream.write_all(b"\n")?;
                tx.send(request)?;
            }
            Ok(())
        });
        let mut ui = state(storage);
        for queue in [false, true] {
            let p = ui.positions.get_mut("one").context("position")?;
            p.draft = Editor::from("compare  please");
            p.draft.cursor = 8;
            p.draft.insert_attachment("[image 1]");
            let attachment = super::super::media::Attachment {
                label: "image 1".into(),
                path: "image.png".into(),
                kind: super::super::media::Kind::Image,
                hash: "image".into(),
            };
            p.attachments = vec![attachment.clone()];
            ui.busy = false;
            ui.send(queue);
            let request = rx.recv_timeout(Duration::from_secs(5))?;
            let Request::Control {
                control:
                    Control::MessageWithAttachments {
                        text,
                        attachments,
                        queue: actual_queue,
                        ..
                    },
                ..
            } = request
            else {
                anyhow::bail!("Expected attachment message");
            };
            assert_eq!(text, "compare [image 1] please");
            assert_eq!(attachments, [attachment]);
            assert_eq!(actual_queue, queue);
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }

    #[test]
    fn attachment_tokens_use_composer_rows_and_persist_only_undeleted_media() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let attachment = super::super::media::Attachment {
            label: "image 1".into(),
            path: "image.png".into(),
            kind: super::super::media::Kind::Image,
            hash: "image".into(),
        };
        super::super::media::save_draft(&storage, "one", std::slice::from_ref(&attachment))?;
        let mut ui = state(storage.clone());
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert_eq!(screen.matches("[image 1]").count(), 1);
        assert!(!screen.contains("[image 1] ×"));
        let height = ui.conversation_height;
        let position = ui.positions.get_mut("one").context("position")?;
        assert_eq!(position.draft.text(), "[image 1]");
        position
            .draft
            .key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        ui.tick_media();
        assert!(super::super::media::load_draft(&storage, "one")?.is_empty());
        draw(&mut ui, 120, 40)?;
        assert_eq!(
            ui.conversation_height, height,
            "attachments have no separate row"
        );
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .undo();
        ui.tick_media();
        assert_eq!(
            super::super::media::load_draft(&storage, "one")?,
            [attachment]
        );
        Ok(())
    }

    #[test]
    fn submitted_messages_keep_the_same_rendering_through_delivery_states() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let session = ui.sessions.get_mut("one").context("session")?;
        session.note(
            "awaiting connection",
            "A submitted message that wraps across multiple lines",
        );
        let entry_id = session.entries.last().context("message")?.id.clone();
        let position = Position {
            focused_entry: Some(entry_id.clone()),
            ..Position::default()
        };
        for focused in [false, true] {
            let mut previous = None;
            for kind in ["awaiting connection", "sending", "userMessage"] {
                session.entries.last_mut().context("message")?.kind = kind.into();
                let (rows, sections) = transcript::render(session, &position, 32, focused);
                let rendered = rows
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(!rendered.contains("Tool activity"));
                assert!(!rendered.contains("awaiting connection"));
                assert!(!rendered.contains("sending"));
                assert_eq!(rendered.matches("A submitted message").count(), 1);
                assert!(!sections.last().context("message section")?.tool);
                assert_eq!(sections.last().context("message section")?.id, entry_id);
                if let Some(previous) = &previous {
                    assert_eq!(&rows, previous);
                }
                previous = Some(rows);
            }
        }
        Ok(())
    }
    #[test]
    fn tool_previews_are_bounded_and_open_a_scrollable_modal() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Conversation;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        for (id, kind, text, data) in [
            (
                "json",
                "mcpToolCall",
                format!("{{\"output\":\"{}\"}}", "escaped\\ntext ".repeat(20000)),
                serde_json::json!({"server":"docs","tool":"read"}),
            ),
            (
                "command",
                "commandExecution",
                (0..100).map(|i| format!("output {i}\n")).collect(),
                serde_json::json!({"command":"echo hello\n".repeat(10000)}),
            ),
            (
                "patch",
                "fileChange",
                String::new(),
                serde_json::json!({"changes":[{"path":"a.rs","diff":format!("@@ -1 +1,10000 @@\n{}","+let value = 1;\n".repeat(10000))}]}),
            ),
        ] {
            session.entries.push(Entry {
                id: id.into(),
                kind: kind.into(),
                text,
                data,
                ..Entry::default()
            });
        }
        session.touch();
        let (lines, sections) = transcript::render(session, &Position::default(), 60, false);
        assert_eq!(sections.len(), 3);
        assert!(
            lines.len() <= 18,
            "preview must cap visual rows across JSON, commands and patches"
        );
        ui.positions.entry("one".into()).or_default().focused_entry = Some("command".into());
        draw(&mut ui, 100, 40)?;
        let before = ui.positions.get("one").context("position")?.conversation;
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        draw(&mut ui, 100, 40)?;
        assert!(
            matches!(ui.modal,Some(Modal::Transcript {ref rows, scroll:0,..}) if rows.len()>100)
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(
            ui.modal,
            Some(Modal::Transcript { scroll: 1, .. })
        ));
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert!(
            matches!(ui.modal,Some(Modal::Transcript {scroll,ref rows,height,..}) if scroll==rows.len()-height)
        );
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        draw(&mut ui, 100, 40)?;
        assert!(ui.modal.is_none());
        assert_eq!(
            ui.positions.get("one").context("position")?.conversation,
            before
        );
        Ok(())
    }
    #[test]
    fn transcript_virtualization_formats_only_nearby_messages_and_keeps_history() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        session.status = Status::Idle;
        for i in 0..10000 {
            session.note(
                "agentMessage",
                format!("Message {i}\n\nSecond paragraph {i}"),
            );
        }
        let mut position = Position {
            follow: true,
            ..Position::default()
        };
        let mut window = transcript_window::Window::default();
        let tail = window.render(session, &mut position, 80, 25, false);
        assert_eq!(session.entries.len(), 10000);
        assert!(
            window.formatted <= 20,
            "cold draw must not format all history"
        );
        assert!(
            tail.lines
                .iter()
                .any(|l| l.to_string().contains("Message 9999"))
        );
        let measured = window.formatted;
        window.render(session, &mut position, 80, 25, false);
        assert_eq!(
            window.formatted, measured,
            "stationary repaint must reuse formatted rows"
        );
        let reference = transcript::render(session, &Position::default(), 80, false).0;
        let mut expected = reference.len().saturating_sub(25);
        position.follow = false;
        for distance in [1, 3, 25, 25, 25, 3, 25] {
            position.conversation = position.conversation.saturating_sub(distance);
            position.scroll_anchor = None;
            expected = expected.saturating_sub(distance);
            let view = window.render(session, &mut position, 80, 25, false);
            let actual = view
                .lines
                .iter()
                .skip(position.conversation.saturating_sub(view.start))
                .take(25)
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let expected_rows = reference
                .iter()
                .skip(expected)
                .take(25)
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert_eq!(
                actual, expected_rows,
                "paging across an unmeasured boundary must preserve exact rows"
            );
        }
        let measured = window.formatted;
        position.follow = false;
        position.scroll_anchor = Some((session.entries.get(4999).context("entry")?.id.clone(), 1));
        let middle = window.render(session, &mut position, 80, 25, false);
        assert!(window.formatted - measured <= 20);
        let reference_sections = transcript::render(session, &Position::default(), 80, false).1;
        let source_id = session.entries.get(4999).context("entry")?.id.clone();
        let mut expected = reference_sections
            .iter()
            .find(|s| s.id == source_id)
            .context("reference")?
            .row
            + 1;
        for distance in [3, 25, 25, 25, 25, 25] {
            position.conversation += distance;
            position.scroll_anchor = None;
            expected += distance;
            let view = window.render(session, &mut position, 80, 25, false);
            assert_eq!(
                view.lines
                    .iter()
                    .skip(position.conversation.saturating_sub(view.start))
                    .take(25)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                reference
                    .iter()
                    .skip(expected)
                    .take(25)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                "paging forward must measure relative to the preceding known boundary"
            );
        }
        let anchor = position.scroll_anchor.clone();
        window.render(session, &mut position, 40, 25, false);
        assert_eq!(
            position.scroll_anchor, anchor,
            "resize must retain source message and row"
        );
        assert!(
            !middle
                .lines
                .iter()
                .any(|l| l.to_string().contains("Message 9999"))
        );
        position.scroll_anchor = None;
        position.conversation = 0;
        let first = window.render(session, &mut position, 80, 25, false);
        assert!(
            first
                .lines
                .iter()
                .any(|l| l.to_string().contains("Message 0"))
        );
        position.follow = true;
        session.note("agentMessage", "New streamed response");
        let tail = window.render(session, &mut position, 80, 25, false);
        assert!(
            tail.lines
                .iter()
                .any(|l| l.to_string().contains("New streamed response"))
        );
        Ok(())
    }
    #[test]
    fn toasts_expire_after_five_seconds_and_new_notices_restart_timer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let now = Instant::now();
        ui.notice = Some(("Success".into(), false));
        ui.expire_toast(now);
        ui.expire_toast(now + Duration::from_millis(4999));
        assert!(ui.notice.is_some());
        ui.expire_toast(now + Duration::from_secs(5));
        assert!(ui.notice.is_none());
        ui.notice = Some(("Error".into(), true));
        ui.expire_toast(now + Duration::from_secs(6));
        ui.notice = Some(("Another error".into(), true));
        ui.expire_toast(now + Duration::from_secs(9));
        ui.expire_toast(now + Duration::from_secs(13));
        assert!(ui.notice.is_some());
        ui.expire_toast(now + Duration::from_secs(14));
        assert!(ui.notice.is_none());
        Ok(())
    }
    #[test]
    fn resource_controls_are_compact_and_notices_overlay_without_reserving_footer_rows()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let (empty, _) = draw(&mut ui, 120, 40)?;
        let height = ui.conversation_height;
        assert!(!empty.contains("Shells (0)") && !empty.contains("Artifacts (0)"));
        assert!(!empty.contains("Input · Tab Sessions"));
        ui.panels.shells.insert(
            "one".into(),
            vec![serde_json::json!({"itemId":"shell", "command":"sleep 30"})],
        );
        let (with_shell, _) = draw(&mut ui, 120, 40)?;
        assert!(with_shell.contains("Shells (1)"));
        assert!(!with_shell.contains("Artifacts (0)"));
        assert_eq!(ui.conversation_height + 1, height);
        ui.notice = Some(("Action accepted".into(), false));
        let (with_notice, _) = draw(&mut ui, 120, 40)?;
        assert_eq!(with_notice.matches("Action accepted").count(), 1);
        assert_eq!(ui.conversation_height + 1, height);
        assert!(ui.toast_rect.x > 40 && ui.toast_rect.y > 25);
        let rect = ui.toast_rect;
        ui.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        assert!(ui.notice.is_none());
        assert_eq!(ui.focus, Focus::Composer);
        Ok(())
    }
    #[test]
    fn keyboard_reaches_only_visible_resource_controls_and_preserves_draft() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        draw(&mut ui, 120, 40)?;
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("Keep this draft");
        ui.panels.shells.insert(
            "one".into(),
            vec![serde_json::json!({"itemId":"shell", "command":"sleep 30"})],
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Resources(false));
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Resources(false));
        ui.sessions
            .get_mut("one")
            .context("session")?
            .artifacts
            .push(super::super::artifacts::Artifact {
                workspace: None,
                path: "report.html".into(),
                title: "Report".into(),
            });
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Resources(true));
        ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Resources(false));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            ui.modal,
            Some(Modal::Resources {
                artifacts: false,
                ..
            })
        ));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Keep this draft"
        );
        Ok(())
    }
    #[test]
    fn suggestions_require_acceptance_and_never_replace_a_draft_or_cross_turns() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.note("userMessage", "Plan the change");
        let prompt = session.entries.last().context("prompt")?.id.clone();
        session.completed_turn = Some("completed".into());
        session.suggestion = Some(super::super::suggestions::Suggestion {
            turn: "completed".into(),
            prompt,
            text: "Okay, implement the plan.".into(),
        });
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(screen.contains("Okay, implement the plan."));
        assert!(!screen.contains("Message · Enter"));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!ui.busy);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .chars
                .is_empty()
        );
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .text()
                .is_empty()
        );
        assert_ne!(ui.focus, Focus::Composer);
        assert!(!ui.busy);
        ui.focus = Focus::Composer;
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Okay, implement the plan."
        );
        assert_eq!(ui.focus, Focus::Composer);
        assert!(!ui.busy);
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("My own text");
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "My own text"
        );
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .clear();
        ui.sessions
            .get_mut("one")
            .context("session")?
            .note("userMessage", "Different task");
        assert!(ui.composer_suggestion().is_none());
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(screen.contains("Ask anything…"));
        assert!(!screen.contains("Okay, implement the plan."));
        Ok(())
    }
    #[test]
    fn inline_commands_accept_search_arguments_and_reject_unsupported_arguments() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.skills.insert("one".into(), (Vec::new(), Vec::new()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        for (command, query) in [
            ("help copy", "copy"),
            ("actions rename", "rename"),
            ("skills rust", "rust"),
        ] {
            ui.modal = None;
            ui.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
            ui.paste(command);
            ui.key(enter);
            let actual = match &ui.modal {
                Some(Modal::Help(state)) => state.query.text(),
                Some(
                    Modal::Menu { query, .. }
                    | Modal::Commands {
                        query,
                        skills_only: true,
                        ..
                    },
                ) => query.text(),
                _ => anyhow::bail!("Wrong result for {command}"),
            };
            assert_eq!(actual, query);
            assert!(!ui.busy);
        }
        for command in [
            "compact unexpected",
            "status unexpected",
            "diff unexpected",
            "new unexpected",
            "voice maybe",
            "model one high extra",
            "effort high extra",
        ] {
            ui.open_commands(false);
            ui.paste(command);
            ui.key(enter);
            assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
            assert!(
                matches!(&ui.modal, Some(Modal::Commands { query, .. }) if query.text() == command)
            );
            assert!(!ui.busy);
        }
        ui.open_commands(false);
        ui.paste("rename");
        ui.key(enter);
        assert!(matches!(ui.modal, Some(Modal::Rename(_))));
        // A pasted slash command uses the same dispatcher, never the message transport.
        ui.modal = None;
        ui.paste("/help keyboard");
        ui.key(enter);
        assert!(matches!(&ui.modal, Some(Modal::Help(state)) if state.query.text() == "keyboard"));
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .chars
                .is_empty()
        );
        assert!(!ui.busy);
        Ok(())
    }
    #[test]
    fn inline_rename_model_and_effort_send_their_arguments_to_the_service() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let socket = super::super::server::socket(&storage)?;
        let listener = UnixListener::bind(socket)?;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request: Request = serde_json::from_str(&line)?;
                serde_json::to_writer(&mut stream, &Reply::Ok)?;
                stream.write_all(b"\n")?;
                tx.send(request)?;
            }
            Ok(())
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.skills.insert("one".into(), (Vec::new(), Vec::new()));
        ui.sessions.get_mut("one").context("session")?.model = Some("fixture-current".into());
        for (i, command) in [
            "rename Lorem / Hello 🦀",
            "model fixture-luna medium",
            "effort high",
        ]
        .into_iter()
        .enumerate()
        {
            ui.open_commands(false);
            ui.paste(command);
            ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            let request = rx.recv_timeout(Duration::from_secs(5))?;
            match (i, request) {
                (0, Request::Rename { id, title }) => {
                    assert_eq!(id, "one");
                    assert_eq!(title, "Lorem / Hello 🦀");
                }
                (
                    1,
                    Request::Control {
                        id,
                        control: Control::Model { model, effort },
                    },
                ) => {
                    assert_eq!(id, "one");
                    assert_eq!(model.as_deref(), Some("fixture-luna"));
                    assert_eq!(effort.as_deref(), Some("medium"));
                }
                (
                    2,
                    Request::Control {
                        id,
                        control: Control::Model { model, effort },
                    },
                ) => {
                    assert_eq!(id, "one");
                    assert_eq!(model.as_deref(), Some("fixture-current"));
                    assert_eq!(effort.as_deref(), Some("high"));
                }
                (_, other) => anyhow::bail!("Unexpected request: {other:?}"),
            }
            ui.busy = false;
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }
    #[test]
    fn main_changes_tree_diff_and_canvas_share_tab_navigation_without_losing_chat() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.paste("Keep my draft");
        let patch = "diff --git a/src/first.rs b/src/first.rs\n--- a/src/first.rs\n+++ b/src/first.rs\n@@ -1 +1 @@\n-old\n+let first = 1;\ndiff --git a/src/second.rs b/src/second.rs\n--- a/src/second.rs\n+++ b/src/second.rs\n@@ -1 +1 @@\n-old\n+let second = 2;\n";
        ui.receive_changes("one".into(), patch.into());
        ui.panels.right = true;
        ui.panels.view = Some(panels::View::Shell {
            id: "shell".into(),
            command: "shell output".into(),
        });
        ui.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        assert!(ui.changes_visible && ui.panels.view.is_some());
        assert_eq!(ui.focus, Focus::ChangeTree);
        let (screen, _) = draw(&mut ui, 160, 40)?;
        assert!(screen.contains("Files"));
        assert!(screen.contains("first.rs") && screen.contains("second.rs"));
        assert!(screen.contains("let first = 1;"));
        assert!(!screen.contains("diff --git"));
        assert!(!screen.contains("Keep my draft"));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 160, 40)?;
        assert!(screen.contains("let second = 2;"));
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        ui.key(tab);
        assert_eq!(ui.focus, Focus::Changes);
        ui.key(tab);
        assert!(ui.panels.focused);
        ui.key(tab);
        assert_eq!(ui.focus, Focus::List);
        assert!(!ui.panels.focused);
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::ChangeTree);
        ui.focus = Focus::List;
        ui.toggle_list();
        assert_eq!(ui.focus, Focus::ChangeTree);
        ui.toggle_list();
        ui.focus = Focus::List;
        ui.key(tab);
        assert_eq!(ui.focus, Focus::ChangeTree);
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(ui.panels.focused);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.panels.view.is_none() && ui.changes_visible);
        assert_eq!(ui.focus, Focus::Changes);
        // Refreshing reordered files keeps the selected path rather than its old index.
        let second = patch
            .split("diff --git a/src/second.rs")
            .nth(1)
            .context("second patch")?;
        ui.receive_changes(
            "one".into(),
            format!("diff --git a/src/second.rs{second}{patch}").into(),
        );
        let p = ui.positions.get("one").context("position")?;
        assert_eq!(
            ui.changes
                .get("one")
                .context("changes")?
                .files
                .get(p.change_file)
                .context("file")?
                .path,
            "src/second.rs"
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.changes_visible);
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("draft")?.draft.text(),
            "Keep my draft"
        );
        Ok(())
    }
    #[test]
    fn agent_diff_large_tree_keeps_folder_and_file_focus_during_refresh() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.changes_visible = true;
        ui.focus = Focus::ChangeTree;
        let patches = ["backend", "dashboard", "packages"]
            .into_iter()
            .flat_map(|root| (0..40).map(move |i| (root, i)))
            .map(|(root, i)| {
                let path = format!("{root}/src/file{i:02}.rs");
                format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+let {root}_{i} = 1;\n")
            })
            .collect::<Vec<_>>();
        let original = patches.concat();
        ui.receive_changes("one".into(), original.clone().into());
        let count = ui.changes.get("one").context("changes")?.tree.len();
        ui.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(ui.positions.get("one").context("position")?.change_tree, 10);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(ui.positions.get("one").context("position")?.change_tree, 0);
        ui.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.change_tree,
            ui.viewport
        );
        ui.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        for expected in 0..count {
            if expected > 0 {
                ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            }
            let path = ui
                .changes
                .get("one")
                .context("changes")?
                .tree
                .get(expected)
                .context("tree entry")?
                .path
                .clone();
            // Polling must not snap a selected directory back to the last file.
            ui.receive_changes("one".into(), original.clone().into());
            assert_eq!(
                ui.positions.get("one").context("position")?.change_tree,
                expected,
                "{path}"
            );
            let (screen, _) = draw(&mut ui, 150, 20)?;
            let label = path.rsplit('/').next().context("label")?;
            assert!(
                screen
                    .lines()
                    .any(|line| line.contains('▸') && line.contains(label)),
                "Selected row left viewport: {path}"
            );
        }
        let folder = ui
            .changes
            .get("one")
            .context("changes")?
            .tree
            .iter()
            .position(|e| e.path == "dashboard" && e.file.is_none())
            .context("folder")?;
        ui.select_change(folder);
        // A real edit and reordered patch sections also preserve directory identity.
        let changed = patches
            .iter()
            .rev()
            .cloned()
            .collect::<String>()
            .replace("= 1;", "= 2;");
        ui.receive_changes("one".into(), changed.into());
        assert_eq!(
            ui.positions
                .get("one")
                .context("position")?
                .change_directory
                .as_deref(),
            Some("dashboard")
        );
        assert_eq!(
            ui.changes
                .get("one")
                .context("changes")?
                .tree
                .get(ui.positions.get("one").context("position")?.change_tree)
                .context("selected entry")?
                .path,
            "dashboard"
        );
        let (screen, _) = draw(&mut ui, 150, 20)?;
        assert!(screen.contains("let dashboard_39 = 2;"));
        assert!(!screen.contains("let backend_") && !screen.contains("let packages_"));
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let p = &ui.positions.get("one").context("position")?;
        assert_eq!(
            ui.changes
                .get("one")
                .context("changes")?
                .files
                .get(p.change_file)
                .context("selected file")?
                .path,
            "packages/src/file39.rs"
        );
        ui.receive_changes("one".into(), original.into());
        let p = &ui.positions.get("one").context("position")?;
        assert_eq!(
            ui.changes
                .get("one")
                .context("changes")?
                .files
                .get(p.change_file)
                .context("selected file")?
                .path,
            "packages/src/file39.rs"
        );
        Ok(())
    }
    #[test]
    fn agent_diff_arrows_cross_files_in_tree_order_and_shift_stays_in_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.changes_visible = true;
        let patch = ["z/last.rs", "a/first.rs", "m/middle.rs"].into_iter().map(|path| {
            format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+let value = 1;\n")
        }).collect::<String>();
        ui.receive_changes("one".into(), patch.into());
        let first = ui
            .changes
            .get("one")
            .context("changes")?
            .tree
            .iter()
            .position(|e| e.file == Some(1))
            .context("first")?;
        ui.select_change(first);
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        draw(&mut ui, 150, 20)?;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.positions.get("one").context("position")?.change_file, 1);
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(ui.positions.get("one").context("position")?.change_file, 1);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.positions.get("one").context("position")?.change_file, 2);
        assert_eq!(ui.positions.get("one").context("position")?.changes, 0);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .selection
                .is_none()
        );
        assert_eq!(ui.focus, Focus::Changes);
        draw(&mut ui, 150, 20)?;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.positions.get("one").context("position")?.change_file, 1);
        assert_eq!(
            ui.positions.get("one").context("position")?.changes,
            ui.change_lines - 1
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        draw(&mut ui, 150, 20)?;
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.positions.get("one").context("position")?.change_file, 0);
        draw(&mut ui, 150, 20)?;
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.positions.get("one").context("position")?.change_file, 0);
        Ok(())
    }
    #[test]
    fn tab_cycles_sessions_composer_and_helper_and_escape_closes_only_focused_helper() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::List;
        ui.panels.right = true;
        ui.panels.view = Some(panels::View::Shell {
            id: "shell".into(),
            command: "sleep".into(),
        });
        ui.positions
            .entry("one".into())
            .or_default()
            .draft
            .insert("Unsent draft");
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.panels.scroll = 12;
        for right in [true, false] {
            ui.panels.right = right;
            ui.key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
            assert!(ui.panels.hidden && !ui.panels.focused);
            assert!(!draw(&mut ui, 120, 40)?.0.contains("sleep"));
            ui.key(tab);
            assert!(!ui.panels.focused);
            ui.key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
            assert!(!ui.panels.hidden && ui.panels.focused);
            assert_eq!(ui.panels.scroll, 12);
            assert!(
                matches!(&ui.panels.view, Some(panels::View::Shell { id, .. }) if id == "shell")
            );
        }
        ui.panels.right = true;
        ui.panels.focused = false;
        ui.focus = Focus::List;
        ui.key(tab);
        assert_eq!(ui.focus, Focus::Composer);
        assert!(!ui.panels.focused);
        ui.key(tab);
        assert!(ui.panels.focused);
        let (_, cursor) = draw(&mut ui, 120, 40)?;
        assert_eq!(cursor, Some((0, 0)));
        ui.key(tab);
        assert_eq!(ui.focus, Focus::List);
        assert!(!ui.panels.focused);
        ui.key(back);
        assert!(ui.panels.focused);
        ui.key(back);
        assert_eq!(ui.focus, Focus::Composer);
        assert!(!ui.panels.focused);
        ui.list_visible = false;
        ui.key(tab);
        assert!(ui.panels.focused);
        ui.key(tab);
        assert_eq!(ui.focus, Focus::Composer);
        assert!(!ui.panels.focused);
        ui.key(tab);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.panels.view.is_none());
        assert!(!ui.panels.focused);
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("draft")?.draft.text(),
            "Unsent draft"
        );
        Ok(())
    }
    #[test]
    fn tool_commands_outputs_and_edited_files_have_syntax_colors() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        session.entries.push(Entry { id: "command".into(), kind: "commandExecution".into(), text: "const output = 42;\nsecond\nthird\nfourth".into(), data: serde_json::json!({"command":"python3 - <<'PY'\nfrom pathlib import Path\nPY","exitCode":0}), ..Entry::default() });
        session.entries.push(Entry { id: "edit".into(), kind: "fileChange".into(), data: serde_json::json!({"changes":[{"path":"src/example.rs","diff":"@@ -9 +9,2 @@\n-old\n+let answer = 42;\n+return answer;"}]}), ..Entry::default() });
        let mut position = Position::default();
        position.expanded.insert("edit".into());
        let (rows, sections) = transcript::render(session, &position, 72, false);
        let text = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Edited src/example.rs +2 -1"));
        assert!(text.contains("9 - old"));
        assert!(text.contains("10 + return answer;"));
        assert!(text.contains("Enter/click to expand"));
        for keyword in ["from", "import", "let", "return"] {
            assert!(
                rows.iter()
                    .flat_map(|row| &row.spans)
                    .any(|span| span.content.contains(keyword) && span.style.fg == Some(ACCENT)),
                "Missing syntax color for {keyword}"
            );
        }
        assert_eq!(sections.len(), 2);
        assert!(sections.iter().all(|section| section.tool));
        assert!(
            rows.iter()
                .any(|row| row.style.bg == Some(crate::ui::ADD_BG))
        );
        assert!(
            rows.iter()
                .any(|row| row.style.bg == Some(crate::ui::REMOVE_BG))
        );
        Ok(())
    }
    #[test]
    fn shell_panes_move_without_losing_chat_or_changes_and_remember_placement() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let mut ui = state(storage.clone());
        ui.drilled = true;
        ui.changes_visible = true;
        ui.positions
            .entry("one".into())
            .or_default()
            .draft
            .insert("Keep this draft");
        ui.panels.shells.insert(
            "one".into(),
            vec![serde_json::json!({"itemId":"shell","command":"git status","processId":"7"})],
        );
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.push(Entry {
            id: "shell".into(),
            kind: "commandExecution".into(),
            text: "$ git status\nfresh streamed output".into(),
            data: serde_json::json!({"aggregatedOutput":""}),
            ..Entry::default()
        });
        ui.select_resource(false, 0);
        let (text, _) = draw(&mut ui, 120, 40)?;
        assert!(text.contains("Shell output"));
        assert!(text.contains("fresh streamed output"));
        assert!(ui.panels.right);
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        assert!(!ui.panels.right);
        assert!(!storage.load_config()?.agent_panel_right);
        assert!(
            !Ui::new(storage.clone(), &storage.load_config()?)
                .panels
                .right
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.panels.view.is_none());
        assert_eq!(ui.focus, Focus::Changes);
        assert!(ui.changes_visible);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Keep this draft"
        );
        let (text, _) = draw(&mut ui, 120, 40)?;
        assert!(text.contains("Current worktree changes"));
        ui.menu_action(14);
        let (text, _) = draw(&mut ui, 120, 40)?;
        assert!(text.contains("Stop this agent"));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        Ok(())
    }
    #[test]
    fn optional_browser_prompt_does_not_install_or_change_chat_until_chosen() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        ui.positions
            .entry("one".into())
            .or_default()
            .draft
            .insert("Unsent draft");
        ui.modal = Some(Modal::InstallBrowser {
            path: dir.path().join("report.html"),
            title: "Report".into(),
            selected: 0,
        });
        let (text, _) = draw(&mut ui, 100, 40)?;
        assert!(text.contains("Open in external browser"));
        assert!(!ui.panels.installing);
        assert!(ui.panels.view.is_none());
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        assert!(!ui.panels.installing);
        assert!(ui.panels.view.is_none());
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Unsent draft"
        );
        assert_eq!(ui.selected.as_deref(), Some("one"));
        Ok(())
    }
    #[test]
    fn guides_are_hidden_from_active_and_archived_session_lists() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        let mut guide = ui
            .summaries
            .first()
            .context("Missing fixture summary")?
            .clone();
        guide.id = "guide".into();
        guide.kind = "Guide".into();
        ui.summaries.insert(0, guide);
        ui.selected = None;
        ui.ensure_selected();
        assert_eq!(ui.selected.as_deref(), Some("one"));
        assert_eq!(ui.filtered().len(), 1);
        ui.archived = true;
        for summary in &mut ui.summaries {
            summary.archived = true;
        }
        assert_eq!(ui.filtered().len(), 1);
        assert_eq!(ui.summaries.len(), 2); // Hiding never deletes review-job history.
        Ok(())
    }
    fn draw(ui: &mut Ui, width: u16, height: u16) -> Result<(String, Option<(u16, u16)>)> {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|f| ui.draw(f))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        use ratatui::backend::Backend;
        let cursor = terminal
            .backend_mut()
            .get_cursor_position()
            .ok()
            .map(|p| (p.x, p.y));
        Ok((text, cursor))
    }
    #[test]
    fn default_settings_save_without_changing_existing_sessions() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        };
        let config = Config {
            wrap_diff: true,
            ..Default::default()
        };
        storage.save_config(&config)?;
        let mut ui = state(storage.clone());
        let before = ui.sessions.get("one").context("session")?.model.clone();
        ui.model_completion.options = vec![crate::model::ModelInfo {
            id: "fixture-model".into(),
            name: "Fixture".into(),
            efforts: vec!["medium".into()],
        }];
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("defaults");
        };
        form.fields = vec![
            Editor::from("/tmp/repo"),
            Editor::from("fixture-model"),
            Editor::from("medium"),
        ];
        for (width, height) in [(40, 12), (100, 30)] {
            draw(&mut ui, width, height)?;
        }
        ui.save_defaults();
        let saved = storage.load_config()?;
        assert!(saved.wrap_diff);
        assert!(saved.agent_defaults.isolated);
        assert_eq!(saved.agent_defaults.model.as_deref(), Some("fixture-model"));
        assert_eq!(ui.defaults.effort.as_deref(), Some("medium"));
        assert_eq!(ui.sessions.get("one").context("session")?.model, before);
        Ok(())
    }
    #[test]
    fn prompt_history_is_per_session_editable_and_restores_empty_draft() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.note("userMessage", "first prompt");
        session.note("agentMessage", "not history");
        session.note("userMessage", "second prompt");
        ui.positions.entry("one".into()).or_default();
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "second prompt"
        );
        ui.key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "first prompt"
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "second prompt?"
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            ""
        );
        assert_eq!(ui.focus, Focus::Composer);
        assert!(!ui.busy);
        ui.paste("new draft");
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "new draft"
        );
        ui.selected = Some("two".into());
        ui.positions.entry("two".into()).or_default();
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("two").context("position")?.draft.text(),
            ""
        );
        Ok(())
    }
    #[test]
    fn command_navigation_highlights_the_whole_message_and_retains_draft() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.sessions
            .get_mut("one")
            .context("session")?
            .note("agentMessage", "Latest response\n\nwith a second line");
        ui.paste("keep this draft");
        draw(&mut ui, 120, 35)?;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(ui.focus, Focus::Conversation);
        let id = ui
            .positions
            .get("one")
            .context("position")?
            .focused_entry
            .clone();
        assert_eq!(
            id,
            ui.sessions
                .get("one")
                .context("session")?
                .entries
                .last()
                .map(|e| e.id.clone())
        );
        let session = ui.sessions.get("one").context("session")?;
        let position = ui.positions.get("one").context("position")?;
        let (rows, sections) = transcript::render(session, position, 60, true);
        let start = sections.last().context("last message")?.row;
        for row in rows.iter().skip(start).take(2) {
            assert_eq!(row.width(), 60);
            assert!(
                row.spans
                    .iter()
                    .all(|s| s.style.bg == Some(ratatui::style::Color::Rgb(16, 39, 25)))
            );
        }
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "keep this draft"
        );
        Ok(())
    }
    #[test]
    fn typing_in_messages_returns_to_input_without_stealing_navigation() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.paste("draft ");
        draw(&mut ui, 100, 30)?;
        ui.focus = Focus::Conversation;
        ui.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        assert!(ui.modal.is_none());
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "draft n"
        );
        ui.focus = Focus::Conversation;
        ui.key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "draft nX"
        );
        ui.focus = Focus::Conversation;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Conversation);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        assert!(matches!(ui.modal, Some(Modal::Pending { .. })));
        ui.modal = None;
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::List);
        ui.focus = Focus::Conversation;
        ui.paste(" pasted");
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "draft nX pasted"
        );
        Ok(())
    }
    #[test]
    fn tab_opens_selected_input_and_returns_directly_to_list() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        });
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(ui.drilled);
        assert_eq!(ui.focus, Focus::Composer);
        ui.paste("draft");
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::List);
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "draft"
        );
        Ok(())
    }
    #[test]
    fn user_prompt_remains_inline_with_full_width_padding() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        });
        ui.drilled = true;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.note("userMessage", "Keep this prompt in place");
        session.note("agentMessage", "Following response");
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert_eq!(screen.matches("Keep this prompt in place").count(), 1);
        let session = ui.sessions.get("one").context("session")?;
        let position = ui.positions.get("one").context("position")?;
        let (rows, sections) = transcript::render(session, position, 60, false);
        let id = session
            .entries
            .iter()
            .find(|entry| entry.kind == "userMessage")
            .context("prompt")?
            .id
            .as_str();
        let start = sections
            .iter()
            .find(|section| section.id == id)
            .context("section")?
            .row;
        for row in rows.iter().skip(start).take(3) {
            assert_eq!(row.width(), 60);
            assert_eq!(row.style, crate::ui::user_message_style());
        }
        assert!(
            rows.get(start)
                .context("top padding")?
                .to_string()
                .trim()
                .is_empty()
        );
        assert!(
            rows.get(start + 2)
                .context("bottom padding")?
                .to_string()
                .trim()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn user_prompt_scrolls_away_and_composer_grows_to_ten_lines() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: tmp.path().join("config.json"),
            cache: tmp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let s = ui.sessions.get_mut("one").context("session")?;
        s.note("userMessage", "Newest user prompt");
        s.note(
            "agentMessage",
            (0..80)
                .map(|i| format!("Response line {i}\n\n"))
                .collect::<String>(),
        );
        ui.positions.entry("one".into()).or_default().follow = true;
        let (first, _) = draw(&mut ui, 100, 40)?;
        assert!(!first.contains("Newest user prompt"));
        let height = ui.conversation_height;
        {
            let position = ui.positions.get_mut("one").context("position")?;
            position.follow = false;
            position.conversation = 0;
            position.scroll_anchor = None;
        }
        let (at_start, _) = draw(&mut ui, 100, 40)?;
        assert_eq!(at_start.matches("Newest user prompt").count(), 1);
        assert_eq!(ui.positions.get("one").context("position")?.conversation, 0);
        assert_eq!(ui.conversation_height, height);
        ui.positions.get_mut("one").context("position")?.follow = true;
        draw(&mut ui, 100, 40)?;
        assert!(!first.contains("Latest prompt"));
        let short = ui.conversation_height;
        ui.paste("line\n".repeat(14).as_str());
        draw(&mut ui, 100, 40)?;
        assert_eq!(short.saturating_sub(ui.conversation_height), 9);
        ui.focus = Focus::Conversation;
        ui.scroll(30, false);
        let (scrolled, _) = draw(&mut ui, 100, 40)?;
        assert!(!scrolled.contains("Newest user prompt"));
        assert_eq!(
            crate::ui::user_message_style().fg,
            Some(ratatui::style::Color::White)
        );
        assert_eq!(
            crate::ui::user_message_style().bg,
            Some(ratatui::style::Color::Rgb(0, 63, 16))
        );
        Ok(())
    }
    #[test]
    fn arrows_handoff_between_transcript_and_composer_without_losing_draft() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        draw(&mut ui, 120, 35)?;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(ui.focus, Focus::Conversation);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(ui.focus, Focus::Composer);
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert("keep me");
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .cursor = 0;
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(ui.focus, Focus::Composer);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER));
        assert_eq!(ui.focus, Focus::Conversation);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER));
        assert_eq!(ui.focus, Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "keep me"
        );
        Ok(())
    }

    #[test]
    fn actions_menu_stays_available_after_returning_to_sessions_list() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.skills.insert("one".into(), (Vec::new(), Vec::new()));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        ui.key(key(KeyCode::Char('/')));
        assert!(matches!(ui.modal, Some(Modal::Menu { .. })));
        ui.key(key(KeyCode::Esc));
        // Tab opens the session input, then returns to the list without closing the session.
        ui.key(key(KeyCode::Tab));
        assert_eq!(ui.focus, Focus::Composer);
        ui.key(key(KeyCode::Tab));
        assert_eq!(ui.focus, Focus::List);
        assert!(ui.drilled);
        for _ in 0..2 {
            ui.key(key(KeyCode::Char('/')));
            assert!(matches!(ui.modal, Some(Modal::Menu { .. })));
            ui.paste("archive");
            let (screen, _) = draw(&mut ui, 100, 30)?;
            assert!(screen.contains("Archive session"));
            ui.key(key(KeyCode::Esc));
        }
        // The empty composer retains native commands, and /actions consistently opens controls.
        ui.key(key(KeyCode::Tab));
        for _ in 0..2 {
            ui.key(key(KeyCode::Char('/')));
            assert!(matches!(ui.modal, Some(Modal::Commands { .. })));
            ui.paste("actions");
            ui.key(key(KeyCode::Enter));
            assert!(matches!(ui.modal, Some(Modal::Menu { .. })));
            ui.key(key(KeyCode::Esc));
        }
        assert!(!ui.busy);
        assert!(
            ui.positions
                .get("one")
                .context("draft")?
                .draft
                .text()
                .is_empty()
        );
        Ok(())
    }
    #[test]
    fn model_and_effort_completion_uses_catalog_without_sending_on_first_enter() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.model_completion.options = vec![
            crate::model::ModelInfo {
                id: "fixture-luna".into(),
                name: "Luna".into(),
                efforts: vec!["low".into(), "medium".into()],
            },
            crate::model::ModelInfo {
                id: "fixture-astra".into(),
                name: "Astra".into(),
                efforts: vec!["high".into()],
            },
        ];
        ui.open_model(0);
        ui.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        ui.paste("LUNA");
        let (screen, cursor) = draw(&mut ui, 100, 30)?;
        assert!(screen.contains("fixture-luna") && !screen.contains("fixture-astra"));
        assert!(cursor.is_some());
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Model { model, .. }) if model.text() == "fixture-luna")
        );
        assert!(!ui.busy);
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Model { effort, .. }) if effort.text() == "medium")
        );
        assert!(!ui.busy);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.skills.insert("one".into(), (Vec::new(), Vec::new()));
        ui.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        ui.paste("effort");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::Model { field: 1, .. })));
        Ok(())
    }

    #[test]
    fn empty_composer_opens_native_commands_and_skills_attach_without_sending() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.selected = Some("one".into());
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.skills.insert(
            "one".into(),
            (
                vec![Skill {
                    name: "fixture".into(),
                    path: "/tmp/project/.agents/skills/fixture/SKILL.md".into(),
                    description: "fixture skill".into(),
                    enabled: true,
                }],
                Vec::new(),
            ),
        );
        for trigger in ['/', '$', '@'] {
            ui.key(KeyEvent::new(KeyCode::Char(trigger), KeyModifiers::NONE));
            assert!(matches!(ui.modal, Some(Modal::Commands { .. })));
            ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
            ui.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            assert!(matches!(ui.modal, Some(Modal::Commands { .. })));
            ui.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            assert!(ui.modal.is_none());
            assert_eq!(ui.focus, Focus::Composer);
            assert!(
                ui.positions
                    .get("one")
                    .context("draft")?
                    .draft
                    .text()
                    .is_empty()
            );
        }
        ui.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::Commands { .. })));
        ui.paste("fixture");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let position = ui.positions.get("one").context("Missing draft")?;
        assert_eq!(position.draft.text(), "$fixture ");
        assert_eq!(position.skills.len(), 1);
        assert!(!ui.busy);
        ui.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        assert_eq!(
            ui.positions
                .get("one")
                .context("Missing draft")?
                .draft
                .text(),
            "$fixture /"
        );
        ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert_eq!(ui.clipboard.as_deref(), Some("/"));
        Ok(())
    }
    #[test]
    fn activity_and_pending_messages_stay_above_composer_when_reading_history() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.status = Status::Running;
        session.turn_id = Some("active".into());
        session.turn_started_at = Some(chrono::Utc::now().timestamp_millis());
        for _ in 0..50 {
            session.note("agentMessage", "Older conversation line");
        }
        session.entries.push(Entry {
            id: "tool".into(),
            kind: "commandExecution".into(),
            data: serde_json::json!({"command":"git diff --stat"}),
            started_at: session.turn_started_at,
            ..Entry::default()
        });
        session.entries.push(Entry {
            id: "review".into(),
            kind: "autoApprovalReview".into(),
            data: serde_json::json!({"targetItemId":"tool"}),
            started_at: session.turn_started_at,
            ..Entry::default()
        });
        session.entries.push(Entry {
            id: "steer".into(),
            kind: "userMessage".into(),
            text: "Focus on navigation".into(),
            data: serde_json::json!({"difuSteeringTurn":"active"}),
            ..Entry::default()
        });
        session.queue.push("Explain the result next".into());
        let position = ui.positions.get_mut("one").context("position")?;
        position.follow = false;
        position.conversation = 0;
        let (screen, _) = draw(&mut ui, 200, 60)?;
        let activity = screen
            .find("Reviewing approval request")
            .context("activity")?;
        let steering = screen
            .find("Messages to be submitted after the next tool call")
            .context("steering")?;
        let queued = screen
            .find("Messages queued for the next turn")
            .context("queued")?;
        let composer = screen.find("Ask anything…").context("composer")?;
        assert!(activity < steering && steering < queued && queued < composer);
        assert!(screen.contains("git diff --stat"));
        assert!(screen.contains("Explain the result next"));
        assert!(screen.contains("press esc to interrupt and send immediately"));
        let session = ui.sessions.get_mut("one").context("session")?;
        session.queue = vec!["long ".repeat(10000).into(), "Another message".into()];
        let rows = transcript::waiting_messages(session, 120, &[]);
        assert!(rows.len() < 12);
        assert!(
            rows.iter()
                .any(|row| row.to_string().starts_with("• Messages")
                    && row.spans.first().is_some_and(|s| s.style.fg == Some(TEXT)))
        );
        assert!(
            rows.iter().any(
                |row| row.to_string().starts_with("  ↳ long") && row.to_string().ends_with('…')
            )
        );
        assert!(
            rows.iter()
                .any(|row| row.to_string() == "  ↳ Another message")
        );
        session.pending.push(Pending {
            id: serde_json::json!("difu-missing-guidance"),
            method: "item/tool/requestUserInput".into(),
            params: Value::Null,
            responded: false,
        });
        assert!(!session.can_send_waiting());
        assert!(
            !transcript::waiting_messages(session, 120, &[])
                .iter()
                .any(|row| row.to_string().contains("press esc"))
        );
        assert_eq!(ui.positions.get("one").context("position")?.conversation, 0);
        Ok(())
    }
    #[test]
    fn composer_accepts_ghostty_word_navigation_without_switching_focus() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.paste("hello, 世界 foo_bar");
        for (code, expected) in [
            (KeyCode::Char('b'), 10),
            (KeyCode::Left, 7),
            (KeyCode::Char('f'), 9),
            (KeyCode::Right, 17),
        ] {
            ui.key(KeyEvent::new(code, KeyModifiers::ALT));
            assert_eq!(ui.focus, Focus::Composer);
            assert!(ui.modal.is_none());
            let draft = &ui.positions.get("one").context("draft")?.draft;
            assert_eq!(draft.cursor, expected);
            assert_eq!(draft.text(), "hello, 世界 foo_bar");
        }
        Ok(())
    }
    #[test]
    fn workspace_suggestions_keep_conversation_and_draft_visible() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.sessions
            .get_mut("one")
            .context("session")?
            .note("agentMessage", "Conversation stays visible");
        ui.paste("Inspect ");
        ui.paths_loading.insert("one".into()); // Use the deterministic cached index.
        ui.workspace_paths.insert(
            "one".into(),
            vec!["src/".into(), "src/main.rs".into(), "README.md".into()],
        );
        ui.key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::SHIFT));
        ui.paste("main");
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("Conversation stays visible"));
        assert!(screen.contains("Inspect @main"));
        assert!(screen.find("@src/main.rs") < screen.find("Inspect @main"));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("draft")?.draft.text(),
            "Inspect @src/main.rs "
        );
        assert!(ui.modal.is_none() && !ui.busy);
        ui.skills.insert(
            "one".into(),
            (
                vec![Skill {
                    name: "inspect".into(),
                    path: "/tmp/inspect/SKILL.md".into(),
                    description: "Inspect code".into(),
                    enabled: true,
                }],
                Vec::new(),
            ),
        );
        ui.key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::SHIFT));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("draft")?.draft.text(),
            "Inspect @src/main.rs $inspect "
        );
        Ok(())
    }
    #[test]
    fn chat_and_expanded_messages_keep_link_targets_after_resize() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().into(),
        });
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        session.note(
            "agentMessage",
            "Open [the invoice](https://example.com/invoice)",
        );
        let entry = session.entries.last().context("message")?.id.clone();
        for width in [120, 90] {
            draw(&mut ui, width, 35)?;
            assert!(ui.hits.iter().any(|(_, action)| matches!(action, Action::Link(url) if url == "https://example.com/invoice")));
        }
        ui.action(Action::ToggleEntry(entry));
        draw(&mut ui, 120, 35)?;
        assert!(ui.hits.iter().any(|(_, action)| matches!(action, Action::Link(url) if url == "https://example.com/invoice")));
        Ok(())
    }
    #[test]
    fn sidebar_shows_status_time_and_colored_cached_counts_without_coding_row() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = state(storage.clone());
        let summary = ui.summaries.first_mut().context("summary")?;
        summary.status = Status::Running;
        summary.turn_started_at = Some(chrono::Utc::now().timestamp_millis() - 12_000);
        ui.sidebar.counts.insert(
            "one".into(),
            DiffStatistics {
                added: 42,
                removed: 7,
            },
        );
        ui.sidebar.save(&storage)?;
        assert_eq!(
            sidebar::State::load(&storage).counts.get("one").copied(),
            Some(DiffStatistics {
                added: 42,
                removed: 7
            })
        );
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("Working 12s"));
        assert!(screen.contains("+42 -7"));
        assert!(!screen.contains("Coding · Working"));
        ui.archived = true;
        ui.summaries.first_mut().context("summary")?.archived = true;
        let (screen, _) = draw(&mut ui, 160, 35)?;
        assert!(screen.contains("archived"));
        Ok(())
    }
    #[test]
    fn sidebar_names_original_repository_and_marks_worktree() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        let session = ui.sessions.get_mut("one").context("session")?;
        session.workspace = Some("/tmp/worktrees/opaque-worktree-id".into());
        session.workspace_ready = true;
        ui.summaries = vec![session.summary()];
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("project 🌳"));
        assert!(!screen.contains("opaque-worktree-id"));
        let session = ui.sessions.get_mut("one").context("session")?;
        session.workspace_ready = false;
        session.workspace = Some("/tmp/project".into());
        ui.summaries = vec![session.summary()];
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("project") && !screen.contains("🌳"));
        Ok(())
    }

    #[test]
    fn async_questions_open_three_tabs_and_keep_answer_drafts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.push(Entry {
            id: "async-questions".into(),
            kind: "agentMessage".into(),
            text: "Three questions".into(),
            data: serde_json::json!({"delivery":"async","questions":[
                {"title":"What next?","options":["Explore","Review"]},
                {"title":"How much detail?","options":["Brief","Full"]},
                {"title":"Anything else?","options":null}
            ]}),
            ..Entry::default()
        });
        session.restore_async_questions();
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("3 question(s)"));
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert("unsent composer");
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        ui.key(KeyEvent::new_with_kind(
            KeyCode::Up,
            KeyModifiers::ALT,
            crossterm::event::KeyEventKind::Release,
        ));
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(
            screen.contains("1 of 3")
                && screen.contains("What next?")
                && screen.contains("3. Other")
        );
        assert_eq!(screen.matches("1 of 3").count(), 1);
        assert!(!screen.contains("3 question(s)"));
        assert!(
            screen
                .lines()
                .any(|line| line.contains("1 of 3") && line.contains("Alt+↑ Questions"))
        );
        let header_row = ui
            .hits
            .iter()
            .find_map(|(rect, action)| matches!(action, Action::Pending).then_some(rect.y))
            .context("questions header")?;
        assert!(
            ui.hits
                .iter()
                .filter(|(_, action)| matches!(action, Action::InlineQuestion(_)))
                .all(|(rect, _)| rect.y == header_row)
        );
        assert!(!screen.contains("Agent needs your input") && !screen.contains("unsent composer"));
        for (code, expected) in [(KeyCode::Tab, 1), (KeyCode::BackTab, 0)] {
            ui.key(KeyEvent::new(code, KeyModifiers::NONE));
            ui.key(KeyEvent::new_with_kind(
                code,
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Release,
            ));
            assert!(matches!(&ui.modal,Some(Modal::Approval{field,..}) if *field == expected));
        }
        ui.key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Release,
        ));
        assert!(!ui.busy && ui.question_send.is_none());

        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        ui.paste("ustom answer");
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("2 of 3") && screen.contains("How much detail?"));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        assert!(
            matches!(&ui.modal,Some(Modal::Approval{answers,field,selected,..}) if *field==0 && *selected==2 && answers.first().is_some_and(|a|a.text()=="custom answer"))
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        assert!(ui.modal.is_none() && ui.focus == Focus::Composer);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "unsent composer"
        );
        ui.open_question(2);
        ui.paste("Keep the composer draft");
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.open_question(2);
        assert!(
            matches!(&ui.modal,Some(Modal::Approval{answers,field,..}) if *field==2 && answers.get(2).is_some_and(|a|a.text()=="Keep the composer draft"))
        );
        for (width, height) in [(40, 12), (80, 24), (180, 50)] {
            draw(&mut ui, width, height)?;
        }
        assert!(!ui.busy);
        Ok(())
    }
    #[test]
    fn question_panel_resize_keeps_chat_rows_fixed_until_new_output_or_navigation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.clear();
        session.note(
            "agentMessage",
            (0..80)
                .map(|i| format!("Stable chat row {i}\n"))
                .collect::<String>(),
        );
        session.pending.push(Pending { id:serde_json::json!("stable"),method:"item/tool/requestUserInput".into(),responded:false,params:serde_json::json!({"questions":[{"id":"q","question":"Choose scope","options":[{"label":"Small"},{"label":"Large"}]},{"id":"q2","question":"Anything else?","options":[]}]}) });
        ui.positions.get_mut("one").context("position")?.follow = true;
        let (original, _) = draw(&mut ui, 120, 35)?;
        let text_offset = original.find("Stable chat row").context("visible chat")?;
        let text = original
            .get(text_offset..)
            .and_then(|s| s.split('│').next())
            .context("chat row")?
            .trim()
            .to_owned();
        let offset = ui.positions.get("one").context("position")?.conversation;
        ui.open_question(0);
        for _ in 0..2 {
            let (screen, _) = draw(&mut ui, 120, 35)?;
            assert_eq!(
                screen.get(text_offset..text_offset + text.len()),
                Some(text.as_str())
            );
            assert_eq!(
                ui.positions.get("one").context("position")?.conversation,
                offset
            );
        }
        ui.open_question(1);
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert_eq!(
            screen.get(text_offset..text_offset + text.len()),
            Some(text.as_str())
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert_eq!(
            screen.get(text_offset..text_offset + text.len()),
            Some(text.as_str())
        );
        ui.open_question(0);
        draw(&mut ui, 120, 35)?;
        ui.sessions
            .get_mut("one")
            .context("session")?
            .note("agentMessage", "New streamed output");
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("New streamed output"));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        draw(&mut ui, 120, 35)?;
        ui.focus = Focus::Conversation;
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        draw(&mut ui, 120, 35)?;
        assert!(
            !ui.positions
                .get("one")
                .context("position")?
                .keep_transcript_position
        );
        Ok(())
    }
    #[test]
    fn question_notes_stay_with_their_choice_and_submit_only_with_that_answer() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.sessions.get_mut("one").context("session")?.pending.push(Pending {
            id:serde_json::json!("notes"),method:"item/tool/requestUserInput".into(),responded:false,
            params:serde_json::json!({"difuAsync":true,"questions":[{"id":"q","question":"Choose scope","options":[{"label":"Small"},{"label":"Large"}]},{"id":"q2","question":"Other question","options":[]}]}),
        });
        ui.open_question(0);
        ui.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        ui.key(KeyEvent::new_with_kind(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Release,
        ));
        assert!(ui.question_note_focused);
        assert_eq!(ui.question_note().context("note")?.text(), "");
        ui.paste("Only change the UI");
        ui.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT));
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Small\n\nNote: Only change the UI!")
        );
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("Note: Only change the UI!"));
        ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert_eq!(ui.clipboard.as_deref(), Some("Only change the UI!"));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.inline_question() && !ui.question_note_focused);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.selected_question_answer().as_deref(), Some("Large"));
        ui.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        ui.paste("Include docs");
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Large\n\nNote: Include docs")
        );
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Large\n\nNote: Include docs")
        );
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Small\n\nNote: Only change the UI!")
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.open_question(0);
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Small\n\nNote: Only change the UI!")
        );
        for (width, height) in [(40, 12), (60, 20)] {
            draw(&mut ui, width, height)?;
        }
        assert!(!ui.busy); // Editing never submits a choice automatically.
        Ok(())
    }
    #[test]
    fn ghostty_line_shortcuts_edit_composer_and_preserve_filter_clear() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.paste("before\ncurrent line");
        ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(ui.positions.get("one").context("position")?.draft.cursor, 7);
        ui.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.cursor,
            19
        );
        ui.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "before"
        );
        assert!(!ui.busy);
        Ok(())
    }
    #[test]
    fn transcript_focus_expansion_and_question_drafts_preserve_composer() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Conversation;
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert("unsent draft");
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.push(Entry {
            id: "tool".into(),
            kind: "commandExecution".into(),
            text: "preview one\npreview two\npreview three\nhidden tool output".into(),
            data: serde_json::json!({"command":"git status","status":"completed"}),
            finished_at: Some(1),
            ..Entry::default()
        });
        session.pending.push(Pending { id:serde_json::json!(42), method:"item/tool/requestUserInput".into(), responded:false, params:serde_json::json!({"questions":[{"id":"q","header":"Choice","question":"Choose a color","options":[{"label":"Green","description":"Matrix"}]}]}) });
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(!screen.contains("hidden tool output"));
        assert!(screen.contains("preview one") && screen.contains("Enter/click to expand"));
        assert!(!screen.contains("agentMessage"));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("hidden tool output"));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        ui.paste("Green");
        assert!(!ui.busy);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.pending(0);
        assert!(
            matches!(&ui.modal,Some(Modal::Approval {answers,..}) if answers.first().is_some_and(|a|a.text()=="Green"))
        );
        ui.modal = None;
        ui.focus = Focus::Conversation;
        ui.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let p = ui.positions.get("one").context("position")?;
        assert!(p.follow);
        assert_eq!(p.draft.text(), "unsent draft");
        Ok(())
    }
    #[test]
    fn pane_toggles_preserve_drafts_scroll_and_session_selection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        };
        let mut ui = state(storage.clone());
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.paste("draft text");
        ui.positions
            .get_mut("one")
            .context("position")?
            .conversation = 7;
        ui.key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::ALT));
        ui.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        assert!(!ui.list_visible && ui.changes_visible);
        assert_eq!(ui.selected.as_deref(), Some("one"));
        let p = ui.positions.get("one").context("position")?;
        assert_eq!(p.draft.text(), "draft text");
        assert_eq!(p.conversation, 7);
        let restored = Ui::new(storage.clone(), &storage.load_config()?);
        assert!(!restored.list_visible && restored.changes_visible);
        ui.focus = Focus::Composer;
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(ui.focus == Focus::Changes);
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(ui.focus == Focus::ChangeTree);
        let (screen, cursor) = draw(&mut ui, 140, 35)?;
        assert!(!screen.contains("draft text") && screen.contains("Current worktree changes"));
        assert_eq!(cursor, Some((0, 0)));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 140, 35)?;
        assert!(screen.contains("draft text"));
        assert!(!ui.changes_visible);
        Ok(())
    }
    #[test]
    fn sidebar_help_copy_and_all_input_layouts_remain_usable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        });
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(!screen.contains("f Filter"));
        assert_eq!(ui.filtered().len(), 1);
        ui.selected = Some("one".into());
        ui.drilled = true;
        ui.focus = Focus::Changes;
        ui.changes_visible = true;
        ui.changes
            .insert("one".into(), "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -0,0 +1,2 @@\n+    first\n+    second\n".into());
        draw(&mut ui, 120, 35)?;
        ui.positions.get_mut("one").context("position")?.changes = ui
            .change_rows
            .iter()
            .position(|row| row.source.is_some())
            .context("source row")?;
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert_eq!(ui.clipboard.as_deref(), Some("    first\n    second"));
        ui.key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        ui.paste("Alt+D");
        let (screen, _) = draw(&mut ui, 100, 30)?;
        assert!(screen.contains("Open or close Changes"));
        assert!(!screen.contains("Launch a new coding agent"));
        ui.modal = Some(Modal::Repository(Editor::from("/tmp/repo")));
        for (width, height) in [(40, 12), (80, 24), (180, 50)] {
            draw(&mut ui, width, height)?;
        }
        ui.modal = Some(Modal::Rename(Editor::default()));
        draw(&mut ui, 80, 24)?;
        ui.modal = Some(Modal::Model {
            model: Editor::default(),
            effort: Editor::default(),
            field: 1,
        });
        draw(&mut ui, 80, 24)?;
        Ok(())
    }
}
