//! Live Claude mode changes and safe settings reloads.
use super::super::permissions::{self, Action, Snapshot};
use super::*;
pub(super) fn execute(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    action: Action,
    cancel: &Cancel,
) -> Result<Snapshot> {
    let session = store.get(id)?;
    let details = !matches!(action, Action::CycleMode);
    match action {
        Action::Refresh => {}
        Action::CycleMode | Action::SetMode { .. } => {
            let mode = match action {
                Action::SetMode { mode } => mode,
                _ => permissions::next_mode(&session),
            };
            ensure!(
                permissions::snapshot(&session)
                    .modes
                    .iter()
                    .any(|v| v.id == mode && v.available),
                "Claude mode {mode} is unavailable"
            );
            store.update(id, |s| {
                permissions::audit(
                    &mut s.permission_state,
                    permissions::PermissionEventKind::ModeChange,
                    None,
                    None,
                    Some(&format!("SetMode({mode})")),
                )
            })?;
            store.save(id)?;
            if mode == "bypassPermissions" && !session.permission_state.bypass_enabled {
                store.update(id, |s| {
                    s.permission_state.bypass_enabled = true;
                    s.permission_state.pending_mode = Some(mode);
                    s.permission_state.reload_pending = true;
                })?;
            } else {
                let result = rpc.call_preserving_requests(
                    json!({"subtype":"set_permission_mode","mode":mode}),
                    cancel,
                    |frame| mcp(store, id, frame),
                );
                match result {
                    Ok(response) => {
                        let observed = response
                            .get("mode")
                            .and_then(Value::as_str)
                            .unwrap_or(&mode)
                            .to_owned();
                        store.update(id, |s| {
                            s.permission_state.mode = Some(observed.clone());
                            s.permission_state.pending_mode = None;
                            s.permissions =
                                json!({"provider":"Claude Code","permissionMode":observed});
                            if mode == "auto" {
                                permissions::set_capability(
                                    &mut s.permission_state,
                                    "auto",
                                    json!(true),
                                );
                            }
                        })?;
                    }
                    Err(error) => {
                        if mode == "auto"
                            && error
                                .downcast_ref::<protocol::ControlError>()
                                .is_some_and(protocol::ControlError::rejects_auto_mode)
                        {
                            store.update(id, |s| {
                                permissions::set_capability(
                                    &mut s.permission_state,
                                    "auto",
                                    json!(false),
                                );
                                permissions::set_capability(
                                    &mut s.permission_state,
                                    "autoReason",
                                    json!(error.to_string()),
                                );
                            })?;
                        }
                        store.save(id)?;
                        return Err(error);
                    }
                }
            }
        }
        Action::SetPreset { .. } => {
            anyhow::bail!("Codex permission presets are unavailable for Claude sessions")
        }
        Action::Edit { edit } => {
            let cwd = session.workspace.as_deref().unwrap_or(session.job.root());
            let overlay =
                permissions::settings::edit(cwd, &session.permission_state.claude_settings, &edit)?;
            store.update(id, |s| {
                s.permission_state.claude_settings = overlay;
                s.permission_state.reload_pending = true;
            })?;
        }
    }
    store.save(id)?;
    snapshot(store, id, details)
}
pub(super) fn reload(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    decoder: &mut events::Decoder,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    if !session.permission_state.reload_pending
        || session.turn_id.is_some()
        || session.pending.iter().any(|p| !p.is_async_question())
    {
        return Ok(());
    }
    rpc.stop()?;
    let replacement = connect(store, id, &session, cancel)?;
    *rpc = replacement;
    *decoder = events::Decoder::default();

    store.save(id)
}

pub(super) fn snapshot(store: &Store, id: &str, details: bool) -> Result<Snapshot> {
    let session = store.get(id)?;
    let mut snapshot = permissions::snapshot(&session);
    if details
        && let Err(error) = permissions::settings::populate(
            &mut snapshot,
            session.workspace.as_deref().unwrap_or(session.job.root()),
            &session.permission_state.claude_settings,
        )
    {
        snapshot.message = Some(error.to_string());
    }
    Ok(snapshot)
}
