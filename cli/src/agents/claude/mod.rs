//! Claude Code adapter. The rest of difu uses Session/Control, never Claude wire frames.
pub(super) mod events;
mod mcp_control;
mod permissions_control;
mod plan_control;
mod protocol;
use super::{
    Control, Entry, Job, Pending, Prompt, Reply, Session, Status,
    provider::Provider,
    server::{Command as AgentCommand, Store},
};
use crate::{model::ModelInfo, process::Cancel};
use anyhow::{Context, Result, ensure};
use protocol::{Connection, Mode, Options};
use serde_json::{Value, json};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

pub(super) fn models(cancel: &Cancel) -> Result<Vec<ModelInfo>> {
    let directory = tempfile::tempdir()?;
    let mut connection = Connection::open(
        Options {
            cwd: directory.path(),
            model: None,
            effort: None,
            resume: None,
            instructions: "",
            mode: Mode::Models,
            permission_mode: None,
            allow_bypass: false,
            settings: None,
        },
        cancel,
    )?;
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
pub(super) fn usage(store: &Store, id: &str, session: &Session, cancel: &Cancel) -> Result<Value> {
    let mut rpc = Connection::open(
        Options {
            cwd: session.workspace.as_deref().unwrap_or(session.job.root()),
            model: None,
            effort: None,
            resume: session.thread_id.as_deref(),
            instructions: "",
            mode: Mode::Session,
            permission_mode: session
                .permission_state
                .pending_mode
                .as_deref()
                .or(session.permission_state.mode.as_deref()),
            allow_bypass: session.permission_state.bypass_enabled,
            settings: Some(&session.permission_state.claude_settings),
        },
        cancel,
    )?;
    rpc.call(json!({"subtype":"initialize"}), cancel, |frame| {
        mcp(store, id, frame)
    })?;
    rpc.call(json!({"subtype":"get_usage"}), cancel, |frame| {
        mcp(store, id, frame)
    })
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
            let tools = super::orchestrator::tools(&store.get(id)?)
                .into_iter()
                .map(|mut tool| {
                    if let Some(tool) = tool.as_object_mut() {
                        tool.remove("type");
                    }
                    tool
                })
                .collect::<Vec<_>>();
            json!({"tools":tools})
        }
        Some("tools/call") => {
            let name = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .context("Missing tool name")?;
            let args = message.pointer("/params/arguments").unwrap_or(&Value::Null);
            let mut session = store.get(id)?;
            let result = match name {
                super::orchestrator::TOOL => {
                    super::orchestrator::handle(store, id, args).map(|v| v.to_string())
                }
                super::questions::TOOL => {
                    super::questions::handle(store, id, args).map(|value| value.to_string())
                }
                super::peers::TOOL => {
                    super::peers::handle(store, id, args).map(|value| value.to_string())
                }
                super::shells::TOOL => {
                    super::shells::tool_call(store, id, args).map(|value| value.to_string())
                }
                super::pr_attachments::TOOL => {
                    super::pr_attachments::handle(store, id, args, &Cancel::default())
                        .map(|value| value.to_string())
                }
                super::folders::TOOL => {
                    super::folders::handle_tool(store, id, args).map(|value| value.to_string())
                }
                super::artifacts::TOOL => super::artifacts::register(&mut session, args)
                    .map(|()| "Artifact registered in difu".to_owned()),
                super::management::TOOL => (|| {
                    super::management::validate(&session, args)?;
                    session.deletion_requests.push(Pending {
                        id: message.get("id").cloned().unwrap_or(Value::Null),
                        method: "delete_session".into(),
                        params: args.clone(),
                        responded: true,
                    });
                    Ok(
                        "Session deletion accepted. Stop here; difu will close and remove this session.".to_owned(),
                    )
                })(),
                super::registration::TOOL => (|| {
                    ensure!(
                        session.registration_requests.is_empty(),
                        "Register one worktree at a time"
                    );
                    super::registration::prepare(&session, args, &Cancel::default())?;
                    session.registration_requests.push(Pending {
                        id: message.get("id").cloned().unwrap_or(Value::Null),
                        method: "worktree".into(),
                        params: args.clone(),
                        responded: false,
                    });
                    Ok(
                        "Worktree registration accepted. Stop here; difu will resume this conversation in the registered workspace.".to_owned(),
                    )
                })(),
                _ => anyhow::bail!("Unknown difu tool"),
            };
            if result.is_ok()
                && ![
                    super::questions::TOOL,
                    super::peers::TOOL,
                    super::pr_attachments::TOOL,
                    super::shells::TOOL,
                    super::folders::TOOL,
                ]
                .contains(&name)
            {
                store.update(id, |s| {
                    s.artifacts = session.artifacts;
                    s.registration_requests = session.registration_requests;
                    s.deletion_requests = session.deletion_requests;
                })?;
                store.save(id)?;
            }
            let text = match &result {
                Ok(text) => text.clone(),
                Err(error) => format!("{error:#}"),
            };
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
    if name == "AskUserQuestion" {
        return defer_questions(store, id, rpc, request, &request_id, &input);
    }
    let mut pending = Pending {
        id: request_id,
        method: "mcpServer/elicitation/request".into(),
        params: json!({"serverName":"Claude Code","message":format!("Allow {name}?\n{}", input.get("command").or_else(|| input.get("file_path")).and_then(Value::as_str).unwrap_or("Review the tool's request in the conversation.")),
            "claudeInput":input,"tool":name,"requestedSchema":{"type":"object","properties":{}}}),
        responded: false,
    };
    super::elicitation::normalize(&mut pending);
    plan_control::decorate(&mut pending, &store.get(id)?);
    store.update(id, |s| {
        if !s.pending.iter().any(|p| p.id == pending.id) {
            super::permissions::audit(
                &mut s.permission_state,
                super::permissions::PermissionEventKind::RequestSurfaced,
                Some(&pending.id),
                Some(name),
                Some("can_use_tool"),
            );
            s.pending.push(pending);
        }
        s.status = Status::Waiting;
    })?;
    store.save(id)
}

const QUESTIONS: &str = "AskUserQuestion is non-blocking in difu: it returns immediately while the user answers in difu's question panel, and each answer arrives later as its own user message. Ask related questions together, then continue work that does not depend on the answers, or end your turn if you cannot proceed without them.";
const DEFERRED: &str = "Difu shows these questions to the user, who will answer them asynchronously. This is not a refusal. Each answer will arrive later as a separate user message. Do not ask them again, reword them, or assume answers. Continue work that does not depend on them; if you cannot proceed without an answer, end your turn and wait.";

/// Difu's question panel for a Claude tool call; answers arrive as chat messages.
fn question_request(tool: &str) -> String {
    format!("difu-async:{tool}")
}

/// AskUserQuestion would hold Claude's turn until every answer is in. Show the
/// questions as asynchronous cards instead and release the tool call at once.
fn defer_questions(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    request: &Value,
    request_id: &Value,
    input: &Value,
) -> Result<()> {
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .context("Missing Claude questions")?
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let mut question = question
                .as_object()
                .cloned()
                .context("Invalid Claude question")?;
            question.insert("id".into(), json!(index.to_string()));
            Ok(Value::Object(question))
        })
        .collect::<Result<Vec<_>>>()?;
    let tool = request
        .get("tool_use_id")
        .and_then(Value::as_str)
        .or_else(|| request_id.as_str())
        .context("Missing Claude tool ID")?;
    let mut pending = Pending {
        id: json!(question_request(tool)),
        method: "item/tool/requestUserInput".into(),
        params: json!({"difuAsync":true,"toolUseId":tool,"questions":questions}),
        responded: false,
    };
    // Persist the questions before Claude can continue past them.
    store.update(id, |s| {
        if !s.pending.iter().any(|p| p.id == pending.id) {
            super::questions::attach_context(&s.entries, &mut pending);
            s.pending.push(pending);
        }
    })?;
    store.save(id)?;
    rpc.reply(
        request_id,
        json!({"behavior":"deny","message":DEFERRED,"interrupt":false}),
    )
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
    let input = pending
        .params
        .get("claudeInput")
        .cloned()
        .context("Not a Claude tool request")?;
    let response = super::elicitation::response(pending, response.clone())?;
    let action = response
        .get("action")
        .and_then(Value::as_str)
        .context("Missing approval action")?;
    ensure!(
        matches!(action, "accept" | "decline" | "cancel"),
        "Invalid approval action"
    );
    let output = if action == "accept" {
        json!({"behavior":"allow","updatedInput":input})
    } else {
        json!({"behavior":"deny","message":"The user declined this tool call."})
    };
    store.update(id, |s| {
        super::permissions::audit(
            &mut s.permission_state,
            super::permissions::PermissionEventKind::AnswerAction,
            Some(&pending.id),
            pending.params.get("tool").and_then(Value::as_str),
            Some(&format!("Respond({action})")),
        );
        if action != "accept" {
            s.permission_state.denials.push(super::permissions::Denial {
                tool: pending
                    .params
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .into(),
                detail: input.to_string(),
                reason: Some("The user declined this tool call".into()),
            });
            if s.permission_state.denials.len() > 100 {
                s.permission_state.denials.remove(0);
            }
        }
        if let Some(pending) = s.pending.iter_mut().find(|p| &p.id == request) {
            pending.responded = true;
        }
    })?;
    store.save(id)?;
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
    if let Prompt::Routed { origin, .. } = prompt {
        store.update(id, |s| s.set_slack_route(origin.clone()))?;
    }
    let session = store.get(id)?;
    let message_id = uuid(&session);
    let mut content = Vec::new();
    if let Some(context) = &session.provider_context {
        content.push(json!({"type":"text","text":context}));
    }
    content.push(json!({"type":"text","text":prompt.text()}));
    let (shell_ids, shell_context) = super::shells::context(&session);
    if let Some(context) = shell_context {
        content.push(json!({"type":"text","text":context}));
    }
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
        s.permission_state.claude_turn_attempted=true;
        s.entries.retain(|entry| !(entry.kind == "awaiting connection" && entry.text == prompt.text()));
        s.entries.push(Entry { id: message_id.clone(), kind: "sending".into(), text: prompt.text().into(),
            data: json!({"prompt":prompt,"attachments":prompt.attachments(),"difuSteeringTurn":s.turn_id}).into(), ..Entry::default() });
        if s.turn_id.is_none() { s.turn_id = Some(message_id.clone()); s.turn_started_at = Some(chrono::Utc::now().timestamp_millis()); }
        if s.pending.iter().all(Pending::is_async_question) { s.status = Status::Running; }
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
    store.update(id, |session| super::shells::delivered(session, &shell_ids))?;
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
        Control::DecideClaudePlan { request, choice } => {
            plan_control::decide(store, id, rpc, &request, choice, cancel)?
        }
        Control::DecidePlan { .. } => {
            anyhow::bail!("This plan action belongs to a Codex conversation")
        }
        Control::Permissions { action, response } => {
            let result = permissions_control::execute(store, id, rpc, action, cancel)
                .and_then(|mut snapshot| {
                    permissions_control::reload(store, id, rpc, decoder, cancel)?;
                    let current = store.get(id)?;
                    snapshot.active_mode = current.permission_state.mode;
                    snapshot.pending_mode = current.permission_state.pending_mode;
                    snapshot.reload_pending = current.permission_state.reload_pending;
                    Ok(snapshot)
                })
                .map_err(|e| format!("{e:#}"));
            let _ = response.send(result);
        }
        Control::Mcp { action, response } => {
            let snapshot =
                mcp_control::execute(store, id, rpc, action, cancel).map_err(|e| format!("{e:#}"));
            let _ = response.send(snapshot);
        }
        Control::Voice { response, .. } => {
            let _ = response.send(Err(
                "Talking to the agent needs a Codex session; Claude sessions use dictation".into(),
            ));
        }
        Control::ReadUsage => {
            store.update(id, |s| {
                super::permissions::audit(
                    &mut s.permission_state,
                    super::permissions::PermissionEventKind::ControlRequested,
                    None,
                    None,
                    Some("ReadUsage"),
                )
            })?;
            store.save(id)?;
            let usage =
                rpc.call_preserving_requests(json!({"subtype":"get_usage"}), cancel, |frame| {
                    mcp(store, id, frame)
                })?;
            store.update(id, |s| s.usage = usage)?;
        }

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
        Control::Respond { request, response } => {
            if let Some(pending) = session
                .pending
                .iter()
                .find(|p| p.id == request && p.is_async_question())
            {
                let text = super::questions::answer_text(pending, &response)?;
                // A failed delivery leaves the questions available for a retry.
                send(
                    store,
                    id,
                    rpc,
                    decoder,
                    &Prompt::QuestionAnswer {
                        text,
                        question_request: request.clone(),
                        question_id: None,
                    },
                )?;
                store.update(id, |s| {
                    if let Some(id) = request.as_str() {
                        s.answered_questions.insert(id.into());
                    }
                    s.pending.retain(|p| p.id != request);
                })?;
            } else {
                respond(store, id, rpc, &request, &response)?;
            }
        }
        Control::AnswerQuestion {
            request,
            question,
            answer,
        } => {
            let (updated, text) =
                super::questions::prepare_answer(&session, &request, &question, answer.as_deref())?;
            if updated.is_async_question() {
                // Each answer is its own chat message, recorded only once sent.
                if answer.is_some() {
                    send(
                        store,
                        id,
                        rpc,
                        decoder,
                        &Prompt::QuestionAnswer {
                            text,
                            question_request: request.clone(),
                            question_id: Some(question),
                        },
                    )?;
                }
                super::questions::save_answer(store, id, updated)?;
            } else {
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
        }
        Control::Interrupt | Control::InterruptAndSend => {
            let send_next = matches!(control, Control::InterruptAndSend);
            decoder.interrupting(&session, send_next);
            store.update(id, |s| {
                let name = if send_next {
                    "InterruptAndSend"
                } else {
                    "Interrupt"
                };
                let requests = session
                    .pending
                    .iter()
                    .filter(|p| p.params.get("claudeInput").is_some())
                    .collect::<Vec<_>>();
                if requests.is_empty() {
                    super::permissions::audit(
                        &mut s.permission_state,
                        super::permissions::PermissionEventKind::InterruptRequested,
                        None,
                        None,
                        Some(name),
                    );
                }
                for pending in requests {
                    super::permissions::audit(
                        &mut s.permission_state,
                        super::permissions::PermissionEventKind::InterruptRequested,
                        Some(&pending.id),
                        pending.params.get("tool").and_then(Value::as_str),
                        Some(name),
                    );
                }
            })?;
            store.save(id)?;
            rpc.call_preserving_requests(json!({"subtype":"interrupt"}), cancel, |frame| {
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
            let prompt = Prompt::from(
                "Continue the task from its current state. Inspect prior actions; do not repeat completed work or publication.",
            );
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
                        *slot = prompt.inherit_route(slot);
                    }
                } else {
                    s.queue.remove(index);
                }
            })?;
        }
        Control::RefreshShells | Control::ReadSubagent { .. } => {}
        Control::CloseShell { .. } => {
            anyhow::bail!("This provider does not expose background terminals")
        }
    }
    store.save(id)
}

pub(super) fn instructions(session: &Session) -> String {
    if session.orchestrator {
        return format!(
            "{}\n\n{QUESTIONS}\n\n{}",
            super::orchestrator::INSTRUCTIONS,
            super::questions::INSTRUCTIONS
        );
    }
    let instructions = format!(
        "{}\n\n{}\n\n{}\n\n{QUESTIONS}\n\n{}\n\n{}\n\n{}\n\n{}",
        super::artifacts::INSTRUCTIONS,
        super::registration::INSTRUCTIONS,
        super::management::INSTRUCTIONS,
        super::questions::INSTRUCTIONS,
        super::peers::INSTRUCTIONS,
        super::pr_attachments::INSTRUCTIONS,
        super::folders::INSTRUCTIONS
    );
    let instructions = format!("{instructions}\n\n{}", super::shells::instructions(session));
    let instructions = if session.native_setup {
        instructions
    } else {
        format!("{}\n\n{instructions}", super::WORKTREE_INSTRUCTIONS)
    };
    let instructions = format!("{}\n\n{instructions}", super::identity(session));
    super::rules::instructions(session, instructions)
}

fn connect(store: &Store, id: &str, session: &Session, cancel: &Cancel) -> Result<Connection> {
    let state = &session.permission_state;
    ensure!(
        session.thread_id.is_some()
            || (!state.claude_turn_attempted && !state.claude_turn_acknowledged),
        "Claude's native conversation ID is unknown after a sent request; the conversation was retained and no prompt was replayed"
    );
    let known_empty = state.claude_session_started
        && !state.claude_turn_attempted
        && !state.claude_turn_acknowledged;
    let resume = if known_empty {
        None
    } else {
        session.thread_id.as_deref()
    };
    match connect_once(store, id, session, resume, cancel) {
        Err(error)
            if resume.is_some()
                && error
                    .downcast_ref::<protocol::MissingConversation>()
                    .is_some_and(|missing| Some(missing.0.as_str()) == resume)
                && super::permissions::legacy_empty(session) =>
        {
            connect_once(store, id, session, None, cancel)
        }
        result => result,
    }
}
fn connect_once(
    store: &Store,
    id: &str,
    session: &Session,
    resume: Option<&str>,
    cancel: &Cancel,
) -> Result<Connection> {
    if let Some(mode) = session.permission_state.pending_mode.as_deref() {
        ensure!(
            super::permissions::snapshot(session)
                .modes
                .iter()
                .any(|choice| choice.id == mode && choice.available),
            "Claude mode {mode} is unavailable"
        );
    }
    let instructions = instructions(session);
    let mut rpc = Connection::open(
        Options {
            cwd: session.workspace.as_deref().context("Missing workspace")?,
            model: session
                .model
                .as_deref()
                .or({
                    if let Job::Coding(launch) = &session.job {
                        launch.model.as_deref()
                    } else {
                        None
                    }
                })
                .map(Provider::native_model),
            effort: session.effort.as_deref().or({
                if let Job::Coding(launch) = &session.job {
                    launch.effort.as_deref()
                } else {
                    None
                }
            }),
            resume,
            instructions: &instructions,
            mode: Mode::Session,
            permission_mode: session
                .permission_state
                .pending_mode
                .as_deref()
                .or(session.permission_state.mode.as_deref()),
            allow_bypass: session.permission_state.bypass_enabled
                || session.permission_state.pending_mode.as_deref() == Some("bypassPermissions"),
            settings: Some(&session.permission_state.claude_settings),
        },
        cancel,
    )?;
    let initialized = rpc.call(json!({"subtype":"initialize"}), cancel, |frame| {
        mcp(store, id, frame)
    })?;
    let observed = initialized
        .get("current_permission_mode")
        .and_then(Value::as_str)
        .map(String::from);
    if let Some(requested) = session.permission_state.pending_mode.as_deref() {
        ensure!(
            observed.as_deref() == Some(requested),
            "Claude did not confirm requested mode {requested}"
        );
    }
    store.update(id, |s| {
        if resume.is_none() {
            s.thread_id = None;
        }
        s.permission_state.claude_session_started = true;
        if resume.is_some() {
            s.permission_state.claude_turn_acknowledged = true;
        }
        if let Some(mode) = observed {
            s.permission_state.mode = Some(mode.clone());
            s.permission_state.pending_mode = None;
            s.permissions = json!({"provider":"Claude Code","permissionMode":mode});
            if mode == "bypassPermissions" {
                s.permission_state.bypass_enabled = true;
            }
            if mode == "auto" {
                super::permissions::set_capability(&mut s.permission_state, "auto", json!(true));
            }
        }
        s.permission_state.reload_pending = false;
        s.status = Status::Idle;
        s.error = None;
        s.artifact_tools = true;
        s.pr_tools = true;
        s.shell_tools = true;
        s.registration_tools = true;
        s.question_tools = true;
        s.peer_tools = true;
        s.folder_tools = true;
    })?;
    Ok(rpc)
}

fn transition(
    store: &Arc<Store>,
    id: &str,
    rpc: &mut Connection,
    decoder: &mut events::Decoder,
    controls: &mpsc::Receiver<AgentCommand>,
    cancel: &Cancel,
) -> Result<()> {
    let session = store.get(id)?;
    let Some(pending) = session.registration_requests.first() else {
        return Ok(());
    };
    let prepared = super::registration::prepare(&session, &pending.params, cancel)?;
    store.update(id, |s| {
        s.registration_requests.clear();
        s.switching_workspace = true;
    })?;
    if session.turn_id.is_some() {
        rpc.call(json!({"subtype":"interrupt"}), cancel, |frame| {
            mcp(store, id, frame)
        })?;
        let started = Instant::now();
        while store.get(id)?.turn_id.is_some() {
            cancel.check()?;
            ensure!(
                started.elapsed() < Duration::from_secs(45),
                "Claude did not stop before the workspace switch"
            );
            if let Some(frame) = rpc.next_frame()? {
                if frame.get("type").and_then(Value::as_str) == Some("control_request") {
                    request(store, id, rpc, &frame)?;
                } else {
                    store.update(id, |s| decoder.apply(s, &frame))?;
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    rpc.stop()?;
    store.update(id, |s| super::registration::apply(s, &prepared))?;
    store.save(id)?;
    let session = store.get(id)?;
    super::guidance::confirm(store, id, &session, controls, cancel)?;
    *rpc = connect(store, id, &session, cancel)?;
    *decoder = events::Decoder::default();
    store.update(id, |s| {
        s.switching_workspace = false;
        s.note(
            "progress",
            format!("Active worktree: {}", prepared.workspace.path.display()),
        );
    })?;
    send(
        store,
        id,
        rpc,
        decoder,
        &Prompt::from(super::registration::continuation(&session)),
    )
}

fn send_queued(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    decoder: &mut events::Decoder,
) -> Result<()> {
    if !store.inspect(id, |s| s.status == Status::Idle && !s.queue.is_empty())? {
        return Ok(());
    }
    let mut next = None;
    store.update(id, |s| {
        if !s.queue.is_empty() {
            next = Some(s.queue.remove(0));
        }
    })?;
    if let Some(prompt) = next {
        let before = store.get(id)?.entries.len();
        if let Err(error) = send(store, id, rpc, decoder, &prompt) {
            store.update(id, |s| {
                if s.entries.len() == before {
                    s.unsent(prompt);
                }
            })?;
            store.save(id)?;
            return Err(error);
        }
    }
    Ok(())
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
    let mut rpc = connect(store, id, &session, cancel)?;
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
    // Send accepted setup input before processing newer controls.
    send_queued(store, id, &mut rpc, &mut decoder)?;
    let mut last_save = Instant::now();
    let mut dirty = false;
    let mut initial_work = true;
    loop {
        cancel.check()?;
        let mut changed = std::mem::take(&mut initial_work);
        while let Some(frame) = rpc.next_frame()? {
            changed = true;
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
        match controls.try_recv() {
            Ok(request) => {
                changed = true;
                let result = command(store, id, &mut rpc, &mut decoder, request.control, cancel);
                let _ = request.reply.send(result);
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if !changed {
            cancel.check()?;
            if dirty {
                cancel
                    .wake()
                    .wait_timeout(Duration::from_millis(200).saturating_sub(last_save.elapsed()));
            } else {
                cancel.wake().wait();
            }
            continue;
        }
        if store.inspect(id, |s| !s.deletion_requests.is_empty())? {
            store.update(id, |s| s.deletion_requests.clear())?;
            super::management::schedule(store, id)?;
            return Ok(());
        }
        if store.inspect(id, |s| !s.registration_requests.is_empty())? {
            transition(store, id, &mut rpc, &mut decoder, &controls, cancel)?;
        }
        permissions_control::reload(store, id, &mut rpc, &mut decoder, cancel)?;
        send_queued(store, id, &mut rpc, &mut decoder)?;
    }
    store.save(id)
}

/// Claude Code reports user, project, plugin and built-in commands, skills
/// included, when it initializes in the workspace. They run as `/name` text.
pub(super) fn skills(cwd: &std::path::Path) -> Result<Reply> {
    let cancel = Cancel::default();
    let mut connection = Connection::open(
        Options {
            cwd,
            model: None,
            effort: None,
            resume: None,
            instructions: "",
            mode: Mode::Commands,
            permission_mode: None,
            allow_bypass: false,
            settings: None,
        },
        &cancel,
    )?;
    let response = connection.call(json!({"subtype":"initialize"}), &cancel, |_| {
        anyhow::bail!("Unexpected request during skill discovery")
    })?;
    Ok(Reply::Skills {
        skills: commands(&response)?,
        errors: Vec::new(),
    })
}

fn commands(response: &Value) -> Result<Vec<super::Skill>> {
    let commands = response
        .get("commands")
        .and_then(Value::as_array)
        .context("Claude CLI did not report its skills and commands")?;
    Ok(commands
        .iter()
        .filter_map(|command| {
            let name = command.get("name").and_then(Value::as_str)?;
            // Double-underscore commands serve server-launched sessions only.
            if name.starts_with("__") {
                return None;
            }
            let description = command
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let hint = command
                .get("argumentHint")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            Some(super::Skill {
                name: name.into(),
                path: Default::default(),
                description: if hint.is_empty() {
                    description
                } else {
                    format!("{hint} · {description}")
                },
                enabled: true,
                slash: true,
            })
        })
        .collect())
}
