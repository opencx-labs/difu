mod isolation;
mod mcp_control;
mod permissions_control;
mod plan_control;
mod registration;
use super::{
    Control, Entry, Job, Pending, Prompt, Reply, Session, Skill, Status,
    server::{Command as AgentCommand, Store},
};
use crate::process::{Cancel, ChildGroup};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Write},
    path::Path,
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

struct Connection {
    _child: ChildGroup,
    input: ChildStdin,
    output: mpsc::Receiver<Result<Value, String>>,
    next: u64,
    inherited: String,
    needs_resume: bool,
}
#[derive(Debug)]
struct RpcError {
    code: Option<i64>,
    method: String,
    detail: Value,
}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Codex {}: {}", self.method, self.detail)
    }
}
impl std::error::Error for RpcError {}
impl RpcError {
    fn missing_thread(&self) -> bool {
        self.method == "thread/resume"
            && self
                .detail
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| {
                    message == "no rollout found"
                        || message.starts_with("no rollout found for thread ")
                })
    }
}
impl Connection {
    fn open(cwd: &Path, cancel: &Cancel) -> Result<Self> {
        let mut child = ChildGroup::spawn_tree(
            Command::new("codex")
                .arg("app-server")
                .current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .context("Cannot start Codex app-server; install and authenticate the Codex CLI")?;
        let input = child.child.stdin.take().context("Missing Codex stdin")?;
        let output = child.child.stdout.take().context("Missing Codex stdout")?;
        let (sender, receiver) = crate::wake::channel(cancel.wake());
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let result = super::client::read_line(&mut reader)
                    .and_then(|line| Ok(serde_json::from_str(&line)?))
                    .map_err(|e| format!("{e:#}"));
                let failed = result.is_err();
                if sender.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            _child: child,
            input,
            output: receiver,
            next: 1,
            inherited: String::new(),
            needs_resume: false,
        })
    }
    fn write(&mut self, value: Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, &value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
    fn call(
        &mut self,
        method: &str,
        params: Value,
        cancel: &Cancel,
        mut event: impl FnMut(Value) -> Result<()>,
    ) -> Result<Value> {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.write(json!({"id":id, "method":method, "params":params}))?;
        let start = Instant::now();
        loop {
            cancel.check()?;
            ensure!(
                start.elapsed() < Duration::from_secs(45),
                "Codex {method} did not acknowledge the request; inspect the session before retrying"
            );
            match self.output.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(value)) if value.get("method").is_some() => event(value)?,
                Ok(Ok(value)) if value.get("id").and_then(Value::as_u64) == Some(id) => {
                    if let Some(error) = value.get("error") {
                        return Err(RpcError {
                            code: error.get("code").and_then(Value::as_i64),
                            method: method.into(),
                            detail: error.clone(),
                        }
                        .into());
                    }
                    return value
                        .get("result")
                        .cloned()
                        .context("Codex response has no result");
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => anyhow::bail!("{error}"),
                Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("Codex disconnected"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    fn initialize(&mut self, cancel: &Cancel) -> Result<()> {
        self.call("initialize", json!({"clientInfo":{"name":"difu","title":"difu","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}), cancel, |_| Ok(()))?;
        self.write(json!({"method":"initialized","params":{}}))
    }
}

pub(super) fn usage(session: &Session, cancel: &Cancel) -> Result<Value> {
    let cwd = session.workspace.as_deref().unwrap_or(session.job.root());
    let mut rpc = Connection::open(cwd, cancel)?;
    rpc.initialize(cancel)?;
    usage_with(&mut rpc, session, cancel, |_| Ok(()))
}
fn usage_with(
    rpc: &mut Connection,
    session: &Session,
    cancel: &Cancel,
    event: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let limits = rpc.call("account/rateLimits/read", Value::Null, cancel, event)?;
    Ok(
        json!({"model":session.model,"reasoningEffort":session.effort,"context":session.token_usage,"limits":limits}),
    )
}

pub fn defaults(cwd: &Path) -> Result<Reply> {
    let expanded = if let Some(relative) = cwd.to_str().and_then(|s| s.strip_prefix("~/")) {
        dirs::home_dir()
            .context("Cannot locate home directory")?
            .join(relative)
    } else {
        cwd.to_owned()
    };
    let cwd = expanded
        .canonicalize()
        .context("Select an existing local repository directory")?;
    let cancel = Cancel::default();
    let mut rpc = Connection::open(&cwd, &cancel)?;
    rpc.initialize(&cancel)?;
    let config = rpc.call(
        "config/read",
        json!({"cwd":cwd,"includeLayers":false}),
        &cancel,
        |_| Ok(()),
    )?;
    let catalog = rpc.call("model/list", json!({"limit":100}), &cancel, |_| Ok(()))?;
    let default = catalog.get("data").and_then(Value::as_array).and_then(|a| {
        a.iter()
            .find(|v| v.get("isDefault").and_then(Value::as_bool) == Some(true))
    });
    let model = config
        .pointer("/config/model")
        .and_then(Value::as_str)
        .or_else(|| default.and_then(|v| v.get("model")).and_then(Value::as_str))
        .context("Codex did not report a default model")?;
    let effort = config
        .pointer("/config/model_reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            default
                .and_then(|v| v.get("defaultReasoningEffort"))
                .and_then(Value::as_str)
        })
        .unwrap_or("model default");
    let permission = json!({"approval_policy":config.pointer("/config/approval_policy"),"sandbox_mode":config.pointer("/config/sandbox_mode"),"approvals_reviewer":config.pointer("/config/approvals_reviewer")});
    Ok(Reply::Defaults {
        model: model.into(),
        effort: effort.into(),
        permissions: permission.to_string(),
    })
}

fn item_text(item: &Value) -> String {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("activity");
    match kind {
        "agentMessage" | "plan" => string(item, "text"),
        "dynamicToolCall"
            if matches!(
                item.get("tool").and_then(Value::as_str),
                Some(super::orchestrator::TOOL | super::peers::TOOL | super::folders::TOOL)
            ) =>
        {
            format!(
                "{} · {} · {}",
                string(item, "tool"),
                item.pointer("/arguments/action")
                    .and_then(Value::as_str)
                    .unwrap_or("lookup"),
                item.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("running")
            )
        }
        "userMessage" => item
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        "commandExecution" => format!(
            "$ {}\n{}",
            string(item, "command"),
            string(item, "aggregatedOutput")
        ),
        "reasoning" => item
            .get("summary")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        "fileChange" => item
            .get("changes")
            .map(Value::to_string)
            .unwrap_or_default(),
        _ => item.to_string(),
    }
}
fn put(value: &mut Value, key: &str, field: Value) -> Result<()> {
    value
        .as_object_mut()
        .context("Expected protocol object")?
        .insert(key.into(), field);
    Ok(())
}
fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn apply_event(session: &mut Session, event: &Value) {
    let method = event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = event.get("params").unwrap_or(&Value::Null);
    if matches!(method, "item/started" | "item/completed")
        && params.pointer("/item/type").and_then(Value::as_str) == Some("userMessage")
    {
        session.permission_state.codex_rollout = true;
    }
    if let Some(id) = event.get("id") {
        let tool = params.get("tool").and_then(Value::as_str);
        if method == "item/tool/call"
            && (session.orchestrator && tool == Some(super::orchestrator::TOOL)
                || session.peer_tools && tool == Some(super::peers::TOOL)
                || session.folder_tools && tool == Some(super::folders::TOOL))
        {
            session.orchestrator_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }
        if session.question_tools
            && method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(super::questions::TOOL)
        {
            session.question_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }
        if method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(super::management::TOOL)
        {
            session.deletion_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }

        if session.registration_tools
            && method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(super::registration::TOOL)
        {
            session.registration_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }

        if session.artifact_tools
            && method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(super::artifacts::TOOL)
        {
            session.artifact_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }

        if session.pr_tools
            && method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(super::pr_attachments::TOOL)
        {
            session.pr_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }
        if session.shell_tools
            && method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(super::shells::TOOL)
        {
            session.shell_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }

        if !session.native_setup
            && session.deferred_workspace
            && method == "item/tool/call"
            && params.get("tool").and_then(Value::as_str) == Some(isolation::TOOL)
        {
            session.workspace_requests.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
            return;
        }
        if !session.pending.iter().any(|p| &p.id == id) {
            let mut pending = Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            };
            super::elicitation::normalize(&mut pending);
            session.pending.push(pending);
        }
        session.status = Status::Waiting;
        return;
    }
    match method {
        "thread/settings/updated" => {
            if params.get("threadId").and_then(Value::as_str) == session.thread_id.as_deref()
                && let Some(mode) = params
                    .pointer("/threadSettings/collaborationMode/mode")
                    .and_then(Value::as_str)
                && session
                    .permission_state
                    .pending_mode
                    .as_deref()
                    .is_none_or(|pending| pending == mode)
            {
                session.permission_state.mode = Some(mode.into());
                session.permission_state.pending_mode = None;
                if mode != "plan" {
                    super::plan::invalidate(session);
                }
            }
        }
        "turn/started" => {
            super::plan::invalidate(session);
            session.turn_started_at = Some(chrono::Utc::now().timestamp_millis());
            session.turn_id = params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(String::from);
            session.status = Status::Running;
        }
        "turn/completed" => {
            super::plan::completed(session, params);
            session.completed_turn_successful =
                params.pointer("/turn/status").and_then(Value::as_str) == Some("completed");
            session.completed_turn = params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(String::from);
            session.turn_id = None;
            session.pending.retain(Pending::is_async_question);
            session.workspace_requests.clear();
            for entry in &mut session.entries {
                if let Some(data) = entry.data.as_object_mut() {
                    data.remove("difuSteeringTurn");
                }
                if entry.data.get("difuUserShell").and_then(Value::as_bool) != Some(true)
                    && entry.started_at.is_some()
                    && entry.finished_at.is_none()
                {
                    entry.finished_at = Some(chrono::Utc::now().timestamp_millis());
                    if entry.kind != "agentMessage"
                        && entry.kind != "reasoning"
                        && let Some(data) = entry.data.as_object_mut()
                    {
                        data.insert("status".into(), json!("interrupted"));
                    }
                }
            }
            let status = params.pointer("/turn/status").and_then(Value::as_str);
            session.status = match status {
                Some("failed") => Status::Failed,
                Some("interrupted") => Status::Interrupted,
                _ => Status::Idle,
            };
            if let Some(error) = params.pointer("/turn/error").filter(|v| !v.is_null()) {
                session.error = Some(error.to_string());
            }
            if status == Some("completed")
                && !session.title_ready
                && let Some(entry) = session.entries.iter().rev().find(|e| {
                    e.kind == "agentMessage"
                        && !e.text.is_empty()
                        && e.started_at >= session.turn_started_at
                })
            {
                session.title_ready = true;
                session.title_response = entry.text.to_string();
            }
            if session.status != Status::Idle && !session.switching_workspace {
                for text in std::mem::take(&mut session.queue) {
                    session.unsent(text);
                }
            }
        }
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => {
            let review_id = string(params, "reviewId");
            if review_id.is_empty() {
                return;
            }
            let id = format!("approval-review-{review_id}");
            let now = chrono::Utc::now().timestamp_millis();
            let completed = method.ends_with("/completed");
            if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                entry.data = params.clone().into();
                if completed {
                    entry.finished_at = Some(
                        params
                            .get("completedAtMs")
                            .and_then(Value::as_i64)
                            .unwrap_or(now),
                    );
                }
            } else {
                session.entries.push(Entry {
                    id,
                    kind: "autoApprovalReview".into(),
                    data: params.clone().into(),
                    started_at: Some(
                        params
                            .get("startedAtMs")
                            .and_then(Value::as_i64)
                            .unwrap_or(now),
                    ),
                    finished_at: completed.then(|| {
                        params
                            .get("completedAtMs")
                            .and_then(Value::as_i64)
                            .unwrap_or(now)
                    }),
                    ..Entry::default()
                });
            }
        }
        "serverRequest/resolved" => {
            session
                .pending
                .retain(|p| Some(&p.id) != params.get("requestId"));
            if session.pending.is_empty() && session.turn_id.is_some() {
                session.status = Status::Running;
            }
        }
        "thread/realtime/started" => session.voice = true,
        "thread/realtime/transcript/delta" | "thread/realtime/transcript/done" => {
            let done = method.ends_with("/done");
            let text = string(params, if done { "text" } else { "delta" });
            voice_transcript(session, &string(params, "role"), &text, done);
        }
        "thread/realtime/error" => {
            session.note("error", format!("Voice: {}", string(params, "message")));
        }
        "thread/realtime/closed" => {
            session.voice = false;
            let now = chrono::Utc::now().timestamp_millis();
            for entry in &mut session.entries {
                if entry.data.get("difuVoice").is_some() && entry.finished_at.is_none() {
                    entry.finished_at = Some(now);
                }
            }
            session.entries.retain(|entry| {
                entry.data.get("difuVoice").is_none() || !entry.text.trim().is_empty()
            });
        }
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item") {
                let id = string(item, "id");
                let mut kind = string(item, "type");
                let mut text = item_text(item);
                // Codex frames work the voice hands over; show the request itself.
                let handoff = (kind == "userMessage").then(|| delegation(&text)).flatten();
                if let Some(input) = &handoff {
                    text.clone_from(input);
                }
                let entry_id = id.clone();
                if kind == "userMessage"
                    && !session.entries.iter().any(|entry| entry.id == id)
                    && let Some(entry) = session.entries.iter_mut().find(|entry| {
                        matches!(
                            entry.kind.as_str(),
                            "sending" | "sending_context" | "userMessage"
                        ) && entry
                            .data
                            .get("wire_text")
                            .and_then(Value::as_str)
                            .is_some_and(|wire| wire == text || entry.text == text)
                    })
                {
                    // Reconcile the server echo in place: delayed echoes must not move
                    // an older prompt after a newer steering message.
                    entry.id = id.clone();
                    if entry.kind == "sending_context" {
                        kind = "system".into();
                    }
                    text = entry.text.to_string();
                }
                if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                    let user_message = kind == "userMessage";
                    if entry.kind != "system" {
                        entry.kind = kind;
                    }
                    if !user_message {
                        entry.text = text.into();
                    }
                    let mut data = item.clone();
                    if entry.kind == "plan"
                        && let Some(object) = data.as_object_mut()
                    {
                        for key in [
                            "difuPlanObserved",
                            "difuPlanThread",
                            "difuPlanTurn",
                            "difuPlanSucceeded",
                            "difuPlanLegacySucceeded",
                        ] {
                            if let Some(value) = entry.data.get(key) {
                                object.insert(key.into(), value.clone());
                            }
                        }
                    }
                    entry.replace_data(data);
                    if method == "item/completed" {
                        entry.finished_at = Some(chrono::Utc::now().timestamp_millis());
                    }
                } else {
                    session.entries.push(Entry {
                        timestamp: Some(chrono::Utc::now().timestamp_millis()),
                        id,
                        kind,
                        text: text.into(),
                        data: item.clone().into(),
                        started_at: Some(chrono::Utc::now().timestamp_millis()),
                        finished_at: (method == "item/completed")
                            .then(|| chrono::Utc::now().timestamp_millis()),
                    });
                }
                if handoff.is_some()
                    && let Some(data) = session
                        .entries
                        .iter_mut()
                        .find(|entry| entry.id == entry_id)
                        .and_then(|entry| entry.data.as_object_mut())
                {
                    data.insert("difuVoiceHandoff".into(), json!(true));
                }
                if method == "item/completed" {
                    super::plan::associate(session, params);
                }
            }
        }
        "item/agentMessage/delta"
        | "item/plan/delta"
        | "item/commandExecution/outputDelta"
        | "item/reasoning/summaryTextDelta" => {
            let id = string(params, "itemId");
            let delta = string(params, "delta");
            if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                entry.text.push_str(&delta);
                if method == "item/plan/delta"
                    && let Some(data) = entry.data.as_object_mut()
                {
                    data.insert("text".into(), json!(entry.text.as_str()));
                }
            }
        }
        "error" => {
            session.error = Some(params.to_string());
            session.note("error", params.to_string());
        }
        "item/mcpToolCall/progress" => {
            let id = string(params, "itemId");
            if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                entry.text.push('\n');
                entry.text.push_str(&string(params, "message"));
            }
        }
        "thread/tokenUsage/updated" => {
            session.token_usage = params.get("tokenUsage").cloned().unwrap_or(Value::Null)
        }
        "turn/plan/updated" => {
            let id = format!("plan-{}", session.turn_id.as_deref().unwrap_or("current"));
            if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                entry.data = params.clone().into();
            } else {
                session.entries.push(Entry {
                    id,
                    kind: "plan".into(),
                    data: params.clone().into(),
                    ..Entry::default()
                });
            }
        }
        _ => {}
    }
    if matches!(
        method,
        "item/started" | "item/completed" | "item/autoApprovalReview/completed"
    ) {
        session.finish_steering_wait();
    }
    if matches!(method, "item/started" | "item/completed") {
        session.restore_async_questions();
    }
    super::plan::reconcile(session);
}
fn event(store: &Store, id: &str, value: Value) -> Result<()> {
    store.update(id, |s| apply_event(s, &value))
}

pub fn skills(cwd: &Path, force: bool) -> Result<Reply> {
    let cancel = Cancel::default();
    let mut rpc = Connection::open(cwd, &cancel)?;
    rpc.initialize(&cancel)?;
    let value = rpc.call(
        "skills/list",
        json!({"cwds":[cwd],"forceReload":force}),
        &cancel,
        |_| Ok(()),
    )?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .context("Codex returned no skill catalog")?;
    let mut skills = Vec::new();
    let mut errors = Vec::new();
    for result in data {
        if let Some(entries) = result.get("skills").and_then(Value::as_array) {
            for entry in entries {
                skills.push(serde_json::from_value::<Skill>(entry.clone())?);
            }
        }
        if let Some(entries) = result.get("errors").and_then(Value::as_array) {
            errors.extend(entries.iter().map(Value::to_string));
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    skills.dedup_by(|a, b| a.path == b.path);
    Ok(Reply::Skills { skills, errors })
}
fn start_turn(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    prompt: &Prompt,
    steer: bool,
    cancel: &Cancel,
) -> Result<()> {
    send_turn(rpc, store, id, prompt, steer, cancel, TurnKind::User)
}
enum TurnKind<'a> {
    User,
    Internal,
    ApprovedPlan(&'a str),
}
fn send_turn(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    prompt: &Prompt,
    steer: bool,
    cancel: &Cancel,
    kind: TurnKind<'_>,
) -> Result<()> {
    store.update(id, super::plan::invalidate)?;
    if !steer {
        permissions_control::apply_preset(rpc, store, id, cancel)?;
    }
    let text = prompt.text();
    let internal = matches!(kind, TurnKind::Internal);
    let displayed = match kind {
        TurnKind::ApprovedPlan(caption) => caption,
        _ => text,
    };
    if let Prompt::Routed { origin, .. } = prompt {
        store.update(id, |s| s.set_slack_route(origin.clone()))?;
    }
    let session = store.get(id)?;
    ensure!(
        steer || session.permission_state.pending_preset.is_none(),
        "Codex has not confirmed the requested permission preset; no prompt was sent"
    );
    ensure!(
        steer || session.permission_state.pending_mode.is_none(),
        "Provider has not confirmed the requested starting mode; no prompt was sent"
    );
    let thread = session
        .thread_id
        .as_ref()
        .context("Codex session is not connected")?;
    let mut input = vec![json!({"type":"text","text":text})];
    if let Some(context) = &session.provider_context {
        input.push(json!({"type":"text","text":context}));
    }
    let (shell_ids, shell_context) = super::shells::context(&session);
    if let Some(context) = shell_context {
        input.push(json!({"type":"text","text":context}));
    }
    if let Some(context) = super::questions::pending_context(&session, prompt) {
        input.push(json!({"type":"text","text":context}));
    }
    for skill in prompt.skills() {
        ensure!(
            skill.enabled && skill.path.is_absolute(),
            "Choose an enabled skill with an absolute path"
        );
        input.push(json!({"type":"skill","name":skill.name,"path":skill.path}));
    }
    for attachment in prompt.attachments() {
        super::media::validate(&store.storage, id, attachment)?;
        match attachment.kind {
            super::media::Kind::Image => input.push(json!({"type":"localImage","path":attachment.path})),
            super::media::Kind::Video => input.push(json!({"type":"text","text":format!("{} Local video path (not extracted into frames by difu): {}", attachment.token(), attachment.path.display())})),
        }
    }
    let wire_text = input
        .iter()
        .filter_map(|v| v.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let mut params = json!({"threadId":thread,"input":input});
    if session.waiting_for_workspace() && !steer {
        put(
            &mut params,
            "sandboxPolicy",
            isolation::read_only_policy(&session),
        )?;
        put(
            &mut params,
            "approvalPolicy",
            isolation::investigation_approvals(),
        )?;
    }

    let method = if steer {
        put(
            &mut params,
            "expectedTurnId",
            json!(
                session
                    .turn_id
                    .as_ref()
                    .context("No active turn to steer; send the message again")?
            ),
        )?;
        "turn/steer"
    } else {
        if let Some(model) = &session.model {
            put(
                &mut params,
                "model",
                json!(super::provider::Provider::native_model(model)),
            )?;
        }
        if let Some(effort) = &session.effort {
            put(&mut params, "effort", json!(effort))?;
        }
        if let Some(mode) = permissions_control::collaboration(&session)? {
            put(&mut params, "collaborationMode", mode)?;
        }
        "turn/start"
    };
    // Store intent before sending: a lost reply must never trigger automatic replay.
    let mut sending_id = None;
    store.update(id, |s| {
        if !steer {
            // A lost acknowledgement can leave a real native conversation behind.
            // Persist this before writing turn/start so reconnect never replaces it.
            s.permission_state.codex_turn_attempted = true;
            s.status = Status::Starting;
            s.error = None;
        }
        if !internal
            && let Job::Coding(launch) = &mut s.job
            && launch.prompt.is_empty()
        {
            launch.prompt = text.to_owned();
        }
        s.entries
            .retain(|entry| !(entry.kind == "awaiting connection" && entry.text == text));
        s.note(
            if internal {
                "sending_context"
            } else {
                "sending"
            },
            displayed,
        );
        if let Some(entry) = s.entries.last_mut() {
            sending_id = Some(entry.id.clone());
            entry.data =
                json!({"wire_text":wire_text,"attachments":prompt.attachments(),"prompt":prompt,
                "difuSteeringTurn":if steer && !internal { session.turn_id.clone() } else { None }})
                .into();
        }
    })?;
    store.save(id)?;
    let result = rpc.call(method, params, cancel, |v| event(store, id, v));
    match result {
        Ok(response) => {
            if !steer {
                store.update(id, |s| s.permission_state.codex_rollout = true)?;
            }
            store.update(id, |s| {
                // Steering may be acknowledged without a userMessage event.
                // Record accepted user input immediately; never promote failed sends.
                if !internal
                    && let Some(entry) = s.entries.iter_mut().find(|entry| {
                        Some(&entry.id) == sending_id.as_ref() && entry.kind == "sending"
                    })
                {
                    entry.kind = "userMessage".into();
                }

                s.provider_context = None;
                super::shells::delivered(s, &shell_ids);
                s.finish_steering_wait();
                if session.waiting_for_workspace() && !steer {
                    s.permissions = json!({"sandbox":isolation::read_only_policy(&session),"approvalPolicy":isolation::investigation_approvals(),
                        "approvalsReviewer":session.inherited_permissions.get("approvalsReviewer")});
                }
                if let Some(turn) = response.pointer("/turn/id").and_then(Value::as_str)
                    && s.completed_turn.as_deref() != Some(turn)
                {
                    s.turn_id = Some(turn.into());
                }
                if s.turn_id.is_some() && s.pending.is_empty() {
                    s.status = Status::Running;
                }
            })?;
            store.save(id)
        }
        Err(error) => {
            store.update(id, |s| {
                if let Some(entry) = s
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| Some(&e.id) == sending_id.as_ref())
                {
                    entry.kind = "unsent or unacknowledged".into();
                } else {
                    s.note("unsent or unacknowledged", displayed);
                }
            })?;
            Err(error)
        }
    }
}

fn resume_thread(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    session: &Session,
    cancel: &Cancel,
) -> Result<()> {
    ensure!(
        session.turn_id.is_none(),
        "The turn is still active; wait for interruption to complete"
    );
    let params = permissions_control::resume_params(rpc, session, false)?;
    let response = rpc.call("thread/resume", params, cancel, |v| event(store, id, v))?;
    rpc.needs_resume = false;
    permissions_control::record_policy_preserving_pending(store, id, &response)?;
    Ok(())
}

fn command(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    control: Control,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    match control {
        Control::DecideClaudePlan { .. } => {
            anyhow::bail!("This plan request belongs to a Claude conversation")
        }
        Control::DecidePlan { plan_id, choice } => {
            plan_control::decide(rpc, store, id, &plan_id, choice, cancel)?;
        }
        Control::Permissions { action, response } => {
            let result = permissions_control::execute(rpc, store, id, action, cancel)
                .map_err(|e| format!("{e:#}"));
            let _ = response.send(result);
        }
        Control::Mcp { action, response } => {
            let snapshot =
                mcp_control::execute(rpc, store, id, action, cancel).map_err(|e| format!("{e:#}"));
            let _ = response.send(snapshot);
        }
        Control::ReadUsage => {
            let usage = usage_with(rpc, &session, cancel, |v| event(store, id, v))?;
            store.update(id, |s| s.usage = usage)?;
        }
        Control::Voice { action, response } => {
            // Voice failures belong to the terminal that asked, not the session.
            let result = voice(rpc, store, id, &session, action, cancel);
            let _ = response.send(result.map_err(|e| format!("{e:#}")));
        }

        Control::Message {
            text,
            queue,
            skills,
            attachments,
        }
        | Control::MessageWithAttachments {
            text,
            queue,
            skills,
            attachments,
        } => {
            ensure!(!text.trim().is_empty(), "Message cannot be empty");
            store.update(id, super::plan::invalidate)?;
            store.save(id)?;
            if matches!(session.status, Status::Interrupted | Status::Failed) {
                resume_thread(rpc, store, id, &session, cancel)?;
            }
            let prompt = Prompt::WithSkills {
                text,
                skills,
                attachments,
            };
            if queue && session.turn_id.is_some() {
                store.update(id, |s| s.queue.push(prompt))?;
                store.save(id)?;
            } else {
                start_turn(rpc, store, id, &prompt, session.turn_id.is_some(), cancel)?;
            }
        }
        Control::ReplaceQueued {
            index,
            expected,
            replacement,
        } => {
            ensure!(
                session.queue.get(index) == Some(&expected),
                "That queued message changed or was already sent; refresh the queue. Your draft is retained."
            );
            store.update(id, |s| {
                if let Some(prompt) = replacement {
                    if let Some(item) = s.queue.get_mut(index) {
                        *item = prompt.inherit_route(item);
                    }
                } else {
                    s.queue.remove(index);
                }
            })?;
            store.save(id)?;
        }
        Control::CloseShell { process } => {
            rpc.call(
                "thread/backgroundTerminals/terminate",
                json!({"threadId":session.thread_id,"processId":process}),
                cancel,
                |value| event(store, id, value),
            )?;
            store.update(id, |session| {
                session.shells.retain(|shell| {
                    shell.get("processId").and_then(Value::as_str) != Some(&process)
                })
            })?;
        }
        Control::RefreshShells => {
            let mut shells = Vec::new();
            let mut cursor = Value::Null;
            loop {
                let result = rpc.call(
                    "thread/backgroundTerminals/list",
                    json!({"threadId":session.thread_id,"cursor":cursor,"limit":100}),
                    cancel,
                    |v| event(store, id, v),
                )?;
                shells.extend(
                    result
                        .get("data")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                );
                cursor = result.get("nextCursor").cloned().unwrap_or(Value::Null);
                if cursor.is_null() {
                    break;
                }
            }
            if session.shells != shells {
                store.update(id, |s| s.shells = shells)?;
            }
        }
        Control::Compact => {
            ensure!(
                session.turn_id.is_none() && session.status == Status::Idle,
                "Finish or interrupt the active turn before compacting"
            );
            let thread = session
                .thread_id
                .as_ref()
                .context("Codex session is not connected")?;
            store.update(id, |s| {
                s.status = Status::Starting;
                s.note("system", "Compacting conversation…");
            })?;
            store.save(id)?;
            if let Err(error) = rpc.call(
                "thread/compact/start",
                json!({"threadId":thread}),
                cancel,
                |v| event(store, id, v),
            ) {
                store.update(id, |s| s.status = Status::Idle)?;
                return Err(error);
            }
        }
        Control::InterruptAndSend => {
            // UI snapshots can lag behind consumption. Never interrupt an unrelated turn.
            if !session.can_send_waiting() {
                return Ok(());
            }
            let accepted = session.pending_steering().next().is_some();
            if let (Some(thread), Some(turn)) = (&session.thread_id, &session.turn_id) {
                let preserve_queue = |value: Value| {
                    store.update(id, |s| {
                        let queue = std::mem::take(&mut s.queue);
                        apply_event(s, &value);
                        s.queue = queue;
                    })
                };
                rpc.call(
                    "turn/interrupt",
                    json!({"threadId":thread,"turnId":turn}),
                    cancel,
                    &preserve_queue,
                )?;
                // The interrupt acknowledgement may precede turn/completed.
                let started = Instant::now();
                while store.get(id)?.turn_id.as_deref() == Some(turn) {
                    cancel.check()?;
                    ensure!(
                        started.elapsed() < Duration::from_secs(45),
                        "Codex has not finished interrupting; waiting messages were retained"
                    );
                    match rpc.output.recv_timeout(Duration::from_millis(50)) {
                        Ok(Ok(value)) => preserve_queue(value)?,
                        Ok(Err(error)) => anyhow::bail!("{error}"),
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            anyhow::bail!("Codex disconnected; waiting messages were retained")
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            }
            let mut delivered = false;
            loop {
                let current = store.get(id)?;
                if current.queue.is_empty() {
                    break;
                }
                let mut next = None;
                store.update(id, |s| {
                    if !s.queue.is_empty() {
                        next = Some(s.queue.remove(0));
                    }
                })?;
                if let Some(prompt) = next {
                    // send_turn records durable intent before the RPC, and never retries it.
                    if let Err(error) =
                        start_turn(rpc, store, id, &prompt, current.turn_id.is_some(), cancel)
                    {
                        let value = serde_json::to_value(&prompt)?;
                        store.update(id, |s| {
                            let recorded = s
                                .entries
                                .iter()
                                .skip(current.entries.len())
                                .any(|entry| entry.data.get("prompt") == Some(&value));
                            if !recorded {
                                s.unsent(prompt);
                            }
                        })?;
                        store.save(id)?;
                        return Err(error);
                    }
                    delivered = true;
                }
            }
            if accepted && !delivered {
                // Continue the existing conversation; accepted steering is already in it.
                command(rpc, store, id, Control::Resume, cancel)?;
            }
            store.save(id)?;
        }
        Control::Interrupt => {
            if let (Some(thread), Some(turn)) = (&session.thread_id, &session.turn_id) {
                rpc.call(
                    "turn/interrupt",
                    json!({"threadId":thread,"turnId":turn}),
                    cancel,
                    |v| event(store, id, v),
                )?;
            }
            store.update(id, |s| {
                s.status = Status::Interrupted;
                for text in std::mem::take(&mut s.queue) {
                    s.unsent(text);
                }
            })?;
        }
        Control::Resume => {
            resume_thread(rpc, store, id, &session, cancel)?;
            start_turn(
                rpc,
                store,
                id,
                &Prompt::from(
                    "Continue the task from its current state. Inspect existing changes and any prior publication before proceeding; do not repeat completed actions.",
                ),
                false,
                cancel,
            )?;
        }
        Control::AnswerQuestion {
            request,
            question,
            answer,
        } => {
            let (mut updated, text) =
                super::questions::prepare_answer(&session, &request, &question, answer.as_deref())?;
            if updated.is_async_question() {
                if answer.is_some() {
                    if matches!(session.status, Status::Interrupted | Status::Failed) {
                        resume_thread(rpc, store, id, &session, cancel)?;
                    }
                    // Record this question's answer only after delivery succeeds.
                    // A failed attempt must leave every unanswered question retryable.
                    start_turn(
                        rpc,
                        store,
                        id,
                        &Prompt::QuestionAnswer {
                            text,
                            question_request: request.clone(),
                            question_id: Some(question.clone()),
                        },
                        session.turn_id.is_some(),
                        cancel,
                    )?;
                }
            } else if updated.unanswered_questions().is_empty() {
                // Blocking native tools accept one response for the whole batch.
                let response = super::elicitation::response(
                    &updated,
                    json!({"answers":updated.params.get("difuAnswers")}),
                )?;
                updated.responded = true;
                super::questions::save_answer(store, id, updated.clone())?;
                rpc.write(json!({"id":request,"result":response}))?;
            }
            super::questions::save_answer(store, id, updated)?;
        }
        Control::Respond { request, response } => {
            let pending = session
                .pending
                .iter()
                .find(|p| p.id == request)
                .context("This request is no longer pending")?;
            ensure!(
                pending.is_async_question() || !pending.responded,
                "A response was already sent; waiting for Codex acknowledgement"
            );
            if session.waiting_for_workspace() {
                match pending.method.as_str() {
                    "item/permissions/requestApproval" => ensure!(
                        response
                            .get("permissions")
                            .and_then(Value::as_object)
                            .is_some_and(|v| v.is_empty()),
                        "Original repository is read-only; request an editing worktree before granting permissions"
                    ),
                    "item/fileChange/requestApproval" | "item/commandExecution/requestApproval" => {
                        ensure!(
                            matches!(
                                response.get("decision").and_then(Value::as_str),
                                Some("decline" | "cancel")
                            ),
                            "Original repository is read-only; request an editing worktree before approving elevated access"
                        )
                    }
                    _ => {}
                }
            }
            validate_response(pending, &response)?;
            if pending.is_async_question() {
                let text = super::questions::answer_text(pending, &response)?;
                if matches!(session.status, Status::Interrupted | Status::Failed) {
                    resume_thread(rpc, store, id, &session, cancel)?;
                }
                // A failed async delivery remains available for an explicit user retry.
                start_turn(
                    rpc,
                    store,
                    id,
                    &Prompt::QuestionAnswer {
                        text,
                        question_request: request.clone(),
                        question_id: None,
                    },
                    session.turn_id.is_some(),
                    cancel,
                )?;
                store.update(id, |s| {
                    if let Some(id) = request.as_str() {
                        s.answered_questions.insert(id.into());
                    }
                    s.pending.retain(|p| p.id != request);
                })?;
                store.save(id)?;
                return Ok(());
            }
            let response = super::elicitation::response(pending, response)?;
            rpc.write(json!({"id":request,"result":response}))?;
            store.update(id, |s| {
                if let Some(pending) = s.pending.iter_mut().find(|p| p.id == request) {
                    pending.responded = true;
                }
                // Only serverRequest/resolved or turn completion clears the request.
            })?;
        }
        Control::Model { model, effort } => {
            ensure!(
                session.turn_id.is_none(),
                "Interrupt or finish the active turn before changing its model"
            );
            // Server validates these settings on thread/resume before persisting them.
            let mut params = permissions_control::resume_params(rpc, &session, false)?;
            if let Some(model) = &model {
                put(
                    &mut params,
                    "model",
                    json!(super::provider::Provider::native_model(model)),
                )?;
            }
            if let Some(effort) = &effort {
                put(
                    &mut params,
                    "config",
                    json!({"model_reasoning_effort":effort}),
                )?;
            }
            permissions_control::selected_policy(&mut params, &session, false)?;
            let result = permissions_control::reload_idle(rpc, store, id, params, cancel)?;
            permissions_control::record_policy_preserving_pending(store, id, &result)?;
            store.update(id, |s| {
                s.model = result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(String::from);
                s.effort = result
                    .get("reasoningEffort")
                    .and_then(Value::as_str)
                    .map(String::from);
            })?;
            store.save(id)?;
        }
    }
    Ok(())
}
/// Starts or stops Codex's realtime voice on the session's own thread, the way
/// Codex's TUI does, so the voice can hand work to this agent. Codex streams
/// the agent's replies back to the voice to speak.
fn voice(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    session: &Session,
    action: super::VoiceAction,
    cancel: &Cancel,
) -> Result<Option<String>> {
    let thread = session
        .thread_id
        .clone()
        .context("Wait until the session has started before talking to it")?;
    let offer = match action {
        super::VoiceAction::Stop => {
            if session.voice {
                rpc.call(
                    "thread/realtime/stop",
                    json!({"threadId":thread}),
                    cancel,
                    |v| event(store, id, v),
                )?;
            }
            return Ok(None);
        }
        super::VoiceAction::Start { offer } => offer,
    };
    let outcome = |value: &Value| -> Option<Result<String, String>> {
        let text = |key| value.pointer(key).and_then(Value::as_str).map(String::from);
        match value.get("method").and_then(Value::as_str) {
            Some("thread/realtime/sdp") => text("/params/sdp").map(Ok),
            Some("thread/realtime/error" | "thread/realtime/closed") => {
                Some(Err(text("/params/message")
                    .or_else(|| text("/params/reason"))
                    .unwrap_or_else(|| "closed".into())))
            }
            _ => None,
        }
    };
    let mut params = json!({
        "threadId": thread,
        "outputModality": "audio",
        "version": "v3",
        "clientManagedHandoffs": false,
        "includeStartupContext": false,
        "transport": {"type": "webrtc", "sdp": offer},
    });
    if let Some(context) = &session.provider_context {
        // A session that just switched to Codex has not received its earlier
        // conversation yet; give the agent its recent part before it takes
        // spoken requests. The next typed message still carries all of it.
        put(
            &mut params,
            "realtimeStartInstructions",
            json!(voice_handoff(context)),
        )?;
    }
    let mut answer = None;
    rpc.call("thread/realtime/start", params, cancel, |value| {
        answer = answer.take().or_else(|| outcome(&value));
        event(store, id, value)
    })?;
    // OpenAI's answer follows the acknowledgement as a notification.
    let deadline = Instant::now() + Duration::from_secs(20);
    while answer.is_none() {
        cancel.check()?;
        ensure!(
            Instant::now() < deadline,
            "Codex did not answer the voice connection; try again"
        );
        match rpc.output.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(value)) => {
                answer = outcome(&value);
                event(store, id, value)?;
            }
            Ok(Err(error)) => anyhow::bail!("{error}"),
            Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("Codex disconnected"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    match answer {
        Some(Ok(answer)) => Ok(Some(answer)),
        Some(Err(failure)) => anyhow::bail!("Codex voice failed: {failure}"),
        None => anyhow::bail!("Codex did not answer the voice connection; try again"),
    }
}

/// Codex's own instructions for an agent behind the voice, which custom start
/// instructions replace, followed by the most recent part of a pending
/// conversation handoff within Codex's limit for them.
fn voice_handoff(context: &str) -> String {
    const START: &str = "Realtime conversation started.\n\nYou are operating as a backend executor behind an intermediary. The user does not talk to you directly. Any response you produce will be consumed by the intermediary and may be summarized before the user sees it.\n\nWhen invoked, you receive the latest conversation transcript and any relevant mode or metadata. The intermediary may invoke you even when backend help is not actually needed. Use the transcript to decide whether you should do work. If backend help is unnecessary, avoid verbose responses that add user-visible latency.\n\nWhen user text is routed from realtime, treat it as a transcript. It may be unpunctuated or contain recognition errors.\n\n- Keep responses concise and action-oriented. Your updates should help the intermediary respond to the user.";
    // Codex allows 8,192 estimated tokens; stay well inside at ~4 bytes each.
    const BUDGET: usize = 24_000;
    let mut start = context.len().saturating_sub(BUDGET);
    while !context.is_char_boundary(start) {
        start += 1;
    }
    let tail = context.get(start..).unwrap_or_default();
    let omitted = if start > 0 {
        "(Earlier conversation omitted.)\n"
    } else {
        ""
    };
    format!("{START}\n\n{omitted}{tail}")
}

/// The spoken words behind a turn the voice handed to the agent.
fn delegation(text: &str) -> Option<String> {
    let body = text
        .trim()
        .strip_prefix("<realtime_delegation>")?
        .strip_suffix("</realtime_delegation>")?;
    let (_, input) = body.split_once("<input>")?;
    let (input, _) = input.split_once("</input>")?;
    Some(
        input
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// Live voice transcripts become chat messages tagged with `difuVoice`, so
/// every view treats them as the user's and the voice's words.
fn voice_transcript(session: &mut Session, role: &str, text: &str, done: bool) {
    let (kind, role) = match role {
        "user" => ("userMessage", "user"),
        "assistant" => ("agentMessage", "assistant"),
        _ => return,
    };
    let now = chrono::Utc::now().timestamp_millis();
    let open = session.entries.iter_mut().rev().find(|entry| {
        entry.finished_at.is_none()
            && entry.data.get("difuVoice").and_then(Value::as_str) == Some(role)
    });
    match open {
        Some(entry) if done => {
            entry.text = text.trim().to_owned().into();
            entry.finished_at = Some(now);
        }
        Some(entry) => entry.text.push_str(text),
        None if text.trim().is_empty() => return,
        None => session.entries.push(Entry {
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            id: format!("voice-{role}-{now}-{}", session.entries.len()),
            kind: kind.into(),
            text: text.trim_start().to_owned().into(),
            data: json!({"difuVoice": role}).into(),
            started_at: None,
            finished_at: done.then_some(now),
        }),
    }
    // A segment whose final text is empty was noise; drop it.
    session.entries.retain(|entry| {
        entry.data.get("difuVoice").is_none()
            || entry.finished_at.is_none()
            || !entry.text.trim().is_empty()
    });
}

fn instructions(session: &Session) -> String {
    if session.orchestrator {
        return format!(
            "{}\n\n{}",
            super::orchestrator::INSTRUCTIONS,
            super::questions::INSTRUCTIONS
        );
    }
    let base = if session.native_setup {
        super::management::INSTRUCTIONS.to_owned()
    } else {
        let policy = if session.waiting_for_workspace() {
            isolation::INSTRUCTIONS
        } else if matches!(&session.job, Job::Coding(s) if s.isolated) {
            super::WORKTREE_INSTRUCTIONS
        } else {
            super::CODING_INSTRUCTIONS
        };
        format!(
            "{policy}\n\nCurrent repository: {}. Read instructions in the current repository before continuing; earlier conversation may refer to a different repository.\n\n{}",
            session.job.root().display(),
            super::management::INSTRUCTIONS
        )
    };
    let base =
        super::rules::instructions(session, format!("{}\n\n{base}", super::identity(session)));
    let base = if session.peer_tools {
        format!("{base}\n\n{}", super::peers::INSTRUCTIONS)
    } else {
        base
    };
    let base = if session.folder_tools {
        format!("{base}\n\n{}", super::folders::INSTRUCTIONS)
    } else {
        base
    };
    let base = if session.question_tools {
        format!("{base}\n\n{}", super::questions::INSTRUCTIONS)
    } else {
        base
    };
    let base = if session.registration_tools {
        format!("{base}\n\n{}", super::registration::INSTRUCTIONS)
    } else {
        base
    };
    let base = if session.pr_tools {
        format!("{base}\n\n{}", super::pr_attachments::INSTRUCTIONS)
    } else {
        format!(
            "{base}\n\n{}",
            super::pr_attachments::command_instructions(&session.id)
        )
    };
    let base = format!("{base}\n\n{}", super::shells::instructions(session));
    if session.artifact_tools {
        format!("{base}\n\n{}", super::artifacts::INSTRUCTIONS)
    } else {
        base
    }
}
fn connect(store: &Store, id: &str, cancel: &Cancel) -> Result<(Connection, bool)> {
    // Capture a legacy turn's durable outcome before reconnect resets live status.
    store.update(id, super::plan::preserve_legacy_outcome)?;
    if store.get(id)?.thread_id.is_none() {
        store.update(id, |s| {
            s.artifact_tools = true;
            s.pr_tools = true;
            s.shell_tools = true;
            s.registration_tools = true;
            s.question_tools = true;
            s.peer_tools = true;
            s.folder_tools = true;
        })?;
    }
    let session = store.get(id)?;
    let cwd = session
        .workspace
        .as_deref()
        .context("Missing session workspace")?;
    let mut rpc = Connection::open(cwd, cancel)?;
    rpc.initialize(cancel)?;
    let effective = rpc.call(
        "config/read",
        json!({"cwd":cwd,"includeLayers":false}),
        cancel,
        |v| event(store, id, v),
    )?;
    rpc.inherited = effective
        .pointer("/config/developer_instructions")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // Empty native threads have no durable rollout. Recreate only a known empty
    // thread; an acknowledged conversation must always resume its original ID.
    let mut resuming = session.thread_id.is_some()
        && (session.permission_state.codex_rollout
            || session.permission_state.codex_turn_attempted
            || !session.permission_state.codex_thread_started);
    let mut params = json!({"cwd":cwd,"runtimeWorkspaceRoots":[cwd],"developerInstructions":format!("{}\n\n{}", rpc.inherited, instructions(&session))});
    if resuming && let Some(thread) = &session.thread_id {
        put(&mut params, "threadId", json!(thread))?;
    }
    let Job::Coding(launch) = &session.job else {
        anyhow::bail!("Not a coding session");
    };
    let model = session.model.as_ref().or(launch.model.as_ref());
    let effort = session.effort.as_ref().or(launch.effort.as_ref());
    if let Some(model) = model {
        put(
            &mut params,
            "model",
            json!(super::provider::Provider::native_model(model)),
        )?;
    }
    if let Some(effort) = effort {
        put(
            &mut params,
            "config",
            json!({"model_reasoning_effort":effort}),
        )?;
    }
    if !resuming {
        let mut tools = super::orchestrator::tools(&session);
        if !session.native_setup && session.deferred_workspace && launch.isolated {
            tools.extend(isolation::tools().as_array().cloned().unwrap_or_default());
        }
        put(&mut params, "dynamicTools", json!(tools))?;
    }
    if resuming {
        isolation::settings(&mut params, &session)?;
    }
    permissions_control::selected_policy(&mut params, &session, true)?;
    let response = rpc.call(
        if resuming {
            "thread/resume"
        } else {
            "thread/start"
        },
        params.clone(),
        cancel,
        |v| event(store, id, v),
    );
    let mut response = match response {
        Err(error)
            if resuming
                && error
                    .downcast_ref::<RpcError>()
                    .is_some_and(RpcError::missing_thread)
                && super::permissions::legacy_empty(&session) =>
        {
            resuming = false;
            if let Some(object) = params.as_object_mut() {
                object.remove("threadId");
            }
            store.update(id, |s| {
                s.artifact_tools = true;
                s.pr_tools = true;
                s.shell_tools = true;
                s.registration_tools = true;
                s.question_tools = true;
                s.peer_tools = true;
                s.folder_tools = true;
            })?;
            let fresh = store.get(id)?;
            put(
                &mut params,
                "developerInstructions",
                json!(format!("{}\n\n{}", rpc.inherited, instructions(&fresh))),
            )?;
            let mut tools = super::orchestrator::tools(&fresh);
            if !session.native_setup && session.deferred_workspace && launch.isolated {
                tools.extend(isolation::tools().as_array().cloned().unwrap_or_default());
            }
            put(&mut params, "dynamicTools", json!(tools))?;
            rpc.call("thread/start", params, cancel, |v| event(store, id, v))?
        }
        result => result?,
    };
    if session.waiting_for_workspace() && resuming {
        ensure!(
            response.pointer("/sandbox/type").and_then(Value::as_str) == Some("readOnly"),
            "Codex did not confirm read-only access; no prompt was sent"
        );
    }
    let thread_id = response
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("Codex returned no thread identifier")?
        .to_owned();
    store.update(id, |s| {
        if s.deferred_workspace && s.inherited_permissions.is_null() {
            s.inherited_permissions = json!({"sandbox":response.get("sandbox"),"approvalPolicy":response.get("approvalPolicy"),"approvalsReviewer":response.get("approvalsReviewer")});
        }
        if resuming && !session.permission_state.codex_thread_started {s.permission_state.codex_rollout=true;}
        s.permission_state.codex_thread_started=true;
        if !resuming {s.permission_state.mode=Some("default".into());}
        if response.pointer("/thread/turns").and_then(Value::as_array).is_some_and(|turns|!turns.is_empty()) {s.permission_state.codex_rollout=true;}
        s.thread_id = Some(thread_id); s.model = response.get("model").and_then(Value::as_str).map(String::from);
        s.effort = response.get("reasoningEffort").and_then(Value::as_str).map(String::from);
        s.permissions = json!({"sandbox":response.get("sandbox"),"approvalPolicy":response.get("approvalPolicy"),"approvalsReviewer":response.get("approvalsReviewer")});
        if let Some(turns) = response.pointer("/thread/turns").and_then(Value::as_array) {
            for turn in turns {
                if let Some(items) = turn.get("items").and_then(Value::as_array) {
                    for item in items { apply_event(s, &json!({"method":"item/completed","params":{"item":item,"difuRestored":true}})); }
                }
            }
        }
        s.status = Status::Idle;
        s.error = None;
        // A new Codex connection never carries an earlier voice connection.
        s.voice = false;
    })?;
    permissions_control::record_policy(store, id, &response)?;
    permissions_control::capabilities(&mut rpc, store, id, cancel)?;
    let restored = permissions_control::restore_mode(
        &mut rpc,
        store,
        id,
        session
            .permission_state
            .pending_mode
            .as_deref()
            .or(session.permission_state.mode.as_deref()),
        &mut response,
        cancel,
    );
    if let Err(error) = restored {
        if let Some(mode) = session.permission_state.pending_mode {
            store.update(id, |s| s.permission_state.pending_mode = Some(mode))?;
        }
        return Err(error);
    }
    store.update(id, super::plan::reconcile)?;
    store.save(id)?;
    Ok((rpc, resuming))
}
fn send_queued(rpc: &mut Connection, store: &Store, id: &str, cancel: &Cancel) -> Result<()> {
    if !store.inspect(id, |s| s.status == Status::Idle && !s.queue.is_empty())? {
        return Ok(());
    }
    let mut next = None;
    store.update(id, |s| {
        if !s.queue.is_empty() {
            next = Some(s.queue.remove(0));
        }
    })?;
    store.save(id)?;
    if let Some(prompt) = next {
        let before = store.get(id)?.entries.len();
        if let Err(error) = start_turn(rpc, store, id, &prompt, false, cancel) {
            store.update(id, |s| {
                if s.entries.len() == before {
                    s.unsent(prompt);
                }
            })?;
            store.save(id)?;
            return Err(error);
        }
    }
    Ok(())
}

pub fn run(
    store: &Arc<Store>,
    id: &str,
    controls: mpsc::Receiver<AgentCommand>,
    cancel: &Cancel,
    initial: Option<Control>,
) -> Result<()> {
    let mut naming = super::title::Task::default();
    let mut suggesting = super::suggestions::Task::default();
    let mut session = store.get(id)?;
    if !session.waiting_for_workspace() {
        super::workspace::prepare(&mut session, &store.home, cancel, |prepared| {
            store.update(id, |s| {
                s.job = prepared.job.clone();
                s.workspace = prepared.workspace.clone();
                s.workspace_ready = prepared.workspace_ready;
                s.baseline = prepared.baseline.clone();
                s.branch = prepared.branch.clone();
            })?;
            store.save(id)
        })?;
        super::guidance::confirm(store, id, &session, &controls, cancel)?;
    }
    let (mut rpc, resuming) = connect(store, id, cancel)?;
    let Job::Coding(launch) = &session.job else {
        anyhow::bail!("Not a coding session");
    };
    if let Some(control) = initial {
        if resuming || !matches!(control, Control::Resume) {
            command(&mut rpc, store, id, control, cancel)?;
        }
    } else if !resuming && !launch.prompt.is_empty() {
        start_turn(
            &mut rpc,
            store,
            id,
            &Prompt::from(launch.prompt.clone()),
            false,
            cancel,
        )?;
    }
    // Send accepted setup input before processing newer controls.
    send_queued(&mut rpc, store, id, cancel)?;
    let mut housekeeping_due = true;
    loop {
        cancel.check()?;
        let mut changed = std::mem::take(&mut housekeeping_due);
        loop {
            match rpc.output.try_recv() {
                Ok(value) => {
                    event(store, id, value.map_err(anyhow::Error::msg)?)?;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("Codex disconnected"),
            }
        }
        match controls.try_recv() {
            Ok(command_request) => {
                changed = true;
                let read_only = matches!(
                    command_request.control,
                    Control::RefreshShells
                        | Control::CloseShell { .. }
                        | Control::ReadUsage
                        | Control::Mcp { .. }
                        | Control::Permissions { .. }
                );
                let result = command(&mut rpc, store, id, command_request.control, cancel);
                if let Err(error) = &result
                    && !read_only
                {
                    store.update(id, |s| s.note("error", format!("{error:#}")))?;
                }
                let _ = command_request.reply.send(result);
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if !changed {
            // Notification tokens survive the drain-to-wait race. Commands,
            // provider output (including EOF), and cancellation all wake us.
            cancel.check()?;
            cancel.wake().wait();
            housekeeping_due = true;
            continue;
        }
        let requests = store.inspect(id, |s| s.orchestrator_requests.clone())?;
        if !requests.is_empty() {
            store.update(id, |s| s.orchestrator_requests.clear())?;
            for request in requests {
                let result = (|| -> Result<Value> {
                    let session = store.get(id)?;
                    ensure!(
                        request.params.get("threadId").and_then(Value::as_str)
                            == session.thread_id.as_deref()
                            && request.params.get("turnId").and_then(Value::as_str)
                                == session.turn_id.as_deref(),
                        "Session action belongs to another turn"
                    );
                    let args = request
                        .params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args = if let Some(text) = args.as_str() {
                        serde_json::from_str(text)?
                    } else {
                        args
                    };
                    let tool = request.params.get("tool").and_then(Value::as_str);
                    if tool == Some(super::peers::TOOL) {
                        super::peers::handle(store, id, &args)
                    } else if tool == Some(super::folders::TOOL) {
                        super::folders::handle_tool(store, id, &args)
                    } else {
                        super::orchestrator::handle(store, id, &args)
                    }
                })();
                let text = match &result {
                    Ok(value) => value.to_string(),
                    Err(error) => format!("{error:#}"),
                };
                rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
            }
        }
        let requests = store.inspect(id, |s| s.deletion_requests.clone())?;
        if !requests.is_empty() {
            store.update(id, |s| s.deletion_requests.clear())?;
            let session = store.get(id)?;
            for request in requests {
                let result = (|| -> Result<()> {
                    ensure!(
                        request.params.get("threadId").and_then(Value::as_str)
                            == session.thread_id.as_deref()
                            && request.params.get("turnId").and_then(Value::as_str)
                                == session.turn_id.as_deref(),
                        "Session deletion belongs to another turn"
                    );
                    let args = request
                        .params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args = if let Some(text) = args.as_str() {
                        serde_json::from_str(text)?
                    } else {
                        args
                    };
                    super::management::validate(&session, &args)
                })();
                let text = match &result {
                    Ok(()) => "Session deletion accepted. Stop here; difu will close and remove this session.".into(),
                    Err(error) => format!("{error:#}"),
                };
                rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
                if result.is_ok() {
                    super::management::schedule(store, id)?;
                    return Ok(());
                }
            }
        }
        let requests = store.inspect(id, |s| s.question_requests.clone())?;
        if !requests.is_empty() {
            store.update(id, |s| s.question_requests.clear())?;
            for request in requests {
                let result = (|| -> Result<Value> {
                    let session = store.get(id)?;
                    ensure!(
                        request.params.get("threadId").and_then(Value::as_str)
                            == session.thread_id.as_deref()
                            && request.params.get("turnId").and_then(Value::as_str)
                                == session.turn_id.as_deref(),
                        "Question removal belongs to another turn"
                    );
                    let args = request
                        .params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args = if let Some(text) = args.as_str() {
                        serde_json::from_str(text)?
                    } else {
                        args
                    };
                    super::questions::handle(store, id, &args)
                })();
                let text = match &result {
                    Ok(value) => value.to_string(),
                    Err(error) => format!("{error:#}"),
                };
                rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
            }
        }
        let requests = store.inspect(id, |s| s.pr_requests.clone())?;
        if !requests.is_empty() {
            store.update(id, |s| s.pr_requests.clear())?;
            for request in requests {
                let result = (|| -> Result<Value> {
                    let session = store.get(id)?;
                    ensure!(
                        request.params.get("threadId").and_then(Value::as_str)
                            == session.thread_id.as_deref()
                            && request.params.get("turnId").and_then(Value::as_str)
                                == session.turn_id.as_deref(),
                        "PR attachment belongs to another turn"
                    );
                    let args = request
                        .params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args = if let Some(text) = args.as_str() {
                        serde_json::from_str(text)?
                    } else {
                        args
                    };
                    super::pr_attachments::handle(store, id, &args, cancel)
                })();
                let text = match &result {
                    Ok(value) => value.to_string(),
                    Err(error) => format!("{error:#}"),
                };
                rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
            }
        }
        let requests = store.inspect(id, |s| s.shell_requests.clone())?;
        if !requests.is_empty() {
            store.update(id, |s| s.shell_requests.clear())?;
            for request in requests {
                let result = (|| -> Result<Value> {
                    let session = store.get(id)?;
                    ensure!(
                        request.params.get("threadId").and_then(Value::as_str)
                            == session.thread_id.as_deref()
                            && request.params.get("turnId").and_then(Value::as_str)
                                == session.turn_id.as_deref(),
                        "Shell request belongs to another turn"
                    );
                    let args = request
                        .params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args = if let Some(text) = args.as_str() {
                        serde_json::from_str(text)?
                    } else {
                        args
                    };
                    super::shells::tool_call(store, id, &args)
                })();
                let text = match &result {
                    Ok(value) => value.to_string(),
                    Err(error) => format!("{error:#}"),
                };
                rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
            }
        }
        let requests = store.inspect(id, |s| s.artifact_requests.clone())?;
        if !requests.is_empty() {
            store.update(id, |s| s.artifact_requests.clear())?;
            for request in requests {
                let mut session = store.get(id)?;
                let args = request
                    .params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Null);
                let args = if let Some(text) = args.as_str() {
                    serde_json::from_str(text).unwrap_or(Value::Null)
                } else {
                    args
                };
                let result = super::artifacts::register(&mut session, &args);
                if result.is_ok() {
                    store.update(id, |s| s.artifacts = session.artifacts)?;
                    store.save(id)?;
                }
                let text = match &result {
                    Ok(()) => "Artifact registered in difu".into(),
                    Err(e) => format!("{e:#}"),
                };
                rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
            }
        }
        let requests = store.inspect(id, |s| s.workspace_requests.clone())?;
        if !requests.is_empty() {
            isolation::transition(&mut rpc, store, id, requests, &controls, cancel)?;
        }
        let requests = store.inspect(id, |s| s.registration_requests.clone())?;
        if !requests.is_empty() {
            registration::transition(&mut rpc, store, id, requests, &controls, cancel)?;
        }
        naming.start(store, id, cancel)?;
        suggesting.start(store, id, cancel)?;
        send_queued(&mut rpc, store, id, cancel)?;
    }
    Ok(())
}

fn validate_response(pending: &Pending, response: &Value) -> Result<()> {
    match pending.method.as_str() {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            ensure!(
                matches!(
                    response.get("decision").and_then(Value::as_str),
                    Some("accept" | "acceptForSession" | "decline" | "cancel")
                ),
                "Invalid approval decision"
            );
        }
        "item/tool/requestUserInput" => {
            ensure!(
                response.get("answers").is_some_and(Value::is_object),
                "Provide an answer map"
            );
        }
        "item/permissions/requestApproval" => {
            ensure!(
                response.get("permissions").is_some_and(Value::is_object),
                "Provide a permission response"
            );
        }
        "mcpServer/elicitation/request" => {
            ensure!(
                matches!(
                    response.get("action").and_then(Value::as_str),
                    Some("accept" | "decline" | "cancel")
                ),
                "Invalid MCP elicitation response"
            );
        }
        _ => {
            ensure!(
                response.is_object(),
                "Provide the structured response requested by Codex"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completing_an_agent_turn_leaves_user_shell_commands_running() {
        let mut session = Session::new(
            "one".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.turn_id = Some("turn".into());
        session.entries.push(Entry {
            id: "difu-shell-test".into(),
            kind: "commandExecution".into(),
            text: "sleep 30".into(),
            data: json!({"difuUserShell":true,"status":"inProgress"}).into(),
            started_at: Some(1),
            ..Entry::default()
        });
        apply_event(
            &mut session,
            &json!({"method":"turn/completed","params":{"turn":{"id":"turn","status":"completed"}}}),
        );
        assert!(
            session
                .entries
                .iter()
                .all(|entry| entry.finished_at.is_none())
        );
        assert!(super::super::shells::context(&session).1.is_none());
    }
    #[test]
    fn new_sessions_keep_rules_and_tools_while_old_sessions_keep_their_policy() -> Result<()> {
        let mut session = Session::new(
            "policy".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        session.repository_rules = "Use the repository service layer.".into();
        session.artifact_tools = true;
        session.pr_tools = true;
        session.shell_tools = true;
        session.registration_tools = true;
        session.question_tools = true;
        session.peer_tools = true;
        let saved = serde_json::to_value(&session)?;
        let restored: Session = serde_json::from_value(saved.clone())?;
        assert!(restored.native_setup);
        for prompt in [
            instructions(&restored),
            super::super::claude::instructions(&restored),
        ] {
            assert!(prompt.contains("Use the repository service layer."));
            assert!(
                prompt.starts_with("You are running inside difu, the user's terminal workspace")
            );
            assert!(prompt.contains("This conversation is difu session policy."));
            for tool in [
                "difu_present_artifact",
                "difu_attach_pr",
                "difu_shell",
                "difu_remove_questions",
                "difu_register_worktree",
                "difu_delete_session",
                "difu_peer_sessions",
            ] {
                assert!(prompt.contains(tool));
            }
            assert!(!prompt.contains("Do not run local tests"));
            assert!(!prompt.contains("Do not commit, push"));
            assert!(!prompt.contains("Current repository:"));
        }
        assert!(!super::super::registration::continuation(&restored).contains("PR CI"));
        assert!(!instructions(&restored).contains("difu attach-pr"));

        let mut resumed = restored.clone();
        resumed.pr_tools = false;
        resumed.shell_tools = false;
        resumed.thread_id = Some("existing-codex-thread".into());
        let prompt = instructions(&resumed);
        assert!(prompt.contains("difu attach-pr <URL> --session 'policy'"));
        assert!(!prompt.contains("difu_attach_pr"));
        assert!(!prompt.contains("Use difu_shell"));
        assert!(prompt.contains("difu shell --session 'policy'"));
        assert!(super::super::claude::instructions(&resumed).contains("difu_attach_pr"));

        let mut old = saved;
        old.as_object_mut()
            .context("session")?
            .remove("native_setup");
        let mut legacy: Session = serde_json::from_value(old)?;
        assert!(!legacy.native_setup);
        for prompt in [
            instructions(&legacy),
            super::super::claude::instructions(&legacy),
        ] {
            assert!(prompt.contains("Use the repository service layer."));
            assert!(prompt.contains(super::super::WORKTREE_INSTRUCTIONS));
        }
        assert!(super::super::registration::continuation(&legacy).contains("PR CI"));

        legacy.deferred_workspace = true;
        assert!(legacy.waiting_for_workspace());
        let mut params = json!({});
        isolation::settings(&mut params, &legacy)?;
        assert_eq!(params.get("sandbox"), Some(&json!("read-only")));
        session.deferred_workspace = true;
        assert!(!session.waiting_for_workspace());
        let mut params = json!({});
        isolation::settings(&mut params, &session)?;
        assert_eq!(params, json!({}));
        Ok(())
    }

    #[test]
    fn delayed_user_echoes_preserve_prompt_order_and_do_not_duplicate() -> Result<()> {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        for text in ["first steering message", "> Question?\nAnswer with notes"] {
            session.note("userMessage", text);
            session.entries.last_mut().context("message")?.data =
                json!({"wire_text":text, "difuSteeringTurn":"t"}).into();
        }
        session.note("agentMessage", "Continuing the task");
        session.turn_id = Some("t".into());
        session.entries.push(Entry {
            id: "tool".into(),
            kind: "commandExecution".into(),
            started_at: Some(1),
            ..Entry::default()
        });
        let echo = json!({"method":"item/completed","params":{"item":{
            "id":"first-server-id","type":"userMessage",
            "content":[{"type":"text","text":"first steering message"}]
        }}});
        apply_event(&mut session, &echo);
        apply_event(&mut session, &echo);
        assert_eq!(session.entries.len(), 4);
        assert!(
            session
                .entries
                .first()
                .context("echoed message")?
                .data
                .get("difuSteeringTurn")
                .is_none()
        );
        assert_eq!(
            session
                .entries
                .get(1)
                .context("pending message")?
                .data
                .get("difuSteeringTurn"),
            Some(&json!("t"))
        );
        assert_eq!(
            session.entries.first().map(|e| e.id.as_str()),
            Some("first-server-id")
        );
        assert_eq!(
            session
                .entries
                .iter()
                .rev()
                .find(|e| e.kind == "userMessage")
                .map(|e| e.text.as_str()),
            Some("> Question?\nAnswer with notes")
        );
        apply_event(
            &mut session,
            &json!({"method":"turn/completed","params":{"turn":{"id":"t","status":"completed"}}}),
        );
        assert_eq!(session.entries.len(), 4);
        assert!(
            session
                .entries
                .iter()
                .all(|e| e.data.get("difuSteeringTurn").is_none())
        );
        assert!(session.queue.is_empty()); // Display cleanup never resubmits accepted steering.
        Ok(())
    }

    #[test]
    fn steering_waits_for_acceptance_and_all_running_tools() -> Result<()> {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        apply_event(
            &mut session,
            &json!({"method":"turn/started","params":{"turn":{"id":"t"}}}),
        );
        for id in ["tool-a", "tool-b"] {
            apply_event(
                &mut session,
                &json!({"method":"item/started","params":{"item":{
                    "id":id,"type":"commandExecution","command":"test"
                }}}),
            );
        }
        session.note("sending", "follow up");
        session.entries.last_mut().context("message")?.data =
            json!({"difuSteeringTurn":"t"}).into();
        assert!(session.tool_running());
        assert_eq!(session.pending_steering().count(), 1);
        session.entries.last_mut().context("message")?.kind = "userMessage".into();
        session.finish_steering_wait();
        assert_eq!(session.pending_steering().count(), 1); // RPC acknowledgement alone is insufficient.
        apply_event(
            &mut session,
            &json!({"method":"item/completed","params":{"item":{
                "id":"tool-a","type":"commandExecution"
            }}}),
        );
        assert_eq!(session.pending_steering().count(), 1); // The parallel tool is still running.
        apply_event(
            &mut session,
            &json!({"method":"item/completed","params":{"item":{
                "id":"tool-b","type":"commandExecution"
            }}}),
        );
        assert!(!session.tool_running());
        assert_eq!(session.pending_steering().count(), 0);
        apply_event(
            &mut session,
            &json!({"method":"item/started","params":{"item":{
                "id":"tool-c","type":"commandExecution"
            }}}),
        );
        assert_eq!(session.pending_steering().count(), 0); // A later tool cannot queue the message again.
        session.note("sending", "delayed acknowledgement");
        session.entries.last_mut().context("message")?.data =
            json!({"difuSteeringTurn":"t"}).into();
        apply_event(
            &mut session,
            &json!({"method":"item/completed","params":{"item":{
                "id":"tool-c","type":"commandExecution"
            }}}),
        );
        assert_eq!(session.pending_steering().count(), 1); // Tool completion cannot accept a send.
        session.entries.last_mut().context("message")?.kind = "userMessage".into();
        session.finish_steering_wait();
        assert_eq!(session.pending_steering().count(), 0);
        Ok(())
    }

    #[test]
    fn async_questions_survive_completed_turns_and_restore_without_duplicate_answers() -> Result<()>
    {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "questions".into(),
                model: None,
                effort: None,
            }),
        );
        apply_event(
            &mut session,
            &json!({"method":"turn/started","params":{"turn":{"id":"t"}}}),
        );
        let item = json!({"id":"async-1","type":"agentMessage","delivery":"async","text":"Three questions", "questions":[
            {"title":"Task?","options":["Explore", "Review"]},
            {"title":"Detail?","options":["Brief","Full"]},
            {"title":"Anything else?","options":null}
        ]});
        let event = json!({"method":"item/completed","params":{"item":item}});
        apply_event(&mut session, &event);
        apply_event(&mut session, &event);
        assert_eq!(session.pending_question_count(), 3);
        assert_eq!(session.pending.len(), 1);
        assert_eq!(session.status, Status::Running);
        apply_event(
            &mut session,
            &json!({"id":42,"method":"item/tool/requestUserInput","params":{"questions":[]}}),
        );
        apply_event(
            &mut session,
            &json!({"method":"turn/completed","params":{"turn":{"id":"t","status":"completed"}}}),
        );
        assert_eq!(session.status, Status::Idle);
        assert_eq!(session.pending.len(), 1);
        let pending = session.pending.first().context("async questions")?;
        let request_id = pending.id.as_str().context("id")?.to_owned();
        let response = json!({"answers":{"0":{"answers":["Explore"]},"1":{"answers":["Full"]},"2":{"answers":["Keep my draft"]}}});
        assert_eq!(
            super::super::questions::answer_text(pending, &response)?,
            "Answers to your questions:\n\n> Task?\nExplore\n\n> Detail?\nFull\n\n> Anything else?\nKeep my draft"
        );
        assert!(super::super::questions::answer_text(pending, &json!({"answers":{}})).is_err());
        // Recover older sessions that stored the question message but no pending request.
        session.pending.clear();
        let mut restored: Session = serde_json::from_value(serde_json::to_value(&session)?)?;
        restored.restore_async_questions();
        assert_eq!(restored.pending_question_count(), 3);
        if let Some(pending) = restored.pending.first_mut() {
            pending.responded = true;
        }
        restored.restore_async_questions();
        assert!(restored.pending.first().is_some_and(|p| p.responded));
        restored.answered_questions.insert(request_id);
        restored.pending.clear();
        apply_event(&mut restored, &event);
        assert!(restored.pending.is_empty());
        Ok(())
    }
    #[test]
    fn automatic_review_events_preserve_identity_and_finish_without_user_approval() -> Result<()> {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Running;
        session.thread_id = Some("thread".into());
        session.turn_id = Some("turn".into());
        let params = json!({"reviewId":"review", "threadId":"thread", "turnId":"turn", "startedAtMs":1000, "targetItemId":"tool", "review":{"status":"inProgress"}});
        let started = json!({"method":"item/autoApprovalReview/started","params":params});
        apply_event(&mut session, &started);
        apply_event(&mut session, &started);
        assert_eq!(session.entries.len(), 1);
        assert!(session.pending.is_empty());
        assert_eq!(session.status, Status::Running);
        apply_event(
            &mut session,
            &json!({"method":"item/autoApprovalReview/completed","params":{
                "reviewId":"review", "threadId":"thread", "turnId":"turn", "startedAtMs":1000, "completedAtMs":4000, "review":{"status":"denied"}
            }}),
        );
        assert_eq!(session.entries.len(), 1);
        assert_eq!(
            session.entries.first().context("review")?.finished_at,
            Some(4000)
        );
        assert_eq!(session.thread_id.as_deref(), Some("thread"));
        assert_eq!(session.turn_id.as_deref(), Some("turn"));
        Ok(())
    }
    #[test]
    fn voice_after_a_provider_switch_carries_the_recent_conversation() {
        let short = voice_handoff("Difu conversation handoff. Fix the login bug.");
        assert!(short.starts_with("Realtime conversation started."));
        assert!(short.ends_with("Difu conversation handoff. Fix the login bug."));
        assert!(!short.contains("omitted"));
        let long = format!("Old start {}é recent end", "x".repeat(40_000));
        let bounded = voice_handoff(&long);
        assert!(bounded.len() < 26_000);
        assert!(bounded.contains("(Earlier conversation omitted.)"));
        assert!(bounded.ends_with("é recent end") && !bounded.contains("Old start"));
    }

    #[test]
    fn voice_transcripts_become_chat_messages_and_handoffs_show_the_request() {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        let realtime = |session: &mut Session, method: &str, params: Value| {
            apply_event(session, &json!({"method":method,"params":params}));
        };
        realtime(
            &mut session,
            "thread/realtime/started",
            json!({"threadId":"t","version":"v3"}),
        );
        assert!(session.voice);
        // Duplex chunks interleave; each role keeps its own message.
        for (role, delta) in [
            ("user", "Run the"),
            ("assistant", " On it"),
            ("user", " tests"),
        ] {
            realtime(
                &mut session,
                "thread/realtime/transcript/delta",
                json!({"threadId":"t","role":role,"delta":delta}),
            );
        }
        realtime(
            &mut session,
            "thread/realtime/transcript/done",
            json!({"threadId":"t","role":"user","text":" Run the tests."}),
        );
        let voice: Vec<_> = session
            .entries
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str(), e.finished_at.is_some()))
            .collect();
        assert_eq!(
            voice,
            [
                ("userMessage", "Run the tests.", true),
                ("agentMessage", "On it", false)
            ]
        );
        assert!(
            session
                .entries
                .iter()
                .all(|e| e.data.get("difuVoice").is_some())
        );
        apply_event(
            &mut session,
            &json!({"method":"item/started","params":{"item":{"id":"u1","type":"userMessage","content":[{"type":"text","text":"<realtime_delegation>\n  <input>Run the tests &amp; report</input>\n</realtime_delegation>"}]}}}),
        );
        let handoff = session.entries.iter().find(|e| e.id == "u1");
        assert_eq!(
            handoff.map(|e| e.text.as_str()),
            Some("Run the tests & report")
        );
        assert!(handoff.is_some_and(|e| e.data.get("difuVoiceHandoff").is_some()));
        // An empty final is noise; closing finishes whatever is still open.
        realtime(
            &mut session,
            "thread/realtime/transcript/delta",
            json!({"threadId":"t","role":"user","delta":"  "}),
        );
        realtime(
            &mut session,
            "thread/realtime/closed",
            json!({"threadId":"t","reason":"requested"}),
        );
        assert!(!session.voice);
        assert!(
            session
                .entries
                .iter()
                .filter(|e| e.data.get("difuVoice").is_some())
                .all(|e| e.finished_at.is_some() && !e.text.trim().is_empty())
        );
    }

    #[test]
    fn proposed_plan_streams_text_and_completed_item_is_authoritative() {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Plan only".into(),
                model: None,
                effort: None,
            }),
        );
        apply_event(
            &mut session,
            &json!({"method":"item/started","params":{"item":{"id":"p","type":"plan","text":""}}}),
        );
        apply_event(
            &mut session,
            &json!({"method":"item/plan/delta","params":{"itemId":"p","delta":"# Draft\n\n"}}),
        );
        apply_event(
            &mut session,
            &json!({"method":"item/plan/delta","params":{"itemId":"p","delta":"- Explore"}}),
        );
        let entry = session.entries.first();
        assert_eq!(entry.map(|e| e.text.as_str()), Some("# Draft\n\n- Explore"));
        assert_eq!(
            entry
                .and_then(|e| e.data.get("text"))
                .and_then(Value::as_str),
            Some("# Draft\n\n- Explore")
        );
        apply_event(
            &mut session,
            &json!({"method":"item/completed","params":{"item":{"id":"p","type":"plan","text":"# Final\n\n- Verify first"}}}),
        );
        assert_eq!(session.entries.len(), 1);
        assert_eq!(
            session.entries.first().map(|e| e.text.as_str()),
            Some("# Final\n\n- Verify first")
        );
        assert!(
            session
                .entries
                .first()
                .is_some_and(|e| e.finished_at.is_some())
        );
    }

    #[test]
    fn approval_and_streaming_lifecycle() {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        apply_event(
            &mut session,
            &json!({"method":"turn/started","params":{"turn":{"id":"t"}}}),
        );
        apply_event(
            &mut session,
            &json!({"method":"item/started","params":{"item":{"id":"i","type":"agentMessage","text":""}}}),
        );
        apply_event(
            &mut session,
            &json!({"method":"item/agentMessage/delta","params":{"itemId":"i","delta":"Hello"}}),
        );
        assert_eq!(
            session.entries.first().map(|e| e.text.as_str()),
            Some("Hello")
        );
        apply_event(
            &mut session,
            &json!({"id":10,"method":"item/commandExecution/requestApproval","params":{"command":"git status"}}),
        );
        assert_eq!(session.status, Status::Waiting);
        assert_eq!(session.pending.len(), 1);
        apply_event(
            &mut session,
            &json!({"method":"serverRequest/resolved","params":{"requestId":10}}),
        );
        assert_eq!(session.status, Status::Running);
        session.queue.push("queued task".into());
        apply_event(
            &mut session,
            &json!({"method":"turn/completed","params":{"turn":{"status":"interrupted"}}}),
        );
        assert_eq!(session.status, Status::Interrupted);
        assert!(session.queue.is_empty());
        assert!(
            session
                .entries
                .iter()
                .any(|e| e.kind == "unsent" && e.text == "queued task")
        );
    }
    #[test]
    fn settings_notifications_match_thread_and_do_not_clear_a_newer_mode_target() -> Result<()> {
        let mut session = Session::new(
            "settings".into(),
            Job::Coding(super::super::Launch {
                repository: "/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.thread_id = Some("current".into());
        session.permission_state.mode = Some("default".into());
        session.permission_state.pending_mode = Some("default".into());
        apply_event(
            &mut session,
            &json!({"method":"thread/settings/updated","params":{"threadId":"current","threadSettings":{"collaborationMode":{"mode":"plan"}}}}),
        );
        assert_eq!(session.permission_state.mode.as_deref(), Some("default"));
        assert_eq!(
            session.permission_state.pending_mode.as_deref(),
            Some("default")
        );
        apply_event(
            &mut session,
            &json!({"method":"thread/settings/updated","params":{"threadId":"other","threadSettings":{"collaborationMode":{"mode":"default"}}}}),
        );
        assert!(session.permission_state.pending_mode.is_some());
        apply_event(
            &mut session,
            &json!({"method":"thread/settings/updated","params":{"threadId":"current","threadSettings":{"collaborationMode":{"mode":"default"}}}}),
        );
        assert!(session.permission_state.pending_mode.is_none());
        Ok(())
    }
}
