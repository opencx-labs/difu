//! Per-session rootless Podman sandboxes on cloud servers. The agent and its
//! shells run inside; difu stays on the host and talks to them over stdio.
//!
//! * A session is sandboxed when the server runs in server mode with
//!   `server.sandbox.enabled`, and only coding sessions are. With sandboxing on
//!   and Podman missing, spawning fails instead of running the agent directly.
//! * One pod per session (`difu-<session id>`), created on first use, so the
//!   agent and its shells share a network namespace (their own ports) that no
//!   other session can reach. The pod also shares its PID namespace, so the
//!   pod's infra process is PID 1 and reaps orphaned processes (neither agent
//!   CLI does), and `server.sandbox.{cpus,memory}` limit the pod as a whole:
//!   one budget per session, however many shells it opens.
//! * Each process runs as `podman run --rm -i [-t] --pod difu-<id> ...`, built
//!   from the original command by [`Sandbox::invocation`]. The session
//!   workspace and its repository's Git common directory (so worktree metadata
//!   resolves), the server logins (read-write, so tokens can refresh) and the
//!   host's Git identity and CLI binaries (read-only) are mounted at identical
//!   absolute paths, with `HOME` set to the host home. So is the session's
//!   attachment directory (read-only): Codex reads pasted and uploaded images
//!   by path, and videos are mentioned by path.
//! * User mapping: rootless Podman's default mapping, pinned with `--user 0:0`
//!   so an image's `USER` cannot change it. Container root is the host user that
//!   runs difu (`difu` on a server) and has no privileges outside the
//!   container's user namespace. Everything written through a mount is owned by
//!   that user on the host, exactly as if difu had run the agent directly; any
//!   other container user would map to a subordinate ID and leave files the
//!   host user cannot manage. Mounted repositories look root-owned inside,
//!   matching the process, so Git's ownership check passes. `IS_SANDBOX=1`
//!   tells Claude Code that running as root is intended.
//! * A [`Container`] guard removes its container when dropped: difu stops
//!   process groups with SIGKILL, which `podman run` cannot forward, so the
//!   container would otherwise keep running. Deleting, clearing or cleaning up
//!   a session removes its pod ([`remove_pod`]); a service restart removes
//!   containers a stopped service left behind ([`sweep`]).
use super::{Job, Session};
use crate::{
    cloud::{SandboxSettings, ServerSettings, cli::SandboxCommand},
    process::{self, Cancel},
    storage::Storage,
};
use anyhow::{Context, Result, bail, ensure};
use portable_pty::CommandBuilder;
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const CONTAINERFILE: &str = include_str!("sandbox/Containerfile");
const PODMAN: &str = "podman";
/// Every sandbox pod and container carries its session ID.
const SESSION_LABEL: &str = "difu.session";
/// Only agent and shell containers carry a role (a pod's infra container does
/// not), so cleanup never addresses an infra container directly.
const ROLE_LABEL: &str = "difu.role";
/// The image's own search path, after the host's `~/.local/bin`.
const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Server logins, relative to the home directory. Read-write: the CLIs refresh
/// tokens and keep their conversation history there, which resuming needs.
const LOGINS: [&str; 4] = [".codex", ".claude", ".claude.json", ".config/gh"];
/// Read-only: the Git identity and `gh auth setup-git` credential helper that
/// commits and pushes need, and the agent CLIs (updated on the host).
const READ_ONLY: [&str; 3] = [".gitconfig", ".local/bin", ".local/share/claude"];
/// Bounds every short Podman call, so a wedged Podman cannot hang a worker.
const TIMEOUT: Duration = Duration::from_secs(60);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What a sandboxed process is; part of its container name and labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Claude,
    Codex,
    Shell,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Shell => "shell",
        }
    }
}

/// A session's sandbox: what wrapping one of its processes needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sandbox {
    session: String,
    pod: String,
    image: String,
    cpus: String,
    memory: String,
    home: PathBuf,
    workspace: PathBuf,
    /// The session's attachment directory (`media::directory`).
    attachments: Option<PathBuf>,
}

impl Sandbox {
    /// `None` runs the session's processes directly, as on every ordinary
    /// installation.
    pub(crate) fn for_session(storage: &Storage, session: &Session) -> Result<Option<Self>> {
        let config = storage.load_config()?;
        let Some(mut sandbox) = Self::decide(&config.server, session, dirs::home_dir().as_deref())?
        else {
            return Ok(None);
        };
        ensure!(
            installed(PODMAN),
            "Sandboxing is enabled on this server, but Podman is not installed. Install podman, or turn sandboxing off (server.sandbox.enabled in config.json) to run agents directly on the server"
        );
        // Created now, so media sent later appears inside the running sandbox.
        sandbox.attachments = Some(super::media::directory(storage, &session.id)?);
        Ok(Some(sandbox))
    }

    fn decide(
        settings: &ServerSettings,
        session: &Session,
        home: Option<&Path>,
    ) -> Result<Option<Self>> {
        if !settings.enabled || !settings.sandbox.enabled {
            return Ok(None);
        }
        let Job::Coding(launch) = &session.job else {
            return Ok(None);
        };
        let home = home.context("Cannot locate the home directory to mount into the sandbox")?;
        Ok(Some(Self {
            pod: pod_name(&session.id)?,
            session: session.id.clone(),
            image: settings.sandbox.image().into(),
            cpus: settings.sandbox.cpus.trim().into(),
            memory: settings.sandbox.memory.trim().into(),
            home: home.into(),
            // Spawn sites fall back to the repository before a worktree exists.
            workspace: session
                .workspace
                .clone()
                .unwrap_or_else(|| launch.repository.clone()),
            attachments: None,
        }))
    }

    /// Run `command` in this session's pod. Keep the returned guard next to
    /// the spawned process and drop it after the process.
    pub(crate) fn command(&self, role: Role, command: &Command) -> Result<(Command, Container)> {
        self.command_with(Arc::new(Podman), role, command)
    }

    fn command_with(
        &self,
        runner: Arc<dyn Runner>,
        role: Role,
        command: &Command,
    ) -> Result<(Command, Container)> {
        let program = Program::from_command(command);
        let (invocation, container) = self.prepare(runner, role, &program, false)?;
        let mut wrapped = Command::new(PODMAN);
        wrapped.args(&invocation.args).envs(invocation.env);
        if let Some(cwd) = &program.cwd {
            wrapped.current_dir(cwd);
        }
        Ok((wrapped, container))
    }

    /// Run a shell in this session's pod with a terminal.
    pub(crate) fn pty(&self, builder: &CommandBuilder) -> Result<(CommandBuilder, Container)> {
        let program = Program::from_pty(builder);
        let (invocation, container) =
            self.prepare(Arc::new(Podman), Role::Shell, &program, true)?;
        let mut wrapped = CommandBuilder::new(PODMAN);
        wrapped.args(&invocation.args);
        for (key, value) in &invocation.env {
            wrapped.env(key, value);
        }
        if let Some(cwd) = &program.cwd {
            wrapped.cwd(cwd);
        }
        Ok((wrapped, container))
    }

    fn prepare(
        &self,
        runner: Arc<dyn Runner>,
        role: Role,
        program: &Program,
        tty: bool,
    ) -> Result<(Invocation, Container)> {
        self.ensure_pod(runner.as_ref())?;
        let cwd = program.cwd.as_deref().unwrap_or(&self.workspace);
        let mounts = self.mounts(cwd);
        let name = container_name(&self.pod, role);
        let invocation = self.invocation(role, &name, program, tty, &mounts)?;
        Ok((invocation, Container::new(name, runner)))
    }

    /// The `podman` arguments and environment for one process. Values of the
    /// command's own variables travel in Podman's environment (`-e KEY`), so
    /// tokens never appear in an argument list other users can read.
    fn invocation(
        &self,
        role: Role,
        name: &str,
        program: &Program,
        tty: bool,
        mounts: &[Mount],
    ) -> Result<Invocation> {
        let cwd = program.cwd.as_deref().unwrap_or(&self.workspace);
        let mut args = os(["run", "--rm", "-i"]);
        if tty {
            args.push("-t".into());
        }
        args.extend(os([
            "--pod",
            &self.pod,
            "--name",
            name,
            "--label",
            &format!("{SESSION_LABEL}={}", self.session),
            "--label",
            &format!("{ROLE_LABEL}={}", role.name()),
            "--user",
            "0:0",
            // Agent streams carry file contents and tokens; keep them out of logs.
            "--log-driver",
            "none",
            // Ctrl-P Ctrl-Q typed in a shell must reach the shell.
            "--detach-keys=",
            "-w",
        ]));
        args.push(cwd.into());
        for mount in mounts {
            args.push("-v".into());
            args.push(mount.volume()?);
        }
        let mut env = Vec::new();
        for variable in self.variables(program) {
            args.push("-e".into());
            if variable.secret {
                args.push(variable.key.clone());
                env.push((variable.key, variable.value));
            } else {
                let mut pair = variable.key;
                pair.push("=");
                pair.push(variable.value);
                args.push(pair);
            }
        }
        args.push((&self.image).into());
        args.push(program.program.clone());
        args.extend(program.args.iter().cloned());
        Ok(Invocation { args, env })
    }

    /// The sandbox's own variables, then the command's. `HOME` and `PATH`
    /// always describe the sandbox, never the host process.
    fn variables(&self, program: &Program) -> Vec<Variable> {
        let mut path = self.home.join(".local/bin").into_os_string();
        path.push(":");
        path.push(SYSTEM_PATH);
        let mut variables = vec![
            Variable::plain("HOME", self.home.as_os_str()),
            Variable::plain("PATH", &path),
            Variable::plain("LANG", "C.UTF-8"),
            Variable::plain("SHELL", "/bin/bash"),
            // The CLIs are mounted read-only and updated on the host.
            Variable::plain("DISABLE_AUTOUPDATER", "1"),
            // Container root is the unprivileged host user (see the module docs).
            Variable::plain("IS_SANDBOX", "1"),
        ];
        for (key, value) in &program.env {
            if key == "HOME" || key == "PATH" {
                continue;
            }
            variables.retain(|variable| &variable.key != key);
            variables.push(Variable {
                key: key.clone(),
                value: value.clone(),
                secret: !podman_reads(key),
            });
        }
        variables
    }

    fn mounts(&self, cwd: &Path) -> Vec<Mount> {
        let mut candidates = Vec::new();
        for directory in [cwd, self.workspace.as_path()] {
            // `-w` needs the working directory; a removed workspace is skipped.
            if directory == cwd || directory.exists() {
                candidates.push(Mount::writable(directory));
            }
            if let Some(common) = common_dir(directory) {
                candidates.push(Mount::writable(&common));
            }
        }
        for login in LOGINS {
            let path = self.home.join(login);
            if path.exists() {
                candidates.push(Mount::writable(&path));
            }
        }
        for file in READ_ONLY {
            let path = self.home.join(file);
            if path.exists() {
                candidates.push(Mount {
                    path,
                    read_only: true,
                });
            }
        }
        if let Some(attachments) = self.attachments.as_ref().filter(|path| path.is_dir()) {
            candidates.push(Mount {
                path: attachments.clone(),
                read_only: true,
            });
        }
        arrange(candidates)
    }

    /// Create the pod if needed and start it with a short-lived Podman call.
    /// The first container started in a pod would otherwise become the parent
    /// of the pod's network helpers, and difu kills the whole process tree of
    /// an agent or shell it stops.
    fn ensure_pod(&self, runner: &dyn Runner) -> Result<()> {
        // Agent and shell spawns of one session must not both create its pod.
        static CREATING: Mutex<()> = Mutex::new(());
        let _creating = CREATING
            .lock()
            .map_err(|_| anyhow::anyhow!("Sandbox lock failed"))?;
        // Podman would try to pull a missing local image and fail obscurely.
        if self.image.starts_with("localhost/") {
            ensure!(
                runner.podman(&os(["image", "exists", &self.image]))?.code == 0,
                "The sandbox image {} is missing. Run `difu sandbox build` on the server",
                self.image
            );
        }
        let exists = |runner: &dyn Runner| -> Result<bool> {
            Ok(runner.podman(&os(["pod", "exists", &self.pod]))?.code == 0)
        };
        if !exists(runner)? {
            let created = runner.podman(&self.pod_args())?;
            if created.code != 0 && !exists(runner)? {
                bail!(
                    "Cannot create sandbox pod {}: {}",
                    self.pod,
                    String::from_utf8_lossy(&created.stderr).trim()
                );
            }
        }
        let started = runner.podman(&os(["pod", "start", &self.pod]))?;
        ensure!(
            started.code == 0,
            "Cannot start sandbox pod {}: {}",
            self.pod,
            String::from_utf8_lossy(&started.stderr).trim()
        );
        Ok(())
    }

    fn pod_args(&self) -> Vec<OsString> {
        let mut args = os([
            "pod",
            "create",
            "--name",
            &self.pod,
            "--label",
            &format!("{SESSION_LABEL}={}", self.session),
            "--share",
            "ipc,net,uts,pid",
        ]);
        if !self.cpus.is_empty() {
            args.extend(os(["--cpus", &self.cpus]));
        }
        if !self.memory.is_empty() {
            args.extend(os(["--memory", &self.memory]));
        }
        args
    }
}

/// Removes its sandbox container when dropped. Own it next to the process
/// running `podman run` and declare it after that process, so the process is
/// stopped first.
pub(crate) struct Container {
    name: String,
    runner: Arc<dyn Runner>,
    removed: bool,
}
impl Container {
    fn new(name: String, runner: Arc<dyn Runner>) -> Self {
        Self {
            name,
            runner,
            removed: false,
        }
    }
    #[cfg(test)]
    fn name(&self) -> &str {
        &self.name
    }
    /// Runs once. Often there is nothing left to remove: `--rm` removed the
    /// container when its process exited, unless difu killed `podman run`.
    pub(crate) fn remove(&mut self) {
        if !std::mem::replace(&mut self.removed, true) {
            let _ = self.runner.podman(&os(["rm", "-f", "-t", "0", &self.name]));
        }
    }
    /// `podman run` returned by itself, so its container has stopped and
    /// `--rm` removed it; dropping the guard then costs no Podman call.
    pub(crate) fn exited(&mut self) {
        self.removed = true;
    }
}
impl Drop for Container {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Removes a session's pod and anything still running in it, after the
/// session is deleted, cleared or cleaned up. Failures are reported, not
/// raised: a leftover pod holds no session data.
pub(crate) fn remove_pod(storage: &Storage, session: &str) {
    let server = storage
        .load_config()
        .is_ok_and(|config| config.server.enabled);
    if server
        && installed(PODMAN)
        && let Err(error) = remove_pod_with(&Podman, session)
    {
        eprintln!("Sandbox cleanup for {session}: {error:#}");
    }
}

fn remove_pod_with(runner: &dyn Runner, session: &str) -> Result<()> {
    // A session whose ID cannot name a pod never had one.
    let Ok(pod) = pod_name(session) else {
        return Ok(());
    };
    let output = runner.podman(&os(["pod", "rm", "-f", "-t", "0", &pod]))?;
    let error = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.code == 0 || error.contains("no such pod"),
        "{}",
        error.trim()
    );
    Ok(())
}

/// Run before the service starts any session. No agent or shell is running
/// yet, so every sandbox container still present was left by a stopped
/// service and could still be changing a worktree.
pub(crate) fn sweep(storage: &Storage) {
    let enabled = storage
        .load_config()
        .is_ok_and(|config| config.server.enabled && config.server.sandbox.enabled);
    if enabled
        && installed(PODMAN)
        && let Err(error) = sweep_with(&Podman)
    {
        eprintln!("Sandbox cleanup: {error:#}");
    }
}

fn sweep_with(runner: &dyn Runner) -> Result<()> {
    let listed = runner.podman(&os([
        "ps",
        "-a",
        "--filter",
        &format!("label={ROLE_LABEL}"),
        "--format",
        "{{.Names}}",
    ]))?;
    ensure!(
        listed.code == 0,
        "Cannot list sandbox containers: {}",
        String::from_utf8_lossy(&listed.stderr).trim()
    );
    let names = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(OsString::from)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return Ok(());
    }
    let mut args = os(["rm", "-f", "-t", "0"]);
    args.extend(names);
    let removed = runner.podman(&args)?;
    ensure!(
        removed.code == 0,
        "Cannot remove stale sandbox containers: {}",
        String::from_utf8_lossy(&removed.stderr).trim()
    );
    Ok(())
}

/// `difu-<session id>`. Session IDs come from difu, but they also name Podman
/// objects, so anything outside Podman's name alphabet is refused.
pub(crate) fn pod_name(session: &str) -> Result<String> {
    let mut characters = session.chars();
    ensure!(
        session.len() <= 128
            && characters.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "Session ID {session:?} cannot name a sandbox"
    );
    Ok(format!("difu-{session}"))
}

fn container_name(pod: &str, role: Role) -> String {
    format!(
        "{pod}-{}-{:x}-{}",
        role.name(),
        chrono::Utc::now().timestamp_millis(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Variables Podman itself reads. Setting them on the Podman process would
/// reconfigure Podman, so they travel as `-e KEY=VALUE` instead.
fn podman_reads(key: &OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return true;
    };
    matches!(
        key,
        "TMPDIR"
            | "NOTIFY_SOCKET"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "REGISTRY_AUTH_FILE"
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "NO_PROXY"
            | "http_proxy"
            | "https_proxy"
            | "no_proxy"
    ) || [
        "XDG_",
        "CONTAINERS_",
        "CONTAINER_",
        "PODMAN_",
        "DOCKER_",
        "BUILDAH_",
        "STORAGE_",
        "LISTEN_",
        "SSL_",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}

fn installed(program: &str) -> bool {
    search(program, std::env::var_os("PATH").as_deref()).is_some()
}

fn search(program: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|directory| directory.join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// The repository's shared `.git` directory, so a linked worktree's metadata
/// (and the objects and refs it commits to) resolve inside the sandbox.
fn common_dir(directory: &Path) -> Option<PathBuf> {
    let output = process::run(
        crate::repo::git(directory).args(["rev-parse", "--git-common-dir"]),
        None,
        &Cancel::default(),
    )
    .ok()
    .filter(|output| output.code == 0)?;
    let relative = String::from_utf8(output.stdout).ok()?;
    directory
        .join(relative.trim_end_matches('\n'))
        .canonicalize()
        .ok()
}

/// Drop duplicates and mounts already covered by another with the same access.
fn arrange(candidates: Vec<Mount>) -> Vec<Mount> {
    let mut mounts: Vec<Mount> = Vec::new();
    for mount in candidates {
        if mounts
            .iter()
            .any(|kept| kept.read_only == mount.read_only && mount.path.starts_with(&kept.path))
        {
            continue;
        }
        mounts.retain(|kept| {
            kept.read_only != mount.read_only || !kept.path.starts_with(&mount.path)
        });
        mounts.push(mount);
    }
    mounts
}

fn os<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<OsString> {
    items.into_iter().map(OsString::from).collect()
}

/// A process as the spawn site described it, before sandboxing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Program {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    cwd: Option<PathBuf>,
}
impl Program {
    fn from_command(command: &Command) -> Self {
        Self {
            program: command.get_program().to_owned(),
            args: command.get_args().map(OsStr::to_owned).collect(),
            // A container never inherits the host environment, so removed
            // variables need no counterpart.
            env: command
                .get_envs()
                .filter_map(|(key, value)| Some((key.to_owned(), value?.to_owned())))
                .collect(),
            cwd: command.get_current_dir().map(Path::to_owned),
        }
    }
    fn from_pty(builder: &CommandBuilder) -> Self {
        // The host's login shell need not exist in the image.
        let login = || (OsString::from("/bin/bash"), vec![OsString::from("-l")]);
        let (program, args) = if builder.is_default_prog() {
            login()
        } else {
            match builder.get_argv().as_slice() {
                [program, args @ ..] => (program.clone(), args.to_vec()),
                [] => login(),
            }
        };
        Self {
            program,
            args,
            env: builder
                .iter_extra_env_as_str()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            cwd: builder.get_cwd().map(PathBuf::from),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    args: Vec<OsString>,
    /// Set on the Podman process and forwarded by name.
    env: Vec<(OsString, OsString)>,
}

struct Variable {
    key: OsString,
    value: OsString,
    secret: bool,
}
impl Variable {
    fn plain(key: &str, value: impl AsRef<OsStr>) -> Self {
        Self {
            key: key.into(),
            value: value.as_ref().to_owned(),
            secret: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mount {
    path: PathBuf,
    read_only: bool,
}
impl Mount {
    fn writable(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            read_only: false,
        }
    }
    /// The same absolute path inside and outside the sandbox.
    fn volume(&self) -> Result<OsString> {
        ensure!(
            self.path.is_absolute(),
            "Sandbox mounts need absolute paths: {}",
            self.path.display()
        );
        ensure!(
            !self.path.as_os_str().as_bytes().contains(&b':'),
            "Cannot mount {} into a sandbox: Podman volume paths cannot contain ':'",
            self.path.display()
        );
        let mut volume = self.path.clone().into_os_string();
        volume.push(":");
        volume.push(&self.path);
        if self.read_only {
            volume.push(":ro");
        }
        Ok(volume)
    }
}

/// Podman calls, injectable so tests cover the guard and pod lifecycle
/// without Podman.
pub(crate) trait Runner: Send + Sync {
    fn podman(&self, args: &[OsString]) -> Result<process::Output>;
}

struct Podman;
impl Runner for Podman {
    fn podman(&self, args: &[OsString]) -> Result<process::Output> {
        bounded(Command::new(PODMAN).args(args), TIMEOUT)
    }
}

fn bounded(command: &mut Command, timeout: Duration) -> Result<process::Output> {
    let cancel = Cancel::default();
    let timer = cancel.clone();
    let (finished, done) = mpsc::channel::<()>();
    thread::spawn(move || {
        if done.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
            timer.cancel();
        }
    });
    let output = process::run(command, None, &cancel);
    drop(finished);
    match output {
        Err(_) if cancel.cancelled() => {
            bail!("Podman did not finish within {} seconds", timeout.as_secs())
        }
        output => output,
    }
}

pub fn command(storage: &Storage, command: SandboxCommand) -> Result<()> {
    match command {
        SandboxCommand::Build => build(),
        SandboxCommand::Status => status(storage),
    }
}

fn build() -> Result<()> {
    ensure!(
        installed(PODMAN),
        "Podman is not installed. Install it first, for example with `sudo apt-get install podman`"
    );
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("Containerfile");
    fs::write(&file, CONTAINERFILE)?;
    let status = Command::new(PODMAN)
        .args(build_args(&file, directory.path()))
        .status()
        .context("Cannot run podman build")?;
    ensure!(status.success(), "podman build failed ({status})");
    println!("Built {}", SandboxSettings::DEFAULT_IMAGE);
    Ok(())
}

fn build_args(file: &Path, context: &Path) -> Vec<OsString> {
    let mut args = os(["build", "-t", SandboxSettings::DEFAULT_IMAGE, "-f"]);
    args.push(file.into());
    args.push(context.into());
    args
}

fn status(storage: &Storage) -> Result<()> {
    let settings = storage.load_config()?.server;
    println!("Sandboxing: {}", mode(&settings));
    println!("Limits per session: {}", limits(&settings.sandbox));
    if !installed(PODMAN) {
        println!("Podman: not installed");
        return Ok(());
    }
    let podman = Podman;
    let version = podman.podman(&os(["--version"]))?;
    ensure!(
        version.code == 0,
        "Podman is installed but does not run: {}",
        String::from_utf8_lossy(&version.stderr).trim()
    );
    println!(
        "Podman: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    let image = settings.sandbox.image();
    let present = podman.podman(&os(["image", "exists", image]))?.code == 0;
    println!(
        "Image: {image} ({})",
        if present {
            "present"
        } else {
            "missing; run `difu sandbox build`"
        }
    );
    let listed = podman.podman(&os([
        "pod",
        "ps",
        "--filter",
        &format!("label={SESSION_LABEL}"),
        "--format",
        "{{.Name}} {{.Status}} {{.NumberOfContainers}}",
    ]))?;
    ensure!(
        listed.code == 0,
        "Cannot list sandboxes: {}",
        String::from_utf8_lossy(&listed.stderr).trim()
    );
    let pods = pods(&String::from_utf8_lossy(&listed.stdout));
    let running = pods.iter().filter(|pod| pod.running).count();
    println!("Sandboxes: {} ({running} running)", pods.len());
    for pod in pods {
        println!("  {}  {}  {}", pod.name, pod.status, pod.containers);
    }
    Ok(())
}

fn mode(settings: &ServerSettings) -> String {
    match (settings.enabled, settings.sandbox.enabled) {
        (true, true) => "on (each session's agent and shells run in a container)".into(),
        (false, true) => "off until server mode is on (`difu serve`)".into(),
        (_, false) => "off (agents run directly on this machine)".into(),
    }
}

fn limits(sandbox: &SandboxSettings) -> String {
    let cpus = sandbox.cpus.trim();
    let memory = sandbox.memory.trim();
    match (cpus.is_empty(), memory.is_empty()) {
        (true, true) => "none".into(),
        (false, true) => format!("{cpus} CPUs"),
        (true, false) => format!("{memory} memory"),
        (false, false) => format!("{cpus} CPUs, {memory} memory"),
    }
}

struct Pod {
    name: String,
    status: String,
    containers: String,
    running: bool,
}

/// Rows of `podman pod ps --format '{{.Name}} {{.Status}} {{.NumberOfContainers}}'`.
fn pods(listing: &str) -> Vec<Pod> {
    listing
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let name = columns.next()?.to_owned();
            let status = columns.next().unwrap_or("unknown").to_owned();
            let count = columns.next().unwrap_or("0");
            Some(Pod {
                running: status.eq_ignore_ascii_case("running"),
                containers: format!("{count} container{}", if count == "1" { "" } else { "s" }),
                name,
                status,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agents::Launch, process::ChildGroup};
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        process::Stdio,
        time::Instant,
    };

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        replies: Mutex<VecDeque<(i32, &'static str)>>,
    }
    impl Fake {
        fn replying(replies: impl IntoIterator<Item = (i32, &'static str)>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::default(),
                replies: Mutex::new(replies.into_iter().collect()),
            })
        }
        fn calls(&self) -> Result<Vec<String>> {
            Ok(self
                .calls
                .lock()
                .map_err(|_| anyhow::anyhow!("Fake lock failed"))?
                .clone())
        }
    }
    impl Runner for Fake {
        fn podman(&self, args: &[OsString]) -> Result<process::Output> {
            self.calls
                .lock()
                .map_err(|_| anyhow::anyhow!("Fake lock failed"))?
                .push(
                    args.iter()
                        .map(|arg| arg.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            let (code, stderr) = self
                .replies
                .lock()
                .map_err(|_| anyhow::anyhow!("Fake lock failed"))?
                .pop_front()
                .unwrap_or((0, ""));
            Ok(process::Output {
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
                code,
            })
        }
    }

    fn sandbox() -> Sandbox {
        Sandbox {
            session: "1791-abc".into(),
            pod: "difu-1791-abc".into(),
            image: SandboxSettings::DEFAULT_IMAGE.into(),
            cpus: String::new(),
            memory: String::new(),
            home: "/home/difu".into(),
            workspace: "/srv/worktrees/1791-abc".into(),
            attachments: None,
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn coding(id: &str, workspace: Option<&str>) -> Session {
        let mut session = Session::new(
            id.into(),
            Job::Coding(Launch {
                repository: "/srv/repos/owner/name".into(),
                isolated: true,
                base: "main".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.workspace = workspace.map(PathBuf::from);
        session
    }

    fn server(enabled: bool, sandbox: bool) -> ServerSettings {
        ServerSettings {
            enabled,
            sandbox: SandboxSettings {
                enabled: sandbox,
                cpus: " 4 ".into(),
                memory: "8g".into(),
                ..SandboxSettings::default()
            },
            ..ServerSettings::default()
        }
    }

    #[test]
    fn pod_names_accept_only_podman_safe_session_ids() -> Result<()> {
        assert_eq!(
            pod_name("1791875325706-02acb407")?,
            "difu-1791875325706-02acb407"
        );
        assert_eq!(pod_name("orchestrator")?, "difu-orchestrator");
        for invalid in [
            "",
            "-leading",
            "../escape",
            "has space",
            "a/b",
            "semi;colon",
        ] {
            ensure!(pod_name(invalid).is_err(), "{invalid:?} was accepted");
        }
        ensure!(pod_name(&"a".repeat(129)).is_err(), "Overlong ID accepted");
        Ok(())
    }

    #[test]
    fn only_server_mode_coding_sessions_with_sandboxing_enabled_are_sandboxed() -> Result<()> {
        let home = Some(Path::new("/home/difu"));
        let session = coding("1791-abc", Some("/srv/worktrees/1791-abc"));
        for (enabled, sandboxing) in [(false, false), (false, true), (true, false)] {
            ensure!(
                Sandbox::decide(&server(enabled, sandboxing), &session, home)?.is_none(),
                "Sandboxed with server {enabled} and sandbox {sandboxing}"
            );
        }
        let decided =
            Sandbox::decide(&server(true, true), &session, home)?.context("Expected a sandbox")?;
        assert_eq!(
            decided,
            Sandbox {
                cpus: "4".into(),
                memory: "8g".into(),
                ..sandbox()
            }
        );
        // Before a worktree exists, spawn sites run in the repository.
        let unprepared = Sandbox::decide(&server(true, true), &coding("1791-abc", None), home)?
            .context("Expected a sandbox")?;
        assert_eq!(unprepared.workspace, Path::new("/srv/repos/owner/name"));
        let review = Session::new(
            "review".into(),
            Job::Conflict {
                root: "/srv/repos/owner/name".into(),
                key: crate::model::PrKey {
                    owner: "owner".into(),
                    repo: "name".into(),
                    number: 1,
                },
                head: "head".into(),
                model: Default::default(),
            },
        );
        ensure!(Sandbox::decide(&server(true, true), &review, home)?.is_none());
        ensure!(Sandbox::decide(&server(true, true), &session, None).is_err());
        ensure!(
            Sandbox::decide(&server(true, true), &coding("bad id", None), home).is_err(),
            "An unusable session ID must not fall back to running unsandboxed"
        );
        Ok(())
    }

    #[test]
    fn sandboxing_without_podman_is_an_error_not_a_direct_run() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let session = coding("1791-abc", Some("/srv/worktrees/1791-abc"));
        ensure!(Sandbox::for_session(&storage, &session)?.is_none());
        storage.update_config(|config| config.server = server(true, true))?;
        if !installed(PODMAN) {
            let error = Sandbox::for_session(&storage, &session)
                .err()
                .context("Expected a missing Podman error")?;
            ensure!(error.to_string().contains("Podman is not installed"));
        }
        Ok(())
    }

    #[test]
    fn agent_invocation_wraps_the_command_with_mounts_and_environment() -> Result<()> {
        let sandbox = sandbox();
        let mut command = Command::new("claude");
        command
            .args(["--print", "--model=opus"])
            .current_dir("/srv/worktrees/1791-abc")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "secret-token")
            .env("HOME", "/root")
            .env("LANG", "en_US.UTF-8")
            .env("XDG_CONFIG_HOME", "/home/difu/.config")
            .env_remove("RUST_LOG");
        let mounts = [
            Mount::writable(Path::new("/srv/worktrees/1791-abc")),
            Mount::writable(Path::new("/srv/repos/owner/name/.git")),
            Mount::writable(Path::new("/home/difu/.codex")),
            Mount {
                path: "/home/difu/.local/bin".into(),
                read_only: true,
            },
        ];
        let invocation = sandbox.invocation(
            Role::Claude,
            "difu-1791-abc-claude-1-0",
            &Program::from_command(&command),
            false,
            &mounts,
        )?;
        assert_eq!(
            strings(&invocation.args),
            [
                "run",
                "--rm",
                "-i",
                "--pod",
                "difu-1791-abc",
                "--name",
                "difu-1791-abc-claude-1-0",
                "--label",
                "difu.session=1791-abc",
                "--label",
                "difu.role=claude",
                "--user",
                "0:0",
                "--log-driver",
                "none",
                "--detach-keys=",
                "-w",
                "/srv/worktrees/1791-abc",
                "-v",
                "/srv/worktrees/1791-abc:/srv/worktrees/1791-abc",
                "-v",
                "/srv/repos/owner/name/.git:/srv/repos/owner/name/.git",
                "-v",
                "/home/difu/.codex:/home/difu/.codex",
                "-v",
                "/home/difu/.local/bin:/home/difu/.local/bin:ro",
                "-e",
                "HOME=/home/difu",
                "-e",
                "PATH=/home/difu/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "-e",
                "SHELL=/bin/bash",
                "-e",
                "DISABLE_AUTOUPDATER=1",
                "-e",
                "IS_SANDBOX=1",
                "-e",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "-e",
                "LANG",
                "-e",
                "XDG_CONFIG_HOME=/home/difu/.config",
                "localhost/difu-sandbox:latest",
                "claude",
                "--print",
                "--model=opus",
            ]
        );
        // Values reach the container through Podman's environment, never argv.
        assert_eq!(
            invocation.env,
            [
                ("CLAUDE_CODE_OAUTH_TOKEN".into(), "secret-token".into()),
                ("LANG".into(), "en_US.UTF-8".into()),
            ]
        );
        Ok(())
    }

    #[test]
    fn shell_invocation_allocates_a_terminal_and_a_login_shell() -> Result<()> {
        let sandbox = sandbox();
        let mut builder = CommandBuilder::new_default_prog();
        builder.cwd("/srv/worktrees/1791-abc/sub");
        builder.env("TERM", "xterm-256color");
        let program = Program::from_pty(&builder);
        assert_eq!(program.program, "/bin/bash");
        let invocation =
            sandbox.invocation(Role::Shell, "difu-1791-abc-shell-1-0", &program, true, &[])?;
        let args = strings(&invocation.args);
        ensure!(
            args.starts_with(&["run", "--rm", "-i", "-t"].map(String::from)),
            "{args:?}"
        );
        ensure!(args.contains(&"difu.role=shell".to_owned()));
        ensure!(
            args.windows(2)
                .any(|pair| pair == ["-w", "/srv/worktrees/1791-abc/sub"]),
            "{args:?}"
        );
        ensure!(
            args.ends_with(&["localhost/difu-sandbox:latest", "/bin/bash", "-l"].map(String::from)),
            "{args:?}"
        );
        assert_eq!(invocation.env, [("TERM".into(), "xterm-256color".into())]);
        let mut command = CommandBuilder::new("/bin/bash");
        command.args(["-lc", "cargo test"]);
        let program = Program::from_pty(&command);
        assert_eq!(program.args, ["-lc", "cargo test"]);
        // Without a working directory the process starts in the workspace.
        let invocation = sandbox.invocation(Role::Shell, "name", &program, true, &[])?;
        ensure!(
            strings(&invocation.args)
                .windows(2)
                .any(|pair| pair == ["-w", "/srv/worktrees/1791-abc"])
        );
        Ok(())
    }

    #[test]
    fn mount_paths_must_be_absolute_and_free_of_colons() -> Result<()> {
        ensure!(Mount::writable(Path::new("relative")).volume().is_err());
        ensure!(Mount::writable(Path::new("/a:b")).volume().is_err());
        assert_eq!(
            Mount {
                path: "/a b".into(),
                read_only: true
            }
            .volume()?,
            "/a b:/a b:ro"
        );
        Ok(())
    }

    #[test]
    fn limits_apply_to_the_session_pod() {
        assert_eq!(
            strings(&sandbox().pod_args()),
            [
                "pod",
                "create",
                "--name",
                "difu-1791-abc",
                "--label",
                "difu.session=1791-abc",
                "--share",
                "ipc,net,uts,pid",
            ]
        );
        let limited = Sandbox {
            cpus: "2.5".into(),
            memory: "8g".into(),
            ..sandbox()
        };
        let args = strings(&limited.pod_args());
        assert_eq!(
            args.get(8..),
            Some(&["--cpus", "2.5", "--memory", "8g"].map(String::from)[..])
        );
    }

    #[test]
    fn mounts_cover_the_worktree_its_git_directory_logins_and_binaries() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repository = root.join("repos/owner/name");
        let worktree = root.join("worktrees/1791-abc");
        fs::create_dir_all(&repository)?;
        let git = |directory: &Path, args: &[&str]| -> Result<()> {
            let output = process::run(
                Command::new("git").arg("-C").arg(directory).args(args),
                None,
                &Cancel::default(),
            )?;
            ensure!(
                output.code == 0,
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        };
        git(&repository, &["init", "-q", "-b", "main"])?;
        git(
            &repository,
            &[
                "-c",
                "user.name=difu",
                "-c",
                "user.email=difu@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        )?;
        let path = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &["worktree", "add", "-q", "-b", "agent", &path],
        )?;
        let home = root.join("home");
        fs::create_dir_all(home.join(".codex"))?;
        fs::create_dir_all(home.join(".local/bin"))?;
        fs::write(home.join(".claude.json"), "{}")?;
        fs::write(home.join(".gitconfig"), "")?;
        let attachments = root.join("difu/agents/attachments/1791-abc");
        let sandbox = Sandbox {
            home: home.clone(),
            workspace: worktree.clone(),
            attachments: Some(attachments.clone()),
            ..sandbox()
        };
        let writable = |path: PathBuf| Mount {
            path,
            read_only: false,
        };
        let read_only = |path: PathBuf| Mount {
            path,
            read_only: true,
        };
        let expected = [
            writable(worktree.clone()),
            writable(repository.join(".git")),
            writable(home.join(".codex")),
            writable(home.join(".claude.json")),
            read_only(home.join(".gitconfig")),
            read_only(home.join(".local/bin")),
        ];
        // A missing attachment directory is not mounted (Podman would fail).
        assert_eq!(sandbox.mounts(&worktree), expected);
        // Media sent to the session is readable inside, but cannot be changed.
        fs::create_dir_all(&attachments)?;
        let mounts = sandbox.mounts(&worktree);
        assert_eq!(mounts.get(..expected.len()), Some(expected.as_slice()));
        assert_eq!(mounts.last(), Some(&read_only(attachments.clone())));
        assert_eq!(mounts.len(), expected.len() + 1);
        // In the base clone itself, its `.git` is already inside the mount.
        let base = Sandbox {
            workspace: repository.clone(),
            ..sandbox.clone()
        };
        let mounts = base.mounts(&repository);
        assert_eq!(mounts.first(), Some(&writable(repository.clone())));
        ensure!(!mounts.contains(&writable(repository.join(".git"))));
        Ok(())
    }

    #[test]
    fn arranged_mounts_drop_duplicates_and_covered_paths_of_the_same_access() {
        let mount = |path: &str, read_only| Mount {
            path: path.into(),
            read_only,
        };
        assert_eq!(
            arrange(vec![
                mount("/w/sub", false),
                mount("/w", false),
                mount("/w", false),
                mount("/w/.git", false),
                mount("/w/bin", true),
                mount("/home/.local/bin", true),
            ]),
            [
                mount("/w", false),
                mount("/w/bin", true),
                mount("/home/.local/bin", true),
            ]
        );
    }

    #[test]
    fn pods_are_created_once_started_and_a_missing_local_image_is_explained() -> Result<()> {
        let sandbox = sandbox();
        // Image present, pod missing, created, started.
        let fake = Fake::replying([(0, ""), (1, ""), (0, ""), (0, "")]);
        sandbox.ensure_pod(fake.as_ref())?;
        assert_eq!(
            fake.calls()?,
            [
                "image exists localhost/difu-sandbox:latest",
                "pod exists difu-1791-abc",
                "pod create --name difu-1791-abc --label difu.session=1791-abc --share ipc,net,uts,pid",
                "pod start difu-1791-abc",
            ]
        );
        // Existing pods are reused (and restarted if they stopped).
        let fake = Fake::replying([(0, ""), (0, ""), (0, "")]);
        sandbox.ensure_pod(fake.as_ref())?;
        assert_eq!(
            fake.calls()?.get(1..),
            Some(&["pod exists difu-1791-abc", "pod start difu-1791-abc"].map(String::from)[..])
        );
        // A pod created concurrently elsewhere is fine.
        let fake = Fake::replying([(0, ""), (1, ""), (125, "pod already exists"), (0, "")]);
        sandbox.ensure_pod(fake.as_ref())?;
        // Real failures carry Podman's message.
        let fake = Fake::replying([
            (0, ""),
            (1, ""),
            (125, "cgroup limits unsupported"),
            (1, ""),
        ]);
        let error = sandbox
            .ensure_pod(fake.as_ref())
            .err()
            .context("Expected a pod creation error")?;
        ensure!(error.to_string().contains("cgroup limits unsupported"));
        let fake = Fake::replying([(0, ""), (0, ""), (125, "no network")]);
        let error = sandbox
            .ensure_pod(fake.as_ref())
            .err()
            .context("Expected a pod start error")?;
        ensure!(error.to_string().contains("no network"));
        let fake = Fake::replying([(1, "")]);
        let error = sandbox
            .ensure_pod(fake.as_ref())
            .err()
            .context("Expected a missing image error")?;
        ensure!(error.to_string().contains("difu sandbox build"));
        // Podman pulls remote images on first use.
        let remote = Sandbox {
            image: "ghcr.io/owner/image:1".into(),
            ..sandbox
        };
        let fake = Fake::replying([]);
        remote.ensure_pod(fake.as_ref())?;
        assert_eq!(
            fake.calls()?,
            ["pod exists difu-1791-abc", "pod start difu-1791-abc"]
        );
        Ok(())
    }

    #[test]
    fn the_guard_removes_its_container_once_after_the_process() -> Result<()> {
        let fake = Fake::replying([]);
        let command = Command::new("codex");
        let (wrapped, container) = sandbox().command_with(fake.clone(), Role::Codex, &command)?;
        assert_eq!(wrapped.get_program(), PODMAN);
        let name = container.name().to_owned();
        ensure!(name.starts_with("difu-1791-abc-codex-"), "{name}");
        ensure!(
            wrapped.get_args().any(|arg| arg == name.as_str()),
            "The container is named after its guard"
        );
        ensure!(
            !fake.calls()?.iter().any(|call| call.starts_with("rm ")),
            "Nothing is removed while the process may run"
        );
        drop(container);
        assert_eq!(fake.calls()?.last(), Some(&format!("rm -f -t 0 {name}")));
        let (_, mut container) = sandbox().command_with(fake.clone(), Role::Codex, &command)?;
        container.remove();
        container.remove();
        drop(container);
        let (_, mut container) = sandbox().command_with(fake.clone(), Role::Codex, &command)?;
        container.exited();
        drop(container);
        let removals = fake
            .calls()?
            .iter()
            .filter(|call| call.starts_with("rm "))
            .count();
        assert_eq!(removals, 2);
        Ok(())
    }

    #[test]
    fn container_names_are_unique() {
        let first = container_name("difu-1", Role::Shell);
        let second = container_name("difu-1", Role::Shell);
        assert_ne!(first, second);
        assert!(first.starts_with("difu-1-shell-"));
    }

    #[test]
    fn pod_removal_ignores_missing_pods_and_reports_other_failures() -> Result<()> {
        let fake = Fake::replying([(1, "Error: no pod with name or ID difu-1 found: no such pod")]);
        remove_pod_with(fake.as_ref(), "1")?;
        assert_eq!(fake.calls()?, ["pod rm -f -t 0 difu-1"]);
        let fake = Fake::replying([(125, "Error: cannot connect")]);
        ensure!(remove_pod_with(fake.as_ref(), "1").is_err());
        let fake = Fake::replying([]);
        remove_pod_with(fake.as_ref(), "../x")?;
        ensure!(fake.calls()?.is_empty());
        Ok(())
    }

    #[test]
    fn sweep_lists_agent_and_shell_containers_by_role_label() -> Result<()> {
        let fake = Fake::replying([]);
        sweep_with(fake.as_ref())?;
        assert_eq!(
            fake.calls()?,
            ["ps -a --filter label=difu.role --format {{.Names}}"]
        );
        Ok(())
    }

    #[test]
    fn executables_are_found_on_the_search_path() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin)?;
        fs::write(bin.join("podman"), "#!/bin/sh\n")?;
        fs::write(bin.join("plain"), "")?;
        fs::set_permissions(bin.join("podman"), fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([temp.path().join("missing"), bin.clone()])?;
        assert_eq!(search("podman", Some(&path)), Some(bin.join("podman")));
        assert_eq!(search("plain", Some(&path)), None);
        assert_eq!(search("podman", None), None);
        Ok(())
    }

    #[test]
    fn status_describes_mode_limits_and_pods() {
        assert_eq!(
            mode(&server(false, false)),
            "off (agents run directly on this machine)"
        );
        assert!(mode(&server(true, true)).starts_with("on"));
        assert_eq!(limits(&SandboxSettings::default()), "none");
        assert_eq!(limits(&server(true, true).sandbox), "4 CPUs, 8g memory");
        let listed = pods("difu-1 Running 3\ndifu-2 Exited 1\n\n");
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .first()
                .is_some_and(|pod| pod.running && pod.containers == "3 containers")
        );
        assert!(
            listed
                .get(1)
                .is_some_and(|pod| !pod.running && pod.containers == "1 container")
        );
    }

    #[test]
    fn build_writes_the_bundled_containerfile_and_tags_the_default_image() {
        assert!(CONTAINERFILE.contains("FROM docker.io/library/ubuntu:24.04"));
        for package in [
            "git",
            "gh",
            "curl",
            "ca-certificates",
            "build-essential",
            "pkg-config",
            "libssl-dev",
            "python3",
            "python3-pip",
            "python3-venv",
            "nodejs",
            "npm",
            "ripgrep",
            "jq",
            "unzip",
        ] {
            assert!(
                CONTAINERFILE
                    .lines()
                    .any(|line| line.trim().trim_end_matches('\\').trim() == package),
                "{package} is missing from the Containerfile"
            );
        }
        assert_eq!(
            strings(&build_args(
                Path::new("/tmp/x/Containerfile"),
                Path::new("/tmp/x")
            )),
            [
                "build",
                "-t",
                "localhost/difu-sandbox:latest",
                "-f",
                "/tmp/x/Containerfile",
                "/tmp/x",
            ]
        );
    }

    /// Runs only where rootless Podman works (Linux CI); skips elsewhere.
    #[test]
    fn podman_runs_session_processes_in_the_pod_and_removes_them() -> Result<()> {
        const IMAGE: &str = "quay.io/libpod/alpine:latest";
        let podman = Podman;
        let available = installed(PODMAN)
            && podman
                .podman(&os(["info"]))
                .is_ok_and(|output| output.code == 0);
        if !available {
            eprintln!("Skipping the Podman sandbox test: Podman is not available");
            return Ok(());
        }
        let succeeds = |args: &[&str]| {
            podman
                .podman(&os(args.iter().copied()))
                .is_ok_and(|output| output.code == 0)
        };
        if !succeeds(&["image", "exists", IMAGE]) && !succeeds(&["pull", "-q", IMAGE]) {
            eprintln!("Skipping the Podman sandbox test: cannot pull {IMAGE}");
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().canonicalize()?.join("workspace");
        let home = temp.path().canonicalize()?.join("home");
        let attachments = temp.path().canonicalize()?.join("attachments");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&attachments)?;
        fs::write(attachments.join("media-1.png"), "uploaded")?;
        let session = format!(
            "test-{}-{:x}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        );
        let sandbox = Sandbox {
            pod: pod_name(&session)?,
            session: session.clone(),
            image: IMAGE.into(),
            cpus: String::new(),
            memory: String::new(),
            home: home.clone(),
            workspace: workspace.clone(),
            attachments: Some(attachments.clone()),
        };
        let exists = |kind: &str, name: &str| -> Result<bool> {
            Ok(podman.podman(&os([kind, "exists", name]))?.code == 0)
        };
        let result = (|| -> Result<()> {
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    "pwd; echo \"$HOME $DIFU_SECRET\"; cat \"$1/media-1.png\"; echo; touch \"$1/new\" 2>/dev/null || echo read-only; cat > out.txt",
                    "sh",
                ])
                .arg(&attachments)
                .current_dir(&workspace)
                .env("DIFU_SECRET", "forwarded");
            let (mut wrapped, container) = sandbox.command(Role::Shell, &command)?;
            wrapped
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut group = ChildGroup::spawn_tree(&mut wrapped)?;
            let mut input = group.child.stdin.take().context("Missing stdin")?;
            input.write_all(b"through the mount")?;
            drop(input);
            let mut output = String::new();
            group
                .child
                .stdout
                .take()
                .context("Missing stdout")?
                .read_to_string(&mut output)?;
            let status = group.child.wait()?;
            let mut errors = String::new();
            if let Some(mut stderr) = group.child.stderr.take() {
                stderr.read_to_string(&mut errors)?;
            }
            ensure!(status.success(), "podman run failed: {errors}");
            assert_eq!(
                output,
                format!(
                    "{}\n{} forwarded\nuploaded\nread-only\n",
                    workspace.display(),
                    home.display()
                )
            );
            assert_eq!(
                fs::read_to_string(workspace.join("out.txt"))?,
                "through the mount"
            );
            drop(group);
            drop(container);
            // A process difu kills with SIGKILL leaves no container behind.
            let mut sleeper = Command::new("sleep");
            sleeper.arg("600").current_dir(&workspace);
            let (mut wrapped, container) = sandbox.command(Role::Claude, &sleeper)?;
            let name = container.name().to_owned();
            wrapped
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut group = ChildGroup::spawn_tree(&mut wrapped)?;
            let started = Instant::now();
            while !exists("container", &name)? {
                ensure!(
                    started.elapsed() < Duration::from_secs(60),
                    "The container never started"
                );
                thread::sleep(Duration::from_millis(200));
            }
            group.stop()?;
            drop(container);
            ensure!(!exists("container", &name)?, "The guard left {name} behind");
            Ok(())
        })();
        remove_pod_with(&podman, &session)?;
        ensure!(!exists("pod", &sandbox.pod)?, "The pod was not removed");
        result
    }
}
