//! A completed plan is inert until an explicit, validated user choice.
use super::super::plan::{self, Choice, Proposal};
use super::*;

pub(super) fn decide(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    plan_id: &str,
    choice: Choice,
    cancel: &Cancel,
) -> Result<()> {
    let mut claimed = None;
    store.update(id, |s| claimed = Some(plan::claim(s, plan_id)))?;
    let proposal = claimed.context("Plan decision was not validated")??;
    store.save(id)?;
    if choice == Choice::ContinuePlanning {
        return Ok(());
    }
    let preparation = (|| -> Result<()> {
        permissions_control::apply_preset(rpc, store, id, cancel)?;
        if choice == Choice::FreshContext {
            fresh_context(rpc, store, id, &proposal, cancel)
        } else {
            permissions_control::execute(
                rpc,
                store,
                id,
                super::super::permissions::Action::SetMode {
                    mode: "default".into(),
                },
                cancel,
            )?;
            Ok(())
        }
    })();
    if let Err(error) = preparation {
        // No user turn was sent. The same explicit choice can be retried; the
        // handled marker still prevents duplicate native events from reoffering.
        store.update(id, |s| {
            if s.thread_id.as_ref() == Some(&proposal.thread_id)
                && s.completed_turn.as_ref() == Some(&proposal.turn_id)
                && s.turn_id.is_none()
            {
                s.plan_prompt = Some(proposal.clone());
            }
        })?;
        store.save(id)?;
        return Err(error);
    }
    // Persist the consumed decision before the write. A lost turn/start reply is
    // an uncertain send, never an invitation to replay this plan automatically.
    send_turn(
        rpc,
        store,
        id,
        &Prompt::from(plan::execution(&proposal)),
        false,
        cancel,
        TurnKind::ApprovedPlan(if choice == Choice::FreshContext {
            "Clear context and implement this plan."
        } else {
            "Implement this plan."
        }),
    )
}

fn fresh_context(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    proposal: &Proposal,
    cancel: &Cancel,
) -> Result<()> {
    let mut session = store.get(id)?;
    session.pr_tools = true;
    let cwd = session
        .workspace
        .as_deref()
        .context("Missing plan workspace")?;
    let mut replacement = Connection::open(cwd, cancel)?;
    replacement.initialize(cancel)?;
    replacement.inherited = rpc.inherited.clone();
    let mut tools = super::super::orchestrator::tools(&session);
    if !session.native_setup
        && session.deferred_workspace
        && matches!(&session.job,Job::Coding(launch) if launch.isolated)
    {
        tools.extend(isolation::tools().as_array().cloned().unwrap_or_default());
    }
    let mut params = json!({"cwd":cwd,"runtimeWorkspaceRoots":[cwd],
        "developerInstructions":format!("{}\n\n{}",replacement.inherited,instructions(&session)),
        "dynamicTools":tools});
    if let Some(model) = &session.model {
        put(
            &mut params,
            "model",
            json!(super::super::provider::Provider::native_model(model)),
        )?;
    }
    if let Some(effort) = &session.effort {
        put(
            &mut params,
            "config",
            json!({"model_reasoning_effort":effort}),
        )?;
    }
    let response = replacement.call("thread/start", params, cancel, |_| Ok(()))?;
    let thread = response
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("Codex returned no fresh context identifier")?
        .to_owned();
    ensure!(
        thread != proposal.thread_id,
        "Codex did not create a fresh context; no prompt was sent"
    );
    let model = session
        .model
        .as_deref()
        .context("Codex did not report the current model")?;
    let mut settings = json!({"threadId":thread,"collaborationMode":{"mode":"default","settings":{
        "model":super::super::provider::Provider::native_model(model),"reasoning_effort":session.effort,"developer_instructions":null}}});
    for (source, target) in [
        ("sandbox", "sandboxPolicy"),
        ("approvalPolicy", "approvalPolicy"),
        ("approvalsReviewer", "approvalsReviewer"),
    ] {
        let value = session
            .permissions
            .get(source)
            .filter(|v| !v.is_null())
            .with_context(|| {
                format!("Codex did not report {source}; no fresh-context prompt was sent")
            })?;
        put(&mut settings, target, value.clone())?;
    }
    replacement.call("thread/settings/update", settings, cancel, |_| Ok(()))?;
    // Until both acknowledgements succeed the original connection/ID/context
    // remains untouched. Replacing a native context never replaces its worktree.
    store.update(id,|s| {
        s.thread_id=Some(thread.clone());
        s.pr_tools=true;
        s.provider_context=None;
        s.permission_state.mode=Some("default".into());
        s.permission_state.pending_mode=None;
        s.permission_state.codex_thread_started=true;
        s.permission_state.codex_turn_attempted=false;
        s.permission_state.codex_rollout=false;
        s.voice=false;
        s.note("system","Implementing the approved plan with fresh Codex context. The chat and workspace are retained.");
        if let Some(entry)=s.entries.last_mut() {entry.data=json!({"difuPlan":proposal.id,"previousThreadId":proposal.thread_id,"threadId":thread}).into();}
    })?;
    store.save(id)?;
    let _ = rpc._child.stop();
    *rpc = replacement;
    Ok(())
}
