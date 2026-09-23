//! Native session UI. Network/process work is performed off the terminal thread.
use super::*;
mod browser;
mod commands;
mod defaults;
mod inline_questions;
mod media;
mod models;
mod panels;
mod prompt;
mod questions;
mod selection;
mod sidebar;
mod transcript;
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
    Filter,
    Conversation,
    Prompt,
    Composer,
    Changes,
}
#[derive(Default)]
pub struct Position {
    pub conversation: usize,
    pub follow: bool,
    pub changes: usize,
    pub horizontal: usize,
    pub selection: Option<usize>,
    pub draft: Editor,
    history: Option<prompt::History>,
    pub skills: Vec<super::Skill>,
    pub attachments: Vec<super::media::Attachment>,
    pub image_count: usize,
    pub video_count: usize,
    media_loaded: bool,
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
    Resource(bool, usize),
    RefreshArtifact,
    ExternalArtifact,
    BrowserChoice(usize),
    New,
    ToggleList,
    ToggleChanges,
    Approval(usize),
    MenuItem(usize),
    Command(usize),
    ToggleEntry(String),
    ShowPrompt(String),
    RemoveAttachment(std::path::PathBuf),
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
    DefaultField(usize),
    ModelField(usize),
    ModelOption(usize),
    DefaultToggle,
    DefaultSave,
    Approve(usize),
    Answer(usize, String),
    InlineQuestion(usize),
    QuestionChoice(usize),
    QuestionNote(usize),
    SubmitQuestion(bool),
}
pub enum Modal {
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
    Prompt {
        text: String,
        scroll: usize,
    },
    Voice {
        key: Editor,
        field: usize,
    },
    Repository(Editor),
    Commands {
        query: Editor,
        selected: usize,
        skills_only: bool,
        files_only: bool,
    },
    Status,
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
    Changes(String),
    Statistics(String),
    Shells(String),
    OpenArtifact,
    InstallBrowser(String, std::path::PathBuf, String),
    WorkspacePaths(String),
    Defaults(String),
    Skills(String),
    Launch,
    Action,
    Question,
    Delete(String),
    Send(String, String, Vec<super::media::Attachment>),
}

pub struct Ui {
    panels: panels::Panels,
    pub defaults: crate::storage::AgentDefaults,
    pub selected: Option<String>,
    pub summaries: Vec<Summary>,
    pub sessions: HashMap<String, Session>,
    pub positions: HashMap<String, Position>,
    pub changes: HashMap<String, String>,
    pub focus: Focus,
    pub drilled: bool,
    pub list_visible: bool,
    pub changes_visible: bool,
    pub filter: Editor,
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
    question_send: Option<(String, Value, String, usize)>,
    question_reveal: bool,
    question_note_focused: bool,
    hits: Vec<(Rect, Action)>,
    pub viewport: usize,
    list_scroll: usize,
    conversation_lines: usize,
    conversation_sections: Vec<transcript::Section>,
    conversation_height: usize,
    text_selection: selection::Selection,
    change_lines: usize,
    skills: HashMap<String, (Vec<Skill>, Vec<String>)>,
    skills_loading: Option<String>,
    workspace_paths: HashMap<String, Vec<String>>,
    paths_loading: HashSet<String>,
    sidebar: sidebar::State,
    voice: voice::State,
    model_completion: models::State,
    media_pending: Option<(String, mpsc::Receiver<Result<super::media::Paste, String>>)>,
}
impl Ui {
    pub fn new(storage: Storage, config: &Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            panels: panels::Panels::new(config.agent_panel_right),
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
            filter: Editor::default(),
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
            question_send: None,
            question_reveal: true,
            question_note_focused: false,
            hits: Vec::new(),
            viewport: 20,
            list_scroll: 0,
            conversation_lines: 0,
            conversation_sections: Vec::new(),
            conversation_height: 0,
            text_selection: selection::Selection::default(),
            change_lines: 0,
            skills: HashMap::new(),
            skills_loading: None,
            workspace_paths: HashMap::new(),
            paths_loading: HashSet::new(),
            voice: voice::State::new(config.voice_enabled),
            model_completion: models::State::default(),
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
        self.tick_panels(visible);
        self.tick_models();
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
                Task::Changes(_) => {
                    self.changing = false;
                    self.changes_at = Some(Instant::now());
                }
                Task::Launch | Task::Action | Task::Delete(_) | Task::Send(..) => self.busy = false,
                Task::Defaults(_)
                | Task::Question
                | Task::OpenArtifact
                | Task::InstallBrowser(..) => {}
                Task::WorkspacePaths(id) => {
                    self.paths_loading.remove(id);
                }
                Task::Statistics(id) => {
                    self.sidebar.finished(id);
                }
                Task::Skills(_) => self.skills_loading = None,
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
                    if matches!(message.kind, Task::Question) {
                        self.busy = false;
                        self.question_send = None;
                    }
                    self.notice = Some((error, true));
                }
                Ok(reply) => match (message.kind, reply) {
                    (Task::WorkspacePaths(id), Reply::WorkspacePaths(paths)) => {
                        self.workspace_paths.insert(id, paths);
                    }
                    (Task::Statistics(id), Reply::Statistics(stats)) => {
                        self.sidebar.counts.insert(id, stats);
                        if let Err(error) = self.sidebar.save(&self.storage) {
                            self.notice =
                                Some((format!("Cannot cache session statistics: {error:#}"), true));
                        }
                    }
                    (Task::Skills(id), Reply::Skills { skills, errors }) => {
                        self.skills.insert(id, (skills, errors));
                    }
                    (Task::List, Reply::Sessions(sessions)) => {
                        self.sidebar.observe(&sessions);
                        self.summaries = sessions;
                        self.ensure_selected();
                    }
                    (Task::Read(id), Reply::Session(session)) => {
                        if !self.sessions.contains_key(&id) && session.thread_id.is_none() {
                            self.task(
                                Task::Defaults(id.clone()),
                                Request::Defaults {
                                    cwd: session.job.root().clone(),
                                },
                                true,
                            );
                        }
                        if session.workspace_removed {
                            self.changes.remove(&id);
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
                    (Task::Changes(id), Reply::Changes(patch)) => {
                        self.changes.insert(id, patch);
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
                        self.positions.entry(id).or_default().follow = true;
                        self.drilled = true;
                        self.focus = Focus::Composer;
                        self.modal = None;
                        self.refreshed = None;
                        if let Ok(config) = self.storage.load_config() {
                            self.defaults = config.agent_defaults;
                        }
                    }
                    (Task::Send(id, text, attachments), Reply::Ok) => {
                        if let Some(position) = self.positions.get_mut(&id) {
                            if position.draft.text() == text {
                                position.draft = Editor::default();
                                position.history = None;
                                position.skills.clear();
                                position.attachments.retain(|a| !attachments.contains(a));
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
                        self.modal = None;
                        self.notice = Some(("Action accepted".into(), false));
                        self.refreshed = None;
                    }
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
                self.task(Task::Changes(id.clone()), Request::Changes { id }, false);
            }
        }
    }
    fn filtered(&self) -> Vec<Summary> {
        let query = self.filter.text().to_lowercase();
        self.summaries
            .iter()
            .filter(|s| {
                s.kind != "Guide"
                    && s.archived == self.archived
                    && format!("{} {} {}", s.title, s.kind, s.workspace.display())
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect()
    }
    fn ensure_selected(&mut self) {
        let list = self.filtered();
        if !list.iter().any(|s| Some(&s.id) == self.selected.as_ref()) && !self.drilled {
            self.selected = list.first().map(|s| s.id.clone());
        }
    }
    fn select(&mut self, id: String) {
        self.selected = Some(id.clone());
        self.positions.entry(id.clone()).or_default();
        self.restore_media(&id);
        self.conversation_sections.clear();
        self.text_selection.clear();
        self.changes_at = None;
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
    fn menu_entries(&self) -> Vec<&'static str> {
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
            "Toggle agents list · Ctrl+B",
            "Toggle Changes · Ctrl+D",
            "Delete clean inactive worktree",
            "Open Reviews",
            "Queued outgoing messages",
            "Pending questions · Alt+↑",
            "Default repository, model, reasoning and worktree",
            "Delete chat and clean up worktree",
            "Open shells",
            "HTML artifacts",
            "Move shell / artifact pane · Alt+P",
        ]
    }
    fn menu_action(&mut self, index: usize) {
        match index {
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
        if !self.list_visible && matches!(self.focus, Focus::List | Focus::Filter) {
            self.focus = Focus::Conversation;
        }
        self.save_visibility();
    }
    pub fn toggle_changes(&mut self) {
        self.panels.view = None;
        self.panels.focused = false;
        self.changes_visible = !self.changes_visible;
        if !self.changes_visible && self.focus == Focus::Changes {
            self.focus = Focus::Conversation;
        }
        self.changes_at = None;
        self.save_visibility();
    }
    fn send(&mut self, queue: bool) {
        if self.busy || self.media_pending.is_some() {
            return;
        }
        if let Some(id) = self.selected.clone() {
            let draft = self.positions.entry(id.clone()).or_default().draft.text();
            let attachments = self
                .positions
                .get(&id)
                .map(|p| p.attachments.clone())
                .unwrap_or_default();
            let text = format!(
                "{}{}",
                draft,
                attachments
                    .iter()
                    .map(|a| format!("\n{}", a.token()))
                    .collect::<String>()
            );
            if !text.trim().is_empty() {
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
            Some(Modal::Repository(editor)) => Some(editor),
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
            None if self.focus == Focus::Filter => Some(&self.filter),
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
            && matches!(self.focus, Focus::Conversation | Focus::Prompt)
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
        if ctrl && key.code == KeyCode::Char('b') {
            self.toggle_list();
            return;
        }
        if ctrl && key.code == KeyCode::Char('d') {
            self.toggle_changes();
            return;
        }
        if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::ALT) {
            self.open_questions();
            return;
        }
        if self.modal.is_some() {
            if self.prompt_key(key) {
                return;
            }
            self.modal_key(key);
            self.remember_answers();
            return;
        }
        if self.drilled
            && key.modifiers == KeyModifiers::SUPER
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && matches!(
                self.focus,
                Focus::Composer | Focus::Conversation | Focus::Prompt
            )
        {
            if self.focus == Focus::Composer {
                if key.code == KeyCode::Up {
                    self.focus = if self.conversation_sections.is_empty()
                        && self.latest_prompt().is_some()
                    {
                        Focus::Prompt
                    } else {
                        Focus::Conversation
                    };
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
            } else if self.focus == Focus::Prompt {
                if key.code == KeyCode::Down {
                    self.focus = if self.conversation_sections.is_empty() {
                        Focus::Composer
                    } else {
                        Focus::Conversation
                    };
                }
            } else {
                self.move_transcript(key.code == KeyCode::Down);
            }
            return;
        }
        if self.focus == Focus::Filter {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.focus = Focus::List,
                KeyCode::Char('u') if ctrl => self.filter.clear(),
                _ => self.filter.key(key),
            }
            self.ensure_selected();
            return;
        }
        if self.focus == Focus::Composer && self.drilled {
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
            KeyCode::Enter if self.focus == Focus::Prompt => {
                if let Some(text) = self.latest_prompt() {
                    self.action(Action::ShowPrompt(text));
                }
            }
            KeyCode::Down if self.focus == Focus::Prompt => {
                self.focus = if self.conversation_sections.is_empty() {
                    Focus::Composer
                } else {
                    Focus::Conversation
                };
            }
            KeyCode::Up if self.focus == Focus::Prompt => {}
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
            KeyCode::Char('f') if self.list_visible => self.focus = Focus::Filter,
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
            KeyCode::Char('i') | KeyCode::Enter if self.drilled => {
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
    fn cycle_focus(&mut self, _backwards: bool) {
        if self.focus == Focus::List {
            if self
                .selected
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .is_some_and(|s| matches!(s.job, Job::Coding(_)))
            {
                self.drilled = true;
                self.focus = Focus::Composer;
                self.changes_at = None;
            }
        } else if self.list_visible {
            self.focus = Focus::List;
        } else if self.drilled {
            self.focus = Focus::Composer;
        }
    }
    fn move_transcript(&mut self, down: bool) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let has_prompt = self.latest_prompt().is_some();
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
        if !down && current == Some(0) && has_prompt {
            self.focus = Focus::Prompt;
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
        if let Some(id) = self.selected.as_ref()
            && let (Some(p), Some(patch)) = (self.positions.get(id), self.changes.get(id))
        {
            let start = p.selection.unwrap_or(p.changes).min(p.changes);
            let end = p.selection.unwrap_or(p.changes).max(p.changes);
            let text = patch
                .lines()
                .skip(start)
                .take(end.saturating_sub(start) + 1)
                .map(|line| {
                    if !line.starts_with("+++")
                        && !line.starts_with("---")
                        && matches!(line.as_bytes().first(), Some(b'+' | b'-' | b' '))
                    {
                        line.get(1..).unwrap_or(line)
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.clipboard = Some(text);
        }
    }
    pub fn paste(&mut self, text: &str) {
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
                }
            }
            Some(Modal::Repository(editor)) => editor.insert(text),
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
            None if self.focus == Focus::Filter => {
                self.filter.insert(text);
                self.ensure_selected();
            }
            None if self.focus == Focus::Composer => {
                if let Some(id) = self.selected.clone() {
                    self.positions.entry(id).or_default().draft.paste(text);
                }
            }
            _ => {}
        }
    }
    fn modal_key(&mut self, key: KeyEvent) {
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
            self.modal = None;
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let menu = self.menu_entries();
        let mut action = None;
        match &mut self.modal {
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
            Action::ModelField(index) => self.model_field(index),
            Action::ModelOption(index) => self.model_option(index),
            Action::DefaultField(index) => {
                if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
                    form.field = index;
                }
            }
            Action::DefaultToggle => {
                if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
                    form.field = 3;
                    form.isolated = !form.isolated;
                }
            }
            Action::DefaultSave => self.save_defaults(),
            Action::Select(id) => {
                self.select(id);
                self.focus = Focus::List;
            }
            Action::Open => {
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
            Action::Resources(artifacts) => self.open_resources(artifacts),
            Action::Resource(artifacts, index) => self.select_resource(artifacts, index),
            Action::RefreshArtifact => self.refresh_artifact(),
            Action::ExternalArtifact => self.external_artifact(),
            Action::BrowserChoice(index) => self.browser_choice(index),
            Action::MenuItem(index) => self.menu_action(index),
            Action::Command(index) => self.run_command(index),
            Action::RemoveAttachment(path) => {
                if let Some(id) = self.selected.clone()
                    && let Some(p) = self.positions.get_mut(&id)
                {
                    p.attachments.retain(|a| a.path != path);
                    if let Err(error) = super::media::save_draft(&self.storage, &id, &p.attachments)
                    {
                        self.notice =
                            Some((format!("Cannot save attachment draft: {error:#}"), true));
                    }
                }
            }
            Action::ShowPrompt(text) => self.modal = Some(Modal::Prompt { text, scroll: 0 }),
            Action::ToggleEntry(entry) => {
                if let Some(id) = self.selected.clone() {
                    let p = self.positions.entry(id).or_default();
                    p.focused_entry = Some(entry.clone());
                    if !p.expanded.remove(&entry) {
                        p.expanded.insert(entry);
                    }
                    self.focus = Focus::Conversation;
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
                } else if let Some(Modal::Approval { scroll, .. } | Modal::Prompt { scroll, .. }) =
                    &mut self.modal
                {
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }));
    frame.render_widget(block, rect);
    inner(rect)
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
                " Ctrl+B Agents ",
                Action::ToggleList,
                self.list_visible,
            );
            self.button(
                frame,
                Rect::new(toolbar.x + 46, toolbar.y, 18, 1),
                " Ctrl+D Changes ",
                Action::ToggleChanges,
                self.changes_visible,
            );
        }
        let content = Rect::new(
            1,
            4,
            area.width.saturating_sub(2),
            area.height.saturating_sub(9),
        );
        self.viewport = content.height.saturating_sub(2) as usize;
        let list_width = if self.list_visible {
            (content.width / 4)
                .clamp(20, 38)
                .min(content.width.saturating_sub(20))
        } else {
            0
        };
        let panel = self.panels.view.is_some();
        let changes_width =
            if self.drilled && ((panel && self.panels.right) || (!panel && self.changes_visible)) {
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
        if list_width > 0 {
            self.draw_list(frame, list);
        }
        if panel && !self.panels.right {
            self.draw_panel(frame, conversation);
        } else {
            self.draw_conversation(frame, conversation);
        }
        if changes_width > 0 {
            if panel {
                self.draw_panel(frame, changes);
            } else {
                self.draw_changes(frame, changes);
            }
        }
        if self.selected.is_some() {
            let shells = self
                .selected
                .as_ref()
                .and_then(|id| self.panels.shells.get(id))
                .map_or(0, Vec::len);
            let artifacts = self
                .selected
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .map_or(0, |s| s.artifacts.len());
            self.button(
                frame,
                Rect::new(
                    conversation.x,
                    content.bottom() + 1,
                    18.min(conversation.width),
                    1,
                ),
                &format!(" Shells ({shells}) "),
                Action::Resources(false),
                false,
            );
            if conversation.width > 19 {
                self.button(
                    frame,
                    Rect::new(
                        conversation.x + 19,
                        content.bottom() + 1,
                        (conversation.width - 19).min(24),
                        1,
                    ),
                    &format!(" Artifacts ({artifacts}) "),
                    Action::Resources(true),
                    false,
                );
            }
        }
        if let Some((notice, error)) = &self.notice {
            frame.render_widget(
                Paragraph::new(crate::model::clean(notice)).style(Style::default().fg(if *error {
                    RED
                } else {
                    GREEN
                })),
                Rect::new(
                    1,
                    area.height.saturating_sub(2),
                    area.width.saturating_sub(2),
                    1,
                ),
            );
        }
        let footer = if self.inline_question() {
            "Questions · ↑↓ Choose · n Note · Enter Submit · Ctrl+] Skip · Alt+↑↓ Navigate · Esc Input"
        } else if matches!(self.modal, Some(Modal::Commands { .. })) {
            "Suggestions · Type to filter · ↑↓ Choose · Enter Select · Esc Input"
        } else {
            match self.focus {
                Focus::List => {
                    "Sessions · Tab Input · ↑↓ Select · Enter Open · f Filter · n New · / Actions · ? Help"
                }
                Focus::Filter => "Session filter · Enter/Esc List · Ctrl+U Clear",
                Focus::Composer => {
                    "Input · Tab Sessions · Cmd+↑ Messages · ↑ History · Enter Send · / Commands"
                }
                Focus::Conversation => {
                    "Messages · Type to reply · Cmd+↑↓ Navigate · Enter Expand · Tab Sessions · Ctrl/Cmd+C Copy"
                }
                Focus::Prompt => "Prompt · Enter Expand · Cmd+↓ Messages · Tab Sessions",
                Focus::Changes => "Changes · ↑↓ Scroll · Shift+↑↓ Select · c Copy · Tab Sessions",
            }
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(DIM)),
            Rect::new(
                1,
                area.height.saturating_sub(1),
                area.width.saturating_sub(2),
                1,
            ),
        );
        self.text_selection.highlight(frame);
        self.draw_modal(frame);
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
            matches!(self.focus, Focus::List | Focus::Filter),
        );
        self.hits.push((rect, Action::Focus(Focus::List)));
        let filter = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            3.min(area.height.saturating_sub(1)),
        );
        editor(
            frame,
            filter,
            "f Filter",
            &self.filter,
            self.focus == Focus::Filter && self.modal.is_none(),
        );
        self.hits.push((filter, Action::Focus(Focus::Filter)));
        let items = Rect::new(
            area.x,
            filter.bottom().saturating_add(1),
            area.width,
            area.height.saturating_sub(filter.height + 2),
        );
        let list = self.filtered();
        let selected = list
            .iter()
            .position(|s| Some(&s.id) == self.selected.as_ref())
            .unwrap_or(0);
        let capacity = (items.height as usize / 4).max(1);
        if selected < self.list_scroll {
            self.list_scroll = selected;
        }
        if selected >= self.list_scroll + capacity {
            self.list_scroll = selected.saturating_sub(capacity - 1);
        }
        for (offset, session) in list
            .iter()
            .skip(self.list_scroll)
            .take(capacity)
            .enumerate()
        {
            let selected = Some(&session.id) == self.selected.as_ref();
            let row = Rect::new(
                items.x,
                items.y.saturating_add((offset * 4) as u16),
                items.width,
                3.min(items.height.saturating_sub((offset * 4) as u16)),
            );
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
            let title_width = usize::from(row.width)
                .saturating_sub(unicode_width::UnicodeWidthStr::width(status.as_str()) + 3);
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{} {}",
                            if selected { "›" } else { " " },
                            crate::ui::crop(&session.title, 0, title_width)
                        ),
                        Style::default().fg(if selected { ACCENT } else { TEXT }),
                    ),
                    Span::styled(format!(" {status}"), Style::default().fg(color)),
                ]),
                Line::from(Span::styled(
                    format!(
                        "  {}{}",
                        session
                            .workspace
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
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
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().bg(if selected { PANEL } else { BG })),
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
    }
    fn draw_conversation(&mut self, frame: &mut Frame, rect: Rect) {
        let title = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(|s| format!("{} · {}", s.title, s.status.label()))
            .unwrap_or_else(|| "Conversation".into());
        let area = panel(
            frame,
            rect,
            &crate::model::clean(&title),
            matches!(
                self.focus,
                Focus::Conversation | Focus::Prompt | Focus::Composer
            ),
        );
        self.hits.push((rect, Action::Focus(Focus::Conversation)));
        let Some(id) = self.selected.clone() else {
            frame.render_widget(Paragraph::new("Launch a coding agent with n.\n\nSessions and review jobs keep running when difu closes.").wrap(Wrap { trim:false }).style(Style::default().fg(DIM)), area);
            return;
        };
        let Some(session) = self.sessions.get(&id).cloned() else {
            frame.render_widget(Paragraph::new("Loading session…"), area);
            return;
        };
        let coding = matches!(session.job, Job::Coding(_));
        if coding {
            self.restore_media(&id);
        }
        let voice_status = self.voice_status();
        let media_height = self.media_rows(area.width).min(area.height / 4);
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
            (lines + 2 + media_height + if voice_status.is_some() { 2 } else { 0 })
                .min(area.height.saturating_sub(3))
        } else {
            0
        };
        let mut activity_rows =
            transcript::activity(&session, area.width, chrono::Utc::now().timestamp_millis());
        let steering = session
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == "userMessage"
                    && session.turn_id.as_deref().is_some_and(|turn| {
                        entry.data.get("difuSteeringTurn").and_then(Value::as_str) == Some(turn)
                    })
            })
            .collect::<Vec<_>>();
        if !steering.is_empty() {
            activity_rows.push(Line::default());
            activity_rows.extend(
                wrapped(
                    "Messages to be submitted after the next tool call",
                    area.width,
                )
                .into_iter()
                .map(|text| Line::from(Span::styled(text, Style::default().fg(DIM)))),
            );
            for entry in steering {
                activity_rows.extend(
                    wrapped(
                        &format!("  ↳ {}", crate::model::clean(&entry.text)),
                        area.width,
                    )
                    .into_iter()
                    .map(|text| Line::from(Span::styled(text, Style::default().fg(DIM)))),
                );
            }
        }
        if !session.queue.is_empty() {
            activity_rows.push(Line::default());
            activity_rows.extend(
                wrapped(
                    if session
                        .pending
                        .iter()
                        .any(|p| p.id == "difu-missing-guidance")
                    {
                        "Messages queued until repository guidance is answered · click to edit"
                    } else {
                        "Messages queued for the next turn · click to edit"
                    },
                    area.width,
                )
                .into_iter()
                .map(|text| Line::from(Span::styled(text, Style::default().fg(DIM)))),
            );
            for prompt in &session.queue {
                activity_rows.extend(
                    wrapped(
                        &format!("  ↳ {}", crate::model::clean(prompt.text())),
                        area.width,
                    )
                    .into_iter()
                    .map(|text| Line::from(Span::styled(text, Style::default().fg(DIM)))),
                );
            }
        }
        let activity_height = (activity_rows.len().min(usize::from(area.height / 3))) as u16;
        let requests_height = u16::from(!session.pending.is_empty()) + activity_height;
        let (model, effort) = match &session.job {
            Job::Guide { model, .. } | Job::Conflict { model, .. } => {
                (model.model.as_str(), model.effort.as_str())
            }
            Job::Coding(_) => (
                session.model.as_deref().unwrap_or("Codex defaults"),
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
        let latest = session
            .entries
            .iter()
            .rev()
            .find(|e| e.kind == "userMessage");
        let body = if let Some(entry) = latest {
            self.prompt_header(frame, body, &entry.text)
        } else {
            body
        };
        self.conversation_height = usize::from(body.height);
        let position = self.positions.entry(id.clone()).or_default();
        let (mut lines, sections) = transcript::render(
            &session,
            position,
            body.width,
            self.focus == Focus::Conversation,
        );
        if lines.is_empty() {
            lines.push(Line::from("Preparing session…"));
        }
        if let Some(error) = &session.error
            && !session
                .entries
                .iter()
                .any(|e| e.kind == "error" && &e.text == error)
        {
            lines.extend(
                wrapped(error, body.width)
                    .into_iter()
                    .map(|text| Line::from(Span::styled(text, Style::default().fg(RED)))),
            );
        }
        self.conversation_lines = lines.len();
        let position = self.positions.entry(id.clone()).or_default();
        if let Some((width, height, version)) = position.transcript_viewport {
            if width != body.width || version != session.version {
                position.keep_transcript_position = false;
            } else if height != body.height {
                // Changing composer/question height must not move existing chat rows.
                position.keep_transcript_position = true;
            }
        }
        position.transcript_viewport = Some((body.width, body.height, session.version));
        if position.follow && !position.keep_transcript_position {
            position.conversation = lines.len().saturating_sub(body.height as usize);
        } else if let Some((id, offset)) = &position.scroll_anchor
            && let Some(section) = sections.iter().find(|section| &section.id == id)
        {
            position.conversation = section.row.saturating_add(*offset);
        }
        let maximum = if position.keep_transcript_position {
            lines.len().saturating_sub(1)
        } else {
            lines.len().saturating_sub(body.height as usize)
        };
        position.conversation = position.conversation.min(maximum);
        position.scroll_anchor = sections
            .iter()
            .rev()
            .find(|s| s.row <= position.conversation)
            .map(|s| (s.id.clone(), position.conversation.saturating_sub(s.row)));
        for (index, section) in sections.iter().enumerate() {
            let first = section.row.max(position.conversation);
            let last = sections
                .get(index + 1)
                .map_or(lines.len(), |next| next.row)
                .min(
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
        self.conversation_sections = sections;
        self.text_selection
            .register(1, body, position.conversation, &lines);
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(position.conversation)
                    .take(body.height as usize)
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        if !session.pending.is_empty() {
            self.button(
                frame,
                Rect::new(area.x, body.bottom(), area.width, 1),
                &format!(
                    "{} question(s) · {} approval(s) · Alt+↑ Questions",
                    session.pending_question_count(),
                    session
                        .pending
                        .iter()
                        .filter(|p| p.method != "item/tool/requestUserInput")
                        .count()
                ),
                Action::Pending,
                true,
            );
        }
        if activity_height > 0 {
            let activity_area = Rect::new(
                area.x,
                body.bottom() + u16::from(!session.pending.is_empty()),
                area.width,
                activity_height,
            );
            frame.render_widget(
                Paragraph::new(
                    activity_rows
                        .into_iter()
                        .take(usize::from(activity_height))
                        .collect::<Vec<_>>(),
                ),
                activity_area,
            );
            if !session.queue.is_empty() {
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
                return;
            }
            self.draw_media(
                frame,
                Rect::new(composer.x, composer.y, composer.width, media_height),
            );
            let composer = Rect::new(
                composer.x,
                composer.y.saturating_add(media_height),
                composer.width,
                composer.height.saturating_sub(media_height),
            );
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
            let position = self.positions.entry(id.clone()).or_default();
            editor(
                frame,
                composer,
                if let Some(status) = &voice_status {
                    status
                } else if self.busy {
                    "Sending… draft retained until acknowledged"
                } else if session.status.active() {
                    "Message · Enter steer · Ctrl+Enter queue"
                } else {
                    "Message · Enter send"
                },
                &position.draft,
                self.focus == Focus::Composer && self.modal.is_none(),
            );
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
    fn draw_changes(&mut self, frame: &mut Frame, rect: Rect) {
        let area = panel(
            frame,
            rect,
            "Changes since session start",
            self.focus == Focus::Changes,
        );
        self.hits.push((rect, Action::Focus(Focus::Changes)));
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(patch) = self.changes.get(&id) else {
            frame.render_widget(
                Paragraph::new(if self.changing {
                    "Reading local changes…"
                } else {
                    "Changes are available for coding sessions."
                })
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(DIM)),
                area,
            );
            return;
        };
        let lines: Vec<_> = patch.lines().collect();
        self.change_lines = lines.len();
        let p = self.positions.entry(id).or_default();
        p.changes = p.changes.min(lines.len().saturating_sub(1));
        let top = p
            .changes
            .saturating_sub(area.height as usize / 2)
            .min(lines.len().saturating_sub(area.height as usize));
        let rows = lines
            .iter()
            .enumerate()
            .skip(top)
            .take(area.height as usize)
            .map(|(index, line)| {
                let selected = p.selection.is_some_and(|anchor| {
                    (anchor.min(p.changes)..=anchor.max(p.changes)).contains(&index)
                });
                let color = if line.starts_with('+') {
                    GREEN
                } else if line.starts_with('-') {
                    RED
                } else if line.starts_with("@@") || line.starts_with("diff --git") {
                    ACCENT
                } else {
                    TEXT
                };
                Line::from(vec![
                    Span::styled(
                        if index == p.changes { "> " } else { "  " },
                        Style::default().fg(ACCENT),
                    ),
                    Span::styled(
                        crate::model::clean(line)
                            .chars()
                            .skip(p.horizontal)
                            .collect::<String>(),
                        Style::default()
                            .fg(color)
                            .bg(if selected { PANEL } else { BG }),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(if rows.is_empty() {
                vec![Line::from("No changes since session start")]
            } else {
                rows
            }),
            area,
        );
    }
    fn draw_modal(&mut self, frame: &mut Frame) {
        if self.modal.is_none()
            || self.inline_question()
            || matches!(self.modal, Some(Modal::Commands { .. }))
        {
            return;
        }
        if !matches!(self.modal, Some(Modal::Prompt { .. })) {
            self.text_selection.clear();
        }
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
            Some(Modal::InstallBrowser { .. }) => "Optional HTML preview · Esc cancels",
            Some(Modal::AgentDefaults(_)) => "New agent defaults · Tab fields · Esc cancels",
            Some(Modal::Prompt { .. }) => "Latest user prompt · Esc closes",
            Some(Modal::Voice { .. }) => "Voice settings",
            Some(Modal::Repository(_)) => "Choose and remember your default repository",
            Some(Modal::Menu { .. }) => "Agent actions",
            Some(Modal::Commands { .. }) => "Codex commands",
            Some(Modal::Status) => "Session status",
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
        if matches!(self.modal, Some(Modal::Model { .. })) {
            self.draw_model(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::AgentDefaults(_))) {
            self.draw_defaults(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Prompt { .. })) {
            self.draw_prompt(frame, area);
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
            Some(Modal::Model { .. } | Modal::Resources { .. } | Modal::InstallBrowser { .. }) => {}
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
                frame.render_widget(Paragraph::new("Stop this agent and permanently delete its difu chat, saved questions, queue, and attachments? Its clean, unlocked difu-owned worktree will also be removed.\n\nModified worktrees remain protected: deletion stops with an error and retains the chat. Existing directories, named branches, commits, and Codex’s own history are not deleted.\n\nEnter confirms · Esc cancels").wrap(Wrap { trim:false }), area);
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
                | Modal::Prompt { .. }
                | Modal::Voice { .. }
                | Modal::Commands { .. }
                | Modal::Status
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
        ("Ctrl+1 / Ctrl+2", "Switch Agents / Reviews"),
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
        ("Enter / Esc", "Open session / go back"),
        ("f / Ctrl+U", "Focus session filter / clear filter"),
        ("Tab / Shift+Tab", "Switch sessions list and message input"),
        ("Drag · Ctrl+C / Cmd+C", "Select and copy conversation text"),
        (
            "Ctrl+V / Cmd+V",
            "Paste text or media; expand a pasted-content token at the cursor",
        ),
        ("Ctrl+B", "Show or hide agents list (saved)"),
        (
            "Alt+P",
            "Move shell / artifact between side pane and main area",
        ),
        ("Ctrl+D", "Show or hide Changes pane (saved)"),
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
            "Search Codex commands / skills; /actions opens session controls",
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
        assert_eq!(ui.focus, Focus::Composer);
        assert!(ui.changes_visible);
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Keep this draft"
        );
        let (text, _) = draw(&mut ui, 120, 40)?;
        assert!(text.contains("Changes since session start"));
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
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("defaults");
        };
        form.fields = vec![
            Editor::from("/tmp/repo"),
            Editor::from("fixture-model"),
            Editor::from("medium"),
        ];
        form.isolated = false;
        for (width, height) in [(40, 12), (100, 30)] {
            draw(&mut ui, width, height)?;
        }
        ui.save_defaults();
        let saved = storage.load_config()?;
        assert!(saved.wrap_diff);
        assert!(!saved.agent_defaults.isolated);
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
    fn pinned_prompt_remains_inline_with_full_width_padding() -> Result<()> {
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
        assert_eq!(screen.matches("Keep this prompt in place").count(), 2);
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
    fn newest_prompt_stays_pinned_and_composer_grows_to_ten_lines() -> Result<()> {
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
                .map(|i| format!("Response line {i}\n"))
                .collect::<String>(),
        );
        let (first, _) = draw(&mut ui, 100, 40)?;
        assert!(first.contains("Newest user prompt"));
        assert!(!first.contains("Latest prompt"));
        let short = ui.conversation_height;
        ui.paste("line\n".repeat(14).as_str());
        draw(&mut ui, 100, 40)?;
        assert_eq!(short.saturating_sub(ui.conversation_height), 9);
        ui.focus = Focus::Conversation;
        ui.scroll(30, false);
        let (scrolled, _) = draw(&mut ui, 100, 40)?;
        assert!(scrolled.contains("Newest user prompt"));
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
        let composer = screen.find("Message · Enter steer").context("composer")?;
        assert!(activity < steering && steering < queued && queued < composer);
        assert!(screen.contains("git diff --stat"));
        assert!(screen.contains("Explain the result next"));
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
        ui.focus = Focus::Filter;
        ui.filter.insert("first\nsecond");
        ui.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(ui.filter.text(), "");
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
        assert!(screen.contains("preview one") && screen.contains("+1 lines"));
        assert!(!screen.contains("agentMessage"));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("hidden tool output"));
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
        ui.key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        ui.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(!ui.list_visible && ui.changes_visible);
        assert_eq!(ui.selected.as_deref(), Some("one"));
        let p = ui.positions.get("one").context("position")?;
        assert_eq!(p.draft.text(), "draft text");
        assert_eq!(p.conversation, 7);
        let restored = Ui::new(storage.clone(), &storage.load_config()?);
        assert!(!restored.list_visible && restored.changes_visible);
        ui.focus = Focus::Composer;
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(ui.focus == Focus::Composer);
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(ui.focus == Focus::Composer);
        let (screen, cursor) = draw(&mut ui, 140, 35)?;
        assert!(screen.contains("draft text") && screen.contains("Changes since session start"));
        assert!(cursor.is_some());
        Ok(())
    }
    #[test]
    fn filter_help_copy_and_all_input_layouts_remain_usable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        });
        ui.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        ui.paste("IMPLEMENT");
        assert_eq!(ui.filtered().len(), 1);
        ui.paste("missing");
        assert!(ui.filtered().is_empty());
        ui.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.selected = Some("one".into());
        ui.drilled = true;
        ui.focus = Focus::Changes;
        ui.changes_visible = true;
        ui.changes
            .insert("one".into(), "+    first\n+    second\n".into());
        ui.change_lines = 2;
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert_eq!(ui.clipboard.as_deref(), Some("    first\n    second"));
        ui.key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        ui.paste("Ctrl+D");
        let (screen, _) = draw(&mut ui, 100, 30)?;
        assert!(screen.contains("Show or hide Changes"));
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
