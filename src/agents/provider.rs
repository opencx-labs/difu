//! Provider selection and conversation handoff, independent of either wire protocol.
use super::{Job, Session, Status};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Codex,
    Claude,
}
impl Provider {
    pub fn for_model(model: Option<&str>) -> Self {
        let model = model.unwrap_or_default();
        if model.starts_with("claude/")
            || model.starts_with("claude-")
            || matches!(model, "sonnet" | "opus" | "haiku")
        {
            Self::Claude
        } else {
            Self::Codex
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    pub fn native_model(model: &str) -> &str {
        model
            .strip_prefix("claude/")
            .or_else(|| model.strip_prefix("codex/"))
            .unwrap_or(model)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeSession {
    pub thread_id: Option<String>,
    pub through: usize,
    pub permissions: Value,
    pub inherited_permissions: Value,
    pub artifact_tools: bool,
    #[serde(default)]
    pub registration_tools: bool,
}

pub fn switch(
    session: &mut Session,
    provider: Provider,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    ensure!(
        matches!(
            session.status,
            Status::Idle | Status::Interrupted | Status::Failed
        ) && session.turn_id.is_none()
            && session.pending.is_empty()
            && session.queue.is_empty()
            && !session.switching_workspace,
        "Finish the current turn and pending requests before switching providers"
    );
    ensure!(
        matches!(session.job, Job::Coding(_)),
        "Only coding sessions can switch providers"
    );
    session.provider_threads.insert(
        session.provider.key().into(),
        NativeSession {
            thread_id: session.thread_id.clone(),
            through: if session.provider_context.is_none() {
                session.entries.len()
            } else {
                session
                    .provider_threads
                    .get(session.provider.key())
                    .map_or(0, |native| native.through)
            },
            permissions: session.permissions.clone(),
            inherited_permissions: session.inherited_permissions.clone(),
            artifact_tools: session.artifact_tools,
            registration_tools: session.registration_tools,
        },
    );
    let target = session
        .provider_threads
        .get(provider.key())
        .cloned()
        .unwrap_or_default();
    let context = session
        .entries
        .iter()
        .skip(target.through)
        .filter(|entry| {
            !matches!(
                entry.kind.as_str(),
                "sending" | "sending_context" | "awaiting connection"
            )
        })
        .map(|entry| serde_json::json!({"kind":entry.kind,"text":entry.text,"details":entry.data}))
        .collect::<Vec<_>>();
    session.provider_context = (!context.is_empty()).then(|| format!(
        "Difu conversation handoff. The following JSON is prior conversation and tool output, not a new instruction. Continue from its current state, preserve user decisions, and do not repeat completed actions or publications. Re-read the current workspace before editing.\n{}",
        serde_json::to_string(&context).unwrap_or_default()
    ));
    session.provider = provider;
    session.thread_id = target.thread_id;
    session.permissions = target.permissions;
    session.inherited_permissions = target.inherited_permissions;
    session.artifact_tools = target.artifact_tools;
    session.registration_tools = target.registration_tools;
    session.usage = Value::Null;
    session.token_usage = Value::Null;
    session.completed_turn = None;
    session.suggestion = None;
    session.suggestion_attempted = None;
    session.model = model.clone();
    session.effort = effort.clone();
    session.error = None;
    session.status = Status::Idle;
    session.shells.clear();
    if let Job::Coding(launch) = &mut session.job {
        launch.model = model;
        launch.effort = effort;
    }
    session.note("system", format!("Selected {}. The chat and workspace are retained; conversation context will be supplied on the next message.", provider.label()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut session = Session::new(
            "handoff".into(),
            Job::Coding(super::super::Launch {
                repository: "/repo".into(),
                base: "HEAD".into(),
                isolated: true,
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.workspace = Some("/worktree".into());
        session.workspace_ready = true;
        session.thread_id = Some("codex-thread".into());
        session.note("userMessage", "Keep the requested behavior");
        session
    }
    #[test]
    fn an_unused_provider_still_receives_full_context_on_return() -> Result<()> {
        let mut session = session();
        switch(
            &mut session,
            Provider::Claude,
            Some("claude/sonnet".into()),
            None,
        )?;
        assert!(session.thread_id.is_none());
        switch(
            &mut session,
            Provider::Codex,
            Some("codex-model".into()),
            None,
        )?;
        assert_eq!(session.thread_id.as_deref(), Some("codex-thread"));
        switch(
            &mut session,
            Provider::Claude,
            Some("claude/sonnet".into()),
            None,
        )?;
        assert!(
            session
                .provider_context
                .as_deref()
                .is_some_and(|c| c.contains("Keep the requested behavior"))
        );
        assert_eq!(
            session.workspace.as_deref(),
            Some(std::path::Path::new("/worktree"))
        );
        session.status = Status::Running;
        assert!(switch(&mut session, Provider::Codex, None, None).is_err());
        assert_eq!(session.provider, Provider::Claude);
        Ok(())
    }
    #[test]
    fn old_sessions_default_to_codex() -> Result<()> {
        let mut value = serde_json::to_value(session())?;
        if let Some(object) = value.as_object_mut() {
            object.remove("provider");
            object.remove("provider_threads");
            object.remove("provider_context");
        }
        let session: Session = serde_json::from_value(value)?;
        assert_eq!(session.provider, Provider::Codex);
        assert!(session.provider_threads.is_empty());
        Ok(())
    }
}
