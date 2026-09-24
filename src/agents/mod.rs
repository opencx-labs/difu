//! Durable, local Codex sessions. Only sessions launched by difu are registered.
mod artifacts;
pub mod client;
mod engine;
mod guidance;
pub mod media;
mod questions;
pub mod server;
mod suggestions;
mod title;
pub mod ui;
mod workspace;

use crate::{
    codex::Guide,
    diff::Snapshot,
    model::{ModelChoice, PrDetail, PrKey},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub const LOCAL_VALIDATION_RULE: &str = "Do not run local tests, linting, typechecks, builds, CI scripts, or validation suites, including through wrappers or delegated agents. Rely on pull-request CI for project validation. Git inspection and git diff checks are allowed.";
pub const WORKTREE_INSTRUCTIONS: &str = "This is a difu-managed worktree. Do not run local tests, linting, typechecks, builds, CI scripts, or other validation suites, including through wrappers or delegated agents. Rely on pull-request CI for all project validation. Git inspection and git diff checks are allowed. Normal source editing tools remain available. Do not commit, push, open a PR, or otherwise publish work unless explicitly requested by the user's task. Finishing a task is not publication authorization.";
pub const CODING_INSTRUCTIONS: &str = "Do not commit, push, open a PR, or otherwise publish work unless explicitly requested by the user's task. Finishing a task is not publication authorization.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Launch {
    pub repository: PathBuf,
    pub isolated: bool,
    pub base: String,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Job {
    Coding(Launch),
    Guide {
        root: PathBuf,
        pr: Box<PrDetail>,
        snapshot: Box<Snapshot>,
        model: ModelChoice,
    },
    Conflict {
        root: PathBuf,
        key: PrKey,
        head: String,
        model: ModelChoice,
    },
}
impl Job {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Coding(_) => "Coding",
            Self::Guide { .. } => "Guide",
            Self::Conflict { .. } => "Conflicts",
        }
    }
    pub fn root(&self) -> &PathBuf {
        match self {
            Self::Coding(s) => &s.repository,
            Self::Guide { root, .. } | Self::Conflict { root, .. } => root,
        }
    }
    pub fn title(&self) -> String {
        match self {
            Self::Coding(s) => s
                .prompt
                .lines()
                .next()
                .unwrap_or("Coding session")
                .chars()
                .take(100)
                .collect(),
            Self::Guide { pr, .. } => format!("Guide · {}", pr.key.id()),
            Self::Conflict { key, .. } => format!("Resolve conflicts · {}", key.id()),
        }
    }
    fn identity(&self) -> Option<String> {
        match self {
            Self::Coding(_) => None,
            Self::Guide {
                pr,
                snapshot,
                model,
                ..
            } => crate::codex::cache_key(pr, snapshot, model).ok(),
            Self::Conflict { key, head, .. } => Some(format!("conflict:{}:{head}", key.id())),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    Starting,
    Running,
    Waiting,
    Idle,
    Completed,
    Interrupted,
    Failed,
}
impl Status {
    pub fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Waiting)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Running => "Working",
            Self::Waiting => "Needs input",
            Self::Idle => "Ready",
            Self::Completed => "Completed",
            Self::Interrupted => "Interrupted",
            Self::Failed => "Failed",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
}
impl Entry {
    pub fn is_tool(&self) -> bool {
        !matches!(
            self.kind.as_str(),
            "agentMessage"
                | "userMessage"
                | "sending"
                | "sending_context"
                | "awaiting connection"
                | "result"
                | "plan"
                | "error"
                | "system"
                | "unsent"
                | "unsent or unacknowledged"
                | "reasoning"
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub id: Value,
    pub method: String,
    pub params: Value,
    #[serde(default)]
    pub responded: bool,
}
impl Pending {
    pub fn unanswered_questions(&self) -> Vec<(usize, &Value)> {
        self.params
            .get("questions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, question)| {
                question
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| {
                        self.params
                            .get("difuAnswers")
                            .and_then(Value::as_object)
                            .is_none_or(|answers| !answers.contains_key(id))
                    })
            })
            .collect()
    }
    pub fn is_async_question(&self) -> bool {
        self.method == "item/tool/requestUserInput"
            && self.params.get("difuAsync").and_then(Value::as_bool) == Some(true)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub title_ready: bool,
    #[serde(default)]
    pub title_response: String,
    #[serde(default)]
    pub title_attempted: bool,
    #[serde(default)]
    pub title_manual: bool,
    #[serde(default)]
    pub suggestion: Option<suggestions::Suggestion>,
    #[serde(default)]
    pub suggestion_attempted: Option<String>,
    pub job: Job,
    pub status: Status,
    pub archived: bool,
    pub updated: i64,
    pub version: u64,
    pub workspace: Option<PathBuf>,
    #[serde(default)]
    pub workspace_ready: bool,
    #[serde(default)]
    pub deferred_workspace: bool,
    #[serde(default)]
    pub inherited_permissions: Value,
    #[serde(skip)]
    pub workspace_requests: Vec<Pending>,
    #[serde(skip)]
    pub switching_workspace: bool,
    #[serde(default)]
    pub guidance_checked: bool,
    #[serde(default)]
    pub workspace_removed: bool,
    pub baseline: Option<String>,
    pub branch: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    #[serde(default)]
    pub turn_started_at: Option<i64>,
    #[serde(default)]
    pub token_usage: Value,
    #[serde(default)]
    pub completed_turn: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permissions: Value,
    #[serde(default)]
    pub artifacts: Vec<artifacts::Artifact>,
    #[serde(default)]
    pub artifact_tools: bool,
    #[serde(skip)]
    pub artifact_requests: Vec<Pending>,
    #[serde(skip)]
    pub shells: Vec<Value>,
    pub entries: Vec<Entry>,
    pub pending: Vec<Pending>,
    #[serde(default)]
    pub answered_questions: std::collections::BTreeSet<String>,
    pub queue: Vec<Prompt>,
    pub error: Option<String>,
    pub result: Option<String>,
    pub guide: Option<Guide>,
}
impl Session {
    pub fn new(id: String, job: Job) -> Self {
        Self {
            title: job.title(),
            title_ready: false,
            title_response: String::new(),
            title_attempted: false,
            title_manual: false,
            suggestion: None,
            suggestion_attempted: None,
            id,
            job,
            status: Status::Starting,
            archived: false,
            updated: chrono::Utc::now().timestamp_millis(),
            version: 0,
            workspace: None,
            workspace_ready: false,
            deferred_workspace: false,
            inherited_permissions: Value::Null,
            workspace_requests: Vec::new(),
            switching_workspace: false,
            guidance_checked: false,
            workspace_removed: false,
            baseline: None,
            branch: None,
            thread_id: None,
            turn_id: None,
            turn_started_at: None,
            token_usage: Value::Null,
            completed_turn: None,
            model: None,
            effort: None,
            permissions: Value::Null,
            artifacts: Vec::new(),
            artifact_tools: false,
            artifact_requests: Vec::new(),
            shells: Vec::new(),
            entries: Vec::new(),
            pending: Vec::new(),
            answered_questions: Default::default(),
            queue: Vec::new(),
            error: None,
            result: None,
            guide: None,
        }
    }
    pub fn waiting_for_workspace(&self) -> bool {
        self.deferred_workspace
            && !self.workspace_ready
            && matches!(&self.job, Job::Coding(launch) if launch.isolated)
    }
    pub fn touch(&mut self) {
        self.version = self.version.saturating_add(1);
        self.updated = chrono::Utc::now().timestamp_millis();
    }
    pub fn note(&mut self, kind: &str, text: impl Into<String>) {
        self.entries.push(Entry {
            id: format!("difu-{}-{}", self.version, self.entries.len()),
            kind: kind.into(),
            text: text.into(),
            ..Entry::default()
        });
        self.touch();
    }
    pub fn unsent(&mut self, prompt: Prompt) {
        self.note("unsent", prompt.text());
        if let Some(entry) = self.entries.last_mut() {
            entry.data = serde_json::json!({"prompt":prompt});
        }
    }
    pub fn tool_running(&self) -> bool {
        self.turn_id.is_some()
            && self.entries.iter().any(|entry| {
                entry.is_tool() && entry.started_at.is_some() && entry.finished_at.is_none()
            })
    }
    pub fn is_pending_steering(&self, entry: &Entry) -> bool {
        matches!(entry.kind.as_str(), "userMessage" | "sending")
            && self.turn_id.as_deref().is_some_and(|turn| {
                entry.data.get("difuSteeringTurn").and_then(Value::as_str) == Some(turn)
            })
    }
    pub fn pending_steering(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| self.is_pending_steering(entry))
    }
    pub fn finish_steering_wait(&mut self) {
        if !self.tool_running() {
            for entry in &mut self.entries {
                if entry.kind == "userMessage"
                    && let Some(data) = entry.data.as_object_mut()
                {
                    data.remove("difuSteeringTurn");
                }
            }
        }
    }
    pub fn can_send_waiting(&self) -> bool {
        matches!(self.job, Job::Coding(_))
            && !self.archived
            && !self.pending.iter().any(|p| p.id == "difu-missing-guidance")
            && (!self.queue.is_empty() || self.pending_steering().next().is_some())
    }
    pub fn summary(&self) -> Summary {
        Summary {
            id: self.id.clone(),
            title: self.title.clone(),
            kind: self.job.kind().into(),
            status: self.status,
            archived: self.archived,
            updated: self.updated,
            version: self.version,
            workspace: self
                .workspace
                .clone()
                .unwrap_or_else(|| self.job.root().clone()),
            pending: self.pending_question_count()
                + self
                    .pending
                    .iter()
                    .filter(|p| p.method != "item/tool/requestUserInput")
                    .count(),
            queued: self.queue.len(),
            turn_started_at: self.turn_started_at,
            can_read_changes: self.workspace.is_some()
                && self.baseline.is_some()
                && !self.workspace_removed
                && matches!(self.job, Job::Coding(_)),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: Status,
    pub archived: bool,
    pub updated: i64,
    pub version: u64,
    #[serde(default)]
    pub turn_started_at: Option<i64>,
    pub workspace: PathBuf,
    #[serde(default)]
    pub can_read_changes: bool,
    pub pending: usize,
    pub queued: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Prompt {
    Text(String),
    WithSkills {
        text: String,
        skills: Vec<Skill>,
        #[serde(default)]
        attachments: Vec<media::Attachment>,
    },
}
impl Prompt {
    pub fn text(&self) -> &str {
        match self {
            Self::Text(s) | Self::WithSkills { text: s, .. } => s,
        }
    }
    pub fn attachments(&self) -> &[media::Attachment] {
        match self {
            Self::Text(_) => &[],
            Self::WithSkills { attachments, .. } => attachments,
        }
    }
    pub fn skills(&self) -> &[Skill] {
        match self {
            Self::Text(_) => &[],
            Self::WithSkills { skills, .. } => skills,
        }
    }
}
impl From<String> for Prompt {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for Prompt {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}
impl From<Prompt> for String {
    fn from(value: Prompt) -> Self {
        value.text().to_owned()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Control {
    Message {
        text: String,
        queue: bool,
        #[serde(default)]
        skills: Vec<Skill>,
        #[serde(default)]
        attachments: Vec<media::Attachment>,
    },
    MessageWithAttachments {
        text: String,
        queue: bool,
        skills: Vec<Skill>,
        attachments: Vec<media::Attachment>,
    },
    Interrupt,
    InterruptAndSend,
    Compact,
    RefreshShells,
    ReplaceQueued {
        index: usize,
        expected: Prompt,
        replacement: Option<Prompt>,
    },
    Resume,
    Respond {
        request: Value,
        response: Value,
    },
    AnswerQuestion {
        request: Value,
        question: String,
        answer: Option<String>,
    },
    Model {
        model: Option<String>,
        effort: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Ping,
    ServiceVersion,
    List,
    Read {
        id: String,
        version: Option<u64>,
    },
    Launch {
        job: Box<Job>,
    },
    NewAgent {
        defaults: crate::storage::AgentDefaults,
        cwd: PathBuf,
        remember_repository: bool,
    },
    Control {
        id: String,
        control: Control,
    },
    Rename {
        id: String,
        title: String,
    },
    Archive {
        id: String,
        archived: bool,
    },
    Cleanup {
        id: String,
    },
    Delete {
        id: String,
    },
    Changes {
        id: String,
    },
    Statistics {
        id: String,
    },
    Shells {
        id: String,
    },
    WorkspacePaths {
        id: String,
    },
    Defaults {
        cwd: PathBuf,
    },
    Skills {
        id: String,
        force: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Ok,
    ServiceVersion {
        version: String,
    },
    Shells(Vec<Value>),
    Sessions(Vec<Summary>),
    Session(Box<Session>),
    Unchanged,
    Launched(String),
    ChooseRepository,
    Changes(String),
    Statistics(DiffStatistics),
    WorkspacePaths(Vec<String>),
    Skills {
        skills: Vec<Skill>,
        errors: Vec<String>,
    },
    Defaults {
        model: String,
        effort: String,
        permissions: String,
    },
    Error(String),
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffStatistics {
    pub added: u64,
    pub removed: u64,
}
