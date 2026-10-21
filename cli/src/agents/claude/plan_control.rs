//! Native ExitPlanMode permission decisions, without a synthetic user turn.
use super::super::plan::ClaudeChoice;
use super::*;

pub(super) fn decorate(pending: &mut Pending, session: &Session) {
    if pending.params.get("tool").and_then(Value::as_str) != Some("ExitPlanMode") {
        return;
    }
    let input = pending
        .params
        .get("claudeInput")
        .cloned()
        .unwrap_or(Value::Null);
    let auto = super::super::permissions::snapshot(session)
        .modes
        .into_iter()
        .find(|m| m.id == "auto");
    let info = json!({
        "text": input.get("plan").and_then(Value::as_str).unwrap_or_default(),
        "path": input.get("planFilePath").and_then(Value::as_str),
        "auto_available": auto.as_ref().is_some_and(|m|m.available),
        "auto_reason": auto.and_then(|m|m.reason),
    });
    if let Some(params) = pending.params.as_object_mut() {
        params.insert("difuClaudePlan".into(), info);
    }
}

fn response(session: &Session, pending: &Pending, choice: &ClaudeChoice) -> Result<Value> {
    ensure!(
        session.provider == Provider::Claude,
        "This request belongs to Claude Code"
    );
    ensure!(
        !pending.responded
            && pending.params.get("tool").and_then(Value::as_str) == Some("ExitPlanMode"),
        "This plan request is no longer pending"
    );
    let input = pending
        .params
        .get("claudeInput")
        .context("Missing native plan request")?;
    match choice {
        ClaudeChoice::Feedback { text } => {
            ensure!(!text.trim().is_empty(), "Tell Claude what to change first");
            Ok(
                json!({"behavior":"deny","message":format!("Please revise the plan: {}\nStay in Plan mode; do not implement yet.",text.trim()),"interrupt":false}),
            )
        }
        ClaudeChoice::Auto | ClaudeChoice::Manual => {
            ensure!(
                input
                    .get("plan")
                    .and_then(Value::as_str)
                    .is_some_and(|p| !p.trim().is_empty()),
                "The plan text is unavailable; ask Claude to provide it again"
            );
            let mode = if matches!(choice, ClaudeChoice::Auto) {
                "auto"
            } else {
                "default"
            };
            ensure!(
                super::super::permissions::snapshot(session)
                    .modes
                    .iter()
                    .any(|m| m.id == mode && m.available),
                "Claude has not enabled {mode} mode for this session"
            );
            Ok(
                json!({"behavior":"allow","updatedInput":input,"updatedPermissions":[{"type":"setMode","mode":mode,"destination":"session"}]}),
            )
        }
    }
}

pub(super) fn decide(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    request: &Value,
    choice: ClaudeChoice,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    let pending = session
        .pending
        .iter()
        .find(|p| &p.id == request)
        .context("This plan request is no longer pending")?;
    let output = response(&session, pending, &choice)?;
    if matches!(choice, ClaudeChoice::Feedback { .. })
        && session.permission_state.mode.as_deref() != Some("plan")
    {
        permissions_control::execute(
            store,
            id,
            rpc,
            super::super::permissions::Action::SetMode {
                mode: "plan".into(),
            },
            cancel,
        )?;
    }
    store.update(id, |s| {
        if let Some(p) = s.pending.iter_mut().find(|p| &p.id == request) {
            p.responded = true;
        }
        super::super::permissions::audit(
            &mut s.permission_state,
            super::super::permissions::PermissionEventKind::AnswerAction,
            Some(request),
            Some("ExitPlanMode"),
            Some(match choice {
                ClaudeChoice::Auto => "PlanAuto",
                ClaudeChoice::Manual => "PlanManual",
                ClaudeChoice::Feedback { .. } => "PlanFeedback",
            }),
        );
    })?;
    store.save(id)?;
    // A failed write has an unknown outcome; never automatically replay consent.
    rpc.reply(request, output)?;
    store.update(id, |s| {
        s.pending.retain(|p| &p.id != request);
        s.status = if s.pending.iter().all(Pending::is_async_question) {
            Status::Running
        } else {
            Status::Waiting
        };
    })?;
    store.save(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Session, Pending) {
        let mut s = Session::new(
            "test".into(),
            Job::Coding(super::super::super::Launch {
                repository: "/tmp".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: Some("claude/sonnet".into()),
                effort: None,
            }),
        );
        s.provider = Provider::Claude;
        s.permission_state.mode = Some("plan".into());
        s.permission_state.capabilities = json!({"auto":true});
        let p = Pending {
            id: json!("exit"),
            method: "item/tool/requestUserInput".into(),
            params: json!({"tool":"ExitPlanMode","claudeInput":{"plan":"# Plan\nKeep the files","planFilePath":"/tmp/plan.md"}}),
            responded: false,
        };
        (s, p)
    }
    #[test]
    fn native_plan_choices_preserve_input_and_apply_session_modes() -> Result<()> {
        let (s, mut p) = fixture();
        decorate(&mut p, &s);
        assert_eq!(
            p.params
                .pointer("/difuClaudePlan/text")
                .and_then(Value::as_str),
            Some("# Plan\nKeep the files")
        );
        for (choice, mode) in [
            (ClaudeChoice::Auto, "auto"),
            (ClaudeChoice::Manual, "default"),
        ] {
            let r = response(&s, &p, &choice)?;
            assert_eq!(r.get("updatedInput"), p.params.get("claudeInput"));
            assert_eq!(
                r.pointer("/updatedPermissions/0/mode")
                    .and_then(Value::as_str),
                Some(mode)
            );
            assert_eq!(
                r.pointer("/updatedPermissions/0/destination")
                    .and_then(Value::as_str),
                Some("session")
            );
        }
        let feedback = response(
            &s,
            &p,
            &ClaudeChoice::Feedback {
                text: "Preserve the newline".into(),
            },
        )?;
        assert_eq!(feedback.get("behavior"), Some(&json!("deny")));
        assert_eq!(feedback.get("interrupt"), Some(&json!(false)));
        assert!(feedback.get("updatedPermissions").is_none());
        p.responded = true;
        assert!(response(&s, &p, &ClaudeChoice::Manual).is_err());
        Ok(())
    }
}
