//! Claude Code adapter. The rest of difu uses Session/Control, never Claude wire frames.
mod events;
mod protocol;
use super::{
    Control, Entry, Job, Pending, Prompt, Reply, Session, Status,
    provider::Provider,
    server::{Command as AgentCommand, Store},
};
use crate::{model::ModelInfo, process::Cancel};
use anyhow::{Context, Result, ensure};
use protocol::{Connection, Options};
use serde_json::{Value, json};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

pub(super) fn models(cancel: &Cancel) -> Result<Vec<ModelInfo>> {
    let directory = tempfile::tempdir()?;
    let mut connection = Connection::open(Options {
        cwd: directory.path(),
        model: None,
        effort: None,
        resume: None,
        instructions: "",
        discovery: true,
    })?;
    let response = connection.call(json!({"subtype":"initialize"}), cancel, |_| {
        anyhow::bail!("Unexpected request during model discovery")
    })?;
    let models = response
        .get("models")
        .and_then(Value::as_array)
        .context("Claude CLI did not report its model catalog")?;
    Ok(models
        .iter()
        .filter_map(|model| {
            let id = model.get("value").and_then(Value::as_str)?;
            Some(ModelInfo {
                id: format!("claude/{id}"),
                name: format!(
                    "Claude Code · {}",
                    model
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                ),
                efforts: model
                    .get("supportedEffortLevels")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect(),
            })
        })
        .collect())
}
fn mcp(store: &Store, id: &str, frame: &Value) -> Result<Value> {
    let request = frame
        .get("request")
        .context("Missing Claude control request")?;
    ensure!(
        request.get("subtype").and_then(Value::as_str) == Some("mcp_message")
            && request.get("server_name").and_then(Value::as_str) == Some("difu"),
        "Unsupported Claude control request"
    );
    let message = request.get("message").context("Missing MCP message")?;
    let result = match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"difu","version":env!("CARGO_PKG_VERSION")}})
        }
        Some("notifications/initialized" | "ping") => json!({}),
        Some("tools/list") => {
            let mut tool = super::artifacts::tool();
            if let Some(tool) = tool.as_object_mut() {
                tool.remove("type");
            }
            json!({"tools":[tool]})
        }
        Some("tools/call") => {
            ensure!(
                message.pointer("/params/name").and_then(Value::as_str)
                    == Some(super::artifacts::TOOL),
                "Unknown difu tool"
            );
            let mut session = store.get(id)?;
            let result = super::artifacts::register(
                &mut session,
                message.pointer("/params/arguments").unwrap_or(&Value::Null),
            );
            let text = match &result {
                Ok(()) => "Artifact registered in difu".into(),
                Err(error) => format!("{error:#}"),
            };
            if result.is_ok() {
                store.update(id, |s| s.artifacts = session.artifacts)?;
                store.save(id)?;
            }
            json!({"isError":result.is_err(),"content":[{"type":"text","text":text}]})
        }
        _ => anyhow::bail!("Unknown difu MCP method"),
    };
    let response = if let Some(id) = message.get("id") {
        json!({"jsonrpc":"2.0","id":id,"result":result})
    } else {
        // Notifications have no JSON-RPC response ID, but the transport needs an ack.
        json!({"jsonrpc":"2.0","result":{}})
    };
    Ok(json!({"mcp_response":response}))
}
fn request(store: &Store, id: &str, rpc: &mut Connection, frame: &Value) -> Result<()> {
    let request_id = frame
        .get("request_id")
        .cloned()
        .context("Missing Claude request ID")?;
    if frame.pointer("/request/subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return match mcp(store, id, frame) {
            Ok(response) => rpc.reply(&request_id, response),
            Err(error) => rpc.reject(&request_id, &format!("{error:#}")),
        };
    }
    let request = frame.get("request").context("Missing permission request")?;
    let name = request
        .get("tool_name")
        .and_then(Value::as_str)
        .context("Missing tool name")?;
    let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let pending = if name == "AskUserQuestion" {
        let questions = input
            .get("questions")
            .and_then(Value::as_array)
            .context("Missing Claude questions")?
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let mut question = question.clone();
                question["id"] = json!(index.to_string());
                question
            })
            .collect::<Vec<_>>();
        Pending {
            id: request_id,
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions":questions,"claudeInput":input}),
            responded: false,
        }
    } else {
        Pending {
            id: request_id,
            method: "mcpServer/elicitation/request".into(),
            params: json!({"serverName":"Claude Code","message":format!("Allow {name}?\n{}", serde_json::to_string_pretty(&input)?),
                "claudeInput":input,"tool":name,"requestedSchema":{"type":"object","properties":{}}}),
            responded: false,
        }
    };
    store.update(id, |s| {
        if !s.pending.iter().any(|p| p.id == pending.id) {
            s.pending.push(pending);
        }
        s.status = Status::Waiting;
    })?;
    store.save(id)
}
fn respond(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    request: &Value,
    response: &Value,
) -> Result<()> {
    let session = store.get(id)?;
    let pending = session
        .pending
        .iter()
        .find(|p| &p.id == request)
        .context("This request is no longer pending")?;
    ensure!(!pending.responded, "This request was already answered");
    let mut input = pending
        .params
        .get("claudeInput")
        .cloned()
        .context("Not a Claude tool request")?;
    let output = if pending.method == "item/tool/requestUserInput" {
        let answers = response
            .get("answers")
            .and_then(Value::as_object)
            .context("Missing answers")?;
        let questions = pending
            .params
            .get("questions")
            .and_then(Value::as_array)
            .context("Missing questions")?;
        let mut values = serde_json::Map::new();
        for question in questions {
            let key = question
                .get("id")
                .and_then(Value::as_str)
                .context("Missing question ID")?;
            let answer = answers
                .get(key)
                .and_then(|v| v.get("answers"))
                .and_then(Value::as_array)
                .context("Answer each question")?;
            values.insert(
                question
                    .get("question")
                    .and_then(Value::as_str)
                    .context("Missing question")?
                    .into(),
                json!(answer
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")),
            );
        }
        input["answers"] = Value::Object(values);
        json!({"behavior":"allow","updatedInput":input})
    } else {
        let action = response
            .get("action")
            .and_then(Value::as_str)
            .context("Missing approval action")?;
        ensure!(
            matches!(action, "accept" | "decline" | "cancel"),
            "Invalid approval action"
        );
        if action == "accept" {
            json!({"behavior":"allow","updatedInput":input})
        } else {
            json!({"behavior":"deny","message":"The user declined this tool call."})
        }
    };
    store.update(id, |s| {
        if let Some(pending) = s.pending.iter_mut().find(|p| &p.id == request) {
            pending.responded = true;
        }
    })?;
    store.save(id)?;
    rpc.reply(request, output)?;
    store.update(id, |s| {
        s.pending.retain(|p| &p.id != request);
        s.status = if s.pending.is_empty() {
            Status::Running
        } else {
            Status::Waiting
        };
    })?;
    store.save(id)
}
fn uuid(session: &Session) -> String {
    let seed = format!(
        "{}:{}:{}",
        session.id,
        session.version,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let hash = crate::storage::hash(seed.as_bytes());
    format!(
        "{}-{}-4{}-8{}-{}",
        hash.get(..8).unwrap_or_default(),
        hash.get(8..12).unwrap_or_default(),
        hash.get(13..16).unwrap_or_default(),
        hash.get(17..20).unwrap_or_default(),
        hash.get(20..32).unwrap_or_default()
    )
}
fn send(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    decoder: &mut events::Decoder,
    prompt: &Prompt,
) -> Result<()> {
    let session = store.get(id)?;
    let message_id = uuid(&session);
    let mut content = Vec::new();
    if let Some(context) = &session.provider_context {
        content.push(json!({"type":"text","text":context}));
    }
    content.push(json!({"type":"text","text":prompt.text()}));
    if let Some(context) = super::questions::pending_context(&session, prompt) {
        content.push(json!({"type":"text","text":context}));
    }
    for skill in prompt.skills() {
        content.push(
            json!({"type":"text","text":format!("Use the skill at {}", skill.path.display())}),
        );
    }
    for attachment in prompt.attachments() {
        super::media::validate(&store.storage, id, attachment)?;
        if attachment.kind == super::media::Kind::Image {
            use base64::Engine;
            let bytes = std::fs::read(&attachment.path)?;
            let format = image::guess_format(&bytes)?;
            let mime = match format {
                image::ImageFormat::Png => "image/png",
                image::ImageFormat::Jpeg => "image/jpeg",
                image::ImageFormat::Gif => "image/gif",
                image::ImageFormat::WebP => "image/webp",
                _ => anyhow::bail!("Unsupported Claude image format"),
            };
            content.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}}));
        } else {
            content.push(json!({"type":"text","text":format!("{} Local video: {}", attachment.token(), attachment.path.display())}));
        }
    }
    store.update(id, |s| {
        s.entries.retain(|entry| !(entry.kind == "awaiting connection" && entry.text == prompt.text()));
        s.entries.push(Entry { id: message_id.clone(), kind: "sending".into(), text: prompt.text().into(),
            data: json!({"prompt":prompt,"attachments":prompt.attachments(),"difuSteeringTurn":s.turn_id}), ..Entry::default() });
        if s.turn_id.is_none() { s.turn_id = Some(message_id.clone()); s.turn_started_at = Some(chrono::Utc::now().timestamp_millis()); }
        if s.pending.is_empty() { s.status = Status::Running; }
        s.error = None;
        if let Job::Coding(launch) = &mut s.job && launch.prompt.is_empty() { launch.prompt = prompt.text().into(); }
        if !s.title_manual && !s.title_ready { s.title = prompt.text().lines().next().unwrap_or("Claude session").chars().take(100).collect(); s.title_ready = true; }
    })?;
    store.save(id)?;
    if let Err(error) = rpc.write(&protocol::user_message(
        &message_id,
        session.thread_id.as_deref(),
        json!(content),
    )) {
        store.update(id, |s| {
            if let Some(entry) = s.entries.iter_mut().find(|e| e.id == message_id) {
                entry.kind = "unsent or unacknowledged".into();
            }
        })?;
        store.save(id)?;
        return Err(error);
    }
    decoder.sending();
    store.save(id)
}
fn command(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    decoder: &mut events::Decoder,
    control: Control,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    match control {
        Control::Message {
            text,
            queue,
            skills,
            attachments,
        }
        | Control::MessageWithAttachments {
            text,
            queue,
            skills,
            attachments,
        } => {
            let prompt = Prompt::WithSkills {
                text,
                skills,
                attachments,
            };
            if queue && session.turn_id.is_some() {
                store.update(id, |s| s.queue.push(prompt))?;
            } else {
                send(store, id, rpc, decoder, &prompt)?;
            }
        }
        Control::Respond { request, response } => respond(store, id, rpc, &request, &response)?,
        Control::AnswerQuestion {
            request,
            question,
            answer,
        } => {
            let (updated, _) =
                super::questions::prepare_answer(&session, &request, &question, answer.as_deref())?;
            super::questions::save_answer(store, id, updated.clone())?;
            if updated.unanswered_questions().is_empty() {
                respond(
                    store,
                    id,
                    rpc,
                    &request,
                    &json!({"answers":updated.params.get("difuAnswers")}),
                )?;
            }
        }
        Control::Interrupt | Control::InterruptAndSend => {
            let send_next = matches!(control, Control::InterruptAndSend);
            rpc.call(json!({"subtype":"interrupt"}), cancel, |frame| {
                mcp(store, id, frame)
            })?;
            store.update(id, |s| {
                if !send_next {
                    for prompt in std::mem::take(&mut s.queue) {
                        s.unsent(prompt);
                    }
                }
                s.status = Status::Interrupted;
            })?;
            // The interrupted result is drained before a queued prompt starts.
            if send_next {
                store.update(id, |s| s.status = Status::Running)?;
            }
        }
        Control::Resume => {
            let prompt = Prompt::from("Continue the task from its current state. Inspect prior actions; do not repeat completed work or publication.");
            send(store, id, rpc, decoder, &prompt)?;
        }
        Control::Compact => {
            ensure!(
                session.status == Status::Idle && session.turn_id.is_none(),
                "Finish the current turn before compacting"
            );
            send(store, id, rpc, decoder, &Prompt::from("/compact"))?;
        }
        // The service disconnects idle workers before applying model/effort changes.
        Control::Model { .. } => {
            anyhow::bail!("Model change must be handled by the session service")
        }
        Control::ReplaceQueued {
            index,
            expected,
            replacement,
        } => {
            ensure!(
                session.queue.get(index) == Some(&expected),
                "Queued message changed; reopen the queue"
            );
            store.update(id, |s| {
                if let Some(prompt) = replacement {
                    if let Some(slot) = s.queue.get_mut(index) {
                        *slot = prompt;
                    }
                } else {
                    s.queue.remove(index);
                }
            })?;
        }
        Control::RefreshShells => {}
    }
    store.save(id)
}

pub(super) fn run(
    store: &Arc<Store>,
    id: &str,
    controls: mpsc::Receiver<AgentCommand>,
    cancel: &Cancel,
    initial: Option<Control>,
) -> Result<()> {
    let mut session = store.get(id)?;
    super::workspace::prepare(&mut session, &store.home, cancel, |prepared| {
        store.update(id, |s| {
            s.job = prepared.job.clone();
            s.workspace = prepared.workspace.clone();
            s.workspace_ready = prepared.workspace_ready;
            s.baseline = prepared.baseline.clone();
            s.branch = prepared.branch.clone();
        })?;
        store.save(id)
    })?;
    super::guidance::confirm(store, id, &session, &controls, cancel)?;
    session = store.get(id)?;
    let instructions = format!(
        "{}\n\n{}",
        super::WORKTREE_INSTRUCTIONS,
        super::artifacts::INSTRUCTIONS
    );
    let mut rpc = Connection::open(Options {
        cwd: session.workspace.as_deref().context("Missing workspace")?,
        model: session
            .model
            .as_deref()
            .or_else(|| {
                if let Job::Coding(launch) = &session.job {
                    launch.model.as_deref()
                } else {
                    None
                }
            })
            .map(Provider::native_model),
        effort: session.effort.as_deref().or_else(|| {
            if let Job::Coding(launch) = &session.job {
                launch.effort.as_deref()
            } else {
                None
            }
        }),
        resume: session.thread_id.as_deref(),
        instructions: &instructions,
        discovery: false,
    })?;
    rpc.call(json!({"subtype":"initialize"}), cancel, |frame| {
        mcp(store, id, frame)
    })?;
    store.update(id, |s| {
        s.status = Status::Idle;
        s.artifact_tools = true;
    })?;
    let mut decoder = events::Decoder::default();
    if let Some(control) = initial {
        command(store, id, &mut rpc, &mut decoder, control, cancel)?;
    } else if session.thread_id.is_none()
        && let Job::Coding(launch) = &session.job
        && !launch.prompt.is_empty()
    {
        send(
            store,
            id,
            &mut rpc,
            &mut decoder,
            &Prompt::from(launch.prompt.clone()),
        )?;
    }
    let mut last_save = Instant::now();
    let mut dirty = false;
    loop {
        cancel.check()?;
        while let Some(frame) = rpc.next_frame()? {
            if frame.get("type").and_then(Value::as_str) == Some("control_request") {
                request(store, id, &mut rpc, &frame)?;
            } else {
                store.update(id, |s| decoder.apply(s, &frame))?;
                dirty = true;
            }
        }
        if dirty && last_save.elapsed() >= Duration::from_millis(200) {
            store.save(id)?;
            last_save = Instant::now();
            dirty = false;
        }
        match controls.recv_timeout(Duration::from_millis(40)) {
            Ok(request) => {
                let result = command(store, id, &mut rpc, &mut decoder, request.control, cancel);
                let _ = request.reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let session = store.get(id)?;
        if session.status == Status::Idle && !session.queue.is_empty() {
            let mut next = None;
            store.update(id, |s| {
                if !s.queue.is_empty() {
                    next = Some(s.queue.remove(0));
                }
            })?;
            if let Some(prompt) = next {
                let before = store.get(id)?.entries.len();
                if let Err(error) = send(store, id, &mut rpc, &mut decoder, &prompt) {
                    store.update(id, |s| {
                        if s.entries.len() == before {
                            s.unsent(prompt);
                        }
                    })?;
                    store.save(id)?;
                    return Err(error);
                }
            }
        }
    }
    store.save(id)
}

pub(super) fn skills() -> Reply {
    Reply::Skills { skills: Vec::new(), errors: vec!["Claude Code loads its workspace skills natively; use their slash commands in the message.".into()] }
}
