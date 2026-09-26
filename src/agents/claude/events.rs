//! Translate Claude stream frames into difu's durable transcript.
use super::super::{Entry, Session, Status};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Decoder {
    message: String,
    blocks: HashMap<u64, String>,
    input: HashMap<String, String>,
    state: Option<String>,
    result_received: bool,
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
            entry.text = content;
            entry.data = data;
        }
        if complete && entry_kind != "mcpToolCall" {
            entry.finished_at = Some(now);
        }
    } else {
        session.entries.push(Entry {
            id: id.clone(),
            kind: entry_kind.into(),
            text: content,
            data,
            started_at: Some(now),
            finished_at: (complete && entry_kind != "mcpToolCall").then_some(now),
        });
    }
    id
}
impl Decoder {
    pub fn sending(&mut self) {
        self.result_received = false;
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
            && let Some(id) = frame
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        {
            session.thread_id = Some(id.into());
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
                if self.state.as_deref() == Some("running") && session.pending.is_empty() {
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
                                    entry.text = arguments.to_string();
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
                        if let Some(tool) = block.get("tool_use_id").and_then(Value::as_str)
                            && let Some(entry) = session
                                .entries
                                .iter_mut()
                                .find(|entry| entry.id == format!("claude-tool-{tool}"))
                        {
                            entry.finished_at = Some(now);
                            let failed =
                                block.get("is_error").and_then(Value::as_bool) == Some(true);
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
                            );
                        }
                    }
                }
                session.finish_steering_wait();
            }
            Some("result") if frame.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                self.result_received = true;
                let failed = frame.get("is_error").and_then(Value::as_bool) == Some(true);
                if failed {
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
                session
                    .pending
                    .retain(|p| Some(&p.id) != frame.get("request_id"));
                if session.pending.is_empty() && session.turn_id.is_some() {
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
                data: json!({"difuSteeringTurn":"turn"}),
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
}
