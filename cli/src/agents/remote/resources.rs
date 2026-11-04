//! What the terminal shows around a session's chat, for the phone: presented
//! HTML artifacts, shells, sub-agent transcripts, pins and sidebar folders,
//! plus the session actions beyond messages (editing a queued message,
//! attaching a PR, moving to a saved server). Everything runs through the
//! agent service like the terminal's own requests, so the same rules apply.
use super::{
    super::{
        Control, Prompt, Reply, Request, Session, Status, client,
        folders::{self, Folder},
        provider::Provider,
        shells,
        subagents::{State, Subagent},
        ui::transcript,
    },
    api::{config, optional, param},
    view::{self, EntryView, SubagentView},
};
use crate::storage::Storage;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Shell output sent to the phone: the tail of the terminal's text.
const SHELL_OUTPUT: usize = 64 * 1024;
/// Text the phone may type into a shell at once.
const SHELL_INPUT: usize = 16 * 1024;
/// A sub-agent's latest messages sent at once.
const SUBAGENT_ENTRIES: usize = 300;

static CLIENTS: AtomicU64 = AtomicU64::new(0);

/// One phone connection typing into shells as a person at a terminal: its
/// input goes in order, and agent writes wait while it types, as with the
/// terminal's shell pane.
pub struct ShellInput {
    client: String,
    sequence: Mutex<u64>,
}

impl ShellInput {
    pub fn new(device: &str) -> Self {
        let device: String = device.chars().take(64).collect();
        Self {
            client: format!(
                "phone-{device}-{}-{}",
                chrono::Utc::now().timestamp_millis(),
                CLIENTS.fetch_add(1, Ordering::Relaxed)
            ),
            sequence: Mutex::new(0),
        }
    }

    /// Held until the service accepted it, so inputs never overtake.
    fn send(&self, storage: &Storage, id: String, shell: String, text: String) -> Result<()> {
        let mut sequence = self
            .sequence
            .lock()
            .map_err(|_| anyhow::anyhow!("Shell input lock failed"))?;
        *sequence += 1;
        request(
            storage,
            Request::Shell {
                id,
                action: shells::Action::Human {
                    shell,
                    client: self.client.clone(),
                    sequence: *sequence,
                    focused: true,
                    text,
                },
            },
        )?;
        Ok(())
    }
}

pub fn dispatch(
    storage: &Storage,
    input: &ShellInput,
    method: &str,
    params: &Value,
) -> Option<Result<Value>> {
    Some(match method {
        "session.artifact" => artifact(storage, params),
        "session.shells" => (|| {
            let id: String = param(params, "id")?;
            let shells: Vec<ShellView> = listed(storage, &id)?.iter().map(ShellView::new).collect();
            Ok(json!({"shells": shells}))
        })(),
        "session.shell" => read_shell(storage, params),
        "session.shell.open" => open_shell(storage, params),
        "session.shell.write" => write_shell(storage, input, params),
        "session.shell.close" => close_shell(storage, params),
        "session.subagent" => subagent(storage, params),
        "session.pin" => pin(storage, params),
        "session.editQueued" => edit_queued(storage, params),
        "session.attachPr" => (|| {
            let url: String = param(params, "url")?;
            let url = url.trim();
            ensure!(!url.is_empty(), "Paste the pull request's GitHub URL");
            request(
                storage,
                Request::AttachPr {
                    id: param(params, "id")?,
                    url: url.to_owned(),
                },
            )?;
            Ok(json!({}))
        })(),
        "session.move" => (|| {
            let server: String = param(params, "server")?;
            match request(
                storage,
                Request::Move {
                    id: param(params, "id")?,
                    server: server.trim().to_owned(),
                },
            )? {
                Reply::Launched(id) => Ok(json!({"id": id})),
                _ => anyhow::bail!("Unexpected move reply"),
            }
        })(),
        "agents.servers" => servers(storage),
        "folders.list" => Ok(json!({"folders": folders(storage)})),
        "folders.create" | "folders.rename" | "folders.delete" | "folders.move"
        | "folders.reorder" => arrange(storage, method, params),
        _ => return None,
    })
}

fn request(storage: &Storage, request: Request) -> Result<Reply> {
    client::request(storage, request)
}

fn session(storage: &Storage, id: &str) -> Result<Session> {
    match request(
        storage,
        Request::Read {
            id: id.to_owned(),
            version: None,
        },
    )? {
        Reply::Session(session) => Ok(*session),
        _ => anyhow::bail!("Unexpected session reply"),
    }
}

// MARK: Artifacts

/// A presented artifact's file, read by the service with the rules it uses
/// for another computer: registered artifacts only, inside their workspace,
/// HTML, at most `artifacts::DOWNLOAD_LIMIT`.
fn artifact(storage: &Storage, params: &Value) -> Result<Value> {
    let path: String = param(params, "path")?;
    let workspace = optional::<String>(params, "workspace")?.filter(|w| !w.is_empty());
    let Reply::ArtifactFile(data) = request(
        storage,
        Request::ReadArtifact {
            id: param(params, "id")?,
            path: PathBuf::from(&path),
            workspace: workspace.map(PathBuf::from),
        },
    )?
    else {
        anyhow::bail!("Unexpected artifact reply");
    };
    let padding = data.bytes().rev().take_while(|b| *b == b'=').count();
    let size = (data.len() / 4 * 3).saturating_sub(padding);
    let name = Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(path);
    Ok(json!({"name": name, "mime": "text/html", "size": size, "data": data}))
}

// MARK: Shells

/// A shell as the phone lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellView {
    pub id: String,
    pub title: String,
    /// `shell` (shared interactive shell), `command` (a `!` command) or
    /// `background` (Codex's background shell; read-only).
    pub kind: &'static str,
    pub running: bool,
    pub exit_code: Option<i64>,
    pub cwd: Option<String>,
    /// Changes with every output; `session.shell` skips unchanged output.
    pub revision: Option<u64>,
    /// Someone types in it at the terminal (or on a phone) right now.
    pub human_focused: bool,
    pub can_write: bool,
    pub can_close: bool,
}

impl ShellView {
    fn new(value: &Value) -> Self {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let local = value.get("local").and_then(Value::as_bool) == Some(true);
        let closed = value.get("closed").and_then(Value::as_bool) == Some(true);
        let command = text("command").or_else(|| {
            value.get("command").and_then(Value::as_array).map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        });
        Self {
            id: text("itemId")
                .or_else(|| text("processId"))
                .unwrap_or_default(),
            title: command.unwrap_or_else(|| "Background shell".into()),
            kind: match (local, value.get("shared").and_then(Value::as_bool)) {
                (true, Some(true)) => "shell",
                (true, _) => "command",
                (false, _) => "background",
            },
            running: !closed,
            exit_code: value.get("exitCode").and_then(Value::as_i64),
            cwd: text("cwd"),
            revision: value.get("revision").and_then(Value::as_u64),
            human_focused: value.get("humanFocused").and_then(Value::as_bool) == Some(true),
            can_write: local && !closed,
            can_close: local || text("processId").is_some(),
        }
    }

    /// A `!` command that finished: only its transcript entry remains.
    fn finished(entry: &super::super::Entry) -> Self {
        Self {
            id: entry.id.clone(),
            title: entry
                .data
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Shell")
                .to_owned(),
            kind: "command",
            running: false,
            exit_code: entry.data.get("exitCode").and_then(Value::as_i64),
            cwd: entry
                .data
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_owned),
            revision: None,
            human_focused: false,
            can_write: false,
            can_close: false,
        }
    }
}

/// The Shells control's list: shared shells, running `!` commands and
/// Codex's background shells.
fn listed(storage: &Storage, id: &str) -> Result<Vec<Value>> {
    match request(storage, Request::Shells { id: id.to_owned() })? {
        Reply::Shells(shells) => Ok(shells),
        _ => anyhow::bail!("Unexpected shells reply"),
    }
}

fn find<'a>(shells: &'a [Value], shell: &str) -> Option<&'a Value> {
    shells.iter().find(|value| {
        value.get("itemId").and_then(Value::as_str) == Some(shell)
            || value.get("processId").and_then(Value::as_str) == Some(shell)
    })
}

/// A shell's text (scrollback and screen, without colours) at the
/// terminal's own size: the phone never resizes a shell someone else sees.
/// With the `revision` the phone has, unchanged output isn't sent again.
fn read_shell(storage: &Storage, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let shell: String = param(params, "shell")?;
    let seen = optional::<u64>(params, "revision")?;
    let shells = listed(storage, &id)?;
    let found = find(&shells, &shell);
    if let Some(value) = found.filter(|v| v.get("local").and_then(Value::as_bool) == Some(true)) {
        let Reply::Shell(read) = request(
            storage,
            Request::Shell {
                id,
                action: shells::Action::Read {
                    shell: ShellView::new(value).id,
                    rows: None,
                    cols: None,
                    scrollback: 0,
                },
            },
        )?
        else {
            anyhow::bail!("Unexpected shell reply");
        };
        let view = ShellView::new(&read);
        if seen.is_some() && seen == view.revision {
            return Ok(json!({"shell": view, "unchanged": true}));
        }
        let output = read.get("output").and_then(Value::as_str).unwrap_or("");
        let (output, truncated) = view::clip(output, SHELL_OUTPUT, true);
        return Ok(json!({
            "shell": view,
            "output": output,
            "truncated": truncated,
            "error": read.get("error").cloned().unwrap_or(Value::Null),
        }));
    }
    // Codex's background shells and finished commands live in the transcript.
    let session = session(storage, &id)?;
    let id = found.map_or(shell.clone(), |value| ShellView::new(value).id);
    let entry = session
        .entries
        .iter()
        .find(|entry| entry.id == id || entry.id == shell)
        .context("This shell has closed")?;
    let text = entry.text.as_str();
    let output = text.split_once('\n').map_or(text, |(_, output)| output);
    let (output, truncated) = view::clip(output, SHELL_OUTPUT, true);
    let view = found.map_or_else(|| ShellView::finished(entry), ShellView::new);
    Ok(json!({"shell": view, "output": output, "truncated": truncated, "error": null}))
}

/// Opens a shared shell in the session's worktree, or runs `command` like a
/// `!` message (its output also goes to the chat).
fn open_shell(storage: &Storage, params: &Value) -> Result<Value> {
    let command = optional::<String>(params, "command")?
        .map(|command| command.trim().to_owned())
        .filter(|command| !command.is_empty());
    let action = match command {
        Some(command) => shells::Action::Run { command },
        None => shells::Action::Open,
    };
    let Reply::Shell(opened) = request(
        storage,
        Request::Shell {
            id: param(params, "id")?,
            action,
        },
    )?
    else {
        anyhow::bail!("Unexpected shell reply");
    };
    let shell = opened
        .get("shell")
        .and_then(Value::as_str)
        .context("The service did not name the new shell")?;
    Ok(json!({"id": shell}))
}

/// Types into a shell: a newline is Enter, as a terminal sends it, and
/// `enter` adds one. Control characters (Ctrl+C is `\u{3}`) go as they are.
fn write_shell(storage: &Storage, input: &ShellInput, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let shell: String = param(params, "shell")?;
    let text: String = param(params, "text")?;
    let mut typed = text.replace("\r\n", "\r").replace('\n', "\r");
    if optional::<bool>(params, "enter")?.unwrap_or(false) {
        typed.push('\r');
    }
    ensure!(!typed.is_empty(), "Type something to send");
    ensure!(
        typed.len() <= SHELL_INPUT,
        "Send at most {} KB to a shell at once",
        SHELL_INPUT / 1024
    );
    input.send(storage, id, shell, typed)?;
    Ok(json!({}))
}

/// Terminates a shell, as **x** does in the terminal's Shells list.
fn close_shell(storage: &Storage, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let shell: String = param(params, "shell")?;
    let shells = listed(storage, &id)?;
    let value = find(&shells, &shell).context("This shell already closed")?;
    if value.get("local").and_then(Value::as_bool) == Some(true) {
        request(
            storage,
            Request::Shell {
                id,
                action: shells::Action::Close {
                    shell: ShellView::new(value).id,
                },
            },
        )?;
    } else {
        let process = value
            .get("processId")
            .and_then(Value::as_str)
            .context("This shell can't be closed from here")?;
        request(
            storage,
            Request::Control {
                id,
                control: Control::CloseShell {
                    process: process.to_owned(),
                },
            },
        )?;
    }
    Ok(json!({}))
}

// MARK: Sub-agents

/// A sub-agent's transcript, read-only. Codex keeps a sub-agent's history in
/// its own thread: it is read when first opened and on `refresh`, as the
/// terminal's pane does; Claude Code's arrive with the session.
fn subagent(storage: &Storage, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let agent: String = param(params, "agent")?;
    let refresh = optional::<bool>(params, "refresh")?.unwrap_or(false);
    let find = |session: &Session| -> Result<Subagent> {
        session
            .subagents
            .iter()
            .find(|a| a.id == agent)
            .cloned()
            .context("Sub-agent is no longer available")
    };
    let mut parent = session(storage, &id)?;
    let mut child = find(&parent)?;
    let mut error = None;
    if child.provider == Provider::Codex && (refresh || child.entries.is_empty()) {
        match request(
            storage,
            Request::Subagent {
                id: id.clone(),
                agent: agent.clone(),
            },
        ) {
            Ok(_) => {
                parent = session(storage, &id)?;
                child = find(&parent)?;
            }
            Err(failed) => error = Some(format!("{failed:#}")),
        }
    }
    // A display session renders the child's rows like the terminal's pane.
    let mut display = Session::new(child.id.clone(), parent.job.clone());
    display.provider = child.provider;
    display.status = match child.state {
        State::Starting | State::Running => Status::Running,
        State::Failed => Status::Failed,
        State::Interrupted => Status::Interrupted,
        _ => Status::Completed,
    };
    display.entries = std::mem::take(&mut child.entries);
    let visible: Vec<_> = display
        .entries
        .iter()
        .filter(|entry| transcript::visible(&display, entry))
        .collect();
    let earlier = visible.len().saturating_sub(SUBAGENT_ENTRIES);
    let entries: Vec<EntryView> = visible
        .iter()
        .skip(earlier)
        .map(|entry| EntryView::new(&display, entry, false))
        .collect();
    child.entries = display.entries.clone();
    Ok(json!({
        "subagent": SubagentView::new(&child),
        "entries": entries,
        "earlier": earlier,
        "error": error,
    }))
}

// MARK: Pins and folders

/// Every session the service lists (also another server's, which the
/// terminal can file too), for checks and pruning.
fn listed_sessions(storage: &Storage) -> Result<Vec<super::super::Summary>> {
    match request(storage, Request::List)? {
        Reply::Sessions(list) => Ok(list),
        _ => anyhow::bail!("Unexpected session list reply"),
    }
}

/// Pins or unpins a session at the top of the list, as **p** does.
fn pin(storage: &Storage, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let pinned: bool = param(params, "pinned")?;
    ensure!(
        listed_sessions(storage)?.iter().any(|s| s.id == id),
        "Session not found"
    );
    storage.update_config(|config| {
        if pinned {
            config.pinned_sessions.insert(id.clone());
        } else {
            config.pinned_sessions.remove(&id);
        }
    })?;
    Ok(json!({}))
}

/// The sidebar folders, in order, with the ids they hold.
pub fn folders(storage: &Storage) -> Vec<Folder> {
    config(storage).session_folders
}

/// The terminal's folder actions, saved in its config like its own.
fn arrange(storage: &Storage, method: &str, params: &Value) -> Result<Value> {
    let list = listed_sessions(storage)?;
    let known: HashSet<String> = list.iter().map(|s| s.id.clone()).collect();
    let text = |key: &str| -> Result<String> {
        let value: String = param(params, key)?;
        Ok(value.trim().to_owned())
    };
    let ids = || -> Result<Vec<String>> {
        let mut ids: Vec<String> = match optional::<Vec<String>>(params, "ids")? {
            Some(ids) => ids,
            None => optional::<String>(params, "id")?.into_iter().collect(),
        };
        ids.iter_mut().for_each(|id| *id = id.trim().to_owned());
        ids.retain(|id| !id.is_empty());
        ids.dedup();
        ensure!(
            ids.len() <= folders::MAX_MOVE,
            "Move at most {} sessions at once",
            folders::MAX_MOVE
        );
        for id in &ids {
            let summary = list
                .iter()
                .find(|s| &s.id == id)
                .with_context(|| format!("Session {id} not found"))?;
            ensure!(
                summary.kind != "Orchestrator",
                "Orchestrator chats stay outside folders"
            );
        }
        Ok(ids)
    };
    let mut created = None;
    let saved = match method {
        "folders.create" => {
            let name = text("name")?;
            let ids = ids()?;
            storage.try_update_config(|config| {
                folders::prune(&mut config.session_folders, &known);
                let id = folders::create(&mut config.session_folders, &name)?;
                folders::file(&mut config.session_folders, &ids, Some(&id))?;
                created = Some(id);
                Ok(())
            })?
        }
        "folders.rename" => {
            let (folder, name) = (text("folder")?, text("name")?);
            storage.try_update_config(|config| {
                folders::prune(&mut config.session_folders, &known);
                folders::rename(&mut config.session_folders, &folder, &name)
            })?
        }
        "folders.delete" => {
            let folder = text("folder")?;
            storage.try_update_config(|config| {
                folders::prune(&mut config.session_folders, &known);
                folders::remove(&mut config.session_folders, &folder).map(drop)
            })?
        }
        "folders.move" => {
            let ids = ids()?;
            ensure!(!ids.is_empty(), "Choose the sessions to move");
            let folder = optional::<String>(params, "folder")?
                .map(|folder| folder.trim().to_owned())
                .filter(|folder| !folder.is_empty());
            storage.try_update_config(|config| {
                folders::prune(&mut config.session_folders, &known);
                folders::file(&mut config.session_folders, &ids, folder.as_deref())
            })?
        }
        _ => {
            let folder = text("folder")?;
            let position: usize = param(params, "position")?;
            storage.try_update_config(|config| {
                folders::prune(&mut config.session_folders, &known);
                folders::place(&mut config.session_folders, &folder, position)
            })?
        }
    };
    let mut result = json!({"folders": saved.session_folders});
    if let (Some(id), Some(object)) = (created, result.as_object_mut()) {
        object.insert("id".into(), json!(id));
    }
    Ok(result)
}

// MARK: Session actions

/// Edits a queued message, as the terminal's queue does: `expected` is the
/// text the phone showed, so a message that changed or was sent meanwhile
/// is never overwritten. Skills, attachments the text still names, and a
/// question answer's link stay.
fn edit_queued(storage: &Storage, params: &Value) -> Result<Value> {
    let id: String = param(params, "id")?;
    let index: usize = param(params, "index")?;
    let shown: String = param(params, "expected")?;
    let text: String = param(params, "text")?;
    ensure!(
        !text.trim().is_empty(),
        "Enter a message, or remove it from the queue"
    );
    let session = session(storage, &id)?;
    let expected = session
        .queue
        .get(index)
        .cloned()
        .context("That queued message was already sent")?;
    let (current, truncated) = view::clip(expected.text(), view::QUEUED_TEXT, false);
    ensure!(
        !truncated,
        "This message is too long to edit here; edit it in difu on the computer"
    );
    ensure!(
        current == shown,
        "That queued message changed or was already sent; your edit was not saved"
    );
    let replacement = if let Prompt::QuestionAnswer {
        question_request,
        question_id,
        ..
    } = &expected
    {
        Prompt::QuestionAnswer {
            text: text.clone(),
            question_request: question_request.clone(),
            question_id: question_id.clone(),
        }
    } else {
        Prompt::WithSkills {
            text: text.clone(),
            skills: expected.skills().to_vec(),
            attachments: expected
                .attachments()
                .iter()
                .filter(|a| text.contains(&a.token()))
                .cloned()
                .collect(),
        }
    };
    request(
        storage,
        Request::Control {
            id,
            control: Control::ReplaceQueued {
                index,
                expected,
                replacement: Some(replacement),
            },
        },
    )?;
    Ok(json!({}))
}

/// The saved cloud servers this computer can move sessions to; a server
/// (or a computer without saved servers) lists none.
fn servers(storage: &Storage) -> Result<Value> {
    let Reply::Servers(servers) = request(storage, Request::Servers)? else {
        anyhow::bail!("Unexpected servers reply");
    };
    let servers: Vec<Value> = servers
        .iter()
        .map(|server| {
            json!({
                "name": server.name,
                "connected": server.connected,
                "version": server.version,
                "problem": server.problem,
            })
        })
        .collect();
    Ok(json!({"servers": servers}))
}

#[cfg(test)]
mod tests {
    use super::super::super::{Entry, Job, Launch, server::Served};
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::time::{Duration, Instant};

    fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
        value.pointer(pointer).unwrap_or(&Value::Null)
    }

    fn text(value: &Value, pointer: &str) -> String {
        at(value, pointer).as_str().unwrap_or_default().to_owned()
    }

    /// An idle coding session working in `workspace`.
    fn coding(id: &str, workspace: &Path) -> Session {
        let mut session = Session::new(
            id.into(),
            Job::Coding(Launch {
                repository: workspace.into(),
                isolated: true,
                base: "origin/main".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.workspace = Some(workspace.into());
        session.workspace_ready = true;
        session.thread_id = Some("thread".into());
        session
    }

    struct Fixture {
        served: Served,
        storage: Storage,
        input: ShellInput,
        workspace: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(sessions: impl FnOnce(&Path) -> Vec<Session>) -> Result<Self> {
            let dir = tempfile::tempdir()?;
            let workspace = dir.path().canonicalize()?.join("worktree");
            std::fs::create_dir_all(workspace.join("out"))?;
            let served = Served::start(dir.path().join("home"), sessions(&workspace))?;
            let storage = served.store.storage.clone();
            Ok(Self {
                served,
                storage,
                input: ShellInput::new("phone"),
                workspace,
                _dir: dir,
            })
        }

        fn call(&self, method: &str, params: Value) -> Result<Value> {
            dispatch(&self.storage, &self.input, method, &params)
                .with_context(|| format!("{method} is a phone method"))?
        }

        fn refused(&self, method: &str, params: Value) -> Result<String> {
            match self.call(method, params) {
                Ok(result) => anyhow::bail!("{method} should fail, got {result}"),
                Err(error) => Ok(format!("{error:#}")),
            }
        }
    }

    #[test]
    fn other_methods_are_left_to_the_rest_of_the_api() {
        let storage = Storage {
            config: "/nonexistent/config.json".into(),
            cache: "/nonexistent/cache".into(),
        };
        let input = ShellInput::new("phone");
        assert!(dispatch(&storage, &input, "session.send", &json!({})).is_none());
        assert!(dispatch(&storage, &input, "server.info", &json!({})).is_none());
    }

    #[test]
    fn presented_artifacts_open_with_the_service_limits() -> Result<()> {
        let fixture = Fixture::new(|workspace| {
            let mut session = coding("one", workspace);
            for (path, title) in [("out/report.html", "Report"), ("out/huge.html", "Huge")] {
                session
                    .artifacts
                    .push(super::super::super::artifacts::Artifact {
                        path: path.into(),
                        title: title.into(),
                        workspace: Some(workspace.into()),
                    });
            }
            vec![session]
        })?;
        std::fs::write(fixture.workspace.join("out/report.html"), "<h1>Report</h1>")?;
        std::fs::write(fixture.workspace.join("out/secret.html"), "private")?;
        // Sparse: the size check reads metadata, not the contents.
        std::fs::File::create(fixture.workspace.join("out/huge.html"))?
            .set_len(super::super::super::artifacts::DOWNLOAD_LIMIT + 1)?;
        let workspace = fixture.workspace.display().to_string();
        let opened = fixture.call(
            "session.artifact",
            json!({"id": "one", "path": "out/report.html", "workspace": workspace}),
        )?;
        assert_eq!(text(&opened, "/name"), "report.html");
        assert_eq!(text(&opened, "/mime"), "text/html");
        assert_eq!(at(&opened, "/size"), &json!(15));
        assert_eq!(STANDARD.decode(text(&opened, "/data"))?, b"<h1>Report</h1>");
        let unlisted = fixture.refused(
            "session.artifact",
            json!({"id": "one", "path": "out/secret.html", "workspace": workspace}),
        )?;
        assert!(unlisted.contains("no longer presented"), "{unlisted}");
        let huge = fixture.refused(
            "session.artifact",
            json!({"id": "one", "path": "out/huge.html", "workspace": workspace}),
        )?;
        assert!(huge.contains("up to 32 MB"), "{huge}");
        let missing = fixture.refused("session.artifact", json!({"id": "one"}))?;
        assert!(missing.contains("`path`"), "{missing}");
        Ok(())
    }

    /// Reads the shell until its output contains `needle`.
    fn wait_for(fixture: &Fixture, shell: &str, needle: &str) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let read = fixture.call("session.shell", json!({"id": "one", "shell": shell}))?;
            if text(&read, "/output").contains(needle) {
                return Ok(read);
            }
            anyhow::ensure!(Instant::now() < deadline, "no {needle:?} in {read}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn phones_open_type_into_read_and_close_shells() -> Result<()> {
        let fixture = Fixture::new(|workspace| vec![coding("one", workspace)])?;
        let opened = fixture.call("session.shell.open", json!({"id": "one"}))?;
        let shell = text(&opened, "/id");
        assert!(shell.starts_with("difu-shell-"), "{opened}");
        let listed = fixture.call("session.shells", json!({"id": "one"}))?;
        let row = at(&listed, "/shells/0");
        assert_eq!(text(row, "/id"), shell);
        assert_eq!(text(row, "/kind"), "shell");
        assert_eq!(at(row, "/running"), &json!(true));
        assert_eq!(at(row, "/canWrite"), &json!(true));
        assert_eq!(at(row, "/canClose"), &json!(true));
        assert_eq!(text(row, "/cwd"), fixture.workspace.display().to_string());

        fixture.call(
            "session.shell.write",
            json!({"id": "one", "shell": shell, "text": "printf 'phone-%s\\n' typed", "enter": true}),
        )?;
        let read = wait_for(&fixture, &shell, "phone-typed")?;
        // While the phone types, agents wait, as with the terminal's pane.
        fixture.call(
            "session.shell.write",
            json!({"id": "one", "shell": shell, "text": "true\n"}),
        )?;
        let listed = fixture.call("session.shells", json!({"id": "one"}))?;
        assert_eq!(at(&listed, "/shells/0/humanFocused"), &json!(true));
        let blocked = request(
            &fixture.storage,
            Request::Shell {
                id: "one".into(),
                action: shells::Action::Write {
                    shell: shell.clone(),
                    text: "echo agent".into(),
                    enter: true,
                },
            },
        );
        assert!(blocked.is_err(), "agent input waits for the human");
        // Output that didn't change isn't sent again.
        let revision = at(&read, "/shell/revision").clone();
        let again = fixture.call(
            "session.shell",
            json!({"id": "one", "shell": shell, "revision": revision}),
        )?;
        if at(&again, "/shell/revision") == &revision {
            assert_eq!(at(&again, "/unchanged"), &json!(true));
            assert_eq!(at(&again, "/output"), &Value::Null);
        }

        let empty = fixture.refused(
            "session.shell.write",
            json!({"id": "one", "shell": shell, "text": ""}),
        )?;
        assert!(empty.contains("Type something"), "{empty}");
        let flood = fixture.refused(
            "session.shell.write",
            json!({"id": "one", "shell": shell, "text": "x".repeat(SHELL_INPUT + 1)}),
        )?;
        assert!(flood.contains("at most 16 KB"), "{flood}");

        fixture.call("session.shell.close", json!({"id": "one", "shell": shell}))?;
        let listed = fixture.call("session.shells", json!({"id": "one"}))?;
        assert_eq!(at(&listed, "/shells"), &json!([]));
        let gone = fixture.refused("session.shell", json!({"id": "one", "shell": shell}))?;
        assert!(gone.contains("This shell has closed"), "{gone}");
        let closed =
            fixture.refused("session.shell.close", json!({"id": "one", "shell": shell}))?;
        assert!(closed.contains("already closed"), "{closed}");
        Ok(())
    }

    #[test]
    fn commands_run_like_bang_messages_and_stay_readable_after_they_finish() -> Result<()> {
        let fixture = Fixture::new(|workspace| vec![coding("one", workspace)])?;
        let opened = fixture.call(
            "session.shell.open",
            json!({"id": "one", "command": "printf 'ran-%s\\n' bang"}),
        )?;
        let shell = text(&opened, "/id");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            fixture.served.store.shells.tick(&fixture.served.store)?;
            let listed = fixture.call("session.shells", json!({"id": "one"}))?;
            if at(&listed, "/shells") == &json!([]) {
                break;
            }
            assert_eq!(text(&listed, "/shells/0/kind"), "command");
            anyhow::ensure!(Instant::now() < deadline, "the command never finished");
            std::thread::sleep(Duration::from_millis(50));
        }
        let read = fixture.call("session.shell", json!({"id": "one", "shell": shell}))?;
        assert!(text(&read, "/output").contains("ran-bang"), "{read}");
        assert_eq!(text(&read, "/shell/kind"), "command");
        assert_eq!(at(&read, "/shell/running"), &json!(false));
        assert_eq!(at(&read, "/shell/canWrite"), &json!(false));
        assert_eq!(at(&read, "/shell/exitCode"), &json!(0));
        Ok(())
    }

    #[test]
    fn shells_need_a_ready_coding_session() -> Result<()> {
        let fixture = Fixture::new(|workspace| {
            let mut archived = coding("archived", workspace);
            archived.archived = true;
            vec![archived]
        })?;
        let refused = fixture.refused("session.shell.open", json!({"id": "archived"}))?;
        assert!(refused.contains("available worktree"), "{refused}");
        Ok(())
    }

    fn subagent(state: State, entries: Vec<Entry>) -> Subagent {
        let mut agent = Subagent::new(Provider::Claude, "native");
        agent.name = "Explore".into();
        agent.task = "Find the login handler\nand its tests".into();
        agent.state = state;
        agent.entries = entries;
        agent
    }

    #[test]
    fn sub_agent_transcripts_are_read_only_views() -> Result<()> {
        let fixture = Fixture::new(|workspace| {
            let mut session = coding("one", workspace);
            session.subagents.push(subagent(
                State::Running,
                vec![
                    Entry {
                        id: "a1".into(),
                        kind: "agentMessage".into(),
                        text: "Found it in **auth.rs**".to_owned().into(),
                        ..Entry::default()
                    },
                    Entry {
                        id: "t1".into(),
                        kind: "commandExecution".into(),
                        text: "$ rg login\nauth.rs".to_owned().into(),
                        data: json!({"command": "rg login", "status": "completed"}).into(),
                        started_at: Some(1),
                        finished_at: Some(2),
                        ..Entry::default()
                    },
                ],
            ));
            vec![session]
        })?;
        let agent = "claude:native";
        let read = fixture.call("session.subagent", json!({"id": "one", "agent": agent}))?;
        assert_eq!(text(&read, "/subagent/id"), agent);
        assert_eq!(
            text(&read, "/subagent/title"),
            "Explore · Find the login handler"
        );
        assert_eq!(text(&read, "/subagent/state"), "Running");
        assert_eq!(text(&read, "/subagent/stateLabel"), "Running");
        assert_eq!(at(&read, "/subagent/active"), &json!(true));
        assert_eq!(text(&read, "/subagent/provider"), "claude");
        assert_eq!(text(&read, "/subagent/providerLabel"), "Claude Code");
        assert_eq!(at(&read, "/subagent/entries"), &json!(2));
        assert_eq!(at(&read, "/earlier"), &json!(0));
        assert_eq!(at(&read, "/error"), &Value::Null);
        assert_eq!(text(&read, "/entries/0/role"), "assistant");
        assert_eq!(text(&read, "/entries/1/role"), "tool");
        assert_eq!(text(&read, "/entries/1/detail"), "rg login");
        let gone = fixture.refused("session.subagent", json!({"id": "one", "agent": "nope"}))?;
        assert!(gone.contains("no longer available"), "{gone}");
        Ok(())
    }

    #[test]
    fn long_sub_agent_transcripts_send_their_latest_messages() -> Result<()> {
        let entries = (0..SUBAGENT_ENTRIES + 5)
            .map(|n| Entry {
                id: format!("m{n}"),
                kind: "agentMessage".into(),
                text: format!("message {n}").into(),
                ..Entry::default()
            })
            .collect();
        let fixture = Fixture::new(|workspace| {
            let mut session = coding("one", workspace);
            session.subagents.push(subagent(State::Completed, entries));
            vec![session]
        })?;
        let read = fixture.call(
            "session.subagent",
            json!({"id": "one", "agent": "claude:native"}),
        )?;
        assert_eq!(at(&read, "/earlier"), &json!(5));
        assert_eq!(
            at(&read, "/entries").as_array().map(Vec::len),
            Some(SUBAGENT_ENTRIES)
        );
        assert_eq!(text(&read, "/entries/0/text"), "message 5");
        assert_eq!(at(&read, "/subagent/active"), &json!(false));
        Ok(())
    }

    #[test]
    fn pins_and_folders_arrange_the_list_like_the_terminal() -> Result<()> {
        let fixture = Fixture::new(|workspace| {
            let mut orchestrator = coding(super::super::super::orchestrator::ID, workspace);
            orchestrator.orchestrator = true;
            vec![
                coding("one", workspace),
                coding("two", workspace),
                orchestrator,
            ]
        })?;
        fixture.call("session.pin", json!({"id": "one", "pinned": true}))?;
        assert!(
            fixture
                .storage
                .load_config()?
                .pinned_sessions
                .contains("one")
        );
        fixture.call("session.pin", json!({"id": "one", "pinned": false}))?;
        assert!(fixture.storage.load_config()?.pinned_sessions.is_empty());
        let unknown = fixture.refused("session.pin", json!({"id": "ghost", "pinned": true}))?;
        assert!(unknown.contains("Session not found"), "{unknown}");

        let created = fixture.call(
            "folders.create",
            json!({"name": "  Auth   work ", "ids": ["one"]}),
        )?;
        assert_eq!(text(&created, "/id"), "auth-work");
        assert_eq!(
            at(&created, "/folders"),
            &json!([{"id": "auth-work", "name": "Auth work", "sessions": ["one"]}])
        );
        fixture.call("folders.create", json!({"name": "Later"}))?;
        let moved = fixture.call("folders.move", json!({"ids": ["two"], "folder": "later"}))?;
        assert_eq!(at(&moved, "/folders/1/sessions"), &json!(["two"]));
        let back = fixture.call("folders.move", json!({"id": "one", "folder": null}))?;
        assert_eq!(at(&back, "/folders/0/sessions"), &json!([]));
        fixture.call(
            "folders.rename",
            json!({"folder": "auth-work", "name": "Auth"}),
        )?;
        let reordered =
            fixture.call("folders.reorder", json!({"folder": "Later", "position": 0}))?;
        assert_eq!(text(&reordered, "/folders/0/id"), "later");
        assert_eq!(text(&reordered, "/folders/1/name"), "Auth");
        assert_eq!(
            fixture.call("folders.list", json!({}))?,
            json!({"folders": at(&reordered, "/folders")})
        );

        let orchestrator = fixture.refused(
            "folders.move",
            json!({"ids": [super::super::super::orchestrator::ID], "folder": "Auth"}),
        )?;
        assert!(
            orchestrator.contains("stay outside folders"),
            "{orchestrator}"
        );
        let ghost = fixture.refused("folders.move", json!({"ids": ["ghost"], "folder": "Auth"}))?;
        assert!(ghost.contains("Session ghost not found"), "{ghost}");
        let taken = fixture.refused("folders.create", json!({"name": "auth"}))?;
        assert!(taken.contains("already exists"), "{taken}");
        let nowhere = fixture.refused("folders.move", json!({"ids": ["one"], "folder": "Nope"}))?;
        assert!(nowhere.contains("No folder named"), "{nowhere}");
        let none = fixture.refused("folders.move", json!({"ids": []}))?;
        assert!(none.contains("Choose the sessions"), "{none}");
        let many: Vec<String> = (0..=folders::MAX_MOVE).map(|n| n.to_string()).collect();
        let flood = fixture.refused("folders.move", json!({"ids": many, "folder": "Auth"}))?;
        assert!(flood.contains("at most 100"), "{flood}");

        let deleted = fixture.call("folders.delete", json!({"folder": "later"}))?;
        assert_eq!(
            at(&deleted, "/folders"),
            &json!([{"id": "auth-work", "name": "Auth", "sessions": []}])
        );
        Ok(())
    }

    #[test]
    fn queued_messages_are_edited_only_as_the_phone_saw_them() -> Result<()> {
        let fixture = Fixture::new(|workspace| {
            let mut session = coding("one", workspace);
            // Preparing: the queue waits for setup and edits stay in the store.
            session.status = Status::Starting;
            session.thread_id = None;
            session.queue = vec![
                Prompt::Text("Add tests".into()),
                Prompt::Text("x".repeat(view::QUEUED_TEXT + 1)),
            ];
            vec![session]
        })?;
        fixture.call(
            "session.editQueued",
            json!({"id": "one", "index": 0, "expected": "Add tests", "text": "Add more tests"}),
        )?;
        let queue = fixture.served.store.get("one")?.queue;
        assert_eq!(queue.first().map(Prompt::text), Some("Add more tests"));
        let stale = fixture.refused(
            "session.editQueued",
            json!({"id": "one", "index": 0, "expected": "Add tests", "text": "Again"}),
        )?;
        assert!(stale.contains("changed or was already sent"), "{stale}");
        let long = fixture.refused(
            "session.editQueued",
            json!({"id": "one", "index": 1, "expected": "x".repeat(view::QUEUED_TEXT), "text": "short"}),
        )?;
        assert!(long.contains("too long to edit here"), "{long}");
        let blank = fixture.refused(
            "session.editQueued",
            json!({"id": "one", "index": 0, "expected": "Add more tests", "text": "  "}),
        )?;
        assert!(blank.contains("Enter a message"), "{blank}");
        let sent = fixture.refused(
            "session.editQueued",
            json!({"id": "one", "index": 5, "expected": "", "text": "late"}),
        )?;
        assert!(sent.contains("already sent"), "{sent}");
        Ok(())
    }

    #[test]
    fn moves_and_servers_need_saved_servers() -> Result<()> {
        let fixture = Fixture::new(|workspace| vec![coding("one", workspace)])?;
        assert_eq!(
            fixture.call("agents.servers", json!({}))?,
            json!({"servers": []})
        );
        let refused = fixture.refused("session.move", json!({"id": "one", "server": "box"}))?;
        assert!(refused.contains("cannot reach saved servers"), "{refused}");
        let blank = fixture.refused("session.attachPr", json!({"id": "one", "url": " "}))?;
        assert!(blank.contains("Paste the pull request"), "{blank}");
        let bad = fixture.refused(
            "session.attachPr",
            json!({"id": "one", "url": "https://example.com/x"}),
        )?;
        assert!(bad.contains("full GitHub pull request URL"), "{bad}");
        Ok(())
    }
}
