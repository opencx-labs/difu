//! End only the requesting session's voice connection; keep its coding work alive.
use super::{Pending, Session};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub(super) const TOOL: &str = "difu_stop_voice";
pub(super) const FRONTEND: &str = "Difu voice mode keeps the microphone live between replies. When the user asks to end voice, stop listening, hang up, or close the voice session, delegate that request to the coding agent so it can stop the actual voice connection. Do not merely say it has ended. Ending voice does not delete the chat or stop coding work.";

pub(super) fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Close this session's voice connection when the user asks to end voice, stop listening, or hang up. Keeps the chat and coding work. Takes no arguments and cannot target another session.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}})
}

pub(super) fn instructions(session: &Session) -> String {
    let action = if session.voice_tools {
        TOOL.to_owned()
    } else {
        format!(
            "`difu stop-voice --session '{}'`",
            session.id.replace('\'', "'\\''")
        )
    };
    format!(
        "When the user asks to end voice, stop listening, hang up, or close the voice session, use {action} to close the voice connection. This keeps the chat and coding work. Do not delete the session, kill a process, or claim voice has stopped without performing this action."
    )
}

pub(super) fn validate(session: &Session, request: &Pending) -> Result<()> {
    ensure!(
        request.params.get("threadId").and_then(Value::as_str) == session.thread_id.as_deref()
            && session.thread_id.is_some()
            && request.params.get("turnId").and_then(Value::as_str) == session.turn_id.as_deref()
            && session.turn_id.is_some(),
        "Voice stop belongs to another turn"
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
    ensure!(
        args.as_object().is_some_and(|args| args.is_empty()),
        "Voice stop takes no arguments and can only close its own voice connection"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_is_scoped_to_its_active_turn_and_accepts_no_target() -> Result<()> {
        let mut session = Session::new(
            "one".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: ".".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.thread_id = Some("thread".into());
        session.turn_id = Some("turn".into());
        let request = |thread: &str, turn: &str, args: Value| Pending {
            id: json!(1),
            method: "item/tool/call".into(),
            params: json!({"threadId":thread,"turnId":turn,"arguments":args}),
            responded: false,
        };
        assert!(validate(&session, &request("thread", "turn", json!({}))).is_ok());
        assert!(validate(&session, &request("thread", "turn", json!("{}"))).is_ok());
        assert!(validate(&session, &request("other", "turn", json!({}))).is_err());
        assert!(validate(&session, &request("thread", "old", json!({}))).is_err());
        for args in [Value::Null, json!({"id":"other"}), json!({"delete":true})] {
            assert!(validate(&session, &request("thread", "turn", args)).is_err());
        }
        assert!(instructions(&session).contains("difu stop-voice --session 'one'"));
        session.voice_tools = true;
        assert!(instructions(&session).contains(TOOL));
        assert!(!instructions(&session).contains("--session"));
        Ok(())
    }
}
