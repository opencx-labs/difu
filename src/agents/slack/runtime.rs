use super::{
    Route, Settings,
    api::Api,
    commands,
    protocol::{self, Mention, Scope},
    socket::Socket,
};
use crate::{
    agents::{Control, Pending, Reply, Request, Session, client, orchestrator, server::Store},
    storage,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const HELP: &str = "Send difu a direct message to talk to the persistent orchestrator. Replies stay in the thread of your message; no mention is needed in DMs. In a configured channel, mention @difu on every message.\nReply in the same thread with:\nanswer <question-id> <answer>\napprove <request-id>\ndeny <request-id>\nPrefix commands with @difu in channels. Each ID applies only to the request shown in this thread. A normal message never grants permission.";

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    independent_threads: bool,
    received: BTreeSet<String>,
    inbox: VecDeque<Mention>,
    // Persist dispatch intent before handing a message to the service. After a
    // crash it is reported as uncertain, never silently replayed.
    dispatching: Option<Mention>,
    emitted: BTreeSet<String>,
    outbox: VecDeque<Output>,
    references: BTreeMap<String, Reference>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Output {
    route: Route,
    text: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Reference {
    session: String,
    pending: Pending,
    question: Option<String>,
    route: Route,
}
struct Journal {
    path: PathBuf,
    state: Mutex<State>,
}
impl Journal {
    fn open(path: PathBuf) -> Result<Self> {
        let state = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            State::default()
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }
    fn read<T>(&self, f: impl FnOnce(&State) -> T) -> Result<T> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Slack journal lock failed"))?;
        Ok(f(&state))
    }
    fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Slack journal lock failed"))?;
        let mut next = state.clone();
        let result = f(&mut next);
        storage::atomic_json(&self.path, &next)?;
        *state = next;
        Ok(result)
    }
    fn accept(&self, mention: Mention) -> Result<()> {
        self.update(|state| {
            if state.received.insert(mention.event_id.clone()) {
                state.inbox.push_back(mention);
            }
        })
    }
    fn recover(&self, settings: &Settings) -> Result<()> {
        self.update(|state| {
            if !state.independent_threads {
                // The shared manager was replaced, so its pending request IDs
                // must never resolve against a fresh conversation.
                state.references.retain(|_, r| r.session != orchestrator::ID);
                state.independent_threads = true;
            }
            let mut interrupted: Vec<_> = state.inbox.drain(..).collect();
            interrupted.extend(state.dispatching.take());
            for mention in interrupted {
                enqueue(state, format!("recovery:{}", mention.event_id), route(settings, &mention), "Slack processing was interrupted. This request may have reached difu; inspect the orchestrator before sending it again. No action was replayed.".into());
            }
        })
    }
    fn output(&self, key: String, route: Route, text: String) -> Result<()> {
        if self.read(|s| s.emitted.contains(&key))? {
            return Ok(());
        }
        self.update(|state| enqueue(state, key, route, text))
    }
}
fn enqueue(state: &mut State, key: String, route: Route, text: String) {
    if !state.emitted.insert(key) {
        return;
    }
    let chars: Vec<_> = text.chars().collect();
    for chunk in chars.chunks(3500) {
        state.outbox.push_back(Output {
            route: route.clone(),
            text: chunk.iter().collect(),
        });
    }
}
fn route(settings: &Settings, mention: &Mention) -> Route {
    Route {
        account: settings.account.clone(),
        channel: mention.channel.clone(),
        thread: mention.thread.clone(),
        event: mention.event_id.clone(),
    }
}
fn enabled(store: &Store, settings: &Settings) -> bool {
    store
        .storage
        .load_config()
        .ok()
        .and_then(|c| c.slack)
        .as_ref()
        == Some(settings)
}
fn status(store: &Store, text: &str) {
    let _ = storage::atomic_json(
        &store.home.join("slack").join("status.json"),
        &serde_json::json!({"text":text,"updated":chrono::Utc::now().timestamp_millis()}),
    );
}
fn delay(stop: &AtomicBool, duration: Duration) {
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) && started.elapsed() < duration {
        thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn start(store: Arc<Store>, shutdown: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut current = None;
        let mut connection: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)> = None;
        while !shutdown.load(Ordering::Relaxed) {
            let next = store
                .storage
                .load_config()
                .ok()
                .and_then(|c| c.slack)
                .filter(|s| s.enabled);
            if next != current {
                if let Some((stop, handle)) = connection.take() {
                    stop.store(true, Ordering::Relaxed);
                    // Never allow two processors to open the same durable journal.
                    let _ = handle.join();
                }
                current = next.clone();
                if let Some(settings) = next {
                    let stop = Arc::new(AtomicBool::new(false));
                    let token = stop.clone();
                    let store = store.clone();
                    let handle = thread::spawn(move || {
                        while !stop.load(Ordering::Relaxed) && enabled(&store, &settings) {
                            if let Err(error) = listen(&store, &settings, &stop) {
                                status(&store, &format!("Slack disconnected: {error:#}"));
                            }
                            delay(&stop, Duration::from_secs(10));
                        }
                    });
                    connection = Some((token, handle));
                } else {
                    status(&store, "Slack disabled");
                }
            }
            delay(&shutdown, Duration::from_secs(1));
        }
        if let Some((stop, handle)) = connection {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    });
}

fn listen(store: &Arc<Store>, settings: &Settings, stop: &Arc<AtomicBool>) -> Result<()> {
    let directory = store.home.join("slack");
    fs::create_dir_all(&directory)?;
    let journal = Arc::new(Journal::open(
        directory.join(format!("{}.json", storage::hash(&settings.account))),
    )?);
    // Crash recovery is explicit, matching the service's no-replay policy.
    journal.recover(settings)?;
    let api = Arc::new(super::credentials(settings)?);
    let processing_stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (store, settings, journal, api, stop, local_stop) = (
            store.clone(),
            settings.clone(),
            journal.clone(),
            api.clone(),
            stop.clone(),
            processing_stop.clone(),
        );
        thread::spawn(move || {
            if let Err(error) = process(&store, &settings, &journal, &api, &stop, &local_stop) {
                status(&store, &format!("Slack processing stopped: {error:#}"));
                local_stop.store(true, Ordering::Relaxed);
            }
        })
    };
    // Reconnecting the socket does not recreate the processor or replay input.
    while !stop.load(Ordering::Relaxed)
        && !processing_stop.load(Ordering::Relaxed)
        && !worker.is_finished()
    {
        let connected = (|| -> Result<()> {
            let mut socket = Socket::connect(&api)?;
            status(store, "Slack connected");
            while !stop.load(Ordering::Relaxed)
                && !processing_stop.load(Ordering::Relaxed)
                && !worker.is_finished()
            {
                let Some(envelope) = socket.read()? else {
                    continue;
                };
                if envelope.get("type").and_then(Value::as_str) == Some("disconnect") {
                    break;
                }
                let Some(id) = envelope.get("envelope_id").and_then(Value::as_str) else {
                    continue;
                };
                let scope = Scope {
                    team: &settings.team,
                    user: &settings.user,
                    channel: &settings.channel,
                    bot: &settings.bot,
                };
                if let Some(mention) = envelope
                    .get("payload")
                    .and_then(|p| protocol::mention(p, &scope))
                {
                    if !enabled(store, settings) {
                        break;
                    }
                    journal.accept(mention)?;
                }
                socket.acknowledge(id)?;
            }
            Ok(())
        })();
        if let Err(error) = connected {
            status(store, &format!("Slack reconnecting: {error:#}"));
        }
        delay(stop, Duration::from_secs(2));
    }
    processing_stop.store(true, Ordering::Relaxed);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("Slack processor stopped unexpectedly"))?;
    Ok(())
}

fn process(
    store: &Store,
    settings: &Settings,
    journal: &Journal,
    api: &Api,
    stop: &AtomicBool,
    local_stop: &AtomicBool,
) -> Result<()> {
    let mut next_post = Instant::now();
    let mut observed = BTreeMap::new();
    while !stop.load(Ordering::Relaxed) && !local_stop.load(Ordering::Relaxed) {
        if !enabled(store, settings) {
            break;
        }
        observe(store, settings, journal, &mut observed)?;
        // Independent threads never wait for another manager conversation.
        let mention = journal.read(|state| state.inbox.front().cloned())?;
        if let Some(mention) = mention {
            journal.update(|state| {
                state.inbox.retain(|m| m.event_id != mention.event_id);
                state.dispatching = Some(mention.clone());
            })?;
            let result = dispatch(store, settings, journal, &mention);
            let message = match result {
                Ok(Some(text)) => Some(text),
                Ok(None) => None,
                Err(error) => Some(format!("Could not apply this request: {error:#}")),
            };
            journal.update(|state| {
                if let Some(text) = message {
                    enqueue(
                        state,
                        format!("receipt:{}", mention.event_id),
                        route(settings, &mention),
                        text,
                    );
                }
                state.dispatching = None;
            })?;
        }
        if Instant::now() >= next_post
            && let Some(output) = journal.read(|state| state.outbox.front().cloned())?
        {
            if !enabled(store, settings) {
                break;
            }
            match api.reply(&output.route.channel, &output.route.thread, &output.text) {
                Ok(()) => {
                    journal.update(|state| {
                        state.outbox.pop_front();
                    })?;
                    next_post = Instant::now() + Duration::from_millis(1100);
                }
                Err(error) => {
                    status(store, &format!("Slack reply pending: {error:#}"));
                    next_post = Instant::now() + Duration::from_secs(30);
                }
            }
        }
        delay(stop, Duration::from_millis(200));
    }
    Ok(())
}

fn dispatch(
    store: &Store,
    settings: &Settings,
    journal: &Journal,
    mention: &Mention,
) -> Result<Option<String>> {
    let origin = route(settings, mention);
    let command = commands::parse(&mention.text)?;
    match command {
        commands::Command::Help => Ok(Some(HELP.into())),
        commands::Command::Message => {
            ensure!(
                !mention.text.trim().is_empty(),
                "Send a message, or use help (mention @difu in channels)"
            );
            let Reply::Launched(id) = client::request(
                &store.storage,
                Request::OrchestratorThread {
                    route: origin.clone(),
                    title: mention.text.clone(),
                },
            )?
            else {
                anyhow::bail!("Could not open the orchestrator conversation");
            };
            client::request(
                &store.storage,
                Request::SlackControl {
                    id,
                    route: origin,
                    control: Control::Message {
                        text: format!("[Slack thread {}]\n{}", mention.thread, mention.text),
                        queue: false,
                        skills: Vec::new(),
                        attachments: Vec::new(),
                    },
                },
            )?;
            Ok(None)
        }
        commands::Command::Answer { ref reference, .. }
        | commands::Command::Decision { ref reference, .. } => {
            let reference = journal
                .read(|s| s.references.get(reference).cloned())?
                .context("Unknown request ID; use the ID shown with the pending request")?;
            let session = store.get(&reference.session)?;
            let control = response(&reference, &session, &origin, command)?;
            client::request(
                &store.storage,
                Request::SlackControl {
                    id: reference.session,
                    control,
                    route: origin,
                },
            )?;
            Ok(Some("Response submitted.".into()))
        }
    }
}
fn response(
    reference: &Reference,
    session: &Session,
    origin: &Route,
    command: commands::Command,
) -> Result<Control> {
    ensure!(
        reference.route.account == origin.account
            && reference.route.channel == origin.channel
            && reference.route.thread == origin.thread,
        "Answer in the thread where this request was posted"
    );
    let pending = session
        .pending
        .iter()
        .find(|p| p.id == reference.pending.id && !p.responded)
        .context("This request is no longer pending")?;
    ensure!(
        fingerprint(pending) == fingerprint(&reference.pending),
        "This request changed; use the new request ID"
    );
    let control = match command {
        commands::Command::Answer { text, .. } => {
            let question = reference
                .question
                .clone()
                .context("Use approve or deny for this permission request")?;
            ensure!(
                pending
                    .unanswered_questions()
                    .iter()
                    .any(|(_, q)| q.get("id").and_then(Value::as_str) == Some(question.as_str())),
                "This question is already answered"
            );
            Control::AnswerQuestion {
                request: pending.id.clone(),
                question,
                answer: Some(text),
            }
        }
        commands::Command::Decision { approve, .. } => {
            ensure!(reference.question.is_none(), "Use answer for this question");
            commands::decision(pending, approve)?
        }
        _ => anyhow::bail!("Invalid request command"),
    };
    Ok(control)
}
fn fingerprint(pending: &Pending) -> String {
    let mut params = pending.params.clone();
    if let Some(params) = params.as_object_mut() {
        params.remove("difuAnswers");
        params.remove("difuSlackRoute");
    }
    storage::hash(serde_json::json!([pending.id, pending.method, params]).to_string())
}
fn observe(
    store: &Store,
    settings: &Settings,
    journal: &Journal,
    observed: &mut BTreeMap<String, u64>,
) -> Result<()> {
    for summary in store.list()? {
        if observed.get(&summary.id) == Some(&summary.version) {
            continue;
        }
        observed.insert(summary.id.clone(), summary.version);
        let session = match store.get(&summary.id) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for (index, entry) in session.entries.iter().enumerate() {
            let saved: Option<Route> = entry
                .data
                .get("difuSlackRoute")
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            let origin = saved.as_ref().or(if index >= session.slack_after {
                session.slack_route.as_ref()
            } else {
                None
            });
            let Some(origin) = origin.filter(|r| settings.accepts_route(r)) else {
                continue;
            };
            if (entry.kind == "agentMessage" && entry.finished_at.is_some()
                || entry.kind == "error")
                && !entry.text.trim().is_empty()
            {
                journal.output(
                    format!("entry:{}:{}:{}", session.id, origin.event, entry.id),
                    origin.clone(),
                    if session.orchestrator {
                        entry.text.to_string()
                    } else {
                        format!("{}\n{}", session.title, entry.text)
                    },
                )?;
            }
        }
        for pending in session.pending.iter().filter(|p| !p.responded) {
            let saved: Option<Route> = pending
                .params
                .get("difuSlackRoute")
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            let Some(origin) = saved
                .as_ref()
                .or(session.slack_route.as_ref())
                .filter(|r| settings.accepts_route(r))
            else {
                continue;
            };
            if pending.method == "item/tool/requestUserInput" {
                for (_, question) in pending.unanswered_questions() {
                    let Some(id) = question.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let title = question
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or("Question");
                    let options = question
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|o| o.get("label").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" / ");
                    let multiple =
                        if question.get("multiSelect").and_then(Value::as_bool) == Some(true) {
                            "\nChoose one or more, separated by commas."
                        } else {
                            ""
                        };
                    reference(
                        journal,
                        &session,
                        pending,
                        Some(id.into()),
                        origin,
                        format!("{title}\n{options}{multiple}"),
                    )?;
                }
            } else {
                reference(
                    journal,
                    &session,
                    pending,
                    None,
                    origin,
                    serde_json::to_string_pretty(&pending.params)?,
                )?;
            }
        }
        let Some(origin) = session
            .slack_route
            .as_ref()
            .filter(|r| settings.accepts_route(r))
        else {
            continue;
        };
        if matches!(
            session.status,
            crate::agents::Status::Failed | crate::agents::Status::Interrupted
        ) {
            journal.output(
                format!(
                    "stopped:{}:{}:{}",
                    session.id,
                    origin.event,
                    session.status.label()
                ),
                origin.clone(),
                format!(
                    "{}: {}. {}",
                    session.title,
                    session.status.label(),
                    session
                        .error
                        .as_deref()
                        .unwrap_or("Send a new instruction to continue.")
                ),
            )?;
        }
    }
    Ok(())
}
fn reference(
    journal: &Journal,
    session: &Session,
    pending: &Pending,
    question: Option<String>,
    origin: &Route,
    description: String,
) -> Result<()> {
    let key = storage::hash(format!(
        "{}:{}:{}:{}:{question:?}",
        session.id,
        origin.event,
        origin.thread,
        fingerprint(pending)
    ));
    let id: String = key.chars().take(20).collect();
    if journal.read(|s| s.references.contains_key(&id))? {
        return Ok(());
    }
    let prefix = if super::valid_id(&origin.channel, &['D']) {
        ""
    } else {
        "@difu "
    };
    let instruction = if question.is_some() {
        format!("{prefix}answer {id} <answer>")
    } else {
        format!("{prefix}approve {id}\n{prefix}deny {id}")
    };
    journal.update(|state| {
        state.references.insert(
            id.clone(),
            Reference {
                session: session.id.clone(),
                pending: pending.clone(),
                question,
                route: origin.clone(),
            },
        );
        enqueue(
            state,
            format!("request:{id}"),
            origin.clone(),
            format!(
                "{} ({}) needs input:\n{description}\n\n{instruction}",
                session.title, session.id
            ),
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Entry, Job, Launch, Status};
    use serde_json::json;

    fn origin(thread: &str) -> Route {
        Route {
            account: "fixture-account".into(),
            channel: "C1".into(),
            thread: thread.into(),
            event: format!("event-{thread}"),
        }
    }
    fn session() -> Session {
        let mut session = Session::new(
            "test".into(),
            Job::Coding(Launch {
                repository: "/tmp/fixture".into(),
                isolated: false,
                base: String::new(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session
    }
    #[test]
    fn duplicate_events_and_restart_never_replay_an_action() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("journal.json");
        let journal = Journal::open(path.clone())?;
        let mention = Mention {
            event_id: "event1".into(),
            channel: "C1".into(),
            thread: "thread1".into(),
            text: "delete a session".into(),
        };
        journal.accept(mention.clone())?;
        journal.accept(mention.clone())?;
        assert_eq!(journal.read(|s| s.inbox.len())?, 1);
        journal.update(|s| s.dispatching = s.inbox.pop_front())?;
        let restored = Journal::open(path)?;
        restored.recover(&Settings {
            enabled: true,
            team: "T1".into(),
            bot: "UBOT".into(),
            user: "UALI".into(),
            channel: "C1".into(),
            account: "fixture-account".into(),
        })?;
        restored.accept(mention)?;
        assert!(restored.read(|s| s.inbox.is_empty() && s.dispatching.is_none())?);
        assert_eq!(restored.read(|s| s.outbox.len())?, 1);
        Ok(())
    }
    #[test]
    fn migration_discards_only_legacy_manager_references() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let journal = Journal::open(temp.path().join("journal.json"))?;
        journal.update(|s| {
            for id in [
                orchestrator::ID,
                "coding-session",
                "orchestrator-new-thread",
            ] {
                s.references.insert(
                    id.into(),
                    Reference {
                        session: id.into(),
                        pending: Pending {
                            id: json!(1),
                            method: "approval".into(),
                            params: json!({}),
                            responded: false,
                        },
                        question: None,
                        route: origin("first"),
                    },
                );
            }
        })?;
        let settings = Settings {
            enabled: true,
            team: "T".into(),
            bot: "B".into(),
            user: "U".into(),
            channel: "C1".into(),
            account: "fixture-account".into(),
        };
        journal.recover(&settings)?;
        assert!(!journal.read(|s| s.references.contains_key(orchestrator::ID))?);
        assert_eq!(journal.read(|s| s.references.len())?, 2);
        journal.recover(&settings)?;
        assert_eq!(journal.read(|s| s.references.len())?, 2);
        Ok(())
    }
    #[test]
    fn approval_requires_same_thread_and_unchanged_pending_request() -> Result<()> {
        let mut session = session();
        let pending = Pending {
            id: json!(7),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({"command":"git status"}),
            responded: false,
        };
        session.pending.push(pending.clone());
        let reference = Reference {
            session: session.id.clone(),
            pending,
            question: None,
            route: origin("first"),
        };
        let approve = || commands::Command::Decision {
            reference: "id".into(),
            approve: true,
        };
        assert!(response(&reference, &session, &origin("second"), approve()).is_err());
        assert!(response(&reference, &session, &origin("first"), approve()).is_ok());
        session.pending.first_mut().context("pending")?.params =
            json!({"command":"git reset --hard"});
        assert!(response(&reference, &session, &origin("first"), approve()).is_err());
        session.pending.clear();
        assert!(response(&reference, &session, &origin("first"), approve()).is_err());
        Ok(())
    }
    #[test]
    fn partial_answers_do_not_invalidate_other_questions_or_allow_reanswering() -> Result<()> {
        let mut session = session();
        let pending = Pending {
            id: json!("q"),
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions":[{"id":"a"},{"id":"b"}],"difuAsync":true}),
            responded: false,
        };
        session.pending.push(pending.clone());
        let reference = Reference {
            session: session.id.clone(),
            pending,
            question: Some("b".into()),
            route: origin("first"),
        };
        session
            .pending
            .first_mut()
            .context("pending")?
            .params
            .as_object_mut()
            .context("params")?
            .insert("difuAnswers".into(), json!({"a":{"answers":["done"]}}));
        let answer = || commands::Command::Answer {
            reference: "id".into(),
            text: "yes".into(),
        };
        assert!(response(&reference, &session, &origin("first"), answer()).is_ok());
        session
            .pending
            .first_mut()
            .context("pending")?
            .params
            .get_mut("difuAnswers")
            .and_then(Value::as_object_mut)
            .context("answers")?
            .insert("b".into(), json!({"answers":["done"]}));
        assert!(response(&reference, &session, &origin("first"), answer()).is_err());
        Ok(())
    }
    #[test]
    fn journal_survives_restarts_without_duplicate_output_and_rolls_back_failed_writes()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("journal.json");
        let journal = Journal::open(path.clone())?;
        journal.output("reply".into(), origin("first"), "🙂".repeat(8000))?;
        journal.output("reply".into(), origin("first"), "duplicate".into())?;
        assert_eq!(journal.read(|s| s.outbox.len())?, 3);
        let restored = Journal::open(path.clone())?;
        assert_eq!(
            restored.read(|s| s
                .outbox
                .iter()
                .map(|o| o.text.chars().count())
                .sum::<usize>())?,
            8000
        );
        fs::remove_file(&path)?;
        fs::create_dir(&path)?;
        assert!(
            journal
                .output("failed".into(), origin("first"), "should not commit".into())
                .is_err()
        );
        assert!(!journal.read(|s| s.emitted.contains("failed"))?);
        Ok(())
    }
    #[test]
    fn switching_threads_preserves_old_output_and_questions() -> Result<()> {
        let mut session = session();
        session.set_slack_route(Some(origin("first")));
        session.entries.push(Entry {
            id: "response".into(),
            kind: "agentMessage".into(),
            text: "first reply".into(),
            finished_at: Some(1),
            ..Entry::default()
        });
        session.pending.push(Pending {
            id: json!("question"),
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions":[]}),
            responded: false,
        });
        session.set_slack_route(Some(origin("second")));
        assert_eq!(
            session
                .entries
                .first()
                .context("entry")?
                .data
                .pointer("/difuSlackRoute/thread"),
            Some(&json!("first"))
        );
        assert_eq!(
            session
                .pending
                .first()
                .context("pending")?
                .params
                .pointer("/difuSlackRoute/thread"),
            Some(&json!("first"))
        );
        assert_eq!(session.slack_after, 1);
        crate::agents::engine::apply_event(
            &mut session,
            &json!({"method":"item/completed","params":{"item":{"id":"response","type":"agentMessage","text":"late final text"}}}),
        );
        assert_eq!(
            session.entries.first().context("entry")?.text,
            "late final text"
        );
        session.set_slack_route(None);
        assert_eq!(
            session
                .entries
                .first()
                .context("entry")?
                .data
                .pointer("/difuSlackRoute/thread"),
            Some(&json!("first"))
        );
        Ok(())
    }
}
