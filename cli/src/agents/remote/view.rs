//! Display-ready session projections for the phone. Provider-specific entry
//! shapes are normalized here, reusing the terminal's labels, so the app only
//! renders. Tool output is shortened; `session.entry` returns a full row.
use super::super::{
    Entry, Job, Pending, Session, Summary,
    artifacts::Artifact,
    folders::{self, Folder},
    orchestrator,
    permissions::{self, CodexPreset},
    pr_cache::{SessionLink, SessionLinks},
    provider::Provider,
    subagents::{self, Subagent},
    ui::transcript,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

/// Tool output tail sent with a transcript; the full text is fetched on demand.
const TOOL_TEXT: usize = 6_000;
const MESSAGE_TEXT: usize = 200_000;
const DIFF_TEXT: usize = 20_000;
pub const FULL_TEXT: usize = 4_000_000;
/// A queued message as the queue shows it; `session.editQueued` compares it.
pub const QUEUED_TEXT: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub orchestrator: bool,
    pub status: super::super::Status,
    pub status_label: &'static str,
    pub active: bool,
    pub archived: bool,
    pub pinned: bool,
    pub updated: i64,
    pub turn_started_at: Option<i64>,
    pub repository: Option<String>,
    pub repository_path: Option<String>,
    pub branch: Option<String>,
    pub workspace: String,
    pub worktree: bool,
    pub pending: usize,
    pub queued: usize,
    pub can_read_changes: bool,
    /// Attached and discovered pull requests, current open ones first.
    pub prs: Vec<PrView>,
    /// Presented HTML artifacts (`session.artifact`).
    pub artifacts: usize,
    /// Open shells (`session.shells`).
    pub shells: usize,
    /// Sub-agents, and how many are still working (`session.subagent`).
    pub subagents: usize,
    pub running_subagents: usize,
    /// The sidebar folder holding the session (`folders.list`).
    pub folder: Option<String>,
    /// The `<server>:<session>` this session moved to; it takes no messages.
    pub moved_to: Option<String>,
}

/// A session's pull request, as the terminal's badges show it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrView {
    /// `owner/name`; `pr.overview` and `pr.diff` take it with `number`.
    pub repo: String,
    pub number: u64,
    pub title: Option<String>,
    /// `OPEN`, `MERGED` or `CLOSED`; empty until GitHub was asked.
    pub state: String,
    /// `Open`, `Draft`, `Has conflicts`, `Merged`, `Closed` or `Unknown`.
    pub label: &'static str,
    pub draft: bool,
    pub conflicts: bool,
    /// Attached explicitly (possibly from another repository) rather than
    /// discovered from the session's branch.
    pub attached: bool,
    pub url: String,
    pub head_branch: Option<String>,
}

impl PrView {
    pub fn new(link: &SessionLink) -> Self {
        let pr = &link.pr;
        Self {
            repo: pr.key.repository(),
            number: pr.key.number,
            title: pr
                .summary
                .as_ref()
                .map(|summary| summary.title.clone())
                .filter(|title| !title.is_empty()),
            state: pr.state.clone(),
            label: pr.label(),
            draft: pr.draft,
            conflicts: pr.conflicts,
            attached: link.attached,
            url: pr.key.url(),
            head_branch: (!pr.head_branch.is_empty()).then(|| pr.head_branch.clone()),
        }
    }
}

impl SessionSummary {
    pub fn new(summary: &Summary, pinned: &BTreeSet<String>, prs: &[SessionLink]) -> Self {
        Self {
            id: summary.id.clone(),
            title: summary.title.clone(),
            kind: summary.kind.clone(),
            orchestrator: summary.id == orchestrator::ID,
            status: summary.status,
            status_label: summary.status.label(),
            active: summary.status.active(),
            archived: summary.archived,
            pinned: pinned.contains(&summary.id),
            updated: summary.updated,
            turn_started_at: summary.turn_started_at,
            repository: summary.repository.as_deref().map(display_name),
            repository_path: summary.repository.as_ref().map(|p| p.display().to_string()),
            branch: summary.branch.clone(),
            workspace: summary.workspace.display().to_string(),
            worktree: summary.worktree,
            pending: summary.pending,
            queued: summary.queued,
            can_read_changes: summary.can_read_changes,
            prs: prs.iter().map(PrView::new).collect(),
            artifacts: summary.artifacts,
            shells: summary.shells,
            subagents: summary.subagents,
            running_subagents: summary.running_subagents,
            folder: None,
            moved_to: summary.moved_to.clone(),
        }
    }
}

/// Files each listed session under the sidebar folder holding it.
pub fn file_folders(sessions: &mut [SessionSummary], folders: &[Folder]) {
    for session in sessions {
        session.folder = folders::home(folders, &session.id).map(|folder| folder.id.clone());
    }
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The terminal's session list: orchestrator, pinned, custom order, newest.
/// Guide jobs and Slack orchestrator threads are hidden there too.
pub fn summaries(
    list: &[Summary],
    pinned: &BTreeSet<String>,
    order: &[String],
    prs: &SessionLinks,
) -> Vec<SessionSummary> {
    let positions: HashMap<_, _> = order.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let mut sessions: Vec<_> = list
        .iter()
        .filter(|s| s.kind != "Guide")
        .filter(|s| s.kind != "Orchestrator" || s.id == orchestrator::ID)
        .collect();
    sessions.sort_by(|a, b| {
        (a.id != orchestrator::ID)
            .cmp(&(b.id != orchestrator::ID))
            .then_with(|| pinned.contains(&b.id).cmp(&pinned.contains(&a.id)))
            .then_with(|| positions.get(&a.id).cmp(&positions.get(&b.id)))
            .then_with(|| b.id.cmp(&a.id))
    });
    sessions
        .into_iter()
        .map(|s| SessionSummary::new(s, pinned, prs.get(&s.id).map_or(&[], Vec::as_slice)))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Activity {
    pub label: String,
    pub detail: Option<String>,
    pub since: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Queued {
    pub index: usize,
    pub text: String,
    /// `text` was shortened; edit the message on the computer.
    pub truncated: bool,
}

/// A presented HTML artifact; `session.artifact` takes `path` and `workspace`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArtifactView {
    pub title: String,
    /// Relative to `workspace` (or the session's workspace when null).
    pub path: String,
    pub workspace: Option<String>,
    pub name: String,
}

impl ArtifactView {
    pub fn new(artifact: &Artifact) -> Self {
        Self {
            title: artifact.title.clone(),
            path: artifact.path.display().to_string(),
            workspace: artifact
                .workspace
                .as_ref()
                .map(|path| path.display().to_string()),
            name: artifact
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| artifact.path.display().to_string()),
        }
    }
}

/// A provider sub-agent, without its transcript (`session.subagent`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentView {
    pub id: String,
    pub name: String,
    pub task: String,
    /// The terminal's row title: name and the task's first line.
    pub title: String,
    /// Starting | Running | Completed | Interrupted | Failed | Closed | Unknown
    pub state: subagents::State,
    pub state_label: &'static str,
    pub active: bool,
    pub provider: super::super::provider::Provider,
    pub provider_label: &'static str,
    pub background: bool,
    /// Messages recorded so far (Codex loads them when opened).
    pub entries: usize,
}

impl SubagentView {
    pub fn new(agent: &Subagent) -> Self {
        Self {
            id: agent.id.clone(),
            name: clip(&agent.name, 200, false).0,
            task: clip(&agent.task, 2_000, false).0,
            title: clip(&agent.title(), 300, false).0,
            state: agent.state,
            state_label: agent.state.label(),
            active: agent.state.active(),
            provider: agent.provider,
            provider_label: agent.provider.label(),
            background: agent.background,
            entries: agent.entries.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub version: u64,
    pub provider: super::super::provider::Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub error: Option<String>,
    pub preparing: bool,
    pub can_send: bool,
    pub can_interrupt_and_send: bool,
    /// No turn, pending request or queued message: provider switches (and
    /// Claude Code model changes) are accepted now.
    pub between_turns: bool,
    pub activity: Option<Activity>,
    pub queue: Vec<Queued>,
    pub pending: Vec<PendingView>,
    /// The mode chip: the session's permission mode (and Codex approval
    /// preset); `None` outside coding sessions.
    pub permission_mode: Option<PermissionMode>,
    pub artifacts: Vec<ArtifactView>,
    pub subagents: Vec<SubagentView>,
}

/// A coding session's permission mode as the terminal's mode indicator shows
/// it: ids with their labels, and what waits for the next turn.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionMode {
    pub id: Option<String>,
    pub label: Option<String>,
    pub pending: Option<String>,
    pub pending_label: Option<String>,
    pub preset: Option<CodexPreset>,
    pub preset_label: Option<&'static str>,
    pub pending_preset: Option<CodexPreset>,
    pub pending_preset_label: Option<&'static str>,
}

impl PermissionMode {
    pub fn new(session: &Session) -> Option<Self> {
        if !matches!(session.job, Job::Coding(_)) {
            return None;
        }
        let state = &session.permission_state;
        let provider = session.provider;
        let id = state.mode.clone().or_else(|| {
            (provider == Provider::Claude)
                .then(|| session.permissions.get("permissionMode"))
                .flatten()
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let label = |id: &String| {
            permissions::mode_choices(provider)
                .into_iter()
                .find(|choice| &choice.id == id)
                .map_or_else(|| id.clone(), |choice| choice.label)
        };
        let codex = provider == Provider::Codex;
        let preset = codex
            .then(|| permissions::detected_preset(&session.permissions))
            .flatten();
        let pending_preset = state.pending_preset.filter(|_| codex);
        Some(Self {
            label: id.as_ref().map(label),
            id,
            pending_label: state.pending_mode.as_ref().map(label),
            pending: state.pending_mode.clone(),
            preset_label: preset.map(|preset| preset.label()),
            preset,
            pending_preset_label: pending_preset.map(|preset| preset.label()),
            pending_preset,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub header: Header,
    pub entries: Vec<EntryView>,
}

#[derive(Debug, Serialize)]
pub struct Delta<'a> {
    pub id: &'a str,
    pub base: u64,
    pub session: &'a Header,
    pub count: usize,
    pub entries: Vec<(usize, &'a EntryView)>,
}

/// Rows projected for the previous update. Unchanged entries share their
/// payloads with the last baseline, so comparing them is a pointer check.
#[derive(Default)]
pub struct Cache(HashMap<String, (Entry, bool, EntryView)>);

impl SessionView {
    #[cfg(test)]
    pub fn new(session: &Session, pinned: &BTreeSet<String>) -> Self {
        Self::cached(session, pinned, &[], &mut Cache::default())
    }

    pub fn cached(
        session: &Session,
        pinned: &BTreeSet<String>,
        prs: &[SessionLink],
        cache: &mut Cache,
    ) -> Self {
        let header = Header {
            summary: SessionSummary::new(&session.summary(), pinned, prs),
            version: session.version,
            provider: session.provider,
            model: session.model.clone(),
            effort: session.effort.clone(),
            error: session.error.clone(),
            preparing: session.preparing(),
            can_send: matches!(session.job, Job::Coding(_))
                && !session.archived
                && !session.workspace_removed,
            can_interrupt_and_send: session.can_send_waiting(),
            between_turns: super::super::provider::between_turns(session),
            activity: session.status.active().then(|| {
                let (label, detail, since) = transcript::activity_text(session);
                Activity {
                    label,
                    detail,
                    since,
                }
            }),
            queue: session
                .queue
                .iter()
                .enumerate()
                .map(|(index, prompt)| {
                    let (text, truncated) = clip(prompt.text(), QUEUED_TEXT, false);
                    Queued {
                        index,
                        text,
                        truncated,
                    }
                })
                .collect(),
            pending: session
                .pending
                .iter()
                .map(|p| PendingView::new(session, p))
                .collect(),
            permission_mode: PermissionMode::new(session),
            artifacts: session.artifacts.iter().map(ArtifactView::new).collect(),
            subagents: session.subagents.iter().map(SubagentView::new).collect(),
        };
        let active = session.status.active();
        let mut next = HashMap::new();
        let entries = session
            .entries
            .iter()
            .filter(|entry| transcript::visible(session, entry))
            .map(|entry| {
                let view = match cache.0.remove(&entry.id) {
                    Some((previous, was_active, view))
                        if previous == *entry && was_active == active =>
                    {
                        view
                    }
                    _ => EntryView::new(session, entry, false),
                };
                next.insert(entry.id.clone(), (entry.clone(), active, view.clone()));
                view
            })
            .collect();
        cache.0 = next;
        Self { header, entries }
    }

    /// Changes against the view last sent, so the phone receives only new rows.
    pub fn delta<'a>(&'a self, previous: &SessionView) -> Delta<'a> {
        Delta {
            id: &self.header.summary.id,
            base: previous.header.version,
            session: &self.header,
            count: self.entries.len(),
            entries: self
                .entries
                .iter()
                .enumerate()
                .filter(|(i, e)| previous.entries.get(*i) != Some(*e))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FileView {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub diff: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanStep {
    pub step: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    pub id: String,
    pub kind: String,
    pub role: &'static str,
    pub text: String,
    pub title: Option<String>,
    pub detail: Option<String>,
    pub state: Option<&'static str>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlanStep>,
    pub delivery: Option<&'static str>,
    pub truncated: bool,
}

impl EntryView {
    pub fn new(session: &Session, entry: &Entry, full: bool) -> Self {
        let limit = |normal: usize| if full { FULL_TEXT } else { normal };
        let mut view = Self {
            id: entry.id.clone(),
            kind: entry.kind.clone(),
            role: "system",
            text: String::new(),
            title: None,
            detail: None,
            state: None,
            started_at: entry.started_at,
            finished_at: entry.finished_at,
            files: Vec::new(),
            plan: Vec::new(),
            delivery: None,
            truncated: false,
        };
        let mut truncated = false;
        let mut text = |value: &str, max: usize, tail: bool| {
            let (text, cut) = clip(value, max, tail);
            truncated |= cut;
            text
        };
        match entry.kind.as_str() {
            "userMessage"
            | "sending"
            | "awaiting connection"
            | "unsent"
            | "unsent or unacknowledged" => {
                view.role = "user";
                view.text = text(&entry.text, limit(MESSAGE_TEXT), false);
                view.delivery = match entry.kind.as_str() {
                    "sending" => Some("sending"),
                    "awaiting connection" => Some("awaiting"),
                    "userMessage" => None,
                    _ => Some("unsent"),
                };
            }
            "agentMessage" | "result" => {
                view.role = "assistant";
                view.text = text(&entry.text, limit(MESSAGE_TEXT), false);
            }
            "reasoning" => {
                view.role = "reasoning";
                view.title = entry
                    .text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| crate::model::clean(line.trim().trim_matches('*').trim()));
                view.text = text(&entry.text, limit(MESSAGE_TEXT), false);
            }
            "plan" => {
                view.role = "plan";
                view.plan = entry
                    .data
                    .get("plan")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|step| PlanStep {
                        step: string(step, "step"),
                        status: match string(step, "status").as_str() {
                            "completed" => "completed",
                            "inProgress" | "in_progress" => "inProgress",
                            _ => "pending",
                        }
                        .into(),
                    })
                    .collect();
                view.text = text(&entry.text, limit(MESSAGE_TEXT), false);
            }
            "error" => {
                view.role = "error";
                view.text = text(&entry.text, limit(MESSAGE_TEXT), false);
            }
            "system" | "sending_context" => {
                view.text = text(&entry.text, limit(MESSAGE_TEXT), false);
            }
            _ => {
                view.role = "tool";
                let running = entry.started_at.is_some()
                    && entry.finished_at.is_none()
                    && session.status.active();
                view.title = Some(transcript::label(entry, running));
                view.state = Some(tool_state(entry, running));
                let (detail, output) = tool_body(entry);
                view.detail = detail.map(|d| text(&d, limit(4_000), false));
                view.text = text(&output, limit(TOOL_TEXT), true);
                view.files = tool_files(entry)
                    .into_iter()
                    .map(|mut file| {
                        file.diff = text(&file.diff, limit(DIFF_TEXT), false);
                        file
                    })
                    .collect();
            }
        }
        view.truncated = truncated;
        view
    }
}

fn tool_state(entry: &Entry, running: bool) -> &'static str {
    let review = entry.data.pointer("/review/status").and_then(Value::as_str);
    let status = string(&entry.data, "status");
    let failed = matches!(review, Some("denied" | "timedOut"))
        || matches!(status.as_str(), "failed" | "declined")
        || entry
            .data
            .get("exitCode")
            .and_then(Value::as_i64)
            .is_some_and(|n| n != 0);
    if failed {
        "failed"
    } else if running {
        "running"
    } else if status == "interrupted"
        || review == Some("aborted")
        || (entry.started_at.is_some() && entry.finished_at.is_none())
    {
        "interrupted"
    } else {
        "done"
    }
}

/// The tool's input (shown above its output) and its output text.
fn tool_body(entry: &Entry) -> (Option<String>, String) {
    let data = &*entry.data;
    match entry.kind.as_str() {
        "commandExecution" => {
            let command = string(data, "command");
            let output = entry
                .text
                .strip_prefix(&format!("$ {command}\n"))
                .unwrap_or(&entry.text)
                .to_owned();
            (Some(command), output)
        }
        "webSearch" => (Some(string(data, "query")), String::new()),
        "imageView" => (Some(string(data, "path")), String::new()),
        "fileChange" => (None, String::new()),
        "mcpToolCall" | "dynamicToolCall" => {
            let arguments = data.get("arguments").cloned().unwrap_or(Value::Null);
            let detail = [
                "command",
                "file_path",
                "path",
                "pattern",
                "query",
                "url",
                "prompt",
            ]
            .iter()
            .find_map(|key| arguments.get(key).and_then(Value::as_str))
            .map(str::to_owned)
            .or_else(|| {
                (!arguments.is_null())
                    .then(|| serde_json::to_string_pretty(&arguments).unwrap_or_default())
            });
            let output = match data.get("result") {
                Some(Value::String(text)) => text.clone(),
                Some(result) => result_text(result),
                None => String::new(),
            };
            (detail, output)
        }
        _ => (None, entry.text.to_string()),
    }
}

/// Claude and MCP results are content arrays; keep their text parts.
fn result_text(result: &Value) -> String {
    let parts = result
        .get("content")
        .and_then(Value::as_array)
        .or_else(|| result.as_array());
    match parts {
        Some(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        None => serde_json::to_string_pretty(result).unwrap_or_default(),
    }
}

fn tool_files(entry: &Entry) -> Vec<FileView> {
    let data = &*entry.data;
    if entry.kind == "fileChange" {
        return data
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|change| file(string(change, "path"), string(change, "diff")))
            .collect();
    }
    // Claude Code edits arrive as tool arguments; show them as small diffs.
    let arguments = data.get("arguments").cloned().unwrap_or(Value::Null);
    let path = string(&arguments, "file_path");
    match (entry.kind.as_str(), string(data, "tool").as_str()) {
        ("mcpToolCall", "Edit") if !path.is_empty() => vec![file(
            path,
            replacement(
                &string(&arguments, "old_string"),
                &string(&arguments, "new_string"),
            ),
        )],
        ("mcpToolCall", "MultiEdit") if !path.is_empty() => {
            let diff = arguments
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|edit| replacement(&string(edit, "old_string"), &string(edit, "new_string")))
                .collect::<Vec<_>>()
                .join("");
            vec![file(path, diff)]
        }
        ("mcpToolCall", "Write") if !path.is_empty() => {
            vec![file(path, replacement("", &string(&arguments, "content")))]
        }
        _ => Vec::new(),
    }
}

fn replacement(old: &str, new: &str) -> String {
    let mut diff = String::from("@@\n");
    for line in old.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in new.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn file(path: String, diff: String) -> FileView {
    let (additions, deletions) = counts(&diff);
    FileView {
        path,
        additions,
        deletions,
        diff,
    }
}

pub fn counts(patch: &str) -> (usize, usize) {
    patch.lines().fold((0, 0), |(a, d), line| {
        if line.starts_with('+') && !line.starts_with("+++") {
            (a + 1, d)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (a, d + 1)
        } else {
            (a, d)
        }
    })
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OptionView {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionView {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<OptionView>,
    pub multi_select: bool,
    pub secret: bool,
    pub answered: bool,
    /// `session.answer` takes a `note` for this question.
    pub accepts_note: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PendingView {
    pub request: Value,
    pub kind: &'static str,
    #[serde(rename = "async")]
    pub asynchronous: bool,
    pub title: String,
    pub detail: Option<String>,
    pub questions: Vec<QuestionView>,
    pub choices: Vec<Choice>,
}

pub const APPROVAL_CHOICES: [Choice; 4] = [
    Choice {
        id: "accept",
        label: "Allow once",
    },
    Choice {
        id: "acceptForSession",
        label: "Allow for session",
    },
    Choice {
        id: "decline",
        label: "Decline",
    },
    Choice {
        id: "cancel",
        label: "Cancel",
    },
];
pub const PERMISSION_CHOICES: [Choice; 2] = [
    Choice {
        id: "allow",
        label: "Allow for this turn",
    },
    Choice {
        id: "deny",
        label: "Deny",
    },
];

pub fn pending_kind(method: &str) -> &'static str {
    match method {
        "item/tool/requestUserInput" => "questions",
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => "approval",
        "item/permissions/requestApproval" => "permissions",
        _ => "unsupported",
    }
}

impl PendingView {
    pub fn new(session: &Session, pending: &Pending) -> Self {
        let params = &pending.params;
        let kind = pending_kind(&pending.method);
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .map(str::to_owned);
        let (title, detail, choices) = match kind {
            "questions" => (
                if params.get("difuElicitation").and_then(Value::as_bool) == Some(true) {
                    format!(
                        "{} needs permission",
                        params
                            .get("serverName")
                            .and_then(Value::as_str)
                            .unwrap_or("A tool")
                    )
                } else if pending.is_async_question() {
                    "The agent asked a question".into()
                } else {
                    "The agent needs your answer".into()
                },
                None,
                Vec::new(),
            ),
            "approval" if pending.method == "item/commandExecution/requestApproval" => {
                let command = match params.get("command") {
                    Some(Value::String(command)) => command.clone(),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => item_label(session, params),
                };
                (
                    "Allow command?".into(),
                    Some(join(&[Some(command), reason])),
                    APPROVAL_CHOICES.to_vec(),
                )
            }
            "approval" => (
                "Allow file changes?".into(),
                Some(join(&[Some(item_label(session, params)), reason])),
                APPROVAL_CHOICES.to_vec(),
            ),
            "permissions" => (
                "Allow requested permissions?".into(),
                Some(join(&[
                    params
                        .get("permissions")
                        .map(|p| serde_json::to_string_pretty(p).unwrap_or_default()),
                    reason,
                ])),
                PERMISSION_CHOICES.to_vec(),
            ),
            _ => (
                format!("Unsupported request · {}", pending.method),
                Some("Stop the request and ask the agent to use a question instead.".into()),
                vec![Choice {
                    id: "stop",
                    label: "Stop request",
                }],
            ),
        };
        let answers = params.get("difuAnswers").and_then(Value::as_object);
        let questions = params
            .get("questions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|q| {
                let id = string(q, "id");
                QuestionView {
                    answered: answers.is_some_and(|a| a.contains_key(&id)),
                    id,
                    header: string(q, "header"),
                    question: string(q, "question"),
                    options: q
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|o| OptionView {
                            label: o
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| string(o, "label")),
                            description: string(o, "description"),
                        })
                        .collect(),
                    multi_select: q.get("multiSelect").and_then(Value::as_bool) == Some(true),
                    secret: q.get("isSecret").and_then(Value::as_bool) == Some(true),
                    accepts_note: super::super::questions::accepts_note(pending, q),
                }
            })
            .collect();
        Self {
            request: pending.id.clone(),
            kind,
            asynchronous: pending.is_async_question(),
            title,
            detail: detail.filter(|d| !d.is_empty()),
            questions,
            choices,
        }
    }
}

/// The response the terminal sends for an approval or permission choice.
pub fn decision(pending: &Pending, choice: &str) -> anyhow::Result<Value> {
    match pending_kind(&pending.method) {
        "approval" => {
            anyhow::ensure!(
                APPROVAL_CHOICES.iter().any(|c| c.id == choice),
                "Unknown approval choice"
            );
            Ok(json!({"decision": choice}))
        }
        "permissions" => match choice {
            "allow" => Ok(json!({
                "permissions": pending.params.get("permissions").cloned().unwrap_or_else(|| json!({})),
                "scope": "turn",
            })),
            "deny" => Ok(json!({"permissions": {}, "scope": "turn"})),
            _ => anyhow::bail!("Unknown permission choice"),
        },
        _ => anyhow::bail!("Answer this request's questions instead"),
    }
}

fn item_label(session: &Session, params: &Value) -> String {
    let item = params.get("itemId").and_then(Value::as_str);
    session
        .entries
        .iter()
        .find(|e| Some(e.id.as_str()) == item)
        .map(|e| transcript::label(e, true))
        .unwrap_or_default()
}

fn join(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Shorten to about `max` bytes on a character boundary, keeping the start
/// or (for command output) the end.
pub fn clip(text: &str, max: usize, tail: bool) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    if tail {
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        (format!("…{}", text.get(start..).unwrap_or_default()), true)
    } else {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        (format!("{}…", text.get(..end).unwrap_or_default()), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Launch, Status};
    use anyhow::Context;

    fn session() -> Session {
        let mut session = Session::new(
            "1-a".into(),
            Job::Coding(Launch {
                repository: "/tmp/repo".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session
    }

    #[test]
    fn the_mode_chip_shows_the_mode_preset_and_what_waits() -> Result<(), anyhow::Error> {
        let mut claude = session();
        claude.provider = Provider::Claude;
        claude.permission_state.pending_mode = Some("auto".into());
        let starting = serde_json::to_value(SessionView::new(&claude, &BTreeSet::new()))?;
        assert_eq!(
            starting.get("permissionMode"),
            Some(
                &json!({"id": null, "label": null, "pending": "auto", "pendingLabel": "Auto",
                         "preset": null, "presetLabel": null,
                         "pendingPreset": null, "pendingPresetLabel": null})
            )
        );
        claude.permission_state.pending_mode = None;
        claude.permission_state.mode = Some("default".into());
        let manual = PermissionMode::new(&claude).context("chip")?;
        assert_eq!(manual.label.as_deref(), Some("Manual"));
        // Codex: its mode with the approval preset it runs with.
        let mut codex = session();
        codex.permission_state.mode = Some("default".into());
        codex.permissions = json!({"sandbox": {"type": "workspaceWrite"},
            "approvalPolicy": "on-request", "approvalsReviewer": "auto_review"});
        codex.permission_state.pending_preset = Some(CodexPreset::ReadOnly);
        let chip = PermissionMode::new(&codex).context("chip")?;
        assert_eq!(
            (chip.label.as_deref(), chip.preset, chip.preset_label),
            (
                Some("Regular"),
                Some(CodexPreset::Auto),
                Some("Approve for me")
            )
        );
        assert_eq!(chip.pending_preset_label, Some("Read Only"));
        let value = serde_json::to_value(&chip)?;
        assert_eq!(value.get("preset"), Some(&json!("Auto")));
        // Reviews and guides have no mode.
        let mut guide = session();
        guide.job = Job::Conflict {
            root: "/tmp/repo".into(),
            key: crate::model::PrKey {
                owner: "acme".into(),
                repo: "api".into(),
                number: 1,
            },
            head: "abc".into(),
            model: Default::default(),
        };
        assert_eq!(PermissionMode::new(&guide), None);
        Ok(())
    }

    #[test]
    fn tools_are_labelled_and_long_output_keeps_its_tail() {
        let mut session = session();
        session.entries.push(Entry {
            timestamp: None,
            id: "cmd".into(),
            kind: "commandExecution".into(),
            text: format!("$ cargo test\n{}done", "x".repeat(10_000)).into(),
            data: json!({"command": "cargo test", "exitCode": 1}).into(),
            started_at: Some(1),
            finished_at: Some(2),
        });
        let view = SessionView::new(&session, &BTreeSet::new());
        let entry = view.entries.first();
        assert_eq!(entry.map(|e| e.role), Some("tool"));
        assert_eq!(
            entry.and_then(|e| e.title.as_deref()),
            Some("Ran cargo test")
        );
        assert_eq!(entry.and_then(|e| e.state), Some("failed"));
        assert_eq!(entry.and_then(|e| e.detail.as_deref()), Some("cargo test"));
        assert!(entry.is_some_and(|e| e.truncated && e.text.ends_with("done")));
        let full = session
            .entries
            .first()
            .map(|e| EntryView::new(&session, e, true));
        assert!(full.is_some_and(|e| !e.truncated && e.text.starts_with("xxx")));
    }

    #[test]
    fn clipping_respects_character_boundaries() {
        assert_eq!(clip("héllo", 2, false), ("h…".into(), true));
        assert_eq!(clip("héllo", 4, true), ("…llo".into(), true));
        assert_eq!(clip("abc", 3, true), ("abc".into(), false));
    }

    #[test]
    fn cached_rows_are_reused_until_they_change() {
        let mut session = session();
        session.note("agentMessage", "first");
        let mut cache = Cache::default();
        let before = SessionView::cached(&session, &BTreeSet::new(), &[], &mut cache);
        if let Some(entry) = session.entries.get_mut(0) {
            entry.text.push_str(" edit");
        }
        let after = SessionView::cached(&session, &BTreeSet::new(), &[], &mut cache);
        assert_ne!(before.entries, after.entries);
        assert_eq!(
            after.entries.first().map(|e| e.text.as_str()),
            Some("first edit")
        );
    }

    #[test]
    fn deltas_carry_only_changed_rows() {
        let mut session = session();
        session.note("agentMessage", "first");
        session.note("agentMessage", "second");
        let before = SessionView::new(&session, &BTreeSet::new());
        if let Some(entry) = session.entries.get_mut(1) {
            entry.text.push_str(" more");
        }
        session.note("agentMessage", "third");
        let after = SessionView::new(&session, &BTreeSet::new());
        let delta = after.delta(&before);
        assert_eq!(delta.count, 3);
        assert_eq!(
            delta.entries.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(delta.base, before.header.version);
    }

    #[test]
    fn approvals_offer_terminal_choices_and_map_to_responses() -> anyhow::Result<()> {
        let mut session = session();
        session.pending.push(Pending {
            id: json!(3),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({"command": "rm -rf target", "reason": "Clean"}),
            responded: false,
        });
        let view = SessionView::new(&session, &BTreeSet::new());
        let pending = view.header.pending.first();
        assert_eq!(pending.map(|p| p.kind), Some("approval"));
        assert_eq!(
            pending.and_then(|p| p.detail.as_deref()),
            Some("rm -rf target\n\nClean")
        );
        let request = session.pending.first().context("missing request")?;
        assert_eq!(decision(request, "accept")?, json!({"decision": "accept"}));
        assert!(decision(request, "allow").is_err());
        Ok(())
    }

    #[test]
    fn questions_report_answers_and_options() {
        let mut session = session();
        session.pending.push(Pending {
            id: json!("difu-async:1"),
            method: "item/tool/requestUserInput".into(),
            params: json!({
                "difuAsync": true,
                "questions": [
                    {"id": "0", "header": "Q", "question": "Pick", "options": [{"label": "A", "description": "a"}], "multiSelect": true},
                    {"id": "1", "question": "Free"}
                ],
                "difuAnswers": {"0": {"answers": ["A"]}}
            }),
            responded: false,
        });
        let view = SessionView::new(&session, &BTreeSet::new());
        let pending = view.header.pending.first();
        assert!(pending.is_some_and(|p| p.asynchronous && p.kind == "questions"));
        let questions = pending.map(|p| p.questions.clone()).unwrap_or_default();
        assert!(
            questions
                .first()
                .is_some_and(|q| q.answered && q.multi_select)
        );
        assert!(
            questions
                .get(1)
                .is_some_and(|q| !q.answered && q.options.is_empty())
        );
    }

    #[test]
    fn questions_say_whether_answers_take_a_note() -> anyhow::Result<()> {
        let mut session = session();
        session.pending.push(Pending {
            id: json!("ask"),
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions": [
                {"id": "scope", "question": "Scope?", "options": [{"label": "Small"}]},
                {"id": "token", "question": "Token?", "isSecret": true}
            ]}),
            responded: false,
        });
        let mut form = Pending {
            id: json!("form"),
            method: "mcpServer/elicitation/request".into(),
            params: json!({"serverName": "Claude Code", "message": "Allow?",
                           "requestedSchema": {"type": "object", "properties": {"n": {"type": "integer"}}}}),
            responded: false,
        };
        super::super::super::elicitation::normalize(&mut form);
        session.pending.push(form);
        let view = SessionView::new(&session, &BTreeSet::new());
        let notes: Vec<Vec<(String, bool)>> = view
            .header
            .pending
            .iter()
            .map(|p| {
                p.questions
                    .iter()
                    .map(|q| (q.id.clone(), q.accepts_note))
                    .collect()
            })
            .collect();
        assert_eq!(
            notes,
            [
                vec![("scope".into(), true), ("token".into(), false)],
                vec![("difu-decision".into(), false), ("field:n".into(), false)],
            ]
        );
        let json = serde_json::to_value(view.header.pending.first())?;
        assert_eq!(json.pointer("/questions/0/acceptsNote"), Some(&json!(true)));
        Ok(())
    }

    #[test]
    fn summaries_and_views_carry_the_sessions_prs() -> anyhow::Result<()> {
        let session = session();
        let link = |number: u64, attached: bool, title: &str| SessionLink {
            workspace: "/tmp/repo".into(),
            workspace_revision: 0,
            attached,
            pr: crate::github::SessionPr {
                summary: Some(crate::model::PrSummary {
                    key: crate::model::PrKey {
                        owner: "opencx-labs".into(),
                        repo: "difu".into(),
                        number,
                    },
                    title: title.into(),
                    author: "octocat".into(),
                    updated: String::new(),
                    created: String::new(),
                    stats: None,
                    stats_error: false,
                    metadata: None,
                    draft: false,
                }),
                key: crate::model::PrKey {
                    owner: "opencx-labs".into(),
                    repo: "difu".into(),
                    number,
                },
                state: if attached { "MERGED" } else { "OPEN" }.into(),
                draft: false,
                conflicts: !attached,
                head_branch: if attached { "" } else { "feature" }.into(),
                head: String::new(),
                updated: String::new(),
            },
        };
        let links = vec![link(45, false, "Fix login redirect"), link(7, true, "")];
        let prs = SessionLinks::from([(session.id.clone(), links.clone())]);
        let mut other = session.clone();
        other.id = "2-b".into();
        let list = summaries(
            &[session.summary(), other.summary()],
            &BTreeSet::new(),
            &[],
            &prs,
        );
        let value = serde_json::to_value(&list)?;
        let rows = value.as_array().context("list")?;
        let with = rows
            .iter()
            .find(|row| row.get("id") == Some(&json!("1-a")))
            .context("session with PRs")?;
        assert_eq!(
            with.get("prs"),
            Some(&json!([
                {"repo": "opencx-labs/difu", "number": 45, "title": "Fix login redirect",
                 "state": "OPEN", "label": "Has conflicts", "draft": false, "conflicts": true,
                 "attached": false, "url": "https://github.com/opencx-labs/difu/pull/45",
                 "headBranch": "feature"},
                {"repo": "opencx-labs/difu", "number": 7, "title": null,
                 "state": "MERGED", "label": "Merged", "draft": false, "conflicts": false,
                 "attached": true, "url": "https://github.com/opencx-labs/difu/pull/7",
                 "headBranch": null}
            ]))
        );
        let without = rows
            .iter()
            .find(|row| row.get("id") == Some(&json!("2-b")))
            .context("session without PRs")?;
        assert_eq!(without.get("prs"), Some(&json!([])));
        // A watched session's view and its deltas carry them at the top level.
        let mut cache = Cache::default();
        let before = SessionView::cached(&session, &BTreeSet::new(), &[], &mut cache);
        let after = SessionView::cached(&session, &BTreeSet::new(), &links, &mut cache);
        let view = serde_json::to_value(&after)?;
        assert_eq!(view.pointer("/prs/0/number"), Some(&json!(45)));
        let delta = serde_json::to_value(after.delta(&before))?;
        assert_eq!(delta.pointer("/session/prs/1/attached"), Some(&json!(true)));
        Ok(())
    }

    #[test]
    fn list_order_matches_the_terminal() {
        let summary = |id: &str| {
            let mut session = session();
            session.id = id.into();
            session.summary()
        };
        let list = vec![
            summary("1-a"),
            summary("3-c"),
            summary("2-b"),
            summary("orchestrator"),
        ];
        let pinned = BTreeSet::from(["1-a".to_owned()]);
        let ids: Vec<_> = summaries(&list, &pinned, &[], &SessionLinks::new())
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["orchestrator", "1-a", "3-c", "2-b"]);
    }
}
