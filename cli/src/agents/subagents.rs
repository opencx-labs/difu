//! Provider-owned child conversations. Opening one never starts or resumes work.
use super::{Entry, Session, provider::Provider};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    #[default]
    Starting,
    Running,
    Completed,
    Interrupted,
    Failed,
    Closed,
    Unknown,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Interrupted => "Interrupted",
            Self::Failed => "Failed",
            Self::Closed => "Closed",
            Self::Unknown => "Status unavailable",
        }
    }
    pub fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "pendingInit" | "pending" => Self::Starting,
            "running" | "inProgress" => Self::Running,
            "completed" => Self::Completed,
            "interrupted" | "stopped" | "killed" => Self::Interrupted,
            "errored" | "failed" => Self::Failed,
            "shutdown" => Self::Closed,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    pub provider: Provider,
    pub native_id: String,
    pub name: String,
    pub task: String,
    pub state: State,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub tool_ids: Vec<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub entries: Vec<Entry>,
}
impl Subagent {
    pub fn new(provider: Provider, native_id: &str) -> Self {
        Self {
            id: format!("{}:{native_id}", provider.key()),
            provider,
            native_id: native_id.into(),
            name: "Sub-agent".into(),
            task: String::new(),
            state: State::Starting,
            parent: None,
            tool_ids: Vec::new(),
            run_id: None,
            background: false,
            entries: Vec::new(),
        }
    }
    pub fn title(&self) -> String {
        if self.task.is_empty() {
            self.name.clone()
        } else {
            format!(
                "{} · {}",
                self.name,
                self.task.lines().next().unwrap_or_default()
            )
        }
    }
    pub fn finish(&mut self, state: State) {
        self.state = state;
        if !state.active() {
            let now = chrono::Utc::now().timestamp_millis();
            for entry in &mut self.entries {
                if entry.finished_at.is_none() {
                    entry.finished_at = Some(now);
                }
            }
        }
    }
}

fn string<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Recover launch records saved by versions that had no separate child list.
pub(super) fn restore(session: &mut Session) {
    if session.subagents.is_empty() {
        let launches: Vec<_> = session
            .entries
            .iter()
            .filter(|e| {
                e.kind == "collabAgentToolCall"
                    || (e.data.get("server").and_then(Value::as_str) == Some("Claude Code")
                        && matches!(
                            e.data.get("tool").and_then(Value::as_str),
                            Some("Agent" | "Task")
                        ))
            })
            .cloned()
            .collect();
        for entry in launches {
            if entry.kind == "collabAgentToolCall" {
                codex_collab(session, &entry.data);
            } else if let Some(tool) = entry.id.strip_prefix("claude-tool-") {
                claude_launch(
                    session,
                    &json!({"type":"tool_use", "id":tool,
                    "name":entry.data.get("tool"), "input":entry.data.get("arguments")}),
                    None,
                );
                let child = claude_child(session, None, Some(tool));
                child.finish(if child.background {
                    State::Unknown
                } else {
                    State::parse(
                        entry
                            .data
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                });
            }
        }
    }
    // v0.32.0 retained collaboration activity in the parent transcript without
    // registering its children. Recover missing IDs, even in a mixed session,
    // while preserving newer state already stored in the separate child list.
    let known: std::collections::HashSet<_> = session
        .subagents
        .iter()
        .filter(|a| a.provider == Provider::Codex)
        .map(|a| a.native_id.clone())
        .collect();
    let activity: Vec<_> = session
        .entries
        .iter()
        .filter(|e| {
            e.kind == "subAgentActivity"
                && string(&e.data, "agentThreadId").is_some_and(|id| !known.contains(id))
        })
        .map(|e| e.data.clone())
        .collect();
    let parent = session.thread_id.clone();
    for item in activity {
        codex_activity(session, parent.as_deref(), &item);
    }
    for child in &mut session.subagents {
        if child.state.active() {
            child.finish(State::Interrupted);
        }
    }
}

fn path_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// Collaboration tools report a lifecycle item on the owning thread. The
/// item's completion only finishes the announcement, not the child task.
fn codex_activity(session: &mut Session, parent: Option<&str>, item: &Value) {
    if string(item, "type") != Some("subAgentActivity")
        || !parent.is_some_and(|id| {
            session.thread_id.as_deref() == Some(id)
                || session
                    .subagents
                    .iter()
                    .any(|a| a.provider == Provider::Codex && a.native_id == id)
        })
    {
        return;
    }
    let Some(id) = string(item, "agentThreadId") else {
        return;
    };
    if session.thread_id.as_deref() == Some(id) {
        return;
    }
    let child = codex_child(session, id);
    child.parent = parent.map(String::from);
    if let Some(path) = string(item, "agentPath") {
        child.name = path_name(path).into();
    }
    match string(item, "kind") {
        Some("started" | "interacted") => child.state = State::Running,
        Some("interrupted") => child.finish(State::Interrupted),
        Some("completed") => child.finish(State::Completed),
        _ => {}
    }
}

fn codex_child<'a>(session: &'a mut Session, id: &str) -> &'a mut Subagent {
    let index = session
        .subagents
        .iter()
        .position(|a| a.provider == Provider::Codex && a.native_id == id)
        .unwrap_or_else(|| {
            session.subagents.push(Subagent::new(Provider::Codex, id));
            session.subagents.len() - 1
        });
    // The index either came from position() or the element just appended.
    session
        .subagents
        .get_mut(index)
        .unwrap_or_else(|| unreachable!())
}

pub(super) fn codex_collab(session: &mut Session, item: &Value) {
    if string(item, "type") != Some("collabAgentToolCall") {
        return;
    }
    let sender = string(item, "senderThreadId");
    if !sender.is_some_and(|id| {
        session.thread_id.as_deref() == Some(id)
            || session
                .subagents
                .iter()
                .any(|a| a.provider == Provider::Codex && a.native_id == id)
    }) {
        return;
    }
    let spawn = string(item, "tool") == Some("spawnAgent");
    let ids = item
        .get("receiverThreadIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    for id in ids {
        if session.thread_id.as_deref() == Some(id) {
            continue;
        }
        let known = session
            .subagents
            .iter()
            .any(|a| a.provider == Provider::Codex && a.native_id == id);
        if !spawn && !known {
            continue;
        }
        let child = codex_child(session, id);
        if spawn {
            child.parent = string(item, "senderThreadId").map(String::from);
            if let Some(prompt) = string(item, "prompt") {
                child.task = prompt.into();
            }
        }
        if let Some(state) = item
            .get("agentsStates")
            .and_then(|s| s.get(id))
            .and_then(|s| string(s, "status"))
        {
            child.finish(State::parse(state));
        }
    }
}

pub(super) fn codex_metadata(session: &mut Session, thread: &Value) -> Option<String> {
    let id = string(thread, "id")?;
    let parent = string(thread, "parentThreadId").or_else(|| {
        thread
            .pointer("/source/subAgent/thread_spawn/parent_thread_id")
            .and_then(Value::as_str)
    });
    let known = session
        .subagents
        .iter()
        .any(|a| a.provider == Provider::Codex && a.native_id == id);
    let ours = parent.is_some_and(|p| {
        session.thread_id.as_deref() == Some(p)
            || session
                .subagents
                .iter()
                .any(|a| a.provider == Provider::Codex && a.native_id == p)
    });
    if !known && !ours {
        return None;
    }
    let child = codex_child(session, id);
    if let Some(parent) = parent {
        child.parent = Some(parent.into());
    }
    if let Some(name) = thread
        .pointer("/source/subAgent/thread_spawn/agent_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(path_name)
        .or_else(|| string(thread, "agentNickname"))
        .or_else(|| string(thread, "name"))
        .or_else(|| string(thread, "agentRole"))
    {
        child.name = name.into();
    }
    if child.task.is_empty() {
        child.task = string(thread, "preview").unwrap_or_default().into();
    }
    match thread.pointer("/status/type").and_then(Value::as_str) {
        Some("active") => child.state = State::Running,
        Some("systemError") => child.finish(State::Failed),
        _ => {} // notLoaded/idle alone do not prove an agent finished successfully.
    }
    Some(id.into())
}

/// Child notifications must never change the parent's turn, queue or approvals.
pub(super) fn codex_event(session: &mut Session, event: &Value) -> bool {
    let method = string(event, "method").unwrap_or_default();
    let params = event.get("params").unwrap_or(&Value::Null);
    if method == "thread/started" {
        codex_metadata(session, params.get("thread").unwrap_or(&Value::Null));
        return true;
    }
    if let Some(item) = params.get("item") {
        codex_collab(session, item);
        let parent = string(params, "threadId")
            .map(String::from)
            .or_else(|| session.thread_id.clone());
        codex_activity(session, parent.as_deref(), item);
    }
    let Some(id) = string(params, "threadId") else {
        return false;
    };
    if session.thread_id.as_deref() == Some(id) {
        return false;
    }
    let Some(child) = session
        .subagents
        .iter_mut()
        .find(|a| a.provider == Provider::Codex && a.native_id == id)
    else {
        return session.thread_id.is_some();
    };
    codex_transcript(child, method, params);
    true
}

fn codex_transcript(child: &mut Subagent, method: &str, params: &Value) {
    let now = chrono::Utc::now().timestamp_millis();
    match method {
        "item/started" | "item/completed" => {
            let Some(item) = params.get("item") else {
                return;
            };
            let Some(id) = string(item, "id") else {
                return;
            };
            let completed = method == "item/completed";
            let existing = child.entries.iter().position(|e| e.id == id);
            let entry = Entry {
                id: id.into(),
                kind: string(item, "type").unwrap_or("system").into(),
                text: super::engine::item_text(item).into(),
                data: item.clone().into(),
                started_at: Some(now),
                finished_at: completed.then_some(now),
                timestamp: Some(now),
            };
            if let Some(existing) = existing.and_then(|i| child.entries.get_mut(i)) {
                let start = existing.started_at;
                let timestamp = existing.timestamp;
                *existing = entry;
                existing.started_at = start;
                existing.timestamp = timestamp;
            } else {
                child.entries.push(entry);
            }
        }
        "item/agentMessage/delta"
        | "item/plan/delta"
        | "item/commandExecution/outputDelta"
        | "item/reasoning/summaryTextDelta" => {
            if let Some(entry) = child
                .entries
                .iter_mut()
                .find(|e| Some(e.id.as_str()) == string(params, "itemId"))
            {
                entry
                    .text
                    .push_str(string(params, "delta").unwrap_or_default());
            }
        }
        "turn/started" => child.state = State::Running,
        "turn/completed" => child.finish(State::parse(
            params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "thread/status/changed" => match params.pointer("/status/type").and_then(Value::as_str) {
            Some("active") => child.state = State::Running,
            Some("systemError") => child.finish(State::Failed),
            _ => {}
        },
        _ => {}
    }
}

pub(super) fn codex_history(session: &mut Session, id: &str, thread: &Value) {
    codex_metadata(session, thread);
    if let Some(turns) = thread.get("turns").and_then(Value::as_array) {
        let mut order = Vec::new();
        for turn in turns {
            for item in turn
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(id) = string(item, "id") {
                    order.push(id.to_owned());
                }
                codex_collab(session, item);
                codex_activity(session, Some(id), item);
                let child = codex_child(session, id);
                let ongoing = string(turn, "status") == Some("inProgress")
                    && string(item, "status") == Some("inProgress");
                codex_transcript(
                    child,
                    if ongoing {
                        "item/started"
                    } else {
                        "item/completed"
                    },
                    &json!({"item":item}),
                );
            }
        }
        let child = codex_child(session, id);
        // History may include earlier messages whose live events were not sent
        // to the parent's connection. Insert them at their source position.
        // Keep newer live items beyond the snapshot at the end.
        let live = std::mem::take(&mut child.entries);
        let mut entries: std::collections::HashMap<_, _> = live
            .iter()
            .map(|entry| (entry.id.clone(), entry.clone()))
            .collect();
        child.entries = order
            .into_iter()
            .chain(live.into_iter().map(|e| e.id))
            .filter_map(|id| entries.remove(&id))
            .collect();
        if let Some(turn) = turns.last() {
            codex_child(session, id)
                .finish(State::parse(string(turn, "status").unwrap_or_default()));
        }
    }
}

fn claude_child<'a>(
    session: &'a mut Session,
    native: Option<&str>,
    tool: Option<&str>,
) -> &'a mut Subagent {
    let matches: Vec<_> = session
        .subagents
        .iter()
        .enumerate()
        .filter_map(|(index, a)| {
            (a.provider == Provider::Claude
                && (native.is_some_and(|id| a.native_id == id)
                    || tool.is_some_and(|id| a.tool_ids.iter().any(|t| t == id))))
            .then_some(index)
        })
        .collect();
    let index = matches.first().copied().unwrap_or_else(|| {
        session.subagents.push(Subagent::new(
            Provider::Claude,
            native.or(tool).unwrap_or("unknown"),
        ));
        session.subagents.len() - 1
    });
    // Task notifications and tool frames can introduce separate placeholders
    // before a later event supplies both IDs. Retain the first UI identity.
    for other in matches.into_iter().skip(1).rev() {
        let duplicate = session.subagents.remove(other);
        if let Some(child) = session.subagents.get_mut(index) {
            for entry in duplicate.entries {
                if !child.entries.iter().any(|e| e.id == entry.id) {
                    child.entries.push(entry);
                }
            }
            for tool in duplicate.tool_ids {
                if !child.tool_ids.contains(&tool) {
                    child.tool_ids.push(tool);
                }
            }
            if child.task.is_empty() {
                child.task = duplicate.task;
            }
            if child.name == "Sub-agent" {
                child.name = duplicate.name;
            }
            if child.parent.is_none() {
                child.parent = duplicate.parent;
            }
            child.background |= duplicate.background;
            if duplicate.run_id > child.run_id {
                child.run_id = duplicate.run_id;
                child.state = duplicate.state;
            }
        }
    }
    let child = session
        .subagents
        .get_mut(index)
        .unwrap_or_else(|| unreachable!());
    if let Some(native) = native {
        child.native_id = native.into();
    }
    if let Some(tool) = tool
        && !child.tool_ids.iter().any(|id| id == tool)
    {
        child.tool_ids.push(tool.into());
    }
    child
}

fn claude_launch(session: &mut Session, block: &Value, parent: Option<&str>) {
    if string(block, "type") != Some("tool_use")
        || !matches!(string(block, "name"), Some("Agent" | "Task"))
    {
        return;
    }
    let Some(tool) = string(block, "id") else {
        return;
    };
    let input = block.get("input").unwrap_or(&Value::Null);
    let seen = session
        .subagents
        .iter()
        .any(|a| a.provider == Provider::Claude && a.tool_ids.iter().any(|id| id == tool));
    let child = claude_child(session, string(input, "resume"), Some(tool));
    child.parent = parent.map(String::from);
    if let Some(name) = string(input, "name").or_else(|| string(input, "subagent_type")) {
        child.name = name.into();
    }
    if let Some(task) = string(input, "description").or_else(|| string(input, "prompt")) {
        child.task = task.into();
    }
    child.background |= input.get("run_in_background").and_then(Value::as_bool) == Some(true);
    if !seen {
        child.state = State::Running;
    }
    if let Some(prompt) = string(input, "prompt") {
        let id = format!("claude-task-{tool}");
        if !seen && !child.entries.iter().any(|e| e.id == id) {
            child.entries.push(Entry {
                id,
                kind: "userMessage".into(),
                text: prompt.to_owned().into(),
                ..Entry::default()
            });
        }
    }
}

/// Consume child messages before the main decoder sees them. Complete Claude
/// messages carry attribution; partial events currently belong only to the root.
pub(super) fn claude_event(session: &mut Session, frame: &Value) -> bool {
    let parent = string(frame, "parent_tool_use_id");
    let native = string(frame, "agent_id");
    let kind = string(frame, "type").unwrap_or_default();
    if kind == "system" && string(frame, "subtype").is_some_and(|s| s.starts_with("task_")) {
        let tool = string(frame, "tool_use_id");
        let task = string(frame, "task_id");
        let known = session.subagents.iter().any(|a| {
            a.provider == Provider::Claude
                && (task.is_some_and(|t| a.native_id == t)
                    || tool.is_some_and(|t| a.tool_ids.iter().any(|id| id == t)))
        });
        if frame.get("ambient").and_then(Value::as_bool) == Some(true)
            || (!known
                && !matches!(
                    string(frame, "task_type"),
                    Some("local_agent" | "remote_agent")
                ))
        {
            return true;
        }
        let child = claude_child(session, task, tool);
        let run = string(frame, "run_id");
        if let (Some(current), Some(run)) = (child.run_id.as_deref(), run)
            && run < current
        {
            return true;
        }
        if let Some(run) = run {
            child.run_id = Some(run.into());
        }
        if let Some(description) = string(frame, "description") {
            child.task = description.into();
        }
        if let Some(parent) = string(frame, "parent_task_id") {
            child.parent = Some(parent.into());
        }
        if let Some(background) = frame.get("is_backgrounded").and_then(Value::as_bool) {
            child.background = background;
        }
        match string(frame, "subtype") {
            Some("task_started") => child.state = State::Running,
            Some("task_notification") => {
                child.finish(State::parse(string(frame, "status").unwrap_or_default()))
            }
            Some("task_updated") => {
                let patch = frame.get("patch").unwrap_or(&Value::Null);
                if let Some(state) = string(patch, "status") {
                    child.finish(State::parse(state));
                }
                if let Some(description) = string(patch, "description") {
                    child.task = description.into();
                }
                if let Some(background) = patch.get("is_backgrounded").and_then(Value::as_bool) {
                    child.background = background;
                }
            }
            _ => {}
        }
        if let Some(summary) = string(frame, "summary") {
            let id = format!("claude-task-summary-{}", child.native_id);
            if let Some(entry) = child.entries.iter_mut().find(|e| e.id == id) {
                entry.text = summary.to_owned().into();
            } else {
                child.entries.push(Entry {
                    id,
                    kind: "system".into(),
                    text: summary.to_owned().into(),
                    ..Entry::default()
                });
            }
        }
        return true;
    }
    if kind == "stream_event" && parent.is_none() {
        if let Some(block) = frame.pointer("/event/content_block") {
            claude_launch(session, block, None);
        }
        return false;
    }
    let blocks = frame.pointer("/message/content").and_then(Value::as_array);
    if let Some(blocks) = blocks {
        for block in blocks {
            claude_launch(session, block, native.or(parent));
        }
        if kind == "user" {
            for block in blocks {
                if string(block, "type") != Some("tool_result") {
                    continue;
                }
                let Some(tool) = string(block, "tool_use_id") else {
                    continue;
                };
                if session
                    .subagents
                    .iter()
                    .any(|a| a.provider == Provider::Claude && a.tool_ids.iter().any(|t| t == tool))
                {
                    let native = frame
                        .pointer("/tool_use_result/agentId")
                        .and_then(Value::as_str);
                    let child = claude_child(session, native, Some(tool));
                    if frame
                        .pointer("/tool_use_result/isAsync")
                        .and_then(Value::as_bool)
                        == Some(true)
                    {
                        child.background = true;
                    }
                    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                        child.finish(State::Failed);
                    } else if !child.background {
                        child.finish(State::Completed);
                    }
                }
            }
        }
    }
    if parent.is_none() && native.is_none() {
        return false;
    }
    let child = claude_child(session, native, parent);
    let message_id = frame
        .pointer("/message/id")
        .and_then(Value::as_str)
        .or_else(|| string(frame, "uuid"))
        .unwrap_or_default();
    if kind == "assistant" {
        super::claude::events::complete_message(&mut child.entries, frame);
        return true;
    }
    if let Some(blocks) = blocks {
        for (index, block) in blocks.iter().enumerate() {
            if kind == "user" && string(block, "type") == Some("tool_result") {
                let id = format!(
                    "claude-tool-{}",
                    string(block, "tool_use_id").unwrap_or_default()
                );
                if let Some(entry) = child.entries.iter_mut().find(|e| e.id == id) {
                    entry.finished_at = Some(chrono::Utc::now().timestamp_millis());
                    let status = if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                        "failed"
                    } else {
                        "completed"
                    };
                    if let Some(data) = entry.data.as_object_mut() {
                        data.insert("status".into(), json!(status));
                    }
                    // Full frames can be replayed after reconnect; rebuild, don't append.
                    entry.text = format!(
                        "{}\n{}",
                        entry.data.get("arguments").unwrap_or(&Value::Null),
                        super::claude::events::text(block.get("content").unwrap_or(&Value::Null))
                    )
                    .into();
                }
            } else if kind == "user" && string(block, "type") == Some("text") {
                let id = format!("claude-user-{message_id}-{index}");
                let text = string(block, "text").unwrap_or_default();
                if let Some(entry) = child.entries.iter_mut().find(|e| {
                    parent.is_some_and(|p| e.id == format!("claude-task-{p}"))
                        && e.text.as_str() == text
                }) {
                    entry.id = id.clone();
                }
                if !child.entries.iter().any(|e| e.id == id) {
                    child.entries.push(Entry {
                        id,
                        kind: "userMessage".into(),
                        text: string(block, "text").unwrap_or_default().to_owned().into(),
                        ..Entry::default()
                    });
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut s = Session::new(
            "parent".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: "/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        s.thread_id = Some("parent-thread".into());
        s.turn_id = Some("parent-turn".into());
        s.status = super::super::Status::Running;
        s
    }
    fn activity(id: &str, path: &str, kind: &str) -> Value {
        json!({"method":"item/completed","params":{"threadId":"parent-thread","item":{
            "type":"subAgentActivity","id":format!("{id}-{kind}"),"kind":kind,
            "agentThreadId":id,"agentPath":path,"model":null,"reasoningEffort":null
        }}})
    }
    #[test]
    fn subagent_activity_tracks_lifecycle_without_finishing_the_parent() {
        let mut s = session();
        let started = activity("badge", "/root/badge_check", "started");
        super::super::engine::apply_event(&mut s, &started);
        super::super::engine::apply_event(&mut s, &started);
        assert_eq!(s.subagents.len(), 1);
        assert_eq!(s.subagents.first().map(|a| a.state), Some(State::Running));
        for (kind, expected) in [
            ("completed", State::Completed),
            ("interacted", State::Running),
            ("interrupted", State::Interrupted),
        ] {
            super::super::engine::apply_event(
                &mut s,
                &activity("badge", "/root/badge_check", kind),
            );
            assert_eq!(s.subagents.first().map(|a| a.state), Some(expected));
            assert_eq!(s.status, super::super::Status::Running);
            assert_eq!(s.turn_id.as_deref(), Some("parent-turn"));
        }
        let mut foreign = activity("foreign", "/root/foreign", "started");
        foreign["params"]["threadId"] = json!("unrelated-thread");
        super::super::engine::apply_event(&mut s, &foreign);
        assert_eq!(s.subagents.len(), 1);
        let mut nested = activity("nested", "/root/badge_check/nested", "started");
        nested["params"]["threadId"] = json!("badge");
        super::super::engine::apply_event(&mut s, &nested);
        assert_eq!(s.subagents.len(), 2);
        assert_eq!(
            s.subagents.last().and_then(|a| a.parent.as_deref()),
            Some("badge")
        );
    }
    #[test]
    fn subagent_activity_recovers_both_saved_launches_in_a_mixed_session() -> anyhow::Result<()> {
        let mut s = session();
        for (id, path) in [
            ("badge", "/root/badge_check"),
            ("transcript", "/root/transcript_check"),
        ] {
            for kind in ["started", "completed"] {
                super::super::engine::apply_event(&mut s, &activity(id, path, kind));
            }
        }
        // v0.32.0 saved these items but did not add them to its child list.
        s.subagents.clear();
        let known = codex_child(&mut s, "existing");
        known.finish(State::Closed);
        s.entries.push(Entry {
            id: "old-existing-start".into(),
            kind: "subAgentActivity".into(),
            data: activity("existing", "/root/existing", "started")["params"]["item"]
                .clone()
                .into(),
            ..Entry::default()
        });
        let mut restored: Session = serde_json::from_value(serde_json::to_value(s)?)?;
        restore(&mut restored);
        assert_eq!(restored.subagents.len(), 3);
        let recovered: Vec<_> = restored
            .subagents
            .iter()
            .map(|a| (a.name.as_str(), a.state))
            .collect();
        assert_eq!(
            recovered,
            [
                ("Sub-agent", State::Closed),
                ("badge_check", State::Completed),
                ("transcript_check", State::Completed)
            ]
        );
        let previous = serde_json::to_value(&restored.subagents)?;
        restore(&mut restored);
        assert_eq!(previous, serde_json::to_value(&restored.subagents)?);
        assert_eq!(restored.entries.len(), 5);
        Ok(())
    }
    #[test]
    fn subagents_history_orders_earlier_items_before_live_output() {
        let mut s = session();
        let child = codex_child(&mut s, "child");
        codex_transcript(
            child,
            "item/started",
            &json!({"item":{"id":"reply","type":"agentMessage","text":"Partial"}}),
        );
        codex_transcript(
            child,
            "item/started",
            &json!({"item":{"id":"newer","type":"agentMessage","text":"New live message"}}),
        );
        codex_history(
            &mut s,
            "child",
            &json!({"id":"child","turns":[{"status":"inProgress","items":[
                {"id":"prompt","type":"userMessage","content":[{"type":"text","text":"Check schema"}]},
                {"id":"reply","type":"agentMessage","text":"Full report"}
            ]}]}),
        );
        let entries: Vec<_> = s
            .subagents
            .iter()
            .flat_map(|a| &a.entries)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(entries, ["prompt", "reply", "newer"]);
    }
    #[test]
    fn subagents_restore_legacy_launches_without_replaying_work() -> anyhow::Result<()> {
        let mut s = session();
        let data = json!({"type":"collabAgentToolCall","tool":"spawnAgent","senderThreadId":"parent-thread","receiverThreadIds":["old-child"],"prompt":"Review schema","agentsStates":{"old-child":{"status":"running"}}});
        s.entries.push(Entry {
            id: "old-launch".into(),
            kind: "collabAgentToolCall".into(),
            data: data.into(),
            ..Entry::default()
        });
        restore(&mut s);
        assert_eq!(s.subagents.len(), 1);
        assert_eq!(
            s.subagents.first().map(|a| a.state),
            Some(State::Interrupted)
        );
        let previous = serde_json::to_value(&s.subagents)?;
        restore(&mut s);
        assert_eq!(previous, serde_json::to_value(&s.subagents)?);
        assert_eq!(s.entries.len(), 1);
        Ok(())
    }
    #[test]
    fn subagents_codex_children_are_isolated_durable_and_incremental() -> anyhow::Result<()> {
        let mut s = session();
        super::super::engine::apply_event(
            &mut s,
            &json!({"method":"item/completed","params":{"threadId":"parent-thread","item":{
                "type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","senderThreadId":"parent-thread","receiverThreadIds":["child"],"prompt":"Check migrations", "agentsStates":{"child":{"status":"running"}}
            }}}),
        );
        super::super::engine::apply_event(
            &mut s,
            &json!({"method":"item/started","params":{"threadId":"child","item":{"type":"agentMessage","id":"reply","text":"Checking"}}}),
        );
        let previous = s.clone();
        super::super::engine::apply_event(
            &mut s,
            &json!({"method":"item/agentMessage/delta","params":{"threadId":"child","itemId":"reply","delta":" indexes"}}),
        );
        super::super::engine::apply_event(
            &mut s,
            &json!({"method":"turn/completed","params":{"threadId":"child","turn":{"status":"completed"}}}),
        );
        assert_eq!(s.turn_id.as_deref(), Some("parent-turn"));
        assert_eq!(s.status, super::super::Status::Running);
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.subagents.len(), 1);
        let child = s
            .subagents
            .first()
            .ok_or_else(|| anyhow::anyhow!("child"))?;
        assert_eq!(child.state, State::Completed);
        assert_eq!(
            child.entries.first().map(|e| e.text.as_str()),
            Some("Checking indexes")
        );
        let delta = super::super::transport::Delta::between(&previous, &s);
        assert!(delta.session.subagents.iter().all(|a| a.entries.is_empty()));
        let mut restored: Session = serde_json::from_value(serde_json::to_value(previous)?)?;
        delta.apply(&mut restored)?;
        assert_eq!(
            serde_json::to_value(&restored.subagents)?,
            serde_json::to_value(&s.subagents)?
        );
        Ok(())
    }
    #[test]
    fn subagents_claude_merges_out_of_order_task_and_tool_identity() {
        let mut s = session();
        claude_event(
            &mut s,
            &json!({"type":"system","subtype":"task_started","task_type":"local_agent","task_id":"native","run_id":"01"}),
        );
        claude_event(
            &mut s,
            &json!({"type":"assistant","message":{"id":"root","content":[{"type":"tool_use","id":"tool","name":"Agent","input":{"description":"Review indexes","prompt":"Check the schema"}}]}}),
        );
        claude_event(
            &mut s,
            &json!({"type":"assistant","agent_id":"native","parent_tool_use_id":"tool","uuid":"reply","message":{"id":"child","content":[{"type":"text","text":"Schema checked"}]}}),
        );
        assert_eq!(s.subagents.len(), 1);
        assert_eq!(
            s.subagents.first().map(|a| a.task.as_str()),
            Some("Review indexes")
        );
        assert_eq!(s.subagents.first().map(|a| a.entries.len()), Some(2));
    }
    #[test]
    fn subagents_claude_resume_and_background_tasks_keep_their_identity() -> anyhow::Result<()> {
        let mut s = session();
        let mut decoder = super::super::claude::events::Decoder::default();
        decoder.apply(&mut s, &json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"launch","name":"Agent","input":{"description":"Review schema","prompt":"Check indexes","run_in_background":true}}]}}));
        decoder.apply(&mut s, &json!({"type":"system","subtype":"task_started","task_type":"local_agent","task_id":"agent1","tool_use_id":"launch","run_id":"01","is_backgrounded":true}));
        decoder.apply(&mut s, &json!({"type":"assistant","agent_id":"agent1","parent_tool_use_id":"launch","uuid":"child-msg","message":{"id":"child-message","content":[{"type":"text","text":"Index checked"}]}}));
        decoder.apply(&mut s, &json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"launch","content":"Background task started"}]}}));
        assert_eq!(s.subagents.first().map(|a| a.state), Some(State::Running));
        decoder.apply(&mut s, &json!({"type":"system","subtype":"task_started","task_type":"local_bash","task_id":"bash1","tool_use_id":"bash"}));
        decoder.apply(&mut s, &json!({"type":"system","subtype":"task_notification","task_id":"agent1","run_id":"01","status":"completed"}));
        decoder.apply(&mut s, &json!({"type":"system","subtype":"task_started","task_type":"local_agent","task_id":"agent1","tool_use_id":"resume","run_id":"02"}));
        decoder.apply(&mut s, &json!({"type":"system","subtype":"task_notification","task_id":"agent1","run_id":"01","status":"completed"}));
        assert_eq!(s.subagents.len(), 1);
        let child = s
            .subagents
            .first()
            .ok_or_else(|| anyhow::anyhow!("child"))?;
        assert_eq!(child.state, State::Running);
        assert!(
            child
                .entries
                .iter()
                .any(|e| e.text.as_str() == "Index checked")
        );
        assert!(!s.entries.iter().any(|e| e.text.as_str() == "Index checked"));
        assert_eq!(s.thread_id.as_deref(), Some("parent-thread"));
        Ok(())
    }
}
