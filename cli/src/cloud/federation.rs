//! The local agent service's connections to saved servers: merged session lists
//! and forwarding of requests for `<server>:<session>` ids over `rpc`.
//!
//! A supervisor thread follows `servers.json`; one worker thread per saved
//! server keeps a `client::Connection` open (reconnecting with backoff) and
//! caches the server's session list while a terminal app is listing, so the
//! local `Request::List` never waits on the network.
use super::{
    client::{self, Connection},
    disk,
    servers::{self, SavedServer},
};
use crate::{
    agents::{Control, Reply, Request, Summary, media},
    storage::Storage,
};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Joins a server name and its session id: `hetzner:1790000000000-ab12cd34`.
pub const SEPARATOR: char = ':';
/// A terminal app that listed within this window is still showing the list.
const LISTENING: Duration = Duration::from_secs(15);
/// Cached server sessions stay in the list this long after their last refresh,
/// so a brief network drop does not reshuffle the sidebar.
const STALE: Duration = Duration::from_secs(60);
const REPOSITORIES_STALE: Duration = Duration::from_secs(10);
/// A server's disk status is read again this often while it is wanted; the
/// server answers from its own cached measurement.
const DISK_STALE: Duration = Duration::from_secs(30);
/// Login checks run `gh` and `codex` on the server, so they are read rarely
/// (and again when the `/cloud` panel refreshes).
const LOGINS_STALE: Duration = Duration::from_secs(5 * 60);
const POLL: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Idle `rpc.watch` streams send heartbeats this often, like the local watch.
const HEARTBEAT: Duration = Duration::from_secs(1);

/// What the federation needs from a server connection: `client::Connection` in
/// the service, fakes in tests.
pub trait Remote: Send + Sync {
    fn version(&self) -> String;
    fn full(&self) -> bool;
    fn closed(&self) -> bool;
    fn call(&self, method: &str, params: Value) -> Result<Value>;
    fn rpc(&self, request: &Request) -> Result<Reply>;
    fn watch(&self, id: &str, version: Option<u64>) -> Result<Box<dyn Replies>>;
}

/// A live `rpc.watch` stream; dropping it ends the watch.
pub trait Replies: Send {
    fn next(&mut self, timeout: Duration) -> Next;
}

pub enum Next {
    Reply(Result<Reply>),
    Idle,
    Closed,
}

fn receive(replies: &mpsc::Receiver<Result<Reply>>, timeout: Duration) -> Next {
    match replies.recv_timeout(timeout) {
        Ok(reply) => Next::Reply(reply),
        Err(mpsc::RecvTimeoutError::Timeout) => Next::Idle,
        Err(mpsc::RecvTimeoutError::Disconnected) => Next::Closed,
    }
}

impl Replies for client::Watch {
    fn next(&mut self, timeout: Duration) -> Next {
        receive(&self.replies, timeout)
    }
}

impl Replies for mpsc::Receiver<Result<Reply>> {
    fn next(&mut self, timeout: Duration) -> Next {
        receive(self, timeout)
    }
}

impl Remote for Connection {
    fn version(&self) -> String {
        self.welcome().version
    }
    fn full(&self) -> bool {
        self.welcome().full
    }
    fn closed(&self) -> bool {
        Connection::closed(self)
    }
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        Connection::call(self, method, params)
    }
    fn rpc(&self, request: &Request) -> Result<Reply> {
        Connection::rpc(self, request)
    }
    fn watch(&self, id: &str, version: Option<u64>) -> Result<Box<dyn Replies>> {
        Ok(Box::new(Connection::watch(self, id, version)?))
    }
}

pub type Connector = dyn Fn(&SavedServer) -> Result<Arc<dyn Remote>> + Send + Sync;

/// A saved server as the terminal app's server picker shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub name: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub version: Option<String>,
    /// Why the server's sessions cannot be used from here right now.
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(default)]
    pub repositories: Vec<Repository>,
    /// The server's saved and effective new-session defaults, once read.
    #[serde(default)]
    pub defaults: Option<crate::agents::effective::Defaults>,
    #[serde(default)]
    pub port: u16,
    /// The server's coding sessions, once listed; `None` while they cannot be
    /// used from here (e.g. the versions differ).
    #[serde(default)]
    pub sessions: Option<SessionCounts>,
    /// GitHub, Codex and Claude Code on the server (`server.logins`).
    #[serde(default)]
    pub logins: Vec<Login>,
    /// The server's disk space and what uses it (`server.disk`).
    #[serde(default)]
    pub disk: Option<disk::Report>,
    /// Why `disk` could not be read, such as an older server.
    #[serde(default)]
    pub disk_problem: Option<String>,
}

impl ServerStatus {
    /// The disk status when it is known.
    pub fn disk_status(&self) -> Option<disk::Status> {
        self.disk.as_ref().map(|report| report.status)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionCounts {
    pub active: usize,
    pub archived: usize,
    pub total: usize,
}

/// One tool's login on a server, as `server.logins` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Login {
    pub tool: String,
    pub logged_in: bool,
    pub account: Option<String>,
    pub detail: String,
}

/// One of a server's repositories (`server.repos`); `path` is on the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub cloned: bool,
}

pub fn namespace(server: &str, id: &str) -> String {
    format!("{server}{SEPARATOR}{id}")
}

/// `server:session` → `("server", "session")`.
pub fn split(id: &str) -> Option<(&str, &str)> {
    id.split_once(SEPARATOR)
        .filter(|(server, session)| !server.is_empty() && !session.is_empty())
}

/// Where the local service sends a request.
#[derive(Debug)]
pub enum Target {
    Local(Request),
    /// The request as the server should see it: ids without the prefix.
    Server(String, Request),
}

/// Routing decision: requests naming a server, or carrying a session id with a
/// saved server's prefix, go to that server.
pub fn target(mut request: Request, saved: impl Fn(&str) -> bool) -> Target {
    match &mut request {
        Request::NewAgent {
            server,
            cwd,
            remember_repository,
            ..
        } => {
            if let Some(name) = server.take() {
                // The defaults were read from that server and edited in the
                // new-session form; this computer's directory and saved
                // repository do not apply there.
                *cwd = PathBuf::new();
                *remember_repository = false;
                return Target::Server(name, request);
            }
        }
        Request::RepositoryRules { server, .. }
        | Request::Defaults { server, .. }
        | Request::AgentDefaults { server }
        | Request::DefaultModel { server, .. }
        | Request::Models { server } => {
            if let Some(name) = server.take() {
                return Target::Server(name, request);
            }
        }
        _ => {}
    }
    if let Some(id) = session_id(&mut request) {
        let parts = split(id).map(|(server, local)| (server.to_owned(), local.to_owned()));
        if let Some((server, local)) = parts
            && saved(&server)
        {
            *id = local;
            return Target::Server(server, request);
        }
    }
    Target::Local(request)
}

/// The session a request acts on. Exhaustive so that new requests decide.
fn session_id(request: &mut Request) -> Option<&mut String> {
    match request {
        Request::Subagent { id, .. }
        | Request::Permissions { id, .. }
        | Request::Mcp { id, .. }
        | Request::SlackControl { id, .. }
        | Request::Usage { id }
        | Request::Watch { id, .. }
        | Request::Read { id, .. }
        | Request::AttachPr { id, .. }
        | Request::SessionPrs { id, .. }
        | Request::UploadAttachment { id, .. }
        | Request::ReadArtifact { id, .. }
        | Request::Control { id, .. }
        | Request::Voice { id, .. }
        | Request::Repository { id, .. }
        | Request::Clear { id }
        | Request::Rename { id, .. }
        | Request::Archive { id, .. }
        | Request::Cleanup { id }
        | Request::Delete { id }
        | Request::RegisterWorktree { id, .. }
        | Request::Changes { id }
        | Request::ChangesContext { id, .. }
        | Request::PrChanges { id, .. }
        | Request::Statistics { id }
        | Request::Shells { id }
        | Request::Shell { id, .. }
        | Request::ShellTool { id, .. }
        | Request::WorkspacePaths { id }
        | Request::Skills { id, .. } => Some(id),
        Request::Notifications(_)
        | Request::OrchestratorThread { .. }
        | Request::Ping
        | Request::ServiceVersion
        | Request::Cpu { .. }
        | Request::List
        | Request::Launch { .. }
        | Request::NewAgent { .. }
        | Request::Defaults { .. }
        | Request::AgentDefaults { .. }
        | Request::DefaultModel { .. }
        | Request::Models { .. }
        | Request::Remote { .. }
        | Request::RepositoryRules { .. }
        | Request::Servers
        | Request::Disk { .. }
        | Request::DiskClean { .. }
        | Request::Environment { .. }
        // The local service moves its own session; servers import it.
        | Request::Move { .. }
        | Request::Import { .. } => None,
    }
}

/// A server session's message refers to private copies on this computer
/// (`media`): upload each to the server and send the server's copies instead.
/// The request already carries the server's session id.
fn upload(
    storage: &Storage,
    server: &str,
    remote: &dyn Remote,
    request: &mut Request,
) -> Result<()> {
    let Request::Control {
        id,
        control:
            Control::Message { attachments, .. } | Control::MessageWithAttachments { attachments, .. },
    } = request
    else {
        return Ok(());
    };
    let local = namespace(server, id);
    for attachment in attachments.iter_mut() {
        *attachment = upload_attachment(storage, &local, id, remote, attachment)?;
    }
    Ok(())
}

fn upload_attachment(
    storage: &Storage,
    local: &str,
    id: &str,
    remote: &dyn Remote,
    attachment: &media::Attachment,
) -> Result<media::Attachment> {
    let label = &attachment.label;
    let size = std::fs::metadata(&attachment.path)
        .with_context(|| format!("{label} is missing; paste it again"))?
        .len();
    anyhow::ensure!(
        usize::try_from(size).is_ok_and(|size| size <= media::UPLOAD_LIMIT),
        "{label} is {:.1} MB; attachments for server sessions must be {} MB or smaller",
        size as f64 / f64::from(1u32 << 20),
        media::UPLOAD_LIMIT >> 20
    );
    media::validate(storage, local, attachment).with_context(|| format!("Cannot send {label}"))?;
    let bytes = std::fs::read(&attachment.path)?;
    let extension = attachment
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    match remote.rpc(&Request::UploadAttachment {
        id: id.to_owned(),
        label: label.clone(),
        kind: attachment.kind.clone(),
        extension: extension.to_owned(),
        data: STANDARD.encode(&bytes),
    }) {
        Ok(Reply::Attachment(uploaded)) => Ok(uploaded),
        Ok(Reply::Error(error)) => anyhow::bail!("Cannot upload {label}: {error}"),
        Ok(_) => anyhow::bail!("Unexpected reply while uploading {label}"),
        Err(error) => Err(error.context(format!("Cannot upload {label}"))),
    }
}

/// Keeps what a server answered where this computer's terminal app looks for
/// it, as the local service would have: PR states and published diffs in the
/// GitHub store, and no attachment or artifact copies for a deleted session.
fn remember(storage: &Storage, server: &str, request: &Request, reply: &Reply) {
    let result = match (request, reply) {
        (Request::SessionPrs { .. }, Reply::SessionPrs(lookup)) => lookup
            .links
            .iter()
            .try_for_each(|link| crate::github::store::session(storage, &link.pr)),
        (Request::PrChanges { pr, .. }, Reply::Changes(patch)) => {
            crate::github::store::diff(storage, pr, patch)
        }
        (Request::Delete { id } | Request::Cleanup { id }, Reply::Ok) => {
            let session = namespace(server, id);
            media::cleanup(storage, &session)
                .and(crate::agents::artifacts::forget_copies(storage, &session))
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        eprintln!("Server {server}: {error:#}");
    }
}

/// Requests after which the cached session list is refreshed before replying,
/// so the sidebar reflects the change as promptly as for local sessions.
fn changes_list(request: &Request) -> bool {
    matches!(
        request,
        Request::NewAgent { .. }
            | Request::Control { .. }
            | Request::Repository { .. }
            | Request::Clear { .. }
            | Request::Rename { .. }
            | Request::Archive { .. }
            | Request::Cleanup { .. }
            | Request::Delete { .. }
            | Request::RegisterWorktree { .. }
            | Request::AttachPr { .. }
            | Request::Import { .. }
    )
}

/// Server replies with the server's session ids in this service's namespace.
/// Exhaustive so that new replies decide.
pub fn prefix_reply(server: &str, reply: Reply) -> Reply {
    match reply {
        Reply::Sessions(sessions) => Reply::Sessions(
            sessions
                .into_iter()
                .map(|summary| prefix_summary(server, summary))
                .collect(),
        ),
        Reply::Session(mut session) => {
            session.id = namespace(server, &session.id);
            Reply::Session(session)
        }
        Reply::SessionDelta(mut delta) => {
            delta.session.id = namespace(server, &delta.session.id);
            Reply::SessionDelta(delta)
        }
        Reply::Launched(id) => Reply::Launched(namespace(server, &id)),
        // Sub-agent ids are provider-scoped and shells, snapshots and paths
        // carry no session ids.
        reply @ (Reply::Cpu(_)
        | Reply::Notifications(_)
        | Reply::Permissions { .. }
        | Reply::Mcp(_)
        | Reply::Voice(_)
        | Reply::Usage(_)
        | Reply::Ok
        | Reply::ServiceVersion { .. }
        | Reply::Shells(_)
        | Reply::Shell(_)
        | Reply::Unchanged
        | Reply::ChooseRepository
        | Reply::Changes(_)
        | Reply::ChangesContext(_)
        | Reply::Statistics(_)
        | Reply::WorkspacePaths(_)
        | Reply::Skills { .. }
        | Reply::Defaults { .. }
        | Reply::AgentDefaults(_)
        | Reply::Models(_)
        | Reply::Remote(_)
        | Reply::RepositoryRules { .. }
        | Reply::Servers(_)
        | Reply::Disk(_)
        | Reply::DiskCleaned(_)
        | Reply::Environment(_)
        | Reply::SessionPrs(_)
        | Reply::Attachment(_)
        | Reply::ArtifactFile(_)
        | Reply::Error(_)) => reply,
    }
}

fn prefix_summary(server: &str, mut summary: Summary) -> Summary {
    summary.id = namespace(server, &summary.id);
    summary.server = Some(server.to_owned());
    summary
}

/// The local service's list order.
pub fn sort(list: &mut [Summary]) {
    list.sort_by(|a, b| {
        b.status
            .active()
            .cmp(&a.status.active())
            .then(b.updated.cmp(&a.updated))
            .then(a.id.cmp(&b.id))
    });
}

/// Why a connected server cannot serve `rpc` to this computer.
fn usable(name: &str, remote: &dyn Remote) -> Result<()> {
    anyhow::ensure!(
        remote.full(),
        "This computer has standard access to {name}; pair it again with `difu server join` to use its sessions here"
    );
    let version = remote.version();
    let ours = env!("CARGO_PKG_VERSION");
    anyhow::ensure!(
        version == ours,
        "{name} runs difu {version} and this computer runs difu {ours}. Run `difu server upgrade {name}` to use it from here."
    );
    Ok(())
}

/// A server without `server.environment.*` runs an older difu.
fn environment_error(name: &str, error: anyhow::Error) -> anyhow::Error {
    if format!("{error:#}").contains("Unknown method") {
        anyhow::anyhow!(
            "{name} runs an older difu without environment profiles; run `difu server upgrade {name}`"
        )
    } else {
        error
    }
}

/// A server without `server.disk` runs an older difu.
fn disk_error(name: &str, error: anyhow::Error) -> anyhow::Error {
    if format!("{error:#}").contains("Unknown method") {
        anyhow::anyhow!(
            "{name} runs an older difu without disk status; run `difu server upgrade {name}`"
        )
    } else {
        error
    }
}

fn report(value: Value) -> Result<disk::Report> {
    serde_json::from_value(value).context("Unexpected disk status from the server")
}

fn repositories(value: &Value) -> Result<Vec<Repository>> {
    let list = value
        .get("repositories")
        .cloned()
        .context("Missing repositories in server reply")?;
    Ok(serde_json::from_value(list)?)
}

pub enum Route {
    Local(Request),
    Handled(Result<Reply>),
}

struct Link {
    saved: SavedServer,
    generation: u64,
    remote: Option<Arc<dyn Remote>>,
    error: Option<String>,
    sessions: Vec<Summary>,
    sessions_at: Option<Instant>,
    repositories: Vec<Repository>,
    repositories_at: Option<Instant>,
    defaults: Option<crate::agents::effective::Defaults>,
    disk: Option<disk::Report>,
    disk_problem: Option<String>,
    disk_at: Option<Instant>,
    logins: Vec<Login>,
    logins_at: Option<Instant>,
    /// Someone wants fresh data before the next regular poll.
    refresh: bool,
}

impl Link {
    fn live(&self) -> Option<Arc<dyn Remote>> {
        self.remote.clone().filter(|remote| !remote.closed())
    }
}

#[derive(Default)]
struct State {
    links: BTreeMap<String, Link>,
    /// Modification time of the `servers.json` last loaded (`Some(None)`: none).
    loaded: Option<Option<SystemTime>>,
    listed: Option<Instant>,
    generation: u64,
}

impl State {
    fn link(&mut self, name: &str, generation: u64) -> Option<&mut Link> {
        self.links
            .get_mut(name)
            .filter(|link| link.generation == generation)
    }
    fn listening(&self) -> bool {
        self.listed.is_some_and(|at| at.elapsed() < LISTENING)
    }
}

pub struct Federation {
    storage: Storage,
    connect: Box<Connector>,
    state: Mutex<State>,
    wake: Condvar,
    stopped: AtomicBool,
}

impl Federation {
    pub fn new(storage: Storage, connect: Box<Connector>) -> Self {
        Self {
            storage,
            connect,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
        }
    }

    /// Connects to every saved server in the background.
    pub fn start(storage: Storage) -> Arc<Self> {
        let federation = Arc::new(Self::new(
            storage,
            Box::new(|server| Ok(Arc::new(Connection::connect(server)?) as Arc<dyn Remote>)),
        ));
        let supervisor = federation.clone();
        thread::spawn(move || supervisor.supervise());
        federation
    }

    #[cfg(test)]
    pub(crate) fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn supervise(self: Arc<Self>) {
        while !self.stopped.load(Ordering::Relaxed) {
            for (name, generation) in self.reload() {
                let federation = self.clone();
                thread::spawn(move || federation.work(&name, generation));
            }
            let state = self.lock();
            drop(self.wake.wait_timeout(state, Duration::from_secs(2)));
        }
    }

    /// Follows `servers.json`: drops removed or changed servers (their workers
    /// exit) and returns the new links whose workers must start.
    pub(crate) fn reload(&self) -> Vec<(String, u64)> {
        let modified = servers::path(&self.storage)
            .and_then(|path| Ok(std::fs::metadata(path)?.modified()?))
            .ok();
        let mut state = self.lock();
        if state.loaded == Some(modified) {
            return Vec::new();
        }
        // Report a broken file once per change, keeping the current links.
        state.loaded = Some(modified);
        let saved = match servers::load(&self.storage) {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("Saved servers: {error:#}");
                return Vec::new();
            }
        };
        state.links.retain(|name, link| {
            saved
                .iter()
                .any(|server| &server.name == name && *server == link.saved)
        });
        let mut started = Vec::new();
        for server in saved {
            if state.links.contains_key(&server.name) {
                continue;
            }
            state.generation += 1;
            let generation = state.generation;
            started.push((server.name.clone(), generation));
            state.links.insert(
                server.name.clone(),
                Link {
                    saved: server,
                    generation,
                    remote: None,
                    error: None,
                    sessions: Vec::new(),
                    sessions_at: None,
                    repositories: Vec::new(),
                    repositories_at: None,
                    defaults: None,
                    disk: None,
                    disk_problem: None,
                    disk_at: None,
                    logins: Vec::new(),
                    logins_at: None,
                    refresh: true,
                },
            );
        }
        self.wake.notify_all();
        started
    }

    fn saved(&self, name: &str, generation: u64) -> Option<SavedServer> {
        if self.stopped.load(Ordering::Relaxed) {
            return None;
        }
        self.lock()
            .link(name, generation)
            .map(|link| link.saved.clone())
    }

    fn work(self: Arc<Self>, name: &str, generation: u64) {
        let mut backoff = Duration::from_secs(1);
        while let Some(saved) = self.saved(name, generation) {
            if let Some(remote) = self.connect(name, generation, &saved) {
                backoff = Duration::from_secs(1);
                while self.saved(name, generation).is_some() && !remote.closed() {
                    self.poll(name, generation, &remote);
                    self.pause(name, generation, POLL, true);
                }
                if let Some(link) = self.lock().link(name, generation) {
                    link.remote = None;
                    link.error = Some("Connection closed".into());
                }
            }
            self.pause(name, generation, backoff, false);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Sleeps until `duration` passes, the link is retired, or (when asked)
    /// someone wants fresh data.
    fn pause(&self, name: &str, generation: u64, duration: Duration, refresh: bool) {
        let deadline = Instant::now() + duration;
        let mut state = self.lock();
        loop {
            let now = Instant::now();
            let wanted = match state.link(name, generation) {
                Some(link) => refresh && link.refresh,
                None => true,
            };
            if now >= deadline || wanted || self.stopped.load(Ordering::Relaxed) {
                return;
            }
            state = match self.wake.wait_timeout(state, deadline - now) {
                Ok((state, _)) => state,
                Err(error) => error.into_inner().0,
            };
        }
    }

    pub(crate) fn connect(
        &self,
        name: &str,
        generation: u64,
        saved: &SavedServer,
    ) -> Option<Arc<dyn Remote>> {
        let result = (self.connect)(saved);
        let mut state = self.lock();
        let link = state.link(name, generation)?;
        match result {
            Ok(remote) => {
                link.remote = Some(remote.clone());
                link.error = None;
                link.refresh = true;
                Some(remote)
            }
            Err(error) => {
                link.remote = None;
                link.error = Some(format!("{error:#}"));
                None
            }
        }
    }

    /// Refreshes the cached sessions while someone lists them, and the
    /// repositories, new-session defaults, disk status and logins when they
    /// are stale.
    pub(crate) fn poll(&self, name: &str, generation: u64, remote: &Arc<dyn Remote>) {
        let stale =
            |at: Option<Instant>, limit: Duration| at.is_none_or(|at| at.elapsed() >= limit);
        let (sessions, repos, disk, logins) = {
            let mut state = self.lock();
            let listening = state.listening();
            let Some(link) = state.link(name, generation) else {
                return;
            };
            let refresh = std::mem::take(&mut link.refresh);
            let wanted = refresh || listening;
            (
                wanted && usable(name, remote.as_ref()).is_ok(),
                wanted && stale(link.repositories_at, REPOSITORIES_STALE),
                wanted && stale(link.disk_at, DISK_STALE),
                wanted && stale(link.logins_at, LOGINS_STALE),
            )
        };
        // The phone API answers whatever version the server runs.
        if disk {
            let result = remote
                .call("server.disk", serde_json::json!({}))
                .and_then(report)
                .map_err(|error| disk_error(name, error));
            self.store_disk(name, generation, result);
        }
        if logins {
            let result = remote
                .call("server.logins", serde_json::json!({}))
                .and_then(|value| {
                    Ok(serde_json::from_value::<Vec<Login>>(
                        value.get("logins").cloned().unwrap_or_default(),
                    )?)
                });
            if let Some(link) = self.lock().link(name, generation) {
                link.logins_at = Some(Instant::now());
                if let Ok(logins) = result {
                    link.logins = logins;
                }
            }
        }
        if sessions {
            let reply = remote.rpc(&Request::List);
            self.store_sessions(name, remote, reply);
        }
        if repos {
            let reply = remote
                .call("server.repos", serde_json::json!({}))
                .and_then(|value| repositories(&value));
            // `rpc` needs a matching version; the phone API above does not.
            let defaults = usable(name, remote.as_ref())
                .is_ok()
                .then(|| remote.rpc(&Request::AgentDefaults { server: None }));
            if let Some(link) = self.lock().link(name, generation) {
                match reply {
                    Ok(list) => {
                        link.repositories = list;
                        link.repositories_at = Some(Instant::now());
                    }
                    Err(error) => link.error = Some(format!("{error:#}")),
                }
                // Without them the new-session form starts from blank choices.
                if let Some(Ok(Reply::AgentDefaults(defaults))) = defaults {
                    link.defaults = Some(*defaults);
                }
            }
        }
    }

    fn store_disk(&self, name: &str, generation: u64, result: Result<disk::Report>) {
        if let Some(link) = self.lock().link(name, generation) {
            link.disk_at = Some(Instant::now());
            match result {
                Ok(report) => {
                    link.disk = Some(report);
                    link.disk_problem = None;
                }
                Err(error) => link.disk_problem = Some(format!("{error:#}")),
            }
        }
    }

    fn store_sessions(&self, name: &str, remote: &Arc<dyn Remote>, reply: Result<Reply>) {
        let mut state = self.lock();
        let Some(link) = state.links.get_mut(name).filter(|link| {
            link.remote
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, remote))
        }) else {
            return;
        };
        match reply.map(|reply| prefix_reply(name, reply)) {
            Ok(Reply::Sessions(sessions)) => {
                // Server-side Slack threads and orchestrators stay on the server.
                link.sessions = sessions
                    .into_iter()
                    .filter(|summary| summary.kind != "Orchestrator")
                    .collect();
                link.sessions_at = Some(Instant::now());
            }
            Ok(Reply::Error(error)) => link.error = Some(error),
            Ok(_) => link.error = Some("Unexpected session list reply".into()),
            Err(error) => link.error = Some(format!("{error:#}")),
        }
    }

    /// Local sessions plus every usable server's cached sessions.
    pub fn list(&self, mut local: Vec<Summary>) -> Vec<Summary> {
        let mut state = self.lock();
        let returning = !state.listening();
        state.listed = Some(Instant::now());
        let mut wake = false;
        for (name, link) in &mut state.links {
            if let Some(remote) = link.live()
                && usable(name, remote.as_ref()).is_err()
            {
                continue;
            }
            if returning || link.sessions_at.is_none() {
                link.refresh = true;
                wake = true;
            }
            if link.sessions_at.is_some_and(|at| at.elapsed() < STALE) {
                local.extend(link.sessions.iter().cloned());
            }
        }
        drop(state);
        if wake {
            self.wake.notify_all();
        }
        sort(&mut local);
        local
    }

    /// Saved servers with their state and repositories, for the server picker.
    pub fn servers(&self) -> Vec<ServerStatus> {
        let mut state = self.lock();
        let mut wake = false;
        let list = state
            .links
            .iter_mut()
            .map(|(name, link)| {
                let remote = link.live();
                if remote.is_some()
                    && link
                        .repositories_at
                        .is_none_or(|at| at.elapsed() >= REPOSITORIES_STALE)
                {
                    link.refresh = true;
                    wake = true;
                }
                ServerStatus {
                    name: name.clone(),
                    hosts: link.saved.hosts.clone(),
                    connected: remote.is_some(),
                    version: remote.as_ref().map(|remote| remote.version()),
                    problem: match &remote {
                        Some(remote) => usable(name, remote.as_ref())
                            .err()
                            .map(|error| format!("{error:#}")),
                        None => Some(
                            link.error
                                .clone()
                                .map(|error| format!("Not connected: {error}"))
                                .unwrap_or_else(|| "Connecting…".into()),
                        ),
                    },
                    repositories: link.repositories.clone(),
                    defaults: link.defaults.clone(),
                    port: link.saved.port,
                    sessions: link.sessions_at.map(|_| SessionCounts {
                        active: link
                            .sessions
                            .iter()
                            .filter(|session| session.status.active())
                            .count(),
                        archived: link
                            .sessions
                            .iter()
                            .filter(|session| session.archived)
                            .count(),
                        total: link.sessions.len(),
                    }),
                    logins: link.logins.clone(),
                    disk: link.disk.clone(),
                    disk_problem: link.disk_problem.clone(),
                }
            })
            .collect();
        drop(state);
        if wake {
            self.wake.notify_all();
        }
        list
    }

    /// A usable connection to a saved server, or why there is none.
    pub fn remote(&self, name: &str) -> Result<Arc<dyn Remote>> {
        let state = self.lock();
        let link = state
            .links
            .get(name)
            .with_context(|| format!("No saved server named {name}"))?;
        let remote = link.live().with_context(|| match &link.error {
            Some(error) => format!("Server {name} is not connected: {error}"),
            None => format!("Server {name} is not connected yet"),
        })?;
        drop(state);
        usable(name, remote.as_ref())?;
        Ok(remote)
    }

    /// `remote`, waiting up to `patience` while saved servers are still being
    /// loaded or `name` is still connecting (right after the service starts).
    pub fn remote_within(&self, name: &str, patience: Duration) -> Result<Arc<dyn Remote>> {
        let deadline = Instant::now() + patience;
        loop {
            let connecting = {
                let state = self.lock();
                state.loaded.is_none()
                    || state
                        .links
                        .get(name)
                        .is_some_and(|link| link.live().is_none() && link.error.is_none())
            };
            if !connecting || Instant::now() >= deadline {
                return self.remote(name);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn known(&self) -> BTreeSet<String> {
        self.lock().links.keys().cloned().collect()
    }

    /// A saved server's live connection, whatever version it runs.
    fn connected(&self, name: &str) -> Result<(Arc<dyn Remote>, u64)> {
        let state = self.lock();
        let link = state
            .links
            .get(name)
            .with_context(|| format!("No saved server named {name}"))?;
        let remote = link.live().with_context(|| match &link.error {
            Some(error) => format!("Server {name} is not connected: {error}"),
            None => format!("Server {name} is not connected yet"),
        })?;
        Ok((remote, link.generation))
    }

    /// A saved server's disk status, read now. `refresh` asks the server to
    /// measure again and this computer to read its logins and repositories
    /// again, as the `/cloud` panel's Refresh does.
    pub fn disk(&self, name: &str, refresh: bool) -> Result<disk::Report> {
        let (remote, generation) = self.connected(name)?;
        if refresh {
            if let Some(link) = self.lock().link(name, generation) {
                link.logins_at = None;
                link.repositories_at = None;
                link.refresh = true;
            }
            self.wake.notify_all();
        }
        let result = remote
            .call("server.disk", serde_json::json!({"refresh": refresh}))
            .and_then(report)
            .map_err(|error| disk_error(name, error));
        match result {
            Ok(report) => {
                self.store_disk(name, generation, Ok(report.clone()));
                Ok(report)
            }
            Err(error) => {
                self.store_disk(name, generation, Err(anyhow::anyhow!("{error:#}")));
                Err(error)
            }
        }
    }

    /// Runs a disk cleanup on a saved server, then reads its disk status and
    /// sessions again.
    pub fn clean(&self, name: &str, action: disk::Action) -> Result<disk::Cleaned> {
        let (remote, generation) = self.connected(name)?;
        let value = remote
            .call("server.disk.clean", serde_json::json!({"action": action}))
            .map_err(|error| disk_error(name, error))?;
        let cleaned: disk::Cleaned =
            serde_json::from_value(value).context("Unexpected cleanup reply from the server")?;
        let result = remote
            .call("server.disk", serde_json::json!({}))
            .and_then(report);
        self.store_disk(name, generation, result);
        // Removed worktrees show in the sessions list.
        if usable(name, remote.as_ref()).is_ok() {
            let sessions = remote.rpc(&Request::List);
            self.store_sessions(name, &remote, sessions);
        }
        Ok(cleaned)
    }

    /// Reads, or replaces with `profile`, a repository's environment profile
    /// on a saved server through its phone API (`server.environment.*`).
    pub fn environment(
        &self,
        name: &str,
        repository: &str,
        profile: Option<&crate::cloud::environments::Profile>,
    ) -> Result<crate::cloud::environments::View> {
        let (remote, _) = self.connected(name)?;
        let value = match profile {
            None => remote.call(
                "server.environment.get",
                serde_json::json!({"repository": repository}),
            ),
            Some(profile) => remote.call(
                "server.environment.set",
                serde_json::json!({"repository": repository, "profile": profile}),
            ),
        }
        .map_err(|error| environment_error(name, error))?;
        serde_json::from_value(value).context("Unexpected environment reply from the server")
    }

    /// Answers requests for server sessions; hands everything else back.
    pub fn route(&self, request: Request) -> Route {
        // Disk status, cleanups and environment profiles use the phone API,
        // which works whatever version the server runs.
        match request {
            Request::Environment {
                server: Some(name),
                repository,
                profile,
                ..
            } => {
                return Route::Handled(
                    self.environment(&name, &repository, profile.as_ref())
                        .map(Reply::Environment),
                );
            }
            Request::Disk {
                server: Some(name),
                refresh,
            } => return Route::Handled(self.disk(&name, refresh).map(Reply::Disk)),
            Request::DiskClean {
                server: Some(name),
                action,
            } => return Route::Handled(self.clean(&name, action).map(Reply::DiskCleaned)),
            _ => {}
        }
        let known = self.known();
        match target(request, |name| known.contains(name)) {
            Target::Local(request) => Route::Local(request),
            Target::Server(name, request) => Route::Handled(self.forward(&name, &request)),
        }
    }

    /// Sends `request` to a saved server; replies come back namespaced.
    pub(crate) fn forward(&self, name: &str, request: &Request) -> Result<Reply> {
        let remote = self.remote(name)?;
        // Only messages with attachments are copied, to swap in the server's paths.
        let uploaded = match request {
            Request::Control {
                control:
                    Control::Message { attachments, .. }
                    | Control::MessageWithAttachments { attachments, .. },
                ..
            } if !attachments.is_empty() => {
                let mut copy = request.clone();
                upload(&self.storage, name, remote.as_ref(), &mut copy)?;
                Some(copy)
            }
            _ => None,
        };
        let request = uploaded.as_ref().unwrap_or(request);
        let reply = prefix_reply(name, remote.rpc(request)?);
        remember(&self.storage, name, request, &reply);
        if changes_list(request) && !matches!(reply, Reply::Error(_)) {
            let sessions = remote.rpc(&Request::List);
            self.store_sessions(name, &remote, sessions);
        }
        Ok(reply)
    }

    /// The server and its session id when `id` belongs to a saved server.
    pub fn watch_target(&self, id: &str) -> Option<(String, String)> {
        let (server, session) = split(id)?;
        self.lock()
            .links
            .contains_key(server)
            .then(|| (server.to_owned(), session.to_owned()))
    }

    /// Relays `rpc.watch` for a server session exactly like the local watch:
    /// a baseline, then deltas, with heartbeats while the server is quiet so a
    /// closed terminal is noticed. Returns after sending an error.
    pub fn watch(
        &self,
        server: &str,
        id: &str,
        mut send: impl FnMut(&Reply) -> Result<()>,
    ) -> Result<()> {
        // Like the local watch, every subscription starts from a full baseline
        // (the terminal app applies deltas only on top of one), so the
        // subscriber's version is not passed on.
        let mut stream = match self
            .remote(server)
            .and_then(|remote| remote.watch(id, None))
        {
            Ok(stream) => stream,
            Err(error) => return send(&Reply::Error(format!("{error:#}"))),
        };
        loop {
            let reply = match stream.next(HEARTBEAT) {
                Next::Reply(Ok(reply)) => prefix_reply(server, reply),
                Next::Reply(Err(error)) => Reply::Error(format!("{error:#}")),
                Next::Idle => Reply::Unchanged,
                Next::Closed => Reply::Error(format!("Lost the connection to {server}")),
            };
            send(&reply)?;
            if matches!(reply, Reply::Error(_)) {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::agents::{Job, Launch, Session, Status};
    use crate::{agents::effective::Defaults, storage::AgentDefaults};
    use servers::tests::{server, storage};

    /// Session id, version and the sender feeding that `rpc.watch`.
    pub(crate) type OpenWatch = (String, Option<u64>, mpsc::Sender<Result<Reply>>);

    /// An in-memory server: answers `rpc` from a fixed session map and records
    /// every request it receives.
    pub(crate) struct Fake {
        pub version: String,
        pub full: bool,
        pub closed: AtomicBool,
        pub sessions: Mutex<Vec<Session>>,
        pub requests: Mutex<Vec<Request>>,
        pub watches: Mutex<Vec<OpenWatch>>,
        /// Phone-API calls: method and params.
        pub calls: Mutex<Vec<(String, Value)>>,
        /// What `server.disk` answers; `None` answers like an older server.
        pub disk: Mutex<Option<Value>>,
    }

    impl Fake {
        pub(crate) fn with_version(sessions: Vec<Session>, version: &str) -> Arc<Self> {
            Arc::new(Self {
                version: version.into(),
                full: true,
                closed: AtomicBool::new(false),
                sessions: Mutex::new(sessions),
                requests: Mutex::new(Vec::new()),
                watches: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                disk: Mutex::new(Some(serde_json::json!({
                    "path": "/srv/difu/agents", "total": 100_000_000_000u64,
                    "used": 90_000_000_000u64, "free": 10_000_000_000u64, "status": "low",
                    "measuredAt": 1, "measuring": false,
                    "breakdown": {"sessions": [{"id": "1", "title": "Task", "archived": true, "active": false, "bytes": 4_000_000_000u64, "buildOutputs": 1_000_000_000u64}],
                        "repositories": 1, "attachments": 2, "caches": 3,
                        "sandbox": {"available": true, "images": 4, "containers": 5, "volumes": 6, "reclaimable": 7},
                        "other": 8}
                }))),
            })
        }
        pub(crate) fn calls(&self) -> Vec<(String, Value)> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }
        pub(crate) fn new(sessions: Vec<Session>) -> Arc<Self> {
            Self::with_version(sessions, env!("CARGO_PKG_VERSION"))
        }
        pub(crate) fn requests(&self) -> Vec<Request> {
            self.requests
                .lock()
                .map(|requests| requests.clone())
                .unwrap_or_default()
        }
    }

    impl Remote for Fake {
        fn version(&self) -> String {
            self.version.clone()
        }
        fn full(&self) -> bool {
            self.full
        }
        fn closed(&self) -> bool {
            self.closed.load(Ordering::Relaxed)
        }
        fn call(&self, method: &str, params: Value) -> Result<Value> {
            self.calls
                .lock()
                .map_err(|_| anyhow::anyhow!("lock"))?
                .push((method.to_owned(), params.clone()));
            match method {
                "server.repos" => Ok(serde_json::json!({"repositories":[
                    {"name":"acme/api","path":"/srv/difu/repos/acme/api","cloned":true,"fetchedAt":1}
                ],"refreshMinutes":10})),
                "server.disk" => self
                    .disk
                    .lock()
                    .map_err(|_| anyhow::anyhow!("lock"))?
                    .clone()
                    .context("Unknown method `server.disk`; update difu on your computer"),
                "server.disk.clean" => Ok(serde_json::json!({
                    "freed": 4_000_000_000u64,
                    "summary": format!("Cleaned {}", params.get("action").and_then(Value::as_str).unwrap_or_default()),
                })),
                "server.environment.get" => Ok(serde_json::json!({
                    "repository": params.get("repository").cloned().unwrap_or_default(),
                    "profile": {"tools": {"node": "24"}, "setup": [], "env": {}, "notes": [],
                        "history": [{"at": 1, "by": "Codex in session 1", "summary": "node 24"}]},
                    "detected": [{"tool": "node", "version": "24", "source": ".nvmrc"}],
                })),
                "server.environment.set" => Ok(serde_json::json!({
                    "repository": params.get("repository").cloned().unwrap_or_default(),
                    "profile": params.get("profile").cloned().unwrap_or_default(),
                    "detected": [],
                })),
                "server.logins" => Ok(serde_json::json!({"logins": [
                    {"tool": "github", "loggedIn": true, "account": "octocat", "detail": "Logged in"},
                    {"tool": "codex", "loggedIn": false, "account": null, "detail": "Not logged in"},
                    {"tool": "claude", "loggedIn": true, "account": null, "detail": "Token saved"}
                ]})),
                _ => anyhow::bail!("unexpected {method}"),
            }
        }
        fn rpc(&self, request: &Request) -> Result<Reply> {
            self.requests
                .lock()
                .map_err(|_| anyhow::anyhow!("lock"))?
                .push(request.clone());
            let mut sessions = self.sessions.lock().map_err(|_| anyhow::anyhow!("lock"))?;
            Ok(match request {
                Request::List => Reply::Sessions(sessions.iter().map(Session::summary).collect()),
                Request::Read { id, .. } => match sessions.iter().find(|s| &s.id == id) {
                    Some(session) => Reply::Session(Box::new(session.clone())),
                    None => Reply::Error("Session not found".into()),
                },
                Request::NewAgent { .. } => Reply::Launched("new".into()),
                Request::RepositoryRules { repository, .. } => Reply::RepositoryRules {
                    root: repository.clone(),
                    rules: "server rules".into(),
                },
                Request::AgentDefaults { .. } => Reply::AgentDefaults(Box::new(server_defaults())),
                Request::DefaultModel { model, effort, .. } => {
                    Reply::AgentDefaults(Box::new(Defaults::new(
                        AgentDefaults {
                            model: model.clone(),
                            effort: effort.clone(),
                            ..server_defaults().saved
                        },
                        || None,
                        true,
                    )))
                }
                Request::Models { .. } => Reply::Models(server_models()),
                Request::Defaults { cwd, .. } => Reply::Defaults {
                    model: format!("codex-at-{}", cwd.display()),
                    effort: "medium".into(),
                    permissions: "{}".into(),
                },
                // The server keeps its own copy and names it by its own path.
                Request::UploadAttachment {
                    id,
                    label,
                    kind,
                    extension,
                    data,
                } => Reply::Attachment(media::Attachment {
                    label: label.clone(),
                    path: format!("/srv/difu/agents/attachments/{id}/media-{label}.{extension}")
                        .replace(' ', "-")
                        .into(),
                    kind: kind.clone(),
                    hash: format!(
                        "{:x}",
                        <sha2::Sha256 as sha2::Digest>::digest(STANDARD.decode(data)?)
                    ),
                }),
                Request::SessionPrs { .. } => Reply::SessionPrs(lookup()),
                Request::PrChanges { .. } => Reply::Changes(PATCH.into()),
                // The server switches its own session's mode: Shift+Tab goes
                // from Manual to Accept edits, a chosen mode is taken as is.
                Request::Permissions { id, action } => {
                    let session = sessions.iter_mut().find(|s| &s.id == id);
                    let current = session
                        .as_ref()
                        .and_then(|s| s.permission_state.mode.clone())
                        .unwrap_or_else(|| "default".into());
                    let mode = match action {
                        crate::agents::permissions::Action::SetMode { mode } => mode.clone(),
                        crate::agents::permissions::Action::CycleMode => "acceptEdits".into(),
                        _ => current,
                    };
                    if let Some(session) = session {
                        session.permission_state.mode = Some(mode.clone());
                        session.version += 1;
                    }
                    Reply::Permissions {
                        snapshot: crate::agents::permissions::Snapshot {
                            provider: crate::agents::provider::Provider::Claude,
                            active_mode: Some(mode),
                            modes: crate::agents::permissions::mode_choices(
                                crate::agents::provider::Provider::Claude,
                            ),
                            ..Default::default()
                        },
                    }
                }
                _ => Reply::Ok,
            })
        }
        fn watch(&self, id: &str, version: Option<u64>) -> Result<Box<dyn Replies>> {
            let (tx, rx) = mpsc::channel();
            self.watches
                .lock()
                .map_err(|_| anyhow::anyhow!("lock"))?
                .push((id.into(), version, tx));
            Ok(Box::new(rx))
        }
    }

    /// What every fake server has saved for new sessions.
    /// What every fake server saved, as it reports its defaults: a server
    /// (Claude Code starts in Auto, Codex with "Approve for me").
    pub(crate) fn server_defaults() -> Defaults {
        Defaults::new(
            AgentDefaults {
                model: Some("server-model".into()),
                effort: Some("high".into()),
                repository: Some("/srv/difu/repos/acme/api".into()),
                ..AgentDefaults::default()
            },
            || None,
            true,
        )
    }

    /// The models every fake server's logins offer.
    pub(crate) fn server_models() -> Vec<crate::model::ModelInfo> {
        vec![
            crate::model::ModelInfo {
                id: "server-model".into(),
                name: "Server Model".into(),
                efforts: vec!["low".into(), "high".into()],
            },
            crate::model::ModelInfo {
                id: "claude/opus".into(),
                name: "Opus".into(),
                efforts: vec!["max".into()],
            },
        ]
    }

    const PATCH: &str =
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";

    fn pr(number: u64, state: &str) -> crate::github::SessionPr {
        crate::github::SessionPr {
            summary: None,
            key: crate::model::PrKey {
                owner: "acme".into(),
                repo: "api".into(),
                number,
            },
            state: state.into(),
            draft: false,
            conflicts: false,
            head_branch: "difu/agent-1".into(),
            head: String::new(),
            updated: "2026-11-01T00:00:00Z".into(),
        }
    }

    /// What a server answers for its session's PRs: one discovered on the
    /// session's branch and one attached from another repository.
    fn lookup() -> crate::agents::pr_cache::Lookup {
        let mut attached = pr(7, "MERGED");
        attached.key.repo = "web".into();
        crate::agents::pr_cache::Lookup {
            links: vec![
                crate::agents::pr_cache::SessionLink {
                    workspace: "/srv/difu/agents/worktrees/1".into(),
                    pr: pr(42, "OPEN"),
                    workspace_revision: 0,
                    attached: false,
                },
                crate::agents::pr_cache::SessionLink {
                    workspace: "/srv/difu/agents/worktrees/1".into(),
                    pr: attached,
                    workspace_revision: 0,
                    attached: true,
                },
            ],
            branch: Some("difu/agent-1".into()),
            error: None,
        }
    }

    pub(crate) fn session(id: &str) -> Session {
        let mut session = Session::new(
            id.into(),
            Job::Coding(Launch {
                repository: "/srv/difu/repos/acme/api".into(),
                isolated: true,
                base: "origin/main".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session
    }

    /// A federation whose saved servers are connected to the given fakes,
    /// without background threads.
    pub(crate) fn connected(
        servers: Vec<(&str, Arc<Fake>)>,
    ) -> Result<(tempfile::TempDir, Federation)> {
        connected_to(
            servers
                .into_iter()
                .map(|(name, fake)| (name, fake as Arc<dyn Remote>))
                .collect(),
        )
    }

    /// `connected` with any `Remote`, such as a real service standing in for
    /// a server.
    pub(crate) fn connected_to(
        servers: Vec<(&str, Arc<dyn Remote>)>,
    ) -> Result<(tempfile::TempDir, Federation)> {
        let (dir, storage) = storage()?;
        let remotes = servers
            .into_iter()
            .map(|(name, remote)| (name.to_owned(), remote))
            .collect();
        Ok((dir, linked(storage, remotes)?))
    }

    /// Saves a server per remote in `storage` and connects a federation to
    /// them, without background threads.
    pub(crate) fn linked(
        storage: Storage,
        remotes: BTreeMap<String, Arc<dyn Remote>>,
    ) -> Result<Federation> {
        for name in remotes.keys() {
            servers::save(&storage, server(name, &format!("id-{name}")))?;
        }
        let federation = Federation::new(
            storage,
            Box::new(move |saved| remotes.get(&saved.name).cloned().context("unreachable")),
        );
        for (name, generation) in federation.reload() {
            let saved = federation.saved(&name, generation).context("saved")?;
            if let Some(remote) = federation.connect(&name, generation, &saved) {
                federation.poll(&name, generation, &remote);
            }
        }
        Ok(federation)
    }

    #[test]
    fn moves_wait_briefly_for_a_server_that_is_still_connecting() -> Result<()> {
        let (_dir, connected) = connected(vec![("box", Fake::new(Vec::new()))])?;
        assert!(connected.remote_within("box", Duration::ZERO).is_ok());
        let error = connected
            .remote_within("nowhere", Duration::from_secs(5))
            .err()
            .context("unknown server")?;
        assert!(format!("{error:#}").contains("No saved server named nowhere"));

        let (_dir, storage) = storage()?;
        servers::save(&storage, server("box", "id-box"))?;
        let connecting = Federation::new(storage, Box::new(|_| anyhow::bail!("unreachable")));
        connecting.reload();
        let started = Instant::now();
        let error = connecting
            .remote_within("box", Duration::from_millis(250))
            .err()
            .context("still connecting")?;
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(
            format!("{error:#}").contains("not connected yet"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn requests_for_namespaced_sessions_route_to_their_server_without_the_prefix() -> Result<()> {
        let saved = |name: &str| name == "box";
        let Target::Server(server, Request::Read { id, version }) = target(
            Request::Read {
                id: "box:123-abc".into(),
                version: Some(4),
            },
            saved,
        ) else {
            anyhow::bail!("expected a server read");
        };
        assert_eq!(
            (server.as_str(), id.as_str(), version),
            ("box", "123-abc", Some(4))
        );
        let Target::Server(_, Request::Control { id, .. }) = target(
            Request::Control {
                id: "box:1".into(),
                control: crate::agents::Control::Interrupt,
            },
            saved,
        ) else {
            anyhow::bail!("expected a server control");
        };
        assert_eq!(id, "1");
        // Local ids and unknown prefixes stay local, untouched.
        for id in ["123-abc", "orchestrator", "gone:1", ":1", "box:"] {
            let Target::Local(Request::Delete { id: kept }) =
                target(Request::Delete { id: id.into() }, saved)
            else {
                anyhow::bail!("{id} should stay local");
            };
            assert_eq!(kept, id);
        }
        assert!(matches!(
            target(Request::List, saved),
            Target::Local(Request::List)
        ));
        // PR lookups and uploads act on the server's session.
        let Target::Server(_, Request::SessionPrs { id, history: true }) = target(
            Request::SessionPrs {
                id: "box:1".into(),
                history: true,
            },
            saved,
        ) else {
            anyhow::bail!("expected a server PR lookup");
        };
        assert_eq!(id, "1");
        let Target::Server(_, Request::UploadAttachment { id, .. }) = target(
            Request::UploadAttachment {
                id: "box:1".into(),
                label: "image 1".into(),
                kind: media::Kind::Image,
                extension: "png".into(),
                data: String::new(),
            },
            saved,
        ) else {
            anyhow::bail!("expected a server upload");
        };
        assert_eq!(id, "1");
        let Target::Server(_, Request::ReadArtifact { id, .. }) = target(
            Request::ReadArtifact {
                id: "box:1".into(),
                path: "report.html".into(),
                workspace: None,
            },
            saved,
        ) else {
            anyhow::bail!("expected a server artifact read");
        };
        assert_eq!(id, "1");
        let lookup = Reply::SessionPrs(lookup());
        assert!(
            matches!(prefix_reply("box", lookup), Reply::SessionPrs(l) if l == super::tests::lookup())
        );
        Ok(())
    }

    #[test]
    fn new_agents_and_rules_for_a_server_keep_choices_but_not_local_paths() -> Result<()> {
        // The new-session form reads these from the server and the user edits
        // them there, so they carry over; this computer's directory does not.
        let defaults = AgentDefaults {
            provider: Some(crate::agents::provider::Provider::Claude),
            model: Some("opus".into()),
            effort: Some("high".into()),
            repository: Some("/srv/difu/repos/acme/api".into()),
            ..AgentDefaults::default()
        };
        let Target::Server(
            server,
            Request::NewAgent {
                server: forwarded,
                defaults,
                cwd,
                remember_repository,
            },
        ) = target(
            Request::NewAgent {
                server: Some("box".into()),
                defaults,
                cwd: "/Users/me/code".into(),
                remember_repository: true,
            },
            |_| false,
        )
        else {
            anyhow::bail!("expected a server launch");
        };
        assert_eq!(server, "box");
        assert_eq!(forwarded, None);
        assert_eq!(cwd, PathBuf::new());
        assert!(!remember_repository);
        assert_eq!(defaults.repository, Some("/srv/difu/repos/acme/api".into()));
        assert_eq!(
            (
                defaults.provider,
                defaults.model.as_deref(),
                defaults.effort.as_deref()
            ),
            (
                Some(crate::agents::provider::Provider::Claude),
                Some("opus"),
                Some("high")
            )
        );
        let local = target(
            Request::NewAgent {
                server: None,
                defaults: AgentDefaults::default(),
                cwd: "/Users/me/code".into(),
                remember_repository: true,
            },
            |_| true,
        );
        assert!(matches!(
            local,
            Target::Local(Request::NewAgent {
                remember_repository: true,
                ..
            })
        ));
        let Target::Server(server, Request::RepositoryRules { server: None, .. }) = target(
            Request::RepositoryRules {
                server: Some("box".into()),
                repository: "/srv/difu/repos/acme/api".into(),
                rules: None,
            },
            |_| false,
        ) else {
            anyhow::bail!("expected server rules");
        };
        assert_eq!(server, "box");
        Ok(())
    }

    #[test]
    fn session_panels_reach_the_server_running_the_session() -> Result<()> {
        // Skills, MCP, permissions, usage, file paths and workspace changes
        // read the session's machine, so they must reach the server.
        let id = || "box:7".to_owned();
        let requests = [
            Request::Skills {
                id: id(),
                force: true,
            },
            Request::Mcp {
                id: id(),
                action: crate::agents::mcp::Action::Refresh,
            },
            Request::Permissions {
                id: id(),
                action: crate::agents::permissions::Action::Refresh,
            },
            // Mode switches (Shift+Tab, the permissions panel, presets).
            Request::Permissions {
                id: id(),
                action: crate::agents::permissions::Action::CycleMode,
            },
            Request::Permissions {
                id: id(),
                action: crate::agents::permissions::Action::SetMode {
                    mode: "auto".into(),
                },
            },
            Request::Permissions {
                id: id(),
                action: crate::agents::permissions::Action::SetPreset {
                    preset: crate::agents::permissions::CodexPreset::Auto,
                },
            },
            Request::Usage { id: id() },
            Request::WorkspacePaths { id: id() },
            Request::RegisterWorktree {
                id: id(),
                path: "/srv/difu/worktrees/7".into(),
                base: None,
            },
            Request::Repository {
                id: id(),
                repository: "/srv/difu/repos/acme/web".into(),
            },
        ];
        for request in requests {
            let label = format!("{request:?}");
            let Target::Server(server, mut forwarded) = target(request, |name| name == "box")
            else {
                anyhow::bail!("{label} stayed on this computer");
            };
            assert_eq!(server, "box");
            assert_eq!(
                session_id(&mut forwarded).map(|id| id.as_str()),
                Some("7"),
                "{label}"
            );
        }
        Ok(())
    }

    #[test]
    fn location_requests_naming_a_server_route_there_and_others_stay_local() -> Result<()> {
        let saved = |name: &str| name == "box";
        let Target::Server(server, Request::Defaults { cwd, server: None }) = target(
            Request::Defaults {
                cwd: "/srv/difu/repos/acme/api".into(),
                server: Some("box".into()),
            },
            saved,
        ) else {
            anyhow::bail!("server Codex defaults");
        };
        assert_eq!(
            (server.as_str(), cwd),
            ("box", PathBuf::from("/srv/difu/repos/acme/api"))
        );
        assert!(matches!(
            target(
                Request::AgentDefaults {
                    server: Some("box".into())
                },
                saved
            ),
            Target::Server(name, Request::AgentDefaults { server: None }) if name == "box"
        ));
        assert!(matches!(
            target(Request::Models { server: Some("box".into()) }, saved),
            Target::Server(name, Request::Models { server: None }) if name == "box"
        ));
        assert!(matches!(
            target(
                Request::DefaultModel {
                    server: Some("box".into()),
                    model: Some("m".into()),
                    effort: None
                },
                saved
            ),
            Target::Server(name, Request::DefaultModel { server: None, model: Some(m), .. })
                if name == "box" && m == "m"
        ));
        // Without a server they answer for this machine.
        for request in [
            Request::Defaults {
                cwd: "/Users/me/code".into(),
                server: None,
            },
            Request::AgentDefaults { server: None },
            Request::Models { server: None },
            Request::DefaultModel {
                server: None,
                model: None,
                effort: None,
            },
        ] {
            assert!(matches!(target(request, saved), Target::Local(_)));
        }
        Ok(())
    }

    #[test]
    fn server_status_carries_the_server_defaults_and_forwards_location_requests() -> Result<()> {
        let fake = Fake::new(Vec::new());
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let status = federation.servers();
        let [status] = status.as_slice() else {
            anyhow::bail!("one server");
        };
        assert_eq!(status.defaults, Some(server_defaults()));
        let Route::Handled(reply) = federation.route(Request::Models {
            server: Some("box".into()),
        }) else {
            anyhow::bail!("models are forwarded");
        };
        assert!(matches!(reply?, Reply::Models(models) if models == server_models()));
        let Route::Handled(reply) = federation.route(Request::DefaultModel {
            server: Some("box".into()),
            model: Some("claude/opus".into()),
            effort: None,
        }) else {
            anyhow::bail!("defaults are saved on the server");
        };
        assert!(matches!(
            reply?,
            Reply::AgentDefaults(defaults) if defaults.model.as_deref() == Some("claude/opus")
        ));
        assert!(matches!(
            federation.route(Request::Models { server: None }),
            Route::Local(Request::Models { server: None })
        ));
        let requests = fake.requests();
        assert!(
            requests
                .iter()
                .any(|r| matches!(r, Request::AgentDefaults { server: None }))
        );
        assert!(
            requests
                .iter()
                .any(|r| matches!(r, Request::Models { server: None }))
        );
        Ok(())
    }

    #[test]
    fn server_status_carries_disk_logins_session_counts_and_port() -> Result<()> {
        let mut archived = session("2");
        archived.archived = true;
        let mut working = session("3");
        working.status = Status::Running;
        let fake = Fake::new(vec![session("1"), archived, working]);
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let status = federation.servers();
        let [status] = status.as_slice() else {
            anyhow::bail!("one server");
        };
        assert_eq!(
            status.sessions,
            Some(SessionCounts {
                active: 1,
                archived: 1,
                total: 3
            })
        );
        assert_eq!(status.port, server("box", "id-box").port);
        assert_eq!(status.disk_status(), Some(disk::Status::Low));
        assert_eq!(
            status
                .disk
                .as_ref()
                .map(|d| d.expected(disk::Action::ArchivedWorktrees)),
            Some(4_000_000_000)
        );
        assert!(status.disk_problem.is_none());
        let tools: Vec<(&str, bool)> = status
            .logins
            .iter()
            .map(|login| (login.tool.as_str(), login.logged_in))
            .collect();
        assert_eq!(
            tools,
            vec![("github", true), ("codex", false), ("claude", true)]
        );
        // While listing, sessions refresh but fresh disk status, logins and
        // repositories are not read again.
        let calls = fake.calls().len();
        let lists = fake.requests().len();
        federation.list(Vec::new());
        let generation = federation
            .lock()
            .links
            .get("box")
            .map(|link| link.generation)
            .context("link")?;
        federation.poll("box", generation, &federation.remote("box")?);
        assert_eq!(fake.calls().len(), calls, "{:?}", fake.calls());
        assert!(fake.requests().len() > lists);
        Ok(())
    }

    #[test]
    fn disk_requests_and_cleanups_use_the_phone_api_whatever_version() -> Result<()> {
        let fake = Fake::with_version(vec![session("1")], "0.0.1");
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let Route::Handled(reply) = federation.route(Request::Disk {
            server: Some("box".into()),
            refresh: true,
        }) else {
            anyhow::bail!("disk status comes from the server");
        };
        let Reply::Disk(report) = reply? else {
            anyhow::bail!("expected a disk reply");
        };
        assert_eq!(report.status, disk::Status::Low);
        assert!(
            fake.calls()
                .iter()
                .any(|(method, params)| method == "server.disk"
                    && params == &serde_json::json!({"refresh": true}))
        );
        let Route::Handled(reply) = federation.route(Request::DiskClean {
            server: Some("box".into()),
            action: disk::Action::BuildOutputs,
        }) else {
            anyhow::bail!("cleanups run on the server");
        };
        let Reply::DiskCleaned(cleaned) = reply? else {
            anyhow::bail!("expected a cleanup reply");
        };
        assert_eq!(cleaned.freed, 4_000_000_000);
        assert_eq!(cleaned.summary, "Cleaned buildOutputs");
        assert!(
            fake.calls()
                .iter()
                .any(|(method, params)| method == "server.disk.clean"
                    && params == &serde_json::json!({"action": "buildOutputs"}))
        );
        assert!(
            fake.requests().is_empty(),
            "no rpc with a mismatched version"
        );
        assert!(matches!(
            federation.route(Request::Disk {
                server: None,
                refresh: false
            }),
            Route::Local(Request::Disk { server: None, .. })
        ));
        let Route::Handled(missing) = federation.route(Request::Disk {
            server: Some("nowhere".into()),
            refresh: false,
        }) else {
            anyhow::bail!("unknown servers are answered");
        };
        assert!(missing.is_err_and(|e| e.to_string().contains("No saved server")));
        Ok(())
    }

    #[test]
    fn environment_profiles_use_the_phone_api_whatever_version() -> Result<()> {
        let fake = Fake::with_version(vec![session("1")], "0.0.1");
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let Route::Handled(reply) = federation.route(Request::Environment {
            server: Some("box".into()),
            repository: "acme/api".into(),
            profile: None,
            by: None,
        }) else {
            anyhow::bail!("environment profiles come from the server");
        };
        let Reply::Environment(view) = reply? else {
            anyhow::bail!("expected an environment reply");
        };
        assert_eq!(view.repository, "acme/api");
        assert_eq!(
            view.profile.tools.get("node").map(String::as_str),
            Some("24")
        );
        assert_eq!(view.profile.history.len(), 1);
        assert_eq!(
            view.detected.first().map(|d| d.source.as_str()),
            Some(".nvmrc")
        );
        let profile = crate::cloud::environments::Profile {
            setup: vec!["pnpm install".into()],
            ..Default::default()
        };
        let Route::Handled(reply) = federation.route(Request::Environment {
            server: Some("box".into()),
            repository: "acme/api".into(),
            profile: Some(profile.clone()),
            by: None,
        }) else {
            anyhow::bail!("saved on the server");
        };
        let Reply::Environment(saved) = reply? else {
            anyhow::bail!("expected an environment reply");
        };
        assert_eq!(saved.profile.setup, profile.setup);
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|(method, params)| method == "server.environment.get"
                    && params == &serde_json::json!({"repository": "acme/api"}))
        );
        assert!(
            calls
                .iter()
                .any(|(method, params)| method == "server.environment.set"
                    && params.pointer("/profile/setup/0")
                        == Some(&serde_json::json!("pnpm install")))
        );
        assert!(
            fake.requests().is_empty(),
            "no rpc with a mismatched version"
        );
        assert!(matches!(
            federation.route(Request::Environment {
                server: None,
                repository: "acme/api".into(),
                profile: None,
                by: None,
            }),
            Route::Local(Request::Environment { server: None, .. })
        ));
        // An older server has no such methods.
        let error = environment_error(
            "box",
            anyhow::anyhow!(
                "Unknown method `server.environment.get`; update difu on your computer"
            ),
        );
        assert!(
            error.to_string().contains("difu server upgrade box"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn older_servers_report_a_disk_problem_not_a_connection_problem() -> Result<()> {
        let fake = Fake::new(Vec::new());
        if let Ok(mut disk) = fake.disk.lock() {
            *disk = None;
        }
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let status = federation.servers();
        let [status] = status.as_slice() else {
            anyhow::bail!("one server");
        };
        assert!(status.problem.is_none(), "{status:?}");
        assert!(status.disk.is_none());
        assert!(
            status
                .disk_problem
                .as_deref()
                .is_some_and(|p| p.contains("difu server upgrade box")),
            "{status:?}"
        );
        let Route::Handled(reply) = federation.route(Request::Disk {
            server: Some("box".into()),
            refresh: false,
        }) else {
            anyhow::bail!("answered here");
        };
        assert!(reply.is_err_and(|e| e.to_string().contains("older difu")));
        Ok(())
    }

    #[test]
    fn server_replies_carry_namespaced_ids_including_deltas() -> Result<()> {
        let Reply::Sessions(list) =
            prefix_reply("box", Reply::Sessions(vec![session("1").summary()]))
        else {
            anyhow::bail!("sessions");
        };
        let [summary] = list.as_slice() else {
            anyhow::bail!("one summary");
        };
        assert_eq!(
            (summary.id.as_str(), summary.server.as_deref()),
            ("box:1", Some("box"))
        );
        let Reply::Session(full) = prefix_reply("box", Reply::Session(Box::new(session("1"))))
        else {
            anyhow::bail!("session");
        };
        assert_eq!(full.id, "box:1");
        let Reply::Launched(id) = prefix_reply("box", Reply::Launched("2".into())) else {
            anyhow::bail!("launched");
        };
        assert_eq!(id, "box:2");
        // A delta keeps applying on the client after both ids are rewritten.
        let mut base = session("1");
        base.note("agentMessage", "first");
        let mut next = base.clone();
        next.note("agentMessage", "second");
        let mut metadata = serde_json::to_value(&next)?;
        let entries = metadata
            .get_mut("entries")
            .and_then(Value::as_array_mut)
            .context("entries")?;
        let added = entries.pop().context("added entry")?;
        entries.clear();
        let delta: Reply = serde_json::from_value(serde_json::json!({"SessionDelta": {
            "base": base.version, "session": metadata, "count": 2, "entries": [[1, added]]
        }}))?;
        let Reply::SessionDelta(delta) = prefix_reply("box", delta) else {
            anyhow::bail!("delta");
        };
        assert_eq!(delta.session.id, "box:1");
        let mut client = base.clone();
        client.id = "box:1".into();
        delta.apply(&mut client)?;
        assert_eq!(client.id, "box:1");
        assert_eq!(client.entries.len(), 2);
        assert!(matches!(prefix_reply("box", Reply::Ok), Reply::Ok));
        Ok(())
    }

    #[test]
    fn merged_list_namespaces_server_sessions_and_forwards_requests() -> Result<()> {
        let mut orchestrator = session("orchestrator");
        orchestrator.orchestrator = true;
        let fake = Fake::new(vec![session("1"), orchestrator]);
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let list = federation.list(vec![session("local").summary()]);
        let ids: Vec<_> = list
            .iter()
            .map(|s| (s.id.as_str(), s.server.as_deref()))
            .collect();
        assert!(ids.contains(&("local", None)), "{ids:?}");
        assert!(ids.contains(&("box:1", Some("box"))), "{ids:?}");
        assert_eq!(
            ids.len(),
            2,
            "server orchestrators stay on the server: {ids:?}"
        );
        let Route::Handled(reply) = federation.route(Request::Read {
            id: "box:1".into(),
            version: None,
        }) else {
            anyhow::bail!("expected forwarding");
        };
        let Reply::Session(read) = reply? else {
            anyhow::bail!("session");
        };
        assert_eq!(read.id, "box:1");
        let Route::Handled(reply) = federation.route(Request::NewAgent {
            server: Some("box".into()),
            defaults: AgentDefaults::default(),
            cwd: "/Users/me".into(),
            remember_repository: true,
        }) else {
            anyhow::bail!("expected forwarding");
        };
        assert!(matches!(reply?, Reply::Launched(id) if id == "box:new"));
        let Route::Handled(reply) = federation.route(Request::RepositoryRules {
            server: Some("box".into()),
            repository: "/srv/difu/repos/acme/api".into(),
            rules: Some("x".into()),
        }) else {
            anyhow::bail!("expected forwarding");
        };
        assert!(matches!(reply?, Reply::RepositoryRules { rules, .. } if rules == "server rules"));
        assert!(matches!(
            federation.route(Request::Read {
                id: "local".into(),
                version: None
            }),
            Route::Local(Request::Read { .. })
        ));
        let requests = fake.requests();
        assert!(
            requests
                .iter()
                .any(|r| matches!(r, Request::Read { id, .. } if id == "1"))
        );
        assert!(requests.iter().any(|r| matches!(
            r,
            Request::RepositoryRules { server: None, rules: Some(rules), .. } if rules == "x"
        )));
        let status = federation.servers();
        let [status] = status.as_slice() else {
            anyhow::bail!("one server");
        };
        assert!(status.connected && status.problem.is_none(), "{status:?}");
        assert_eq!(
            status.repositories.first().map(|r| r.name.as_str()),
            Some("acme/api")
        );
        Ok(())
    }

    /// A message's attachments as the terminal app sends them: private copies
    /// in this computer's attachment directory for `box:1`.
    fn pasted(federation: &Federation, files: &[(&str, &[u8])]) -> Result<Vec<media::Attachment>> {
        let dir = tempfile::tempdir()?;
        let mut paths = Vec::new();
        for (name, bytes) in files {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes)?;
            paths.push(path);
        }
        let media::Paste::Attachments(mut copies) =
            media::files(&federation.storage, "box:1", paths)?
        else {
            anyhow::bail!("expected media");
        };
        for (index, copy) in copies.iter_mut().enumerate() {
            copy.label = match copy.kind {
                media::Kind::Image => format!("image {}", index + 1),
                media::Kind::Video => format!("video {}", index + 1),
            };
        }
        Ok(copies)
    }

    fn png() -> Result<Vec<u8>> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2))
            .write_to(&mut bytes, image::ImageFormat::Png)?;
        Ok(bytes.into_inner())
    }

    #[test]
    fn server_session_messages_upload_local_attachments_first() -> Result<()> {
        let fake = Fake::new(vec![session("1")]);
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let image = png()?;
        let copies = pasted(&federation, &[("shot.png", &image), ("clip.mp4", b"video")])?;
        let Route::Handled(reply) = federation.route(Request::Control {
            id: "box:1".into(),
            control: Control::MessageWithAttachments {
                text: "compare [image 1] with [video 2]".into(),
                queue: true,
                skills: Vec::new(),
                attachments: copies.clone(),
            },
        }) else {
            anyhow::bail!("expected forwarding");
        };
        assert!(matches!(reply?, Reply::Ok));
        let requests = fake.requests();
        let uploads: Vec<_> = requests
            .iter()
            .filter_map(|request| match request {
                Request::UploadAttachment {
                    id,
                    label,
                    kind,
                    extension,
                    data,
                } => Some((id.as_str(), label.as_str(), kind, extension.as_str(), data)),
                _ => None,
            })
            .collect();
        let [first, second] = uploads.as_slice() else {
            anyhow::bail!("two uploads: {requests:?}");
        };
        assert_eq!(
            (first.0, first.1, first.2, first.3),
            ("1", "image 1", &media::Kind::Image, "png")
        );
        assert_eq!(STANDARD.decode(first.4)?, image);
        assert_eq!(
            (second.1, second.2, second.3),
            ("video 2", &media::Kind::Video, "mp4")
        );
        assert_eq!(STANDARD.decode(second.4)?, b"video");
        // The message itself follows the uploads and names the server's copies.
        let [
            ..,
            Request::UploadAttachment { .. },
            Request::Control {
                id,
                control:
                    Control::MessageWithAttachments {
                        text,
                        queue,
                        attachments,
                        ..
                    },
            },
            Request::List,
        ] = requests.as_slice()
        else {
            anyhow::bail!("message after the uploads: {requests:?}");
        };
        assert_eq!(
            (id.as_str(), text.as_str(), *queue),
            ("1", "compare [image 1] with [video 2]", true)
        );
        assert_eq!(attachments.len(), 2);
        for (sent, copy) in attachments.iter().zip(&copies) {
            assert!(
                sent.path.starts_with("/srv/difu/agents/attachments/1"),
                "{sent:?}"
            );
            assert_eq!(
                (&sent.label, &sent.kind, &sent.hash),
                (&copy.label, &copy.kind, &copy.hash)
            );
        }
        // Messages without media and other servers' replies pass unchanged.
        let Route::Handled(reply) = federation.route(Request::Control {
            id: "box:1".into(),
            control: Control::Message {
                text: "plain".into(),
                queue: false,
                skills: Vec::new(),
                attachments: Vec::new(),
            },
        }) else {
            anyhow::bail!("expected forwarding");
        };
        assert!(matches!(reply?, Reply::Ok));
        assert_eq!(
            fake.requests().len(),
            requests.len() + 2,
            "message and list refresh only"
        );
        Ok(())
    }

    #[test]
    fn oversized_or_tampered_attachments_fail_before_reaching_the_server() -> Result<()> {
        let fake = Fake::new(vec![session("1")]);
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let copies = pasted(&federation, &[("big.mov", b"x")])?;
        let big = copies.first().context("copy")?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&big.path)?
            .set_len(u64::try_from(media::UPLOAD_LIMIT + (1 << 20))?)?;
        let send = |attachments: Vec<media::Attachment>| {
            let Route::Handled(reply) = federation.route(Request::Control {
                id: "box:1".into(),
                control: Control::MessageWithAttachments {
                    text: "look".into(),
                    queue: false,
                    skills: Vec::new(),
                    attachments,
                },
            }) else {
                anyhow::bail!("expected forwarding");
            };
            Ok(format!("{:#}", reply.err().context("the send must fail")?))
        };
        let error = send(copies.clone())?;
        assert!(
            error.contains("video 1 is 33.0 MB") && error.contains("32 MB or smaller"),
            "{error}"
        );
        // A copy that changed after pasting, or that belongs to another
        // session, is refused like a local session's.
        let changed = pasted(&federation, &[("shot.png", &png()?)])?;
        std::fs::write(&changed.first().context("copy")?.path, b"changed")?;
        let error = send(changed)?;
        assert!(error.contains("Cannot send image 1"), "{error}");
        let source = tempfile::tempdir()?;
        let path = source.path().join("other.png");
        std::fs::write(&path, png()?)?;
        let media::Paste::Attachments(mut other) =
            media::files(&federation.storage, "box:2", vec![path])?
        else {
            anyhow::bail!("expected media");
        };
        for copy in &mut other {
            copy.label = "image 1".into();
        }
        let error = send(other)?;
        assert!(error.contains("does not belong to this session"), "{error}");
        assert!(
            fake.requests().iter().all(|r| !matches!(
                r,
                Request::UploadAttachment { .. } | Request::Control { .. }
            )),
            "nothing reached the server"
        );
        Ok(())
    }

    #[test]
    fn server_pr_lookups_and_diffs_are_kept_where_the_terminal_reads_them() -> Result<()> {
        let fake = Fake::new(vec![session("1")]);
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let Route::Handled(reply) = federation.route(Request::SessionPrs {
            id: "box:1".into(),
            history: true,
        }) else {
            anyhow::bail!("expected forwarding");
        };
        let Reply::SessionPrs(found) = reply? else {
            anyhow::bail!("PR lookup");
        };
        assert_eq!(found, lookup());
        assert!(fake.requests().iter().any(|r| matches!(
            r,
            Request::SessionPrs { id, history: true } if id == "1"
        )));
        // The badges' PR states survive a restart of the terminal app.
        let stored = crate::github::store::load(&federation.storage, &pr(42, "OPEN").key)
            .and_then(|pr| pr.session)
            .context("stored PR")?;
        assert_eq!(stored.state, "OPEN");
        // A published diff from the server opens instantly next time.
        let Route::Handled(reply) = federation.route(Request::PrChanges {
            id: "box:1".into(),
            pr: pr(42, "OPEN").key,
        }) else {
            anyhow::bail!("expected forwarding");
        };
        assert!(matches!(reply?, Reply::Changes(patch) if patch == PATCH));
        let diff = crate::github::store::load(&federation.storage, &pr(42, "OPEN").key)
            .and_then(|pr| pr.diff)
            .context("stored diff")?;
        assert_eq!(diff.value, PATCH);
        Ok(())
    }

    #[test]
    fn deleting_a_server_session_removes_its_local_copies() -> Result<()> {
        let fake = Fake::new(vec![session("1")]);
        let (_dir, federation) = connected(vec![("box", fake)])?;
        let copies = pasted(&federation, &[("shot.png", &png()?)])?;
        let copy = copies.first().context("copy")?;
        assert!(copy.path.is_file());
        let artifact = crate::agents::artifacts::save_copy(
            &federation.storage,
            "box:1",
            &crate::agents::artifacts::Artifact {
                path: "report.html".into(),
                title: "Report".into(),
                workspace: None,
            },
            &STANDARD.encode("<h1>Report</h1>"),
        )?;
        assert!(artifact.is_file());
        let Route::Handled(reply) = federation.route(Request::Delete { id: "box:1".into() }) else {
            anyhow::bail!("expected forwarding");
        };
        assert!(matches!(reply?, Reply::Ok));
        assert!(!copy.path.exists());
        assert!(!artifact.exists());
        Ok(())
    }

    #[test]
    fn version_mismatch_refuses_rpc_and_hides_sessions() -> Result<()> {
        let fake = Fake::with_version(vec![session("1")], "0.0.1");
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        let list = federation.list(Vec::new());
        assert!(list.is_empty(), "{list:?}");
        let Route::Handled(reply) = federation.route(Request::Read {
            id: "box:1".into(),
            version: None,
        }) else {
            anyhow::bail!("expected a server request");
        };
        let error = format!("{:#}", reply.err().context("mismatch must fail")?);
        assert!(error.contains("difu server upgrade box"), "{error}");
        assert!(fake.requests().is_empty(), "no rpc reaches the server");
        let status = federation.servers();
        assert!(
            status
                .first()
                .and_then(|s| s.problem.as_deref())
                .is_some_and(|p| p.contains("0.0.1")),
            "{status:?}"
        );
        let mut sent = Vec::new();
        federation.watch("box", "1", |reply| {
            sent.push(reply.clone());
            Ok(())
        })?;
        assert!(matches!(sent.as_slice(), [Reply::Error(e)] if e.contains("upgrade")));
        Ok(())
    }

    #[test]
    fn watch_relays_server_replies_with_heartbeats_and_ends_on_close() -> Result<()> {
        let fake = Fake::new(vec![session("1")]);
        let (_dir, federation) = connected(vec![("box", fake.clone())])?;
        assert_eq!(
            federation.watch_target("box:1"),
            Some(("box".into(), "1".into()))
        );
        assert_eq!(federation.watch_target("other:1"), None);
        assert_eq!(federation.watch_target("1"), None);
        let feeder = {
            let fake = fake.clone();
            thread::spawn(move || -> Result<()> {
                let deadline = Instant::now() + Duration::from_secs(3);
                let tx = loop {
                    if let Some((id, version, tx)) = fake
                        .watches
                        .lock()
                        .map_err(|_| anyhow::anyhow!("lock"))?
                        .pop()
                    {
                        anyhow::ensure!(id == "1" && version.is_none(), "watch params");
                        break tx;
                    }
                    anyhow::ensure!(Instant::now() < deadline, "watch never opened");
                    thread::sleep(Duration::from_millis(2));
                };
                tx.send(Ok(Reply::Session(Box::new(session("1")))))?;
                // A quiet stream produces a heartbeat before the stream closes.
                thread::sleep(HEARTBEAT + Duration::from_millis(200));
                drop(tx);
                Ok(())
            })
        };
        let mut sent = Vec::new();
        federation.watch("box", "1", |reply| {
            sent.push(reply.clone());
            Ok(())
        })?;
        feeder
            .join()
            .map_err(|_| anyhow::anyhow!("feeder panicked"))??;
        let [Reply::Session(first), rest @ ..] = sent.as_slice() else {
            anyhow::bail!("baseline first: {sent:?}");
        };
        assert_eq!(first.id, "box:1");
        assert!(
            rest.iter().any(|r| matches!(r, Reply::Unchanged)),
            "{sent:?}"
        );
        assert!(matches!(rest.last(), Some(Reply::Error(e)) if e.contains("box")));
        Ok(())
    }

    #[test]
    fn saved_server_changes_retire_and_start_links() -> Result<()> {
        let fake = Fake::new(Vec::new());
        let (_dir, federation) = connected(vec![("box", fake)])?;
        assert!(federation.reload().is_empty(), "unchanged file");
        servers::forget(&federation.storage, "box")?;
        servers::save(&federation.storage, server("home", "id-home"))?;
        // Some filesystems keep whole-second modification times.
        let deadline = Instant::now() + Duration::from_secs(3);
        let started = loop {
            let started = federation.reload();
            if !started.is_empty() {
                break started;
            }
            anyhow::ensure!(Instant::now() < deadline, "reload never noticed the change");
            thread::sleep(Duration::from_millis(50));
            servers::save(&federation.storage, server("home", "id-home"))?;
        };
        assert_eq!(
            started
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["home"]
        );
        assert_eq!(federation.known(), BTreeSet::from(["home".to_owned()]));
        let error = format!(
            "{:#}",
            federation.remote("home").err().context("not connected")?
        );
        assert!(error.contains("not connected"), "{error}");
        Ok(())
    }
}
