//! JSON-RPC for one device connection. Every request runs on its own thread and
//! replies through the connection's queue; subscriptions push events the same
//! way. `full` devices may also tunnel the service protocol (`rpc`, `rpc.watch`).
use super::{
    super::{
        Control, Reply, Request, Session, Skill, Summary, client, media,
        pr_cache::{self, SessionLink, SessionLinks},
        provider::Provider,
        server, transport,
    },
    Access, Remote, commands, reviews,
    view::{self, EntryView, SessionSummary, SessionView},
};
use crate::storage::{AgentDefaults, Config, Storage};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    thread,
    time::{Duration, Instant},
};

const MODELS_TTL: Duration = Duration::from_secs(10 * 60);
/// A session's provider skills are listed again after this long (or on refresh).
const SKILLS_TTL: Duration = Duration::from_secs(2 * 60);
/// Session lists and watched sessions reread cached pull requests this often.
const PRS_TTL: Duration = Duration::from_secs(5);
/// `rpc.watch` streams one connection may hold open.
const MAX_STREAMS: usize = 64;

/// The terminal's config without its one-time migration write: the service
/// must never race the terminal app's own saves.
pub fn config(storage: &Storage) -> Config {
    std::fs::read(&storage.config)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub struct Connection(Arc<Inner>);

struct Inner {
    remote: Arc<Remote>,
    storage: Storage,
    device: String,
    access: Access,
    port: u16,
    sender: SyncSender<String>,
    closed: AtomicBool,
    /// The device revoked itself; the listener hangs up after the reply.
    revoked: AtomicBool,
    watches: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    streams: Mutex<BTreeMap<u64, Stream>>,
    next_stream: AtomicU64,
    listing: AtomicBool,
    models: Mutex<Option<(Instant, Value)>>,
    prs: Mutex<Option<(Instant, SessionLinks)>>,
    skills: Mutex<BTreeMap<String, Skills>>,
}

/// A session's provider skills and discovery errors, as last listed.
#[derive(Clone)]
struct Skills {
    at: Instant,
    provider: Provider,
    skills: Vec<Skill>,
    errors: Vec<String>,
}

/// One `rpc.watch`: a service subscription forwarded as events.
struct Stream {
    stop: Arc<AtomicBool>,
    socket: UnixStream,
}
impl Stream {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

#[derive(Deserialize)]
struct Call {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

impl Connection {
    pub fn new(
        remote: Arc<Remote>,
        device: String,
        access: Access,
        port: u16,
        sender: SyncSender<String>,
    ) -> Self {
        Self(Arc::new(Inner {
            storage: remote.storage.clone(),
            remote,
            device,
            access,
            port,
            sender,
            closed: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            watches: Mutex::new(BTreeMap::new()),
            streams: Mutex::new(BTreeMap::new()),
            next_stream: AtomicU64::new(1),
            listing: AtomicBool::new(false),
            models: Mutex::new(None),
            prs: Mutex::new(None),
            skills: Mutex::new(BTreeMap::new()),
        }))
    }

    pub fn handle(&self, text: &str) {
        let call: Call = match serde_json::from_str(text) {
            Ok(call) => call,
            Err(error) => {
                self.0
                    .push(&json!({"id": null, "error": format!("Invalid request: {error}")}));
                return;
            }
        };
        // Answered on the socket thread, so the reply leaves before it hangs up.
        if let Some(result) = self.revoke(&call) {
            self.0.push(&response(call.id, result));
            return;
        }
        let inner = self.0.clone();
        let _ = thread::Builder::new()
            .name("difu-remote-call".into())
            .spawn(move || {
                if call.method == "rpc.watch" {
                    return rpc_watch(&inner, call.id, &call.params);
                }
                let reply = response(call.id, dispatch(&inner, &call.method, &call.params));
                inner.push(&reply);
            });
    }

    /// `device.unpair`, or forgetting this very device.
    fn revoke(&self, call: &Call) -> Option<Result<Value>> {
        let own = match call.method.as_str() {
            "device.unpair" => true,
            "server.devices.forget" => {
                call.params.get("device").and_then(Value::as_str) == Some(self.0.device.as_str())
            }
            _ => false,
        };
        own.then(|| {
            self.0.remote.forget(&self.0.device, false)?;
            self.0.revoked.store(true, Ordering::Relaxed);
            Ok(json!({}))
        })
    }

    pub fn revoked(&self) -> bool {
        self.0.revoked.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.0.closed.store(true, Ordering::Relaxed);
        if let Ok(watches) = self.0.watches.lock() {
            for stop in watches.values() {
                stop.store(true, Ordering::Relaxed);
            }
        }
        if let Ok(streams) = self.0.streams.lock() {
            for stream in streams.values() {
                stream.stop();
            }
        }
    }
}

fn response(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(result) => json!({"id": id, "result": result}),
        Err(error) => json!({"id": id, "error": format!("{error:#}")}),
    }
}

impl Inner {
    fn push(&self, message: &Value) -> bool {
        !self.closed.load(Ordering::Relaxed) && self.sender.send(message.to_string()).is_ok()
    }
    fn event(&self, event: &str, data: impl serde::Serialize) -> bool {
        self.push(&json!({"event": event, "data": data}))
    }
    fn request(&self, request: Request) -> Result<Reply> {
        client::request(&self.storage, request)
    }
    fn control(&self, id: String, control: Control) -> Result<Value> {
        self.request(Request::Control { id, control })?;
        Ok(json!({}))
    }
    fn session(&self, id: &str) -> Result<Session> {
        match self.request(Request::Read {
            id: id.to_owned(),
            version: None,
        })? {
            Reply::Session(session) => Ok(*session),
            _ => anyhow::bail!("Unexpected session reply"),
        }
    }
    fn full(&self) -> Result<()> {
        ensure!(
            self.access == Access::Full,
            "This device has standard access; `rpc` needs a device paired with full access"
        );
        Ok(())
    }
    fn summaries(&self) -> Result<Vec<SessionSummary>> {
        let Reply::Sessions(mut list) = self.request(Request::List)? else {
            anyhow::bail!("Unexpected session list reply");
        };
        // Phones connect to cloud servers directly; relayed copies would repeat them.
        list.retain(|summary| summary.server.is_none());
        let config = config(&self.storage);
        Ok(view::summaries(
            &list,
            &config.pinned_sessions,
            &config.session_order,
            &self.prs(&list),
        ))
    }
    /// Cached pull requests of the listed sessions, reread every few seconds.
    fn prs(&self, list: &[Summary]) -> SessionLinks {
        let mut cached = self.prs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, prs)) = cached.as_ref()
            && at.elapsed() < PRS_TTL
        {
            return prs.clone();
        }
        let prs = pr_cache::cached(&self.storage, list);
        *cached = Some((Instant::now(), prs.clone()));
        prs
    }
    /// The session provider's skills and commands, listed again after a while,
    /// when its provider changed, or when `force`d.
    fn skills(&self, session: &Session, force: bool) -> Result<Skills> {
        if !force
            && let Some(cached) = self
                .skills
                .lock()
                .map_err(|_| anyhow::anyhow!("Skills lock failed"))?
                .get(&session.id)
                .filter(|c| c.at.elapsed() < SKILLS_TTL && c.provider == session.provider)
        {
            return Ok(cached.clone());
        }
        let Reply::Skills { skills, errors } = self.request(Request::Skills {
            id: session.id.clone(),
            force,
        })?
        else {
            anyhow::bail!("Unexpected skills reply");
        };
        let listed = Skills {
            at: Instant::now(),
            provider: session.provider,
            skills,
            errors,
        };
        self.skills
            .lock()
            .map_err(|_| anyhow::anyhow!("Skills lock failed"))?
            .insert(session.id.clone(), listed.clone());
        Ok(listed)
    }
    /// Whether this service runs in server mode (`difu serve`).
    fn server_mode(&self) -> bool {
        config(&self.storage).server.enabled
    }
    /// A watched session's cached pull requests.
    fn session_prs(&self, session: &Session) -> Vec<SessionLink> {
        pr_cache::cached(&self.storage, &[session.summary()])
            .remove(&session.id)
            .unwrap_or_default()
    }
}

fn param<T: DeserializeOwned>(params: &Value, key: &str) -> Result<T> {
    serde_json::from_value(params.get(key).cloned().unwrap_or(Value::Null))
        .with_context(|| format!("Invalid or missing parameter `{key}`"))
}

fn optional<T: DeserializeOwned>(params: &Value, key: &str) -> Result<Option<T>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .with_context(|| format!("Invalid parameter `{key}`")),
    }
}

fn dispatch(inner: &Arc<Inner>, method: &str, params: &Value) -> Result<Value> {
    let id = || param::<String>(params, "id");
    match method {
        "server.info" => Ok(json!({
            "name": inner.remote.name(),
            "version": env!("CARGO_PKG_VERSION"),
            "hosts": inner.remote.hosts(),
            "port": inner.port,
        })),
        "server.devices" => Ok(json!({"devices": devices(inner)?})),
        "server.devices.forget" => {
            inner
                .remote
                .forget(&param::<String>(params, "device")?, true)?;
            Ok(json!({}))
        }
        "rpc" => {
            inner.full()?;
            let request: Request = param(params, "request")?;
            ensure!(
                !matches!(request, Request::Watch { .. }),
                "Use `rpc.watch` to follow a session"
            );
            Ok(json!({"reply": inner.request(request)?}))
        }
        "rpc.unwatch" => {
            inner.full()?;
            let stream: u64 = param(params, "stream")?;
            if let Some(stream) = inner
                .streams
                .lock()
                .map_err(|_| anyhow::anyhow!("Watch lock failed"))?
                .remove(&stream)
            {
                stream.stop();
            }
            Ok(json!({}))
        }
        "sessions.list" => Ok(json!({"sessions": inner.summaries()?})),
        "sessions.subscribe" => {
            let sessions = inner.summaries()?;
            if !inner.listing.swap(true, Ordering::Relaxed) {
                let inner = inner.clone();
                let first = sessions.clone();
                thread::Builder::new()
                    .name("difu-remote-list".into())
                    .spawn(move || list_sessions(&inner, first))?;
            }
            Ok(json!({"sessions": sessions}))
        }
        "session.watch" => {
            let id = id()?;
            let stop = Arc::new(AtomicBool::new(false));
            let previous = inner
                .watches
                .lock()
                .map_err(|_| anyhow::anyhow!("Watch lock failed"))?
                .insert(id.clone(), stop.clone());
            if let Some(previous) = previous {
                previous.store(true, Ordering::Relaxed);
            }
            let inner = inner.clone();
            thread::Builder::new()
                .name("difu-remote-watch".into())
                .spawn(move || watch(&inner, &id, &stop))?;
            Ok(json!({}))
        }
        "session.unwatch" => {
            if let Some(stop) = inner
                .watches
                .lock()
                .map_err(|_| anyhow::anyhow!("Watch lock failed"))?
                .remove(&id()?)
            {
                stop.store(true, Ordering::Relaxed);
            }
            Ok(json!({}))
        }
        "session.entry" => {
            let id = id()?;
            let entry: String = param(params, "entry")?;
            let session = inner.session(&id)?;
            let found = session
                .entries
                .iter()
                .find(|e| e.id == entry)
                .context("That message is no longer in the session")?;
            Ok(json!({"entry": EntryView::new(&session, found, true)}))
        }
        "session.send" => {
            let id = id()?;
            let mut text: String = param(params, "text")?;
            let queue = optional::<bool>(params, "queue")?.unwrap_or(false);
            let uploads: Vec<Upload> = optional(params, "attachments")?.unwrap_or_default();
            let names: Vec<String> = optional(params, "skills")?.unwrap_or_default();
            let skills = if names.is_empty() {
                Vec::new()
            } else {
                let listed = inner.skills(&inner.session(&id)?, false)?;
                names
                    .iter()
                    .map(|name| commands::find_skill(&listed.skills, name, true).cloned())
                    .collect::<Result<Vec<_>>>()?
            };
            let mut attachments = Vec::new();
            for (index, upload) in uploads.iter().enumerate() {
                let bytes = STANDARD
                    .decode(&upload.data)
                    .context("Attachment is not valid base64")?;
                let mut attachment = media::store(&inner.storage, &id, &bytes)?;
                attachment.label = format!("image {}", index + 1);
                if !text.contains(&attachment.token()) {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&attachment.token());
                }
                attachments.push(attachment);
            }
            ensure!(!text.trim().is_empty(), "Message cannot be empty");
            inner.control(
                id,
                Control::Message {
                    text,
                    queue,
                    skills,
                    attachments,
                },
            )
        }
        "session.commands" => session_commands(
            inner,
            &id()?,
            optional::<bool>(params, "refresh")?.unwrap_or(false),
        ),
        "session.command" => run_command(inner, params),
        "session.interrupt" => inner.control(id()?, Control::Interrupt),
        "session.sendNow" => inner.control(id()?, Control::InterruptAndSend),
        "session.resume" => inner.control(id()?, Control::Resume),
        "session.compact" => inner.control(id()?, Control::Compact),
        "session.unqueue" => {
            let id = id()?;
            let index: usize = param(params, "index")?;
            let expected = inner
                .session(&id)?
                .queue
                .get(index)
                .cloned()
                .context("That queued message was already sent")?;
            inner.control(
                id,
                Control::ReplaceQueued {
                    index,
                    expected,
                    replacement: None,
                },
            )
        }
        "session.answer" => answer(inner, params),
        "session.decide" => {
            let id = id()?;
            let request: Value = param(params, "request")?;
            let choice: String = param(params, "choice")?;
            let session = inner.session(&id)?;
            let pending = session
                .pending
                .iter()
                .find(|p| p.id == request)
                .context("That request was already answered")?;
            if view::pending_kind(&pending.method) == "unsupported" {
                ensure!(choice == "stop", "Unknown choice");
                return inner.control(id, Control::Interrupt);
            }
            let response = view::decision(pending, &choice)?;
            inner.control(id, Control::Respond { request, response })
        }
        "session.model" => inner.control(
            id()?,
            Control::Model {
                model: optional::<String>(params, "model")?.filter(|m| !m.is_empty()),
                effort: optional::<String>(params, "effort")?.filter(|e| !e.is_empty()),
            },
        ),
        "session.rename" => {
            inner.request(Request::Rename {
                id: id()?,
                title: param(params, "title")?,
            })?;
            Ok(json!({}))
        }
        "session.archive" => {
            inner.request(Request::Archive {
                id: id()?,
                archived: param(params, "archived")?,
            })?;
            Ok(json!({}))
        }
        "session.delete" => {
            inner.request(Request::Delete { id: id()? })?;
            Ok(json!({}))
        }
        "session.clear" => {
            inner.request(Request::Clear { id: id()? })?;
            Ok(json!({}))
        }
        "session.changes" => match inner.request(Request::Changes { id: id()? })? {
            Reply::Changes(patch) => Ok(json!({"files": patch_files(&patch)})),
            _ => anyhow::bail!("Unexpected changes reply"),
        },
        "agents.new" => new_session(inner, params),
        "agents.repositories" => Ok(json!({"repositories": repositories(inner)?})),
        "agents.models" => models(inner),
        "agents.defaults" => Ok(super::defaults::get(&inner.storage)),
        "agents.defaults.set" => super::defaults::set(&inner.storage, params),
        "reviews.inbox" => reviews::inbox(
            &inner.storage,
            optional::<bool>(params, "refresh")?.unwrap_or(false),
        ),
        "reviews.repositories" => reviews::repositories(&inner.storage),
        "reviews.repository" => reviews::repository(params),
        "pr.overview" => reviews::overview(&reviews::key(params)?),
        "pr.diff" => reviews::diff(&reviews::key(params)?),
        "pr.guide" => reviews::guide(&inner.storage, &reviews::key(params)?),
        "pr.generateGuide" => reviews::generate(&inner.storage, &reviews::key(params)?),
        "pr.image" => reviews::image(&inner.storage, params),
        "pr.comment" | "pr.review" | "pr.merge" | "pr.close" | "pr.ready" => {
            reviews::act(method, params)
        }
        _ => match super::server_api::dispatch(&inner.storage, method, params) {
            Some(result) => result,
            None => anyhow::bail!("Unknown method `{method}`; update difu on your computer"),
        },
    }
}

#[derive(Deserialize)]
struct Upload {
    data: String,
}

/// Answers one question (`answer: null` skips it). A `note` is added to the
/// answer the way the terminal adds a note to a chosen option.
fn answer(inner: &Inner, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let request: Value = param(params, "request")?;
    let question: String = param(params, "question")?;
    let mut answer: Option<String> = optional(params, "answer")?;
    let note = optional::<String>(params, "note")?
        .map(|note| note.trim().to_owned())
        .filter(|note| !note.is_empty());
    if let Some(note) = note {
        let text = answer
            .as_deref()
            .context("Choose or enter an answer to send a note with it")?;
        let session = inner.session(&id)?;
        let pending = session
            .pending
            .iter()
            .find(|p| p.id == request)
            .context("This question is no longer pending")?;
        let asked = pending
            .params
            .get("questions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|q| q.get("id").and_then(Value::as_str) == Some(question.as_str()))
            .context("This question was already answered or skipped")?;
        ensure!(
            super::super::questions::accepts_note(pending, asked),
            "This question needs an exact answer; send it without a note"
        );
        answer = Some(super::super::questions::with_note(text, &note));
    }
    inner.control(
        id,
        Control::AnswerQuestion {
            request,
            question,
            answer,
        },
    )
}

fn devices(inner: &Inner) -> Result<Vec<Value>> {
    Ok(inner
        .remote
        .status()?
        .devices
        .into_iter()
        .map(|d| {
            json!({
                "id": d.id,
                "name": d.name,
                "access": d.access,
                "paired": d.paired,
                "seen": d.seen,
                "connected": d.connected,
                "current": d.id == inner.device,
            })
        })
        .collect())
}

/// Answer with the stream number, then forward every service reply for the
/// session until `rpc.unwatch`, a closed connection, or the service ending it.
fn rpc_watch(inner: &Arc<Inner>, id: Value, params: &Value) {
    let opened = (|| -> Result<(u64, Arc<AtomicBool>, BufReader<UnixStream>)> {
        inner.full()?;
        let session: String = param(params, "id")?;
        let version: Option<u64> = optional(params, "version")?;
        let mut streams = inner
            .streams
            .lock()
            .map_err(|_| anyhow::anyhow!("Watch lock failed"))?;
        ensure!(streams.len() < MAX_STREAMS, "Too many watched sessions");
        let mut socket = UnixStream::connect(server::socket(&inner.storage)?)
            .context("Agent service is disconnected")?;
        socket.set_read_timeout(Some(Duration::from_secs(30)))?;
        socket.set_write_timeout(Some(Duration::from_secs(10)))?;
        serde_json::to_writer(
            &mut socket,
            &Request::Watch {
                id: session,
                version,
            },
        )?;
        socket.write_all(b"\n")?;
        let number = inner.next_stream.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        streams.insert(
            number,
            Stream {
                stop: stop.clone(),
                socket: socket.try_clone()?,
            },
        );
        Ok((number, stop, BufReader::new(socket)))
    })();
    let (number, stop, mut reader) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            inner.push(&response(id, Err(error)));
            return;
        }
    };
    inner.push(&response(id, Ok(json!({"stream": number}))));
    let mut message = None;
    while !stop.load(Ordering::Relaxed) && !inner.closed.load(Ordering::Relaxed) {
        match transport::read_reply(&mut reader) {
            Ok(reply) => {
                if let Reply::Error(error) = &reply {
                    message = Some(error.clone());
                }
                if !inner.event("rpc.watch", json!({"stream": number, "reply": reply})) {
                    break;
                }
            }
            Err(error) => {
                message.get_or_insert_with(|| format!("{error:#}"));
                break;
            }
        }
    }
    if let Ok(mut streams) = inner.streams.lock() {
        streams.remove(&number);
    }
    if !stop.load(Ordering::Relaxed) && !inner.closed.load(Ordering::Relaxed) {
        inner.event(
            "rpc.watch.closed",
            json!({"stream": number, "message": message.unwrap_or_else(|| "Watch ended".into())}),
        );
    }
}

/// difu's phone commands for the session, then its provider's skills and
/// commands. Skill discovery problems are reported beside the list.
fn session_commands(inner: &Inner, id: &str, refresh: bool) -> Result<Value> {
    let session = inner.session(id)?;
    let mut list = commands::difu(&session, inner.server_mode());
    let mut errors = Vec::new();
    if matches!(session.job, super::super::Job::Coding(_)) {
        match inner.skills(&session, refresh) {
            Ok(listed) => {
                list.extend(commands::provider(session.provider, &listed.skills));
                errors = listed.errors;
            }
            Err(error) => errors.push(format!("{error:#}")),
        }
    }
    Ok(json!({"commands": list, "errors": errors}))
}

/// Runs a slash command: difu's own on the computer (destructive ones only with
/// `confirm`), or a provider skill or command sent as a message.
fn run_command(inner: &Inner, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let requested: String = param(params, "name")?;
    let args = optional::<String>(params, "args")?.unwrap_or_default();
    let args = args.trim();
    let confirm = optional::<bool>(params, "confirm")?.unwrap_or(false);
    let queue = optional::<bool>(params, "queue")?.unwrap_or(false);
    let attached = requested.trim().starts_with('$');
    let name = requested.trim().trim_start_matches(['/', '$']);
    ensure!(!name.is_empty(), "Choose a command");
    let session = inner.session(&id)?;
    let done = |message: &str| Ok(json!({"message": message}));
    if !attached && let Some(builtin) = commands::builtin(name) {
        ensure!(
            commands::available(builtin, &session),
            "/{name} isn't available in this session"
        );
        ensure!(
            args.is_empty() || builtin.arguments != commands::Arguments::None,
            "/{name} does not accept arguments. Run /{name} on its own."
        );
        return match name {
            "compact" => {
                inner.control(id, Control::Compact)?;
                done("Compacting the conversation")
            }
            "clear" => {
                ensure!(
                    confirm,
                    "/clear discards the conversation, PRs and artifacts and starts a fresh worktree from main; confirm to continue"
                );
                inner.request(Request::Clear { id })?;
                done("Started over")
            }
            "repo" => {
                ensure!(!args.is_empty(), "Usage: /repo <path>");
                ensure!(
                    confirm,
                    "/repo discards the current workspace's work; confirm to continue"
                );
                let repository = if inner.server_mode() {
                    crate::cloud::repos::resolve(&inner.storage, args)?
                } else {
                    PathBuf::from(args)
                };
                inner.request(Request::Repository { id, repository })?;
                done("Changed the repository")
            }
            "rename" if !args.is_empty() => {
                inner.request(Request::Rename {
                    id,
                    title: args.to_owned(),
                })?;
                done("Renamed the session")
            }
            "model" | "effort" if !args.is_empty() => {
                let (model, effort) = commands::model_arguments(name, args, &session)?;
                inner.control(id, Control::Model { model, effort })?;
                done("Changed the model")
            }
            "permissions" => {
                let action = if args.is_empty() {
                    super::super::permissions::Action::Refresh
                } else {
                    commands::permission_action(session.provider, args)?
                };
                let Reply::Permissions { snapshot } =
                    inner.request(Request::Permissions { id, action })?
                else {
                    anyhow::bail!("Unexpected permissions reply");
                };
                Ok(json!({
                    "screen": "permissions",
                    "permissions": commands::permissions_view(&snapshot),
                }))
            }
            "usage" => {
                let Reply::Usage(usage) = inner.request(Request::Usage { id })? else {
                    anyhow::bail!("Unexpected usage reply");
                };
                Ok(
                    json!({"screen": "usage", "usage": commands::usage_view(session.provider, &usage)}),
                )
            }
            "status" => Ok(json!({"screen": "status", "status": commands::status_view(&session)})),
            "rules" => Ok(json!({
                "screen": "rules",
                "repository": session.job.root().display().to_string(),
            })),
            _ => Ok(json!({"screen": builtin.screen.unwrap_or(name)})),
        };
    }
    ensure!(
        matches!(session.job, super::super::Job::Coding(_)),
        "Unknown command `/{name}`"
    );
    let listed = inner.skills(&session, false)?;
    let skill = commands::find_skill(&listed.skills, name, attached)?;
    let text = commands::skill_text(skill, args);
    let message = format!(
        "Sent {}",
        text.split_whitespace().next().unwrap_or_default()
    );
    inner.control(
        id,
        Control::Message {
            text,
            queue,
            skills: if skill.slash {
                Vec::new()
            } else {
                vec![skill.clone()]
            },
            attachments: Vec::new(),
        },
    )?;
    done(&message)
}

fn new_session(inner: &Inner, params: &Value) -> Result<Value> {
    // Before a repository is cloned for it.
    crate::cloud::disk::guard(&inner.storage)?;
    let config = config(&inner.storage);
    let saved = config.agent_defaults;
    // A server also takes `owner/name`, adding and cloning it when needed.
    let repository = match optional::<String>(params, "repository")? {
        Some(repository) if config.server.enabled => {
            Some(crate::cloud::repos::resolve(&inner.storage, &repository)?)
        }
        repository => repository.map(PathBuf::from),
    };
    let defaults = phone_defaults(
        saved,
        repository,
        optional::<String>(params, "model")?.filter(|m| !m.is_empty()),
        optional::<String>(params, "effort")?.filter(|e| !e.is_empty()),
    );
    ensure!(
        defaults.repository.is_some(),
        "Choose a repository for the new session"
    );
    let id = match inner.request(Request::NewAgent {
        defaults,
        cwd: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
        remember_repository: false,
        server: None,
    })? {
        Reply::Launched(id) => id,
        Reply::ChooseRepository => anyhow::bail!("Choose a Git repository for the new session"),
        _ => anyhow::bail!("Unexpected new session reply"),
    };
    if let Some(text) = optional::<String>(params, "text")?.filter(|t| !t.trim().is_empty()) {
        inner.control(
            id.clone(),
            Control::Message {
                text,
                queue: false,
                skills: Vec::new(),
                attachments: Vec::new(),
            },
        )?;
    }
    Ok(json!({"id": id}))
}

/// The saved new-session defaults with the phone's choices. A chosen model
/// picks its own provider, so the saved provider and the saved effort and
/// starting mode, which belong to it, apply only when they match.
fn phone_defaults(
    saved: AgentDefaults,
    repository: Option<PathBuf>,
    model: Option<String>,
    effort: Option<String>,
) -> AgentDefaults {
    let repository = repository.or(saved.repository.clone());
    let Some(model) = model else {
        return AgentDefaults {
            repository,
            effort: effort.or(saved.effort.clone()),
            ..saved
        };
    };
    let provider = Provider::for_model(Some(&model));
    let same = saved.resolved_provider() == provider;
    AgentDefaults {
        provider: Some(provider),
        starting_mode: saved.starting_mode.filter(|_| same),
        repository,
        model: Some(model),
        effort: effort.or(saved.effort.filter(|_| same)),
        isolated: saved.isolated,
    }
}

/// Repositories difu already knows: the default, review clones, local diff
/// repositories, and repositories of existing sessions. A server offers its
/// server repositories instead.
fn repositories(inner: &Inner) -> Result<Vec<Value>> {
    let config = config(&inner.storage);
    let default = config.agent_defaults.repository.clone();
    if config.server.enabled {
        return Ok(crate::cloud::repos::list(&inner.storage)?
            .into_iter()
            .filter(|repository| !repository.path.as_os_str().is_empty())
            .map(|repository| {
                json!({
                    "path": repository.path.display().to_string(),
                    "name": repository.name,
                    "default": default.as_ref() == Some(&repository.path),
                })
            })
            .collect());
    }
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    let Reply::Sessions(list) = inner.request(Request::List)? else {
        anyhow::bail!("Unexpected session list reply");
    };
    let mut recent: Vec<_> = list
        .iter()
        .filter_map(|s| s.repository.clone().map(|r| (s.updated, r)))
        .collect();
    recent.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
    for path in default
        .iter()
        .cloned()
        .chain(recent.into_iter().map(|(_, path)| path))
        .chain(config.repositories.values().cloned())
        .chain(config.local_diff_repositories.iter().cloned())
    {
        if path.is_dir() && seen.insert(path.clone()) {
            paths.push(json!({
                "path": path.display().to_string(),
                "name": view::display_name(&path),
                "default": default.as_ref() == Some(&path),
            }));
        }
    }
    Ok(paths)
}

fn models(inner: &Inner) -> Result<Value> {
    if let Some((at, models)) = inner
        .models
        .lock()
        .map_err(|_| anyhow::anyhow!("Model cache lock failed"))?
        .as_ref()
        && at.elapsed() < MODELS_TTL
    {
        return Ok(models.clone());
    }
    let cancel = crate::process::Cancel::default();
    let (codex, claude) = thread::scope(|scope| {
        let codex = scope.spawn(|| crate::codex::models(&cancel));
        let claude = super::super::claude::models(&cancel);
        (
            codex
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Codex model discovery stopped"))),
            claude,
        )
    });
    let mut models = Vec::new();
    let mut errors = Vec::new();
    for (provider, result) in [("codex", codex), ("claude", claude)] {
        match result {
            Ok(list) => models.extend(list.into_iter().map(
                |m| json!({"id": m.id, "name": m.name, "provider": provider, "efforts": m.efforts}),
            )),
            Err(error) => errors.push(format!("{error:#}")),
        }
    }
    ensure!(!models.is_empty(), "{}", errors.join("; "));
    let value = json!({"models": models});
    *inner
        .models
        .lock()
        .map_err(|_| anyhow::anyhow!("Model cache lock failed"))? =
        Some((Instant::now(), value.clone()));
    Ok(value)
}

fn list_sessions(inner: &Inner, mut sent: Vec<SessionSummary>) {
    while !inner.closed.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        match inner.summaries() {
            Ok(list) if list != sent => {
                if !inner.event("sessions", &list) {
                    return;
                }
                sent = list;
            }
            Ok(_) => {}
            Err(error) => eprintln!("Remote session list: {error:#}"),
        }
    }
}

/// Follow one session through the service's own subscription and forward
/// display deltas. Unchanged rows are reused, so streaming stays cheap.
fn watch(inner: &Inner, id: &str, stop: &AtomicBool) {
    let result = (|| -> Result<()> {
        let mut stream = UnixStream::connect(server::socket(&inner.storage)?)
            .context("Agent service is disconnected")?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        serde_json::to_writer(
            &mut stream,
            &Request::Watch {
                id: id.to_owned(),
                version: None,
            },
        )?;
        stream.write_all(b"\n")?;
        let mut reader = BufReader::new(stream);
        let mut session: Option<Session> = None;
        let mut sent: Option<SessionView> = None;
        let mut cache = view::Cache::default();
        let mut pinned = config(&inner.storage).pinned_sessions;
        let mut pinned_at = Instant::now();
        let mut prs = None;
        while !stop.load(Ordering::Relaxed) && !inner.closed.load(Ordering::Relaxed) {
            match transport::read_reply(&mut reader)? {
                Reply::Session(next) => session = Some(*next),
                Reply::SessionDelta(delta) => {
                    delta.apply(session.as_mut().context("Missing session baseline")?)?
                }
                Reply::Unchanged => {
                    if pinned_at.elapsed() < Duration::from_secs(5) {
                        continue;
                    }
                }
                Reply::Error(error) => anyhow::bail!("{error}"),
                _ => anyhow::bail!("Unexpected session update"),
            }
            if pinned_at.elapsed() >= PRS_TTL {
                pinned = config(&inner.storage).pinned_sessions;
                pinned_at = Instant::now();
                prs = None;
            }
            let Some(session) = &session else { continue };
            let prs = prs.get_or_insert_with(|| inner.session_prs(session));
            let next = SessionView::cached(session, &pinned, prs, &mut cache);
            let delivered = match &sent {
                None => inner.event("session", &next),
                Some(previous) if *previous != next => {
                    inner.event("session.delta", next.delta(previous))
                }
                Some(_) => true,
            };
            if !delivered {
                return Ok(());
            }
            sent = Some(next);
        }
        Ok(())
    })();
    if let Err(error) = result
        && !stop.load(Ordering::Relaxed)
    {
        inner.event(
            "session.closed",
            json!({"id": id, "message": format!("{error:#}")}),
        );
    }
    if let Ok(mut watches) = inner.watches.lock()
        && watches
            .get(id)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), stop))
    {
        watches.remove(id);
    }
}

/// Split a unified Git patch into per-file hunks for the phone's diff viewer.
pub fn patch_files(patch: &str) -> Vec<Value> {
    const PATCH_LIMIT: usize = 400_000;
    let mut files: Vec<Vec<&str>> = Vec::new();
    for line in patch.lines() {
        if line.starts_with("diff --git ") || files.is_empty() {
            files.push(Vec::new());
        }
        if let Some(file) = files.last_mut() {
            file.push(line);
        }
    }
    files
        .into_iter()
        .filter(|lines| lines.first().is_some_and(|l| l.starts_with("diff --git ")))
        .map(|lines| {
            let header_end = lines
                .iter()
                .position(|l| l.starts_with("@@"))
                .unwrap_or(lines.len());
            let (header, body) = lines.split_at(header_end);
            let field = |prefix: &str| {
                header
                    .iter()
                    .find_map(|l| l.strip_prefix(prefix))
                    .map(str::to_owned)
            };
            let git_path = header
                .first()
                .and_then(|l| l.rsplit_once(" b/"))
                .map(|(_, path)| path.to_owned())
                .unwrap_or_default();
            let new_path = field("+++ b/").or(field("rename to ")).unwrap_or(git_path);
            let old_path = field("--- a/").or(field("rename from "));
            let status = if header.iter().any(|l| l.starts_with("new file mode")) {
                "added"
            } else if header.iter().any(|l| l.starts_with("deleted file mode")) {
                "deleted"
            } else if header.iter().any(|l| l.starts_with("rename from")) {
                "renamed"
            } else {
                "modified"
            };
            let path = if status == "deleted" {
                old_path.clone().unwrap_or(new_path)
            } else {
                new_path
            };
            let body = body.join("\n");
            let (additions, deletions) = view::counts(&body);
            let (patch, truncated) = view::clip(&body, PATCH_LIMIT, false);
            json!({
                "path": path,
                "oldPath": (status == "renamed").then_some(old_path).flatten(),
                "status": status,
                "additions": additions,
                "deletions": deletions,
                "binary": header.iter().any(|l| l.starts_with("Binary files")),
                "patch": patch,
                "truncated": truncated,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
        value.pointer(pointer).unwrap_or(&Value::Null)
    }

    #[test]
    fn patches_split_into_files_with_status_and_counts() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\ndiff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\ndiff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
        let files = patch_files(patch);
        let summary: Vec<_> = files
            .iter()
            .map(|f| {
                (
                    at(f, "/path").as_str().unwrap_or_default().to_owned(),
                    at(f, "/status").as_str().unwrap_or_default().to_owned(),
                    at(f, "/additions").as_u64().unwrap_or(9),
                    at(f, "/deletions").as_u64().unwrap_or(9),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("src/a.rs".into(), "modified".into(), 1, 1),
                ("new.txt".into(), "added".into(), 1, 0),
                ("gone.txt".into(), "deleted".into(), 0, 1),
                ("y".into(), "renamed".into(), 0, 0),
            ]
        );
        assert!(files.first().is_some_and(|f| {
            at(f, "/patch")
                .as_str()
                .is_some_and(|p| p.starts_with("@@"))
        }));
        assert_eq!(
            files.get(3).map(|f| at(f, "/oldPath").clone()),
            Some(json!("x"))
        );
    }

    #[test]
    fn servers_offer_their_server_repositories() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let clone = crate::cloud::repos::path(&storage, "owner/repo")?;
        storage.update_config(|config| {
            config.server.enabled = true;
            config.server.repositories = vec!["owner/repo".into(), "owner/next".into()];
            config.agent_defaults.repository = Some(clone.clone());
        })?;
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let connection = Connection::new(
            super::super::Remote::start(storage),
            "device".into(),
            super::super::Access::Standard,
            0,
            sender,
        );
        assert_eq!(
            repositories(&connection.0)?,
            [
                json!({"path": clone.display().to_string(), "name": "owner/repo", "default": true}),
                json!({
                    "path": clone.with_file_name("next").display().to_string(),
                    "name": "owner/next",
                    "default": false,
                }),
            ]
        );
        Ok(())
    }

    #[test]
    fn a_critically_full_server_refuses_new_sessions_before_cloning() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        storage.update_config(|config| config.server.enabled = true)?;
        let home = super::super::super::server::home(&storage)?;
        crate::cloud::disk::tests::pretend(&home, Some(crate::cloud::disk::tests::critical()));
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let connection = Connection::new(
            super::super::Remote::start(storage.clone()),
            "device".into(),
            super::super::Access::Standard,
            0,
            sender,
        );
        let result = new_session(&connection.0, &json!({"repository": "owner/repo"}));
        crate::cloud::disk::tests::pretend(&home, None);
        let error = result.err().context("refused")?;
        assert!(error.to_string().contains("Only 2.0 GB is free"), "{error}");
        assert!(
            storage.load_config()?.server.repositories.is_empty(),
            "nothing listed or cloned"
        );
        Ok(())
    }

    fn connection(storage: &Storage) -> Connection {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(64);
        Connection::new(
            super::super::Remote::start(storage.clone()),
            "device".into(),
            super::super::Access::Standard,
            0,
            sender,
        )
    }

    fn storage(dir: &tempfile::TempDir) -> Storage {
        Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        }
    }

    /// A stand-in agent service on the storage's socket: answers every request
    /// with `reply` and records it.
    struct Fake {
        requests: Arc<Mutex<Vec<Request>>>,
        stop: Arc<AtomicBool>,
        socket: PathBuf,
    }
    impl Fake {
        fn start(
            storage: &Storage,
            reply: impl Fn(&Request) -> Reply + Send + Sync + 'static,
        ) -> Result<Self> {
            let socket = server::socket(storage)?;
            let listener = std::os::unix::net::UnixListener::bind(&socket)?;
            listener.set_nonblocking(true)?;
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (recorded, stopped, reply) = (requests.clone(), stop.clone(), Arc::new(reply));
            thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let answered = (|| -> Result<()> {
                        stream.set_nonblocking(false)?;
                        let line = client::read_line(&mut BufReader::new(stream.try_clone()?))?;
                        let request: Request = serde_json::from_str(&line)?;
                        let answer = reply(&request);
                        recorded
                            .lock()
                            .map_err(|_| anyhow::anyhow!("requests"))?
                            .push(request);
                        transport::write_reply(&mut stream, &answer)
                    })();
                    if let Err(error) = answered {
                        eprintln!("fake service: {error:#}");
                    }
                }
            });
            Ok(Self {
                requests,
                stop,
                socket,
            })
        }
        /// Requests other than session reads, in order.
        fn actions(&self) -> Result<Vec<Request>> {
            Ok(self
                .requests
                .lock()
                .map_err(|_| anyhow::anyhow!("requests"))?
                .iter()
                .filter(|r| !matches!(r, Request::Read { .. }))
                .cloned()
                .collect())
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    fn skill(name: &str, slash: bool) -> Skill {
        Skill {
            name: name.into(),
            path: if slash {
                PathBuf::new()
            } else {
                PathBuf::from(format!("{name}/SKILL.md"))
            },
            description: format!("About {name}"),
            enabled: true,
            slash,
        }
    }

    fn coding(provider: Provider) -> Session {
        let mut session = Session::new(
            "one".into(),
            super::super::super::Job::Coding(super::super::super::Launch {
                repository: "/repo".into(),
                isolated: true,
                base: "origin/main".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.provider = provider;
        session.status = super::super::super::Status::Idle;
        session.workspace = Some("/worktree".into());
        session.thread_id = Some("thread".into());
        session
    }

    /// A fake service holding `session` with `skills`.
    fn serve(storage: &Storage, session: Session, skills: Vec<Skill>) -> Result<Fake> {
        Fake::start(storage, move |request| match request {
            Request::Read { .. } => Reply::Session(Box::new(session.clone())),
            Request::Skills { .. } => Reply::Skills {
                skills: skills.clone(),
                errors: Vec::new(),
            },
            Request::Usage { .. } => Reply::Usage(json!({"rate_limits": {
                "five_hour": {"utilization": 30, "resets_at": "2027-01-15T08:00:00Z"}
            }})),
            Request::Permissions { action, .. } => Reply::Permissions {
                snapshot: super::super::super::permissions::Snapshot {
                    provider: Provider::Claude,
                    active_mode: match action {
                        super::super::super::permissions::Action::SetMode { mode } => {
                            Some(mode.clone())
                        }
                        _ => Some("default".into()),
                    },
                    modes: super::super::super::permissions::mode_choices(Provider::Claude),
                    ..Default::default()
                },
            },
            Request::NewAgent { .. } => Reply::Launched("new".into()),
            _ => Reply::Ok,
        })
    }

    #[test]
    fn a_chosen_model_picks_its_provider_for_new_sessions() {
        let saved = AgentDefaults {
            provider: Some(Provider::Codex),
            starting_mode: Some("plan".into()),
            repository: Some("/saved".into()),
            model: Some("gpt-5.5".into()),
            effort: Some("xhigh".into()),
            isolated: true,
        };
        let claude = phone_defaults(saved.clone(), None, Some("claude/opus".into()), None);
        assert_eq!(claude.provider, Some(Provider::Claude));
        assert_eq!(claude.model.as_deref(), Some("claude/opus"));
        assert_eq!(claude.effort, None, "Codex's saved effort stays with Codex");
        assert_eq!(claude.starting_mode, None);
        assert_eq!(claude.repository.as_deref(), Some(Path::new("/saved")));
        assert!(claude.validate().is_ok());
        let codex = phone_defaults(
            saved.clone(),
            Some("/repo".into()),
            Some("gpt-5.6".into()),
            None,
        );
        assert_eq!(codex.provider, Some(Provider::Codex));
        assert_eq!(codex.effort.as_deref(), Some("xhigh"));
        assert_eq!(codex.starting_mode.as_deref(), Some("plan"));
        assert_eq!(codex.repository.as_deref(), Some(Path::new("/repo")));
        let default = phone_defaults(saved, None, None, Some("low".into()));
        assert_eq!(default.provider, Some(Provider::Codex));
        assert_eq!(default.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(default.effort.as_deref(), Some("low"));
        let unsaved = phone_defaults(
            AgentDefaults {
                provider: None,
                model: Some("claude/sonnet".into()),
                ..AgentDefaults::default()
            },
            None,
            Some("gpt-5.6".into()),
            None,
        );
        assert_eq!(unsaved.resolved_provider(), Provider::Codex);
    }

    #[test]
    fn new_phone_sessions_ask_for_the_provider_of_their_model() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        storage.update_config(|config| {
            config.agent_defaults.provider = Some(Provider::Codex);
            config.agent_defaults.effort = Some("xhigh".into());
        })?;
        let fake = serve(&storage, coding(Provider::Codex), Vec::new())?;
        let result = new_session(
            &connection(&storage).0,
            &json!({"repository": "/repo", "model": "claude/opus", "effort": null}),
        )?;
        assert_eq!(result, json!({"id": "new"}));
        let actions = fake.actions()?;
        let [Request::NewAgent { defaults, .. }] = actions.as_slice() else {
            anyhow::bail!("one new session request");
        };
        assert_eq!(defaults.provider, Some(Provider::Claude));
        assert_eq!(defaults.model.as_deref(), Some("claude/opus"));
        assert_eq!(defaults.effort, None);
        Ok(())
    }

    #[test]
    fn the_phone_switches_an_idle_session_to_another_provider() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut busy = coding(Provider::Codex);
        busy.id = "busy".into();
        busy.status = super::super::super::Status::Running;
        busy.turn_id = Some("turn".into());
        let served = server::Served::start(dir.path().into(), vec![coding(Provider::Codex), busy])?;
        let storage = served.store.storage.clone();
        let connection = connection(&storage);
        let view = |id: &str| -> Result<bool> {
            Ok(SessionView::new(&served.store.get(id)?, &BTreeSet::new())
                .header
                .between_turns)
        };
        assert!(view("one")?);
        dispatch(
            &connection.0,
            "session.model",
            &json!({"id": "one", "model": "claude/opus", "effort": "high"}),
        )?;
        let switched = served.store.get("one")?;
        assert_eq!(switched.provider, Provider::Claude);
        assert_eq!(switched.model.as_deref(), Some("claude/opus"));
        assert_eq!(switched.effort.as_deref(), Some("high"));
        assert_eq!(switched.thread_id, None, "a fresh Claude Code conversation");
        assert!(
            switched
                .entries
                .last()
                .is_some_and(|e| e.text.starts_with("Selected Claude Code"))
        );
        assert!(!view("busy")?);
        let error = dispatch(
            &connection.0,
            "session.model",
            &json!({"id": "busy", "model": "claude/opus", "effort": null}),
        )
        .err()
        .context("refused while working")?;
        assert!(
            error
                .to_string()
                .contains("Finish the current turn and pending requests"),
            "{error}"
        );
        assert_eq!(served.store.get("busy")?.provider, Provider::Codex);
        Ok(())
    }

    #[test]
    fn session_commands_list_difu_commands_then_cached_provider_skills() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        let fake = serve(
            &storage,
            coding(Provider::Codex),
            vec![skill("lint", false), skill("review", true)],
        )?;
        let connection = connection(&storage);
        let listed = dispatch(&connection.0, "session.commands", &json!({"id": "one"}))?;
        let triggers: Vec<_> = at(&listed, "/commands")
            .as_array()
            .context("commands")?
            .iter()
            .filter_map(|c| at(c, "/trigger").as_str())
            .collect();
        assert_eq!(triggers.first(), Some(&"/compact"));
        assert_eq!(
            triggers.get(triggers.len().saturating_sub(2)..),
            Some(&["$lint", "/review"][..])
        );
        assert_eq!(*at(&listed, "/errors"), json!([]));
        assert_eq!(
            *at(&listed, "/commands/0"),
            json!({"name": "compact", "trigger": "/compact",
                   "description": "Compact the current conversation", "hint": null,
                   "kind": "action", "screen": null, "confirm": false,
                   "arguments": "none", "source": "difu"})
        );
        let lint = at(&listed, "/commands")
            .as_array()
            .and_then(|c| c.iter().find(|c| *at(c, "/trigger") == "$lint"))
            .context("lint")?;
        assert_eq!(*at(lint, "/kind"), "prompt");
        assert_eq!(*at(lint, "/source"), "codex");
        dispatch(&connection.0, "session.commands", &json!({"id": "one"}))?;
        dispatch(
            &connection.0,
            "session.commands",
            &json!({"id": "one", "refresh": true}),
        )?;
        let listings: Vec<_> = fake
            .actions()?
            .into_iter()
            .filter_map(|r| match r {
                Request::Skills { force, .. } => Some(force),
                _ => None,
            })
            .collect();
        assert_eq!(
            listings,
            [false, true],
            "the second listing came from the cache"
        );
        Ok(())
    }

    #[test]
    fn destructive_commands_run_only_when_confirmed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        let fake = serve(&storage, coding(Provider::Codex), Vec::new())?;
        let connection = connection(&storage);
        let run = |params: Value| dispatch(&connection.0, "session.command", &params);
        let refused = run(json!({"id": "one", "name": "clear"}))
            .err()
            .context("unconfirmed")?;
        assert!(refused.to_string().contains("confirm"), "{refused}");
        let usage = run(json!({"id": "one", "name": "/repo", "confirm": true}))
            .err()
            .context("no path")?;
        assert!(usage.to_string().contains("Usage: /repo <path>"));
        assert!(fake.actions()?.is_empty(), "nothing ran");
        run(json!({"id": "one", "name": "clear", "confirm": true}))?;
        run(json!({"id": "one", "name": "repo", "args": " /other ", "confirm": true}))?;
        let actions = fake.actions()?;
        assert!(matches!(
            actions.as_slice(),
            [Request::Clear { id }, Request::Repository { repository, .. }]
                if id == "one" && repository == Path::new("/other")
        ));
        Ok(())
    }

    #[test]
    fn difu_commands_run_controls_or_name_their_screen() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        let fake = serve(&storage, coding(Provider::Claude), Vec::new())?;
        let connection = connection(&storage);
        let run = |params: Value| dispatch(&connection.0, "session.command", &params);
        assert_eq!(
            run(json!({"id": "one", "name": "/compact"}))?,
            json!({"message": "Compacting the conversation"})
        );
        let extra = run(json!({"id": "one", "name": "compact", "args": "now"}))
            .err()
            .context("no arguments")?;
        assert!(extra.to_string().contains("does not accept arguments"));
        assert_eq!(
            run(json!({"id": "one", "name": "model"}))?,
            json!({"screen": "model"})
        );
        assert_eq!(
            run(json!({"id": "one", "name": "effort"}))?,
            json!({"screen": "model"})
        );
        assert_eq!(
            run(json!({"id": "one", "name": "new"}))?,
            json!({"screen": "new"})
        );
        assert_eq!(
            run(json!({"id": "one", "name": "skills"}))?,
            json!({"screen": "skills"})
        );
        assert_eq!(
            run(json!({"id": "one", "name": "rename"}))?,
            json!({"screen": "rename"})
        );
        let hidden = run(json!({"id": "one", "name": "diff"}))
            .err()
            .context("no changes yet")?;
        assert!(hidden.to_string().contains("isn't available"));
        assert_eq!(
            run(json!({"id": "one", "name": "rules"}))?,
            json!({"screen": "rules", "repository": "/repo"})
        );
        let status = run(json!({"id": "one", "name": "status"}))?;
        assert_eq!(*at(&status, "/screen"), "status");
        assert_eq!(*at(&status, "/status/providerLabel"), "Claude Code");
        let usage = run(json!({"id": "one", "name": "usage"}))?;
        assert_eq!(
            *at(&usage, "/usage/limits"),
            json!([{"section": null, "title": "Current session (5 hours)", "percent": 30.0,
                    "remaining": false, "resetsAt": 1_800_000_000_000_i64}])
        );
        let permissions = run(json!({"id": "one", "name": "permissions", "args": "Plan"}))?;
        assert_eq!(*at(&permissions, "/permissions/activeMode"), "plan");
        run(json!({"id": "one", "name": "rename", "args": "Fix login"}))?;
        run(json!({"id": "one", "name": "model", "args": "gpt-5.6 low"}))?;
        let actions = fake.actions()?;
        assert!(
            matches!(
                actions.as_slice(),
                [
                    Request::Control { control: Control::Compact, .. },
                    Request::Usage { .. },
                    Request::Permissions {
                        action: super::super::super::permissions::Action::SetMode { mode },
                        ..
                    },
                    Request::Rename { title, .. },
                    Request::Control { control: Control::Model { model: Some(model), effort: Some(effort) }, .. },
                ] if mode == "plan" && title == "Fix login" && model == "gpt-5.6" && effort == "low"
            ),
            "{actions:?}"
        );
        Ok(())
    }

    #[test]
    fn provider_skills_are_sent_as_messages_like_the_terminal_sends_them() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(&dir);
        let fake = serve(
            &storage,
            coding(Provider::Codex),
            vec![skill("lint", false), skill("review", true)],
        )?;
        let connection = connection(&storage);
        assert_eq!(
            dispatch(
                &connection.0,
                "session.command",
                &json!({"id": "one", "name": "/review", "args": "45"}),
            )?,
            json!({"message": "Sent /review"})
        );
        dispatch(
            &connection.0,
            "session.command",
            &json!({"id": "one", "name": "$lint", "queue": true}),
        )?;
        dispatch(
            &connection.0,
            "session.send",
            &json!({"id": "one", "text": "$lint the docs", "skills": ["lint"]}),
        )?;
        let unknown = dispatch(
            &connection.0,
            "session.send",
            &json!({"id": "one", "text": "$gone", "skills": ["gone"]}),
        )
        .err()
        .context("unknown skill")?;
        assert!(unknown.to_string().contains("Unknown command `$gone`"));
        let messages: Vec<_> = fake
            .actions()?
            .into_iter()
            .filter_map(|r| match r {
                Request::Control {
                    control:
                        Control::Message {
                            text,
                            queue,
                            skills,
                            ..
                        },
                    ..
                } => Some((
                    text,
                    queue,
                    skills.into_iter().map(|s| s.name).collect::<Vec<_>>(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages,
            [
                ("/review 45".to_owned(), false, vec![]),
                ("$lint".to_owned(), true, vec!["lint".to_owned()]),
                ("$lint the docs".to_owned(), false, vec!["lint".to_owned()]),
            ]
        );
        Ok(())
    }

    fn questions(id: &str, questions: Value) -> super::super::super::Pending {
        super::super::super::Pending {
            id: json!(id),
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions": questions}),
            responded: false,
        }
    }

    #[test]
    fn answers_carry_a_note_the_way_the_terminal_adds_one() -> Result<()> {
        for provider in [Provider::Codex, Provider::Claude] {
            let dir = tempfile::tempdir()?;
            let storage = storage(&dir);
            let mut session = coding(provider);
            session.pending.push(questions(
                "ask",
                json!([
                    {"id": "scope", "question": "Scope?", "options": [{"label": "Small"}, {"label": "Large"}]},
                    {"id": "token", "question": "Token?", "isSecret": true}
                ]),
            ));
            let mut form = super::super::super::Pending {
                id: json!("form"),
                method: "mcpServer/elicitation/request".into(),
                params: json!({"serverName": "Claude Code", "message": "Allow?",
                               "requestedSchema": {"type": "object", "properties": {}}}),
                responded: false,
            };
            super::super::super::elicitation::normalize(&mut form);
            session.pending.push(form);
            let fake = serve(&storage, session, Vec::new())?;
            let connection = connection(&storage);
            let answer = |params: Value| dispatch(&connection.0, "session.answer", &params);
            answer(json!({"id": "one", "request": "ask", "question": "scope",
                          "answer": "Small", "note": "  keep it tiny  "}))?;
            // Older phones send no note; a blank one is no note.
            answer(json!({"id": "one", "request": "ask", "question": "scope", "answer": "Large"}))?;
            answer(json!({"id": "one", "request": "ask", "question": "scope",
                          "answer": "Other text", "note": " "}))?;
            let refusals = [
                json!({"id": "one", "request": "ask", "question": "scope", "answer": null, "note": "why"}),
                json!({"id": "one", "request": "ask", "question": "token", "answer": "s3cret", "note": "hm"}),
                json!({"id": "one", "request": "form", "question": "difu-decision",
                       "answer": "Allow once", "note": "just this once"}),
                json!({"id": "one", "request": "gone", "question": "scope", "answer": "Small", "note": "late"}),
            ];
            let mut errors = Vec::new();
            for params in refusals {
                errors.push(answer(params).err().context("refused")?.to_string());
            }
            assert!(
                errors
                    .first()
                    .is_some_and(|e| e.contains("Choose or enter an answer"))
            );
            assert!(errors.get(1).is_some_and(|e| e.contains("exact answer")));
            assert!(errors.get(2).is_some_and(|e| e.contains("exact answer")));
            assert!(
                errors
                    .get(3)
                    .is_some_and(|e| e.contains("no longer pending"))
            );
            let sent: Vec<_> = fake
                .actions()?
                .into_iter()
                .filter_map(|r| match r {
                    Request::Control {
                        control:
                            Control::AnswerQuestion {
                                request,
                                question,
                                answer,
                            },
                        ..
                    } => Some((request, question, answer)),
                    _ => None,
                })
                .collect();
            let terminal = super::super::super::questions::with_note("Small", "keep it tiny");
            assert_eq!(terminal, "Small\n\nNote: keep it tiny");
            assert_eq!(
                sent,
                [
                    (json!("ask"), "scope".to_owned(), Some(terminal)),
                    (json!("ask"), "scope".to_owned(), Some("Large".to_owned())),
                    (
                        json!("ask"),
                        "scope".to_owned(),
                        Some("Other text".to_owned())
                    ),
                ],
                "{provider:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn requests_report_bad_parameters_by_name() {
        let error = param::<String>(&json!({}), "id");
        assert!(error.is_err_and(|e| e.to_string().contains("`id`")));
        assert_eq!(
            optional::<bool>(&json!({"queue": null}), "queue").ok(),
            Some(None)
        );
    }
}
