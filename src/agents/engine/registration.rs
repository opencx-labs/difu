use super::super::registration as registry;
use super::*;

pub(super) fn transition(
    rpc: &mut Connection,
    store: &Arc<Store>,
    id: &str,
    requests: Vec<Pending>,
    controls: &mpsc::Receiver<AgentCommand>,
    cancel: &Cancel,
) -> Result<()> {
    store.update(id, |s| s.registration_requests.clear())?;
    let session = store.get(id)?;
    let mut prepared = None;
    for request in requests {
        let result = (|| {
            ensure!(prepared.is_none(), "Register one worktree at a time");
            ensure!(
                request.params.get("threadId").and_then(Value::as_str)
                    == session.thread_id.as_deref()
                    && request.params.get("turnId").and_then(Value::as_str)
                        == session.turn_id.as_deref(),
                "Worktree registration belongs to another turn"
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
            registry::prepare(&session, &args, cancel)
        })();
        let text = match &result {
            Ok(_) => "Worktree registration accepted. Stop here; difu will resume this conversation in the registered worktree.".into(),
            Err(error) => format!("{error:#}"),
        };
        rpc.write(json!({"id":request.id,"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":text}]}}))?;
        if let Ok(next) = result {
            prepared = Some(next);
        }
    }
    let Some(prepared) = prepared else {
        return Ok(());
    };
    store.update(id, |s| {
        s.switching_workspace = true;
        s.status = Status::Starting;
    })?;
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
            "Codex did not stop before the workspace switch"
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
    store.update(id, |s| registry::apply(s, &prepared))?;
    store.save(id)?;
    let session = store.get(id)?;
    super::super::guidance::confirm(store, id, &session, controls, cancel)?;
    let (connected, _) = connect(store, id, cancel)?;
    *rpc = connected;
    store.update(id, |s| {
        s.switching_workspace = false;
        s.note(
            "progress",
            format!("Active worktree: {}", prepared.workspace.path.display()),
        );
    })?;
    store.save(id)?;
    send_turn(
        rpc,
        store,
        id,
        &Prompt::from(registry::CONTINUE),
        false,
        cancel,
        true,
    )
}
