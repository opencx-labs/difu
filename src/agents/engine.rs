mod isolation;
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
}
impl Connection {
    fn open(cwd: &Path) -> Result<Self> {
        let mut child = ChildGroup::spawn(
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
        let (sender, receiver) = mpsc::channel();
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
                        anyhow::bail!("Codex {method}: {error}");
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
    let mut rpc = Connection::open(cwd)?;
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
    let mut rpc = Connection::open(&cwd)?;
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
        "agentMessage" => string(item, "text"),
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
    if let Some(id) = event.get("id") {
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

        if session.deferred_workspace
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
            session.pending.push(Pending {
                id: id.clone(),
                method: method.into(),
                params: params.clone(),
                responded: false,
            });
        }
        session.status = Status::Waiting;
        return;
    }
    match method {
        "turn/started" => {
            session.turn_started_at = Some(chrono::Utc::now().timestamp_millis());
            session.turn_id = params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(String::from);
            session.status = Status::Running;
        }
        "turn/completed" => {
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
                if entry.started_at.is_some() && entry.finished_at.is_none() {
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
                session.title_response = entry.text.clone();
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
                entry.data = params.clone();
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
                    data: params.clone(),
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
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item") {
                let id = string(item, "id");
                let mut kind = string(item, "type");
                let mut text = item_text(item);
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
                    text = entry.text.clone();
                }
                if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                    let user_message = kind == "userMessage";
                    if entry.kind != "system" {
                        entry.kind = kind;
                    }
                    if !user_message {
                        entry.text = text;
                    }
                    entry.data = item.clone();
                    if method == "item/completed" {
                        entry.finished_at = Some(chrono::Utc::now().timestamp_millis());
                    }
                } else {
                    session.entries.push(Entry {
                        id,
                        kind,
                        text,
                        data: item.clone(),
                        started_at: Some(chrono::Utc::now().timestamp_millis()),
                        finished_at: (method == "item/completed")
                            .then(|| chrono::Utc::now().timestamp_millis()),
                    });
                }
            }
        }
        "item/agentMessage/delta"
        | "item/commandExecution/outputDelta"
        | "item/reasoning/summaryTextDelta" => {
            let id = string(params, "itemId");
            let delta = string(params, "delta");
            if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
                entry.text.push_str(&delta);
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
                entry.data = params.clone();
            } else {
                session.entries.push(Entry {
                    id,
                    kind: "plan".into(),
                    data: params.clone(),
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
}
fn event(store: &Store, id: &str, value: Value) -> Result<()> {
    store.update(id, |s| apply_event(s, &value))
}

pub fn skills(cwd: &Path, force: bool) -> Result<Reply> {
    let cancel = Cancel::default();
    let mut rpc = Connection::open(cwd)?;
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
    send_turn(rpc, store, id, prompt, steer, cancel, false)
}
fn send_turn(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    prompt: &Prompt,
    steer: bool,
    cancel: &Cancel,
    internal: bool,
) -> Result<()> {
    let text = prompt.text();
    let session = store.get(id)?;
    let thread = session
        .thread_id
        .as_ref()
        .context("Codex session is not connected")?;
    let mut input = vec![json!({"type":"text","text":text})];
    if let Some(context) = &session.provider_context {
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
        "turn/start"
    };
    // Store intent before sending: a lost reply must never trigger automatic replay.
    let mut sending_id = None;
    store.update(id, |s| {
        if !steer {
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
            text,
        );
        if let Some(entry) = s.entries.last_mut() {
            sending_id = Some(entry.id.clone());
            entry.data = json!({"wire_text":wire_text,"attachments":prompt.attachments(),"prompt":prompt,
                "difuSteeringTurn":if steer && !internal { session.turn_id.clone() } else { None }});
        }
    })?;
    store.save(id)?;
    let result = rpc.call(method, params, cancel, |v| event(store, id, v));
    match result {
        Ok(response) => {
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
                    .find(|e| e.kind == "sending" && e.text == text)
                {
                    entry.kind = "unsent or unacknowledged".into();
                } else {
                    s.note("unsent or unacknowledged", text);
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
    let mut params = json!({"threadId":session.thread_id,"cwd":session.workspace,"developerInstructions":format!("{}\n\n{}", rpc.inherited, instructions(session))});
    if let Some(model) = &session.model {
        put(
            &mut params,
            "model",
            json!(super::provider::Provider::native_model(model)),
        )?;
    }
    isolation::settings(&mut params, session)?;
    rpc.call("thread/resume", params, cancel, |v| event(store, id, v))?;
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
        Control::ReadUsage => {
            let usage = usage_with(rpc, &session, cancel, |v| event(store, id, v))?;
            store.update(id, |s| s.usage = usage)?;
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
                        *item = prompt;
                    }
                } else {
                    s.queue.remove(index);
                }
            })?;
            store.save(id)?;
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
                    store.update(id, |s| {
                        if let Some(pending) = s.pending.iter_mut().find(|p| p.id == request) {
                            pending.responded = true;
                        }
                    })?;
                    store.save(id)?;
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
                updated.responded = true;
                super::questions::save_answer(store, id, updated.clone())?;
                rpc.write(
                    json!({"id":request,"result":{"answers":updated.params.get("difuAnswers")}}),
                )?;
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
                !pending.responded,
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
                // Persist intent before sending; never replay answers after an uncertain reply.
                store.update(id, |s| {
                    if let Some(pending) = s.pending.iter_mut().find(|p| p.id == request) {
                        pending.responded = true;
                    }
                })?;
                store.save(id)?;
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
            let mut params = json!({"threadId":session.thread_id,"cwd":session.workspace,"developerInstructions":format!("{}\n\n{}", rpc.inherited, instructions(&session))});
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
            isolation::settings(&mut params, &session)?;
            let result = rpc.call("thread/resume", params, cancel, |v| event(store, id, v))?;
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
fn instructions(session: &Session) -> String {
    let base = if session.waiting_for_workspace() {
        isolation::INSTRUCTIONS
    } else if matches!(&session.job, Job::Coding(s) if s.isolated) {
        super::WORKTREE_INSTRUCTIONS
    } else {
        super::CODING_INSTRUCTIONS
    };
    let base = format!(
        "{base}\n\nCurrent repository: {}. Read instructions in the current repository before continuing; earlier conversation may refer to a different repository.",
        session.job.root().display()
    );
    let base = if session.registration_tools {
        format!("{base}\n\n{}", super::registration::INSTRUCTIONS)
    } else {
        base
    };
    if session.artifact_tools {
        format!("{base}\n\n{}", super::artifacts::INSTRUCTIONS)
    } else {
        base
    }
}
fn connect(store: &Store, id: &str, cancel: &Cancel) -> Result<(Connection, bool)> {
    if store.get(id)?.thread_id.is_none() {
        store.update(id, |s| {
            s.artifact_tools = true;
            s.registration_tools = true;
        })?;
    }
    let session = store.get(id)?;
    let cwd = session
        .workspace
        .as_deref()
        .context("Missing session workspace")?;
    let mut rpc = Connection::open(cwd)?;
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
    let resuming = session.thread_id.is_some();
    let mut params = json!({"cwd":cwd,"runtimeWorkspaceRoots":[cwd],"developerInstructions":format!("{}\n\n{}", rpc.inherited, instructions(&session))});
    if let Some(thread) = &session.thread_id {
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
        let mut tools = vec![super::artifacts::tool(), super::registration::tool()];
        if session.deferred_workspace && launch.isolated {
            tools.extend(isolation::tools().as_array().cloned().unwrap_or_default());
        }
        put(&mut params, "dynamicTools", json!(tools))?;
    }
    if resuming {
        isolation::settings(&mut params, &session)?;
    }
    let response = rpc.call(
        if resuming {
            "thread/resume"
        } else {
            "thread/start"
        },
        params,
        cancel,
        |v| event(store, id, v),
    )?;
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
        s.thread_id = Some(thread_id); s.model = response.get("model").and_then(Value::as_str).map(String::from);
        s.effort = response.get("reasoningEffort").and_then(Value::as_str).map(String::from);
        s.permissions = json!({"sandbox":response.get("sandbox"),"approvalPolicy":response.get("approvalPolicy"),"approvalsReviewer":response.get("approvalsReviewer")});
        if let Some(turns) = response.pointer("/thread/turns").and_then(Value::as_array) {
            for turn in turns {
                if let Some(items) = turn.get("items").and_then(Value::as_array) {
                    for item in items { apply_event(s, &json!({"method":"item/completed","params":{"item":item}})); }
                }
            }
        }
        s.status = Status::Idle;
    })?;
    store.save(id)?;
    Ok((rpc, resuming))
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
    loop {
        cancel.check()?;
        while let Ok(value) = rpc.output.try_recv() {
            event(store, id, value.map_err(anyhow::Error::msg)?)?;
        }
        match controls.recv_timeout(Duration::from_millis(40)) {
            Ok(command_request) => {
                let read_only = matches!(
                    command_request.control,
                    Control::RefreshShells | Control::ReadUsage
                );
                let result = command(&mut rpc, store, id, command_request.control, cancel);
                if let Err(error) = &result
                    && !read_only
                {
                    store.update(id, |s| s.note("error", format!("{error:#}")))?;
                }
                let _ = command_request.reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let requests = store.get(id)?.artifact_requests;
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
        let requests = store.get(id)?.workspace_requests;
        if !requests.is_empty() {
            isolation::transition(&mut rpc, store, id, requests, &controls, cancel)?;
        }
        let requests = store.get(id)?.registration_requests;
        if !requests.is_empty() {
            registration::transition(&mut rpc, store, id, requests, &controls, cancel)?;
        }
        naming.start(store, id, cancel)?;
        suggesting.start(store, id, cancel)?;
        let current = store.get(id)?;
        if current.status == Status::Idle && !current.queue.is_empty() {
            let mut next = None;
            store.update(id, |s| {
                if !s.queue.is_empty() {
                    next = Some(s.queue.remove(0));
                }
            })?;
            store.save(id)?;
            if let Some(text) = next {
                start_turn(&mut rpc, store, id, &text, false, cancel)?;
            }
        }
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
                json!({"wire_text":text, "difuSteeringTurn":"t"});
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
        session.entries.last_mut().context("message")?.data = json!({"difuSteeringTurn":"t"});
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
        session.entries.last_mut().context("message")?.data = json!({"difuSteeringTurn":"t"});
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
}
