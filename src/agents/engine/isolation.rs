//! Read-only investigation, followed by an explicit model-requested worktree transition.
use super::*;

pub(super) const TOOL: &str = "difu_begin_editing";
pub(super) const INSTRUCTIONS: &str = "You are investigating in the original repository with read-only filesystem access. Answer questions and investigate without creating a worktree. If the user's task requires repository edits, call difu_begin_editing BEFORE any write, patch, checkout, or other repository modification. Do not try to bypass read-only access, escalate permissions, or create a worktree yourself. After calling the tool, stop and wait: difu will move this same conversation into an isolated worktree and continue it automatically. Re-read relevant files there because the original checkout may contain unrelated local changes. Do not run local tests, linting, typechecks, builds, CI scripts, or validation suites; rely on PR CI. Git inspection and git diff checks are allowed. Do not commit, push, open a PR, or publish unless explicitly requested by the user's task.";

pub(super) fn tools() -> Value {
    json!([{"type":"function","name":TOOL,
        "description":"Request an isolated worktree when the user's task requires repository edits. Do not call for questions or read-only investigation. After calling, stop; difu will continue this conversation in the worktree.",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false}}])
}

pub(super) fn read_only_policy(session: &Session) -> Value {
    let network = session
        .inherited_permissions
        .pointer("/sandbox/networkAccess")
        .and_then(Value::as_bool)
        .unwrap_or(
            session
                .inherited_permissions
                .pointer("/sandbox/type")
                .and_then(Value::as_str)
                == Some("dangerFullAccess"),
        );
    json!({"type":"readOnly","networkAccess":network})
}

pub(super) fn settings(params: &mut Value, session: &Session) -> Result<()> {
    if session.waiting_for_workspace() {
        put(params, "sandbox", json!("read-only"))?;
        // Prevent an approval from granting writes to the original checkout.
        put(params, "approvalPolicy", json!("never"))?;
    } else if session.deferred_workspace && !session.inherited_permissions.is_null() {
        let mode = match session
            .inherited_permissions
            .pointer("/sandbox/type")
            .and_then(Value::as_str)
        {
            Some("readOnly") => "read-only",
            Some("workspaceWrite") => "workspace-write",
            Some("dangerFullAccess") => "danger-full-access",
            _ => anyhow::bail!(
                "Cannot restore this Codex permission profile in a worktree; no edits were started"
            ),
        };
        put(params, "sandbox", json!(mode))?;
        let approval = session
            .inherited_permissions
            .get("approvalPolicy")
            .filter(|v| !v.is_null())
            .context("Codex did not report its inherited approval policy")?;
        put(params, "approvalPolicy", approval.clone())?;
    }
    Ok(())
}

pub(super) fn transition(
    rpc: &mut Connection,
    store: &Arc<Store>,
    id: &str,
    requests: Vec<Pending>,
    controls: &mpsc::Receiver<AgentCommand>,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    for request in &requests {
        ensure!(
            request.params.get("threadId").and_then(Value::as_str) == session.thread_id.as_deref()
                && request.params.get("turnId").and_then(Value::as_str)
                    == session.turn_id.as_deref(),
            "Worktree request belongs to a different Codex turn"
        );
    }
    store.update(id, |s| s.workspace_requests.clear())?;
    if !session.waiting_for_workspace() {
        for request in &requests {
            rpc.write(json!({"id":request.id,"result":{"success":true,"contentItems":[{
            "type":"inputText","text":format!("The worktree is already available: {}", session.workspace.as_deref().context("Missing workspace")?.display())}]}}))?;
        }
        return Ok(());
    }
    store.update(id, |s| {
        s.switching_workspace = true;
        s.status = Status::Starting;
        s.note("progress", "Preparing an isolated worktree for editing…");
    })?;
    store.save(id)?;
    for request in &requests {
        rpc.write(json!({"id":request.id,"result":{"success":true,"contentItems":[{
        "type":"inputText","text":"Editing workspace requested. Stop here; difu will resume this conversation in the isolated worktree. Do not edit the original repository."}]}}))?;
    }
    rpc.call(
        "turn/interrupt",
        json!({"threadId":session.thread_id,"turnId":session.turn_id}),
        cancel,
        |v| event(store, id, v),
    )?;
    let start = Instant::now();
    while store.get(id)?.turn_id.is_some() {
        cancel.check()?;
        ensure!(
            start.elapsed() < Duration::from_secs(45),
            "Codex did not stop before the workspace switch; no worktree was created"
        );
        match rpc.output.recv_timeout(Duration::from_millis(40)) {
            Ok(value) => event(store, id, value.map_err(anyhow::Error::msg)?)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Codex disconnected before the workspace switch")
            }
        }
    }
    rpc._child.stop()?;
    store.update(id, |s| s.status = Status::Starting)?;
    let mut session = store.get(id)?;
    super::super::workspace::prepare(&mut session, &store.home, cancel, |prepared| {
        store.update(id, |s| {
            s.workspace = prepared.workspace.clone();
            s.workspace_ready = prepared.workspace_ready;
            s.baseline = prepared.baseline.clone();
            s.branch = prepared.branch.clone();
            s.job = prepared.job.clone();
        })?;
        store.save(id)
    })?;
    super::super::guidance::confirm(store, id, &session, controls, cancel)?;
    cancel.check()?;
    let (connected, _) = connect(store, id, cancel)?;
    *rpc = connected;
    store.update(id, |s| {
        s.switching_workspace = false;
        s.note(
            "progress",
            "Isolated worktree ready; continuing the same conversation",
        );
    })?;
    store.save(id)?;
    send_turn(
        rpc,
        store,
        id,
        &Prompt::from(
            "The isolated editing worktree is ready and is now your working directory. Continue the user's task from the existing conversation. Re-read relevant files here before editing: the original checkout may contain unrelated local changes that were not copied. Do not edit the original repository. Do not run local validation suites; rely on PR CI. Do not repeat completed actions or publish unless the user requested it.",
        ),
        false,
        cancel,
        true,
    )
}
