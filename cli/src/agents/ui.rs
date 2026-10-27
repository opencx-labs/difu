//! Native session UI. Network/process work is performed off the terminal thread.
use super::*;
mod browser;
mod cache_io;
mod changes;
mod codex_plan_picker;
mod commands;
mod defaults;
mod flap;
mod folders;
mod inbox;
mod inline_questions;
mod mcp_panel;
mod media;
mod models;
#[cfg(test)]
mod overlay_shortcut_tests;
mod panels;
mod patch;
mod permissions_panel;
mod plan_prompt;
mod prompt;
mod prs;
pub(crate) mod question_preview;
mod questions;
mod remote_control;
mod reveal;
#[cfg(test)]
pub(crate) mod review_shortcut_tests;
mod rules;
mod selection;
mod sends;
mod settings;
pub(crate) mod settings_preview;
#[cfg(test)]
mod shortcut_tests;
#[cfg(test)]
pub(crate) use overlay_shortcut_tests::prepare_slack_copy;
mod shortcuts;
mod sidebar;
mod slack;
mod status_icons;
mod terminals;
mod themes;
pub(crate) mod toast;
pub(in crate::agents) mod transcript;
mod transcript_window;
mod tweaks;
mod usage;
mod voice;
#[cfg(test)]
pub(crate) use browser::shortcut_fixture::{browser_keys, install_browser};
#[cfg(test)]
pub(crate) use voice::tests::active_dictation;
mod watch;
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
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    token: u64,
    text: String,
    queued: bool,
    observed_before: usize,
    failed: Option<Control>,
}
impl PendingSend {
    fn entry_id(&self) -> String {
        format!("difu-outgoing-{}", self.token)
    }
    fn in_chat(&self, session: &Session) -> bool {
        self.failed.is_some() || (!self.queued && !session.tool_running())
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
        self.failed.is_none() && Self::matching(session, &self.text) > self.observed_before
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
    pub change_pr: Option<crate::model::PrKey>,
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
    /// Reading intent: entry ID and unclamped intra-entry row. `conversation`
    /// is the current bounded display row; navigation clears/replaces this intent.
    pub scroll_anchor: Option<(String, usize)>,
    transcript_viewport: Option<(u16, u16, u64)>,
    keep_transcript_position: bool,
    question_height: Option<u16>,
    question_reflow: Option<inline_questions::Reflow>,
    question_scroll: bool,
    pub answers: HashMap<String, questions::Draft>,
    question_choices: HashMap<(String, usize), usize>,
    question_notes: HashMap<(String, usize, usize), Editor>,
    /// Checked options of multi-select questions, by request and question.
    question_checks: HashMap<(String, usize), std::collections::BTreeSet<usize>>,
    /// Chat animations from `/tweaks`.
    reply: Option<reveal::Reply>,
    replies_seen: bool,
    sent: Option<reveal::Typing>,
    clearing: Option<reveal::Typing>,
    /// Split-flap reveals of voice transcripts, by entry.
    voice_flaps: HashMap<String, flap::Flap>,
    /// The working indicator, flipping as it changes.
    activity: flap::Board,
    /// Messages waiting above the composer, flipping in as they are queued.
    waiting: flap::Board,
    /// Chat entries flipping as they appear and change, by entry.
    boards: HashMap<String, flap::Board>,
    /// Entries present when the session was first drawn, which never flip in.
    known: Option<HashSet<String>>,
}
#[derive(Clone)]
enum Action {
    InboxOpen,
    InboxItem(inbox::Key, Option<Box<Action>>),
    McpSelect(usize),
    PermissionTab(usize),
    PermissionItem(usize),
    PermissionField(usize),
    PermissionSave,
    PermissionSearch,
    OpenPlan,
    PlanChoice(usize),
    Select(String),
    OrchestratorBack,
    OpenFolder(String),
    FolderBack,
    FolderChoice(usize),
    Open,
    Focus(Focus),
    Resources(bool),
    PullRequest(usize),
    Resource(bool, usize),
    RefreshArtifact,
    ExternalArtifact,
    BrowserChoice(usize),
    ChangeFile(usize),
    ChangePr(usize),
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
    SlackField(usize),
    SlackSave,
    SlackDisable,
    TweaksField(usize),
    TweaksSave,
    ThemeChoice(usize),
    AnswerFocus,
    QueueEditorFocus,
    ChooseRepository,
    ChangeRepository,
    DefaultField(usize),
    DefaultOption(usize),
    ModelField(usize),
    ModelOption(usize),
    PickerOption(usize),
    DefaultSave,
    DefaultRules,
    RulesField(usize),
    RulesOption(usize),
    RulesSave,
    RulesCancel,
    Approve(usize),
    Answer(usize, String),
    InlineQuestion(usize),
    QuestionChoice(usize),
    QuestionNote(usize),
    SubmitQuestion(bool),
    DismissNotice,
}
pub enum Modal {
    Mcp(Box<mcp_panel::State>),
    Permissions(Box<permissions_panel::State>),
    Plan(Box<plan_prompt::Prompt>),
    Slack(slack::Settings),
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
    RepositoryRules(Box<rules::Settings>),
    Tweaks {
        fields: Box<[Editor; 4]>,
        field: usize,
    },
    Theme {
        selected: usize,
    },
    Remote,
    Repository(Editor),
    ChangeRepository(Editor),
    Commands {
        query: Editor,
        selected: usize,
        skills_only: bool,
        files_only: bool,
    },
    Status {
        scroll: usize,
    },
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
    /// Files the selected session; typing filters folders or names a new one.
    FolderPicker {
        query: Editor,
        selected: usize,
    },
    RenameFolder {
        id: String,
        name: Editor,
    },
    DeleteFolder(String),
    Model {
        model: Editor,
        effort: Editor,
        field: usize,
    },
    ModelPicker(Box<models::Picker>),
    Approval {
        pending: Pending,
        selected: usize,
        answers: Vec<Editor>,
        field: usize,
        scroll: usize,
    },
    Delete,
    Clear,
    Help(crate::help::State),
}
struct ResultMessage {
    kind: Task,
    result: std::result::Result<Reply, String>,
}
#[derive(Clone)]
enum Task {
    Mcp(String, u64),
    Permissions(String, u64),
    DecidePlan(String, String),
    List,
    Read(String),
    InboxRead(String),
    InboxRespond(inbox::Key),
    Changes(changes::Source, u64),
    ChangesContext(changes::Source, u64, String),
    Statistics(String, u64),
    Shells(String),
    Terminal(String, terminals::Operation),
    OpenArtifact,
    OpenLink,
    InstallBrowser(String, std::path::PathBuf, String),
    WorkspacePaths(String, std::path::PathBuf),
    Defaults(String),
    Skills(String, std::path::PathBuf, provider::Provider),
    Launch,
    Repository(String),
    Clear(String),
    Worktree(String),
    Usage(String),
    Action,
    Interrupt(String),
    Question,
    Delete(String),
    Send(String, u64),
}

pub struct Ui {
    pub(crate) preferences: crate::settings::Preferences,
    shortcut_config: Config,
    pub(crate) settings_requested: Option<crate::settings::Category>,
    panels: panels::Panels,
    prs: prs::State,
    inbox: inbox::State,
    pub defaults: crate::storage::AgentDefaults,
    pub selected: Option<String>,
    pub summaries: Vec<Summary>,
    pub sessions: HashMap<String, Session>,
    pub positions: HashMap<String, Position>,
    pub changes: HashMap<String, changes::Document>,
    change_errors: HashMap<String, String>,
    pub focus: Focus,
    pub drilled: bool,
    pub list_visible: bool,
    pub changes_visible: bool,
    pub pinned_sessions: std::collections::BTreeSet<String>,
    pub session_order: Vec<String>,
    pub archived: bool,
    orchestrator_list: bool,
    session_folders: Vec<crate::agents::folders::Folder>,
    /// The folder whose sessions the list shows; None is the main list.
    folder_view: Option<String>,
    /// A highlighted folder card in the main list; no session is selected then.
    folder_cursor: Option<String>,
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
    watch: Option<watch::Watch>,
    cache_writer: cache_io::Writer,
    changing: Option<(changes::Source, u64)>,
    change_request: u64,
    pub busy: bool,
    send_sequence: u64,
    send_repaint: bool,
    send_queues: HashMap<String, VecDeque<(u64, Control)>>,
    interrupting: HashSet<String>,
    deleting: HashSet<String>,
    question_send: Option<(String, Value, String, usize)>,
    plans: plan_prompt::State,
    question_reveal: bool,
    /// The question panel's split-flap.
    question_flip: inline_questions::Flip,
    /// Sidebar cards flipping as their text changes, by session.
    sidebar_boards: HashMap<String, flap::Board>,
    /// Whether the sidebar has listed sessions, which then show at once.
    sidebar_seen: bool,
    /// The sidebar's order and selection last drawn.
    sidebar_order: Vec<String>,
    sidebar_selected: Option<String>,
    /// Selection highlights flipping on or off, by session.
    sidebar_highlights: HashMap<String, flap::Highlight>,
    status_icons: status_icons::Icons,
    /// Last session drawn and the panel reveal started when its selection changed.
    drawn_session: Option<String>,
    session_switched: Option<Instant>,
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
    change_unified: bool,
    change_rows: std::sync::Arc<Vec<patch::SourceRow>>,
    skills: HashMap<String, (Vec<Skill>, Vec<String>)>,
    skills_loading: Option<String>,
    workspace_paths: HashMap<String, Vec<String>>,
    paths_loading: HashSet<String>,
    sidebar: sidebar::State,
    voice: voice::State,
    /// Chat animation timing from `/tweaks`.
    pub tweaks: crate::storage::Tweaks,
    /// Saved colour theme; `/theme` previews others only while open.
    theme: usize,
    slack: slack::State,
    remote: remote_control::State,
    model_completion: models::State,
    toast_rect: Rect,
    media_pending: Option<(String, mpsc::Receiver<Result<super::media::Paste, String>>)>,
}
impl Ui {
    pub(crate) fn notification_viewed_session(&self) -> Option<String> {
        if self.modal.is_some()
            || self.inbox.visible
            || (self.drilled && self.changes_visible)
            || (self.panels.visible() && !self.panels.right)
        {
            None
        } else {
            self.selected.clone()
        }
    }

    pub fn new(storage: Storage, config: &Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            preferences: config.preferences.clone(),
            shortcut_config: config.clone(),
            panels: panels::Panels::new(
                config.preferences.layout.helper(config.agent_panel_right)
                    != crate::settings::HelperPlacement::Main,
            ),
            prs: prs::State::load(&storage),
            inbox: inbox::State::default(),
            defaults: config.agent_defaults.clone(),
            selected: None,
            summaries: Vec::new(),
            sessions: HashMap::new(),
            positions: HashMap::new(),
            changes: HashMap::new(),
            change_errors: HashMap::new(),
            focus: Focus::List,
            drilled: false,
            list_visible: config.agent_list_visible,
            changes_visible: config.agent_changes_visible,
            pinned_sessions: config.pinned_sessions.clone(),
            session_order: config.session_order.clone(),
            archived: false,
            orchestrator_list: false,
            session_folders: config.session_folders.clone(),
            folder_view: None,
            folder_cursor: None,
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
            watch: None,
            cache_writer: Default::default(),
            changing: None,
            change_request: 0,
            busy: false,
            send_sequence: 0,
            send_repaint: false,
            send_queues: HashMap::new(),
            interrupting: HashSet::new(),
            deleting: HashSet::new(),
            question_send: None,
            plans: Default::default(),
            question_reveal: true,
            question_flip: Default::default(),
            sidebar_boards: HashMap::new(),
            sidebar_seen: false,
            sidebar_order: Vec::new(),
            sidebar_selected: None,
            sidebar_highlights: HashMap::new(),
            status_icons: status_icons::Icons::default(),
            drawn_session: None,
            session_switched: None,
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
            change_unified: config.unified,
            change_rows: Default::default(),
            skills: HashMap::new(),
            skills_loading: None,
            workspace_paths: HashMap::new(),
            paths_loading: HashSet::new(),
            settings_requested: None,
            voice: voice::State::new(config.voice_enabled),
            tweaks: config.tweaks,
            theme: crate::theme::index(config.theme.as_deref()),
            slack: slack::State::default(),
            remote: remote_control::State::default(),
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
    pub fn redraw_interval(&self) -> Option<Duration> {
        if self.panels.visible() && matches!(self.panels.view, Some(panels::View::Terminal(_))) {
            return Some(Duration::from_millis(50));
        }
        if self.preferences.motion.level == crate::settings::Motion::Full
            && (self.voice_active()
                || self.session_switched.is_some()
                || self.question_flip.animating(Instant::now())
                || self.inbox.animating()
                || !self.sidebar_highlights.is_empty()
                || self
                    .sidebar_boards
                    .values()
                    .any(|board| board.animating(Instant::now()))
                || self
                    .selected
                    .as_ref()
                    .and_then(|id| self.positions.get(id))
                    .is_some_and(Position::animating))
        {
            return Some(crate::repaint::FRAME_INTERVAL);
        }
        if self.busy
            || !self.deleting.is_empty()
            || self.notice.is_some()
            || self.summaries.iter().any(|s| {
                s.status.active()
                    && (self.selected.as_ref() == Some(&s.id)
                        || self.list_visible && self.prs.visible.contains(&s.id))
            })
        {
            return Some(
                if self.preferences.motion.level == crate::settings::Motion::Full {
                    crate::repaint::FRAME_INTERVAL
                } else {
                    Duration::from_secs(1)
                },
            );
        }
        let icons = if self.preferences.motion.level == crate::settings::Motion::Full
            && self.tweaks.agent_reply_cps > 0
        {
            self.status_icons.redraw_interval()
        } else {
            None
        };
        let panel = if let Some(panels::View::PullRequest { app }) = &self.panels.view {
            app.redraw_interval()
        } else {
            None
        };
        let user_command = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .is_some_and(|session| {
                session.entries.iter().any(|entry| {
                    entry.finished_at.is_none()
                        && entry.data.get("difuUserShell").and_then(Value::as_bool) == Some(true)
                })
            })
            .then_some(Duration::from_secs(1));
        icons.into_iter().chain(panel).chain(user_command).min()
    }
    pub fn tick(&mut self, visible: bool) {
        if let Some(error) = self.cache_writer.error() {
            self.notice = Some((error, true));
        }
        self.tick_watch(visible);
        self.tick_selection_click();
        self.tick_panels(visible);
        self.tick_prs(visible);
        if let Some(id) = self.selected.clone() {
            self.sync_change_pr(&id);
        }
        self.tick_models();
        self.tick_defaults();
        self.tick_rules();
        self.tick_media();
        self.tick_voice(visible);
        self.tick_slack();
        self.tick_remote();
        self.tick_theme();
        if visible {
            self.tick_mcp();
            self.tick_permissions();
        }
        while let Ok(message) = self.receiver.try_recv() {
            let unchanged = match (&message.kind, &message.result) {
                (Task::List, Ok(Reply::Sessions(sessions))) => {
                    sessions.len() == self.summaries.len()
                        && sessions
                            .iter()
                            .zip(&self.summaries)
                            .all(|(a, b)| a.id == b.id && a.version == b.version)
                }
                (_, Ok(Reply::Unchanged)) => true,
                _ => false,
            };
            if !unchanged {
                crate::repaint::request();
            }
            if let Task::InboxRespond(key) = &message.kind {
                self.inbox_response(key.clone(), message.result);
                continue;
            }
            if let Task::DecidePlan(id, plan_id) = &message.kind {
                self.finish_plan_choice(id, plan_id, message.result);
                continue;
            }
            if let Task::Terminal(id, operation) = &message.kind {
                self.finish_terminal(id, operation.clone(), message.result);
                continue;
            }
            if let Task::Send(id, token) = &message.kind {
                self.finish_send(id, *token, message.result);
                continue;
            }
            if let Task::ChangesContext(source, request, patch) = &message.kind {
                self.receive_change_context(source, *request, patch, message.result);
                continue;
            }
            if let Task::Changes(source, request) = &message.kind {
                self.receive_change_result(source, *request, message.result);
                continue;
            }
            match &message.kind {
                Task::Shells(_) => {
                    self.panels.loading = false;
                    self.panels.checked = Some(Instant::now());
                }
                Task::List => {
                    self.listing = false;
                    self.refreshed = Some(Instant::now());
                }
                Task::Read(_) | Task::InboxRespond(_) => {}
                Task::InboxRead(id) => {
                    self.inbox.reads.remove(id);
                }
                Task::Changes(..) | Task::ChangesContext(..) => {}
                Task::Launch
                | Task::Repository(_)
                | Task::Clear(_)
                | Task::Worktree(_)
                | Task::Action => self.busy = false,
                Task::Delete(id) => {
                    self.deleting.remove(id);
                }
                Task::Interrupt(id) => {
                    self.interrupting.remove(id);
                }
                Task::Defaults(_)
                | Task::Send(..)
                | Task::Terminal(..)
                | Task::Question
                | Task::OpenArtifact
                | Task::OpenLink
                | Task::InstallBrowser(..)
                | Task::Usage(_)
                | Task::Mcp(..)
                | Task::Permissions(..)
                | Task::DecidePlan(..) => {}
                Task::WorkspacePaths(id, _) => {
                    self.paths_loading.remove(id);
                }
                Task::Statistics(id, _) => {
                    self.sidebar.finished(id);
                }
                Task::Skills(..) => self.skills_loading = None,
            }
            if let Task::Permissions(id, request) = &message.kind {
                self.finish_permissions(id, *request, message.result);
                continue;
            }
            if let Task::Mcp(id, request) = &message.kind {
                if let Some(Modal::Mcp(state)) = &mut self.modal {
                    state.finish(id, *request, message.result);
                }
                continue;
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
                    if let Task::Delete(id) = &message.kind {
                        let title = self
                            .summaries
                            .iter()
                            .find(|s| &s.id == id)
                            .map(|s| s.title.as_str())
                            .unwrap_or(id);
                        self.notice = Some((format!("Could not delete {title}: {error}"), true));
                        self.refreshed = None;
                        self.ensure_selected();
                        continue;
                    }
                    if matches!(message.kind, Task::Question) {
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
                        if let Err(error) = self.sidebar.save(&self.storage, &self.cache_writer) {
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
                    (Task::List, Reply::Sessions(mut sessions)) => {
                        // List replies may race a newer subscription snapshot.
                        for summary in &mut sessions {
                            if let Some(current) = self.sessions.get(&summary.id)
                                && current.version > summary.version
                            {
                                *summary = current.summary();
                            }
                        }
                        self.sidebar.observe(&sessions);
                        if let Some(id) = self.selected.clone()
                            && self.summaries.iter().any(|s| s.id == id)
                            && !sessions.iter().any(|s| s.id == id)
                        {
                            self.sessions.remove(&id);
                            self.send_queues.remove(&id);
                            self.positions.remove(&id);
                            self.changes.remove(&id);
                            self.sidebar.counts.remove(&id);
                            let _ = self.sidebar.save(&self.storage, &self.cache_writer);
                            let _ = self.prs.forget(&id, &self.storage, &self.cache_writer);
                            self.question_send = None;
                            self.busy = false;
                            self.panels = panels::Panels::new(self.panels.right);
                            self.selected = None;
                            self.drilled = false;
                            self.modal = None;
                            self.focus = Focus::List;
                            self.notice = Some(("Chat deleted".into(), false));
                        }
                        self.summaries = sessions;
                        self.reload_folders();
                        self.ensure_selected();
                    }
                    (Task::Repository(id), Reply::Session(session)) => {
                        self.replaced_workspace(id, *session);
                    }
                    (Task::Clear(id), Reply::Session(session)) => {
                        if let Some(position) = self.positions.get_mut(&id) {
                            // Keep the composer; the old transcript, questions and
                            // change selections no longer exist.
                            *position = Position {
                                draft: std::mem::take(&mut position.draft),
                                skills: std::mem::take(&mut position.skills),
                                attachments: std::mem::take(&mut position.attachments),
                                image_count: position.image_count,
                                video_count: position.video_count,
                                media_loaded: position.media_loaded,
                                saved_attachments: std::mem::take(&mut position.saved_attachments),
                                follow: true,
                                ..Position::default()
                            };
                        }
                        self.replaced_workspace(id, *session);
                    }
                    (
                        Task::Read(id) | Task::InboxRead(id) | Task::Worktree(id),
                        Reply::Session(session),
                    ) => {
                        if self
                            .sessions
                            .get(&id)
                            .is_some_and(|current| current.version > session.version)
                        {
                            continue;
                        }
                        if !session.workspace_ready
                            && self.sessions.get(&id).is_some_and(|old| {
                                old.workspace_revision != session.workspace_revision
                            })
                        {
                            let _ = self.prs.forget(&id, &self.storage, &self.cache_writer);
                            if self.selected.as_ref() == Some(&id) {
                                self.panels = panels::Panels::new(self.panels.right);
                            }
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
                        if let Some(summary) = self.summaries.iter_mut().find(|s| s.id == id) {
                            *summary = session.summary();
                        }
                        self.sessions.insert(id.clone(), *session);
                        self.question_received(&id);
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
                        self.task(
                            Task::Read(id.clone()),
                            Request::Read {
                                id: id.clone(),
                                version: None,
                            },
                            false,
                        );
                        self.file_launched(&id);
                        self.folder_cursor = None;
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
                    (Task::Question, Reply::Ok) => {
                        self.refreshed = None;
                    }
                    (Task::OpenArtifact, Reply::Ok) => {
                        self.notice = Some(("Opened artifact in browser".into(), false));
                    }
                    (Task::Delete(id), Reply::Ok) => {
                        self.panels.shells.remove(&id);
                        self.sidebar.counts.remove(&id);
                        let _ = self.sidebar.save(&self.storage, &self.cache_writer);
                        self.sessions.remove(&id);
                        self.send_queues.remove(&id);
                        self.positions.remove(&id);
                        self.changes.remove(&id);
                        self.summaries.retain(|s| s.id != id);
                        let _ = self.prs.forget(&id, &self.storage, &self.cache_writer);
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
        self.tick_inbox(visible);
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
            self.sync_change_pr(&id);
            let source = self.change_source(&id);
            if self.drilled
                && self.changes_visible
                && self
                    .changing
                    .as_ref()
                    .is_none_or(|(pending, _)| pending != &source)
                && self.changes_at.is_none_or(|at| {
                    at.elapsed() >= Duration::from_secs(if source.pr.is_some() { 30 } else { 2 })
                })
                && self.sessions.get(&id).is_some_and(|s| {
                    matches!(s.job, Job::Coding(_))
                        && (source.pr.is_some()
                            || s.workspace_ready && s.baseline.is_some() && !s.workspace_removed)
                })
            {
                self.change_request = self.change_request.wrapping_add(1);
                self.changing = Some((source.clone(), self.change_request));
                let request = match &source.pr {
                    Some(pr) => Request::PrChanges { id, pr: pr.clone() },
                    None => Request::Changes { id },
                };
                self.task(Task::Changes(source, self.change_request), request, false);
            }
        }
        if visible {
            self.request_change_context();
        }
        self.tick_plan_prompt(visible);
    }
    fn filtered(&self) -> Vec<Summary> {
        let filed = self.filed();
        let mut sessions = self
            .summaries
            .iter()
            .filter(|s| s.kind != "Guide" && s.archived == self.archived)
            .filter(|s| {
                if self.orchestrator_list {
                    s.kind == "Orchestrator"
                } else {
                    s.kind != "Orchestrator" || s.id == super::orchestrator::ID
                }
            })
            // Filed sessions show only inside their folder.
            .filter(|s| {
                self.orchestrator_list
                    || filed.get(s.id.as_str()).copied() == self.folder_view.as_deref()
            })
            .cloned()
            .collect::<Vec<_>>();
        let positions: HashMap<_, _> = self
            .session_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        sessions.sort_by(|a, b| {
            (a.id != super::orchestrator::ID)
                .cmp(&(b.id != super::orchestrator::ID))
                .then_with(|| {
                    self.pinned_sessions
                        .contains(&b.id)
                        .cmp(&self.pinned_sessions.contains(&a.id))
                })
                .then_with(|| match self.preferences.layout.session_sort {
                    crate::settings::SessionSort::Manual => std::cmp::Ordering::Equal,
                    crate::settings::SessionSort::Recent => b.updated.cmp(&a.updated),
                    crate::settings::SessionSort::ActiveFirst => b
                        .status
                        .active()
                        .cmp(&a.status.active())
                        .then_with(|| b.updated.cmp(&a.updated)),
                })
                .then_with(|| positions.get(&a.id).cmp(&positions.get(&b.id)))
                // Keep the creation-ID fallback stable inside each ordering group.
                .then_with(|| b.id.cmp(&a.id))
        });
        if self.orchestrator_list
            && let Some(root) = sessions
                .iter_mut()
                .find(|s| s.id == super::orchestrator::ID)
        {
            root.title = "Difu chat".into();
        }
        sessions
    }
    fn open_orchestrator_list(&mut self) {
        if self.inbox.visible {
            self.close_questions_inbox();
        }
        self.orchestrator_list = true;
        self.drilled = false;
        self.focus = Focus::List;
        self.list_scroll = 0;
        self.ensure_selected();
    }
    fn close_orchestrator_list(&mut self) {
        self.orchestrator_list = false;
        self.folder_view = None;
        self.folder_cursor = None;
        self.drilled = false;
        self.archived = false;
        self.focus = Focus::List;
        self.list_scroll = 0;
        self.select(super::orchestrator::ID.into());
    }
    fn reorder_session(&mut self, down: bool) {
        if self.folder_cursor.is_some() {
            self.reorder_folder(down);
            return;
        }
        let list = self.filtered();
        let Some(index) = list
            .iter()
            .position(|s| Some(&s.id) == self.selected.as_ref())
        else {
            return;
        };
        let Some(next) = (if down {
            index.checked_add(1)
        } else {
            index.checked_sub(1)
        })
        .and_then(|index| list.get(index)) else {
            return;
        };
        let Some(current) = list.get(index) else {
            return;
        };
        if current.id == super::orchestrator::ID
            || next.id == super::orchestrator::ID
            || self.deleting.contains(&current.id)
            || self.deleting.contains(&next.id)
            || self.pinned_sessions.contains(&current.id) != self.pinned_sessions.contains(&next.id)
        {
            return;
        }
        let mut order: Vec<_> = self
            .summaries
            .iter()
            .filter(|s| !self.session_order.contains(&s.id))
            .map(|s| s.id.clone())
            .collect();
        order.sort_by(|a, b| b.cmp(a));
        order.extend(self.session_order.iter().cloned());
        let Some(from) = order.iter().position(|id| id == &current.id) else {
            return;
        };
        let Some(to) = order.iter().position(|id| id == &next.id) else {
            return;
        };
        order.swap(from, to);
        let result = self
            .storage
            .update_config(|config| config.session_order.clone_from(&order));
        match result {
            Ok(_) => {
                if let Some(panels::View::PullRequest { app }) = &mut self.panels.view {
                    app.config.session_order.clone_from(&order);
                }
                self.session_order = order;
            }
            Err(error) => {
                self.notice = Some((format!("Could not save session order: {error:#}"), true))
            }
        }
    }
    /// A new worktree invalidates cached skills, paths, changes, PRs and panes.
    fn replaced_workspace(&mut self, id: String, session: Session) {
        self.skills.remove(&id);
        self.workspace_paths.remove(&id);
        self.changes.remove(&id);
        self.sidebar.counts.remove(&id);
        if let Err(error) = self.prs.forget(&id, &self.storage, &self.cache_writer) {
            self.notice = Some((format!("Cannot clear cached PR: {error:#}"), true));
        }
        self.panels = panels::Panels::new(self.panels.right);
        self.summaries.retain(|s| s.id != id);
        self.summaries.push(session.summary());
        self.sessions.insert(id, session);
        self.modal = None;
        self.refreshed = None;
    }
    fn ensure_selected(&mut self) {
        if self.inbox.visible {
            return;
        }
        let rows = self.selectable_rows();
        if let Some(cursor) = self.folder_cursor.as_deref() {
            if rows
                .iter()
                .any(|row| matches!(row, folders::Row::Folder(card) if card.id == cursor))
            {
                return;
            }
            self.folder_cursor = None;
        }
        let listed = rows.iter().any(
            |row| matches!(row, folders::Row::Session(s) if Some(&s.id) == self.selected.as_ref()),
        );
        if !listed && !self.drilled {
            match rows.into_iter().next() {
                Some(folders::Row::Folder(card)) => self.focus_folder(card.id),
                Some(folders::Row::Session(session)) => self.selected = Some(session.id),
                None => self.selected = None,
            }
        }
    }
    fn select(&mut self, id: String) {
        if self.inbox.visible {
            self.close_questions_inbox();
        }
        if self.deleting.contains(&id) {
            return;
        }
        let changed = self.selected.as_ref() != Some(&id);
        if changed && self.inline_question() {
            self.remember_answers();
            self.modal = None;
        }
        self.folder_cursor = None;
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
        let rows = self.selectable_rows();
        let inbox_row = !self.orchestrator_list && self.folder_view.is_none();
        let cursor = self.cursor();
        let current = if inbox_row && self.inbox.visible {
            0
        } else {
            rows.iter()
                .position(|row| Some(row.key()) == cursor)
                .unwrap_or(0)
                + usize::from(inbox_row)
        };
        let index = current
            .saturating_add_signed(delta as isize)
            .min((rows.len() + usize::from(inbox_row)).saturating_sub(1));
        if inbox_row && index == 0 {
            self.open_questions_inbox();
            self.focus = Focus::List;
            return;
        }
        match rows.into_iter().nth(index - usize::from(inbox_row)) {
            Some(folders::Row::Folder(card)) => self.focus_folder(card.id),
            Some(folders::Row::Session(session)) => self.select(session.id),
            None => {}
        }
        self.focus = Focus::List;
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
    pub(crate) fn menu_entries(&self) -> Vec<String> {
        if self.folder_cursor.is_some() {
            return self.folder_menu_entries();
        }
        let archived = self
            .sessions
            .get(self.selected.as_deref().unwrap_or_default())
            .is_some_and(|s| s.archived);
        let mut entries = vec![
            "New coding agent",
            "Continue session",
            "Interrupt / cancel job",
            "Rename session",
            "Change model / reasoning · Alt+M",
            if archived {
                "Unarchive session"
            } else {
                "Archive session"
            },
            "Show active / archived sessions",
            "Toggle agents list · Alt+[",
            "Toggle Changes · Alt+D",
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
            "Repository rules",
            "Permissions and mode · Shift+Tab",
            "Move to folder · F",
        ];
        if self.plan_available() {
            entries.push("Review completed plan");
        }
        use crate::shortcuts::{Bindings, ids::*};
        let bindings = Bindings::new(&self.shortcut_config);
        entries
            .into_iter()
            .map(|label| {
                let replacement = match label {
                    "Change model / reasoning · Alt+M" => {
                        Some(("Change model / reasoning", AGENTS_MODELS))
                    }
                    "Toggle agents list · Alt+[" => Some(("Toggle agents list", AGENTS_SIDEBAR)),
                    "Toggle Changes · Alt+D" => Some(("Toggle Changes", AGENTS_CHANGES)),
                    "Pending questions · Alt+↑" => Some(("Pending questions", QUESTION_OPEN)),
                    "Move shell / artifact pane · Alt+P" => {
                        Some(("Move shell / artifact pane", HELPER_PLACEMENT))
                    }
                    "Permissions and mode · Shift+Tab" => {
                        Some(("Permissions and mode", COMPOSER_PERMISSION_MODE))
                    }
                    "Move to folder · F" => Some(("Move to folder", FOLDER_MOVE)),
                    _ => None,
                };
                replacement.map_or_else(
                    || label.into(),
                    |(label, id)| format!("{label} · {}", bindings.hint(id)),
                )
            })
            .collect()
    }
    pub(crate) fn menu_action(&mut self, index: usize) {
        if self.folder_cursor.is_some() {
            self.folder_menu_action(index);
            return;
        }
        match index {
            21 => self.open_plan_prompt(),
            20 => {
                self.modal = None;
                self.open_folder_picker();
            }
            19 => self.open_permissions(),
            18 => self.open_rules(None),
            17 => {
                if let Some(id) = self.selected.clone() {
                    self.toggle_pin(&id);
                    self.modal = None;
                }
            }
            14 => self.open_resources(false),
            15 => self.open_resources(true),
            16 => {
                self.modal = None;
                self.move_helper();
            }
            13 => self.modal = Some(Modal::Delete),
            12 => self.open_defaults(),
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
            4 => self.open_model_picker(),
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
            10 => self.modal = Some(Modal::Queue { selected: 0 }),
            11 => self.modal = Some(Modal::Pending { selected: 0 }),
            9 => {
                self.review_requested = true;
                self.modal = None;
            }
            _ => {}
        }
    }
    pub(crate) fn toggle_pin(&mut self, id: &str) {
        let result = self
            .storage
            .update_config(|config| {
                if !config.pinned_sessions.remove(id) {
                    config.pinned_sessions.insert(id.to_owned());
                }
            })
            .map(|config| config.pinned_sessions);
        match result {
            Ok(pins) => self.pinned_sessions = pins,
            Err(error) => {
                self.notice = Some((format!("Could not save session pin: {error:#}"), true))
            }
        }
    }
    pub(crate) fn open_session(&mut self, id: String) {
        if self.deleting.contains(&id) {
            return;
        }
        self.orchestrator_list = id != super::orchestrator::ID
            && self
                .summaries
                .iter()
                .any(|s| s.id == id && s.kind == "Orchestrator");
        self.reveal_in_folder(&id);
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
        if !self.panels.focused || self.panels.session != self.selected {
            return None;
        }
        match &self.panels.view {
            Some(panels::View::PullRequest { app }) if self.panels.visible() => Some(app),
            _ => None,
        }
    }
    pub(crate) fn palette_review_mut(&mut self) -> Option<&mut crate::app::App> {
        self.palette_review()?;
        match &mut self.panels.view {
            Some(panels::View::PullRequest { app }) => Some(app),
            _ => None,
        }
    }
    fn save_visibility(&mut self, moved_helper: bool) {
        let (list, changes) = (self.list_visible, self.changes_visible);
        let result = self.storage.update_config(|config| {
            config.agent_list_visible = list;
            config.agent_changes_visible = changes;
            if moved_helper {
                config.agent_panel_right = true;
                config.preferences.layout.helper_placement = None;
            }
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
        self.save_visibility(false);
    }
    pub fn toggle_changes(&mut self) {
        self.panels.focused = false;
        self.changes_visible = !self.changes_visible;
        let moved_helper = self.changes_visible && self.panels.view.is_some() && !self.panels.right;
        if moved_helper {
            self.panels.right = true;
            self.preferences.layout.helper_placement = None;
        }
        self.drilled = true;
        self.focus = if self.changes_visible {
            Focus::ChangeTree
        } else {
            Focus::Composer
        };
        self.changes_at = None;
        self.save_visibility(moved_helper);
    }
    fn composer_suggestion(&self) -> Option<&str> {
        let id = self.selected.as_ref()?;
        let position = self.positions.get(id)?;
        if !position.draft.chars.is_empty() || !position.active_attachments().is_empty() {
            return None;
        }
        let session = self.sessions.get(id)?;
        if self.plan_available() {
            return None;
        }
        session
            .suggestion
            .as_ref()
            .filter(|suggestion| suggestion.current(session))
            .map(|suggestion| suggestion.text.as_str())
    }
    fn pending(&mut self, index: usize) {
        self.remember_answers();
        if let Some(pending) = self
            .sessions
            .get(self.selected.as_deref().unwrap_or_default())
            .and_then(|s| s.pending.get(index))
            .cloned()
        {
            if pending.params.get("difuClaudePlan").is_some() && !self.inbox.visible {
                self.open_plan_prompt();
                return;
            }
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
        if self.inbox.visible
            && matches!(&self.modal, Some(Modal::Approval { pending, .. }) if pending.params.get("difuClaudePlan").is_some())
        {
            self.close_questions_inbox();
            self.changes_visible = false;
            self.open_plan_prompt();
            return;
        }
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
                            if super::elicitation::is_form(pending) {
                                continue;
                            }
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
            _ => {
                self.control(Control::Interrupt);
                self.notice = Some((
                    "Request stopped. Ask the agent to use the questions tool.".into(),
                    false,
                ));
                return;
            }
        };
        let request = pending.id.clone();
        if self.inbox.visible {
            self.respond_inbox(request, response);
        } else {
            self.control(Control::Respond { request, response });
        }
    }
    fn focused_editor(&self) -> Option<&Editor> {
        if self.inbox.visible && self.focus == Focus::List {
            return None;
        }
        if self.question_note_focused
            && let Some(note) = self.question_note()
        {
            return Some(note);
        }
        match &self.modal {
            Some(Modal::Repository(editor) | Modal::ChangeRepository(editor)) => Some(editor),
            Some(Modal::AgentDefaults(form)) => form.fields.get(form.field),
            Some(Modal::RepositoryRules(form)) => match form.field {
                0 => Some(&form.repository),
                1 => Some(&form.text),
                _ => None,
            },
            Some(Modal::Menu { query, .. } | Modal::Commands { query, .. }) => Some(query),
            Some(Modal::Help(state)) => Some(&state.query),
            Some(Modal::Permissions(state)) => state.editor(),
            Some(Modal::Plan(state)) => state.editor(),
            Some(
                Modal::Rename(editor)
                | Modal::FolderPicker { query: editor, .. }
                | Modal::RenameFolder { name: editor, .. }
                | Modal::QueuedEdit {
                    editor, focus: 0, ..
                },
            ) => Some(editor),
            Some(Modal::Tweaks { fields, field }) => fields.get(*field),
            Some(Modal::Slack(form)) => form.copy_editor(),
            Some(Modal::Model {
                model,
                effort,
                field,
            }) => Some(if *field == 0 { model } else { effort }),
            Some(Modal::Approval {
                pending,
                answers,
                field,
                ..
            }) if pending.method == "item/tool/requestUserInput" => answers.get(*field),
            None if !self.inbox.visible && self.focus == Focus::Composer && self.drilled => self
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
        use crate::shortcuts::ids::*;
        if self.inbox_key(key) {
            return;
        }
        if self.shortcut_context() == crate::shortcuts::contexts::AGENT_CHANGES
            && matches!(self.shortcut(key), Some(NAVIGATION_FIRST | NAVIGATION_LAST))
            && let Some(document) = self
                .selected
                .as_ref()
                .and_then(|id| self.changes.get_mut(id))
        {
            document.clear_reading();
        }
        self.question_flip.interrupt();
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
        if ((key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL)
            || self.shortcut(key) == Some(SELECTION_COPY))
            && self.copy_chat_selection()
        {
            return;
        }
        if self.shortcut(key) == Some(SELECTION_COPY) && self.focused_editor().is_some() {
            return;
        }
        if matches!(self.modal, Some(Modal::Plan(_))) {
            self.plan_key(key);
            return;
        }
        if self.question_key(key) {
            return;
        }
        if self.voice_key(key) {
            return;
        }
        let shortcut = self.shortcut(key);
        if shortcut == Some(AGENTS_SIDEBAR) {
            self.toggle_list();
            return;
        }
        if shortcut == Some(AGENTS_CHANGES) {
            self.toggle_changes();
            return;
        }
        if shortcut == Some(AGENTS_MODELS) {
            if matches!(self.modal, Some(Modal::ModelPicker(_))) {
                self.modal = None;
            } else {
                self.open_model_picker();
            }
            return;
        }
        if self.shortcut(key) == Some(crate::shortcuts::ids::QUESTION_OPEN) {
            self.open_questions();
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            self.remember_answers();
            return;
        }
        if self.focus == Focus::List
            && matches!(shortcut, Some(SESSION_REORDER_UP | SESSION_REORDER_DOWN))
        {
            self.reorder_session(shortcut == Some(SESSION_REORDER_DOWN));
            return;
        }
        if self.focus == Focus::List {
            match shortcut {
                Some(FOLDER_MOVE) => return self.open_folder_picker(),
                Some(FOLDER_RENAME) => return self.open_rename_folder(),
                Some(FOLDER_DELETE) => return self.open_delete_folder(),
                _ => {}
            }
        }
        if self.drilled
            && matches!(shortcut, Some(CONVERSATION_PREVIOUS | CONVERSATION_NEXT))
            && matches!(self.focus, Focus::Composer | Focus::Conversation)
        {
            if self.focus == Focus::Composer {
                if shortcut == Some(CONVERSATION_PREVIOUS) {
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
                self.move_transcript(shortcut == Some(CONVERSATION_NEXT));
            }
            return;
        }
        if self.focus == Focus::List
            && shortcut == Some(NAVIGATION_ACTIVATE)
            && !self.orchestrator_list
            && self.selected.as_deref() == Some(super::orchestrator::ID)
        {
            self.open_orchestrator_list();
            return;
        }
        if self.orchestrator_list && !self.drilled && shortcut == Some(AGENTS_BACK) {
            self.close_orchestrator_list();
            return;
        }
        if self.focus == Focus::List
            && shortcut == Some(NAVIGATION_ACTIVATE)
            && let Some(folder) = self.folder_cursor.clone()
        {
            self.open_folder(folder);
            return;
        }
        if self.folder_view.is_some() && !self.drilled && shortcut == Some(AGENTS_BACK) {
            self.close_folder();
            return;
        }
        if self.changes_visible
            && self.drilled
            && matches!(shortcut, Some(AGENTS_BACK | CONVERSATION_INTERRUPT))
        {
            self.toggle_changes();
            return;
        }
        if self.changes_visible
            && self.drilled
            && !self.panels.focused
            && matches!(self.focus, Focus::Changes | Focus::ChangeTree)
            && matches!(shortcut, Some(CHANGES_PREVIOUS_PR | CHANGES_NEXT_PR))
        {
            self.cycle_change_pr(shortcut == Some(CHANGES_NEXT_PR));
            return;
        }
        if shortcut == Some(CONVERSATION_INTERRUPT)
            && self.drilled
            && matches!(self.focus, Focus::Composer | Focus::Conversation)
            && let Some(session) = self.selected.as_ref().and_then(|id| self.sessions.get(id))
        {
            // An unanswered synchronous permission must survive incidental or
            // repeated Escape events. Open its explicit decision instead.
            if let Some(index) = synchronous_permission_index(session) {
                self.pending(index);
                return;
            }
            if session.can_send_waiting() {
                self.interrupt_chat(Control::InterruptAndSend);
                return;
            }
            if session.status.active() {
                self.interrupt_chat(Control::Interrupt);
                return;
            }
        }
        if matches!(
            shortcut,
            Some(NAVIGATION_UP | NAVIGATION_DOWN | NAVIGATION_FAST_UP | NAVIGATION_FAST_DOWN)
        ) && self.step_change_file(matches!(
            shortcut,
            Some(NAVIGATION_DOWN | NAVIGATION_FAST_DOWN)
        )) {
            return;
        }
        if self.focus == Focus::ChangeTree && self.changes_visible {
            match shortcut {
                Some(NAVIGATION_ACTIVATE) => {
                    self.focus = Focus::Changes;
                    return;
                }
                Some(NAVIGATION_LEFT | NAVIGATION_RIGHT) => {
                    if let Some(p) = self
                        .selected
                        .as_ref()
                        .and_then(|id| self.positions.get_mut(id))
                    {
                        p.change_tree_horizontal = p.change_tree_horizontal.saturating_add_signed(
                            if shortcut == Some(NAVIGATION_LEFT) {
                                -4
                            } else {
                                4
                            },
                        );
                    }
                    return;
                }
                Some(NAVIGATION_FIRST | NAVIGATION_LAST) => {
                    self.move_change_tree(if shortcut == Some(NAVIGATION_FIRST) {
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
            if shortcut == Some(COMPOSER_PERMISSION_MODE)
                && !self.changes_visible
                && !self.panels.focused
                && !self.question_panel()
            {
                self.cycle_permission_mode();
                return;
            }
            if shortcut == Some(COMPOSER_SUGGESTION)
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
            if shortcut == Some(COMPOSER_ATTACH) {
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
            if matches!(shortcut, Some(COMPOSER_SKILLS | COMPOSER_PATHS))
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
                if shortcut == Some(COMPOSER_PATHS) {
                    self.open_paths();
                } else {
                    self.open_commands(true);
                }
                return;
            }
            match shortcut {
                Some(AGENTS_COMMANDS)
                    if self
                        .selected
                        .as_ref()
                        .and_then(|id| self.positions.get(id))
                        .is_none_or(|p| p.draft.chars.is_empty()) =>
                {
                    self.open_commands(false)
                }
                Some(CONVERSATION_INTERRUPT) => {
                    self.drilled = false;
                    self.focus = Focus::List;
                }
                Some(
                    NAVIGATION_NEXT_FOCUS
                    | NAVIGATION_NEXT_FOCUS_COMPOSER
                    | NAVIGATION_PREVIOUS_FOCUS,
                ) => self.cycle_focus(shortcut == Some(NAVIGATION_PREVIOUS_FOCUS)),
                Some(COMPOSER_QUEUE) => self.send(true),
                Some(COMPOSER_SEND) => self.send_primary(),
                Some(COMPOSER_NEWLINE) => {
                    if let Some(id) = self.selected.clone() {
                        self.positions.entry(id).or_default().draft.insert("\n");
                    }
                }
                _ => {
                    if let Some(id) = self.selected.clone() {
                        self.positions.entry(id).or_default().draft.key(key);
                    }
                }
            }
            return;
        }
        match shortcut {
            Some(SESSION_NEW) => self.launch(),
            Some(AGENTS_COMMANDS) if self.drilled && self.focus != Focus::List => {
                self.open_commands(false)
            }
            Some(AGENTS_COMMANDS) => {
                self.modal = Some(Modal::Menu {
                    query: Editor::default(),
                    selected: 0,
                })
            }
            Some(AGENTS_HELP) => self.modal = Some(Modal::Help(Default::default())),
            Some(SESSION_REFRESH) => {
                self.refreshed = None;
                self.changes_at = None;
                if let Some(id) = &self.selected {
                    self.prs.refresh(id);
                }
            }
            Some(NAVIGATION_ACTIVATE)
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
            Some(NAVIGATION_ACTIVATE) if self.changes_visible && self.focus == Focus::List => {
                if self.selected.is_some() {
                    self.follow_latest();
                    self.drilled = true;
                    self.focus = Focus::ChangeTree;
                    self.changes_at = None;
                }
            }
            Some(CONVERSATION_COMPOSE | NAVIGATION_ACTIVATE) if self.drilled => {
                if self.focus == Focus::List {
                    self.follow_latest();
                }
                self.focus = Focus::Composer;
            }
            Some(NAVIGATION_ACTIVATE) => {
                if self.selected.is_some() {
                    self.follow_latest();
                    self.drilled = true;
                    self.focus = Focus::Composer;
                    self.changes_at = None;
                }
            }
            Some(AGENTS_BACK | CONVERSATION_INTERRUPT) => {
                self.drilled = false;
                self.focus = Focus::List;
            }
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                self.cycle_focus(shortcut == Some(NAVIGATION_PREVIOUS_FOCUS))
            }
            Some(NAVIGATION_UP | NAVIGATION_DOWN) if self.focus == Focus::Conversation => {
                self.move_transcript(shortcut == Some(NAVIGATION_DOWN));
            }
            Some(
                NAVIGATION_UP | NAVIGATION_DOWN | NAVIGATION_PAGE_UP | NAVIGATION_PAGE_DOWN
                | NAVIGATION_FAST_UP | NAVIGATION_FAST_DOWN | CHANGES_SELECT_UP
                | CHANGES_SELECT_DOWN,
            ) => {
                let down = matches!(
                    shortcut,
                    Some(
                        NAVIGATION_DOWN
                            | NAVIGATION_PAGE_DOWN
                            | NAVIGATION_FAST_DOWN
                            | CHANGES_SELECT_DOWN
                    )
                );
                let amount = if matches!(shortcut, Some(NAVIGATION_PAGE_UP | NAVIGATION_PAGE_DOWN))
                {
                    self.viewport.max(1)
                } else if matches!(shortcut, Some(NAVIGATION_FAST_UP | NAVIGATION_FAST_DOWN)) {
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
                    matches!(shortcut, Some(CHANGES_SELECT_UP | CHANGES_SELECT_DOWN)),
                );
            }
            Some(NAVIGATION_LEFT | NAVIGATION_RIGHT) if self.focus == Focus::Changes => {
                if let Some(id) = self.selected.clone() {
                    let p = self.positions.entry(id).or_default();
                    p.horizontal =
                        p.horizontal
                            .saturating_add_signed(if shortcut == Some(NAVIGATION_LEFT) {
                                -4
                            } else {
                                4
                            });
                }
            }
            Some(NAVIGATION_LAST) => {
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
            Some(NAVIGATION_FIRST) => {
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
            Some(SELECTION_COPY) if self.focus == Focus::Changes => self.copy_change(),
            _ => {}
        }
    }
    fn cycle_focus(&mut self, backwards: bool) {
        let helper = self.drilled && self.panels.visible();
        // Visible panes cycle in screen order: sessions, composer, helper canvas.
        let mut targets = Vec::new();
        if self.list_visible {
            targets.push(0);
        }
        if !(helper && !self.panels.right) {
            if self.changes_visible && self.drilled {
                targets.extend([3, 4]);
            } else {
                targets.push(1);
            }
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
        if self.focus == Focus::Changes
            && let Some(document) = self
                .selected
                .as_ref()
                .and_then(|id| self.changes.get_mut(id))
        {
            document.clear_reading();
        }
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
                p.question_reflow = None;
                p.conversation = p.conversation.saturating_add_signed(delta as isize).min(
                    self.conversation_lines
                        .saturating_sub(self.conversation_height),
                );
            }
        }
    }
    fn copy_change(&mut self) {
        if let Some(id) = &self.selected
            && let Some(position) = self.positions.get(id)
        {
            self.clipboard = self
                .changes
                .get(id)
                .and_then(|d| d.copied_source(position))
                .or_else(|| changes::copy_rows(&self.change_rows, position));
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.terminal_input() {
            self.terminal_paste(text);
            return;
        }
        if self.inbox.visible && (self.modal.is_none() || self.focus == Focus::List) {
            return;
        }
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
            Some(Modal::Permissions(state)) => state.paste(text),
            Some(Modal::Plan(state)) => state.paste(text),
            Some(Modal::Slack(_)) => self.slack_paste(text),
            Some(Modal::RepositoryRules(_)) => self.rules_paste(text),
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
                | Modal::FolderPicker { query: e, .. }
                | Modal::RenameFolder { name: e, .. }
                | Modal::QueuedEdit { editor: e, .. },
            ) => e.insert(text),
            Some(Modal::Tweaks { fields, field }) => {
                if let Some(editor) = fields.get_mut(*field) {
                    editor.insert(text);
                }
            }
            Some(Modal::Model { .. }) => self.model_paste(text),
            Some(Modal::Approval {
                answers,
                field,
                pending,
                selected,
                ..
            }) if pending.method == "item/tool/requestUserInput" => {
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
                    self.positions
                        .entry(id)
                        .or_default()
                        .draft
                        .paste_with_threshold(text, self.preferences.input.paste_threshold);
                }
            }
            _ => {}
        }
    }
    fn modal_key(&mut self, key: KeyEvent) {
        if matches!(self.modal, Some(Modal::Permissions(_))) {
            self.permissions_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Mcp(_))) {
            self.mcp_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Slack(_))) {
            self.slack_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Usage { .. })) {
            self.usage_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Model { .. })) {
            self.model_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::ModelPicker(_))) {
            self.picker_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::AgentDefaults(_))) {
            self.defaults_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::RepositoryRules(_))) {
            self.rules_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Tweaks { .. })) {
            self.tweaks_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Theme { .. })) {
            self.theme_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::Remote)) {
            self.remote_key(key);
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
        if matches!(
            self.modal,
            Some(Modal::Repository(_) | Modal::ChangeRepository(_))
        ) {
            if self.shortcut(key) == Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) {
                self.action(if matches!(self.modal, Some(Modal::Repository(_))) {
                    Action::ChooseRepository
                } else {
                    Action::ChangeRepository
                });
            } else {
                match &mut self.modal {
                    Some(Modal::Repository(editor))
                        if key.code == KeyCode::Char('u')
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        editor.clear()
                    }
                    Some(Modal::Repository(editor) | Modal::ChangeRepository(editor)) => {
                        editor.key(key)
                    }
                    _ => {}
                }
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shortcut = self.shortcut(key);
        let menu = self.menu_entries();
        let choices = match &self.modal {
            Some(Modal::FolderPicker { query, .. }) => self.folder_choices(&query.text()).len(),
            _ => 0,
        };
        let mut action = None;
        match &mut self.modal {
            Some(Modal::Transcript {
                scroll,
                rows,
                height,
                ..
            }) => {
                let maximum = rows.len().saturating_sub(*height);
                *scroll = shortcut_scroll(shortcut, *scroll, *height, maximum);
            }
            Some(Modal::Status { scroll }) => {
                *scroll = shortcut_scroll(shortcut, *scroll, 10, usize::MAX);
            }
            Some(Modal::Menu { query, selected }) => {
                let matching: Vec<_> = menu
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| {
                        label.to_lowercase().contains(&query.text().to_lowercase())
                    })
                    .collect();
                match shortcut {
                    Some(crate::shortcuts::ids::NAVIGATION_UP) => {
                        *selected = selected.saturating_sub(1)
                    }
                    Some(crate::shortcuts::ids::NAVIGATION_DOWN) => {
                        *selected = (*selected + 1).min(matching.len().saturating_sub(1))
                    }
                    Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) => {
                        action = matching
                            .get(*selected)
                            .map(|(index, _)| Action::MenuItem(*index))
                    }
                    _ if key.code == KeyCode::Char('u') && ctrl => {
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
                if shortcut == Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) {
                    if let Some(id) = self.selected.clone() {
                        let title = editor.text();
                        self.busy = true;
                        self.task(Task::Action, Request::Rename { id, title }, false);
                    }
                } else {
                    editor.key(key);
                }
            }
            Some(Modal::FolderPicker { query, selected }) => match shortcut {
                Some(crate::shortcuts::ids::NAVIGATION_UP) => {
                    *selected = selected.saturating_sub(1)
                }
                Some(crate::shortcuts::ids::NAVIGATION_DOWN) => {
                    *selected = (*selected + 1).min(choices.saturating_sub(1))
                }
                Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) => {
                    action = Some(Action::FolderChoice(*selected))
                }
                _ if key.code == KeyCode::Char('u') && ctrl => {
                    *query = Editor::default();
                    *selected = 0;
                }
                _ => {
                    query.key(key);
                    *selected = 0;
                }
            },
            Some(Modal::RenameFolder { id, name }) => {
                if shortcut == Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) {
                    let (id, name) = (id.clone(), name.text());
                    self.rename_folder(&id, &name);
                } else {
                    name.key(key);
                }
            }
            Some(Modal::DeleteFolder(id))
                if shortcut == Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) =>
            {
                let id = id.clone();
                self.delete_folder(&id);
            }
            Some(Modal::Delete) if shortcut == Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) => {
                if let Some(id) = self.selected.clone()
                    && self.deleting.insert(id.clone())
                {
                    self.modal = None;
                    self.selected = None;
                    self.drilled = false;
                    self.focus = Focus::List;
                    self.panels = panels::Panels::new(self.panels.right);
                    self.text_selection.clear();
                    self.ensure_selected();
                    self.task(Task::Delete(id.clone()), Request::Delete { id }, false);
                }
            }
            Some(Modal::Clear) if shortcut == Some(crate::shortcuts::ids::NAVIGATION_ACTIVATE) => {
                if !self.busy
                    && let Some(id) = self.selected.clone()
                {
                    self.busy = true;
                    self.modal = None;
                    self.task(Task::Clear(id.clone()), Request::Clear { id }, false);
                }
            }
            Some(Modal::Approval {
                pending,
                selected,
                answers,
                field,
                scroll,
            }) => {
                let text_input = pending.method == "item/tool/requestUserInput";
                match shortcut {
                    Some(crate::shortcuts::ids::NAVIGATION_PAGE_UP) => {
                        *scroll = scroll.saturating_sub(10)
                    }
                    Some(crate::shortcuts::ids::NAVIGATION_PAGE_DOWN) => {
                        *scroll = scroll.saturating_add(10)
                    }
                    Some(crate::shortcuts::ids::APPROVAL_SUBMIT) if !text_input => {
                        action = Some(Action::Approve(*selected))
                    }
                    Some(
                        crate::shortcuts::ids::NAVIGATION_NEXT_FOCUS
                        | crate::shortcuts::ids::NAVIGATION_PREVIOUS_FOCUS,
                    ) if text_input => {
                        *field = if key.code == KeyCode::BackTab {
                            (*field + answers.len().saturating_sub(1)) % answers.len().max(1)
                        } else {
                            (*field + 1) % answers.len().max(1)
                        };
                        *scroll = 0
                    }
                    Some(crate::shortcuts::ids::NAVIGATION_UP) if !text_input => {
                        *selected = selected.saturating_sub(1)
                    }
                    Some(crate::shortcuts::ids::NAVIGATION_DOWN) if !text_input => {
                        let last = match pending.method.as_str() {
                            "item/permissions/requestApproval" => 1,
                            "item/commandExecution/requestApproval"
                            | "item/fileChange/requestApproval" => 3,
                            _ => 0,
                        };
                        *selected = (*selected + 1).min(last)
                    }
                    _ if text_input => {
                        if let Some(e) = answers.get_mut(*field) {
                            e.key(key);
                        }
                    }
                    _ => {}
                }
            }
            Some(Modal::Help(state)) => match shortcut {
                Some(
                    crate::shortcuts::ids::NAVIGATION_UP
                    | crate::shortcuts::ids::NAVIGATION_DOWN
                    | crate::shortcuts::ids::NAVIGATION_FAST_UP
                    | crate::shortcuts::ids::NAVIGATION_FAST_DOWN,
                ) => {
                    state.scroll = shortcut_scroll(shortcut, state.scroll, 10, usize::MAX);
                }
                _ if key.code == KeyCode::Char('u') && ctrl => {
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
            Action::InboxOpen => self.open_questions_inbox(),
            Action::InboxItem(key, action) => self.inbox_item_action(key, action),
            Action::DismissNotice => self.notice = None,
            Action::ModelField(index) => self.model_field(index),
            Action::ModelOption(index) => self.model_option(index),
            Action::PickerOption(index) => self.picker_option(index),
            Action::DefaultOption(index) => self.default_option(index),
            Action::DefaultField(index) => {
                if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
                    form.field = index;
                    form.changed();
                }
            }
            Action::DefaultSave => self.save_defaults(),
            Action::DefaultRules => self.default_rules(),
            Action::RulesField(field) => {
                if let Some(Modal::RepositoryRules(form)) = &mut self.modal {
                    form.field = field;
                }
            }
            Action::RulesOption(index) => self.rules_option(index),
            Action::RulesSave => self.save_rules(),
            Action::RulesCancel => self.close_rules(),
            Action::OrchestratorBack => self.close_orchestrator_list(),
            Action::OpenFolder(id) => self.open_folder(id),
            Action::FolderBack => self.close_folder(),
            Action::FolderChoice(index) => self.choose_folder(index),
            Action::Select(id) => {
                if id == super::orchestrator::ID && !self.orchestrator_list {
                    self.open_orchestrator_list();
                    return;
                }
                if self.deleting.contains(&id) {
                    return;
                }
                self.select(id);
                self.focus = Focus::List;
            }
            Action::Open => {
                if let Some(folder) = self.folder_cursor.clone() {
                    self.open_folder(folder);
                    return;
                }
                if self.selected.as_deref() == Some(super::orchestrator::ID)
                    && !self.orchestrator_list
                {
                    self.open_orchestrator_list();
                    return;
                }
                if self
                    .selected
                    .as_ref()
                    .is_none_or(|id| self.deleting.contains(id))
                {
                    return;
                }
                self.follow_latest();
                self.drilled = true;
                self.focus = Focus::Composer;
            }
            Action::Focus(focus) => self.focus = focus,
            Action::ChangeFile(index) => self.select_change(index),
            Action::ChangePr(index) => self.select_change_pr(index),
            Action::Approval(index) => self.pending(index),
            Action::Pending => self.open_questions(),
            Action::OpenPlan => self.open_plan_prompt(),
            Action::PlanChoice(index) => self.choose_plan(index),
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
                self.toggle_question_check();
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
            Action::SlackSave => self.save_slack(),
            Action::SlackDisable => self.disable_slack(),
            Action::SlackField(index) => {
                if let Some(Modal::Slack(form)) = &mut self.modal {
                    form.field = index;
                }
            }
            Action::TweaksSave => self.save_tweaks(),
            Action::ThemeChoice(index) => self.preview_theme(index),
            Action::TweaksField(index) => {
                if let Some(Modal::Tweaks { field, .. }) = &mut self.modal {
                    *field = index;
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
            Action::McpSelect(index) => self.mcp_select(Some(index)),
            Action::PermissionTab(index) => self.permissions_tab(index),
            Action::PermissionItem(index) => self.permissions_select(Some(index)),
            Action::PermissionField(index) => self.permissions_field(index),
            Action::PermissionSave => self.permissions_save(),
            Action::PermissionSearch => self.permissions_search(),
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
                if self.retry_send(&entry) {
                    return;
                }
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
    /// Flips the whole drawn screen in after a tab switch at `started`;
    /// returns whether the flip is still playing.
    pub fn flip_screen(&self, buffer: &mut ratatui::buffer::Buffer, started: Instant) -> bool {
        if !self.preferences.motion.screen() {
            return false;
        }
        let duration = Duration::from_millis(u64::from(self.tweaks.capped(flap::SCREEN_MS)));
        self.tweaks.agent_reply_cps > 0 && flap::screen(buffer, started, duration, Instant::now())
    }
    /// Scrolls the open information modal by `rows`, reporting whether there
    /// was one: a message, usage, status, help or a request for your input.
    fn scroll_modal(&mut self, rows: isize) -> bool {
        match &mut self.modal {
            Some(Modal::Transcript {
                scroll,
                rows: lines,
                height,
                ..
            }) => {
                *scroll = scroll
                    .saturating_add_signed(rows)
                    .min(lines.len().saturating_sub(*height));
            }
            Some(Modal::Usage {
                scroll, maximum, ..
            }) => *scroll = scroll.saturating_add_signed(rows).min(*maximum),
            Some(Modal::Plan(prompt)) => {
                prompt.scroll = prompt
                    .scroll
                    .saturating_add_signed(rows)
                    .min(prompt.maximum)
            }
            Some(Modal::Status { scroll } | Modal::Approval { scroll, .. }) => {
                *scroll = scroll.saturating_add_signed(rows);
            }
            Some(Modal::Help(state)) => state.scroll = state.scroll.saturating_add_signed(rows),
            _ => return false,
        }
        true
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        if self.notice.is_some() && self.toast_rect.contains((event.column, event.row).into()) {
            if event.kind == MouseEventKind::Down(MouseButton::Left) {
                self.action(Action::DismissNotice);
            }
            return;
        }
        if self.inbox_mouse(event) {
            return;
        }
        if self.footer_mouse(event) {
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
                if matches!(self.modal, Some(Modal::Permissions(_))) {
                    self.permissions_scroll(event.kind == MouseEventKind::ScrollDown);
                } else if matches!(self.modal, Some(Modal::Mcp(_))) {
                    self.mcp_scroll(event.kind == MouseEventKind::ScrollDown);
                } else if self.scroll_modal(if event.kind == MouseEventKind::ScrollDown {
                    3
                } else {
                    -3
                }) {
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

fn approval_choices(pending: &Pending) -> Vec<&'static str> {
    if pending.params.get("difuClaudePlan").is_some() {
        return vec!["Review plan"];
    }
    match pending.method.as_str() {
        "item/permissions/requestApproval" => {
            vec!["Allow requested permissions for this turn", "Deny"]
        }
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            vec!["Allow once", "Allow for session", "Decline", "Cancel"]
        }
        _ => vec!["Stop request · ask the agent to use questions"],
    }
}

fn synchronous_permission_index(session: &Session) -> Option<usize> {
    session.pending.iter().position(|pending| {
        !pending.responded
            && !pending.is_async_question()
            && (super::elicitation::is_form(pending)
                || matches!(
                    pending.method.as_str(),
                    "item/commandExecution/requestApproval"
                        | "item/fileChange/requestApproval"
                        | "item/permissions/requestApproval"
                        | "mcpServer/elicitation/request"
                ))
    })
}

/// Presentation only: retain the provider diagnostic in session/error history.
fn provider_error_text(error: &str) -> &str {
    match error {
        r#"["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"]"# => {
            "Claude ended this turn without a final response."
        }
        _ => error,
    }
}

/// Scroll by semantic intent so custom physical keys retain the same motion.
fn shortcut_scroll(
    action: Option<crate::shortcuts::ActionId>,
    current: usize,
    page: usize,
    maximum: usize,
) -> usize {
    use crate::shortcuts::ids::*;
    match action {
        Some(NAVIGATION_UP) => current.saturating_sub(1),
        Some(NAVIGATION_DOWN) => current.saturating_add(1).min(maximum),
        Some(NAVIGATION_FAST_UP) => current.saturating_sub(10),
        Some(NAVIGATION_FAST_DOWN) => current.saturating_add(10).min(maximum),
        Some(NAVIGATION_PAGE_UP) => current.saturating_sub(page),
        Some(NAVIGATION_PAGE_DOWN) => current.saturating_add(page).min(maximum),
        Some(NAVIGATION_FIRST) => 0,
        Some(NAVIGATION_LAST) => maximum,
        _ => current,
    }
}

#[cfg(test)]
impl Ui {
    /// Moves every flip on, as if a second had passed.
    pub(super) fn settle_flips(&mut self) {
        let second = Duration::from_secs(1);
        for position in self.positions.values_mut() {
            for board in [&mut position.activity, &mut position.waiting]
                .into_iter()
                .chain(position.boards.values_mut())
            {
                board.age(second);
            }
        }
        for board in self.sidebar_boards.values_mut() {
            board.age(second);
        }
        self.sidebar_highlights.clear();
        self.session_switched = None;
        self.question_flip.age(second);
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
fn editor(frame: &mut Frame, rect: Rect, label: &str, value: &Editor, focused: bool) {
    editor_with(frame, rect, label, value, focused, None);
}
/// An editor whose laid-out rows pass through `transform`, such as an
/// animation. A transformed editor shows no cursor.
type Rows<'a> = &'a dyn Fn(&mut [Line<'static>]);
fn editor_with(
    frame: &mut Frame,
    rect: Rect,
    label: &str,
    value: &Editor,
    focused: bool,
    transform: Option<Rows>,
) {
    let area = panel(frame, rect, label, focused);
    editor_rows(frame, area, value, focused, transform);
}
fn composer_editor(
    frame: &mut Frame,
    rect: Rect,
    value: &Editor,
    focused: bool,
    transform: Option<Rows>,
) -> Rect {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }))
        .padding(Padding::horizontal(1));
    let area = block.inner(rect);
    frame.render_widget(block, rect);
    editor_rows(frame, area, value, focused, transform);
    area
}
fn editor_rows(
    frame: &mut Frame,
    area: Rect,
    value: &Editor,
    focused: bool,
    transform: Option<Rows>,
) {
    if area.is_empty() {
        return;
    }
    let (mut lines, (x, y)) = value.styled_layout(
        area.width as usize,
        Style::default().bg(ACCENT).fg(crate::ui::INK),
    );
    if let Some(transform) = transform {
        transform(&mut lines);
    }
    let focused = focused && transform.is_none();
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
            Paragraph::new(label.to_owned()).style(crate::ui::option_style(selected)),
            rect,
        );
        self.hits.push((rect, action));
    }
    pub fn draw(&mut self, frame: &mut Frame) {
        self.text_selection.frame();
        self.hits.clear();
        self.status_icons.begin_frame(Instant::now());
        self.reset_footer(self.drawn_session != self.selected);
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 40 || area.height < 12 {
            frame.render_widget(Paragraph::new("Resize to at least 40 × 12"), area);
            return;
        }
        let panel = self.panels.visible() && !self.inbox.visible;
        let conversation_visible = self.inbox.visible
            || ((!panel || self.panels.right) && !(self.drilled && self.changes_visible));
        let shown = if self.inbox.visible {
            Some("difu-questions-inbox".into())
        } else {
            self.selected.clone()
        };
        if self.drawn_session != shown {
            self.session_switched = (self.drawn_session.is_some()
                && self.selected.is_some()
                && conversation_visible
                && self.preferences.motion.screen()
                && self.tweaks.agent_reply_cps > 0)
                .then(Instant::now);
            self.drawn_session = shown;
        }
        if !conversation_visible
            || !self.preferences.motion.screen()
            || self.tweaks.agent_reply_cps == 0
        {
            self.session_switched = None;
        }
        let footer_height = if conversation_visible {
            0
        } else {
            self.session_footer_height()
        };
        let content = Rect::new(
            1,
            2,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2 + footer_height),
        );
        self.viewport = content.height.saturating_sub(2) as usize;
        let list_width = if self.list_visible {
            let requested = self.preferences.layout.sidebar_width;
            if requested == 0 {
                (content.width / 4).clamp(20, 38)
            } else {
                requested.clamp(20, 40) as u16
            }
            .min(content.width.saturating_sub(20))
        } else {
            0
        };
        let bottom = panel
            && self.panels.right
            && self.preferences.layout.helper_placement
                == Some(crate::settings::HelperPlacement::Bottom);
        let changes_width = if self.drilled && panel && self.panels.right && !bottom {
            (content.width.saturating_sub(list_width) / 2).max(12)
        } else {
            0
        };
        let list = Rect::new(content.x, content.y, list_width, content.height);
        let mut conversation = Rect::new(
            content.x + list_width,
            content.y,
            content.width.saturating_sub(list_width + changes_width),
            content.height,
        );
        let changes = if bottom {
            let height = (content.height / 2)
                .max(4)
                .min(content.height.saturating_sub(4));
            conversation.height = conversation.height.saturating_sub(height);
            Rect::new(
                conversation.x,
                conversation.bottom(),
                conversation.width,
                height,
            )
        } else {
            Rect::new(
                conversation.right(),
                content.y,
                changes_width,
                content.height,
            )
        };
        self.prs.visible.clear();
        if list_width > 0 {
            self.draw_list(frame, list);
        }
        if self.inbox.visible {
            self.draw_inbox(frame, conversation);
        } else if panel && !self.panels.right {
            self.draw_panel(frame, conversation);
        } else if self.drilled && self.changes_visible {
            self.draw_changes(frame, conversation);
        } else {
            self.draw_conversation(frame, conversation);
        }
        if changes.width > 0 && changes.height > 0 {
            self.draw_panel(frame, changes);
        }
        if footer_height > 0 {
            self.draw_session_footer(
                frame,
                Rect::new(
                    conversation.x,
                    content.bottom(),
                    conversation.width,
                    footer_height,
                ),
            );
        }
        self.text_selection.highlight(frame);
        if let Some(started) = self.session_switched {
            let duration = Duration::from_millis(u64::from(self.tweaks.capped(flap::SCREEN_MS)));
            if !flap::region(
                frame.buffer_mut(),
                conversation,
                started,
                duration,
                Instant::now(),
            ) {
                self.session_switched = None;
            }
        }
        self.draw_modal(frame);
        self.draw_toast(frame);
    }
    fn expire_toast(&mut self, now: Instant) {
        match (&self.notice, &self.toast_started) {
            (Some((text, error)), Some((previous, was_error, started)))
                if text == previous && error == was_error =>
            {
                if now.saturating_duration_since(*started) >= toast::LIFETIME {
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
        self.toast_rect = toast::draw(frame, frame.area(), notice, *error);
    }
    fn draw_list(&mut self, frame: &mut Frame, rect: Rect) {
        let folder = self.open_folder_name().map(|name| {
            crate::model::clean(&if self.archived {
                format!("Archived · {name}")
            } else {
                name.to_owned()
            })
        });
        let area = panel(
            frame,
            rect,
            if self.orchestrator_list {
                if self.archived {
                    "Archived orchestrator chats"
                } else {
                    "Orchestrator chats"
                }
            } else if let Some(folder) = &folder {
                folder
            } else if self.archived {
                "Archived agents"
            } else {
                "Agents"
            },
            !self.panels.focused && self.focus == Focus::List,
        );
        self.hits.push((rect, Action::Focus(Focus::List)));
        if self.orchestrator_list || folder.is_some() {
            let back = Rect::new(area.x, area.y, area.width, 1);
            frame.render_widget(
                Paragraph::new("← All sessions · Esc").style(Style::default().fg(DIM)),
                back,
            );
            self.hits.push((
                back,
                if self.orchestrator_list {
                    Action::OrchestratorBack
                } else {
                    Action::FolderBack
                },
            ));
        }
        let top = if self.orchestrator_list || folder.is_some() {
            1
        } else {
            self.draw_inbox_card(
                frame,
                Rect::new(area.x, area.y, area.width, area.height.min(3)),
            );
            4
        };
        let items = Rect::new(
            area.x,
            area.y.saturating_add(top),
            area.width,
            area.height.saturating_sub(top),
        );
        let rows = self.rows();
        let heights = rows
            .iter()
            .map(|row| match row {
                folders::Row::Folder(_) => 4,
                folders::Row::Session(session) => {
                    4 + usize::from(self.sidebar.counts.contains_key(&session.id))
                        + self.prs.all(&session.id).len()
                }
            })
            .collect::<Vec<_>>();
        let cursor = if self.inbox.visible {
            None
        } else {
            self.cursor()
        };
        let selected = rows
            .iter()
            .position(|row| Some(row.key()) == cursor)
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
        let now = Instant::now();
        // Cards flip as their text changes; those listed first show at once.
        let settled = !self.sidebar_seen
            || self.tweaks.agent_reply_cps == 0
            || !self.preferences.motion.sidebar();
        if settled {
            self.sidebar_highlights.clear();
        }
        let order: Vec<String> = rows.iter().map(folders::Row::key).collect();
        if !settled && order != self.sidebar_order {
            // A card moving into a slot flips from the text that was there.
            let previous: Vec<Option<flap::Board>> = self
                .sidebar_order
                .iter()
                .map(|id| self.sidebar_boards.get(id).cloned())
                .collect();
            for (index, id) in order.iter().enumerate() {
                if self.sidebar_order.get(index) != Some(id) {
                    let seed = previous.get(index).cloned().flatten().unwrap_or_default();
                    self.sidebar_boards.insert(id.clone(), seed);
                }
            }
        }
        self.sidebar_order = order;
        if cursor != self.sidebar_selected {
            if !settled {
                let right_to_left = self
                    .sidebar_order
                    .iter()
                    .position(|key| Some(key) == self.sidebar_selected.as_ref())
                    .is_some_and(|previous| selected > previous);
                for (id, on) in [
                    (self.sidebar_selected.clone(), false),
                    (cursor.clone(), true),
                ] {
                    if let Some(id) = id
                        && let Some(highlight) =
                            flap::Highlight::start(on, right_to_left, now, self.tweaks)
                    {
                        self.sidebar_highlights.insert(id, highlight);
                    }
                }
            }
            self.sidebar_selected.clone_from(&cursor);
        }
        let listed: HashSet<&str> = self.sidebar_order.iter().map(String::as_str).collect();
        self.sidebar_boards
            .retain(|id, _| listed.contains(id.as_str()));
        self.sidebar_highlights
            .retain(|id, highlight| listed.contains(id.as_str()) && !highlight.done(now));
        for (index, entry) in rows.iter().enumerate().skip(self.list_scroll) {
            if offset >= usize::from(items.height) {
                break;
            }
            let height = heights.get(index).copied().unwrap_or(4);
            let session = match entry {
                folders::Row::Folder(card) => {
                    let row = Rect::new(
                        items.x,
                        items.y.saturating_add(offset as u16),
                        items.width,
                        (height as u16 - 1).min(items.height.saturating_sub(offset as u16)),
                    );
                    offset += height;
                    self.draw_folder_card(frame, card, row, settled, now);
                    continue;
                }
                folders::Row::Session(session) => session,
            };
            let selected = !self.inbox.visible && Some(&session.id) == self.selected.as_ref();
            let deleting = self.deleting.contains(&session.id);
            let row = Rect::new(
                items.x,
                items.y.saturating_add(offset as u16),
                items.width,
                (height as u16 - 1).min(items.height.saturating_sub(offset as u16)),
            );
            offset += height;
            visible_pr_sessions.push(session.id.clone());
            let color = if deleting {
                DIM
            } else {
                status_icons::color(session.status)
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
            let status = if deleting {
                "Being deleted…".into()
            } else {
                format!(
                    "{}{}{}",
                    session.status.label(),
                    elapsed,
                    if session.archived { " · archived" } else { "" }
                )
            };
            let pin = if session.id == super::orchestrator::ID
                || self.pinned_sessions.contains(&session.id)
            {
                "📌 "
            } else {
                ""
            };
            let icon_inset = usize::from(status_icons::WIDTH) + 1;
            let title_width = usize::from(row.width)
                .saturating_sub(icon_inset + unicode_width::UnicodeWidthStr::width(pin));
            let mut lines = vec![
                Line::from(Span::styled(
                    format!(
                        "{} {pin}{}",
                        " ".repeat(usize::from(status_icons::WIDTH)),
                        crate::ui::crop(&session.title, 0, title_width)
                    ),
                    Style::default().fg(if deleting {
                        DIM
                    } else if selected {
                        ACCENT
                    } else {
                        TEXT
                    }),
                )),
                Line::from(Span::styled(
                    format!(
                        "   {}{}{}",
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
                    Span::raw("   "),
                    Span::styled(format!("+{}", stats.added), Style::default().fg(GREEN)),
                    Span::styled(format!(" -{}", stats.removed), Style::default().fg(RED)),
                ]));
            }
            lines.push(Line::from(Span::styled(
                format!("   {status}"),
                Style::default().fg(color),
            )));
            for link in self.prs.all(&session.id) {
                let pr = &link.pr;
                lines.push(Line::from(Span::styled(
                    format!(
                        "   {} · {}{}",
                        link.badge(),
                        pr.label(),
                        if self.prs.stale(&session.id) {
                            " · stale"
                        } else {
                            ""
                        }
                    ),
                    Style::default().fg(prs::color(pr)),
                )));
            }
            let board = self.sidebar_boards.entry(session.id.clone()).or_default();
            if settled {
                board.settle(&lines, now);
            } else {
                board.show(&mut lines, now, ACCENT, row.width);
            }
            frame.render_widget(
                Paragraph::new(lines).style(if selected {
                    crate::ui::user_message_style()
                } else {
                    Style::default().bg(BG)
                }),
                row,
            );
            if let Some(highlight) = self.sidebar_highlights.get(&session.id) {
                let background = crate::ui::user_message_style().bg.unwrap_or(PANEL);
                highlight.apply(frame.buffer_mut(), row, background, BG, now);
            }
            if !deleting {
                self.status_icons.draw(
                    frame,
                    Rect::new(row.x, row.y, status_icons::WIDTH, 1).intersection(row),
                    status_icons::display_status(session.status, session.pending > 0),
                    self.preferences.motion.sidebar() && self.tweaks.agent_reply_cps > 0,
                    self.modal.is_none() && self.notice.is_none(),
                );
                self.hits.push((row, Action::Select(session.id.clone())));
            }
        }
        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.listing {
                    "Connecting…"
                } else if self.folder_view.is_some() {
                    "This folder is empty\n\nn · New agent here\nf on a session in the main list · file it here"
                } else {
                    "No matching sessions\n\nn · New agent"
                })
                .style(Style::default().fg(DIM)),
                items,
            );
        }
        self.sidebar_seen |= !rows.is_empty();
        self.prs.visible.extend(visible_pr_sessions);
    }
    fn draw_conversation(&mut self, frame: &mut Frame, rect: Rect) {
        if self.draw_folder_overview(frame, rect) {
            return;
        }
        let title = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(|s| {
                format!(
                    "{} {} · {}",
                    " ".repeat(usize::from(status_icons::WIDTH)),
                    s.title,
                    s.status.label()
                )
            })
            .unwrap_or_else(|| "Conversation".into());
        let mut area = panel(
            frame,
            rect,
            &crate::model::clean(&title),
            !self.panels.focused && matches!(self.focus, Focus::Conversation | Focus::Composer),
        );
        if let Some(status) = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(|s| {
                status_icons::display_status(
                    s.status,
                    s.pending_question_count() > 0
                        || s.pending
                            .iter()
                            .any(|p| p.method != "item/tool/requestUserInput"),
                )
            })
        {
            self.status_icons.draw(
                frame,
                Rect::new(rect.x.saturating_add(2), rect.y, status_icons::WIDTH, 1)
                    .intersection(rect),
                status,
                self.preferences.motion.level == crate::settings::Motion::Full
                    && self.tweaks.agent_reply_cps > 0,
                self.modal.is_none() && self.notice.is_none(),
            );
        }
        self.hits.push((rect, Action::Focus(Focus::Conversation)));
        let composer_footer = self.drilled
            && self
                .selected
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .is_some_and(|session| matches!(session.job, Job::Coding(_)))
            && !self.question_panel()
            && !matches!(self.modal, Some(Modal::Plan(_) | Modal::Commands { .. }));
        let footer_height = if composer_footer {
            0
        } else {
            self.session_footer_height().min(area.height)
        };
        let footer = Rect::new(
            area.x,
            area.bottom().saturating_sub(footer_height),
            area.width,
            footer_height,
        );
        area.height = area.height.saturating_sub(footer_height);
        self.draw_session_footer(frame, footer);
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
        if matches!(self.modal, Some(Modal::Plan(_))) {
            self.draw_plan_prompt(frame, area);
            return;
        }
        if !self.question_panel() {
            self.question_flip = Default::default();
        }
        let shell_cwd = self.command_mode().then(|| {
            self.sessions
                .get(&id)
                .and_then(|session| session.workspace.as_ref())
                .map(|path| format!("pwd: {}", path.display()))
                .unwrap_or_else(|| "pwd: Worktree is not ready".into())
        });
        let cwd_lines = shell_cwd
            .as_ref()
            .map(|cwd| wrapped(&crate::model::clean(cwd), area.width.saturating_sub(2)))
            .unwrap_or_default();
        let cwd_height = (cwd_lines.len() as u16).min(area.height.saturating_sub(6));
        let composer_height = if self.question_panel() {
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
                .clamp(
                    1,
                    self.preferences.input.composer_max_height.clamp(3, 20) as usize,
                ) as u16;
            (lines + 2 + self.voice_rows(area.width) + cwd_height)
                .min(area.height.saturating_sub(3))
        } else {
            0
        };
        let question_open = self.question_panel();
        let reclaimed = self.reconcile_question_space(&id, question_open, composer_height);
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let mut activity_rows =
            transcript::activity(session, area.width, chrono::Utc::now().timestamp_millis());
        let waiting = transcript::waiting_messages(
            session,
            area.width,
            self.positions
                .get(&id)
                .map(|p| p.outgoing.as_slice())
                .unwrap_or_default(),
        );
        let working = activity_rows.len();
        activity_rows.extend(waiting);
        if let Some(position) = self.positions.get_mut(&id) {
            let now = Instant::now();
            let tweaks = self.preferences.motion.tweaks(self.tweaks);
            let (working, waiting) = activity_rows.split_at_mut(working);
            if working.is_empty() {
                position.activity = flap::Board::default();
            } else {
                if self.preferences.motion.level == crate::settings::Motion::Full {
                    position.activity.show(working, now, ACCENT, area.width);
                } else {
                    position.activity.settle(working, now);
                }
            }
            // Queued messages flip in over the sent-message time.
            let sent = Duration::from_millis(u64::from(tweaks.capped(tweaks.sent_message_ms)));
            // Messages already waiting when the session opens show at once.
            let opening = position.known.is_none();
            if waiting.is_empty() {
                position.waiting = flap::Board::default();
            } else if sent.is_zero() || opening {
                position.waiting.settle(waiting, now);
            } else {
                position
                    .waiting
                    .show_over(waiting, now, ACCENT, area.width, |_| sent);
            }
        }
        let activity_height = (activity_rows.len().min(usize::from(area.height / 3))) as u16;
        let plan_ready = self.plan_available();
        let requests_height =
            u16::from(!session.pending.is_empty()) + activity_height + u16::from(plan_ready);
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
                .saturating_sub(composer_height + reclaimed + requests_height + 1),
        );
        self.conversation_height = usize::from(body.height);
        let position = self.positions.entry(id.clone()).or_default();
        if let Some((old_width, old_height, version)) = position.transcript_viewport {
            if position.question_scroll || old_width != body.width || version != session.version {
                position.keep_transcript_position = false;
            } else if old_height != body.height {
                position.keep_transcript_position = true;
            }
        }
        position.transcript_viewport = Some((body.width, body.height, session.version));
        let before_measure = position.conversation;
        let tweaks = self.preferences.motion.tweaks(self.tweaks);
        let appearance_changed = self.transcript_window.appearance != self.preferences.appearance
            || self.transcript_window.conversation != self.preferences.conversation;
        self.transcript_window.appearance = self.preferences.appearance.clone();
        self.transcript_window.conversation = self.preferences.conversation.clone();
        let view = self.transcript_window.render(
            session,
            position,
            body.width,
            body.height,
            self.focus == Focus::Conversation,
        );
        if !appearance_changed {
            self.text_selection
                .rebase(1, position.conversation as isize - before_measure as isize);
        }
        let transcript_window::View {
            mut lines,
            mut links,
            start,
            mut total,
            mut sections,
        } = view;
        let now = Instant::now();
        let turn = flap::chat(
            position, session, &sections, start, total, &mut lines, tweaks, body.width, now,
        );
        reveal::apply(
            position, session, &sections, start, total, &mut lines, tweaks, body.width, now, turn,
        );
        if self.preferences.motion.reply() {
            flap::apply(
                position, session, &sections, start, total, &mut lines, body.width, now,
            );
        } else {
            position.voice_flaps.clear();
        }
        // Rows still waiting for their first character take no space yet.
        let dropped = flap::grow(
            &mut sections,
            start,
            &mut total,
            &mut lines,
            &mut links,
            position.conversation,
        );
        let maximum = total.saturating_sub(if position.keep_transcript_position {
            1
        } else {
            usize::from(body.height)
        });
        position.conversation = if (match self.preferences.conversation.auto_scroll {
            crate::settings::AutoScroll::WhenAtBottom => position.follow,
            crate::settings::AutoScroll::Always => true,
            crate::settings::AutoScroll::Manual => false,
        } || position.question_scroll)
            && !position.keep_transcript_position
        {
            maximum
        } else {
            position.conversation.saturating_sub(dropped).min(maximum)
        };
        if session.entries.is_empty() && total == 0 {
            lines.push(Line::from(if session.status == Status::Idle {
                "Send a message to start."
            } else {
                "Preparing session…"
            }));
            total = 1;
        }
        if let Some(error) = &session.error
            && !session
                .entries
                .iter()
                .any(|e| e.kind == "error" && &e.text == error)
            && start + lines.len() == total
        {
            let extra = wrapped(provider_error_text(error), body.width)
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
            let retry = position
                .outgoing
                .iter()
                .any(|pending| pending.failed.is_some() && pending.entry_id() == section.id);
            if (section.tool || retry) && first < last {
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
        if appearance_changed {
            self.text_selection
                .reflow(1, &self.conversation_sections, &sections);
        }
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
            .filter(|p| {
                !p.responded
                    && (p.method != "item/tool/requestUserInput" || super::elicitation::is_form(p))
            })
            .count();
        if pending {
            let questions = self.question_index().map_or_else(
                || format!("{question_count} question(s)"),
                |index| format!("{} of {question_count} questions", index + 1),
            );
            let header = format!(
                "{questions} · {approval_count} approval(s) · {} Questions",
                crate::shortcuts::Bindings::new(&self.shortcut_config)
                    .hint(crate::shortcuts::ids::QUESTION_OPEN)
            );
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
        if plan_ready {
            self.draw_plan_affordance(
                frame,
                Rect::new(
                    area.x,
                    body.bottom() + activity_height + u16::from(pending),
                    area.width,
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
            if self.question_panel() {
                self.draw_inline_question(frame, composer);
                return;
            }
            if matches!(self.modal, Some(Modal::Commands { .. })) {
                self.draw_commands(frame, composer);
                return;
            }
            let status_y = composer.bottom().saturating_sub(1);
            self.draw_composer_footer(
                frame,
                Rect::new(
                    composer.x.saturating_add(1),
                    status_y,
                    composer.width.saturating_sub(2),
                    1,
                ),
            );
            let composer = Rect::new(
                composer.x,
                composer.y,
                composer.width,
                composer.height.saturating_sub(1),
            );
            let voice = self
                .voice_rows(composer.width)
                .min(composer.height.saturating_sub(2));
            let composer = if voice > 0 {
                self.draw_voice(
                    frame,
                    Rect::new(composer.x, composer.y, composer.width, voice),
                );
                Rect::new(
                    composer.x,
                    composer.y + voice,
                    composer.width,
                    composer.height - voice,
                )
            } else {
                composer
            };
            let composer = if cwd_height > 0 {
                frame.render_widget(
                    Paragraph::new(cwd_lines.into_iter().map(Line::from).collect::<Vec<_>>())
                        .style(Style::default().fg(DIM)),
                    Rect::new(
                        composer.x + 1,
                        composer.y,
                        composer.width.saturating_sub(2),
                        cwd_height,
                    ),
                );
                Rect::new(
                    composer.x,
                    composer.y + cwd_height,
                    composer.width,
                    composer.height.saturating_sub(cwd_height),
                )
            } else {
                composer
            };
            let placeholder = self
                .composer_suggestion()
                .unwrap_or("Ask anything…")
                .to_owned();
            let position = self.positions.entry(id.clone()).or_default();
            let now = Instant::now();
            let focused =
                self.focus == Focus::Composer && !self.panels.focused && self.modal.is_none();
            let input_area = if let Some(clearing) = reveal::clearing(position, now) {
                composer_editor(
                    frame,
                    composer,
                    &Editor::from(clearing.text()),
                    focused,
                    Some(&|lines: &mut [Line<'static>]| clearing.vanish(lines, now)),
                )
            } else {
                composer_editor(frame, composer, &position.draft, focused, None)
            };
            let clearing = position.clearing.is_some();
            if position.draft.chars.is_empty() && !clearing {
                frame.render_widget(
                    Paragraph::new(placeholder).style(Style::default().fg(DIM)),
                    input_area,
                );
            }
            self.hits.push((composer, Action::Focus(Focus::Composer)));
        } else if !self.drilled {
            self.button(
                frame,
                Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                &format!(
                    "{} · Open session",
                    crate::shortcuts::Bindings::new(&self.shortcut_config)
                        .hint(crate::shortcuts::ids::NAVIGATION_ACTIVATE)
                ),
                Action::Open,
                false,
            );
        }
    }
    fn draw_modal(&mut self, frame: &mut Frame) {
        if self.modal.is_none()
            || self.inline_question()
            || (self.inbox.visible && matches!(self.modal, Some(Modal::Approval { .. })))
            || matches!(self.modal, Some(Modal::Commands { .. }))
        {
            return;
        }
        if matches!(self.modal, Some(Modal::Plan(_))) {
            return;
        }
        if matches!(self.modal, Some(Modal::Permissions(_))) {
            self.text_selection.clear();
            self.draw_permissions_overlay(frame);
            return;
        }
        if matches!(self.modal, Some(Modal::Mcp(_))) {
            self.text_selection.clear();
            self.draw_mcp_overlay(frame);
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
        let rect = if matches!(self.modal, Some(Modal::Theme { .. })) {
            themes::rect(full)
        } else {
            Rect::new(
                full.width.saturating_sub(width) / 2,
                full.height.saturating_sub(height) / 2,
                width,
                height,
            )
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default().style(Style::default().bg(PANEL).fg(TEXT)),
            rect,
        );
        let menu_entries = self.menu_entries();
        let title = match self.modal {
            Some(Modal::Transcript { .. }) => "Message",
            Some(Modal::InstallBrowser { .. }) => "Optional HTML preview · Esc cancels",
            Some(Modal::AgentDefaults(_)) => "New agent defaults",
            Some(Modal::RepositoryRules(_)) => "Repository rules",
            Some(Modal::Slack(_)) => "Slack setup",
            Some(Modal::Tweaks { .. }) => "Tweaks · chat animations",
            Some(Modal::Theme { .. }) => "Theme",
            Some(Modal::Remote) => "Remote control · iPhone",
            Some(Modal::Repository(_)) => "Choose and remember your default repository",
            Some(Modal::ChangeRepository(_)) => "Change session repository",
            Some(Modal::Menu { .. }) => "Agent actions",
            Some(Modal::Commands { .. }) => "Session commands",
            Some(Modal::Status { .. }) => "Session status",
            Some(Modal::Permissions(_)) => "Permissions",
            Some(Modal::Plan(_)) => "Implement this plan?",
            Some(Modal::Mcp(_)) => "MCP",
            Some(Modal::Usage { .. }) => "Provider usage",
            Some(Modal::Pending { .. }) => "Pending questions and approvals",
            Some(Modal::Queue { .. }) => "Queued outgoing messages",
            Some(Modal::QueuedEdit { .. }) => "Edit queued message",
            Some(Modal::Rename(_)) => "Rename session",
            Some(Modal::FolderPicker { .. }) => "Move to folder",
            Some(Modal::RenameFolder { .. }) => "Rename folder",
            Some(Modal::DeleteFolder(_)) => "Delete folder",
            Some(Modal::Model { .. }) => "Session model",
            Some(Modal::ModelPicker(_)) => "Select model",
            Some(Modal::Approval { .. }) => "Agent needs your input",
            Some(Modal::Resources {
                artifacts: true, ..
            }) => "HTML artifacts",
            Some(Modal::Resources { .. }) => "Open shells",
            Some(Modal::Delete) => "Stop and delete chat",
            Some(Modal::Clear) => "Clear session",
            Some(Modal::Help(_)) => "Search agent shortcuts",
            None => "",
        };
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        let context = self.shortcut_context();
        let hint = |action| bindings.primary_hint_in(action, context);
        let owned_title = match self.modal {
            Some(Modal::Transcript { .. }) => Some(format!(
                "Message · {}/{} Scroll · {}/{} Page · Esc Close",
                hint(crate::shortcuts::ids::NAVIGATION_UP),
                hint(crate::shortcuts::ids::NAVIGATION_DOWN),
                hint(crate::shortcuts::ids::NAVIGATION_PAGE_UP),
                hint(crate::shortcuts::ids::NAVIGATION_PAGE_DOWN)
            )),
            Some(Modal::Status { .. }) => Some(format!(
                "Session status · {}/{} Scroll · Esc Close",
                hint(crate::shortcuts::ids::NAVIGATION_UP),
                hint(crate::shortcuts::ids::NAVIGATION_DOWN)
            )),
            Some(Modal::Usage { .. }) => Some(format!(
                "Provider usage · {} Refresh · {}/{} Scroll · Esc Close",
                hint(crate::shortcuts::ids::USAGE_REFRESH),
                hint(crate::shortcuts::ids::NAVIGATION_UP),
                hint(crate::shortcuts::ids::NAVIGATION_DOWN)
            )),
            Some(Modal::Slack(_)) => Some(format!(
                "Slack setup · {}/{} fields · Esc closes",
                hint(crate::shortcuts::ids::NAVIGATION_NEXT_FOCUS),
                hint(crate::shortcuts::ids::NAVIGATION_PREVIOUS_FOCUS)
            )),
            Some(Modal::Mcp(_)) => Some(format!(
                "MCP · {}/{} Select · {} Open · {} Refresh · Esc Back",
                hint(crate::shortcuts::ids::NAVIGATION_UP),
                hint(crate::shortcuts::ids::NAVIGATION_DOWN),
                hint(crate::shortcuts::ids::NAVIGATION_ACTIVATE),
                hint(crate::shortcuts::ids::MCP_REFRESH)
            )),
            Some(Modal::Resources { artifacts, .. }) => Some(format!(
                "{} · {} open",
                if artifacts {
                    "HTML artifacts"
                } else {
                    "Open shells"
                },
                hint(crate::shortcuts::ids::NAVIGATION_ACTIVATE)
            )),
            Some(Modal::AgentDefaults(_)) => Some(format!(
                "New agent defaults · {}/{} fields · Esc cancels",
                hint(crate::shortcuts::ids::NAVIGATION_NEXT_FOCUS),
                hint(crate::shortcuts::ids::NAVIGATION_PREVIOUS_FOCUS)
            )),
            Some(Modal::RepositoryRules(_)) => Some(format!(
                "Repository rules · {}/{} fields",
                hint(crate::shortcuts::ids::NAVIGATION_NEXT_FOCUS),
                hint(crate::shortcuts::ids::NAVIGATION_PREVIOUS_FOCUS)
            )),
            Some(Modal::ModelPicker(_)) => Some(format!(
                "Select model · {}/{} Select · {}/{} Effort · Esc Cancel",
                hint(crate::shortcuts::ids::NAVIGATION_UP),
                hint(crate::shortcuts::ids::NAVIGATION_DOWN),
                hint(crate::shortcuts::ids::MODELS_PREVIOUS_EFFORT),
                hint(crate::shortcuts::ids::MODELS_NEXT_EFFORT)
            )),
            _ => None,
        };
        let title = owned_title.as_deref().unwrap_or(title);
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
        if matches!(self.modal, Some(Modal::ModelPicker(_))) {
            self.draw_picker(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::AgentDefaults(_))) {
            self.draw_defaults(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::RepositoryRules(_))) {
            self.draw_rules(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Commands { .. })) {
            self.draw_commands(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Tweaks { .. })) {
            self.draw_tweaks(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Theme { .. })) {
            self.draw_theme(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Slack(_))) {
            self.draw_slack(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Remote)) {
            self.draw_remote(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Mcp(_))) {
            self.draw_mcp(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Usage { .. })) {
            self.draw_usage(frame, area);
            return;
        }
        if matches!(self.modal, Some(Modal::Status { .. })) {
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
        let activate_hint = crate::shortcuts::Bindings::new(&self.shortcut_config).primary_hint_in(
            crate::shortcuts::ids::NAVIGATION_ACTIVATE,
            self.shortcut_context(),
        );
        let approval_submit_hint = crate::shortcuts::Bindings::new(&self.shortcut_config)
            .primary_hint_in(
                crate::shortcuts::ids::APPROVAL_SUBMIT,
                self.shortcut_context(),
            );
        let approval_page_hint = format!(
            "{}/{}",
            crate::shortcuts::Bindings::new(&self.shortcut_config).primary_hint_in(
                crate::shortcuts::ids::NAVIGATION_PAGE_UP,
                self.shortcut_context()
            ),
            crate::shortcuts::Bindings::new(&self.shortcut_config).primary_hint_in(
                crate::shortcuts::ids::NAVIGATION_PAGE_DOWN,
                self.shortcut_context()
            )
        );
        let picker = match &self.modal {
            Some(Modal::FolderPicker { query, .. }) => self.folder_choices(&query.text()),
            _ => Vec::new(),
        };
        match &mut self.modal {
            Some(
                Modal::Transcript { .. }
                | Modal::Model { .. }
                | Modal::ModelPicker(_)
                | Modal::Resources { .. }
                | Modal::InstallBrowser { .. },
            ) => {}
            Some(Modal::ChangeRepository(value)) => {
                let input = Rect::new(area.x, area.y, area.width, area.height.min(3));
                editor(frame, input, "Local repository", value, true);
                deferred.push((
                    Rect::new(area.x, area.y.saturating_add(4), area.width, 1).intersection(area),
                    format!("[ {activate_hint} · Change repository ]"),
                    Action::ChangeRepository,
                    true,
                ));
                frame.render_widget(
                    Paragraph::new("Changing repositories cancels current work and preparation, then discards the managed worktree and its local work, artifacts, and PR associations. Connected PRs stay open.")
                        .wrap(Wrap { trim: false }),
                    Rect::new(
                        area.x,
                        area.y.saturating_add(6),
                        area.width,
                        area.height.saturating_sub(6),
                    ),
                );
            }
            Some(Modal::Repository(value)) => {
                let input = Rect::new(area.x, area.y, area.width, area.height.min(3));
                editor(frame, input, "Local repository", value, true);
                let button =
                    Rect::new(area.x, area.y.saturating_add(4), area.width, 1).intersection(area);
                deferred.push((
                    button,
                    format!("[ {activate_hint} · Save and open session ]"),
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
                    &format!("Name · {activate_hint} save"),
                    value,
                    true,
                );
            }
            Some(Modal::FolderPicker { query, selected }) => {
                editor(
                    frame,
                    Rect::new(area.x, area.y, area.width, 3.min(area.height)),
                    &format!("Type to filter or name a new folder · {activate_hint} move"),
                    query,
                    true,
                );
                *selected = (*selected).min(picker.len().saturating_sub(1));
                let selected = *selected;
                self.draw_folder_picker(frame, area, selected, &picker, &mut deferred);
            }
            Some(Modal::RenameFolder { name, .. }) => {
                editor(
                    frame,
                    Rect::new(area.x, area.y, area.width, 3.min(area.height)),
                    &format!("Folder name · {activate_hint} save"),
                    name,
                    true,
                );
            }
            Some(Modal::DeleteFolder(id)) => {
                let id = id.clone();
                let prompt = self.delete_folder_prompt(&id, &activate_hint);
                frame.render_widget(Paragraph::new(prompt).wrap(Wrap { trim: false }), area);
            }
            Some(Modal::Delete) => {
                frame.render_widget(Paragraph::new(format!("Stop this agent and permanently delete its difu chat, saved questions, queue, and attachments? Its unlocked difu-owned worktree will also be removed, including all uncommitted, untracked, and ignored files. Existing directories, named branches, commits, and native provider history are not deleted.\n\n{activate_hint} confirms · Esc cancels")).wrap(Wrap { trim:false }), area);
            }
            Some(Modal::Clear) => {
                frame.render_widget(Paragraph::new(format!("Stop this agent and start this session over? Its chat, conversation context, queue, questions, artifacts, and PR links are removed, and the next message starts a new conversation. Its difu-owned worktree and local branch are discarded, including uncommitted and unpushed work, and a fresh worktree is created from the latest origin/main. Pushed branches, open PRs, registered external worktrees, and native provider history are not deleted.\n\n{activate_hint} confirms · Esc cancels")).wrap(Wrap { trim:false }), area);
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
                let help = agent_help(&state.query.text(), &self.shortcut_config);
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
                    let mut description = format!(
                        "{} · {approval_page_hint} scroll\n\n{}\n\n{}",
                        pending.method, content, related
                    );
                    let unsupported = !matches!(
                        pending.method.as_str(),
                        "item/commandExecution/requestApproval"
                            | "item/fileChange/requestApproval"
                            | "item/permissions/requestApproval"
                    );
                    if unsupported {
                        description = "This request cannot be displayed as questions. Stop it and ask the agent to use the questions tool.".into();
                    }
                    let rows = wrapped(&description, area.width);
                    let height = area.height.saturating_sub(5);
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
                    let choices = approval_choices(pending);
                    for (index, label) in choices.iter().enumerate() {
                        deferred.push((
                            Rect::new(
                                area.x,
                                area.bottom().saturating_sub(choices.len() as u16) + index as u16,
                                area.width,
                                1,
                            ),
                            format!(
                                "{} {label}",
                                if *selected == index {
                                    format!("> {approval_submit_hint} ·")
                                } else {
                                    " ".into()
                                }
                            ),
                            Action::Approve(index),
                            *selected == index,
                        ));
                    }
                }
            }
            Some(
                Modal::AgentDefaults(_)
                | Modal::RepositoryRules(_)
                | Modal::Slack(_)
                | Modal::Tweaks { .. }
                | Modal::Theme { .. }
                | Modal::Remote
                | Modal::Commands { .. }
                | Modal::Status { .. }
                | Modal::Mcp(_)
                | Modal::Permissions(_)
                | Modal::Plan(_)
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
fn agent_help(query: &str, config: &Config) -> Vec<(String, &'static str)> {
    use crate::shortcuts::{Bindings, ids::*};
    let bindings = Bindings::new(config);
    let hints = |ids: &[crate::shortcuts::ActionId]| {
        ids.iter()
            .map(|id| bindings.hint(*id))
            .collect::<Vec<_>>()
            .join(" / ")
    };
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
            "Open session / interrupt active work; unanswered permissions open for review",
        ),
        (
            "Esc with permission pending",
            "Open or dismiss approval; use Actions to stop the agent",
        ),
        ("Cmd+K", "Search sessions, pull requests and commands"),
        (
            "Ctrl+↑ / ↓ in sessions",
            "Reorder within the pinned or unpinned group, or move a folder (saved)",
        ),
        (
            "f / e / x in sessions",
            "Move the session to a folder (type a new name to create one); rename or delete the highlighted folder",
        ),
        (
            "Tab / Shift+Tab",
            "Cycle visible panes; composer permission mode has its own shortcut",
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
            "Alt+M",
            "Pick a Codex or Claude Code model and reasoning level; Enter also sets the default",
        ),
        (
            "Enter in composer",
            "Send message using the configured running-turn behavior",
        ),
        ("Ctrl+Enter", "Queue message for the next turn"),
        ("Question answer", "Submit the current question answer"),
        (
            "Alt+↑ / ↓",
            "Next / previous question; previous from first returns to input",
        ),
        ("Ctrl+]", "Skip the current question without answering"),
        (
            "n (question choice)",
            "Add a note beneath the selected answer; submitting sends both",
        ),
        (
            "/ · $ · @",
            "Commands, workspace skills, and file suggestions above input",
        ),
        ("Shift+Enter", "Insert a newline in the composer"),
        (
            "Shift+Tab (composer)",
            "Cycle the provider permission mode; /permissions opens settings",
        ),
        (
            "Hold Space",
            "Talk when enabled with /voice: Codex sessions answer aloud, others take dictation",
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
    .map(|(key, description)| {
        let key = match key {
            "⌥+1 / ⌥+2" => hints(&[SHELL_AGENTS, SHELL_REVIEWS]),
            "n" => hints(&[SESSION_NEW]),
            "↑ / ↓" if description.starts_with("Up in") => {
                hints(&[COMPOSER_HISTORY_PREVIOUS, COMPOSER_HISTORY_NEXT])
            }
            "↑ / ↓" => hints(&[NAVIGATION_UP, NAVIGATION_DOWN]),
            "Enter on a tool" => hints(&[NAVIGATION_ACTIVATE]),
            "PageUp / PageDown / wheel" => format!(
                "{} / wheel",
                hints(&[NAVIGATION_PAGE_UP, NAVIGATION_PAGE_DOWN])
            ),
            "Enter / Esc" => hints(&[NAVIGATION_ACTIVATE, CONVERSATION_INTERRUPT]),
            "Cmd+K" => hints(&[PALETTE_OPEN]),
            "Ctrl+↑ / ↓ in sessions" => hints(&[SESSION_REORDER_UP, SESSION_REORDER_DOWN]),
            "f / e / x in sessions" => hints(&[FOLDER_MOVE, FOLDER_RENAME, FOLDER_DELETE]),
            "Tab / Shift+Tab" => hints(&[NAVIGATION_NEXT_FOCUS, NAVIGATION_PREVIOUS_FOCUS]),
            "Drag · Ctrl+C / Cmd+C" => format!("Drag · Ctrl+C / {}", hints(&[SELECTION_COPY])),
            "Ctrl+V / Cmd+V" => hints(&[COMPOSER_ATTACH]),
            "Alt+[" => hints(&[AGENTS_SIDEBAR]),
            "Alt+D" => hints(&[AGENTS_CHANGES]),
            "Alt+M" => hints(&[AGENTS_MODELS]),
            "Enter in composer" => hints(&[COMPOSER_SEND]),
            "Ctrl+Enter" => hints(&[COMPOSER_QUEUE]),
            "Question answer" => bindings
                .primary_hint_in(QUESTION_SUBMIT, crate::shortcuts::contexts::AGENT_QUESTION),
            "Alt+]" => hints(&[HELPER_VISIBILITY]),
            "Alt+P" => hints(&[HELPER_PLACEMENT]),
            "Alt+↑ / ↓" => hints(&[QUESTION_OPEN, QUESTION_PREVIOUS, QUESTION_NEXT]),
            "Ctrl+]" => hints(&[QUESTION_SKIP]),
            "n (question choice)" => hints(&[QUESTION_NOTE]),
            "/ · $ · @" => hints(&[AGENTS_COMMANDS, COMPOSER_SKILLS, COMPOSER_PATHS]),
            "Shift+Enter" => hints(&[COMPOSER_NEWLINE]),
            "Shift+Tab (composer)" => hints(&[COMPOSER_PERMISSION_MODE]),
            "Hold Space" => format!("Hold {}", hints(&[VOICE_HOLD])),
            "Cmd+A / Cmd+C" => format!("Cmd+A / {}", hints(&[SELECTION_COPY])),
            "i" => hints(&[CONVERSATION_COMPOSE]),
            "/" => hints(&[AGENTS_COMMANDS]),
            "r" => hints(&[SESSION_REFRESH]),
            "← / →" => hints(&[NAVIGATION_LEFT, NAVIGATION_RIGHT]),
            "Cmd+↑ / ↓" => hints(&[
                CONVERSATION_PREVIOUS,
                CONVERSATION_NEXT,
                NAVIGATION_FAST_UP,
                NAVIGATION_FAST_DOWN,
            ]),
            "Shift+↑ / ↓" => hints(&[CHANGES_SELECT_UP, CHANGES_SELECT_DOWN]),
            "c / Cmd+C" => hints(&[SELECTION_COPY]),
            "Home / End" => hints(&[NAVIGATION_FIRST, NAVIGATION_LAST]),
            "?" => hints(&[AGENTS_HELP]),
            "Ctrl+C" => format!("Ctrl+C copy / {} exit", hints(&[APP_QUIT])),
            _ => key.into(),
        };
        (key, description)
    })
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
    /// Delivery and layout tests read text the moment it arrives.
    fn still(ui: &mut Ui) {
        ui.tweaks = crate::storage::Tweaks {
            agent_reply_cps: 0,
            sent_message_ms: 0,
            input_clear_ms: 0,
            max_animation_ms: 0,
        };
    }
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
    fn notification_presence_follows_conversation_visibility_with_helper_placements() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = Ui::new(storage, &Config::default());
        ui.selected = Some("selected".into());
        ui.drilled = true;
        ui.panels.session = Some("remembered-helper-session".into());
        ui.panels.view = Some(panels::View::Shell {
            id: "shell".into(),
            command: "example".into(),
        });
        for placement in [
            crate::settings::HelperPlacement::Right,
            crate::settings::HelperPlacement::Bottom,
        ] {
            ui.preferences.layout.helper_placement = Some(placement);
            ui.panels.right = true;
            assert_eq!(
                ui.notification_viewed_session().as_deref(),
                Some("selected")
            );
        }
        ui.preferences.layout.helper_placement = Some(crate::settings::HelperPlacement::Main);
        ui.panels.right = false;
        assert_eq!(ui.notification_viewed_session(), None);
        ui.panels.hidden = true;
        assert_eq!(
            ui.notification_viewed_session().as_deref(),
            Some("selected")
        );
        ui.changes_visible = true;
        assert_eq!(ui.notification_viewed_session(), None);
        Ok(())
    }
    #[test]
    fn permissions_command_and_actions_open_for_both_providers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        for provider in [provider::Provider::Codex, provider::Provider::Claude] {
            ui.sessions.get_mut("one").context("session")?.provider = provider;
            ui.run_palette_command("/permissions", "permissions");
            assert!(matches!(ui.modal, Some(Modal::Permissions(_))));
            assert!(
                ui.palette_commands()
                    .iter()
                    .any(|(name, _, _)| name == "/permissions")
            );
            ui.modal = None;
            let index = ui
                .menu_entries()
                .iter()
                .position(|label| label.starts_with("Permissions"))
                .context("permissions menu")?;
            ui.menu_action(index);
            assert!(matches!(ui.modal, Some(Modal::Permissions(_))));
        }
        Ok(())
    }
    #[test]
    fn native_incomplete_turn_diagnostic_is_humanized_only_for_error_display() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let diagnostic =
            r#"["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"]"#;
        for kind in ["error", "userMessage", "system"] {
            let session = ui.sessions.get_mut("one").context("session")?;
            session.entries.clear();
            session.error = Some(diagnostic.into());
            session.note(kind, diagnostic);
            let rendered = transcript::render(session, &Position::default(), 160, false)
                .0
                .iter()
                .map(Line::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            if kind == "error" {
                assert!(rendered.contains("Claude ended this turn without a final response."));
                assert!(!rendered.contains("ede_diagnostic"));
            } else {
                assert!(rendered.contains("ede_diagnostic"));
            }
            assert_eq!(session.error.as_deref(), Some(diagnostic));
            assert_eq!(session.entries.first().context("entry")?.text, diagnostic);
        }
        let other =
            r#"["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=other"]"#;
        assert_eq!(provider_error_text(other), other);
        Ok(())
    }
    #[test]
    fn composer_permission_status_is_inset_above_the_outer_border() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        still(&mut ui);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.provider = provider::Provider::Claude;
        session.permission_state.mode = Some("auto".into());
        for (width, height, resources) in [
            (104, 32, false),
            (48, 12, false),
            (22, 10, false),
            (104, 32, true),
            (48, 12, true),
            (22, 10, true),
        ] {
            if resources {
                ui.panels
                    .shells
                    .insert("one".into(), vec![serde_json::json!({"itemId": "shell"})]);
            }
            ui.hits.clear();
            let status_y = height - 2;
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| ui.draw_conversation(frame, frame.area()))?;
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.cell((2, status_y)).context("status")?.symbol(), "A");
            assert_eq!(buffer.cell((2, status_y)).context("status")?.fg, ACCENT);
            assert_eq!(
                buffer.cell((1, status_y)).context("status inset")?.symbol(),
                " "
            );
            for x in 1..width - 1 {
                assert_eq!(
                    buffer
                        .cell((x, height - 1))
                        .context("outer border")?
                        .symbol(),
                    "─"
                );
            }
            if resources {
                ui.focus = Focus::Resources(false);
                ui.hits.clear();
                terminal.draw(|frame| ui.draw_conversation(frame, frame.area()))?;
                let buffer = terminal.backend().buffer();
                let footer = ui
                    .hits
                    .iter()
                    .find_map(|(rect, action)| {
                        matches!(action, Action::Resources(false)).then_some(*rect)
                    })
                    .context("shell control")?;
                assert_eq!(footer.y, status_y);
                assert!(footer.x > 0 && footer.right() < width);
                let footer_text = (footer.x..footer.right())
                    .filter_map(|x| buffer.cell((x, footer.y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(footer_text.contains("Shells (1)"), "{footer_text}");
                assert!(
                    ui.hits
                        .iter()
                        .rev()
                        .find(|(rect, _)| rect.contains((footer.x, footer.y).into()))
                        .is_some_and(|(_, action)| matches!(action, Action::Resources(false))),
                    "the footer must receive clicks instead of the conversation"
                );
            }
            ui.focus = Focus::Composer;
            ui.hits.clear();
            terminal.draw(|frame| ui.draw_conversation(frame, frame.area()))?;
            let buffer = terminal.backend().buffer();
            let input_y = status_y - 1;
            for x in 1..width - 1 {
                assert_eq!(
                    buffer.cell((x, input_y - 1)).context("separator")?.symbol(),
                    "─"
                );
            }
            for x in [1, width - 2] {
                assert_eq!(
                    buffer.cell((x, input_y)).context("input inset")?.symbol(),
                    " "
                );
            }
            assert_eq!(
                buffer
                    .cell((2, input_y))
                    .context("input alignment")?
                    .symbol(),
                "A"
            );
        }
        Ok(())
    }
    #[test]
    fn composer_backtab_changes_mode_without_sending_or_losing_attachments() -> Result<()> {
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
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let p = ui.positions.get_mut("one").context("position")?;
        p.draft.insert("Unsent text ");
        p.draft.insert_attachment("[image 1]");
        let attachment = super::super::media::Attachment {
            label: "image 1".into(),
            path: "image.png".into(),
            kind: super::super::media::Kind::Image,
            hash: "fixture".into(),
        };
        p.attachments.push(attachment.clone());
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            ui.key(KeyEvent::new(KeyCode::BackTab, modifiers));
            assert!(
                matches!(rx.recv_timeout(Duration::from_secs(5))?, Request::Permissions { id, action: super::super::permissions::Action::CycleMode } if id == "one")
            );
            assert_eq!(ui.focus, Focus::Composer);
            let p = ui.positions.get("one").context("position")?;
            assert_eq!(p.draft.text(), "Unsent text [image 1]");
            assert_eq!(p.attachments.as_slice(), std::slice::from_ref(&attachment));
            assert!(p.outgoing.is_empty());
        }
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::List);
        ui.modal = Some(Modal::Model {
            model: Editor::default(),
            effort: Editor::default(),
            field: 1,
        });
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(matches!(ui.modal, Some(Modal::Model { field: 0, .. })));
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }
    #[test]
    fn mcp_command_is_available_for_both_providers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        for provider in [provider::Provider::Codex, provider::Provider::Claude] {
            ui.sessions.get_mut("one").context("fixture")?.provider = provider;
            ui.modal = Some(Modal::Commands {
                query: Editor::from("mcp"),
                selected: 0,
                skills_only: false,
                files_only: false,
            });
            ui.run_palette_command("/mcp", "mcp");
            assert!(!matches!(ui.modal, Some(Modal::Commands { .. })));
        }
        Ok(())
    }
    #[test]
    fn orchestrator_threads_live_in_a_separate_navigable_list() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let mut root = ui.sessions.get("one").context("fixture")?.clone();
        root.id = super::super::orchestrator::ID.into();
        root.orchestrator = true;
        root.title = "Difu orchestrator".into();
        let mut thread = root.clone();
        thread.id = "orchestrator-100-thread".into();
        thread.title = "Investigate billing".into();
        ui.summaries.extend([root.summary(), thread.summary()]);
        ui.sessions.insert(root.id.clone(), root);
        ui.sessions.insert(thread.id.clone(), thread);
        assert!(
            !ui.filtered()
                .iter()
                .any(|s| s.id == "orchestrator-100-thread")
        );
        ui.action(Action::Select(super::super::orchestrator::ID.into()));
        assert!(ui.orchestrator_list && !ui.drilled);
        assert!(ui.filtered().iter().all(|s| s.kind == "Orchestrator"));
        assert!(
            ui.filtered()
                .iter()
                .any(|s| s.id == "orchestrator-100-thread")
        );
        ui.action(Action::Select("orchestrator-100-thread".into()));
        ui.action(Action::Open);
        assert!(ui.drilled);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.drilled && ui.orchestrator_list);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.orchestrator_list);
        assert_eq!(ui.selected.as_deref(), Some(super::super::orchestrator::ID));
        assert!(ui.filtered().iter().any(|s| s.id == "one"));
        Ok(())
    }
    #[test]
    fn palette_review_follows_focus_and_selected_session() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let mut ui = state(storage.clone());
        ui.drilled = true;
        ui.panels.session = ui.selected.clone();
        ui.panels.view = Some(panels::View::PullRequest {
            app: Box::new(crate::app::App::new(storage, Config::default())),
        });
        ui.panels.focused = true;
        assert!(ui.palette_review().is_some());
        assert!(ui.palette_review_mut().is_some());
        ui.cycle_focus(false);
        assert!(!ui.panels.focused);
        assert!(ui.palette_review().is_none());
        assert!(ui.palette_review_mut().is_none());
        ui.panels.focused = true;
        ui.panels.hidden = true;
        assert!(ui.palette_review().is_none());
        assert!(ui.palette_review_mut().is_none());
        ui.panels.hidden = false;
        ui.selected = Some("another-session".into());
        assert!(ui.palette_review().is_none());
        assert!(ui.palette_review_mut().is_none());
        Ok(())
    }
    #[test]
    fn deletion_runs_without_blocking_and_preserves_current_activity() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        for fails in [false, true] {
            for with_other_session in [false, true] {
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
                    assert!(
                        matches!(serde_json::from_str::<Request>(&line)?, Request::Delete { id } if id == "one")
                    );
                    hold.recv_timeout(Duration::from_secs(5))?;
                    let reply = if fails {
                        Reply::Error("Worktree is locked".into())
                    } else {
                        Reply::Ok
                    };
                    serde_json::to_writer(&mut stream, &reply)?;
                    stream.write_all(b"\n")?;
                    Ok(())
                });
                let mut ui = state(storage);
                if with_other_session {
                    let mut other = ui.sessions.get("one").context("session")?.clone();
                    other.id = "two".into();
                    ui.summaries.push(other.summary());
                    ui.sessions.insert(other.id.clone(), other);
                }
                ui.drilled = true;
                ui.modal = Some(Modal::Delete);
                ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                assert!(ui.modal.is_none());
                assert!(!ui.busy && !ui.drilled);
                assert!(ui.deleting.contains("one"));
                assert_eq!(ui.focus, Focus::List);
                let selected = with_other_session.then_some("two");
                assert_eq!(ui.selected.as_deref(), selected);
                let (screen, _) = draw(&mut ui, 120, 30)?;
                assert!(screen.contains("Being deleted…"));
                assert!(!screen.contains("Enter confirms"));
                for (_, action) in &ui.hits {
                    assert!(!matches!(action, Action::Select(id) if id == "one"));
                }
                ui.action(Action::Select("one".into()));
                ui.open_session("one".into());
                ui.move_list(-1);
                ui.ensure_selected();
                assert_eq!(ui.selected.as_deref(), selected);
                assert!(ui.inbox.visible);
                assert_eq!(ui.focus, Focus::List);
                if with_other_session {
                    ui.open_session("two".into());
                    ui.paste("Continue other work");
                    assert_eq!(
                        ui.positions.get("two").context("draft")?.draft.text(),
                        "Continue other work"
                    );
                }
                // Completion must not close a new dialog or clear another task's busy flag.
                ui.modal = Some(Modal::Help(Default::default()));
                ui.busy = true;
                release.send(())?;
                let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
                ui.sender.send(response)?;
                ui.tick(false);
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("Fixture panicked"))??;
                assert!(ui.deleting.is_empty());
                assert!(ui.busy && matches!(ui.modal, Some(Modal::Help(_))));
                assert!(ui.drilled);
                assert_eq!(ui.sessions.contains_key("one"), fails);
                assert_eq!(ui.summaries.iter().any(|s| s.id == "one"), fails);
                if with_other_session {
                    assert_eq!(ui.selected.as_deref(), Some("two"));
                    assert_eq!(ui.focus, Focus::Composer);
                    assert_eq!(
                        ui.positions.get("two").context("draft")?.draft.text(),
                        "Continue other work"
                    );
                }
                if fails {
                    let (notice, error) = ui.notice.as_ref().context("error notice")?;
                    assert!(*error && notice.contains("Worktree is locked"));
                    ui.open_session("one".into());
                    assert_eq!(ui.selected.as_deref(), Some("one"));
                    assert!(ui.drilled);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn clear_requires_confirmation_and_keeps_the_draft_but_not_the_old_view() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let mut ui = state(storage);
        let mut cleared = ui.sessions.get("one").context("session")?.clone();
        cleared.entries.clear();
        cleared.title = "New session".into();
        cleared.note("system", "Session cleared");
        let reply = cleared.clone();
        let worker = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            assert!(
                matches!(serde_json::from_str::<Request>(&line)?, Request::Clear { id } if id == "one")
            );
            serde_json::to_writer(&mut stream, &Reply::Session(Box::new(reply)))?;
            stream.write_all(b"\n")?;
            Ok(())
        });
        ui.drilled = true;
        let position = ui.positions.entry("one".into()).or_default();
        position.draft.insert("Next task");
        position.follow = false;
        position.conversation = 40;
        position.expanded.insert("old-entry".into());
        ui.run_palette_command("/clear", "/clear");
        assert!(matches!(ui.modal, Some(Modal::Clear)));
        let (screen, _) = draw(&mut ui, 120, 30)?;
        assert!(screen.contains("uncommitted and unpushed work"));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none() && !ui.busy, "Esc must not clear");
        ui.run_palette_command("/clear", "/clear");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ui.modal.is_none() && ui.busy);
        let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        ui.sender.send(response)?;
        ui.tick(false);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Fixture panicked"))??;
        assert!(!ui.busy);
        let session = ui.sessions.get("one").context("cleared session")?;
        assert_eq!(session.entries.len(), 1);
        assert!(
            ui.summaries
                .iter()
                .any(|s| s.id == "one" && s.title == "New session")
        );
        let position = ui.positions.get("one").context("position")?;
        assert_eq!(position.draft.text(), "Next task");
        assert!(position.follow && position.conversation == 0 && position.expanded.is_empty());
        Ok(())
    }

    #[test]
    fn keyboard_skips_deleting_sessions_and_each_deletion_finishes_independently() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        for id in ["two", "three", "four"] {
            let mut other = ui.sessions.get("one").context("session")?.clone();
            other.id = id.into();
            ui.summaries.push(other.summary());
            ui.sessions.insert(id.into(), other);
        }
        ui.deleting.extend(["two".into(), "three".into()]);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.selected.as_deref(), Some("four"));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.selected.as_deref(), Some("one"));
        ui.sender.send(ResultMessage {
            kind: Task::Delete("two".into()),
            result: Ok(Reply::Ok),
        })?;
        ui.tick(false);
        assert!(!ui.deleting.contains("two") && ui.deleting.contains("three"));
        ui.select("three".into());
        assert_eq!(ui.selected.as_deref(), Some("one"));
        ui.sender.send(ResultMessage {
            kind: Task::Delete("three".into()),
            result: Err("Worktree is locked".into()),
        })?;
        ui.tick(false);
        ui.select("three".into());
        assert_eq!(ui.selected.as_deref(), Some("three"));
        Ok(())
    }

    #[test]
    fn manual_session_order_persists_and_stays_inside_pin_groups() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut ui = state(storage.clone());
        let template = ui.summaries.first().context("session")?.clone();
        ui.summaries = ["100-a", "200-b", "300-c", "400-d", "orchestrator"]
            .into_iter()
            .map(|id| {
                let mut summary = template.clone();
                summary.id = id.into();
                if id == "orchestrator" {
                    summary.kind = "Orchestrator".into();
                }
                summary
            })
            .collect();
        ui.toggle_pin("100-a");
        ui.toggle_pin("200-b");
        ui.panels.view = Some(panels::View::PullRequest {
            app: Box::new(crate::app::App::new(
                storage.clone(),
                storage.load_config()?,
            )),
        });
        fn ids(ui: &Ui) -> Vec<String> {
            ui.filtered().into_iter().map(|s| s.id).collect()
        }
        let original = ids(&ui);
        assert_eq!(
            original,
            ["orchestrator", "200-b", "100-a", "400-d", "300-c"]
        );
        // Simulate backend polls changing both activity and input order.
        ui.summaries.reverse();
        for (index, summary) in ui.summaries.iter_mut().enumerate() {
            summary.updated = index as i64;
            summary.status = if index % 2 == 0 {
                Status::Running
            } else {
                Status::Idle
            };
        }
        assert_eq!(ids(&ui), original);
        ui.focus = Focus::List;
        ui.selected = Some("100-a".into());
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(ids(&ui), original); // Cannot cross into unpinned sessions.
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(
            ids(&ui),
            ["orchestrator", "100-a", "200-b", "400-d", "300-c"]
        );
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(ids(&ui).first().map(String::as_str), Some("orchestrator"));
        ui.selected = Some("400-d".into());
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(
            ids(&ui),
            ["orchestrator", "100-a", "200-b", "400-d", "300-c"]
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(
            ids(&ui),
            ["orchestrator", "100-a", "200-b", "300-c", "400-d"]
        );
        assert_eq!(ui.selected.as_deref(), Some("400-d"));
        // Saving a setting in a pane opened before the reorder must preserve it.
        let Some(panels::View::PullRequest { app }) = &mut ui.panels.view else {
            anyhow::bail!("PR pane");
        };
        app.action(crate::app::Action::ToggleWrap);
        assert_eq!(storage.load_config()?.session_order, ui.session_order);
        let mut restored = Ui::new(storage.clone(), &storage.load_config()?);
        restored.summaries = ui.summaries.clone();
        assert_eq!(ids(&restored), ids(&ui));
        // A newly created session appears first in its group without disturbing
        // the relative order of sessions that were manually arranged.
        let mut newest = template;
        newest.id = "500-e".into();
        restored.summaries.push(newest);
        assert_eq!(
            ids(&restored),
            ["orchestrator", "100-a", "200-b", "500-e", "300-c", "400-d"]
        );
        Ok(())
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
        assert!(screen.contains("📌  Pinned task"));
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
    fn sidebar_cards_flip_their_changes_highlight_and_new_order() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        let first = ui.summaries.first().context("session")?.title.clone();
        let mut other = ui.summaries.first().context("session")?.clone();
        other.id = "two".into();
        other.title = "Second card".into();
        ui.summaries.push(other);
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(
            screen.contains("Second card") && screen.contains(&first),
            "the first list shows at once"
        );
        ui.summaries.last_mut().context("other")?.title = "Second card renamed".into();
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(screen.contains("Second card") && !screen.contains("renamed"));
        let (screen, _) = draw_settled(&mut ui, 120, 40)?;
        assert!(screen.contains("Second card renamed"));
        // Moving the selection flips the highlight off one card and onto the
        // other, a flap at a time.
        ui.select("two".into());
        draw(&mut ui, 120, 40)?;
        assert_eq!(ui.sidebar_highlights.len(), 2);
        // The status icon now occupies the first cells. Sample after the
        // highlight has advanced into the text instead of under that icon.
        ui.sidebar_highlights.insert(
            "two".into(),
            flap::Highlight::start(
                true,
                false,
                Instant::now() - Duration::from_millis(40),
                ui.tweaks,
            )
            .context("selection animation")?,
        );
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(screen.contains('▀'));
        let (screen, _) = draw_settled(&mut ui, 120, 40)?;
        assert!(!screen.contains('▀'));
        // A card moving into another slot flips from the text that was there.
        assert_eq!(ui.filtered().first().map(|s| s.id.as_str()), Some("two"));
        ui.toggle_pin("one");
        assert_eq!(ui.filtered().first().map(|s| s.id.as_str()), Some("one"));
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(!screen.contains("Second card renamed"), "{screen}");
        let (screen, _) = draw_settled(&mut ui, 120, 40)?;
        assert!(screen.contains(&format!("📌  {first}")) && screen.contains("Second card renamed"));
        Ok(())
    }

    #[test]
    fn pending_requests_show_the_hand_in_the_sidebar_and_chat_header() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        still(&mut ui);
        ui.drilled = true;
        let check = |ui: &mut Ui, expected: &str| -> Result<()> {
            ui.summaries = vec![ui.sessions.get("one").context("session")?.summary()];
            let mut terminal = Terminal::new(TestBackend::new(120, 40))?;
            terminal.draw(|frame| ui.draw(frame))?;
            let sidebar = ui
                .hits
                .iter()
                .find_map(|(rect, action)| {
                    matches!(action, Action::Select(id) if id == "one").then_some(*rect)
                })
                .context("sidebar row")?;
            let chat = ui
                .hits
                .iter()
                .find_map(|(rect, action)| {
                    matches!(action, Action::Focus(Focus::Conversation)).then_some(*rect)
                })
                .context("conversation box")?;
            for (rect, inset) in [(sidebar, 0), (chat, 2)] {
                let icon = terminal
                    .backend()
                    .buffer()
                    .cell((rect.x + inset, rect.y))
                    .context("status icon")?;
                assert_eq!(icon.symbol(), expected);
                if expected.starts_with('✋') {
                    assert_eq!(icon.fg, crate::ui::YELLOW);
                }
            }
            Ok(())
        };
        for (status, normal) in [
            (Status::Idle, "●"),
            (Status::Running, "🏃\u{fe0e}"),
            (Status::Interrupted, "⚠\u{fe0e}"),
        ] {
            let session = ui.sessions.get_mut("one").context("session")?;
            session.status = status;
            session.queue = vec!["A queued prompt is not a request".into()];
            session.pending = vec![Pending {
                id: serde_json::json!("questions"),
                method: "item/tool/requestUserInput".into(),
                params: serde_json::json!({
                    "difuAsync": true,
                    "questions": [{"id":"first", "question":"First?"}, {"id":"second", "question":"Second?"}],
                    "difuAnswers": {"first":"Answered"}
                }),
                responded: false,
            }];
            check(&mut ui, "✋\u{fe0e}")?;
            let session = ui.sessions.get_mut("one").context("session")?;
            assert_eq!(
                session.status, status,
                "icons do not change execution state"
            );
            *session
                .pending
                .first_mut()
                .context("questions")?
                .params
                .get_mut("difuAnswers")
                .context("answers")? = serde_json::json!({"first":"Answered", "second":"Answered"});
            check(&mut ui, normal)?;
            ui.sessions
                .get_mut("one")
                .context("session")?
                .pending
                .push(Pending {
                    id: serde_json::json!("approval"),
                    method: "item/commandExecution/requestApproval".into(),
                    params: serde_json::json!({}),
                    responded: false,
                });
            check(&mut ui, "✋\u{fe0e}")?;
            ui.sessions
                .get_mut("one")
                .context("session")?
                .pending
                .clear();
            check(&mut ui, normal)?;
        }
        Ok(())
    }

    #[test]
    fn switching_sessions_reveals_once_and_respects_motion_settings() -> Result<()> {
        for (level, screen, speed, animated) in [
            (crate::settings::Motion::Full, true, 200, true),
            (crate::settings::Motion::Full, false, 200, false),
            (crate::settings::Motion::Full, true, 0, false),
            (crate::settings::Motion::Reduced, true, 200, false),
            (crate::settings::Motion::Off, true, 200, false),
        ] {
            let dir = tempfile::tempdir()?;
            let mut ui = state(Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            });
            ui.preferences.motion.level = level;
            ui.preferences.motion.screen_transition = screen;
            ui.preferences.motion.sidebar = false;
            ui.tweaks.agent_reply_cps = speed;
            ui.drilled = true;
            let mut second = ui.sessions.get("one").context("first session")?.clone();
            second.id = "two".into();
            second.title = "Second session".into();
            ui.summaries.push(second.summary());
            ui.sessions.insert("two".into(), second);
            ui.positions.entry("one".into()).or_default().draft = Editor::from("First draft");
            ui.positions.entry("two".into()).or_default().draft = Editor::from("Second draft");
            draw(&mut ui, 120, 40)?;
            assert!(ui.session_switched.is_none(), "initial render is immediate");
            ui.select("two".into());
            draw(&mut ui, 120, 40)?;
            assert_eq!(ui.session_switched.is_some(), animated);
            if let Some(started) = ui.session_switched {
                draw(&mut ui, 120, 40)?;
                assert_eq!(
                    ui.session_switched,
                    Some(started),
                    "polling must not replay the switch"
                );
                ui.session_switched = Some(started - Duration::from_secs(1));
                let (settled, _) = draw(&mut ui, 120, 40)?;
                assert!(ui.session_switched.is_none());
                assert!(settled.contains("Second session"));
            }
            assert_eq!(
                ui.positions.get("one").context("first draft")?.draft.text(),
                "First draft"
            );
            assert_eq!(
                ui.positions
                    .get("two")
                    .context("second draft")?
                    .draft
                    .text(),
                "Second draft"
            );
        }
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
    fn entering_a_session_with_questions_keeps_the_composer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        });
        ui.sessions.get_mut("one").context("session")?.pending.push(Pending {
            id: serde_json::json!("question"), method: "item/tool/requestUserInput".into(), responded: false,
            params: serde_json::json!({"questions":[{"id":"q","question":"Which scope?","options":[]}]}),
        });
        ui.drilled = true;
        ui.focus = Focus::List;
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(ui.focus, Focus::Composer);
        assert!(ui.modal.is_none());
        ui.open_questions();
        assert!(ui.inline_question());
        Ok(())
    }

    #[test]
    fn worktree_preparation_keeps_the_composer_and_send_available() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let worker = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            let request: Request = serde_json::from_str(&line)?;
            assert!(matches!(request, Request::Control {
                control: Control::Message { text, .. }, ..
            } if text == "Prepare this change"));
            serde_json::to_writer(&mut stream, &Reply::Ok)?;
            stream.write_all(b"\n")?;
            Ok(())
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.status = Status::Starting;
        session.entries.clear();
        ui.paste("Prepare this change");
        let (screen, _) = draw_settled(&mut ui, 150, 35)?;
        assert!(screen.contains("Preparing worktree"));
        assert!(screen.contains("Messages sent now will be queued"));
        assert!(screen.contains("Prepare this change"));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .outgoing
                .first()
                .context("outgoing message")?
                .queued
        );
        let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        ui.sender.send(response)?;
        ui.tick(false);
        assert!(!ui.busy);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .text()
                .is_empty()
        );
        ui.paste("Another message while preparing");
        let (screen, _) = draw(&mut ui, 150, 35)?;
        assert!(screen.contains("Another message while preparing"));
        assert!(!screen.contains("interrupt and send immediately"));
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Fixture panicked"))??;
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
        still(&mut ui);
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
        assert!(position.draft.text().is_empty());
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
            serde_json::json!({"difuSteeringTurn":"t"}).into();
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
        still(&mut ui);
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
        assert_eq!(screen.matches("Show this immediately").count(), 1);
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
            serde_json::json!({"difuSteeringTurn":"t"}).into();
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
    fn rapid_sends_clear_immediately_deliver_in_order_and_leave_new_drafts_alone() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (sent, received) = mpsc::channel();
        let (release, hold) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request: Request = serde_json::from_str(&line)?;
                sent.send(request)?;
                hold.recv_timeout(Duration::from_secs(5))?;
                serde_json::to_writer(&mut stream, &Reply::Ok)?;
                stream.write_all(b"\n")?;
            }
            Ok(())
        });
        let mut ui = state(storage);
        still(&mut ui);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("first");
        ui.send(false);
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("second");
        ui.send(false);
        assert!(!ui.busy);
        assert!(ui.take_send_repaint());
        assert!(!ui.take_send_repaint());
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .text()
                .is_empty()
        );
        assert_eq!(ui.send_queues.get("one").context("send queue")?.len(), 2);
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert!(screen.contains("› first"));
        assert!(screen.contains("› second"));
        // Even an identical new draft must survive the earlier send's acknowledgement.
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("second");
        for text in ["first", "second"] {
            let request = received.recv_timeout(Duration::from_secs(5))?;
            assert!(matches!(request, Request::Control {
                control: Control::Message { text: actual, .. }, ..
            } if actual == text));
            release.send(())?;
            let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
            ui.sender.send(response)?;
            ui.tick(false);
            assert_eq!(
                ui.positions.get("one").context("position")?.draft.text(),
                "second"
            );
        }
        assert!(!ui.send_queues.contains_key("one"));
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }

    #[test]
    fn failed_sends_offer_retry_with_original_attachments_and_skills() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (sent, received) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for reply in [Reply::Error("Disconnected".into()), Reply::Ok] {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                sent.send(serde_json::from_str::<Value>(&line)?)?;
                serde_json::to_writer(&mut stream, &reply)?;
                stream.write_all(b"\n")?;
            }
            Ok(())
        });
        let mut ui = state(storage.clone());
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let attachment = super::super::media::Attachment {
            label: "image 1".into(),
            path: "image.png".into(),
            kind: super::super::media::Kind::Image,
            hash: "image".into(),
        };
        let skill = Skill {
            name: "review".into(),
            path: "review/SKILL.md".into(),
            description: String::new(),
            enabled: true,
            slash: false,
        };
        let p = ui.positions.get_mut("one").context("position")?;
        p.draft = Editor::from("Review ");
        p.draft.insert_attachment(&attachment.token());
        p.attachments = vec![attachment.clone()];
        p.saved_attachments = p.attachments.clone();
        p.skills = vec![skill.clone()];
        super::super::media::save_draft(&storage, "one", &p.attachments)?;
        ui.send(true);
        let p = ui.positions.get("one").context("position")?;
        assert!(p.draft.text().is_empty());
        assert!(p.attachments.is_empty() && p.skills.is_empty());
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("Keep typing");
        let original = received.recv_timeout(Duration::from_secs(5))?;
        let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        ui.sender.send(response)?;
        ui.tick(false);
        ui.cache_writer.flush()?;
        assert!(super::super::media::load_draft(&storage, "one")?.is_empty());
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| *error && text.contains("Disconnected"))
        );
        let p = ui.positions.get("one").context("position")?;
        assert_eq!(p.draft.text(), "Keep typing");
        let failed = p.outgoing.first().context("failed message")?;
        let entry = failed.entry_id();
        assert!(
            matches!(&failed.failed, Some(Control::MessageWithAttachments {
            attachments, skills, queue: true, ..
        }) if attachments == std::slice::from_ref(&attachment) && skills == std::slice::from_ref(&skill))
        );
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert!(screen.contains("Not sent · Enter/click to retry"));
        assert!(
            ui.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::ToggleEntry(id) if id == &entry))
        );
        ui.action(Action::ToggleEntry(entry));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Keep typing"
        );
        assert_eq!(received.recv_timeout(Duration::from_secs(5))?, original);
        let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        ui.sender.send(response)?;
        ui.tick(false);
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .outgoing
                .iter()
                .all(|p| p.failed.is_none())
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
                token: 0,
                text: "Follow up".into(),
                queued: false,
                observed_before: 0,
                failed: None,
            },
            PendingSend {
                token: 1,
                text: "Next turn".into(),
                queued: true,
                observed_before: 0,
                failed: None,
            },
        ];
        let (screen, _) = draw(&mut ui, 150, 40)?;
        assert!(screen.contains("↳ Follow up"));
        assert!(!screen.contains("› Follow up"));
        let session = ui.sessions.get_mut("one").context("session")?;
        session.entries.first_mut().context("tool")?.finished_at = Some(2);
        session.touch();
        // Moving into the chat, the message flips in.
        let (screen, _) = draw_settled(&mut ui, 150, 40)?;
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
        let (screen, _) = draw_settled(&mut ui, 150, 40)?;
        assert!(screen.contains("› Another follow up"));
        assert!(!screen.contains("› Follow up"));
        Ok(())
    }

    #[test]
    fn queue_snapshots_reconcile_repeated_messages_and_failures_preserve_new_drafts() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        let p = ui.positions.get_mut("one").context("position")?;
        p.outgoing = vec![
            PendingSend {
                token: 0,
                text: "again".into(),
                queued: true,
                observed_before: 0,
                failed: None,
            },
            PendingSend {
                token: 1,
                text: "again".into(),
                queued: false,
                observed_before: 1,
                failed: None,
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
        p.draft = Editor::from("a newer draft");
        p.outgoing.push(PendingSend {
            token: 2,
            text: "retry this".into(),
            queued: false,
            observed_before: 0,
            failed: None,
        });
        p.outgoing.push(PendingSend {
            token: 3,
            text: "retry this".into(),
            queued: false,
            observed_before: 1,
            failed: None,
        });
        ui.send_queues.insert(
            "one".into(),
            VecDeque::from([(
                2,
                Control::Message {
                    text: "retry this".into(),
                    queue: false,
                    skills: vec![],
                    attachments: vec![],
                },
            )]),
        );
        // A send result must not unlock an unrelated action.
        ui.busy = true;
        ui.sender.send(ResultMessage {
            kind: Task::Send("one".into(), 2),
            result: Err("Disconnected".into()),
        })?;
        ui.tick(false);
        let p = ui.positions.get("one").context("position")?;
        assert!(p.outgoing.first().is_some_and(|p| p.failed.is_some()));
        assert_eq!(p.draft.text(), "a newer draft");
        assert!(ui.busy);
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| *error && text.contains("Disconnected"))
        );
        // A later successful copy reconciles without discarding the failed one.
        let mut snapshot = ui.sessions.get("one").context("session")?.clone();
        snapshot.note("userMessage", "retry this");
        ui.sender.send(ResultMessage {
            kind: Task::Read("one".into()),
            result: Ok(Reply::Session(Box::new(snapshot))),
        })?;
        ui.tick(false);
        let p = ui.positions.get("one").context("position")?;
        assert_eq!(p.outgoing.len(), 1);
        assert!(
            p.outgoing
                .first()
                .is_some_and(|pending| pending.token == 2 && pending.failed.is_some())
        );
        Ok(())
    }

    #[test]
    fn native_synchronous_approval_escape_opens_the_request_without_interrupting() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
        ] {
            let session = ui.sessions.get_mut("one").context("session")?;
            session.status = Status::Waiting;
            session.pending = vec![Pending {
                id: serde_json::json!(method),
                method: method.into(),
                params: serde_json::json!({}),
                responded: false,
            }];
            session.queue = vec!["Keep queued".into()];
            ui.modal = None;
            for _ in 0..3 {
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(ui.interrupting.is_empty());
            }
            assert!(
                matches!(&ui.modal, Some(Modal::Approval { pending, .. }) if pending.id == serde_json::json!(method))
            );
            assert_eq!(ui.sessions.get("one").context("session")?.queue.len(), 1);
        }
        Ok(())
    }
    #[test]
    fn synchronous_claude_approval_survives_escape_and_only_explicit_actions_are_sent() -> Result<()>
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
        // Same raw can_use_tool approval shape constructed by the Claude adapter.
        let mut pending = Pending {
            id: serde_json::json!("claude-permission"),
            method: "mcpServer/elicitation/request".into(),
            params: serde_json::json!({"serverName":"Claude Code","message":"Allow mcp__clickhouse__list_databases?","claudeInput":{},"tool":"mcp__clickhouse__list_databases","requestedSchema":{"type":"object","properties":{}}}),
            responded: false,
        };
        super::super::elicitation::normalize(&mut pending);
        assert!(super::super::elicitation::is_form(&pending));
        assert!(!pending.is_async_question());
        let mut ui = state(storage);
        ui.drilled = true;
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert("Keep this draft");
        let session = ui.sessions.get_mut("one").context("session")?;
        session.provider = provider::Provider::Claude;
        session.status = Status::Waiting;
        session.pending.push(pending.clone());
        session.queue.push("Send this later".into());
        let queued = transcript::waiting_messages(session, 100, &[])
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(queued.contains("esc opens approval"));
        assert!(!queued.contains("interrupt and send"));
        for focus in [Focus::Composer, Focus::Conversation] {
            ui.focus = focus;
            ui.modal = None;
            for kind in [
                crossterm::event::KeyEventKind::Press,
                crossterm::event::KeyEventKind::Repeat,
                crossterm::event::KeyEventKind::Repeat,
            ] {
                ui.key(KeyEvent::new_with_kind(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                    kind,
                ));
                assert!(ui.interrupting.is_empty());
                assert!(rx.try_recv().is_err());
            }
            // A fragmented legacy Alt+Up can arrive as Escape, then the remaining
            // Up event; this protects the decoded Escape without claiming its origin.
            ui.modal = None;
            ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            assert!(ui.interrupting.is_empty());
            assert!(
                matches!(&ui.modal, Some(Modal::Approval { pending, .. }) if pending.id == serde_json::json!("claude-permission"))
            );
        }
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Keep this draft"
        );
        assert_eq!(ui.sessions.get("one").context("session")?.pending.len(), 1);
        // Selecting Allow once sends the native question answer, never an interrupt.
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5))?, Request::Control { id, control: Control::AnswerQuestion { request, question, answer: Some(answer) } } if id == "one" && request == serde_json::json!("claude-permission") && question == "difu-decision" && answer == "Allow once")
        );
        ui.question_send = None;
        ui.busy = false;
        ui.menu_action(2);
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5))?, Request::Control { id, control: Control::Interrupt } if id == "one")
        );
        // A normal asynchronous user question retains the existing global Escape behavior.
        ui.modal = None;
        ui.busy = false;
        ui.sessions.get_mut("one").context("session")?.queue.clear();
        ui.sessions.get_mut("one").context("session")?.pending = vec![Pending {
            id: serde_json::json!("async"),
            method: "item/tool/requestUserInput".into(),
            params: serde_json::json!({"difuAsync":true,"questions":[{"id":"q","question":"Continue?","options":[{"label":"Yes"}]}]}),
            responded: false,
        }];
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5))?, Request::Control { id, control: Control::Interrupt } if id == "one")
        );
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
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
                ui.modal = Some(Modal::Status { scroll: 0 });
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
                ui.modal = Some(Modal::Status { scroll: 0 });
                ui.sender.send(reply)?;
                ui.tick(false);
                assert!(ui.interrupting.is_empty());
                assert!(ui.busy);
                assert!(matches!(ui.modal, Some(Modal::Status { .. })));
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
            let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
            ui.sender.send(response)?;
            ui.tick(false);
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
        ui.cache_writer.flush()?;
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
        ui.cache_writer.flush()?;
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
                text: text.into(),
                data: data.into(),
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
        session
            .entries
            .last_mut()
            .context("last entry")?
            .text
            .push_str(" streamed");
        session.touch();
        window.render(session, &mut position, 80, 25, false);
        assert_eq!(
            window.formatted,
            measured + 1,
            "streaming must only reformat the changed message"
        );
        session.touch();
        window.render(session, &mut position, 80, 25, false);
        assert_eq!(
            window.formatted,
            measured + 1,
            "metadata updates must preserve transcript rows"
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
        assert_eq!(ui.conversation_height, height);
        ui.notice = Some(("Action accepted".into(), false));
        let (with_notice, _) = draw(&mut ui, 120, 40)?;
        assert_eq!(with_notice.matches("Action accepted").count(), 1);
        assert_eq!(ui.conversation_height, height);
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
    fn alt_m_model_picker_snaps_to_the_current_model_and_applies_or_saves_defaults() -> Result<()> {
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
        ui.skills.insert("one".into(), (Vec::new(), Vec::new()));
        ui.model_completion.options = vec![
            crate::model::ModelInfo {
                id: "fixture-astra".into(),
                name: "Astra".into(),
                efforts: vec!["high".into()],
            },
            crate::model::ModelInfo {
                id: "claude/fixture-luna".into(),
                name: "Claude Code · Luna".into(),
                efforts: vec!["low".into(), "medium".into()],
            },
        ];
        {
            let session = ui.sessions.get_mut("one").context("session")?;
            session.model = Some("claude/fixture-luna".into());
            session.effort = Some("medium".into());
        }
        ui.key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        assert!(matches!(ui.modal, Some(Modal::ModelPicker(_))));
        let (screen, _) = draw(&mut ui, 100, 30)?;
        assert!(screen.contains("Select model"));
        assert!(screen.contains("1. Astra") && screen.contains("2. Claude Code · Luna ✔"));
        assert!(screen.contains("Medium effort"), "{screen}");
        // The highlighted row is the session's model, so Enter keeps it and raises the effort.
        ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 100, 30)?;
        assert!(screen.contains("Low effort"), "{screen}");
        ui.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 100, 30)?;
        // Astra does not support "low", so the effort falls back to the model default.
        assert!(screen.contains("Default effort"), "{screen}");
        ui.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        match rx.recv_timeout(Duration::from_secs(5))? {
            Request::Control {
                id,
                control: Control::Model { model, effort },
            } => {
                assert_eq!(id, "one");
                assert_eq!(model.as_deref(), Some("fixture-astra"));
                assert_eq!(effort.as_deref(), Some("high"));
            }
            other => anyhow::bail!("Unexpected request: {other:?}"),
        }
        assert!(ui.defaults.model.is_none());
        ui.busy = false;
        // Enter saves the pick for new sessions and applies it to the open session.
        ui.open_commands(false);
        ui.paste("model");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::ModelPicker(_))));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        match rx.recv_timeout(Duration::from_secs(5))? {
            Request::Control {
                id,
                control: Control::Model { model, effort },
            } => {
                assert_eq!(id, "one");
                assert_eq!(model.as_deref(), Some("claude/fixture-luna"));
                assert_eq!(effort.as_deref(), Some("medium"));
            }
            other => anyhow::bail!("Unexpected request: {other:?}"),
        }
        assert_eq!(ui.defaults.model.as_deref(), Some("claude/fixture-luna"));
        assert_eq!(ui.defaults.effort.as_deref(), Some("medium"));
        let saved = ui.storage.load_config()?.agent_defaults;
        assert_eq!(saved.model.as_deref(), Some("claude/fixture-luna"));
        assert_eq!(saved.effort.as_deref(), Some("medium"));
        ui.busy = false;
        ui.key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        let (screen, _) = draw(&mut ui, 100, 30)?;
        assert!(screen.contains("claude/fixture-luna · default"), "{screen}");
        ui.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::Model { field: 0, .. })));
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
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
        session.entries.push(Entry { id: "command".into(), kind: "commandExecution".into(), text: "const output = 42;\nsecond\nthird\nfourth".into(), data: serde_json::json!({"command":"python3 - <<'PY'\nfrom pathlib import Path\nPY","exitCode":0}).into(), ..Entry::default() });
        session.entries.push(Entry { id: "edit".into(), kind: "fileChange".into(), data: serde_json::json!({"changes":[{"path":"src/example.rs","diff":"@@ -9 +9,2 @@\n-old\n+let answer = 42;\n+return answer;"}]}).into(), ..Entry::default() });
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
                    .any(|span| span.content.contains(keyword)
                        && span.style.fg == Some(crate::theme::slot(crate::theme::Role::Keyword))),
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
            data: serde_json::json!({"aggregatedOutput":""}).into(),
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
        ui.menu_action(13);
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
    /// Draws once everything new on screen has flipped in.
    fn draw_settled(ui: &mut Ui, width: u16, height: u16) -> Result<(String, Option<(u16, u16)>)> {
        draw(ui, width, height)?;
        ui.settle_flips();
        draw(ui, width, height)
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
                    .all(|s| s.style.bg
                        == Some(crate::theme::slot(crate::theme::Role::FocusedEntry)))
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
    fn user_prompt_remains_inline_with_compact_full_width_bar() -> Result<()> {
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
        let prompt = rows.get(start).context("prompt row")?;
        assert_eq!(prompt.width(), 60);
        assert_eq!(prompt.style, crate::ui::user_message_style());
        assert_eq!(prompt.to_string().trim_end(), "› Keep this prompt in place");
        let separator = rows.get(start + 1).context("message separator")?;
        assert!(separator.to_string().is_empty());
        assert_eq!(separator.style.bg, None);
        assert_eq!(
            rows.get(start + 2)
                .context("following response")?
                .to_string()
                .trim(),
            "Following response"
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
            Some(crate::theme::slot(crate::theme::Role::UserMessageText))
        );
        assert_eq!(
            crate::ui::user_message_style().bg,
            Some(crate::theme::slot(crate::theme::Role::UserMessage))
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
                    slash: false,
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
    fn claude_skills_complete_as_slash_commands_sent_as_message_text() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        let command = |name: &str, description: &str| Skill {
            name: name.into(),
            path: Default::default(),
            description: description.into(),
            enabled: true,
            slash: true,
        };
        ui.skills.insert(
            "one".into(),
            (
                vec![
                    command("fixture-skill", "<target> · Fixture skill (project)"),
                    command("compact", "Claude's own compaction"),
                ],
                Vec::new(),
            ),
        );
        let entries = ui.palette_commands();
        assert!(
            entries
                .iter()
                .any(|(name, description, _)| name == "/fixture-skill"
                    && description == "<target> · Fixture skill (project)")
        );
        // difu's /compact handles the name; Claude's duplicate is not listed.
        assert_eq!(
            entries
                .iter()
                .filter(|(name, _, _)| name == "/compact")
                .map(|(_, description, _)| description.as_str())
                .collect::<Vec<_>>(),
            ["Compact the current conversation"]
        );
        ui.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        ui.paste("fixture-sk");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        let position = ui.positions.get_mut("one").context("draft")?;
        assert_eq!(position.draft.text(), "/fixture-skill ");
        assert!(position.skills.is_empty());
        position.draft.insert("src/lib.rs");
        assert!(!ui.command_draft(), "Claude commands are sent as messages");
        let position = ui.positions.get_mut("one").context("draft")?;
        position.draft.clear();
        position.draft.insert("/status");
        assert!(ui.command_draft());
        assert!(matches!(ui.modal, Some(Modal::Status { .. })));
        Ok(())
    }
    #[test]
    fn sending_flips_the_composer_away_and_the_message_into_chat() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        ui.drilled = true;
        ui.focus = Focus::Composer;
        draw(&mut ui, 120, 30)?;
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("Typed message");
        ui.send(false);
        assert!(
            ui.redraw_interval().is_some(),
            "animations keep frames coming"
        );
        let (screen, _) = draw(&mut ui, 120, 30)?;
        // The composer's text has started flipping away from its end; the chat
        // copy has only started flipping in.
        assert!(screen.contains("Typed messa") && !screen.contains("Typed message"));
        assert!(!screen.contains("Ask anything"));
        assert!(
            ui.positions
                .get("one")
                .context("draft")?
                .draft
                .text()
                .is_empty()
        );
        let position = ui.positions.get_mut("one").context("position")?;
        for typing in [&mut position.sent, &mut position.clearing]
            .into_iter()
            .flatten()
        {
            typing.age(Duration::from_secs(2));
        }
        let (screen, _) = draw(&mut ui, 120, 30)?;
        assert!(screen.contains("› Typed message"));
        assert!(screen.contains("Ask anything"));
        assert!(!ui.positions.get("one").context("position")?.animating());
        Ok(())
    }
    #[test]
    fn tweaks_save_animation_timing_and_reject_invalid_values() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = state(storage.clone());
        assert!(
            ui.palette_commands()
                .iter()
                .any(|(name, _, _)| name == "/tweaks")
        );
        ui.open_tweaks();
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(screen.contains("Agent reply speed") && screen.contains("200"));
        let set = |ui: &mut Ui, value: &str| {
            ui.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            ui.paste(value);
            ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        };
        set(&mut ui, "120");
        set(&mut ui, "0");
        set(&mut ui, "20000");
        set(&mut ui, "300");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::Tweaks { .. })));
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| *error && text.contains("0 to 10000"))
        );
        assert_eq!(ui.tweaks, crate::storage::Tweaks::default());
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        set(&mut ui, "750");
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let expected = crate::storage::Tweaks {
            agent_reply_cps: 120,
            sent_message_ms: 0,
            input_clear_ms: 750,
            max_animation_ms: 300,
        };
        assert!(ui.modal.is_none());
        assert_eq!(ui.tweaks, expected);
        assert_eq!(storage.load_config()?.tweaks, expected);
        // A sent message is not animated when its duration is 0.
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.positions.get_mut("one").context("position")?.draft = Editor::from("No typing");
        ui.send(false);
        let position = ui.positions.get("one").context("position")?;
        assert!(position.sent.is_none() && position.clearing.is_some());
        Ok(())
    }
    #[test]
    fn theme_picker_previews_saves_and_restores_default() -> Result<()> {
        use crate::theme;
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = state(storage.clone());
        assert!(
            ui.palette_commands()
                .iter()
                .any(|(name, _, _)| name == "/theme")
        );
        let key = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE));

        // Moving previews live; Esc returns to the saved theme. The picker
        // leaves the conversation visible so the preview shows on it.
        ui.drilled = true;
        ui.run_palette_command("/theme", "/theme");
        assert!(ui.settings_requested.is_none());
        assert!(matches!(ui.modal, Some(Modal::Theme { .. })));
        let (screen, _) = draw(&mut ui, 120, 40)?;
        assert!(
            screen.contains("Default")
                && screen.contains("GitHub Light")
                && screen.contains("saved")
        );
        assert!(screen.contains("A streamed response with useful context"));
        assert!(
            theme::themes()
                .iter()
                .all(|choice| choice.name.chars().count() <= themes::NAME_WIDTH)
        );
        // A short terminal scrolls the list to keep the selection visible.
        key(&mut ui, KeyCode::End);
        let (screen, _) = draw(&mut ui, 100, 16)?;
        assert!(screen.contains("› Gruvbox Light"));
        key(&mut ui, KeyCode::Home);
        key(&mut ui, KeyCode::Down);
        assert_eq!(theme::active().id, "amber");
        key(&mut ui, KeyCode::Esc);
        assert_eq!(theme::active().id, "default");
        assert_eq!(storage.load_config()?.theme, None);

        // A preview never outlives the picker, however it closes.
        ui.open_theme();
        key(&mut ui, KeyCode::Down);
        ui.modal = None;
        ui.tick(false);
        assert_eq!(theme::active().id, "default");

        ui.open_theme();
        for _ in 0..theme::index(Some("dracula")) {
            key(&mut ui, KeyCode::Down);
        }
        key(&mut ui, KeyCode::Enter);
        assert!(ui.modal.is_none());
        assert_eq!(theme::active().id, "dracula");
        assert_eq!(storage.load_config()?.theme.as_deref(), Some("dracula"));
        ui.tick(false);
        assert_eq!(theme::active().id, "dracula", "a saved theme stays");

        // Choosing Default goes back to the original colours and clears the setting.
        ui.open_theme();
        key(&mut ui, KeyCode::Home);
        key(&mut ui, KeyCode::Enter);
        assert_eq!(theme::active().id, "default");
        assert_eq!(storage.load_config()?.theme, None);

        // Every theme resolves every colour difu draws.
        ui.drilled = true;
        ui.focus = Focus::Composer;
        for (index, choice) in theme::themes().iter().enumerate() {
            theme::select(index);
            let mut terminal = Terminal::new(TestBackend::new(120, 40))?;
            terminal.draw(|frame| {
                ui.draw(frame);
                theme::present(frame.buffer_mut());
            })?;
            let buffer = terminal.backend().buffer();
            assert!(
                buffer
                    .content
                    .iter()
                    .all(|cell| !theme::is_slot(cell.fg) && !theme::is_slot(cell.bg)),
                "{} left a colour unresolved",
                choice.id
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.fg == choice.color(theme::Role::Accent)),
                "{} accent is drawn",
                choice.id
            );
        }
        theme::select(0);
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
            data: serde_json::json!({"command":"git diff --stat"}).into(),
            started_at: session.turn_started_at,
            ..Entry::default()
        });
        session.entries.push(Entry {
            id: "review".into(),
            kind: "autoApprovalReview".into(),
            data: serde_json::json!({"targetItemId":"tool"}).into(),
            started_at: session.turn_started_at,
            ..Entry::default()
        });
        session.entries.push(Entry {
            id: "steer".into(),
            kind: "userMessage".into(),
            text: "Focus on navigation".into(),
            data: serde_json::json!({"difuSteeringTurn":"active"}).into(),
            ..Entry::default()
        });
        session.queue.push("Explain the result next".into());
        let position = ui.positions.get_mut("one").context("position")?;
        position.follow = false;
        position.conversation = 0;
        let (screen, _) = draw_settled(&mut ui, 200, 60)?;
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
                    slash: false,
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
        ui.sidebar.save(&storage, &ui.cache_writer)?;
        ui.cache_writer.flush()?;
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
        let (screen, _) = draw_settled(&mut ui, 160, 35)?;
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
    fn decoded_alt_up_and_question_header_keep_the_current_idle_session_and_draft() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        });
        still(&mut ui);
        ui.drilled = true;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.provider = provider::Provider::Claude;
        session.status = Status::Idle;
        session.pending = vec![Pending {
            id: serde_json::json!("async-question"),
            method: "item/tool/requestUserInput".into(),
            params: serde_json::json!({"difuAsync":true,"questions":[{"id":"next","question":"What should happen next?","options":[{"label":"Continue"},{"label":"Wait"}]}]}),
            responded: false,
        }];
        let mut other = session.clone();
        other.id = "two".into();
        other.title = "Other chat".into();
        other.pending.clear();
        ui.summaries = vec![other.summary(), session.summary()];
        ui.sessions.insert("two".into(), other);
        ui.session_order = vec!["two".into(), "one".into()];
        ui.positions
            .entry("one".into())
            .or_default()
            .draft
            .insert("Keep the current draft");
        ui.positions
            .entry("two".into())
            .or_default()
            .draft
            .insert("Other chat draft");
        for focus in [Focus::Composer, Focus::List] {
            ui.focus = focus;
            ui.modal = None;
            ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            assert_eq!(ui.selected.as_deref(), Some("one"));
            assert!(
                matches!(&ui.modal, Some(Modal::Approval { pending, .. }) if pending.id == serde_json::json!("async-question"))
            );
            assert_eq!(
                ui.positions
                    .get("one")
                    .context("current draft")?
                    .draft
                    .text(),
                "Keep the current draft"
            );
            assert_eq!(
                ui.positions.get("two").context("other draft")?.draft.text(),
                "Other chat draft"
            );
        }
        ui.modal = None;
        ui.focus = Focus::Composer;
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("1 question(s) · 0 approval(s)"));
        let header = ui
            .hits
            .iter()
            .find_map(|(rect, action)| matches!(action, Action::Pending).then_some(*rect))
            .context("question header")?;
        let point = (header.x + header.width.min(6).saturating_sub(1), header.y);
        assert!(!ui.hits.iter().any(
            |(rect, action)| rect.contains(point.into()) && matches!(action, Action::Select(_))
        ));
        ui.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(ui.selected.as_deref(), Some("one"));
        assert!(
            matches!(&ui.modal, Some(Modal::Approval { pending, .. }) if pending.id == serde_json::json!("async-question"))
        );
        assert_eq!(
            ui.positions
                .get("one")
                .context("current draft")?
                .draft
                .text(),
            "Keep the current draft"
        );
        assert_eq!(
            ui.positions.get("two").context("other draft")?.draft.text(),
            "Other chat draft"
        );
        assert!(!ui.busy && ui.interrupting.is_empty() && ui.question_send.is_none());
        assert!(
            ui.positions
                .values()
                .all(|position| position.outgoing.is_empty())
        );
        assert!(ui.receiver.try_recv().is_err());
        assert_eq!(
            ui.sessions.get("one").context("session")?.status,
            Status::Idle
        );
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
            ]})
            .into(),
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
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
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
                .any(|line| line.contains("1 of 3") && line.contains("Alt+Up Questions"))
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
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
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
        still(&mut ui);
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
        assert_eq!(
            ui.positions.get("one").context("position")?.conversation,
            ui.conversation_lines.saturating_sub(ui.conversation_height),
            "closing questions must reclaim their space and bring the last message to the bottom"
        );
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
    fn question_send_ignores_unrelated_busy_work_and_keeps_failed_answers_retryable() -> Result<()>
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let (sent, received) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                sent.send(serde_json::from_str::<Value>(&line)?)?;
                serde_json::to_writer(&mut stream, &Reply::Error("Disconnected".into()))?;
                stream.write_all(b"\n")?;
            }
            Ok(())
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.sessions
            .get_mut("one")
            .context("session")?
            .pending
            .push(Pending {
                id: serde_json::json!("retry"),
                method: "item/tool/requestUserInput".into(),
                responded: true,
                params: serde_json::json!({"difuAsync":true,"questions":[
                    {"id":"q","question":"Choose scope","options":[{"label":"Small"}]}
                ]}),
            });
        ui.open_question(0);
        let mut requests = Vec::new();
        for busy in [true, false] {
            ui.busy = busy;
            ui.submit_question(false);
            assert!(ui.question_send.is_some());
            assert_eq!(ui.busy, busy);
            requests.push(received.recv_timeout(Duration::from_secs(5))?);
            let response = ui.receiver.recv_timeout(Duration::from_secs(5))?;
            ui.sender.send(response)?;
            ui.tick(false);
            assert!(ui.question_send.is_none());
            assert_eq!(ui.busy, busy);
            assert_eq!(ui.selected_question_answer().as_deref(), Some("Small"));
            assert!(
                ui.notice
                    .as_ref()
                    .is_some_and(|(text, error)| { *error && text.contains("Disconnected") })
            );
        }
        assert_eq!(requests.first(), requests.last());
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }
    #[test]
    fn questions_flip_in_as_they_open_and_flip_away_once_answered() -> Result<()> {
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
            .pending
            .push(Pending {
                id: serde_json::json!("flip"),
                method: "item/tool/requestUserInput".into(),
                responded: true,
                params: serde_json::json!({"difuAsync":true,"questions":[
                    {"id":"q","question":"Choose scope","options":[{"label":"Small"}]},
                    {"id":"q2","question":"Pick a colour","options":[{"label":"Blue"}]}
                ]}),
            });
        ui.open_question(0);
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(!screen.contains("Choose scope"), "a question flips in");
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(screen.contains("Choose scope"));
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(!screen.contains("Pick a colour") && !screen.contains("Choose scope"));
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(screen.contains("Pick a colour"));

        // An answer flips its question away, last character first, and keeps
        // it away until the answer is through.
        ui.submit_question(false);
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("Pick a colour") && screen.contains("Sending answer…"));
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(!screen.contains("Pick a colour") && screen.contains("Sending answer…"));
        // An answer that did not go through brings its question back.
        ui.question_send = None;
        draw(&mut ui, 120, 35)?;
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(screen.contains("Pick a colour"));

        // A key finishes the flip away at once.
        ui.submit_question(false);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(screen.contains("Pick a colour"));

        // The last answered question keeps the panel while it flips away.
        ui.question_send = None;
        ui.submit_question(false);
        ui.question_send = None;
        ui.modal = None;
        let (screen, _) = draw(&mut ui, 120, 35)?;
        assert!(screen.contains("Pick a colour"));
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(!screen.contains("Pick a colour"));
        Ok(())
    }
    #[test]
    fn information_modals_scroll_with_the_wheel_and_cmd_arrows() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = state(Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        });
        let wheel = |ui: &mut Ui| {
            ui.mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::NONE,
            });
        };
        let cmd_down = KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER);
        ui.modal = Some(Modal::Usage {
            id: "one".into(),
            provider: provider::Provider::Claude,
            data: None,
            error: None,
            scroll: 0,
            maximum: 30,
        });
        wheel(&mut ui);
        ui.key(cmd_down);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::Usage { scroll: 12, .. })));
        ui.modal = Some(Modal::Transcript {
            entry: "missing".into(),
            scroll: 0,
            rows: vec![Line::default(); 100],
            links: Vec::new(),
            layout: None,
            height: 10,
        });
        wheel(&mut ui);
        ui.key(cmd_down);
        assert!(matches!(
            ui.modal,
            Some(Modal::Transcript { scroll: 13, .. })
        ));
        ui.modal = Some(Modal::Status { scroll: 0 });
        wheel(&mut ui);
        ui.key(cmd_down);
        assert!(matches!(ui.modal, Some(Modal::Status { scroll: 13 })));
        // Drawing keeps the status within its text.
        draw(&mut ui, 120, 35)?;
        assert!(matches!(ui.modal, Some(Modal::Status { scroll: 0 })));
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
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
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
    fn multi_select_questions_check_several_options_and_send_every_label() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let listener = UnixListener::bind(super::super::server::socket(&storage)?)?;
        let worker = thread::spawn(move || -> Result<Request> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            serde_json::to_writer(&mut stream, &Reply::Ok)?;
            stream.write_all(b"\n")?;
            Ok(serde_json::from_str(&line)?)
        });
        let mut ui = state(storage);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        ui.sessions
            .get_mut("one")
            .context("session")?
            .pending
            .push(Pending {
                id: serde_json::json!("multi"),
                method: "item/tool/requestUserInput".into(),
                responded: false,
                params: serde_json::json!({"questions":[
                    {"id":"0","question":"Which checks?","multiSelect":true,
                     "options":[{"label":"Lint"},{"label":"Tests"},{"label":"Build"}]},
                    {"id":"1","question":"Ship it?","multiSelect":false,
                     "options":[{"label":"Yes"},{"label":"No"}]}
                ]}),
            });
        ui.open_question(0);
        assert_eq!(ui.selected_question_answer().as_deref(), Some("Lint"));
        let space = || KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        ui.key(space());
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.key(space());
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Lint, Build")
        );
        ui.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        ui.paste("CI only");
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Lint, Build\n\nNote on Build: CI only")
        );
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        for text in [
            "Choose one or more",
            "[x] 1. Lint",
            "[ ] 2. Tests",
            "[x] 3. Build",
            "Space choose",
        ] {
            assert!(screen.contains(text), "missing {text}");
        }
        // Unchecking drops the option and its note; a click checks like Space.
        ui.key(space());
        assert_eq!(ui.selected_question_answer().as_deref(), Some("Lint"));
        ui.key(space());
        ui.action(Action::QuestionChoice(1));
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let (screen, _) = draw_settled(&mut ui, 120, 35)?;
        assert!(screen.contains("Ship it?") && !screen.contains("[ ]"));
        assert_eq!(ui.selected_question_answer().as_deref(), Some("Yes"));
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        ui.paste("ocs");
        assert_eq!(
            ui.selected_question_answer().as_deref(),
            Some("Lint, Tests, Build, Docs\n\nNote on Build: CI only")
        );
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let request = worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        let Request::Control {
            control:
                Control::AnswerQuestion {
                    request,
                    question,
                    answer,
                },
            ..
        } = request
        else {
            anyhow::bail!("expected a question answer");
        };
        assert_eq!(request, serde_json::json!("multi"));
        assert_eq!(question, "0");
        assert_eq!(
            answer.as_deref(),
            Some("Lint, Tests, Build, Docs\n\nNote on Build: CI only")
        );
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
            data: serde_json::json!({"command":"git status","status":"completed"}).into(),
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
