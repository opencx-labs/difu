//! Session-scoped management; deletion is executed by the service after tool acknowledgement.
use super::{Job, Request, Session, client, server::Store};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) const TOOL: &str = "difu_delete_session";
pub(super) const INSTRUCTIONS: &str = "When the user explicitly asks to delete this session and its worktree, use difu_delete_session if available. This permanently deletes this difu chat, attachments, and its managed worktree including uncommitted files. Never use this tool for routine cleanup, task completion, or a request to delete a different session. Call it alone, after other tools finish, then stop.";

pub(super) fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Permanently delete this session, its saved chat and attachments, and its difu-owned worktree including dirty files. Use only when the user explicitly requests deleting this session. No other session can be targeted. Call alone, then stop.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}})
}

pub(super) fn validate(session: &Session, args: &Value) -> Result<()> {
    ensure!(
        matches!(session.job, Job::Coding(_)),
        "Only coding sessions can be deleted here"
    );
    ensure!(
        args.as_object().is_some_and(|args| args.is_empty()),
        "This tool takes no arguments and can only delete its own session"
    );
    Ok(())
}

pub(super) fn schedule(store: &Arc<Store>, id: &str) -> Result<()> {
    let store = Arc::clone(store);
    let id = id.to_owned();
    std::thread::Builder::new()
        .name("difu-delete-session".into())
        .spawn(move || {
            // A separate service request can cancel and join the provider worker
            // without that worker ever attempting to join itself.
            if let Err(error) = client::request(&store.storage, Request::Delete { id: id.clone() })
            {
                let message = format!("Session deletion failed: {error:#}");
                let _ = store.update(&id, |s| {
                    s.error = Some(message.clone());
                    s.note("error", message);
                });
                let _ = store.save(&id);
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_cannot_target_another_session_or_path() {
        let session = Session::new(
            "current".into(),
            Job::Coding(super::super::Launch {
                repository: "/tmp/repo".into(),
                base: "main".into(),
                isolated: true,
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        assert!(validate(&session, &json!({})).is_ok());
        assert!(validate(&session, &json!({"id":"another"})).is_err());
        assert!(validate(&session, &json!({"path":"/tmp/other"})).is_err());
    }
}
