//! JSON-RPC for one phone connection. Every request runs on its own thread and
//! replies through the connection's queue; subscriptions push events the same way.
use super::{
    super::{Control, Reply, Request, Session, client, media, server, transport},
    reviews,
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
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    thread,
    time::{Duration, Instant},
};

const MODELS_TTL: Duration = Duration::from_secs(10 * 60);

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
    storage: Storage,
    port: u16,
    sender: SyncSender<String>,
    closed: AtomicBool,
    watches: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    listing: AtomicBool,
    models: Mutex<Option<(Instant, Value)>>,
}

#[derive(Deserialize)]
struct Call {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

impl Connection {
    pub fn new(storage: Storage, port: u16, sender: SyncSender<String>) -> Self {
        Self(Arc::new(Inner {
            storage,
            port,
            sender,
            closed: AtomicBool::new(false),
            watches: Mutex::new(BTreeMap::new()),
            listing: AtomicBool::new(false),
            models: Mutex::new(None),
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
        let inner = self.0.clone();
        let _ = thread::Builder::new()
            .name("difu-remote-call".into())
            .spawn(move || {
                let reply = match dispatch(&inner, &call.method, &call.params) {
                    Ok(result) => json!({"id": call.id, "result": result}),
                    Err(error) => json!({"id": call.id, "error": format!("{error:#}")}),
                };
                inner.push(&reply);
            });
    }

    pub fn close(&self) {
        self.0.closed.store(true, Ordering::Relaxed);
        if let Ok(watches) = self.0.watches.lock() {
            for stop in watches.values() {
                stop.store(true, Ordering::Relaxed);
            }
        }
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
    fn summaries(&self) -> Result<Vec<SessionSummary>> {
        let Reply::Sessions(list) = self.request(Request::List)? else {
            anyhow::bail!("Unexpected session list reply");
        };
        let config = config(&self.storage);
        Ok(view::summaries(
            &list,
            &config.pinned_sessions,
            &config.session_order,
        ))
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
            "name": super::network::name(),
            "version": env!("CARGO_PKG_VERSION"),
            "hosts": super::network::hosts(),
            "port": inner.port,
        })),
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
                    skills: Vec::new(),
                    attachments,
                },
            )
        }
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
        "session.answer" => inner.control(
            id()?,
            Control::AnswerQuestion {
                request: param(params, "request")?,
                question: param(params, "question")?,
                answer: optional(params, "answer")?,
            },
        ),
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
        "pr.comment" | "pr.review" | "pr.merge" | "pr.close" | "pr.ready" => {
            reviews::act(method, params)
        }
        _ => anyhow::bail!("Unknown method `{method}`; update difu on your computer"),
    }
}

#[derive(Deserialize)]
struct Upload {
    data: String,
}

fn new_session(inner: &Inner, params: &Value) -> Result<Value> {
    let saved = config(&inner.storage).agent_defaults;
    let repository: Option<PathBuf> = optional(params, "repository")?;
    let defaults = AgentDefaults {
        provider: saved.provider,
        starting_mode: saved.starting_mode,
        repository: repository.or(saved.repository),
        model: optional::<String>(params, "model")?
            .filter(|m| !m.is_empty())
            .or(saved.model),
        effort: optional::<String>(params, "effort")?
            .filter(|e| !e.is_empty())
            .or(saved.effort),
        isolated: saved.isolated,
    };
    ensure!(
        defaults.repository.is_some(),
        "Choose a repository for the new session"
    );
    let id = match inner.request(Request::NewAgent {
        defaults,
        cwd: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
        remember_repository: false,
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

/// Repositories difu already knows: the default, review clones, local diff
/// repositories, and repositories of existing sessions.
fn repositories(inner: &Inner) -> Result<Vec<Value>> {
    let config = config(&inner.storage);
    let default = config.agent_defaults.repository.clone();
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
            if pinned_at.elapsed() >= Duration::from_secs(5) {
                pinned = config(&inner.storage).pinned_sessions;
                pinned_at = Instant::now();
            }
            let Some(session) = &session else { continue };
            let next = SessionView::cached(session, &pinned, &mut cache);
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
    fn requests_report_bad_parameters_by_name() {
        let error = param::<String>(&json!({}), "id");
        assert!(error.is_err_and(|e| e.to_string().contains("`id`")));
        assert_eq!(
            optional::<bool>(&json!({"queue": null}), "queue").ok(),
            Some(None)
        );
    }
}
