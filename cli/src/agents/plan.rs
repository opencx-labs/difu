//! Explicit user choices after a completed native Codex plan.
use super::{Session, Status, provider::Provider};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Choice {
    Implement,
    FreshContext,
    ContinuePlanning,
}

/// Native Claude ExitPlanMode choices; independent of Codex's completed plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClaudeChoice {
    Auto,
    Manual,
    Feedback { text: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proposal {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub text: String,
}
fn proposal_id(thread: &str, turn: &str, item: &str, text: &str) -> String {
    format!(
        "{thread}:{turn}:{item}:{:x}",
        Sha256::digest(text.as_bytes())
    )
}

/// Invalidate once; duplicate native output and reconnect must not reoffer it.
pub(super) fn invalidate(session: &mut Session) {
    if let Some(proposal) = session.plan_prompt.take() {
        session.plan_handled = Some(proposal.id);
    }
}

fn ready(session: &Session) -> bool {
    session.provider == Provider::Codex
        && session.permission_state.mode.as_deref() == Some("plan")
        && session.status == Status::Idle
        && session.turn_id.is_none()
        && session.pending.is_empty()
        && session.workspace_requests.is_empty()
        && session.registration_requests.is_empty()
        && session.artifact_requests.is_empty()
        && session.question_requests.is_empty()
        && session.deletion_requests.is_empty()
        && session.orchestrator_requests.is_empty()
        && session.queue.is_empty()
        && !session.switching_workspace
        && !session.archived
        && !session.workspace_removed
        && session.deletion_progress.is_none()
        && session.error.is_none()
        && session.permission_state.pending_mode.is_none()
        && !session.permission_state.reload_pending
}

pub(super) fn associate(session: &mut Session, params: &serde_json::Value) {
    if params
        .get("difuRestored")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return;
    }
    if params
        .pointer("/item/type")
        .and_then(serde_json::Value::as_str)
        != Some("plan")
    {
        return;
    }
    if let Some(entry) = session.entries.iter_mut().find(|entry| {
        Some(entry.id.as_str())
            == params
                .pointer("/item/id")
                .and_then(serde_json::Value::as_str)
    }) && let Some(data) = entry.data.as_object_mut()
    {
        data.insert("difuPlanObserved".into(), true.into());
    }
    let (Some(thread), Some(turn), Some(item)) = (
        params.get("threadId").and_then(serde_json::Value::as_str),
        params.get("turnId").and_then(serde_json::Value::as_str),
        params.get("item"),
    ) else {
        return;
    };
    if item.get("type").and_then(serde_json::Value::as_str) != Some("plan")
        || session.thread_id.as_deref() != Some(thread)
        || session.turn_id.as_deref() != Some(turn)
    {
        return;
    }
    if let Some(entry) = session
        .entries
        .iter_mut()
        .find(|entry| Some(entry.id.as_str()) == item.get("id").and_then(serde_json::Value::as_str))
        && let Some(data) = entry.data.as_object_mut()
    {
        data.insert("difuPlanThread".into(), thread.into());
        data.insert("difuPlanTurn".into(), turn.into());
        data.entry("difuPlanSucceeded").or_insert(false.into());
    }
}

pub(super) fn completed(session: &mut Session, params: &serde_json::Value) {
    let (Some(thread), Some(turn)) = (
        params.get("threadId").and_then(serde_json::Value::as_str),
        params
            .pointer("/turn/id")
            .and_then(serde_json::Value::as_str),
    ) else {
        return;
    };
    if session.thread_id.as_deref() != Some(thread) || session.turn_id.as_deref() != Some(turn) {
        return;
    }
    let success = params
        .pointer("/turn/status")
        .and_then(serde_json::Value::as_str)
        == Some("completed");
    for entry in &mut session.entries {
        if entry
            .data
            .get("difuPlanThread")
            .and_then(serde_json::Value::as_str)
            == Some(thread)
            && entry
                .data
                .get("difuPlanTurn")
                .and_then(serde_json::Value::as_str)
                == Some(turn)
            && let Some(data) = entry.data.as_object_mut()
        {
            data.insert("difuPlanSucceeded".into(), success.into());
        }
    }
    if !success {
        invalidate(session);
    }
}

/// Backfill only a native final plan tied to the latest successfully finished
/// turn. Older records lack Difu's association tags; the native `<turn>-plan`
/// identifier is the narrowly supported legacy proof.
pub(super) fn preserve_legacy_outcome(session: &mut Session) {
    let Some(turn) = session.completed_turn.as_deref() else {
        return;
    };
    let succeeded =
        session.status == Status::Idle && session.turn_id.is_none() && session.error.is_none();
    if let Some(entry) = session.entries.iter_mut().find(|entry| {
        entry.id == format!("{turn}-plan")
            && entry.kind == "plan"
            && entry.data.get("type").and_then(serde_json::Value::as_str) == Some("plan")
            && entry.data.get("difuPlanObserved").is_none()
            && entry.data.get("difuPlanThread").is_none()
    }) {
        let completed = entry.finished_at.is_some()
            && entry
                .data
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|status| status == "completed");
        if let Some(data) = entry.data.as_object_mut() {
            data.entry("difuPlanLegacySucceeded")
                .or_insert((succeeded && completed).into());
        }
    }
}
pub(super) fn reconcile(session: &mut Session) {
    preserve_legacy_outcome(session);
    if !ready(session) {
        return;
    }
    let (Some(thread), Some(turn)) = (&session.thread_id, &session.completed_turn) else {
        return;
    };
    let Some((index, entry)) = session.entries.iter().enumerate().rev().find(|(_, entry)| {
        entry.kind == "plan"
            && entry.data.get("type").and_then(serde_json::Value::as_str) == Some("plan")
            && entry.finished_at.is_some()
    }) else {
        return;
    };
    let text = entry
        .data
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(entry.text.as_str());
    if text.trim().is_empty() {
        return;
    }
    let associated = entry
        .data
        .get("difuPlanThread")
        .and_then(serde_json::Value::as_str)
        == Some(thread)
        && entry
            .data
            .get("difuPlanTurn")
            .and_then(serde_json::Value::as_str)
            == Some(turn)
        && entry
            .data
            .get("difuPlanSucceeded")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    let legacy = entry.id == format!("{turn}-plan")
        && entry.data.get("difuPlanThread").is_none()
        && entry.data.get("difuPlanObserved").is_none()
        && entry
            .data
            .get("difuPlanLegacySucceeded")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && entry
            .data
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|status| status == "completed");
    if (!associated && !legacy)
        || session.entries.iter().skip(index + 1).any(|entry| {
            matches!(
                entry.kind.as_str(),
                "userMessage"
                    | "sending"
                    | "sending_context"
                    | "awaiting connection"
                    | "unsent"
                    | "unsent or unacknowledged"
                    | "error"
            )
        })
    {
        return;
    }
    let id = proposal_id(thread, turn, &entry.id, text);
    if session
        .plan_handled
        .as_deref()
        .is_some_and(|handled| handled.starts_with(&format!("{thread}:{turn}:")))
    {
        return;
    }
    session.plan_prompt = Some(Proposal {
        id,
        thread_id: thread.clone(),
        turn_id: turn.clone(),
        text: text.to_owned(),
    });
}

pub(super) fn claim(session: &mut Session, id: &str) -> Result<Proposal> {
    ensure!(
        ready(session),
        "Finish the current turn, questions, and queued messages before choosing a plan action"
    );
    let proposal = session
        .plan_prompt
        .as_ref()
        .context("That plan choice is no longer available")?;
    ensure!(
        proposal.id == id
            && session.thread_id.as_ref() == Some(&proposal.thread_id)
            && session.completed_turn.as_ref() == Some(&proposal.turn_id),
        "That plan belongs to an earlier conversation or turn"
    );
    ensure!(
        session.entries.iter().any(|entry| {
            let text = entry
                .data
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(entry.text.as_str());
            entry.kind == "plan"
                && entry.finished_at.is_some()
                && text == proposal.text
                && proposal_id(&proposal.thread_id, &proposal.turn_id, &entry.id, text)
                    == proposal.id
        }),
        "The completed plan changed; review its current text before choosing an action"
    );
    let proposal = proposal.clone();
    session.plan_prompt = None;
    session.plan_handled = Some(proposal.id.clone());
    Ok(proposal)
}

pub(super) fn execution(proposal: &Proposal) -> String {
    format!(
        "Implement the following approved plan. The user explicitly chose to implement this exact plan. Inspect the current workspace and existing changes before editing, preserve unrelated work, and follow the normal repository and harness instructions.\n\n<approved_plan>\n{}\n</approved_plan>",
        proposal.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Entry, Job, Launch, Session, Status};
    use serde_json::json;

    fn finished() -> Session {
        let mut s = Session::new(
            "chat".into(),
            Job::Coding(Launch {
                repository: "/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "plan".into(),
                model: Some("model".into()),
                effort: None,
            }),
        );
        s.thread_id = Some("thread".into());
        s.completed_turn = Some("turn".into());
        s.status = Status::Idle;
        s.permission_state.mode = Some("plan".into());
        s.entries.push(Entry {
            id: "turn-plan".into(),
            kind: "plan".into(),
            text: "# Approved plan\nChange the marker.".into(),
            data:
                json!({"id":"turn-plan","type":"plan","text":"# Approved plan\nChange the marker."})
                    .into(),
            finished_at: Some(1),
            ..Entry::default()
        });
        s
    }

    #[test]
    fn legacy_final_item_backfills_once_and_claim_is_atomic() -> Result<()> {
        let mut s = finished();
        let entry = s.entries.first_mut().context("plan entry")?;
        entry.text = entry.data.to_string().into(); // Historical display format.
        reconcile(&mut s);
        let proposal = s
            .plan_prompt
            .clone()
            .context("legacy completed native plan")?;
        assert_eq!(proposal.thread_id, "thread");
        assert_eq!(proposal.turn_id, "turn");
        assert_eq!(proposal.text, "# Approved plan\nChange the marker.");
        assert!(claim(&mut s, "wrong").is_err());
        assert!(claim(&mut s, &proposal.id).is_ok());
        assert!(claim(&mut s, &proposal.id).is_err());
        reconcile(&mut s);
        assert!(s.plan_prompt.is_none());
        let mut restored: Session = serde_json::from_value(serde_json::to_value(s)?)?;
        reconcile(&mut restored);
        assert!(restored.plan_prompt.is_none());
        Ok(())
    }

    #[test]
    fn checklist_unfinished_failed_unassociated_and_stale_plans_never_offer() -> Result<()> {
        for variant in 0..9 {
            let mut s = finished();
            let entry = s.entries.first_mut().context("plan entry")?;
            match variant {
                0 => entry.data = json!({"plan":[{"step":"checklist"}]}).into(),
                1 => entry.finished_at = None,
                2 => s.status = Status::Failed,
                3 => entry.id = "unproven".into(),
                4 => s.permission_state.mode = Some("default".into()),
                5 => s.provider = crate::agents::provider::Provider::Claude,
                6 => s.note("userMessage", "refine it"),
                7 => s.error = Some("failed".into()),
                _ => s.completed_turn = Some("later".into()),
            }
            reconcile(&mut s);
            assert!(s.plan_prompt.is_none(), "variant {variant}");
        }
        Ok(())
    }

    #[test]
    fn busy_or_changed_proposals_cannot_be_claimed() -> Result<()> {
        for variant in 0..9 {
            let mut s = finished();
            reconcile(&mut s);
            let id = s.plan_prompt.as_ref().context("plan proposal")?.id.clone();
            match variant {
                0 => s.status = Status::Running,
                1 => s.turn_id = Some("new".into()),
                2 => s.queue.push("refine".into()),
                3 => s.pending.push(crate::agents::Pending {
                    id: json!(1),
                    method: "approval".into(),
                    params: json!({}),
                    responded: false,
                }),
                4 => s.thread_id = Some("other".into()),
                5 => s.switching_workspace = true,
                6 => s.archived = true,
                7 => s.workspace_removed = true,
                _ => s.deletion_progress = Some(crate::agents::DeletionStage::Stopping),
            }
            assert!(claim(&mut s, &id).is_err(), "variant {variant}");
        }
        Ok(())
    }

    #[test]
    fn changed_authoritative_text_requires_new_review_but_handled_turn_never_reoffers() -> Result<()>
    {
        let mut s = finished();
        reconcile(&mut s);
        let original = s.plan_prompt.clone().context("proposal")?;
        s.entries.first_mut().context("entry")?.data =
            json!({"type":"plan","text":"Different final plan"}).into();
        assert!(claim(&mut s, &original.id).is_err());
        reconcile(&mut s);
        let updated = s.plan_prompt.clone().context("updated proposal")?;
        assert_ne!(updated.id, original.id);
        assert_eq!(updated.text, "Different final plan");
        claim(&mut s, &updated.id)?;
        s.entries.first_mut().context("entry")?.data =
            json!({"type":"plan","text":"Late replacement after decision"}).into();
        reconcile(&mut s);
        assert!(s.plan_prompt.is_none());
        Ok(())
    }

    #[test]
    fn modern_completion_requires_matching_native_thread_turn_and_success() -> Result<()> {
        use crate::agents::engine::apply_event;
        for variant in 0..5 {
            let mut s = finished();
            s.entries.clear();
            s.completed_turn = None;
            s.turn_id = Some("turn".into());
            s.status = Status::Running;
            let thread = if variant == 1 {
                "other-thread"
            } else {
                "thread"
            };
            let turn = if variant == 2 { "other-turn" } else { "turn" };
            apply_event(
                &mut s,
                &json!({"method":"turn/plan/updated","params":{"threadId":"thread","turnId":"turn","plan":[{"step":"checklist","status":"completed"}]}}),
            );
            apply_event(
                &mut s,
                &json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"id":"native-final-item","type":"plan","text":"Native final plan"}}}),
            );
            assert!(s.plan_prompt.is_none());
            let status = if variant == 3 {
                "failed"
            } else if variant == 4 {
                "interrupted"
            } else {
                "completed"
            };
            apply_event(
                &mut s,
                &json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":status}}}),
            );
            assert_eq!(s.plan_prompt.is_some(), variant == 0);
            if let Some(proposal) = s.plan_prompt.clone() {
                assert_eq!(proposal.text, "Native final plan");
                claim(&mut s, &proposal.id)?;
                apply_event(
                    &mut s,
                    &json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"native-final-item","type":"plan","text":"Native final plan"}}}),
                );
                apply_event(
                    &mut s,
                    &json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}),
                );
                assert!(s.plan_prompt.is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn legacy_failed_or_interrupted_outcome_cannot_be_promoted_by_reconnect_idle() -> Result<()> {
        for incomplete in [false, true] {
            let mut s = finished();
            s.status = if incomplete {
                Status::Interrupted
            } else {
                Status::Failed
            };
            if incomplete {
                let entry = s.entries.first_mut().context("entry")?;
                entry
                    .data
                    .as_object_mut()
                    .context("entry data")?
                    .insert("status".into(), json!("interrupted"));
            }
            // A finalized plan item is not proof the containing turn succeeded.
            preserve_legacy_outcome(&mut s);
            s.status = Status::Idle;
            s.error = None;
            reconcile(&mut s);
            assert!(s.plan_prompt.is_none());
            let mut restored: Session = serde_json::from_value(serde_json::to_value(s)?)?;
            crate::agents::engine::apply_event(
                &mut restored,
                &json!({"method":"item/completed","params":{"difuRestored":true,"item":{"id":"turn-plan","type":"plan","text":"Finalized native item read back on reconnect"}}}),
            );
            reconcile(&mut restored);
            assert!(restored.plan_prompt.is_none());
            assert_eq!(
                restored
                    .entries
                    .first()
                    .and_then(|entry| entry.data.get("difuPlanLegacySucceeded")),
                Some(&json!(false))
            );
        }
        let mut s = finished();
        s.entries
            .first_mut()
            .context("entry")?
            .data
            .as_object_mut()
            .context("data")?
            .insert("status".into(), json!("interrupted"));
        // Also reject an old partial item even if its original status was lost
        // by a provider reconnect before this version was installed.
        reconcile(&mut s);
        assert!(s.plan_prompt.is_none());
        Ok(())
    }
}
