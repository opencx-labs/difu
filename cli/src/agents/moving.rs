//! Moving a coding session from this computer to a saved cloud server. The local
//! service exports the session (chat, settings, repository, branch, commits and
//! uncommitted files as a git bundle) and sends it with `Request::Import` over the
//! server's `rpc`; the server continues it as a new session in a normal difu
//! worktree. Design and wire format: docs/cloud-agents.md.
use super::{Entry, Job, Launch, Session, Status, permissions, provider, server::Store};
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) const TOOL: &str = "difu_move_to_server";
pub(super) const INSTRUCTIONS: &str = "Use difu_move_to_server, if available, only when the user explicitly asks to move this session to one of their difu cloud servers. The move happens after your turn ends: the server continues this conversation as a new session with this branch, its commits and the uncommitted files, and this local session is archived. Call it alone with the server's name, reply in one short sentence, and stop. Never move a session on your own initiative.";

/// A move travels in one WebSocket message; servers accept at most this.
pub const LIMIT: usize = 48 << 20;
/// Room for the `rpc` envelope around the package.
const ENVELOPE: usize = 64 << 10;
/// Servers start sessions from origin/main, so the commits it has are not sent.
const MAIN: &str = "refs/remotes/origin/main";
/// `git bundle` records refs only, so the tip gets a temporary one.
const BUNDLE_REF: &str = "refs/difu/move";
/// The temporary commit holding uncommitted files is never kept anywhere.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "difu"),
    ("GIT_AUTHOR_EMAIL", "difu@localhost"),
    ("GIT_COMMITTER_NAME", "difu"),
    ("GIT_COMMITTER_EMAIL", "difu@localhost"),
];

pub(super) fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Move this session to one of the user's saved difu cloud servers. Use only when the user explicitly asks to move this session to a server. difu moves it after the current turn ends: the server continues this conversation, branch, commits and uncommitted files as a new session, and this session is archived and stops taking messages. Only this session can be moved. Call alone, then stop.","inputSchema":{"type":"object","properties":{"server":{"type":"string","description":"The saved server's name, as in `difu server list`."}},"required":["server"],"additionalProperties":false}})
}

/// Everything a server needs to continue a session (`Request::Import`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    /// The session on the computer it moved from, and that computer's name.
    pub source: String,
    pub computer: String,
    pub title: String,
    #[serde(default)]
    pub title_manual: bool,
    pub provider: provider::Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Permission mode (`plan`, `acceptEdits`, ...), restored as the starting mode.
    pub mode: Option<String>,
    /// Codex permission preset.
    pub preset: Option<permissions::CodexPreset>,
    /// The session's first prompt.
    pub prompt: String,
    /// The chat, restored as is and handed to the server's first provider turn.
    pub entries: Vec<Entry>,
    /// `owner/name` from the `origin` remote.
    pub repository: String,
    /// The session branch; empty for a detached worktree.
    pub branch: String,
    /// The branch tracked `origin/<branch>`, as after `git push -u`.
    #[serde(default)]
    pub upstream: bool,
    /// The worktree's HEAD.
    pub head: String,
    /// A commit on top of `head` whose tree is the worktree with its
    /// uncommitted and untracked (not ignored) files; `None` when clean.
    pub wip: Option<String>,
    /// Base64 `git bundle` of the commits not on origin/main up to `wip` (or
    /// `head`); `None` when there are none.
    pub bundle: Option<String>,
    /// Commits on the branch that are not on origin/main.
    #[serde(default)]
    pub commits: usize,
}

/// Why `session` cannot move now.
pub(super) fn check(session: &Session) -> Result<()> {
    ensure!(
        !session.orchestrator && matches!(session.job, Job::Coding(_)),
        "Only coding sessions can move to a server"
    );
    if let Some(moved) = &session.moved_to {
        bail!("This session already moved to {moved}");
    }
    ensure!(
        !session.server_mode,
        "This session already runs on a server"
    );
    ensure!(!session.archived, "Unarchive the session before moving it");
    ensure!(
        !session.workspace_removed,
        "This session's worktree was removed, so there is no work to move"
    );
    ensure!(
        session.deletion_progress.is_none() && !session.clearing,
        "This session is being deleted or cleared"
    );
    ensure!(
        !session.status.active() && session.turn_id.is_none() && !session.switching_workspace,
        "Wait for the current turn to finish before moving this session"
    );
    ensure!(
        session.pending.is_empty(),
        "Answer or dismiss this session's pending questions and approvals before moving it"
    );
    ensure!(
        session.queue.is_empty(),
        "Send or remove this session's queued messages before moving it"
    );
    ensure!(
        session.workspace_ready && session.workspace.is_some(),
        "This session's worktree is still being prepared"
    );
    Ok(())
}

/// The local side of a move. Reads the worktree without changing it: the
/// uncommitted files are captured with a temporary index.
pub(super) fn export(session: &Session, cancel: &Cancel) -> Result<Package> {
    check(session)?;
    let Job::Coding(launch) = &session.job else {
        bail!("Only coding sessions can move to a server");
    };
    let workspace = session
        .workspace
        .as_deref()
        .context("This session has no worktree")?;
    ensure!(
        workspace.is_dir(),
        "This session's worktree no longer exists: {}",
        workspace.display()
    );
    let origin = read(workspace, &["remote", "get-url", "origin"], cancel)
        .context("Moving needs the repository's origin remote on GitHub")?;
    let repository = repo::remote_repository(&origin).with_context(|| {
        format!(
            "origin is {origin}, not a GitHub repository; servers clone repositories from GitHub"
        )
    })?;
    crate::cloud::repos::validate(&repository)?;
    ensure!(
        succeeds(
            workspace,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{MAIN}^{{commit}}")
            ],
            cancel
        )?,
        "This repository has no origin/main; servers start sessions from origin/main"
    );
    let head = read(
        workspace,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        cancel,
    )?;
    let branch = read(workspace, &["branch", "--show-current"], cancel)?;
    let upstream = !branch.is_empty()
        && read(
            workspace,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
            cancel,
        )
        .is_ok_and(|upstream| upstream == format!("origin/{branch}"));
    let wip = snapshot(workspace, &session.id, cancel)?;
    let tip = wip.as_deref().unwrap_or(&head);
    let commits = count(workspace, &head, cancel)?;
    let bundle = if count(workspace, tip, cancel)? > 0 {
        Some(STANDARD.encode(bundle(workspace, &session.id, tip, cancel)?))
    } else {
        None
    };
    let state = &session.permission_state;
    Ok(Package {
        source: session.id.clone(),
        computer: crate::cloud::servers::computer_name(),
        title: session.title.clone(),
        title_manual: session.title_manual,
        provider: session.provider,
        model: session.model.clone().or_else(|| launch.model.clone()),
        effort: session.effort.clone().or_else(|| launch.effort.clone()),
        mode: state.pending_mode.clone().or_else(|| state.mode.clone()),
        preset: state
            .pending_preset
            .or(state.preset)
            .or_else(|| permissions::detected_preset(&session.permissions)),
        prompt: launch.prompt.clone(),
        entries: session
            .entries
            .iter()
            .filter(|entry| !transient(entry))
            .cloned()
            .collect(),
        repository,
        branch,
        upstream,
        head,
        wip,
        bundle,
        commits,
    })
}

/// Refuses a package that cannot travel in one WebSocket message of `limit`.
pub(super) fn fits(package: &Package, limit: usize) -> Result<()> {
    let size = serde_json::to_vec(package)?.len().saturating_add(ENVELOPE);
    if size <= limit {
        return Ok(());
    }
    let work = package.bundle.as_ref().map_or(0, String::len);
    let chat = serde_json::to_vec(&package.entries)?.len();
    bail!(
        "This session is too large to move: it needs {} in one message and a server accepts at most {} (commits and files {}, chat {}). Remove large untracked files from the worktree or commit less binary data, then try again.",
        megabytes(size),
        megabytes(limit),
        megabytes(work),
        megabytes(chat)
    )
}

/// The server side of a move: a new session `id` in a normal difu worktree of
/// `root` (the server's base clone) with the moved work, chat and settings. A
/// failed import removes the worktree and branch it created.
pub(super) fn import(
    package: &Package,
    id: String,
    root: PathBuf,
    store: &Store,
    cancel: &Cancel,
) -> Result<Session> {
    validate(package)?;
    let mut session = Session::new(
        id,
        Job::Coding(Launch {
            repository: root,
            isolated: true,
            base: "origin/main".into(),
            prompt: package.prompt.clone(),
            model: package.model.clone(),
            effort: package.effort.clone(),
        }),
    );
    session.provider = package.provider;
    if let Some(mode) = &package.mode
        && permissions::mode_choices(package.provider)
            .iter()
            .any(|choice| &choice.id == mode)
    {
        session.permission_state.pending_mode = Some(mode.clone());
    }
    if package.provider == provider::Provider::Codex {
        session.permission_state.pending_preset = package.preset;
    }
    session.title = package.title.clone();
    session.title_manual = package.title_manual;
    session.title_ready = true;
    session.title_attempted = true;
    super::rules::capture(&store.storage, &mut session)?;
    session.status = Status::Idle;
    let restored = restore(package, &mut session, &store.home, cancel);
    let branch = match restored {
        Ok(branch) => branch,
        Err(error) => {
            if session.workspace.is_some()
                && let Err(cleanup) = super::workspace::discard(&session, &store.home, cancel)
            {
                return Err(error.context(format!(
                    "The partial worktree could not be removed: {cleanup:#}"
                )));
            }
            return Err(error);
        }
    };
    session.entries = package
        .entries
        .iter()
        .filter(|entry| !transient(entry))
        .cloned()
        .collect();
    session.note("system", note(package, &branch));
    session.provider_context = provider::handoff(&session.entries, 0);
    Ok(session)
}

/// Lets `difu_move_to_server` schedule a move of its own session; the service
/// runs it once the turn ends (`Service::scheduled_moves`).
pub(super) fn schedule(store: &Store, id: &str, args: &Value) -> Result<Value> {
    ensure!(
        args.as_object().is_some_and(|args| args.len() == 1),
        "difu_move_to_server takes only server and can move only this session"
    );
    let server = args
        .get("server")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .context("Missing server: the saved server's name")?
        .to_owned();
    let session = store.get(id)?;
    ensure!(
        !session.orchestrator && matches!(session.job, Job::Coding(_)),
        "Only coding sessions can move to a server"
    );
    ensure!(
        !session.server_mode,
        "This session already runs on a server"
    );
    if let Some(moved) = &session.moved_to {
        bail!("This session already moved to {moved}");
    }
    let saved = crate::cloud::servers::load(&store.storage)?;
    if !saved.iter().any(|saved| saved.name == server) {
        let names = saved
            .iter()
            .map(|saved| saved.name.as_str())
            .collect::<Vec<_>>();
        bail!(
            "No saved server named {server}. {}",
            if names.is_empty() {
                "This computer has no saved servers; the user can add one with `difu server add`."
                    .to_owned()
            } else {
                format!("Saved servers: {}.", names.join(", "))
            }
        );
    }
    store.update(id, |s| {
        s.move_scheduled = Some(server.clone());
        s.note(
            "system",
            format!("Moving this session to {server} when the current turn ends"),
        );
    })?;
    store.save(id)?;
    Ok(
        json!({"scheduled":true,"server":server,"next":"difu moves this session after your turn ends and reports the result in this chat. Reply in one short sentence and stop; do not start other work."}),
    )
}

fn read(path: &Path, args: &[&str], cancel: &Cancel) -> Result<String> {
    Ok(process::checked(repo::git(path).args(args), cancel)?
        .trim_end_matches('\n')
        .to_owned())
}

fn succeeds(path: &Path, args: &[&str], cancel: &Cancel) -> Result<bool> {
    Ok(process::run(repo::git(path).args(args), None, cancel)?.code == 0)
}

fn transient(entry: &Entry) -> bool {
    matches!(
        entry.kind.as_str(),
        "sending" | "sending_context" | "awaiting connection"
    )
}

fn megabytes(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / f64::from(1 << 20))
}

/// Commits reachable from `tip` that origin/main does not have.
fn count(workspace: &Path, tip: &str, cancel: &Cancel) -> Result<usize> {
    Ok(read(
        workspace,
        &["rev-list", "--count", &format!("{MAIN}..{tip}")],
        cancel,
    )?
    .parse()?)
}

/// A commit of the whole worktree (tracked, untracked, deletions) on top of
/// HEAD, built in a temporary index so the session's index stays as it is.
fn snapshot(workspace: &Path, id: &str, cancel: &Cancel) -> Result<Option<String>> {
    let temp = tempfile::tempdir()?;
    let index = temp.path().join("index");
    let indexed = |args: &[&str]| -> Result<String> {
        Ok(process::checked(
            repo::git(workspace)
                .env("GIT_INDEX_FILE", &index)
                .args(args),
            cancel,
        )?
        .trim_end_matches('\n')
        .to_owned())
    };
    indexed(&["read-tree", "HEAD"])?;
    indexed(&["add", "--all"]).context("Cannot read the session's uncommitted files")?;
    let tree = indexed(&["write-tree"])?;
    if tree == read(workspace, &["rev-parse", "HEAD^{tree}"], cancel)? {
        return Ok(None);
    }
    let message = format!("difu: uncommitted changes of session {id}");
    let commit = process::checked(
        repo::git(workspace).envs(IDENTITY).args([
            "commit-tree",
            "--no-gpg-sign",
            &tree,
            "-p",
            "HEAD",
            "-m",
            &message,
        ]),
        cancel,
    )?;
    Ok(Some(commit.trim().to_owned()))
}

/// Deletes the temporary bundle ref however the export ends.
struct Reference<'a> {
    workspace: &'a Path,
    name: String,
}

impl Drop for Reference<'_> {
    fn drop(&mut self) {
        let _ = process::run(
            repo::git(self.workspace).args(["update-ref", "-d", &self.name]),
            None,
            &Cancel::default(),
        );
    }
}

/// `git bundle` of the commits from origin/main to `tip`.
fn bundle(workspace: &Path, id: &str, tip: &str, cancel: &Cancel) -> Result<Vec<u8>> {
    let name = format!(
        "{BUNDLE_REF}/{}",
        id.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
    );
    read(workspace, &["update-ref", &name, tip], cancel)?;
    let reference = Reference { workspace, name };
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("session.bundle");
    process::checked(
        repo::git(workspace)
            .args(["bundle", "create"])
            .arg(&file)
            .args([reference.name.as_str(), &format!("^{MAIN}")]),
        cancel,
    )
    .context("Cannot bundle the session's commits")?;
    Ok(fs::read(&file)?)
}

fn validate(package: &Package) -> Result<()> {
    crate::cloud::repos::validate(&package.repository)?;
    for commit in std::iter::once(&package.head).chain(&package.wip) {
        ensure!(
            commit.len() >= 40 && commit.chars().all(|c| c.is_ascii_hexdigit()),
            "Invalid commit {commit} in the moved session"
        );
    }
    ensure!(
        package.wip.is_none() || package.bundle.is_some(),
        "The moved session's uncommitted files are missing"
    );
    Ok(())
}

/// A branch the moved session can take: a valid name nobody uses here.
fn available(root: &Path, branch: &str, cancel: &Cancel) -> Result<bool> {
    Ok(!branch.starts_with('-')
        && succeeds(root, &["check-ref-format", "--branch", branch], cancel)?
        && !succeeds(
            root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            cancel,
        )?)
}

/// Creates the session's worktree exactly as for a new session (from a fresh
/// origin/main), then puts the moved commits on its branch and the uncommitted
/// files back in the worktree. Returns the branch, with a note when the moved
/// branch name was already taken.
fn restore(
    package: &Package,
    session: &mut Session,
    home: &Path,
    cancel: &Cancel,
) -> Result<String> {
    super::workspace::prepare(session, home, cancel, |_| Ok(()))?;
    let worktree = session
        .workspace
        .clone()
        .context("The new worktree is missing")?;
    let root = session.job.root().clone();
    if let Some(bundle) = &package.bundle {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("session.bundle");
        fs::write(
            &file,
            STANDARD
                .decode(bundle)
                .context("The moved commits are not valid base64")?,
        )?;
        // Objects only: no ref changes, and malformed trees are refused.
        process::checked(
            repo::git(&worktree)
                .args(["-c", "transfer.fsckObjects=true", "bundle", "unbundle"])
                .arg(&file),
            cancel,
        )
        .with_context(|| {
            format!(
                "This server's copy of {} lacks commits the session builds on; make sure its origin/main is on GitHub",
                package.repository
            )
        })?;
    }
    for commit in std::iter::once(&package.head).chain(&package.wip) {
        ensure!(
            succeeds(
                &worktree,
                &["cat-file", "-e", &format!("{commit}^{{commit}}")],
                cancel
            )?,
            "The moved work is missing commit {commit}"
        );
    }
    let managed = session
        .branch
        .clone()
        .context("The new worktree has no branch")?;
    let mut taken = false;
    if !package.branch.is_empty() && package.branch != managed {
        if available(&root, &package.branch, cancel)? {
            read(&worktree, &["branch", "-m", &package.branch], cancel)?;
            session.branch = Some(package.branch.clone());
        } else {
            taken = true;
        }
    }
    process::checked(
        repo::checkout_git(&worktree, cancel)?.args(["reset", "--hard", "--quiet", &package.head]),
        cancel,
    )?;
    if let Some(wip) = &package.wip {
        process::checked(
            repo::checkout_git(&worktree, cancel)?.args(["read-tree", "-u", "--reset", wip]),
            cancel,
        )?;
        process::checked(repo::git(&worktree).args(["reset", "--quiet"]), cancel)?;
    }
    let branch = session.branch.clone().unwrap_or(managed);
    if package.upstream && branch == package.branch {
        // Pushing continues the branch (and its pull request) as before.
        for (key, value) in [
            (format!("branch.{branch}.remote"), "origin".to_owned()),
            (
                format!("branch.{branch}.merge"),
                format!("refs/heads/{branch}"),
            ),
        ] {
            let _ = process::run(
                repo::git(&worktree).args(["config", &key, &value]),
                None,
                cancel,
            );
        }
    }
    Ok(if taken {
        format!(
            "{branch} (the branch name {} is already taken here)",
            package.branch
        )
    } else {
        branch
    })
}

fn note(package: &Package, branch: &str) -> String {
    let commits = match package.commits {
        0 => "no commits".to_owned(),
        1 => "1 commit".to_owned(),
        n => format!("{n} commits"),
    };
    let files = if package.wip.is_some() {
        " and the uncommitted files"
    } else {
        ""
    };
    format!(
        "Moved here from {} (session {}). Branch {branch} has {commits} ahead of origin/main{files}. The next message continues this conversation on this server.",
        package.computer, package.source
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::process::Command;

    pub(crate) const REPOSITORY: &str = "acme/widgets";
    pub(crate) const BINARY: [u8; 10] = [0, 159, 146, 150, 255, 0, 1, 2, 13, 10];

    /// Git for fixtures: any transport, a fixed identity, no signing.
    pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env_remove("GIT_ALLOW_PROTOCOL");
        Ok(process::checked(&mut command, &Cancel::default())?
            .trim()
            .to_owned())
    }

    /// A bare `origin` whose main has a README and a file the session deletes.
    pub(crate) fn origin(temp: &Path) -> Result<PathBuf> {
        let origin = temp.join("origin.git");
        let seed = temp.join("seed");
        fs::create_dir_all(&seed)?;
        git(temp, &["init", "--bare", "origin.git"])?;
        git(&seed, &["init"])?;
        fs::write(seed.join("README.md"), "hello\n")?;
        fs::write(seed.join("obsolete.txt"), "remove me\n")?;
        git(&seed, &["add", "."])?;
        git(&seed, &["commit", "-m", "Initial"])?;
        git(
            &seed,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )?;
        git(&seed, &["push", "origin", "main"])?;
        Ok(origin)
    }

    /// Lands a new commit on origin's main, as other people's merges do.
    pub(crate) fn advance_main(temp: &Path) -> Result<()> {
        let seed = temp.join("seed");
        fs::write(seed.join("CHANGELOG.md"), "newer main\n")?;
        git(&seed, &["add", "."])?;
        git(&seed, &["commit", "-m", "Newer main"])?;
        git(&seed, &["push", "origin", "main"])?;
        Ok(())
    }

    /// A local clone of `origin` whose origin URL is on GitHub, and an idle
    /// session `one` in a difu worktree with two commits ahead of origin/main
    /// (the first pushed with upstream tracking), a staged new file, a
    /// modified and a deleted tracked file, and untracked text and binary
    /// files. Returns the session and its worktree.
    pub(crate) fn local_session(temp: &Path, origin: &Path, home: &Path) -> Result<Session> {
        let local = temp.join("local");
        git(temp, &["clone", &origin.to_string_lossy(), "local"])?;
        let worktree = home.join("worktrees").join("one");
        fs::create_dir_all(home.join("worktrees"))?;
        git(
            &local,
            &[
                "worktree",
                "add",
                "-b",
                "difu/agent-one",
                &worktree.to_string_lossy(),
                "origin/main",
            ],
        )?;
        let base = git(&worktree, &["rev-parse", "HEAD"])?;
        fs::write(worktree.join("feature.rs"), "fn feature() {}\n")?;
        git(&worktree, &["add", "."])?;
        git(&worktree, &["commit", "-m", "Add feature"])?;
        git(&worktree, &["push", "-u", "origin", "difu/agent-one"])?;
        fs::write(worktree.join("feature.rs"), "fn feature() { todo() }\n")?;
        fs::write(worktree.join("logo.bin"), BINARY)?;
        git(&worktree, &["add", "."])?;
        git(&worktree, &["commit", "-m", "Draw the logo"])?;
        git(
            &local,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://github.com/{REPOSITORY}.git"),
            ],
        )?;
        fs::write(worktree.join("README.md"), "hello, server\n")?;
        fs::remove_file(worktree.join("obsolete.txt"))?;
        fs::write(worktree.join("staged.txt"), "staged\n")?;
        git(&worktree, &["add", "staged.txt"])?;
        fs::write(worktree.join("notes.txt"), "untracked\n")?;
        fs::create_dir_all(worktree.join("assets"))?;
        fs::write(worktree.join("assets").join("icon.bin"), BINARY)?;
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: local.canonicalize()?,
                isolated: true,
                base: "origin/main".into(),
                prompt: "Build the widget".into(),
                model: Some("claude/opus".into()),
                effort: Some("high".into()),
            }),
        );
        session.provider = provider::Provider::Claude;
        session.title = "Widget work".into();
        session.title_manual = true;
        session.status = Status::Idle;
        session.workspace = Some(worktree);
        session.workspace_ready = true;
        session.branch = Some("difu/agent-one".into());
        session.baseline = Some(base);
        session.permission_state.mode = Some("plan".into());
        session.thread_id = Some("local-native-thread".into());
        session.note("userMessage", "Build the widget");
        session.note("agentMessage", "The widget is half done; next: the logo.");
        Ok(session)
    }

    /// A server's storage in server mode whose base clone of `REPOSITORY` came
    /// from `origin` (as `gh repo clone` would).
    pub(crate) fn server_storage(home: &Path, origin: &Path) -> Result<Storage> {
        fs::create_dir_all(home)?;
        let storage = Storage {
            config: home.join("config.json"),
            cache: home.join("cache"),
        };
        storage.update_config(|config| config.server.enabled = true)?;
        let clone = crate::cloud::repos::path(&storage, REPOSITORY)?;
        fs::create_dir_all(clone.parent().context("clone parent")?)?;
        git(
            home,
            &["clone", &origin.to_string_lossy(), &clone.to_string_lossy()],
        )?;
        Ok(storage)
    }

    fn status(path: &Path) -> Result<String> {
        git(path, &["status", "--porcelain", "--untracked-files=all"])
    }

    fn lines(text: &str) -> std::collections::BTreeSet<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_session_round_trips_with_commits_uncommitted_untracked_and_binary_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let origin = origin(temp.path())?;
        let local_home = temp.path().join("mac");
        let session = local_session(temp.path(), &origin, &local_home)?;
        let worktree = session.workspace.clone().context("worktree")?;
        let before = status(&worktree)?;
        let head = git(&worktree, &["rev-parse", "HEAD"])?;

        let package = export(&session, &Cancel::default())?;
        // Exporting reads the worktree without changing it or leaving refs.
        assert_eq!(status(&worktree)?, before);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"])?, head);
        assert!(git(&worktree, &["show-ref", "--verify", "refs/difu/move/one"]).is_err());
        assert_eq!(package.repository, REPOSITORY);
        assert_eq!(package.branch, "difu/agent-one");
        assert!(package.upstream);
        assert_eq!(package.head, head);
        assert_eq!(package.commits, 2);
        assert!(package.wip.is_some() && package.bundle.is_some());
        assert_eq!(package.mode.as_deref(), Some("plan"));
        assert_eq!(package.entries.len(), 2);
        fits(&package, LIMIT)?;

        // Other work lands on main before the server prepares the worktree.
        advance_main(temp.path())?;
        let server_home = temp.path().join("server");
        let storage = server_storage(&server_home, &origin)?;
        let store = Store::fixture(server_home.clone(), Vec::new());
        assert_eq!(store.storage.config, storage.config);
        let root = crate::cloud::repos::ensure(&storage, &package.repository)?;
        let imported = import(
            &package,
            "two".into(),
            root.clone(),
            &store,
            &Cancel::default(),
        )?;

        let moved = imported.workspace.clone().context("server worktree")?;
        assert_eq!(moved, server_home.join("worktrees").join("two"));
        assert_eq!(git(&moved, &["rev-parse", "HEAD"])?, head);
        assert_eq!(
            git(&moved, &["branch", "--show-current"])?,
            "difu/agent-one"
        );
        assert_eq!(imported.branch.as_deref(), Some("difu/agent-one"));
        assert_eq!(
            git(&moved, &["config", "branch.difu/agent-one.merge"])?,
            "refs/heads/difu/agent-one"
        );
        // Staged work comes back unstaged; everything else exactly as it was.
        assert_eq!(
            lines(&status(&moved)?),
            lines(&before.replace("A  staged.txt", "?? staged.txt"))
        );
        assert_eq!(fs::read(moved.join("logo.bin"))?, BINARY);
        assert_eq!(fs::read(moved.join("assets").join("icon.bin"))?, BINARY);
        assert_eq!(
            fs::read_to_string(moved.join("README.md"))?,
            "hello, server\n"
        );
        assert!(!moved.join("obsolete.txt").exists());
        assert!(!moved.join("CHANGELOG.md").exists());
        // Only the scratch commit's objects remain; no ref points at it.
        assert!(git(&root, &["for-each-ref", "refs/difu"])?.is_empty());

        assert_eq!(imported.title, "Widget work");
        assert!(imported.title_manual && imported.title_ready);
        assert_eq!(imported.provider, provider::Provider::Claude);
        assert_eq!(imported.model.as_deref(), Some("claude/opus"));
        assert_eq!(imported.effort.as_deref(), Some("high"));
        assert_eq!(
            imported.permission_state.pending_mode.as_deref(),
            Some("plan")
        );
        assert!(imported.server_mode);
        assert_eq!(imported.status, Status::Idle);
        assert!(imported.thread_id.is_none() && imported.workspace_ready);
        assert!(matches!(&imported.job, Job::Coding(launch)
            if launch.prompt == "Build the widget" && launch.isolated && launch.repository == root.canonicalize()?));
        let texts: Vec<_> = imported.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts.first(), Some(&"Build the widget"));
        let note = texts.last().context("note")?;
        assert!(note.starts_with("Moved here from ") && note.contains("session one"));
        assert!(note.contains("2 commits ahead of origin/main and the uncommitted files"));
        let handoff = imported.provider_context.as_deref().context("handoff")?;
        assert!(handoff.starts_with("Difu conversation handoff."));
        assert!(handoff.contains("next: the logo") && handoff.contains("Moved here from"));
        Ok(())
    }

    fn idle() -> Session {
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: "/tmp/repo".into(),
                isolated: true,
                base: "origin/main".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.workspace = Some("/tmp/worktree".into());
        session.workspace_ready = true;
        session
    }

    #[test]
    fn busy_waiting_queued_moved_and_non_coding_sessions_cannot_move() -> Result<()> {
        check(&idle())?;
        type Change = fn(&mut Session);
        let refusals: Vec<(&str, Change)> = vec![
            ("current turn", |s| s.status = Status::Running),
            ("current turn", |s| s.turn_id = Some("turn".into())),
            ("current turn", |s| s.status = Status::Starting),
            ("pending questions", |s| {
                s.pending.push(super::super::Pending {
                    id: json!(1),
                    method: "item/tool/requestUserInput".into(),
                    params: json!({"difuAsync":true}),
                    responded: false,
                })
            }),
            ("queued messages", |s| s.queue.push("next".into())),
            ("already moved", |s| s.moved_to = Some("box:2".into())),
            ("already runs on a server", |s| s.server_mode = true),
            ("Unarchive", |s| s.archived = true),
            ("worktree was removed", |s| s.workspace_removed = true),
            ("still being prepared", |s| s.workspace_ready = false),
            ("Only coding sessions", |s| s.orchestrator = true),
            ("Only coding sessions", |s| {
                s.job = Job::Conflict {
                    root: "/tmp/repo".into(),
                    key: crate::model::PrKey {
                        owner: "acme".into(),
                        repo: "widgets".into(),
                        number: 1,
                    },
                    head: "abc".into(),
                    model: crate::model::ModelChoice::default(),
                }
            }),
        ];
        for (reason, change) in refusals {
            let mut session = idle();
            change(&mut session);
            let error = check(&session).err().context(reason)?;
            assert!(format!("{error:#}").contains(reason), "{reason}: {error:#}");
            assert!(export(&session, &Cancel::default()).is_err());
        }
        Ok(())
    }

    #[test]
    fn packages_over_the_message_cap_are_refused_with_their_sizes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let origin = origin(temp.path())?;
        let session = local_session(temp.path(), &origin, &temp.path().join("mac"))?;
        let package = export(&session, &Cancel::default())?;
        fits(&package, LIMIT)?;
        let error = fits(&package, ENVELOPE).err().context("over the cap")?;
        let message = format!("{error:#}");
        assert!(
            message.starts_with("This session is too large to move"),
            "{message}"
        );
        assert!(
            message.contains("at most 0.1 MB") && message.contains("chat "),
            "{message}"
        );
        let mut huge = package;
        huge.bundle = Some("A".repeat(LIMIT));
        assert!(fits(&huge, LIMIT).is_err());
        Ok(())
    }

    #[test]
    fn a_server_missing_the_base_commits_refuses_and_leaves_nothing_behind() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let origin = origin(temp.path())?;
        let mut session = local_session(temp.path(), &origin, &temp.path().join("mac"))?;
        // This computer's origin/main has a commit GitHub never saw.
        let worktree = session.workspace.clone().context("worktree")?;
        fs::write(worktree.join("local-only.txt"), "only here\n")?;
        git(&worktree, &["add", "local-only.txt"])?;
        git(&worktree, &["commit", "-m", "Local main"])?;
        let local_main = git(&worktree, &["rev-parse", "HEAD"])?;
        git(&worktree, &["update-ref", MAIN, &local_main])?;
        fs::write(worktree.join("after.txt"), "after\n")?;
        git(&worktree, &["add", "after.txt"])?;
        git(&worktree, &["commit", "-m", "After local main"])?;
        session.note("userMessage", "more");
        let package = export(&session, &Cancel::default())?;
        assert_eq!(package.commits, 1);

        let server_home = temp.path().join("server");
        let storage = server_storage(&server_home, &origin)?;
        let store = Store::fixture(server_home.clone(), Vec::new());
        let root = crate::cloud::repos::ensure(&storage, REPOSITORY)?;
        let error = import(
            &package,
            "two".into(),
            root.clone(),
            &store,
            &Cancel::default(),
        )
        .err()
        .context("missing prerequisites")?;
        assert!(format!("{error:#}").contains("lacks commits"), "{error:#}");
        assert!(!server_home.join("worktrees").join("two").exists());
        assert!(
            git(&root, &["for-each-ref", "refs/heads"])?
                .lines()
                .all(|line| line.ends_with("refs/heads/main"))
        );
        assert_eq!(git(&root, &["worktree", "list"])?.lines().count(), 1);
        Ok(())
    }

    #[test]
    fn a_clean_session_moves_without_a_bundle_and_keeps_its_base() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let origin = origin(temp.path())?;
        let local = temp.path().join("local");
        git(temp.path(), &["clone", &origin.to_string_lossy(), "local"])?;
        git(
            &local,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:Acme/Widgets.git",
            ],
        )?;
        let home = temp.path().join("mac");
        let worktree = home.join("worktrees").join("one");
        git(
            &local,
            &[
                "worktree",
                "add",
                "--detach",
                &worktree.to_string_lossy(),
                "origin/main",
            ],
        )?;
        let mut session = idle();
        session.workspace = Some(worktree.clone());
        let package = export(&session, &Cancel::default())?;
        assert_eq!(package.repository, REPOSITORY);
        assert!(package.bundle.is_none() && package.wip.is_none());
        assert!(package.branch.is_empty() && !package.upstream);
        assert_eq!(package.commits, 0);

        advance_main(temp.path())?;
        let server_home = temp.path().join("server");
        let storage = server_storage(&server_home, &origin)?;
        let store = Store::fixture(server_home, Vec::new());
        let root = crate::cloud::repos::ensure(&storage, REPOSITORY)?;
        let imported = import(&package, "two".into(), root, &store, &Cancel::default())?;
        let moved = imported.workspace.clone().context("server worktree")?;
        assert_eq!(git(&moved, &["rev-parse", "HEAD"])?, package.head);
        assert_eq!(
            git(&moved, &["branch", "--show-current"])?,
            "difu/agent-two"
        );
        assert!(status(&moved)?.is_empty());
        assert!(
            imported
                .entries
                .last()
                .is_some_and(|entry| entry.text.contains("no commits ahead"))
        );
        Ok(())
    }

    #[test]
    fn a_branch_name_taken_on_the_server_keeps_the_managed_branch() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let origin = origin(temp.path())?;
        let session = local_session(temp.path(), &origin, &temp.path().join("mac"))?;
        let package = export(&session, &Cancel::default())?;
        let server_home = temp.path().join("server");
        let storage = server_storage(&server_home, &origin)?;
        let store = Store::fixture(server_home, Vec::new());
        let root = crate::cloud::repos::ensure(&storage, REPOSITORY)?;
        git(&root, &["branch", "difu/agent-one", "main"])?;
        let taken = git(&root, &["rev-parse", "difu/agent-one"])?;
        let imported = import(
            &package,
            "two".into(),
            root.clone(),
            &store,
            &Cancel::default(),
        )?;
        let moved = imported.workspace.clone().context("server worktree")?;
        assert_eq!(
            git(&moved, &["branch", "--show-current"])?,
            "difu/agent-two"
        );
        assert_eq!(git(&moved, &["rev-parse", "HEAD"])?, package.head);
        // Someone else's branch stays untouched.
        assert_eq!(git(&root, &["rev-parse", "difu/agent-one"])?, taken);
        assert!(
            imported
                .entries
                .last()
                .is_some_and(|entry| entry.text.contains("already taken here"))
        );
        Ok(())
    }

    #[test]
    fn the_tool_schedules_only_its_own_session_onto_a_saved_server() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut orchestrator = idle();
        orchestrator.id = "manager".into();
        orchestrator.orchestrator = true;
        let store = Store::fixture(temp.path().into(), vec![idle(), orchestrator]);
        let error = schedule(&store, "one", &json!({"server":"box"}))
            .err()
            .context("no saved servers")?;
        assert!(
            format!("{error:#}").contains("no saved servers"),
            "{error:#}"
        );
        crate::cloud::servers::save(
            &store.storage,
            crate::cloud::servers::tests::server("box", "id-box"),
        )?;
        for args in [
            json!({}),
            json!({"server":""}),
            json!({"server":"box","id":"another"}),
            json!({"server":"elsewhere"}),
        ] {
            assert!(schedule(&store, "one", &args).is_err(), "{args}");
        }
        assert!(store.get("one")?.move_scheduled.is_none());
        assert!(schedule(&store, "manager", &json!({"server":"box"})).is_err());
        let reply = schedule(&store, "one", &json!({"server":"box"}))?;
        assert_eq!(reply.get("scheduled"), Some(&json!(true)));
        let session = store.get("one")?;
        assert_eq!(session.move_scheduled.as_deref(), Some("box"));
        assert!(
            session
                .entries
                .last()
                .is_some_and(|entry| entry.text.contains("when the current turn ends"))
        );
        // A scheduled move is never persisted, so a restart cannot replay it.
        let saved: Session = serde_json::from_slice(&fs::read(temp.path().join("one.json"))?)?;
        assert!(saved.move_scheduled.is_none());
        Ok(())
    }

    #[test]
    fn the_tool_is_offered_to_local_coding_sessions_only() {
        let named = |session: &Session| {
            super::super::orchestrator::tools(session)
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some(TOOL))
        };
        let mut session = idle();
        assert!(named(&session));
        let schema = tool();
        assert_eq!(
            schema.pointer("/inputSchema/required"),
            Some(&json!(["server"]))
        );
        assert_eq!(
            schema.pointer("/inputSchema/additionalProperties"),
            Some(&json!(false))
        );
        assert!(
            schema
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("explicitly asks")
                    && text.contains("after the current turn ends"))
        );
        session.server_mode = true;
        assert!(!named(&session));
        session.server_mode = false;
        session.orchestrator = true;
        assert!(!named(&session));
    }
}
