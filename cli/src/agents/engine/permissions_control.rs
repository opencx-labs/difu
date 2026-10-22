//! Native Codex mode/policy controls. No approval request is answered here.
use super::super::permissions::{self, Action, Snapshot};
use super::*;

pub(super) fn capabilities(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    cancel: &Cancel,
) -> Result<()> {
    let modes = rpc
        .call("collaborationMode/list", json!({}), cancel, |v| {
            event(store, id, v)
        })
        .ok()
        .and_then(|v| v.get("data").cloned())
        .unwrap_or(Value::Null);
    let requirements = rpc
        .call("configRequirements/read", Value::Null, cancel, |v| {
            event(store, id, v)
        })
        .ok()
        .map(|v| {
            v.get("requirements")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}))
        })
        .unwrap_or(Value::Null);
    store.update(id, |s| {
        s.permission_state.capabilities = json!({"collaborationModes":modes});
        s.permission_state.requirements = requirements;
    })
}
pub(super) fn execute(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    action: Action,
    cancel: &Cancel,
) -> Result<Snapshot> {
    if matches!(action, Action::Refresh) {
        store.update(id, |s| s.permission_state.pending_mode = None)?;
        capabilities(rpc, store, id, cancel)?;
        if rpc.needs_resume {
            let params = resume_params(rpc, &store.get(id)?, true)?;
            let response = reload_idle(rpc, store, id, params, cancel)?;
            record_policy(store, id, &response)?;
        } else if store.inspect(id, |s| {
            s.turn_id.is_none()
                && (s.permission_state.codex_rollout || s.permission_state.codex_turn_attempted)
        })? {
            let thread = store.get(id)?.thread_id;
            let response = rpc.call(
                "thread/resume",
                json!({"threadId":thread,"excludeTurns":true}),
                cancel,
                |v| event(store, id, v),
            )?;
            record_policy_preserving_pending(store, id, &response)?;
        }
    }
    let session = store.get(id)?;
    match action {
        Action::Refresh => {}
        Action::CycleMode | Action::SetMode { .. } => {
            store.update(id, |s| s.permission_state.pending_mode = None)?;
            store.save(id)?;
            if rpc.needs_resume {
                let params = resume_params(rpc, &store.get(id)?, false)?;
                let response = reload_idle(rpc, store, id, params, cancel)?;
                record_policy_preserving_pending(store, id, &response)?;
            }
            let session = store.get(id)?;
            let mode = match action {
                Action::SetMode { mode } => mode,
                _ => permissions::next_mode(&session),
            };
            ensure!(
                permissions::snapshot(&session)
                    .modes
                    .iter()
                    .any(|v| v.id == mode && v.available),
                "Codex has not reported support for mode {mode}"
            );
            let settings = mode_settings(&session, &mode)?;
            update_mode(rpc, store, id, &mode, settings, cancel)?;
        }
        Action::SetPreset { preset } => {
            permissions::validate_preset(&session, preset)?;
            store.update(id, |s| s.permission_state.pending_preset = Some(preset))?;
            if session.turn_id.is_none() && session.pending.is_empty() {
                apply_preset(rpc, store, id, cancel)?;
            }
        }
        Action::Edit { .. } => {
            anyhow::bail!("Claude permission rules are unavailable for Codex sessions")
        }
    }
    store.save(id)?;
    Ok(permissions::snapshot(&store.get(id)?))
}
fn update_mode(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    mode: &str,
    settings: Value,
    cancel: &Cancel,
) -> Result<()> {
    let thread = store
        .get(id)?
        .thread_id
        .context("Codex thread is not connected")?;
    let result = rpc.call(
        "thread/settings/update",
        json!({"threadId":thread,"collaborationMode":settings}),
        cancel,
        |notification| {
            // Settings notifications may precede the acknowledgement. Commit this
            // selection only after the native update succeeds.
            if notification.get("method").and_then(Value::as_str) == Some("thread/settings/updated")
                && notification
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    == Some(thread.as_str())
            {
                Ok(())
            } else {
                event(store, id, notification)
            }
        },
    );
    if let Err(error) = result {
        store.update(id, |s| s.permission_state.pending_mode = None)?;
        store.save(id)?;
        if error
            .downcast_ref::<RpcError>()
            .is_some_and(|error| error.code == Some(-32601))
        {
            anyhow::bail!(
                "This Codex version does not support immediate mode changes; update Codex and retry"
            );
        }
        return Err(error);
    }
    store.update(id, |s| {
        s.permission_state.mode = Some(mode.into());
        s.permission_state.pending_mode = None;
        if mode != "plan" {
            super::super::plan::invalidate(s);
        }
    })?;
    store.save(id)
}
fn mode_settings(session: &Session, mode: &str) -> Result<Value> {
    Ok(
        json!({"mode":mode,"settings":{"model":super::super::provider::Provider::native_model(session.model.as_deref().context("Codex did not report the current model")?),"reasoning_effort":session.effort,"developer_instructions":null}}),
    )
}
pub(super) fn restore_mode(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    mode: Option<&str>,
    response: &mut Value,
    cancel: &Cancel,
) -> Result<()> {
    let Some(mode) = mode else {
        return Ok(());
    };
    if mode == "default" && response.get("collaborationMode").is_none() {
        store.update(id, |s| {
            s.permission_state.mode = Some(mode.into());
            s.permission_state.pending_mode = None;
        })?;
        return Ok(());
    }
    let mut session = store.get(id)?;
    if let Some(model) = response.get("model").and_then(Value::as_str) {
        session.model = Some(model.into());
    }
    if let Some(effort) = response.get("reasoningEffort") {
        session.effort = effort.as_str().map(String::from);
    }
    let settings = mode_settings(&session, mode)?;
    if response
        .pointer("/collaborationMode/mode")
        .and_then(Value::as_str)
        == Some(mode)
        && response.pointer("/collaborationMode/settings/model")
            == settings.pointer("/settings/model")
        && response.pointer("/collaborationMode/settings/reasoning_effort")
            == settings.pointer("/settings/reasoning_effort")
    {
        store.update(id, |s| {
            s.permission_state.mode = Some(mode.into());
            s.permission_state.pending_mode = None;
        })?;
        return Ok(());
    }
    ensure!(
        super::super::permissions::snapshot(&session)
            .modes
            .iter()
            .any(|choice| choice.id == mode && choice.available),
        "Codex has not reported support for mode {mode}"
    );
    update_mode(rpc, store, id, mode, settings.clone(), cancel)?;
    put(response, "collaborationMode", settings)
}
pub(super) fn apply_preset(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    if session.permission_state.pending_preset.is_none() {
        return Ok(());
    }
    ensure!(
        session.turn_id.is_none() && session.pending.iter().all(Pending::is_async_question),
        "Resolve the current turn and approvals before applying a Codex permission preset"
    );
    if !has_rollout(&session) && !session.permission_state.codex_turn_attempted {
        // No native conversation exists yet. A fresh thread/start applies the
        // exact reviewer as well as sandbox before the first real prompt.
        rpc._child.stop()?;
        let (replacement, _) = connect(store, id, cancel)?;
        *rpc = replacement;
    } else {
        let params = resume_params(rpc, &session, true)?;
        let response = reload_idle(rpc, store, id, params, cancel)?;
        record_policy(store, id, &response)?;
    }
    ensure!(
        store.get(id)?.permission_state.pending_preset.is_none(),
        "Codex did not confirm the requested permission preset"
    );
    store.save(id)
}
/// A loaded app-server thread ignores thread/resume overrides. Release this
/// connection's subscription before resuming the same durable conversation.
pub(super) fn reload_idle(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    params: Value,
    cancel: &Cancel,
) -> Result<Value> {
    let session = store.get(id)?;
    ensure!(
        session.turn_id.is_none() && session.pending.iter().all(Pending::is_async_question),
        "Finish the current turn and resolve approvals before reloading Codex settings"
    );
    if !rpc.needs_resume {
        if session
            .permission_state
            .capabilities
            .get("unsubscribe")
            .and_then(Value::as_bool)
            == Some(false)
        {
            replace_connection(rpc, &session, cancel)?;
        } else {
            match rpc.call(
                "thread/unsubscribe",
                json!({"threadId":session.thread_id}),
                cancel,
                |v| event(store, id, v),
            ) {
                Ok(response) => ensure!(
                    matches!(
                        response.get("status").and_then(Value::as_str),
                        Some("unsubscribed" | "notLoaded" | "notSubscribed")
                    ),
                    "Codex did not release the thread for reloading"
                ),
                Err(error)
                    if error
                        .downcast_ref::<RpcError>()
                        .is_some_and(|e| e.code == Some(-32601)) =>
                {
                    store.update(id, |s| {
                        permissions::set_capability(
                            &mut s.permission_state,
                            "unsubscribe",
                            json!(false),
                        )
                    })?;
                    replace_connection(rpc, &session, cancel)?;
                }
                Err(error) => return Err(error),
            }
        }
        rpc.needs_resume = true;
    }
    match rpc.call("thread/resume", params, cancel, |v| event(store, id, v)) {
        Ok(mut response) => {
            ensure!(
                response.pointer("/thread/id").and_then(Value::as_str)
                    == session.thread_id.as_deref(),
                "Codex resumed a different conversation; no prompt was sent"
            );
            rpc.needs_resume = false;
            restore_mode(
                rpc,
                store,
                id,
                session.permission_state.mode.as_deref(),
                &mut response,
                cancel,
            )?;
            store.update(id, |s| {
                s.status = Status::Idle;
                s.error = None;
                s.voice = false;
            })?;
            Ok(response)
        }
        Err(error) => {
            store.update(id,|s|{s.status=Status::Failed;s.error=Some(format!("Codex settings reload failed; the same conversation can be reconnected: {error:#}"));})?;
            store.save(id)?;
            Err(error.context("Codex settings reload failed after releasing the thread; no prompt was sent. Refresh or reconnect to retry"))
        }
    }
}
fn replace_connection(rpc: &mut Connection, session: &Session, cancel: &Cancel) -> Result<()> {
    let mut replacement = Connection::open(
        session.workspace.as_deref().context("Missing workspace")?,
        cancel,
    )?;
    replacement.initialize(cancel)?;
    replacement.inherited = rpc.inherited.clone();
    replacement.needs_resume = true;
    rpc._child.stop()?;
    *rpc = replacement;
    Ok(())
}
pub(super) fn resume_params(
    rpc: &Connection,
    session: &Session,
    include_pending: bool,
) -> Result<Value> {
    let mut params = json!({"threadId":session.thread_id.as_ref().context("Codex thread is not connected")?,"cwd":session.workspace,"runtimeWorkspaceRoots":[session.workspace],"developerInstructions":format!("{}\n\n{}",rpc.inherited,instructions(session)),"excludeTurns":true});
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
    isolation::settings(&mut params, session)?;
    selected_policy(&mut params, session, include_pending)?;
    Ok(params)
}
pub(super) fn record_policy(store: &Store, id: &str, response: &Value) -> Result<()> {
    record_policy_inner(store, id, response, true)
}
pub(super) fn record_policy_preserving_pending(
    store: &Store,
    id: &str,
    response: &Value,
) -> Result<()> {
    record_policy_inner(store, id, response, false)
}
fn record_policy_inner(
    store: &Store,
    id: &str,
    response: &Value,
    accept_pending: bool,
) -> Result<()> {
    store.update(id,|s| {
        if let Some(mode)=response.pointer("/collaborationMode/mode").and_then(Value::as_str) {s.permission_state.mode=Some(mode.into());}
        s.permissions=json!({"sandbox":response.get("sandbox"),"approvalPolicy":response.get("approvalPolicy"),"approvalsReviewer":response.get("approvalsReviewer")});
        let observed=permissions::detected_preset(&s.permissions);
        // Observed defaults are not an instruction to override provider configuration
        // on reconnect. Persist only a preset the user explicitly selected.
        if accept_pending && s.permission_state.pending_preset.is_some()
            && s.permission_state.pending_preset==observed {
            s.permission_state.preset=observed;
            s.permission_state.pending_preset=None;
        }
        super::super::plan::reconcile(s);
    })
}
pub(super) fn selected_policy(
    params: &mut Value,
    session: &Session,
    include_pending: bool,
) -> Result<()> {
    let selected = if include_pending {
        session
            .permission_state
            .pending_preset
            .or(session.permission_state.preset)
    } else {
        session.permission_state.preset
    };
    let Some(preset) = selected else {
        return Ok(());
    };
    permissions::validate_preset(session, preset)?;
    let mut policy = permissions::preset_params(preset);
    if let Some(override_config) = policy.get("config").and_then(Value::as_object) {
        let mut config = params
            .get("config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        config.extend(override_config.clone());
        put(&mut policy, "config", Value::Object(config))?;
    }
    params
        .as_object_mut()
        .context("Codex parameters must be an object")?
        .extend(policy.as_object().cloned().unwrap_or_default());
    Ok(())
}
pub(super) fn collaboration(session: &Session) -> Result<Option<Value>> {
    let Some(mode) = session.permission_state.mode.as_ref() else {
        return Ok(None);
    };
    let available = session
        .permission_state
        .capabilities
        .get("collaborationModes")
        .and_then(Value::as_array)
        .is_some_and(|modes| {
            modes
                .iter()
                .any(|v| v.get("mode").and_then(Value::as_str) == Some(mode))
        });
    // Collaboration overrides are optional for ordinary native-default turns.
    // Older providers and transient discovery failures must not disable chat.
    if !available && mode == "default" {
        return Ok(None);
    }
    ensure!(available, "Codex no longer reports support for {mode} mode");
    Ok(Some(mode_settings(session, mode)?))
}

pub(super) fn has_rollout(session: &Session) -> bool {
    session.permission_state.codex_rollout
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collaboration_uses_native_builtin_instructions_and_current_model() -> Result<()> {
        let mut s = Session::new(
            "mode".into(),
            Job::Coding(super::super::super::Launch {
                repository: "/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: Some("codex/test-model".into()),
                effort: Some("high".into()),
            }),
        );
        s.permission_state.mode = Some("plan".into());
        assert!(collaboration(&s).is_err());
        s.permission_state.capabilities = json!({"collaborationModes":[{"mode":"plan"}]});
        assert_eq!(
            collaboration(&s)?,
            Some(
                json!({"mode":"plan","settings":{"model":"test-model","reasoning_effort":"high","developer_instructions":null}})
            )
        );
        Ok(())
    }
    #[test]
    fn observed_defaults_are_not_selected_presets_and_model_observation_keeps_pending() -> Result<()>
    {
        let mut session = Session::new(
            "policy".into(),
            Job::Coding(super::super::super::Launch {
                repository: "/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.permission_state.pending_mode = Some("plan".into());
        let store = Store::fixture("/tmp/native-policy-test".into(), vec![session]);
        let inherited = json!({"sandbox":{"type":"workspaceWrite"},"approvalPolicy":"on-request","approvalsReviewer":"user"});
        record_policy(&store, "policy", &inherited)?;
        assert!(store.get("policy")?.permission_state.preset.is_none());
        store.update("policy", |s| {
            s.permission_state.preset = Some(permissions::CodexPreset::ReadOnly);
            s.permission_state.pending_preset = Some(permissions::CodexPreset::Ask);
        })?;
        record_policy_preserving_pending(&store, "policy", &inherited)?;
        let observed = store.get("policy")?;
        assert_eq!(
            observed.permission_state.preset,
            Some(permissions::CodexPreset::ReadOnly)
        );
        assert_eq!(
            observed.permission_state.pending_preset,
            Some(permissions::CodexPreset::Ask)
        );
        assert_eq!(
            observed.permission_state.pending_mode.as_deref(),
            Some("plan")
        );
        record_policy(&store, "policy", &inherited)?;
        assert_eq!(
            store.get("policy")?.permission_state.preset,
            Some(permissions::CodexPreset::Ask)
        );
        assert!(
            store
                .get("policy")?
                .permission_state
                .pending_preset
                .is_none()
        );
        Ok(())
    }
}
