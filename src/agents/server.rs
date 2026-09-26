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
    io::{BufReader, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

pub struct Store {
    pub home: PathBuf,
    pub storage: Storage,
    sessions: Mutex<BTreeMap<String, Session>>,
    save_lock: Mutex<()>,
}
impl Store {
    pub fn get(&self, id: &str) -> Result<Session> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session store lock failed"))?
            .get(id)
            .cloned()
            .context("Session not found")
    }
    pub fn update(&self, id: &str, f: impl FnOnce(&mut Session)) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session store lock failed"))?;
        let session = sessions.get_mut(id).context("Session not found")?;
        f(session);
        session.touch();
        Ok(())
    }
    pub fn save(&self, id: &str) -> Result<()> {
        let _guard = self
            .save_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Persistence lock failed"))?;
        storage::atomic_json(&self.home.join(format!("{id}.json")), &self.get(id)?)
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
    sender: mpsc::Sender<Command>,
    cancel: Cancel,
    handle: thread::JoinHandle<()>,
}
struct Service {
    store: Arc<Store>,
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
    fn start(&self, id: &str, initial: Option<Control>) -> Result<()> {
        let session = self.store.get(id)?;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker lock failed"))?;
        let (sender, receiver) = mpsc::channel();
        let cancel = Cancel::default();
        let token = cancel.clone();
        let store = self.store.clone();
        let id_owned = id.to_owned();
        // The map sender owns the session lifetime independently of connected TUIs.
        let handle = thread::Builder::new()
            .name(format!("difu-agent-{id}"))
            .spawn(move || {
                let output = match session.job {
                    Job::Coding(_) => match session.provider {
                        provider::Provider::Codex => {
                            super::engine::run(&store, &id_owned, receiver, &token, initial)
                        }
                        provider::Provider::Claude => {
                            super::claude::run(&store, &id_owned, receiver, &token, initial)
                        }
                    },
                    _ => run_review(&store, &id_owned, &token),
                };
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
        if empty {
            session.title = "New session".into();
            session.status = Status::Idle;
            if let Err(error) = super::workspace::prepare(
                &mut session,
                &self.store.home,
                &Cancel::default(),
                |prepared| {
                    storage::atomic_json(&self.store.home.join(format!("{id}.json")), prepared)
                },
            ) {
                session.status = Status::Failed;
                session.error = Some(format!("{error:#}"));
                session.note("error", format!("Cannot prepare worktree: {error:#}"));
            }
        }
        storage::atomic_json(&self.store.home.join(format!("{id}.json")), &session)?;
        self.store
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Session lock failed"))?
            .insert(id.clone(), session);
        if !empty {
            self.start(&id, None)?;
        }
        Ok(id)
    }
    fn handle(&self, request: Request) -> Result<Reply> {
        let key = match &request {
            Request::Control { id, .. }
            | Request::Cleanup { id }
            | Request::Delete { id }
            | Request::Archive { id, .. }
            | Request::Repository { id, .. }
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
            Request::Ping => Ok(Reply::Ok),
            Request::ServiceVersion => Ok(Reply::ServiceVersion {
                version: env!("CARGO_PKG_VERSION").into(),
            }),
            Request::List => Ok(Reply::Sessions(self.store.list()?)),
            Request::Read { id, version } => {
                let session = self.store.get(&id)?;
                if version == Some(session.version) {
                    Ok(Reply::Unchanged)
                } else {
                    Ok(Reply::Session(Box::new(session)))
                }
            }
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
                    base: "HEAD".into(),
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
                    if session.pending.iter().any(|p| p.id == *request && p.is_async_question() && !p.responded));
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
                                    }});
                                }
                            }
                        })?;
                        self.store.save(&id)?;
                        self.start(&id, Some(control))?;
                    }
                }
                Ok(Reply::Ok)
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
                let mut candidate = self.store.get(&id)?;
                ensure!(
                    candidate.waiting_for_workspace(),
                    "Repository can only change before the first worktree is created"
                );
                ensure!(
                    candidate.status == Status::Idle
                        && candidate.turn_id.is_none()
                        && candidate.pending.is_empty()
                        && candidate.queue.is_empty()
                        && !candidate.switching_workspace
                        && !candidate.archived,
                    "Wait for an idle session with no pending requests before changing repository"
                );
                let Job::Coding(launch) = &mut candidate.job else {
                    anyhow::bail!("Only coding sessions can change repository");
                };
                launch.repository = repository;
                candidate.baseline = None;
                super::workspace::inspect(&mut candidate, &Cancel::default())?;
                // Validate first so an invalid path leaves the current connection intact.
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
                    s.job = candidate.job;
                    s.workspace = candidate.workspace;
                    s.baseline = candidate.baseline;
                    s.branch = None;
                    s.guidance_checked = false;
                    s.shells.clear();
                    s.suggestion = None;
                    s.suggestion_attempted = None;
                    s.note("system", format!("Repository changed to {}. Re-read repository instructions before continuing.", s.job.root().display()));
                })?;
                self.store.save(&id)?;
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
                // Confirmation explicitly authorizes stopping the session before cleanup.
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
                let session = self.store.get(&id)?;
                if session.workspace_ready
                    && !session.workspace_removed
                    && matches!(&session.job, Job::Coding(launch) if launch.isolated)
                {
                    super::workspace::cleanup(&session, &self.store.home, &Cancel::default())?;
                    self.store.update(&id, |s| s.workspace_removed = true)?;
                    self.store.save(&id)?;
                }
                super::media::cleanup(&self.store.storage, &id)?;
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
                Ok(Reply::Ok)
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
            Request::Skills { id, force } => {
                let session = self.store.get(&id)?;
                if session.provider == provider::Provider::Claude {
                    return Ok(super::claude::skills());
                }
                ensure!(
                    matches!(session.job, Job::Coding(_)),
                    "Skills are available for coding agents"
                );
                let cwd = session
                    .workspace
                    .as_deref()
                    .context("Workspace is still being prepared")?;
                super::engine::skills(cwd, force)
            }
        }
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
        ensure!(
            path.file_stem().and_then(|s| s.to_str()) == Some(&session.id),
            "Invalid session storage identity"
        );
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
    let service = Arc::new(Service {
        store: Arc::new(Store {
            home,
            storage,
            sessions: Mutex::new(sessions),
            save_lock: Mutex::new(()),
        }),
        workers: Mutex::new(BTreeMap::new()),
        action_locks: Mutex::new(BTreeMap::new()),
        launch_lock: Mutex::new(()),
    });
    let _pr_cache =
        super::pr_cache::Refresher::start(service.store.storage.clone(), service.store.clone())?;
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, stopped.clone())?;
    }
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
    let reply = service
        .handle(request)
        .unwrap_or_else(|e| Reply::Error(format!("{e:#}")));
    serde_json::to_writer(&mut stream, &reply)?;
    stream.write_all(b"\n")?;
    Ok(())
}
