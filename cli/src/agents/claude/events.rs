//! Translate Claude stream frames into difu's durable transcript.
use super::super::permissions::{self, PermissionEventKind};
use super::super::{Entry, Session, Status};
use serde_json::{Value, json};
use std::collections::HashMap;

struct Interruption {
    turn: Option<String>,
    send_next: bool,
    requests: Vec<(Value, Option<String>)>,
    native_cancel: bool,
}
const PERMISSION_CANCEL_DIAGNOSTIC: &str =
    "[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use";

#[derive(Default)]
pub(super) struct Decoder {
    message: String,
    blocks: HashMap<u64, String>,
    input: HashMap<String, String>,
    state: Option<String>,
    result_received: bool,
    result_outcome: Option<(Option<String>, bool)>,
    interruption: Option<Interruption>,
}
fn text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        text.into()
    } else if let Some(blocks) = value.as_array() {
        blocks
            .iter()
            .filter_map(|v| v.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        value.to_string()
    }
}
fn kind(block: &Value) -> &'static str {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => "agentMessage",
        Some("thinking") => "reasoning",
        Some("tool_use") => "mcpToolCall",
        _ => "system",
    }
}
fn block_entry(
    session: &mut Session,
    message: &str,
    index: u64,
    block: &Value,
    complete: bool,
) -> String {
    let id = block
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("claude-tool-{id}"))
        .unwrap_or_else(|| format!("claude-{message}-{index}"));
    let entry_kind = kind(block);
    let content = match entry_kind {
        "agentMessage" => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        "reasoning" => block
            .get("thinking")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        _ => block.get("input").map(Value::to_string).unwrap_or_default(),
    };
    let data = if entry_kind == "mcpToolCall" {
        json!({"server":"Claude Code","tool":block.get("name"),"arguments":block.get("input"),"status":"inProgress"})
    } else {
        block.clone()
    };
    let now = chrono::Utc::now().timestamp_millis();
    if let Some(entry) = session.entries.iter_mut().find(|entry| entry.id == id) {
        // A full assistant frame reconciles deltas; it does not finish a tool call.
        if entry.finished_at.is_none() || entry_kind != "mcpToolCall" {
            entry.text = content.into();
            entry.replace_data(data);
        }
        if complete && entry_kind != "mcpToolCall" {
            entry.finished_at = Some(now);
        }
    } else {
        session.entries.push(Entry {
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            id: id.clone(),
            kind: entry_kind.into(),
            text: content.into(),
            data: data.into(),
            started_at: Some(now),
            finished_at: (complete && entry_kind != "mcpToolCall").then_some(now),
        });
    }
    id
}
impl Decoder {
    pub fn interrupting(&mut self, session: &Session, send_next: bool) {
        let requests = session
            .pending
            .iter()
            .filter(|p| p.params.get("claudeInput").is_some())
            .map(|p| {
                (
                    p.id.clone(),
                    p.params
                        .get("tool")
                        .and_then(Value::as_str)
                        .map(String::from),
                )
            })
            .collect::<Vec<_>>();
        if let Some(current) = &mut self.interruption
            && current.turn == session.turn_id
        {
            current.send_next = send_next;
            for request in requests {
                if !current.requests.iter().any(|(id, _)| id == &request.0) {
                    current.requests.push(request);
                }
            }
        } else if session.turn_id.is_some() || !requests.is_empty() {
            self.interruption = Some(Interruption {
                turn: session.turn_id.clone(),
                send_next,
                requests,
                native_cancel: false,
            });
        }
    }
    pub fn sending(&mut self) {
        self.result_received = false;
        self.result_outcome = None;
    }
    fn finish(&self, session: &mut Session) {
        if !self.result_received
            || self.state.as_deref().is_some_and(|state| state != "idle")
            || session.entries.iter().any(|entry| entry.kind == "sending")
        {
            return;
        }
        if !matches!(session.status, Status::Interrupted | Status::Failed) {
            session.status = Status::Idle;
        }
        if let Some(turn) = session.turn_id.take() {
            session.completed_turn_successful = self
                .result_outcome
                .as_ref()
                .is_some_and(|(id, success)| *success && id.as_deref() == Some(&turn));
            session.completed_turn = Some(turn);
        }
        session
            .pending
            .retain(super::super::Pending::is_async_question);
        let now = chrono::Utc::now().timestamp_millis();
        for entry in &mut session.entries {
            if entry.id.starts_with("claude-") && entry.finished_at.is_none() {
                entry.finished_at = Some(now);
                if entry.is_tool()
                    && let Some(data) = entry.data.as_object_mut()
                {
                    data.insert("status".into(), json!("interrupted"));
                }
            }
        }
        session.finish_steering_wait();
    }
    pub fn apply(&mut self, session: &mut Session, frame: &Value) {
        let now = chrono::Utc::now().timestamp_millis();
        if frame.get("parent_tool_use_id").is_none_or(Value::is_null)
            && matches!(
                frame.get("type").and_then(Value::as_str),
                Some("user" | "assistant" | "result")
            )
        {
            session.permission_state.claude_turn_acknowledged = true;
        }
        if frame.get("parent_tool_use_id").is_none_or(Value::is_null)
            && let Some(id) = frame
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        {
            session.thread_id = Some(id.into());
        }
        if let Some(mode) = frame
            .get("permissionMode")
            .or_else(|| frame.get("permission_mode"))
            .and_then(Value::as_str)
        {
            session.permission_state.mode = Some(mode.into());
            session.permissions = json!({"provider":"Claude Code","permissionMode":mode});
            if mode == "bypassPermissions" {
                session.permission_state.bypass_enabled = true;
            }
            if mode == "auto" {
                super::super::permissions::set_capability(
                    &mut session.permission_state,
                    "auto",
                    json!(true),
                );
            }
        }
        match frame.get("type").and_then(Value::as_str) {
            Some("system") if frame.get("subtype").and_then(Value::as_str) == Some("init") => {
                if let Some(model) = frame.get("model").and_then(Value::as_str) {
                    session.model = Some(format!("claude/{model}"));
                }
                session.permissions =
                    json!({"provider":"Claude Code","permissionMode":frame.get("permissionMode")});
            }
            Some("system")
                if frame.get("subtype").and_then(Value::as_str)
                    == Some("session_state_changed") =>
            {
                self.state = frame.get("state").and_then(Value::as_str).map(String::from);
                if self.state.as_deref() == Some("running")
                    && session
                        .pending
                        .iter()
                        .all(super::super::Pending::is_async_question)
                {
                    session.status = Status::Running;
                }
                self.finish(session);
            }
            Some("stream_event") if frame.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                let event = frame.get("event").unwrap_or(&Value::Null);
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                match event.get("type").and_then(Value::as_str) {
                    Some("message_start") => {
                        self.message = event
                            .pointer("/message/id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                        self.blocks.clear();
                        self.input.clear();
                        self.result_received = false;
                        self.result_outcome = None;
                        if session.turn_id.is_none() {
                            session.turn_id = Some(self.message.clone());
                            session.turn_started_at = Some(now);
                        }
                        if session.status != Status::Waiting {
                            session.status = Status::Running;
                        }
                    }
                    Some("content_block_start") => {
                        if let Some(block) = event.get("content_block") {
                            let id = block_entry(session, &self.message, index, block, false);
                            self.blocks.insert(index, id);
                        }
                    }
                    Some("content_block_delta") => {
                        if let Some(id) = self.blocks.get(&index)
                            && let Some(entry) =
                                session.entries.iter_mut().find(|entry| &entry.id == id)
                        {
                            let delta = event.get("delta").unwrap_or(&Value::Null);
                            if let Some(text) = delta
                                .get("text")
                                .or_else(|| delta.get("thinking"))
                                .and_then(Value::as_str)
                            {
                                entry.text.push_str(text);
                            }
                            if let Some(partial) = delta.get("partial_json").and_then(Value::as_str)
                            {
                                let input = self.input.entry(id.clone()).or_default();
                                input.push_str(partial);
                                if let Ok(arguments) = serde_json::from_str::<Value>(input) {
                                    entry.text = arguments.to_string().into();
                                    if let Some(data) = entry.data.as_object_mut() {
                                        data.insert("arguments".into(), arguments);
                                    }
                                }
                            }
                        }
                    }
                    Some("content_block_stop") => {
                        if let Some(id) = self.blocks.get(&index)
                            && let Some(entry) =
                                session.entries.iter_mut().find(|entry| &entry.id == id)
                            && !entry.is_tool()
                        {
                            entry.finished_at = Some(now);
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                if frame.get("parent_tool_use_id").is_none_or(Value::is_null) {
                    self.result_received = false;
                    self.result_outcome = None;
                }
                let message = frame.get("message").unwrap_or(&Value::Null);
                let message_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    for (index, block) in blocks.iter().enumerate() {
                        block_entry(session, message_id, index as u64, block, true);
                    }
                }
            }
            Some("user") => {
                let uuid = frame.get("uuid").and_then(Value::as_str);
                if let Some(entry) = session
                    .entries
                    .iter_mut()
                    .find(|entry| Some(entry.id.as_str()) == uuid && entry.kind == "sending")
                {
                    entry.kind = "userMessage".into();
                    entry.finished_at = Some(now);
                    session.provider_context = None;
                }
                if let Some(blocks) = frame.pointer("/message/content").and_then(Value::as_array) {
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                            continue;
                        }
                        let Some(tool) = block.get("tool_use_id").and_then(Value::as_str) else {
                            continue;
                        };
                        // Deferred questions decline the tool call, but nothing failed.
                        let request = super::question_request(tool);
                        let deferred = session.answered_questions.contains(&request)
                            || session.pending.iter().any(|p| p.id == request.as_str());
                        if let Some(entry) = session
                            .entries
                            .iter_mut()
                            .find(|entry| entry.id == format!("claude-tool-{tool}"))
                        {
                            entry.finished_at = Some(now);
                            let failed = !deferred
                                && block.get("is_error").and_then(Value::as_bool) == Some(true);
                            if let Some(data) = entry.data.as_object_mut() {
                                data.insert(
                                    "status".into(),
                                    json!(if failed { "failed" } else { "completed" }),
                                );
                                data.insert(
                                    "result".into(),
                                    block.get("content").cloned().unwrap_or(Value::Null),
                                );
                            }
                            entry.text = format!(
                                "{}\n{}",
                                entry.text,
                                text(block.get("content").unwrap_or(&Value::Null))
                            )
                            .into();
                        }
                    }
                }
                session.finish_steering_wait();
            }
            Some("result") if frame.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                let failed = frame.get("is_error").and_then(Value::as_bool) == Some(true);
                let interruption = self.interruption.take();
                let cancelled = interruption.as_ref().is_some_and(|intent| {
                    intent.turn == session.turn_id
                        && (!failed
                            || (intent.native_cancel
                                && frame.get("subtype").and_then(Value::as_str)
                                    == Some("error_during_execution")
                                && frame.get("errors").and_then(Value::as_array).is_some_and(
                                    |errors| {
                                        !errors.is_empty()
                                            && errors.iter().all(|error| {
                                                error.as_str() == Some(PERMISSION_CANCEL_DIAGNOSTIC)
                                            })
                                    },
                                )))
                });
                if let Some(denials) = frame.get("permission_denials").and_then(Value::as_array) {
                    for denial in denials {
                        session
                            .permission_state
                            .denials
                            .push(super::super::permissions::Denial {
                                tool: denial
                                    .get("tool_name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .into(),
                                detail: denial
                                    .get("tool_input")
                                    .map(Value::to_string)
                                    .unwrap_or_default(),
                                reason: denial
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .map(String::from),
                            });
                    }
                    let excess = session.permission_state.denials.len().saturating_sub(100);
                    session.permission_state.denials.drain(..excess);
                }
                self.result_received = true;
                self.result_outcome = Some((session.turn_id.clone(), !failed && !cancelled));
                if cancelled {
                    if let Some(intent) = interruption {
                        if failed {
                            session
                                .permission_state
                                .cancellation_diagnostics
                                .push(PERMISSION_CANCEL_DIAGNOSTIC.into());
                            let excess = session
                                .permission_state
                                .cancellation_diagnostics
                                .len()
                                .saturating_sub(16);
                            session
                                .permission_state
                                .cancellation_diagnostics
                                .drain(..excess);
                        }
                        if intent.requests.is_empty() {
                            permissions::audit(
                                &mut session.permission_state,
                                PermissionEventKind::ResultCancelled,
                                None,
                                None,
                                Some("result"),
                            );
                        }
                        for (request, tool) in intent.requests {
                            permissions::audit(
                                &mut session.permission_state,
                                PermissionEventKind::ResultCancelled,
                                Some(&request),
                                tool.as_deref(),
                                Some("result"),
                            );
                        }
                        session.error = None;
                        session.status = if intent.send_next {
                            Status::Idle
                        } else {
                            Status::Interrupted
                        };
                        self.state = Some("idle".into());
                    }
                } else if failed {
                    let error = frame
                        .get("errors")
                        .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
                        .or_else(|| frame.get("result"))
                        .or_else(|| frame.get("subtype"))
                        .map(Value::to_string)
                        .unwrap_or_else(|| "Claude failed".into());
                    session.error = Some(error.clone());
                    session.note("error", error);
                    session.status = Status::Failed;
                    permissions::audit(
                        &mut session.permission_state,
                        PermissionEventKind::ResultFailed,
                        None,
                        None,
                        Some("result"),
                    );
                    self.state = Some("idle".into());
                    for entry in &mut session.entries {
                        if entry.kind == "sending" {
                            entry.kind = "unsent or unacknowledged".into();
                        }
                    }
                }
                session.token_usage = frame.get("usage").cloned().unwrap_or(Value::Null);
                self.finish(session);
            }

            Some("control_cancel_request") => {
                let pending = session
                    .pending
                    .iter()
                    .find(|p| Some(&p.id) == frame.get("request_id"))
                    .cloned();
                permissions::audit(
                    &mut session.permission_state,
                    PermissionEventKind::NativeCancel,
                    frame.get("request_id"),
                    pending
                        .as_ref()
                        .and_then(|p| p.params.get("tool"))
                        .and_then(Value::as_str),
                    Some("control_cancel_request"),
                );
                let mut stopping = false;
                if let Some(intent) = &mut self.interruption
                    && intent
                        .requests
                        .iter()
                        .any(|(id, _)| Some(id) == frame.get("request_id"))
                {
                    intent.native_cancel = true;
                    stopping = !intent.send_next;
                }
                session
                    .pending
                    .retain(|p| Some(&p.id) != frame.get("request_id"));
                if session
                    .pending
                    .iter()
                    .all(super::super::Pending::is_async_question)
                    && session.turn_id.is_some()
                    && !stopping
                {
                    session.status = Status::Running;
                }
            }
            _ => {}
        }
        session.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut session = Session::new(
            "claude".into(),
            super::super::super::Job::Coding(super::super::super::Launch {
                repository: "/repo".into(),
                base: "HEAD".into(),
                isolated: true,
                prompt: String::new(),
                model: Some("claude/sonnet".into()),
                effort: None,
            }),
        );
        session.status = Status::Running;
        session.turn_id = Some("turn".into());
        session
    }
    #[test]
    fn notifications_from_real_claude_reducer_exclude_both_cancellation_paths() -> anyhow::Result<()>
    {
        use crate::agents::{
            notifications::{Capabilities, Command, Delivery, Dispatcher, Event, Kind},
            server::Store,
        };
        struct Boundary;
        impl Delivery for Boundary {
            fn status(&mut self) -> anyhow::Result<Capabilities> {
                Ok(Capabilities::default())
            }
            fn authorize(&mut self) -> anyhow::Result<Capabilities> {
                anyhow::bail!("Unexpected authorization")
            }
            fn desktop(&mut self, _: &Event) -> anyhow::Result<()> {
                anyhow::bail!("Unexpected desktop delivery")
            }
            fn sound(&mut self) -> anyhow::Result<()> {
                anyhow::bail!("Unexpected audio")
            }
        }
        let temp = tempfile::tempdir()?;
        let mut store = Store::fixture(temp.path().into(), vec![session()]);
        store.notifications = Some(Dispatcher::start(store.storage.clone(), Boundary)?);
        let dispatcher = store
            .notifications
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("dispatcher"))?;
        let first = dispatcher.request(Command::Poll {
            client: "test".into(),
            focused: false,
            viewed: None,
            cursor: None,
        })?;
        let mut decoder = Decoder::default();
        for (turn, failed, interrupted) in [
            ("success", false, None),
            ("failed", true, None),
            ("stopped", false, Some(false)),
            ("replace", false, Some(true)),
            ("success-later", false, None),
        ] {
            decoder.sending();
            store.update("claude", |s| {
                s.turn_id = Some(turn.into());
                decoder.apply(
                    s,
                    &json!({"type":"system","subtype":"session_state_changed","state":"running"}),
                );
            })?;
            if let Some(send_next) = interrupted {
                store.inspect("claude", |s| decoder.interrupting(s, send_next))?;
            }
            store.update("claude",|s|decoder.apply(s,&json!({"type":"result","subtype":if failed{"error_during_execution"}else{"success"},"is_error":failed,"errors":if failed{vec!["private error"]}else{vec![]},"usage":{}})))?;
            store.update("claude", |s| {
                decoder.apply(
                    s,
                    &json!({"type":"system","subtype":"session_state_changed","state":"idle"}),
                )
            })?;
        }
        dispatcher.request(Command::Status)?;
        let snapshot = dispatcher.request(Command::Poll {
            client: "test".into(),
            focused: false,
            viewed: None,
            cursor: first.cursor,
        })?;
        assert_eq!(
            snapshot.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![Kind::Completion, Kind::Error, Kind::Completion]
        );
        Ok(())
    }
    #[test]
    fn tool_completion_releases_steering_and_native_idle_finishes_coalesced_messages() {
        let mut session = session();
        let mut decoder = Decoder::default();
        decoder.apply(
            &mut session,
            &json!({"type":"system","subtype":"session_state_changed","state":"running"}),
        );
        decoder.apply(&mut session, &json!({"type":"assistant","message":{"id":"a","content":[{"type":"tool_use","id":"tool","name":"Bash","input":{"command":"pwd"}}]}}));
        assert!(session.tool_running());
        for id in ["first", "second"] {
            session.entries.push(Entry {
                id: id.into(),
                kind: "sending".into(),
                text: id.into(),
                data: json!({"difuSteeringTurn":"turn"}).into(),
                ..Entry::default()
            });
            decoder.sending();
            decoder.apply(
                &mut session,
                &json!({"type":"user","uuid":id,"message":{"content":[]}}),
            );
        }
        assert_eq!(session.pending_steering().count(), 2);
        decoder.apply(&mut session, &json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool","content":"/repo"}]}}));
        assert!(!session.tool_running());
        assert_eq!(session.pending_steering().count(), 0);
        decoder.apply(&mut session, &json!({"type":"result","is_error":false}));
        assert_eq!(session.status, Status::Running);
        decoder.apply(
            &mut session,
            &json!({"type":"system","subtype":"session_state_changed","state":"idle"}),
        );
        assert_eq!(session.status, Status::Idle);
        assert!(session.turn_id.is_none());
        assert_eq!(
            session
                .entries
                .iter()
                .filter(|e| e.kind == "userMessage")
                .count(),
            2
        );
    }
    #[test]
    fn full_messages_reconcile_streaming_and_errors_preserve_uncertain_sends() {
        let mut session = session();
        let mut decoder = Decoder::default();
        decoder.apply(
            &mut session,
            &json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"a"}}}),
        );
        decoder.apply(&mut session, &json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}));
        decoder.apply(&mut session, &json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"text":"Hello"}}}));
        decoder.apply(&mut session, &json!({"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"Hello"}]}}));
        assert_eq!(
            session.entries.iter().filter(|e| e.text == "Hello").count(),
            1
        );
        session.note("sending", "Keep this draft");
        decoder.apply(
            &mut session,
            &json!({"type":"result","is_error":true,"errors":["Connection lost"]}),
        );
        assert_eq!(session.status, Status::Failed);
        assert!(session.turn_id.is_none());
        assert!(
            session
                .entries
                .iter()
                .any(|e| e.kind == "unsent or unacknowledged" && e.text == "Keep this draft")
        );
    }
    #[test]
    fn native_mode_notifications_and_result_denials_are_observed() {
        let mut session = session();
        let mut decoder = Decoder::default();
        decoder.apply(&mut session,&json!({"type":"system","subtype":"session_state_changed","permissionMode":"plan","state":"running"}));
        assert_eq!(session.permission_state.mode.as_deref(), Some("plan"));
        decoder.apply(&mut session,&json!({"type":"system","subtype":"session_state_changed","permissionMode":"bypassPermissions","state":"running"}));
        assert!(session.permission_state.bypass_enabled);
        decoder.apply(&mut session,&json!({"type":"result","is_error":false,"permission_denials":[{"tool_name":"Bash","tool_input":{"command":"restricted"},"reason":"Native policy denied"}]}));
        assert_eq!(session.permission_state.denials.len(), 1);
        assert!(session.permission_state.denials.first().is_some_and(
            |d| d.tool == "Bash" && d.reason.as_deref() == Some("Native policy denied")
        ));
    }
    #[test]
    fn deferred_questions_neither_fail_their_tool_call_nor_block_the_turn() {
        let mut session = session();
        session.status = Status::Idle;
        let mut decoder = Decoder::default();
        decoder.apply(
            &mut session,
            &json!({"type":"assistant","message":{"id":"a","content":[
                {"type":"tool_use","id":"ask","name":"AskUserQuestion","input":{}},
                {"type":"tool_use","id":"bash","name":"Bash","input":{"command":"false"}}
            ]}}),
        );
        session.pending.push(super::super::super::Pending {
            id: json!(super::super::question_request("ask")),
            method: "item/tool/requestUserInput".into(),
            params: json!({"difuAsync":true,"questions":[{"id":"0","question":"Scope?"}]}),
            responded: false,
        });
        decoder.apply(
            &mut session,
            &json!({"type":"system","subtype":"session_state_changed","state":"running"}),
        );
        assert_eq!(session.status, Status::Running);
        decoder.apply(&mut session, &json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"ask","content":"Answered later","is_error":true},
            {"type":"tool_result","tool_use_id":"bash","content":"exit 1","is_error":true}
        ]}}));
        let status = |id: &str| {
            session
                .entries
                .iter()
                .find(|e| e.id == format!("claude-tool-{id}"))
                .and_then(|e| e.data.get("status").cloned())
        };
        assert_eq!(status("ask"), Some(json!("completed")));
        assert_eq!(status("bash"), Some(json!("failed")));
    }
    #[test]
    fn permission_cancel_diagnostic_requires_explicit_matching_interruption() {
        for explicit in [false, true] {
            let mut session = session();
            let mut decoder = Decoder::default();
            if explicit {
                decoder.interrupting(&session, false);
            }
            decoder.apply(&mut session,&json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":[PERMISSION_CANCEL_DIAGNOSTIC]}));
            assert_eq!(session.status, Status::Failed);
            assert!(session.error.is_some());
        }
        let mut session = session();
        let mut decoder = Decoder::default();
        session.pending.push(super::super::super::Pending {
            id: json!("approval"),
            method: "item/tool/requestUserInput".into(),
            params: json!({"claudeInput":{},"tool":"Bash"}),
            responded: false,
        });
        decoder.interrupting(&session, false);
        decoder.apply(
            &mut session,
            &json!({"type":"control_cancel_request","request_id":"approval"}),
        );
        decoder.apply(&mut session,&json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":[PERMISSION_CANCEL_DIAGNOSTIC,"A genuine fatal error"]}));
        assert_eq!(session.status, Status::Failed);
        assert!(
            session
                .error
                .as_deref()
                .is_some_and(|error| error.contains("genuine fatal"))
        );
    }
    #[test]
    fn assistant_receipt_timestamps_survive_full_frame_reconciliation() -> anyhow::Result<()> {
        use anyhow::Context;
        let mut session = session();
        let mut decoder = Decoder::default();
        let before = chrono::Utc::now().timestamp_millis();
        let frame = json!({"type":"assistant","message":{"id":"timed","content":[{"type":"text","text":"New assistant words"}]}});
        decoder.apply(&mut session, &frame);
        let receipt = session
            .entries
            .last()
            .and_then(|e| e.timestamp)
            .context("receipt time")?;
        assert!((before..=chrono::Utc::now().timestamp_millis()).contains(&receipt));
        decoder.apply(&mut session, &frame);
        assert_eq!(
            session.entries.last().context("entry")?.timestamp,
            Some(receipt)
        );
        Ok(())
    }
}
