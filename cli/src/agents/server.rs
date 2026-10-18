use super::*;
use crate::{
    process::Cancel,
    storage::{self, Storage},
};
use anyhow::{Context, Result, ensure};
use nix::fcntl::{Flock, FlockArg};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::BufReader,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

pub struct Store {
    pub home: PathBuf,
    pub storage: Storage,
    sessions: Mutex<BTreeMap<String, Session>>,
    changed: Condvar,
    save_lock: Mutex<()>,
}
impl Store {
    #[cfg(test)]
    pub(super) fn fixture(home: PathBuf, sessions: Vec<Session>) -> Self {
        Self {
            storage: Storage {
                config: home.join("config.json"),
                cache: home.join("cache"),
            },
            home,
            sessions: Mutex::new(sessions.into_iter().map(|s| (s.id.clone(), s)).collect()),
            changed: Condvar::new(),
            save_lock: Mutex::new(()),
        }
    }
    pub fn get(&self, id: &str) -> Result<Session> {
        self.inspect(id, Clone::clone)
    }
    /// Read only the needed fields; idle checks must not copy conversation history.
    pub(super) fn inspect<T>(&self, id: &str, read: impl FnOnce(&Session) -> T) -> Result<T> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session store lock failed"))?;
        Ok(read(sessions.get(id).context("Session not found")?))
    }
    pub fn update(&self, id: &str, f: impl FnOnce(&mut Session)) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session store lock failed"))?;
        let session = sessions.get_mut(id).context("Session not found")?;
        f(session);
        session.touch();
        self.changed.notify_all();
        Ok(())
    }
    /// Serialize durable manager-record changes with saves, without touching chat history.
    pub(super) fn durable<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self
            .save_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Persistence lock failed"))?;
        action()
    }
    pub fn save(&self, id: &str) -> Result<()> {
        let _guard = self
            .save_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Persistence lock failed"))?;
        // A background flush may have listed this session just before deletion.
        // The save lock prevents recreating a file after Delete has removed it.
        let session = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session store lock failed"))?
            .get(id)
            .cloned();
        if let Some(session) = session {
            storage::atomic_json(&self.home.join(format!("{id}.json")), &session)?;
        }
        Ok(())
    }
    fn queue_preparing(
        &self,
        id: &str,
        control: &Control,
        route: &Option<slack::Route>,
    ) -> Result<bool> {
        if !matches!(
            control,
            Control::Message { .. }
                | Control::MessageWithAttachments { .. }
                | Control::ReplaceQueued { .. }
        ) {
            return Ok(false);
        }
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session lock failed"))?;
        let session = sessions.get_mut(id).context("Session not found")?;
        if !session.preparing() {
            return Ok(false);
        }
        // Recheck and append atomically with setup completing or failing.
        match control {
            Control::Message {
                text,
                skills,
                attachments,
                ..
            }
            | Control::MessageWithAttachments {
                text,
                skills,
                attachments,
                ..
            } => {
                ensure!(!text.trim().is_empty(), "Message cannot be empty");
                session.queue.push(Prompt::Routed {
                    prompt: Box::new(Prompt::WithSkills {
                        text: text.clone(),
                        skills: skills.clone(),
                        attachments: attachments.clone(),
                    }),
                    origin: route.clone(),
                });
            }
            Control::ReplaceQueued {
                index,
                expected,
                replacement,
            } => {
                ensure!(
                    session.queue.get(*index) == Some(expected),
                    "That queued message changed or was already sent; refresh the queue. Your draft is retained."
                );
                if let Some(prompt) = replacement {
                    if let Some(item) = session.queue.get_mut(*index) {
                        *item = prompt.clone().inherit_route(item);
                    }
                } else {
                    session.queue.remove(*index);
                }
            }
            _ => return Ok(false),
        }
        session.touch();
        self.changed.notify_all();
        Ok(true)
    }
    pub(super) fn list(&self) -> Result<Vec<Summary>> {
        let mut list: Vec<_> = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session store lock failed"))?
            .values()
            .map(Session::summary)
            .collect();
        list.sort_by(|a, b| {
            b.status
                .active()
                .cmp(&a.status.active())
                .then(b.updated.cmp(&a.updated))
                .then(a.id.cmp(&b.id))
        });
        Ok(list)
    }
}
pub struct Command {
    pub control: Control,
    pub reply: mpsc::Sender<Result<()>>,
}
struct Worker {
    sender: crate::wake::Sender<Command>,
    cancel: Cancel,
    handle: thread::JoinHandle<()>,
}
struct Service {
    store: Arc<Store>,
    remote: Arc<super::remote::Remote>,
    workers: Mutex<BTreeMap<String, Worker>>,
    action_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    launch_lock: Mutex<()>,
}

pub fn home(storage: &Storage) -> Result<PathBuf> {
    let home = storage
        .config
        .parent()
        .context("Missing config directory")?
        .join("agents");
    fs::create_dir_all(&home)?;
    ensure!(
        !fs::symlink_metadata(&home)?.file_type().is_symlink(),
        "Agent storage must not be a symlink"
    );
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
    Ok(home)
}
pub fn socket(storage: &Storage) -> Result<PathBuf> {
    Ok(home(storage)?.join("service.sock"))
}

impl Service {
    fn stop_worker(&self, id: &str) -> Result<()> {
        let worker = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
            .remove(id);
        if let Some(worker) = worker {
            worker.cancel.cancel();
            worker
                .handle
                .join()
                .map_err(|_| anyhow::anyhow!("Session worker stopped unexpectedly"))?;
        }
        self.store.update(id, |s| s.shells.clear())
    }

    fn start(&self, id: &str, initial: Option<Control>) -> Result<()> {
        let session = self.store.get(id)?;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker lock failed"))?;
        // Every reconnect checks under the worker lock, including messages that
        // raced with /mcp after a service upgrade. Never replace a live worker.
        if let Some(worker) = workers
            .get(id)
            .filter(|worker| !worker.handle.is_finished())
        {
            if let Some(control) = initial {
                let (reply, _) = mpsc::channel();
                worker
                    .sender
                    .send(Command { control, reply })
                    .context("Agent disconnected before the request was sent")?;
            }
            return Ok(());
        }
        if matches!(&initial, Some(Control::Mcp { .. })) {
            ensure!(
                session.queue.is_empty()
                    && session.turn_id.is_none()
                    && session.pending.is_empty()
                    && !session.switching_workspace,
                "Finish or resolve the session's pending work before reconnecting MCP"
            );
        }
        let cancel = Cancel::default();
        let (sender, receiver) = crate::wake::channel(cancel.wake());
        let token = cancel.clone();
        let store = self.store.clone();
        let id_owned = id.to_owned();
        // The map sender owns the session lifetime independently of connected TUIs.
        let handle = thread::Builder::new()
            .name(format!("difu-agent-{id}"))
            .spawn(move || {
                let output = (|| -> Result<()> {
                    let initial = if initial.is_none()
                        && session.thread_id.is_none()
                        && matches!(&session.job, Job::Coding(launch) if launch.prompt.is_empty())
                    {
                        super::startup::prepare(&store, &id_owned, &receiver, &token)?
                    } else {
                        initial
                    };
                    match session.job {
                        Job::Coding(_) => match store.get(&id_owned)?.provider {
                            provider::Provider::Codex => {
                                super::engine::run(&store, &id_owned, receiver, &token, initial)
                            }
                            provider::Provider::Claude => {
                                super::claude::run(&store, &id_owned, receiver, &token, initial)
                            }
                        },
                        _ => run_review(&store, &id_owned, &token),
                    }
                })();
                if let Err(error) = output {
                    let message = format!("{error:#}");
                    let _ = store.update(&id_owned, |s| {
                        if token.cancelled() {
                            if s.status.active() {
                                s.status = Status::Interrupted;
                            }
                            s.error = None;
                            s.note(
                                "system",
                                "Agent connection stopped; workspace edits retained",
                            );
                        } else {
                            s.status = Status::Failed;
                            s.error = Some(message.clone());
                            s.note("error", message.clone());
                        }
                        for text in std::mem::take(&mut s.queue) {
                            s.unsent(text);
                        }
                        s.pending.retain(Pending::is_async_question);
                        for entry in &mut s.entries {
                            if entry.kind == "sending" {
                                entry.kind = "unsent or unacknowledged".into();
                            }
                        }
                        s.turn_id = None;
                        s.switching_workspace = false;
                        s.registration_requests.clear();
                    });
                }
                if let Err(error) = store.save(&id_owned) {
                    eprintln!("Cannot save session {id_owned}: {error:#}");
                }
            })?;
        workers.insert(
            id.into(),
            Worker {
                sender,
                cancel,
                handle,
            },
        );
        Ok(())
    }
    fn insert_session(&self, mut job: Job, empty: bool) -> Result<String> {
        let entropy = tempfile::Builder::new()
            .prefix("id-")
            .tempfile_in(&self.store.home)?;
        let id = format!(
            "{}-{}",
            chrono::Utc::now().timestamp_millis(),
            storage::hash(entropy.path().as_os_str().as_encoded_bytes())
                .chars()
                .take(8)
                .collect::<String>()
        );
        if let Job::Coding(launch) = &mut job {
            launch.isolated = true;
        }
        let mut session = Session::new(id.clone(), job);
        super::rules::capture(&self.store.storage, &mut session)?;
        if empty {
            session.title = "New session".into();
        }
        storage::atomic_json(&self.store.home.join(format!("{id}.json")), &session)?;
        self.store
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session lock failed"))?
            .insert(id.clone(), session);
        self.start(&id, None)?;
        Ok(id)
    }
    fn handle(&self, request: Request) -> Result<Reply> {
        let (request, route) = match request {
            Request::SlackControl { id, control, route } => {
                (Request::Control { id, control }, Some(route))
            }
            request => (request, None),
        };
        let key = match &request {
            Request::Control { id, .. }
            | Request::Cleanup { id }
            | Request::Delete { id }
            | Request::Archive { id, .. }
            | Request::Repository { id, .. }
            | Request::Clear { id }
            | Request::RegisterWorktree { id, .. }
            | Request::Usage { id }
            | Request::Rename { id, .. } => Some(id.clone()),
            _ => None,
        };
        let action_lock = if let Some(id) = key {
            Some(
                self.action_locks
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Action lock failed"))?
                    .entry(id)
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone(),
            )
        } else {
            None
        };
        let _action_guard = action_lock
            .as_ref()
            .map(|lock| {
                lock.lock()
                    .map_err(|_| anyhow::anyhow!("Session action lock failed"))
            })
            .transpose()?;
        match request {
            Request::OrchestratorThread { route, title } => {
                let _launch = self
                    .launch_lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Launch lock failed"))?;
                let id = super::orchestrator::thread_id(&route);
                if self.store.inspect(&id, |s| s.orchestrator).unwrap_or(false) {
                    return Ok(Reply::Launched(id));
                }
                let session = super::orchestrator::thread_session(&self.store, route, &title)?;
                storage::atomic_json(&self.store.home.join(format!("{id}.json")), &session)?;
                self.store
                    .sessions
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Session lock failed"))?
                    .insert(id.clone(), session);
                self.store.changed.notify_all();
                Ok(Reply::Launched(id))
            }
            Request::SlackControl { .. } => anyhow::bail!("Unexpected nested Slack control"),
            Request::Ping => Ok(Reply::Ok),
            Request::ServiceVersion => Ok(Reply::ServiceVersion {
                version: env!("CARGO_PKG_VERSION").into(),
            }),
            Request::List => Ok(Reply::Sessions(self.store.list()?)),
            Request::Watch { .. } => anyhow::bail!("Watch requires a streaming connection"),
            Request::Read { id, version } => self.store.inspect(&id, |session| {
                if version == Some(session.version) {
                    Reply::Unchanged
                } else {
                    Reply::Session(Box::new(session.clone()))
                }
            }),
            Request::NewAgent {
                defaults,
                cwd,
                remember_repository,
            } => {
                let root = match super::workspace::repository(
                    defaults.repository.as_deref().unwrap_or(&cwd),
                    &Cancel::default(),
                ) {
                    Ok(root) => root,
                    Err(_) if defaults.repository.is_none() => return Ok(Reply::ChooseRepository),
                    Err(error) => return Err(error),
                };
                if remember_repository {
                    let mut config = self.store.storage.load_config()?;
                    config.agent_defaults.repository = Some(root.clone());
                    self.store.storage.save_config(&config)?;
                }
                let job = Job::Coding(Launch {
                    repository: root,
                    isolated: defaults.isolated,
                    base: if defaults.isolated {
                        "origin/main"
                    } else {
                        "HEAD"
                    }
                    .into(),
                    prompt: String::new(),
                    model: defaults.model,
                    effort: defaults.effort,
                });
                Ok(Reply::Launched(self.insert_session(job, true)?))
            }
            Request::Launch { job } => {
                let _launch_guard = self
                    .launch_lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Launch lock failed"))?;
                if let Some(identity) = job.identity() {
                    let sessions = self
                        .store
                        .sessions
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Session lock failed"))?;
                    if let Some(existing) = sessions
                        .values()
                        .find(|s| s.status.active() && s.job.identity().as_ref() == Some(&identity))
                    {
                        return Ok(Reply::Launched(existing.id.clone()));
                    }
                }
                let id = self.insert_session(*job, false)?;
                Ok(Reply::Launched(id))
            }
            Request::Control { id, control } => {
                let session = self.store.get(&id)?;
                ensure!(
                    !session.workspace_removed,
                    "This worktree was explicitly removed. Launch a new session from its retained branch to continue coding."
                );
                ensure!(
                    !session.archived || matches!(control, Control::Interrupt),
                    "Unarchive this session before continuing"
                );
                if route.is_some()
                    && matches!(
                        control,
                        Control::Message { .. }
                            | Control::MessageWithAttachments { .. }
                            | Control::Resume
                    )
                    && session.orchestrator
                    && (session.status.active() || !session.pending.is_empty())
                {
                    ensure!(
                        session
                            .slack_route
                            .as_ref()
                            .zip(route.as_ref())
                            .is_some_and(|(a, b)| a.account == b.account
                                && a.channel == b.channel
                                && a.thread == b.thread),
                        "The orchestrator is busy in another conversation; retry when it is ready"
                    );
                }
                // A queued task belongs to its originating thread. Changing the
                // active route here would send the current task's output elsewhere.
                if let Control::Message {
                    text,
                    queue: true,
                    skills,
                    attachments,
                }
                | Control::MessageWithAttachments {
                    text,
                    queue: true,
                    skills,
                    attachments,
                } = &control
                    && session.turn_id.is_some()
                {
                    ensure!(!text.trim().is_empty(), "Message cannot be empty");
                    self.store.update(&id, |s| {
                        s.queue.push(Prompt::Routed {
                            prompt: Box::new(Prompt::WithSkills {
                                text: text.clone(),
                                skills: skills.clone(),
                                attachments: attachments.clone(),
                            }),
                            origin: route.clone(),
                        })
                    })?;
                    self.store.save(&id)?;
                    return Ok(Reply::Ok);
                }
                if self.store.queue_preparing(&id, &control, &route)? {
                    self.store.save(&id)?;
                    return Ok(Reply::Ok);
                }
                if matches!(
                    control,
                    Control::Message { .. }
                        | Control::MessageWithAttachments { .. }
                        | Control::Resume
                ) || route.is_some()
                    && matches!(
                        control,
                        Control::AnswerQuestion { .. } | Control::Respond { .. }
                    )
                {
                    self.store
                        .update(&id, |s| s.set_slack_route(route.clone()))?;
                    self.store.save(&id)?;
                }
                let session = self.store.get(&id)?;
                if !matches!(session.job, Job::Coding(_)) {
                    ensure!(
                        matches!(control, Control::Interrupt),
                        "Review jobs support cancellation; retry them through Reviews"
                    );
                    if let Some(worker) = self
                        .workers
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                        .get(&id)
                    {
                        worker.cancel.cancel();
                    }
                    return Ok(Reply::Ok);
                }
                if let Control::AnswerQuestion {
                    request,
                    question,
                    answer: None,
                } = &control
                    && session
                        .pending
                        .iter()
                        .any(|p| p.id == *request && p.is_async_question())
                {
                    let (updated, _) =
                        super::questions::prepare_answer(&session, request, question, None)?;
                    super::questions::save_answer(&self.store, &id, updated)?;
                    return Ok(Reply::Ok);
                }
                if let Control::Model { model, effort } = &control {
                    let target = model
                        .as_deref()
                        .map(|model| provider::Provider::for_model(Some(model)))
                        .unwrap_or(session.provider);
                    if target != session.provider || target == provider::Provider::Claude {
                        ensure!(
                            matches!(
                                session.status,
                                Status::Idle | Status::Interrupted | Status::Failed
                            ) && session.turn_id.is_none()
                                && session.pending.is_empty()
                                && session.queue.is_empty()
                                && !session.switching_workspace,
                            "Finish the current turn and pending requests before switching models or providers"
                        );
                        if let Some(worker) = self
                            .workers
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                            .remove(&id)
                        {
                            worker.cancel.cancel();
                            worker.handle.join().map_err(|_| {
                                anyhow::anyhow!("Session worker stopped unexpectedly")
                            })?;
                        }
                        let mut updated = self.store.get(&id)?;
                        if target != updated.provider {
                            provider::switch(&mut updated, target, model.clone(), effort.clone())?;
                        } else {
                            updated.model = model.clone();
                            updated.effort = effort.clone();
                            updated.status = Status::Idle;
                            updated.error = None;
                            if let Job::Coding(launch) = &mut updated.job {
                                launch.model = model.clone();
                                launch.effort = effort.clone();
                            }
                        }
                        self.store.update(&id, |s| *s = updated)?;
                        self.store.save(&id)?;
                        return Ok(Reply::Ok);
                    }
                }
                let async_response = matches!(&control, Control::Respond { request, .. } | Control::AnswerQuestion { request, .. }
                    if session.pending.iter().any(|p| p.id == *request && p.is_async_question()));
                let sends_message = matches!(
                    control,
                    Control::Message { .. } | Control::MessageWithAttachments { .. }
                ) || async_response;
                if matches!(session.status, Status::Interrupted | Status::Failed) {
                    ensure!(
                        sends_message || matches!(control, Control::Resume | Control::Model { .. }),
                        "This session was interrupted; send a message or choose Continue to resume"
                    );
                }
                let connected = self
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                    .get(&id)
                    .map(|w| w.sender.clone());
                let live = self
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                    .get(&id)
                    .is_some_and(|w| !w.handle.is_finished());
                if !live && let Control::Model { model, effort } = &control {
                    self.store.update(&id, |s| {
                        s.model = model.clone();
                        s.effort = effort.clone();
                        if let Job::Coding(launch) = &mut s.job {
                            launch.model = model.clone();
                            launch.effort = effort.clone();
                        }
                    })?;
                    self.store.save(&id)?;
                    return Ok(Reply::Ok);
                }
                if session.status == Status::Starting && matches!(control, Control::Interrupt) {
                    if let Some(worker) = self
                        .workers
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                        .get(&id)
                    {
                        worker.cancel.cancel();
                    }
                    return Ok(Reply::Ok);
                }
                ensure!(
                    session.thread_id.is_some() || !matches!(control, Control::Compact),
                    "Send the first message to start this session"
                );
                let (reply, result) = mpsc::channel();
                let command = Command {
                    control: control.clone(),
                    reply,
                };
                match connected.map(|sender| sender.send(command).is_ok()) {
                    Some(true) => result.recv_timeout(Duration::from_secs(60)).context(
                        "Agent did not acknowledge the action; inspect its session before retrying",
                    )??,
                    _ => {
                        ensure!(
                            matches!(control, Control::Resume)
                                || (sends_message
                                    && matches!(
                                        session.status,
                                        Status::Interrupted | Status::Failed
                                    ))
                                || (session.status == Status::Idle
                                    && (async_response
                                        || matches!(
                                            control,
                                            Control::Message { .. }
                                                | Control::MessageWithAttachments { .. }
                                                | Control::Model { .. }
                                                | Control::Compact
                                        ))),
                            "Session is disconnected; send a message or choose Continue to reconnect"
                        );
                        self.store.update(&id, |s| {
                            s.status = Status::Starting;
                            s.error = None;
                            if let Control::Message { text, skills, attachments, .. }
                                | Control::MessageWithAttachments { text, skills, attachments, .. } = &control {
                                s.note("awaiting connection", text);
                                if let Some(entry) = s.entries.last_mut() {
                                    entry.data = serde_json::json!({"prompt":Prompt::WithSkills {
                                        text:text.clone(), skills:skills.clone(), attachments:attachments.clone()
                                    }}).into();
                                }
                            }
                        })?;
                        self.store.save(&id)?;
                        self.start(&id, Some(control))?;
                    }
                }
                Ok(Reply::Ok)
            }
            Request::Mcp { id, action } => {
                let session = self.store.get(&id)?;
                ensure!(
                    matches!(session.job, Job::Coding(_)),
                    "MCP controls are available for coding sessions"
                );
                ensure!(
                    !session.archived && !session.preparing(),
                    "Wait until the session is ready before managing MCP servers"
                );
                let (inventory, result) = mpsc::channel();
                self.start(
                    &id,
                    Some(Control::Mcp {
                        action,
                        response: inventory,
                    }),
                )?;
                Ok(Reply::Mcp(
                    result
                        .recv_timeout(Duration::from_secs(60))
                        .context(
                            "Agent did not return MCP status; check its connection and refresh",
                        )?
                        .map_err(anyhow::Error::msg)?,
                ))
            }
            Request::Usage { id } => {
                let session = self.store.get(&id)?;
                ensure!(
                    matches!(session.job, Job::Coding(_)),
                    "Usage is available for coding sessions"
                );
                let sender = self
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                    .get(&id)
                    .filter(|w| !w.handle.is_finished())
                    .map(|w| w.sender.clone());
                if let Some(sender) = sender {
                    let (reply, response) = mpsc::channel();
                    sender
                        .send(Command {
                            control: Control::ReadUsage,
                            reply,
                        })
                        .context("Provider disconnected before usage was read")?;
                    response
                        .recv_timeout(Duration::from_secs(60))
                        .context("Provider did not return usage in time")??;
                    return Ok(Reply::Usage(self.store.get(&id)?.usage));
                }
                let cancel = Cancel::default();
                let usage = match session.provider {
                    provider::Provider::Codex => super::engine::usage(&session, &cancel)?,
                    provider::Provider::Claude => {
                        super::claude::usage(&self.store, &id, &session, &cancel)?
                    }
                };
                Ok(Reply::Usage(usage))
            }
            Request::RegisterWorktree { id, path, base } => {
                let session = self.store.get(&id)?;
                ensure!(
                    !session.status.active()
                        && session.turn_id.is_none()
                        && session.pending.is_empty()
                        && session.queue.is_empty()
                        && !session.switching_workspace
                        && !session.archived,
                    "Wait for an idle session with no pending requests before registering a worktree"
                );
                let prepared = super::registration::prepare(
                    &session,
                    &serde_json::json!({"path":path,"base":base}),
                    &Cancel::default(),
                )?;
                if let Some(worker) = self
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                    .remove(&id)
                {
                    worker.cancel.cancel();
                    worker
                        .handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("Session worker stopped unexpectedly"))?;
                }
                self.store.update(&id, |s| {
                    super::registration::apply(s, &prepared);
                    s.status = Status::Idle;
                    s.error = None;
                    s.shells.clear();
                    s.suggestion = None;
                    s.suggestion_attempted = None;
                    s.note("system", format!("Active worktree: {}. Resume in this workspace and re-read repository instructions.", prepared.workspace.path.display()));
                })?;
                self.store.save(&id)?;
                Ok(Reply::Session(Box::new(self.store.get(&id)?)))
            }
            Request::Repository { id, repository } => {
                let previous = self.store.get(&id)?;
                let mut candidate = previous.clone();
                ensure!(
                    !candidate.archived,
                    "Unarchive the session before changing repository"
                );
                let Job::Coding(launch) = &mut candidate.job else {
                    anyhow::bail!("Only coding sessions can change repository");
                };
                launch.repository = repository;
                candidate.baseline = None;
                super::workspace::inspect(&mut candidate, &Cancel::default())?;
                super::rules::capture(&self.store.storage, &mut candidate)?;
                let owned = self.store.home.join("worktrees").join(&id);
                if owned.try_exists()? {
                    ensure!(
                        !candidate.job.root().starts_with(owned.canonicalize()?),
                        "Select a repository outside the worktree being discarded"
                    );
                }
                // Validate first so an invalid path leaves the current connection intact.
                self.stop_worker(&id)?;
                // Preparation can record a branch/worktree while the new path is being
                // validated. Join the cancelled worker before reading cleanup ownership.
                let previous = self.store.get(&id)?;
                super::workspace::discard(&previous, &self.store.home, &Cancel::default())?;
                let revision = previous.workspace_revision.saturating_add(1);
                super::pr_cache::disconnect(&self.store.storage, &id, revision)?;
                self.store.update(&id, |s| {
                    reset_workspace(s, candidate, revision);
                    s.note("system", format!("Repository changed to {}. Previous local work, artifacts, and PR associations were discarded. Re-read repository instructions before continuing.", s.job.root().display()));
                })?;
                self.store.save(&id)?;
                Ok(Reply::Session(Box::new(self.store.get(&id)?)))
            }
            Request::Clear { id } => {
                let mut candidate = self.store.get(&id)?;
                ensure!(
                    !candidate.archived,
                    "Unarchive the session before clearing it"
                );
                ensure!(
                    !candidate.workspace_removed,
                    "This worktree was explicitly removed. Launch a new session from its retained branch to continue coding."
                );
                ensure!(
                    !candidate.orchestrator
                        && matches!(&candidate.job, Job::Coding(launch) if launch.isolated),
                    "Only worktree coding sessions can be cleared"
                );
                candidate.baseline = None;
                super::workspace::inspect(&mut candidate, &Cancel::default())?;
                super::rules::capture(&self.store.storage, &mut candidate)?;
                let owned = self.store.home.join("worktrees").join(&id);
                if owned.try_exists()? {
                    ensure!(
                        !candidate.job.root().starts_with(owned.canonicalize()?),
                        "The session repository is inside the worktree being discarded"
                    );
                }
                // Validate first so a missing repository leaves the session intact.
                self.stop_worker(&id)?;
                let previous = self.store.get(&id)?;
                super::workspace::discard(&previous, &self.store.home, &Cancel::default())?;
                let revision = previous.workspace_revision.saturating_add(1);
                super::pr_cache::disconnect(&self.store.storage, &id, revision)?;
                self.store.update(&id, |s| {
                    reset_workspace(s, candidate, revision);
                    clear_conversation(s);
                    s.status = Status::Starting;
                    s.note(
                        "system",
                        "Session cleared. Preparing a fresh worktree from the latest origin/main.",
                    );
                })?;
                self.store.save(&id)?;
                // The empty-session path fetches origin/main and prepares the worktree
                // without starting a provider until the next message.
                self.start(&id, None)?;
                Ok(Reply::Session(Box::new(self.store.get(&id)?)))
            }
            Request::Rename { id, title } => {
                ensure!(!title.trim().is_empty(), "Session name cannot be empty");
                self.store.update(&id, |s| {
                    s.title = title.trim().chars().take(200).collect();
                    s.title_manual = true;
                })?;
                self.store.save(&id)?;
                Ok(Reply::Ok)
            }
            Request::Archive { id, archived } => {
                if archived {
                    self.stop_worker(&id)?;
                }
                self.store.update(&id, |s| s.archived = archived)?;
                self.store.save(&id)?;
                Ok(Reply::Ok)
            }
            Request::Shells { id } => {
                let sender = self
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                    .get(&id)
                    .filter(|w| !w.handle.is_finished())
                    .map(|w| w.sender.clone());
                let Some(sender) = sender else {
                    return Ok(Reply::Shells(Vec::new()));
                };
                let (tx, rx) = mpsc::channel();
                sender.send(Command {
                    control: Control::RefreshShells,
                    reply: tx,
                })?;
                rx.recv_timeout(Duration::from_secs(50))
                    .context("Shell list unavailable while the agent is busy")??;
                Ok(Reply::Shells(self.store.get(&id)?.shells))
            }
            Request::Delete { id } => {
                let session = self.store.get(&id)?;
                ensure!(
                    matches!(session.job, Job::Coding(_)),
                    "Only coding chats can be deleted here"
                );
                let result = (|| -> Result<Reply> {
                    self.store
                        .update(&id, |s| s.deletion_progress = Some(DeletionStage::Stopping))?;
                    // Confirmation explicitly authorizes stopping the session before cleanup.
                    self.stop_worker(&id)?;
                    self.store.update(&id, |s| {
                        if s.status.active() {
                            s.status = Status::Interrupted;
                        }
                        s.turn_id = None;
                        s.deletion_requests.clear();
                    })?;
                    self.store
                        .update(&id, |s| s.deletion_progress = Some(DeletionStage::Worktree))?;
                    let session = self.store.get(&id)?;
                    let owned = self.store.home.join("worktrees").join(&id);
                    if owned.try_exists()?
                        && matches!(&session.job, Job::Coding(launch) if launch.isolated)
                    {
                        // Registered external worktrees are retained. The session's
                        // original managed worktree remains owned after switching.
                        let mut managed = session.clone();
                        managed.workspace = Some(owned);
                        super::workspace::delete(&managed, &self.store.home, &Cancel::default())?;
                    }
                    self.store.update(&id, |s| {
                        s.deletion_progress = Some(DeletionStage::Attachments)
                    })?;
                    super::media::cleanup(&self.store.storage, &id)?;
                    self.store
                        .update(&id, |s| s.deletion_progress = Some(DeletionStage::History))?;
                    let _guard = self
                        .store
                        .save_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Persistence lock failed"))?;
                    fs::remove_file(self.store.home.join(format!("{id}.json")))?;
                    self.store
                        .sessions
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Session lock failed"))?
                        .remove(&id);
                    self.store.changed.notify_all();
                    Ok(Reply::Ok)
                })();
                if result.is_err() {
                    self.store.update(&id, |s| s.deletion_progress = None)?;
                    self.store.save(&id)?;
                }
                result
            }
            Request::Cleanup { id } => {
                let session = self.store.get(&id)?;
                ensure!(!session.status.active(), "Active workspaces are protected");
                super::workspace::validate_cleanup(&session, &self.store.home, &Cancel::default())?;
                // Disconnect the idle engine before removing its cwd.
                if let Some(worker) = self
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Worker lock failed"))?
                    .remove(&id)
                {
                    worker.cancel.cancel();
                    worker
                        .handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("Session worker stopped unexpectedly"))?;
                }
                super::workspace::cleanup(&session, &self.store.home, &Cancel::default())?;
                self.store.update(&id, |s| {
                    s.archived = true;
                    s.workspace_removed = true;
                    s.note("system", "Worktree removed; branch and commits retained");
                })?;
                self.store.save(&id)?;
                super::media::cleanup(&self.store.storage, &id)?;
                Ok(Reply::Ok)
            }
            Request::Changes { id } => Ok(Reply::Changes(super::workspace::changes(
                &self.store.get(&id)?,
                &self.store.storage,
                &Cancel::default(),
            )?)),
            Request::PrChanges { id, pr } => {
                ensure!(
                    matches!(self.store.get(&id)?.job, Job::Coding(_)),
                    "PR changes are available for coding sessions"
                );
                Ok(Reply::Changes(crate::github::pr_diff(
                    &pr,
                    &Cancel::default(),
                )?))
            }
            Request::Statistics { id } => Ok(Reply::Statistics(super::workspace::statistics(
                &self.store.get(&id)?,
                &self.store.storage,
                &Cancel::default(),
            )?)),
            Request::WorkspacePaths { id } => Ok(Reply::WorkspacePaths(super::workspace::paths(
                &self.store.get(&id)?,
                &Cancel::default(),
            )?)),
            Request::Defaults { cwd } => super::engine::defaults(&cwd),
            Request::Remote { command } => Ok(Reply::Remote(self.remote.command(command)?)),
            Request::Skills { id, force } => {
                let session = self.store.get(&id)?;
                ensure!(
                    matches!(session.job, Job::Coding(_)),
                    "Skills are available for coding agents"
                );
                let cwd = session
                    .workspace
                    .as_deref()
                    .context("Workspace is still being prepared")?;
                match session.provider {
                    provider::Provider::Codex => super::engine::skills(cwd, force),
                    // Each listing starts Claude Code, which rereads skills from disk.
                    provider::Provider::Claude => super::claude::skills(cwd),
                }
            }
        }
    }
}

/// A replacement worktree drops the previous workspace's requests, artifacts and
/// PR associations. Queued prompts remain visible as unsent messages.
fn reset_workspace(s: &mut Session, candidate: Session, revision: u64) {
    s.status = Status::Idle;
    s.turn_id = None;
    s.turn_started_at = None;
    s.switching_workspace = false;
    for prompt in std::mem::take(&mut s.queue) {
        s.unsent(prompt);
    }
    s.pending.clear();
    s.workspace_requests.clear();
    s.registration_requests.clear();
    s.artifact_requests.clear();
    s.question_requests.clear();
    s.deletion_requests.clear();
    s.job = candidate.job;
    s.repository_rules = candidate.repository_rules;
    s.workspace = candidate.workspace;
    s.baseline = candidate.baseline;
    s.workspace_ready = false;
    s.workspace_removed = false;
    s.deferred_workspace = false;
    s.workspaces.clear();
    s.workspace_revision = revision;
    s.branch = None;
    s.comparison_base = None;
    s.artifacts.clear();
    s.guidance_checked = false;
    s.shells.clear();
    s.suggestion = None;
    s.suggestion_attempted = None;
    s.result = None;
    s.error = None;
}

/// Keep the session's identity, provider, model and manual name, but nothing it
/// learned: the next message starts a new native thread with no handoff context.
fn clear_conversation(s: &mut Session) {
    s.entries.clear();
    s.slack_route = None;
    s.slack_after = 0;
    s.thread_id = None;
    s.provider_threads.clear();
    s.provider_context = None;
    s.completed_turn = None;
    s.token_usage = Value::Null;
    s.usage = Value::Null;
    s.answered_questions.clear();
    s.permissions = Value::Null;
    s.inherited_permissions = Value::Null;
    s.artifact_tools = false;
    s.registration_tools = false;
    s.question_tools = false;
    s.peer_tools = false;
    s.native_setup = true;
    s.guide = None;
    if let Job::Coding(launch) = &mut s.job {
        launch.prompt.clear();
    }
    s.title_ready = false;
    s.title_attempted = false;
    s.title_response.clear();
    if !s.title_manual {
        s.title = "New session".into();
    }
}

fn run_review(store: &Arc<Store>, id: &str, cancel: &Cancel) -> Result<()> {
    store.update(id, |s| s.status = Status::Running)?;
    let session = store.get(id)?;
    let progress_store = store.clone();
    let progress_id = id.to_owned();
    let progress = move |message: String| {
        let _ = progress_store.update(&progress_id, |s| s.note("progress", message));
    };
    match session.job {
        Job::Guide {
            root,
            pr,
            snapshot,
            model,
        } => {
            let guide = crate::codex::generate(
                &root,
                &pr,
                &snapshot,
                &model,
                &store.storage,
                cancel,
                progress,
            )?;
            store.update(id, |s| {
                s.guide = Some(guide);
                s.result = Some("Guide ready".into());
                s.status = Status::Completed;
            })?;
        }
        Job::Conflict {
            root,
            key,
            head,
            model,
        } => {
            let result =
                crate::conflicts::resolve(&root, &key, &head, &model, cancel, Arc::new(progress))?;
            store.update(id, |s| {
                s.result = Some(result.clone());
                s.note("result", result);
                s.status = Status::Completed;
            })?;
        }
        Job::Coding(_) => anyhow::bail!("Coding job routed to the review worker"),
    }
    Ok(())
}

fn ensure_orchestrator(store: &Store) -> Result<()> {
    if store
        .inspect(super::orchestrator::ID, |s| s.orchestrator_generation > 0)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let session = super::orchestrator::session(store)?;
    store
        .sessions
        .lock()
        .map_err(|_| anyhow::anyhow!("Session lock failed"))?
        .insert(session.id.clone(), session);
    store.save(super::orchestrator::ID)
}

pub fn run(storage: Storage) -> Result<()> {
    let home = home(&storage)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(home.join("service.lock"))?;
    let _lease: Flock<File> = Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, e)| anyhow::anyhow!("Agent service already running: {e}"))?;
    let path = socket(&storage)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    let listener =
        UnixListener::bind(&path).context("Cannot bind the private agent service socket")?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let mut sessions = BTreeMap::new();
    for entry in fs::read_dir(&home)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let mut session: Session = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("Cannot restore session {}", path.display()))?;
        session.deletion_progress = None;
        ensure!(
            path.file_stem().and_then(|s| s.to_str()) == Some(&session.id),
            "Invalid session storage identity"
        );
        // Replace the legacy shared manager once. Its provider thread, queued
        // actions and history must not enter any of the new conversations.
        if session.id == super::orchestrator::ID && session.orchestrator_generation == 0 {
            continue;
        }
        if session.status.active() {
            session.status = Status::Interrupted;
            session.turn_id = None;
            session.pending.retain(Pending::is_async_question);
            session.note("system", "Service restarted. Work was interrupted; send a message or choose Continue to resume. No prompt or publication action was replayed.");
        }
        for entry in &mut session.entries {
            if entry.kind == "sending" {
                entry.kind = "unsent or unacknowledged".into();
            }
        }
        // Queued messages remain visible, but never replay after a service restart.
        if !session.queue.is_empty() {
            let queued = std::mem::take(&mut session.queue);
            for text in queued {
                session.unsent(text);
            }
        }
        session.restore_async_questions();
        storage::atomic_json(&path, &session)?;
        sessions.insert(session.id.clone(), session);
    }
    let remote = super::remote::Remote::start(storage.clone());
    let service = Arc::new(Service {
        remote,
        store: Arc::new(Store {
            home,
            storage,
            sessions: Mutex::new(sessions),
            changed: Condvar::new(),
            save_lock: Mutex::new(()),
        }),
        workers: Mutex::new(BTreeMap::new()),
        action_locks: Mutex::new(BTreeMap::new()),
        launch_lock: Mutex::new(()),
    });
    ensure_orchestrator(&service.store)?;
    let _pr_cache =
        super::pr_cache::Refresher::start(service.store.storage.clone(), service.store.clone())?;
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, stopped.clone())?;
    }
    super::slack::start(service.store.clone(), stopped.clone());
    let mut saved = BTreeMap::new();
    let mut flushed = Instant::now();
    while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let service = service.clone();
                thread::spawn(move || {
                    if let Err(error) = serve(&service, stream) {
                        eprintln!("Agent client: {error:#}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(error.into()),
        }
        if flushed.elapsed() < Duration::from_millis(500) {
            continue;
        }
        flushed = Instant::now();
        for session in service.store.list()? {
            if saved.get(&session.id) != Some(&session.version) {
                service.store.save(&session.id)?;
                saved.insert(session.id, session.version);
            }
        }
    }
    let workers = std::mem::take(
        &mut *service
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker lock failed"))?,
    );
    for worker in workers.values() {
        worker.cancel.cancel();
    }
    for (_, worker) in workers {
        worker
            .handle
            .join()
            .map_err(|_| anyhow::anyhow!("Agent worker stopped unexpectedly"))?;
    }
    // Persist interrupted state before exit; no queued prompt or mutation is replayed.
    for session in service.store.list()? {
        if session.status.active() {
            service.store.update(&session.id, |s| {
                s.status = Status::Interrupted;
                s.pending.retain(Pending::is_async_question);
                s.turn_id = None;
            })?;
        }
        service.store.save(&session.id)?;
    }
    fs::remove_file(path)?;
    Ok(())
}
fn serve(service: &Service, mut stream: UnixStream) -> Result<()> {
    // The listener is nonblocking. On macOS accepted sockets inherit that flag;
    // request JSON may arrive in several writes, so read it in blocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let request: Request = serde_json::from_str(&super::client::read_line(&mut reader)?)?;
    if let Request::Watch { id, version } = request {
        return watch(&service.store, &mut stream, &id, version);
    }
    let reply = service
        .handle(request)
        .unwrap_or_else(|e| Reply::Error(format!("{e:#}")));
    super::transport::write_reply(&mut stream, &reply)?;
    Ok(())
}

fn watch(store: &Store, stream: &mut UnixStream, id: &str, _version: Option<u64>) -> Result<()> {
    let mut previous: Option<Session> = None;
    // Each connection starts with a complete baseline, then sends only changes.
    let mut version = None;
    loop {
        let sessions = store
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session lock failed"))?;
        // Predicate and updates share the same lock, so a change between replies
        // cannot be lost. Heartbeats detect disconnected/hidden UI subscribers.
        let (sessions, _) = store
            .changed
            .wait_timeout_while(sessions, Duration::from_secs(1), |sessions| {
                sessions.get(id).is_some_and(|s| Some(s.version) == version)
            })
            .map_err(|_| anyhow::anyhow!("Session lock failed"))?;
        let reply = match sessions.get(id) {
            Some(session) if Some(session.version) != version => {
                version = Some(session.version);
                let reply = match &previous {
                    Some(previous) => {
                        Reply::SessionDelta(super::transport::Delta::between(previous, session))
                    }
                    None => Reply::Session(Box::new(session.clone())),
                };
                previous = Some(session.clone());
                reply
            }
            Some(_) => Reply::Unchanged,
            None => Reply::Error("Session not found".into()),
        };
        drop(sessions);
        // Never hold the provider's session lock during socket I/O.
        super::transport::write_reply(&mut *stream, &reply)?;
        if matches!(reply, Reply::Error(_)) {
            return Ok(());
        }
        // Coalesce token bursts; a slow UI never builds an unbounded frame queue.
        thread::sleep(crate::repaint::FRAME_INTERVAL);
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    #[test]
    fn subscription_delivers_changes_without_list_polling_and_disconnects() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: temp.path().into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        let store = Arc::new(Store {
            home: temp.path().into(),
            storage: Storage {
                config: temp.path().join("config.json"),
                cache: temp.path().join("cache"),
            },
            sessions: Mutex::new(BTreeMap::from([("one".into(), session)])),
            changed: Condvar::new(),
            save_lock: Mutex::new(()),
        });
        let (client, mut server) = UnixStream::pair()?;
        client.set_read_timeout(Some(Duration::from_secs(2)))?;
        server.set_write_timeout(Some(Duration::from_secs(2)))?;
        let watching = store.clone();
        let (done, finished) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = watch(&watching, &mut server, "one", None);
            let _ = done.send(());
        });
        let mut reader = BufReader::new(client);
        let first = super::super::transport::read_reply(&mut reader)?;
        assert!(matches!(first, Reply::Session(s) if s.version == 0));
        // Update while the subscriber is between writes/waits. The predicate
        // must catch the change even if the notification precedes the wait.
        store.update("one", |s| s.note("agentMessage", "streamed"))?;
        let next = super::super::transport::read_reply(&mut reader)?;
        assert!(
            matches!(next, Reply::SessionDelta(s) if s.session.version > 0 && s.entries.last().is_some_and(|(_,e)| e.text == "streamed"))
        );
        reader.get_ref().shutdown(std::net::Shutdown::Both)?;
        drop(reader);
        finished.recv_timeout(Duration::from_secs(5))?;
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("watch panicked"))?;
        Ok(())
    }
}

#[cfg(test)]
mod routed_queue_tests {
    use super::*;
    #[test]
    fn replacing_legacy_manager_is_one_time_and_threads_reuse_only_their_own_context() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let store = Arc::new(Store::fixture(temp.path().into(), Vec::new()));
        let mut old = super::super::orchestrator::session(&store)?;
        old.orchestrator_generation = 0;
        old.thread_id = Some("old-provider-thread".into());
        old.note("agentMessage", "legacy shared transcript");
        old.queue.push(Prompt::from("must not replay"));
        store
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .insert(old.id.clone(), old);
        ensure_orchestrator(&store)?;
        let fresh = store.get(super::super::orchestrator::ID)?;
        assert!(fresh.entries.is_empty() && fresh.queue.is_empty() && fresh.thread_id.is_none());
        store.update(&fresh.id, |s| {
            s.note("userMessage", "new local conversation")
        })?;
        ensure_orchestrator(&store)?;
        assert_eq!(store.get(&fresh.id)?.entries.len(), 1);
        let service = Service {
            remote: super::remote::Remote::start(store.storage.clone()),
            store,
            workers: Mutex::new(BTreeMap::new()),
            action_locks: Mutex::new(BTreeMap::new()),
            launch_lock: Mutex::new(()),
        };
        let mut route = slack::Route {
            account: "account".into(),
            channel: "D1".into(),
            thread: "1.0".into(),
            event: "first".into(),
        };
        let request = |route| Request::OrchestratorThread {
            route,
            title: "First task".into(),
        };
        let Reply::Launched(first) = service.handle(request(route.clone()))? else {
            anyhow::bail!("missing conversation")
        };
        service
            .store
            .update(&first, |s| s.note("userMessage", "thread-specific context"))?;
        route.event = "followup".into();
        let Reply::Launched(continued) = service.handle(request(route.clone()))? else {
            anyhow::bail!("missing conversation")
        };
        assert_eq!(first, continued);
        assert_eq!(service.store.get(&continued)?.entries.len(), 1);
        route.thread = "2.0".into();
        let Reply::Launched(second) = service.handle(request(route))? else {
            anyhow::bail!("missing conversation")
        };
        assert_ne!(first, second);
        assert!(service.store.get(&second)?.entries.is_empty());
        assert!(
            service
                .workers
                .lock()
                .map_err(|_| anyhow::anyhow!("lock"))?
                .is_empty()
        );
        Ok(())
    }
    #[test]
    fn queueing_another_threads_task_does_not_retarget_active_output() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut session = Session::new(
            "worker".into(),
            Job::Coding(Launch {
                repository: temp.path().into(),
                isolated: false,
                base: String::new(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Running;
        session.turn_id = Some("turn".into());
        let first = slack::Route {
            account: "account".into(),
            channel: "D1".into(),
            thread: "1.0".into(),
            event: "e1".into(),
        };
        let second = slack::Route {
            thread: "2.0".into(),
            event: "e2".into(),
            ..first.clone()
        };
        session.slack_route = Some(first.clone());
        session.note("agentMessage", "first task output");
        let service = Service {
            remote: super::remote::Remote::start(Storage {
                config: temp.path().join("config.json"),
                cache: temp.path().join("cache"),
            }),
            store: Arc::new(Store::fixture(temp.path().into(), vec![session])),
            workers: Mutex::new(BTreeMap::new()),
            action_locks: Mutex::new(BTreeMap::new()),
            launch_lock: Mutex::new(()),
        };
        service.handle(Request::SlackControl {
            id: "worker".into(),
            route: second.clone(),
            control: Control::Message {
                text: "second task".into(),
                queue: true,
                skills: Vec::new(),
                attachments: Vec::new(),
            },
        })?;
        let mut current = service.store.get("worker")?;
        assert_eq!(current.slack_route, Some(first.clone()));
        let queued = current.queue.pop().context("queued task")?;
        let Prompt::Routed { origin, .. } = &queued else {
            anyhow::bail!("Missing queued origin")
        };
        assert_eq!(origin.as_ref(), Some(&second));
        let replacement = Prompt::from("edited task").inherit_route(&queued);
        assert!(matches!(replacement,Prompt::Routed{origin:Some(ref route),..} if route==&second));
        current.set_slack_route(origin.clone());
        let saved = current
            .entries
            .first()
            .and_then(|e| e.data.get("difuSlackRoute"))
            .context("original output route")?;
        assert_eq!(
            serde_json::from_value::<slack::Route>(saved.clone())?,
            first
        );
        assert_eq!(current.slack_route, Some(second));
        Ok(())
    }
}

#[cfg(test)]
mod clear_tests {
    use super::*;
    use crate::{
        agents::pr_cache::{SESSION_LINKS, SessionLink, SessionLinks},
        github::SessionPr,
        model::PrKey,
    };
    fn git(root: &std::path::Path, args: &[&str]) -> Result<String> {
        crate::process::checked(
            crate::repo::git(root)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args),
            &Cancel::default(),
        )
        .map(|s| s.trim().to_owned())
    }
    #[test]
    fn clear_discards_context_links_and_work_then_prepares_fresh_main() -> Result<()> {
        for manual_title in [false, true] {
            let temp = tempfile::tempdir()?;
            let root = temp.path().canonicalize()?;
            let (origin, local, home) =
                (root.join("origin"), root.join("local"), root.join("home"));
            for path in [&origin, &local] {
                fs::create_dir(path)?;
                git(path, &["init", "-b", "main"])?;
                fs::write(path.join("tracked.txt"), "base\n")?;
                git(path, &["add", "."])?;
                git(path, &["commit", "-m", "base"])?;
            }
            git(
                &local,
                &[
                    "remote",
                    "add",
                    "origin",
                    origin.to_str().context("origin")?,
                ],
            )?;
            fs::write(local.join("tracked.txt"), "user's own checkout edit\n")?;
            let local_head = git(&local, &["rev-parse", "HEAD"])?;
            let mut session = Session::new(
                "one".into(),
                Job::Coding(Launch {
                    repository: local.clone(),
                    isolated: true,
                    base: "origin/main".into(),
                    prompt: "Original task".into(),
                    model: Some("chosen-model".into()),
                    effort: None,
                }),
            );
            let cancel = Cancel::default();
            super::super::workspace::inspect(&mut session, &cancel)?;
            super::super::workspace::prepare(&mut session, &home, &cancel, |_| Ok(()))?;
            let tree = home.join("worktrees").join("one");
            assert_eq!(session.workspace.as_deref(), Some(tree.as_path()));
            // Unpushed commit, uncommitted edit and untracked file in the old worktree.
            fs::write(tree.join("tracked.txt"), "old task commit\n")?;
            git(&tree, &["commit", "-am", "old task"])?;
            fs::write(tree.join("tracked.txt"), "old uncommitted edit\n")?;
            fs::write(tree.join("untracked.txt"), "old untracked\n")?;
            session.status = Status::Idle;
            session.thread_id = Some("old-thread".into());
            session
                .provider_threads
                .insert("claude".into(), Default::default());
            session.provider_context = Some("old handoff".into());
            session.note("userMessage", "earlier private context");
            session.queue.push(Prompt::from("queued for the old task"));
            session.answered_questions.insert("old-question".into());
            session.token_usage = serde_json::json!({"total": 1});
            session.artifacts.push(super::super::artifacts::Artifact {
                path: "report.html".into(),
                title: "Old report".into(),
                workspace: Some(tree.clone()),
            });
            session.title = "Old task title".into();
            session.title_ready = true;
            session.title_attempted = true;
            session.title_manual = manual_title;
            let store = Arc::new(Store::fixture(home.clone(), vec![session.clone()]));
            fs::create_dir_all(&store.storage.cache)?;
            let link = SessionLink {
                workspace: tree.clone(),
                workspace_revision: 0,
                pr: SessionPr {
                    summary: None,
                    key: PrKey {
                        owner: "example".into(),
                        repo: "project".into(),
                        number: 7,
                    },
                    state: "OPEN".into(),
                    draft: false,
                    conflicts: false,
                    head_branch: "difu/agent-one".into(),
                    head: git(&tree, &["rev-parse", "HEAD"])?,
                    updated: "2026-10-01T00:00:00Z".into(),
                },
            };
            crate::github::store::session(&store.storage, &link.pr)?;
            let links_path = store.storage.cache.join(SESSION_LINKS);
            let links: SessionLinks = [("one".to_owned(), vec![link])].into_iter().collect();
            super::super::pr_cache::save_links(&store.storage, &links_path, &links)?;
            assert!(super::super::pr_cache::load_links(&links_path).contains_key("one"));
            fs::write(origin.join("tracked.txt"), "fresh remote main\n")?;
            git(&origin, &["commit", "-am", "advance main"])?;
            let fresh = git(&origin, &["rev-parse", "HEAD"])?;
            let service = Service {
                remote: super::remote::Remote::start(store.storage.clone()),
                store: store.clone(),
                workers: Mutex::new(BTreeMap::new()),
                action_locks: Mutex::new(BTreeMap::new()),
                launch_lock: Mutex::new(()),
            };

            let Reply::Session(_) = service.handle(Request::Clear { id: "one".into() })? else {
                anyhow::bail!("clear returned no session")
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            let cleared = loop {
                let current = store.get("one")?;
                if current.status == Status::Idle && current.workspace_ready {
                    break current;
                }
                ensure!(
                    current.status != Status::Failed && Instant::now() < deadline,
                    "fresh worktree was not prepared: {:?} {:?}",
                    current.status,
                    current.error
                );
                thread::sleep(Duration::from_millis(50));
            };
            service.stop_worker("one")?;

            assert_eq!(cleared.workspace.as_deref(), Some(tree.as_path()));
            assert_eq!(cleared.baseline.as_deref(), Some(fresh.as_str()));
            assert_eq!(cleared.branch.as_deref(), Some("difu/agent-one-1"));
            assert_eq!(git(&tree, &["rev-parse", "HEAD"])?, fresh);
            assert_eq!(
                fs::read_to_string(tree.join("tracked.txt"))?,
                "fresh remote main\n"
            );
            assert!(!tree.join("untracked.txt").exists());
            assert!(
                git(
                    &local,
                    &["show-ref", "--verify", "refs/heads/difu/agent-one"]
                )
                .is_err()
            );
            assert_eq!(git(&local, &["rev-parse", "HEAD"])?, local_head);
            assert_eq!(
                fs::read_to_string(local.join("tracked.txt"))?,
                "user's own checkout edit\n"
            );

            assert_eq!(cleared.entries.len(), 1);
            assert!(
                cleared
                    .entries
                    .iter()
                    .all(|e| e.kind == "system" && !e.text.contains("earlier private context"))
            );
            assert!(cleared.thread_id.is_none() && cleared.provider_context.is_none());
            assert!(cleared.provider_threads.is_empty());
            assert!(cleared.queue.is_empty() && cleared.pending.is_empty());
            assert!(cleared.answered_questions.is_empty() && cleared.artifacts.is_empty());
            assert!(cleared.token_usage.is_null());
            assert!(matches!(&cleared.job, Job::Coding(l) if l.prompt.is_empty()
                && l.model.as_deref() == Some("chosen-model")));
            assert!(!cleared.title_ready && !cleared.title_attempted);
            assert_eq!(
                cleared.title,
                if manual_title {
                    "Old task title"
                } else {
                    "New session"
                }
            );
            assert!(!super::super::pr_cache::load_links(&links_path).contains_key("one"));
            assert_eq!(
                super::super::pr_cache::snapshot(&store.storage, "one").disconnected_before,
                1
            );
        }
        Ok(())
    }
}
