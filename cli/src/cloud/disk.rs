//! Disk space on a difu server: how full the filesystem holding difu's agent
//! data is, what uses it, and explicit cleanups. Clients read it with
//! `server.disk` and clean with `server.disk.clean`; see
//! docs/cloud-agents.md#disk-space.
//!
//! Free space comes from `statvfs` on every request. The breakdown walks big
//! trees, so a background thread measures it every few minutes (and after
//! cleanups) and requests only read the last measurement.
use super::cli::DiskCommand;
use crate::{
    agents::{Reply, Request, sandbox::Runner},
    process::{self, Cancel},
    repo,
    storage::Storage,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

/// Decimal units, like disk vendors and `df -H`.
pub const GB: u64 = 1_000_000_000;
/// Free space below either bound makes a server `low`.
pub const LOW_PERCENT: u64 = 15;
pub const LOW_BYTES: u64 = 20 * GB;
/// Free space below either bound makes a server `critical`: it then refuses
/// new sessions, moves and repository clones until space is freed.
pub const CRITICAL_PERCENT: u64 = 5;
pub const CRITICAL_BYTES: u64 = 5 * GB;
/// The background measurement repeats this often.
pub const REFRESH: Duration = Duration::from_secs(5 * 60);
/// Rebuildable directories a `buildOutputs` cleanup deletes, only where Git
/// ignores them and they hold no tracked file.
pub const BUILD_OUTPUTS: [&str; 9] = [
    "target",
    "node_modules",
    ".next",
    ".turbo",
    "dist",
    "build",
    ".venv",
    "__pycache__",
    ".gradle",
];
/// Paths per `git ls-files` call, well below any argument limit.
const BATCH: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Ok,
    Low,
    Critical,
}

impl Status {
    pub fn of(total: u64, free: u64) -> Self {
        let below = |percent: u64, bytes: u64| {
            free < bytes || u128::from(free) * 100 < u128::from(total) * u128::from(percent)
        };
        if below(CRITICAL_PERCENT, CRITICAL_BYTES) {
            Self::Critical
        } else if below(LOW_PERCENT, LOW_BYTES) {
            Self::Low
        } else {
            Self::Ok
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Low => "low",
            Self::Critical => "critical",
        }
    }
}

/// A filesystem's size in bytes; `free` is what difu may still use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

impl Space {
    pub fn status(&self) -> Status {
        Status::of(self.total, self.free)
    }
    /// Whole percent of the filesystem still free.
    pub fn free_percent(&self) -> u64 {
        if self.total == 0 {
            return 0;
        }
        u64::try_from(u128::from(self.free) * 100 / u128::from(self.total)).unwrap_or(100)
    }
}

/// The filesystem holding `path`.
pub fn space(path: &Path) -> Result<Space> {
    #[cfg(test)]
    if let Some(space) = tests::pretended(path) {
        return Ok(space);
    }
    let stat = nix::sys::statvfs::statvfs(path)
        .with_context(|| format!("Cannot read disk space for {}", path.display()))?;
    // The field types differ between Linux and macOS.
    fn wide(value: impl Into<u64>) -> u64 {
        value.into()
    }
    let unit = wide(stat.fragment_size());
    let blocks = wide(stat.blocks());
    Ok(Space {
        total: blocks.saturating_mul(unit),
        used: blocks
            .saturating_sub(wide(stat.blocks_free()))
            .saturating_mul(unit),
        free: wide(stat.blocks_available()).saturating_mul(unit),
    })
}

/// One session's worktree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionUsage {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub active: bool,
    pub bytes: u64,
    /// The part a `buildOutputs` cleanup could clear.
    pub build_outputs: u64,
}

/// Rootless Podman's storage (`podman system df`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SandboxUsage {
    /// Podman is installed.
    pub available: bool,
    pub images: u64,
    pub containers: u64,
    pub volumes: u64,
    /// Unused images and stopped containers, as Podman counts them.
    pub reclaimable: u64,
}

impl SandboxUsage {
    pub fn total(&self) -> u64 {
        self.images
            .saturating_add(self.containers)
            .saturating_add(self.volumes)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Breakdown {
    /// Biggest first.
    pub sessions: Vec<SessionUsage>,
    /// Base clones under `<data>/repos`.
    pub repositories: u64,
    /// Pasted images and videos under `<data>/attachments`.
    pub attachments: u64,
    /// difu's cache directory (PR cache, images, artifact copies).
    pub caches: u64,
    pub sandbox: SandboxUsage,
    /// Everything else on the filesystem: the system, logs, CLIs, other users.
    pub other: u64,
}

/// `server.disk`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Report {
    /// difu's agent data directory; the numbers are for its filesystem.
    pub path: PathBuf,
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub status: Status,
    /// When the breakdown was measured (Unix milliseconds); `None` before the
    /// first measurement finishes.
    pub measured_at: Option<i64>,
    /// A measurement is running.
    pub measuring: bool,
    pub breakdown: Breakdown,
}

impl Report {
    pub fn space(&self) -> Space {
        Space {
            total: self.total,
            used: self.used,
            free: self.free,
        }
    }
    /// Archived, inactive sessions that still have a worktree, biggest first.
    pub fn archived(&self) -> impl Iterator<Item = &SessionUsage> {
        self.breakdown
            .sessions
            .iter()
            .filter(|session| session.archived && !session.active && session.bytes > 0)
    }
    /// Inactive sessions with build outputs to clear, biggest first.
    pub fn build_outputs(&self) -> impl Iterator<Item = &SessionUsage> {
        self.breakdown
            .sessions
            .iter()
            .filter(|session| !session.active && session.build_outputs > 0)
    }
    /// What `action` should free, from the last measurement.
    pub fn expected(&self, action: Action) -> u64 {
        match action {
            Action::ArchivedWorktrees => self.archived().map(|s| s.bytes).sum(),
            Action::BuildOutputs => self.build_outputs().map(|s| s.build_outputs).sum(),
            Action::Sandbox => self.breakdown.sandbox.reclaimable,
        }
    }
}

/// An explicit cleanup (`server.disk.clean`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    /// Remove the worktrees of archived sessions; their chats stay.
    ArchivedWorktrees,
    /// Delete ignored build directories in idle sessions.
    BuildOutputs,
    /// Prune Podman and remove unused images except the sandbox image.
    Sandbox,
}

impl Action {
    pub const ALL: [Self; 3] = [Self::ArchivedWorktrees, Self::BuildOutputs, Self::Sandbox];
    pub fn label(self) -> &'static str {
        match self {
            Self::ArchivedWorktrees => "Clean archived worktrees",
            Self::BuildOutputs => "Clear build outputs",
            Self::Sandbox => "Prune sandbox images",
        }
    }
    /// What the cleanup removes and keeps, for confirmations.
    pub fn describe(self) -> String {
        match self {
            Self::ArchivedWorktrees => "Removes the worktrees of archived sessions. Their chats, branches and commits stay. Build outputs in them are cleared first; worktrees with other uncommitted, untracked or ignored files are kept. Active sessions are never touched.".into(),
            Self::BuildOutputs => format!(
                "Deletes rebuildable build folders that Git ignores and that hold no tracked file ({}) in idle sessions. Running sessions are skipped.",
                BUILD_OUTPUTS.join(", ")
            ),
            Self::Sandbox => "Runs `podman system prune -f` (stopped containers, dangling images, build cache) and removes unused images, keeping the configured sandbox image.".into(),
        }
    }
    /// The `difu disk clean` argument.
    pub fn argument(self) -> &'static str {
        match self {
            Self::ArchivedWorktrees => "archived-worktrees",
            Self::BuildOutputs => "build-outputs",
            Self::Sandbox => "sandbox",
        }
    }
}

/// A cleanup's outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cleaned {
    pub freed: u64,
    pub summary: String,
}

/// `1.2 GB`, in decimal units.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {name}")
    } else {
        format!("{value:.1} {name}")
    }
}

/// Why new work is refused on a critically full server.
pub fn refusal(space: &Space) -> String {
    format!(
        "Only {} is free on this server ({}% of {}), so difu is not starting new sessions, moves or clones. Free up space in Cmd+K → /cloud or with `difu disk clean` on the server; running sessions continue.",
        human(space.free),
        space.free_percent(),
        human(space.total)
    )
}

/// Refuses new work that needs disk space while a server is critically full.
/// Ordinary installations are never refused.
pub fn guard(storage: &Storage) -> Result<()> {
    if !server_mode(storage) {
        return Ok(());
    }
    let Ok(path) = crate::agents::server::home(storage) else {
        return Ok(());
    };
    // Tests enforce the guard only for a pretended disk, so a full disk on the
    // machine running them cannot fail unrelated tests.
    #[cfg(test)]
    if tests::pretended(&path).is_none() {
        return Ok(());
    }
    match space(&path) {
        Ok(space) => {
            ensure!(space.status() != Status::Critical, "{}", refusal(&space));
            Ok(())
        }
        // Unknown space never blocks work.
        Err(_) => Ok(()),
    }
}

/// `config.server.enabled`, read without the terminal's migration write.
pub fn server_mode(storage: &Storage) -> bool {
    fs::read(&storage.config)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|config| config.pointer("/server/enabled").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn os<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<OsString> {
    items.into_iter().map(OsString::from).collect()
}

/// Sizes trees like `du -x`: allocated blocks, symlinks never followed, other
/// filesystems skipped, hard links counted once per measurement.
struct Walker {
    device: Option<u64>,
    seen: HashSet<(u64, u64)>,
}

impl Walker {
    fn new(device: Option<u64>) -> Self {
        Self {
            device,
            seen: HashSet::new(),
        }
    }
    fn foreign(&self, meta: &fs::Metadata) -> bool {
        self.device.is_some_and(|device| device != meta.dev())
    }
    fn entry(&mut self, meta: &fs::Metadata) -> u64 {
        if meta.nlink() > 1 && !meta.is_dir() && !self.seen.insert((meta.dev(), meta.ino())) {
            return 0;
        }
        meta.blocks().saturating_mul(512)
    }
    /// `root` and everything under it; missing or unreadable parts count as
    /// nothing.
    fn usage(&mut self, root: &Path) -> u64 {
        let Ok(meta) = fs::symlink_metadata(root) else {
            return 0;
        };
        if self.foreign(&meta) {
            return 0;
        }
        let mut total = self.entry(&meta);
        if !meta.is_dir() {
            return total;
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                // Like lstat: a symlink is the link, never its target.
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if self.foreign(&meta) {
                    continue;
                }
                total = total.saturating_add(self.entry(&meta));
                if meta.is_dir() {
                    stack.push(entry.path());
                }
            }
        }
        total
    }
    /// A worktree's size and its outermost directories named like build
    /// outputs, each with its size. Symlinked directories are never entered,
    /// so every candidate is inside the worktree.
    fn worktree(&mut self, root: &Path) -> (u64, Vec<(PathBuf, u64)>) {
        let mut candidates = Vec::new();
        let Ok(meta) = fs::symlink_metadata(root) else {
            return (0, candidates);
        };
        if self.foreign(&meta) {
            return (0, candidates);
        }
        let mut total = self.entry(&meta);
        if !meta.is_dir() {
            return (total, candidates);
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if self.foreign(&meta) {
                    continue;
                }
                let path = entry.path();
                let name = entry.file_name();
                if meta.is_dir()
                    && name != ".git"
                    && BUILD_OUTPUTS.iter().any(|output| name == *output)
                {
                    let size = self.usage(&path);
                    total = total.saturating_add(size);
                    candidates.push((path, size));
                    continue;
                }
                total = total.saturating_add(self.entry(&meta));
                if meta.is_dir() {
                    stack.push(path);
                }
            }
        }
        (total, candidates)
    }
}

/// The size of `path` and everything under it, on its own filesystem.
pub fn usage(path: &Path) -> u64 {
    let device = fs::symlink_metadata(path).ok().map(|meta| meta.dev());
    Walker::new(device).usage(path)
}

/// Of `candidates` inside `worktree`, those Git ignores and that hold no
/// tracked file (a file force-added under an ignored directory is tracked).
fn ignored(
    worktree: &Path,
    candidates: Vec<(PathBuf, u64)>,
    cancel: &Cancel,
) -> Result<Vec<(PathBuf, u64)>> {
    let relative: Vec<(String, PathBuf, u64)> = candidates
        .into_iter()
        .filter_map(|(path, size)| {
            let relative = path.strip_prefix(worktree).ok()?.to_str()?.to_owned();
            // Git separates paths with NUL; anything else odd stays.
            (!relative.is_empty() && !relative.contains('\0')).then_some((relative, path, size))
        })
        .collect();
    if relative.is_empty() {
        return Ok(Vec::new());
    }
    let input: String = relative
        .iter()
        .map(|(path, ..)| format!("{path}\0"))
        .collect();
    let checked = process::run(
        repo::git(worktree).args(["check-ignore", "-z", "--stdin"]),
        Some(input.into_bytes()),
        cancel,
    )?;
    // 1 means nothing is ignored.
    ensure!(
        checked.code <= 1,
        "git check-ignore failed in {}: {}",
        worktree.display(),
        String::from_utf8_lossy(&checked.stderr).trim()
    );
    let ignored: HashSet<String> = checked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8_lossy(path)
                .trim_end_matches('/')
                .to_owned()
        })
        .collect();
    let kept: Vec<(String, PathBuf, u64)> = relative
        .into_iter()
        .filter(|(path, ..)| ignored.contains(path))
        .collect();
    let mut tracked = Vec::new();
    for batch in kept.chunks(BATCH) {
        let listed = process::run(
            repo::git(worktree)
                .env("GIT_LITERAL_PATHSPECS", "1")
                .args(["ls-files", "-z", "--"])
                .args(batch.iter().map(|(path, ..)| path)),
            None,
            cancel,
        )?;
        ensure!(
            listed.code == 0,
            "git ls-files failed in {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&listed.stderr).trim()
        );
        tracked.extend(
            listed
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| String::from_utf8_lossy(path).into_owned()),
        );
    }
    Ok(kept
        .into_iter()
        .filter(|(path, ..)| {
            let inside = format!("{path}/");
            !tracked
                .iter()
                .any(|file| file == path || file.starts_with(&inside))
        })
        .map(|(_, path, size)| (path, size))
        .collect())
}

/// The build output directories a `buildOutputs` cleanup would delete in a
/// worktree, with their sizes.
pub(crate) fn build_outputs(worktree: &Path, cancel: &Cancel) -> Result<Vec<(PathBuf, u64)>> {
    let meta = fs::symlink_metadata(worktree)
        .with_context(|| format!("{} is missing", worktree.display()))?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "{} is not a worktree directory",
        worktree.display()
    );
    let (_, candidates) = Walker::new(Some(meta.dev())).worktree(worktree);
    ignored(worktree, candidates, cancel)
}

/// Deletes `outputs` (from `build_outputs`), refusing anything that is not a
/// real directory inside `worktree`. Returns the bytes freed and one message
/// per directory left alone.
pub(crate) fn remove_outputs(worktree: &Path, outputs: &[(PathBuf, u64)]) -> (u64, Vec<String>) {
    let root = match worktree.canonicalize() {
        Ok(root) => root,
        Err(error) => return (0, vec![format!("{}: {error}", worktree.display())]),
    };
    let mut freed = 0u64;
    let mut errors = Vec::new();
    for (path, size) in outputs {
        let result = (|| -> Result<()> {
            let relative = path
                .strip_prefix(worktree)
                .context("outside the worktree")?;
            ensure!(
                !relative.as_os_str().is_empty()
                    && relative
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "outside the worktree"
            );
            let meta = fs::symlink_metadata(path)?;
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "not a directory"
            );
            // A symlinked parent would lead outside the worktree.
            ensure!(
                path.canonicalize()? == root.join(relative),
                "leads outside the worktree"
            );
            // Never follows symlinks inside: links are removed, not their targets.
            fs::remove_dir_all(path)?;
            Ok(())
        })();
        match result {
            Ok(()) => freed = freed.saturating_add(*size),
            Err(error) => errors.push(format!("{}: {error:#}", path.display())),
        }
    }
    (freed, errors)
}

/// Parses `podman system df --format json`: an array of `{Type, RawSize,
/// RawReclaimable, Size, Reclaimable}` (or one object per line), sizes as
/// numbers or human strings depending on the version.
fn parse_system_df(text: &str) -> Result<SandboxUsage> {
    let text = text.trim();
    let rows: Vec<Value> = match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(rows)) => rows,
        Ok(row @ Value::Object(_)) => vec![row],
        _ => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()
            .context("Unexpected `podman system df` output")?,
    };
    let mut usage = SandboxUsage {
        available: true,
        ..SandboxUsage::default()
    };
    for row in rows {
        let kind = row
            .get("Type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let size = row_bytes(&row, "RawSize", "Size");
        let reclaimable = row_bytes(&row, "RawReclaimable", "Reclaimable");
        if kind.starts_with("image") {
            usage.images = size;
            usage.reclaimable = usage.reclaimable.saturating_add(reclaimable);
        } else if kind.starts_with("container") {
            usage.containers = size;
            usage.reclaimable = usage.reclaimable.saturating_add(reclaimable);
        } else if kind.contains("volume") {
            // `podman system prune` without `--volumes` keeps volumes.
            usage.volumes = size;
        }
    }
    Ok(usage)
}

fn row_bytes(row: &Value, raw: &str, human: &str) -> u64 {
    match row.get(raw) {
        Some(Value::Number(number)) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|value| value.max(0.0) as u64))
            .unwrap_or(0),
        _ => row
            .get(human)
            .and_then(Value::as_str)
            .and_then(parse_human)
            .unwrap_or(0),
    }
}

/// `1.234GB`, `12.5 kB (50%)`, `0B` → bytes.
fn parse_human(text: &str) -> Option<u64> {
    let token: String = text
        .split('(')
        .next()?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let split = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(token.len());
    let (number, unit) = token.split_at(split);
    let number: f64 = number.parse().ok()?;
    let factor = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * factor).max(0.0) as u64)
}

/// Podman's storage, or nothing when Podman is missing or cannot answer.
fn sandbox_usage(runner: &dyn Runner) -> SandboxUsage {
    if !runner.available() {
        return SandboxUsage::default();
    }
    runner
        .podman(&os(["system", "df", "--format", "json"]))
        .ok()
        .filter(|output| output.code == 0)
        .and_then(|output| parse_system_df(&String::from_utf8_lossy(&output.stdout)).ok())
        .unwrap_or(SandboxUsage {
            available: true,
            ..SandboxUsage::default()
        })
}

/// Whether Podman's storage is on `device`; unknown counts as yes (rootless
/// storage lives in the home directory).
fn sandbox_on(runner: &dyn Runner, device: u64) -> bool {
    let root = runner
        .podman(&os(["info", "--format", "{{.Store.GraphRoot}}"]))
        .ok()
        .filter(|output| output.code == 0)
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|root| root.starts_with('/'));
    root.is_none_or(|root| fs::metadata(root).is_ok_and(|meta| meta.dev() == device))
}

/// What the measurement needs about a session.
#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub active: bool,
}

/// Walks difu's data and asks Podman for its storage. Slow on big servers:
/// run it off the request path.
/// `home` is difu's agent data directory and `cache` its cache directory.
pub(crate) fn measure(
    home: &Path,
    cache: &Path,
    sessions: &[SessionInfo],
    runner: &dyn Runner,
    cancel: &Cancel,
) -> Result<Breakdown> {
    let device = fs::metadata(home)?.dev();
    let mut walker = Walker::new(Some(device));
    let known: BTreeMap<&str, &SessionInfo> = sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect();
    let mut usage = Vec::new();
    if let Ok(entries) = fs::read_dir(home.join("worktrees")) {
        for entry in entries.flatten() {
            cancel.check()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Worktrees without a session count as other.
            let Some(session) = known.get(name.as_str()) else {
                continue;
            };
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let (bytes, candidates) = walker.worktree(&path);
            let build_outputs = match ignored(&path, candidates, cancel) {
                Ok(outputs) => outputs.iter().map(|(_, size)| size).sum(),
                Err(_) => 0,
            };
            usage.push(SessionUsage {
                id: session.id.clone(),
                title: session.title.clone(),
                archived: session.archived,
                active: session.active,
                bytes,
                build_outputs,
            });
        }
    }
    usage.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));
    let repositories = walker.usage(&home.join("repos"));
    let attachments = walker.usage(&home.join("attachments"));
    let caches = walker.usage(cache);
    let sandbox = sandbox_usage(runner);
    let sandbox_here = sandbox.available && sandbox_on(runner, device);
    let space = space(home)?;
    let accounted = usage
        .iter()
        .map(|session| session.bytes)
        .fold(0u64, u64::saturating_add)
        .saturating_add(repositories)
        .saturating_add(attachments)
        .saturating_add(caches)
        .saturating_add(if sandbox_here { sandbox.total() } else { 0 });
    Ok(Breakdown {
        sessions: usage,
        repositories,
        attachments,
        caches,
        sandbox,
        other: space.used.saturating_sub(accounted),
    })
}

/// Podman image as `podman images --format json` lists it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Image {
    #[serde(rename = "Id", alias = "ID")]
    id: String,
    #[serde(rename = "Names")]
    names: Option<Vec<String>>,
    #[serde(rename = "Containers")]
    containers: u64,
}

/// `name:latest` when no tag or digest is given.
fn tagged(name: &str) -> String {
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains(':') || last.contains('@') {
        name.to_owned()
    } else {
        format!("{name}:latest")
    }
}

/// Whether an image name refers to `keep`; short names such as
/// `ubuntu:24.04` match their qualified form.
fn same_image(name: &str, keep: &str) -> bool {
    let (name, keep) = (tagged(name), tagged(keep));
    name == keep || name.ends_with(&format!("/{keep}")) || keep.ends_with(&format!("/{name}"))
}

/// `podman system prune -f`, then every unused image except `keep`.
pub(crate) fn prune_sandbox(runner: &dyn Runner, keep: &str) -> Result<Cleaned> {
    ensure!(
        runner.available(),
        "Podman is not installed on this server, so there is no sandbox storage to prune"
    );
    let before = sandbox_usage(runner);
    let pruned = runner.podman(&os(["system", "prune", "-f"]))?;
    ensure!(
        pruned.code == 0,
        "podman system prune failed: {}",
        String::from_utf8_lossy(&pruned.stderr).trim()
    );
    let listed = runner.podman(&os(["images", "--format", "json"]))?;
    ensure!(
        listed.code == 0,
        "Cannot list sandbox images: {}",
        String::from_utf8_lossy(&listed.stderr).trim()
    );
    let text = String::from_utf8_lossy(&listed.stdout);
    let images: Vec<Image> = if text.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&text).context("Unexpected `podman images` output")?
    };
    let (mut removed, mut busy) = (0usize, 0usize);
    for image in images {
        let names = image.names.unwrap_or_default();
        if image.id.is_empty()
            || image.containers > 0
            || names.iter().any(|name| same_image(name, keep))
        {
            continue;
        }
        // Without `--force` Podman refuses images a container still uses.
        let output = runner.podman(&os(["rmi", image.id.as_str()]))?;
        if output.code == 0 {
            removed += 1;
        } else {
            busy += 1;
        }
    }
    let after = sandbox_usage(runner);
    let freed = before.total().saturating_sub(after.total());
    let mut summary = format!(
        "Pruned stopped containers and dangling images and removed {removed} unused {} ({} freed). Kept the sandbox image {keep}.",
        if removed == 1 { "image" } else { "images" },
        human(freed),
    );
    if busy > 0 {
        summary.push_str(&format!(
            " {busy} {} still in use stayed.",
            if busy == 1 { "image" } else { "images" }
        ));
    }
    Ok(Cleaned { freed, summary })
}

pub(crate) type Sessions = dyn Fn() -> Option<Vec<SessionInfo>> + Send + Sync;

#[derive(Default)]
struct Measured {
    breakdown: Breakdown,
    measured_at: Option<i64>,
    measuring: bool,
    wanted: bool,
    started: bool,
}

/// Keeps the last breakdown and measures again every `REFRESH`, or sooner
/// when asked (`refresh`), on its own thread.
pub(crate) struct Monitor {
    /// difu's agent data directory and cache directory.
    home: PathBuf,
    cache: PathBuf,
    runner: Arc<dyn Runner>,
    /// The service's coding sessions; `None` once the service is gone, which
    /// ends the thread.
    sessions: Box<Sessions>,
    state: Mutex<Measured>,
    wake: Condvar,
    /// One cleanup at a time.
    cleaning: Mutex<()>,
}

impl Monitor {
    pub(crate) fn new(
        home: PathBuf,
        cache: PathBuf,
        runner: Arc<dyn Runner>,
        sessions: Box<Sessions>,
    ) -> Self {
        Self {
            home,
            cache,
            runner,
            sessions,
            state: Mutex::new(Measured::default()),
            wake: Condvar::new(),
            cleaning: Mutex::new(()),
        }
    }
    fn lock(&self) -> MutexGuard<'_, Measured> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    /// Starts measuring in the background, once.
    pub(crate) fn start(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.started {
                return;
            }
            state.started = true;
            state.wanted = true;
        }
        let monitor = self.clone();
        if let Err(error) = thread::Builder::new()
            .name("difu-disk".into())
            .spawn(move || monitor.run())
        {
            eprintln!("Cannot start the disk measurement: {error}");
        }
    }
    fn run(&self) {
        let mut next = Instant::now();
        loop {
            {
                let mut state = self.lock();
                while !state.wanted && Instant::now() < next {
                    let wait = next.saturating_duration_since(Instant::now());
                    state = match self.wake.wait_timeout(state, wait) {
                        Ok((state, _)) => state,
                        Err(error) => error.into_inner().0,
                    };
                }
                state.wanted = false;
                state.measuring = true;
            }
            let Some(sessions) = (self.sessions)() else {
                return;
            };
            if let Err(error) = self.measure_with(&sessions) {
                eprintln!("Disk measurement: {error:#}");
            }
            next = Instant::now() + REFRESH;
        }
    }
    fn measure_with(&self, sessions: &[SessionInfo]) -> Result<()> {
        let result = measure(
            &self.home,
            &self.cache,
            sessions,
            self.runner.as_ref(),
            &Cancel::default(),
        );
        let mut state = self.lock();
        state.measuring = false;
        let breakdown = result?;
        state.breakdown = breakdown;
        state.measured_at = Some(now());
        Ok(())
    }
    /// Asks for a new measurement soon; the report says `measuring` until then.
    pub(crate) fn refresh(&self) {
        self.lock().wanted = true;
        self.wake.notify_all();
    }
    /// Live free space with the last breakdown.
    pub(crate) fn report(self: &Arc<Self>, refresh: bool) -> Result<Report> {
        self.start();
        if refresh {
            self.refresh();
        }
        let space = space(&self.home)?;
        let state = self.lock();
        Ok(Report {
            path: self.home.clone(),
            total: space.total,
            used: space.used,
            free: space.free,
            status: space.status(),
            measured_at: state.measured_at,
            measuring: state.measuring || state.wanted,
            breakdown: state.breakdown.clone(),
        })
    }
    /// Runs one cleanup at a time, then measures again.
    pub(crate) fn cleaning<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        let _cleaning = match self.cleaning.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                anyhow::bail!(
                    "Another disk cleanup is running on this server; try again when it finishes"
                )
            }
        };
        let result = work();
        self.refresh();
        result
    }
}

fn ago(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86400),
    }
}

/// `difu disk` output.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let space = report.space();
    out.push_str(&format!("{}\n", report.path.display()));
    out.push_str(&format!(
        "{} used of {} · {} free ({}%) · {}\n",
        human(report.used),
        human(report.total),
        human(report.free),
        space.free_percent(),
        report.status.label()
    ));
    if report.status == Status::Critical {
        out.push_str(&format!("{}\n", refusal(&space)));
    }
    match (report.measured_at, report.measuring) {
        (None, _) => out.push_str("Measuring what uses the space…\n"),
        (Some(at), measuring) => out.push_str(&format!(
            "Measured {}{}\n",
            ago(now() - at),
            if measuring {
                " · measuring again…"
            } else {
                ""
            }
        )),
    }
    if report.measured_at.is_none() {
        return out;
    }
    let breakdown = &report.breakdown;
    let sessions: u64 = breakdown.sessions.iter().map(|s| s.bytes).sum();
    let row = |label: &str, bytes: u64| format!("{label:<34} {:>9}\n", human(bytes));
    out.push('\n');
    out.push_str(&row(
        &format!("Session worktrees ({})", breakdown.sessions.len()),
        sessions,
    ));
    const SHOWN: usize = 15;
    for session in breakdown.sessions.iter().take(SHOWN) {
        let mut label = format!("  {}", session.title);
        if session.active {
            label.push_str(" · active");
        } else if session.archived {
            label.push_str(" · archived");
        }
        let label: String = label.chars().take(34).collect();
        out.push_str(&format!("{label:<34} {:>9}", human(session.bytes)));
        if session.build_outputs > 0 {
            out.push_str(&format!("  build outputs {}", human(session.build_outputs)));
        }
        out.push('\n');
    }
    if breakdown.sessions.len() > SHOWN {
        out.push_str(&format!(
            "  … and {} more\n",
            breakdown.sessions.len() - SHOWN
        ));
    }
    out.push_str(&row("Repositories", breakdown.repositories));
    out.push_str(&row("Attachments", breakdown.attachments));
    out.push_str(&row("Caches", breakdown.caches));
    let sandbox = &breakdown.sandbox;
    if sandbox.available {
        out.push_str(&format!(
            "{:<34} {:>9}  images {} · containers {} · volumes {} · {} reclaimable\n",
            "Sandbox storage",
            human(sandbox.total()),
            human(sandbox.images),
            human(sandbox.containers),
            human(sandbox.volumes),
            human(sandbox.reclaimable)
        ));
    }
    out.push_str(&row("Other", breakdown.other));
    out.push('\n');
    for action in Action::ALL {
        out.push_str(&format!(
            "difu disk clean {:<20} {} (about {})\n",
            action.argument(),
            action.label().to_lowercase(),
            human(report.expected(action))
        ));
    }
    out
}

/// `difu disk` on a server: asks the running service, which keeps the
/// breakdown, and runs cleanups there.
pub fn command(storage: &Storage, command: Option<DiskCommand>) -> Result<()> {
    ensure!(
        server_mode(storage),
        "`difu disk` runs on a difu server (`difu serve`). On your computer, `difu server list` and Cmd+K → /cloud show each server's disk space."
    );
    let service = |request| {
        crate::agents::client::request(storage, request).map_err(|error| {
            error.context(
                "The difu service is not answering; start it with `systemctl start difu-agent`",
            )
        })
    };
    match command.unwrap_or(DiskCommand::Status { refresh: false }) {
        DiskCommand::Status { refresh } => {
            let mut report = match service(Request::Disk {
                server: None,
                refresh,
            }) {
                Ok(Reply::Disk(report)) => report,
                Ok(_) => anyhow::bail!("Unexpected disk reply"),
                Err(error) => {
                    // Free space needs no service; the breakdown does.
                    let path = crate::agents::server::home(storage)?;
                    let space = space(&path)?;
                    eprintln!("{error:#}");
                    Report {
                        path,
                        total: space.total,
                        used: space.used,
                        free: space.free,
                        status: space.status(),
                        ..Report::default()
                    }
                }
            };
            // The first measurement after a start, or the one asked for, takes a moment.
            let deadline = Instant::now() + Duration::from_secs(60);
            while report.measuring
                && (report.measured_at.is_none() || refresh)
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(500));
                if let Ok(Reply::Disk(next)) = service(Request::Disk {
                    server: None,
                    refresh: false,
                }) {
                    report = next;
                }
            }
            print!("{}", render(&report));
            Ok(())
        }
        DiskCommand::Clean { action } => {
            println!("{}…", action.label());
            match service(Request::DiskClean {
                server: None,
                action,
            })? {
                Reply::DiskCleaned(cleaned) => {
                    println!("{}", cleaned.summary);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected cleanup reply"),
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::process::Output;
    use std::{os::unix::fs::symlink, process::Command};

    static PRETEND: Mutex<BTreeMap<PathBuf, Space>> = Mutex::new(BTreeMap::new());

    /// Makes `space` report `pretend` for `path` (difu's agent data directory
    /// in tests), or the real filesystem again with `None`.
    pub(crate) fn pretend(path: &Path, pretend: Option<Space>) {
        let mut map = PRETEND.lock().unwrap_or_else(|error| error.into_inner());
        match pretend {
            Some(space) => map.insert(path.to_path_buf(), space),
            None => map.remove(path),
        };
    }

    pub(super) fn pretended(path: &Path) -> Option<Space> {
        PRETEND
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(path)
            .copied()
    }

    /// 2 GB free of 100 GB.
    pub(crate) fn critical() -> Space {
        Space {
            total: 100 * GB,
            used: 98 * GB,
            free: 2 * GB,
        }
    }

    pub(crate) fn storage(dir: &Path, server: bool) -> Result<Storage> {
        let storage = Storage {
            config: dir.join("config.json"),
            cache: dir.join("cache"),
        };
        storage.update_config(|config| config.server.enabled = server)?;
        Ok(storage)
    }

    /// Podman stand-in: answers by command prefix and records every call.
    #[derive(Default)]
    pub(crate) struct Podman {
        pub missing: bool,
        pub replies: Mutex<Vec<(String, i32, String)>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl Podman {
        pub(crate) fn replying(replies: &[(&str, i32, &str)]) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(
                    replies
                        .iter()
                        .map(|(prefix, code, stdout)| {
                            ((*prefix).to_owned(), *code, (*stdout).to_owned())
                        })
                        .collect(),
                ),
                ..Self::default()
            })
        }
        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }
    }

    impl Runner for Podman {
        fn podman(&self, args: &[OsString]) -> Result<Output> {
            let call = args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            self.calls
                .lock()
                .map_err(|_| anyhow::anyhow!("lock"))?
                .push(call.clone());
            let mut replies = self.replies.lock().map_err(|_| anyhow::anyhow!("lock"))?;
            // The first matching reply is used once, unless it is the last.
            let index = replies
                .iter()
                .position(|(prefix, ..)| call.starts_with(prefix.as_str()));
            let (code, stdout) = match index {
                Some(index)
                    if replies
                        .iter()
                        .skip(index + 1)
                        .any(|(prefix, ..)| call.starts_with(prefix.as_str())) =>
                {
                    let (_, code, stdout) = replies.remove(index);
                    (code, stdout)
                }
                Some(index) => replies
                    .get(index)
                    .map(|(_, code, stdout)| (*code, stdout.clone()))
                    .unwrap_or_default(),
                None => (0, String::new()),
            };
            Ok(Output {
                stdout: stdout.into_bytes(),
                stderr: if code == 0 {
                    Vec::new()
                } else {
                    b"in use".to_vec()
                },
                code,
            })
        }
        fn available(&self) -> bool {
            !self.missing
        }
    }

    pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String> {
        crate::process::checked(
            Command::new("git")
                .arg("-C")
                .arg(root)
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
    }

    fn write(path: &Path, bytes: usize) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, vec![b'x'; bytes])?;
        Ok(())
    }

    /// A repository with ignored build outputs, a tracked file under an
    /// ignored directory and an unignored `dist`.
    pub(crate) fn worktree(root: &Path) -> Result<()> {
        fs::create_dir_all(root)?;
        git(root, &["init", "-q"])?;
        fs::write(
            root.join(".gitignore"),
            "target/\nnode_modules\nbuild/\n__pycache__/\n",
        )?;
        write(&root.join("src/main.rs"), 100)?;
        write(&root.join("target/debug/app"), 64 * 1024)?;
        write(&root.join("web/node_modules/pkg/index.js"), 32 * 1024)?;
        write(&root.join("build/keep.txt"), 10)?;
        write(&root.join("dist/bundle.js"), 8 * 1024)?;
        write(&root.join("tools/__pycache__/x.pyc"), 4 * 1024)?;
        git(root, &["add", ".gitignore", "src", "dist"])?;
        git(root, &["add", "-f", "build/keep.txt"])?;
        git(root, &["commit", "-q", "-m", "init"])?;
        Ok(())
    }

    #[test]
    fn status_thresholds_use_percent_or_bytes_whichever_is_stricter() {
        let tb = 1000 * GB;
        assert_eq!(Status::of(tb, 200 * GB), Status::Ok);
        // 14% of a terabyte is plenty of bytes but under 15%.
        assert_eq!(Status::of(tb, 140 * GB), Status::Low);
        assert_eq!(Status::of(tb, 49 * GB), Status::Critical);
        // A small disk: 19 GB free of 40 GB is 47% but under 20 GB.
        assert_eq!(Status::of(40 * GB, 19 * GB), Status::Low);
        assert_eq!(Status::of(40 * GB, 4 * GB), Status::Critical);
        assert_eq!(Status::of(40 * GB, 20 * GB), Status::Ok);
        assert_eq!(Status::of(0, 0), Status::Critical);
        assert_eq!(
            serde_json::to_value(Status::Critical).ok(),
            Some(serde_json::json!("critical"))
        );
    }

    #[test]
    fn statvfs_reports_a_consistent_filesystem() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let space = space(dir.path())?;
        assert!(space.total > 0);
        assert!(space.used <= space.total && space.free <= space.total);
        assert!(space.free_percent() <= 100);
        assert!(super::space(&dir.path().join("missing")).is_err());
        Ok(())
    }

    #[test]
    fn sizes_read_like_df() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1_500), "1.5 kB");
        assert_eq!(human(28_400_000_000), "28.4 GB");
        assert_eq!(human(123_000_000_000), "123 GB");
        assert_eq!(human(2_000_000_000_000), "2.0 TB");
    }

    #[test]
    fn refusal_names_free_space_and_the_cleanups() {
        let message = refusal(&critical());
        assert!(message.contains("Only 2.0 GB is free"), "{message}");
        assert!(message.contains("2% of 100 GB"), "{message}");
        assert!(message.contains("/cloud") && message.contains("difu disk clean"));
    }

    #[test]
    fn guard_refuses_only_critical_servers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(dir.path(), false)?;
        let home = crate::agents::server::home(&storage)?;
        pretend(&home, Some(critical()));
        // A computer is never refused.
        guard(&storage)?;
        storage.update_config(|config| config.server.enabled = true)?;
        let error = guard(&storage).err().context("critical is refused")?;
        assert!(error.to_string().contains("Only 2.0 GB"), "{error}");
        pretend(
            &home,
            Some(Space {
                total: 100 * GB,
                used: 90 * GB,
                free: 10 * GB,
            }),
        );
        guard(&storage)?;
        pretend(&home, None);
        Ok(())
    }

    #[test]
    fn walking_counts_blocks_once_and_never_follows_symlinks() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("tree");
        let outside = dir.path().join("outside");
        write(&root.join("a/file"), 100_000)?;
        write(&outside.join("big"), 1_000_000)?;
        symlink(&outside, root.join("link"))?;
        symlink(outside.join("big"), root.join("file-link"))?;
        fs::hard_link(root.join("a/file"), root.join("a/again"))?;
        let size = usage(&root);
        assert!(size >= 100_000, "{size}");
        assert!(
            size < 400_000,
            "symlink targets and hard links are not counted twice: {size}"
        );
        assert_eq!(usage(&dir.path().join("missing")), 0);
        Ok(())
    }

    #[test]
    fn build_outputs_are_ignored_untracked_directories_only() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("worktree");
        worktree(&root)?;
        // A symlink named like an output, pointing outside, is never a candidate.
        let outside = dir.path().join("outside");
        write(&outside.join("precious"), 10)?;
        fs::write(root.join(".git/info/exclude"), "node_modules-link\n.venv\n")?;
        symlink(&outside, root.join(".venv"))?;
        let outputs = build_outputs(&root, &Cancel::default())?;
        let mut names: Vec<String> = outputs
            .iter()
            .filter_map(|(path, _)| path.strip_prefix(&root).ok())
            .map(|path| path.display().to_string())
            .collect();
        names.sort();
        // `build` holds a tracked file and `dist` is not ignored.
        assert_eq!(
            names,
            vec!["target", "tools/__pycache__", "web/node_modules"]
        );
        assert!(outputs.iter().all(|(_, size)| *size > 0));
        let (freed, errors) = remove_outputs(&root, &outputs);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(freed >= 64 * 1024 + 32 * 1024, "{freed}");
        assert!(!root.join("target").exists());
        assert!(!root.join("web/node_modules").exists());
        assert!(root.join("build/keep.txt").is_file(), "tracked files stay");
        assert!(root.join("dist/bundle.js").is_file());
        assert!(root.join("src/main.rs").is_file());
        assert!(outside.join("precious").is_file());
        assert!(root.join(".venv").is_symlink());
        assert_eq!(git(&root, &["status", "--porcelain"])?, "");
        Ok(())
    }

    #[test]
    fn removal_refuses_symlink_escapes_and_paths_outside_the_worktree() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("worktree");
        let outside = dir.path().join("outside");
        write(&outside.join("node_modules/keep"), 10)?;
        write(&root.join("file"), 10)?;
        symlink(&outside, root.join("pkg"))?;
        symlink(outside.join("node_modules"), root.join("target"))?;
        let attempts = vec![
            (root.join("pkg/node_modules"), 10),
            (root.join("target"), 10),
            (outside.join("node_modules"), 10),
            (root.join("../outside/node_modules"), 10),
        ];
        let (freed, errors) = remove_outputs(&root, &attempts);
        assert_eq!(freed, 0);
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(outside.join("node_modules/keep").is_file());
        assert!(root.join("target").is_symlink());
        Ok(())
    }

    #[test]
    fn podman_storage_parses_numbers_and_human_sizes() -> Result<()> {
        let json = r#"[
            {"Type":"Images","Total":3,"Active":1,"RawSize":3000000000,"RawReclaimable":900000000,"Size":"3GB","Reclaimable":"900MB (30%)"},
            {"Type":"Containers","Total":2,"Active":1,"RawSize":120000000,"RawReclaimable":20000000},
            {"Type":"Local Volumes","Total":1,"Active":0,"RawSize":5000,"RawReclaimable":5000}
        ]"#;
        let usage = parse_system_df(json)?;
        assert_eq!(
            usage,
            SandboxUsage {
                available: true,
                images: 3_000_000_000,
                containers: 120_000_000,
                volumes: 5000,
                reclaimable: 920_000_000,
            }
        );
        let lines = "{\"Type\":\"Images\",\"Size\":\"1.5GB\",\"Reclaimable\":\"500MB (33%)\"}\n{\"Type\":\"Containers\",\"Size\":\"0B\",\"Reclaimable\":\"0B\"}\n";
        let usage = parse_system_df(lines)?;
        assert_eq!(usage.images, 1_500_000_000);
        assert_eq!(usage.reclaimable, 500_000_000);
        assert_eq!(parse_human("12.5 kB"), Some(12_500));
        assert_eq!(parse_human("1GiB"), Some(1 << 30));
        assert_eq!(parse_human("lots"), None);
        assert!(parse_system_df("not json").is_err());
        Ok(())
    }

    #[test]
    fn measurement_breaks_down_sessions_repositories_and_sandbox() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(dir.path(), true)?;
        let home = crate::agents::server::home(&storage)?;
        worktree(&home.join("worktrees/one"))?;
        write(&home.join("worktrees/two/notes.txt"), 2000)?;
        write(&home.join("worktrees/orphan/big"), 50_000)?;
        write(&home.join("repos/acme/api/.git/objects/pack"), 40_000)?;
        write(&home.join("attachments/one/media.png"), 3000)?;
        write(&storage.cache.join("agent-prs.json"), 1000)?;
        pretend(
            &home,
            Some(Space {
                total: 100 * GB,
                used: 10 * GB,
                free: 90 * GB,
            }),
        );
        let podman = Podman::replying(&[(
            "system df",
            0,
            r#"[{"Type":"Images","RawSize":2000,"RawReclaimable":500},{"Type":"Containers","RawSize":100,"RawReclaimable":0}]"#,
        )]);
        let sessions = vec![
            SessionInfo {
                id: "one".into(),
                title: "Build things".into(),
                archived: true,
                active: false,
            },
            SessionInfo {
                id: "two".into(),
                title: "Notes".into(),
                archived: false,
                active: true,
            },
        ];
        let breakdown = measure(
            &home,
            &storage.cache,
            &sessions,
            podman.as_ref(),
            &Cancel::default(),
        )?;
        pretend(&home, None);
        let [first, second] = breakdown.sessions.as_slice() else {
            anyhow::bail!("two sessions, not the orphan: {:?}", breakdown.sessions);
        };
        assert_eq!(first.id, "one");
        assert!(first.archived && !first.active);
        assert!(first.bytes > second.bytes);
        assert!(first.build_outputs >= 64 * 1024 + 32 * 1024);
        assert!(first.build_outputs < first.bytes);
        assert_eq!((second.id.as_str(), second.build_outputs), ("two", 0));
        assert!(breakdown.repositories >= 40_000);
        assert!(breakdown.attachments >= 3000);
        assert!(breakdown.caches >= 1000);
        assert_eq!(breakdown.sandbox.images, 2000);
        assert_eq!(breakdown.sandbox.reclaimable, 500);
        assert!(breakdown.other > 9 * GB && breakdown.other < 10 * GB);
        let report = Report {
            breakdown,
            ..Report::default()
        };
        assert_eq!(
            report.expected(Action::ArchivedWorktrees),
            report.breakdown.sessions.first().map_or(0, |s| s.bytes)
        );
        assert_eq!(report.expected(Action::Sandbox), 500);
        Ok(())
    }

    #[test]
    fn sandbox_pruning_keeps_the_configured_and_used_images() -> Result<()> {
        let images = r#"[
            {"Id":"aaa","Names":["localhost/difu-sandbox:latest"],"Containers":0},
            {"Id":"bbb","Names":["docker.io/library/ubuntu:24.04"],"Containers":0},
            {"Id":"ccc","Names":["ghcr.io/acme/tool:1"],"Containers":2},
            {"Id":"ddd","Names":null,"Containers":0},
            {"Id":"eee","Names":["quay.io/x/busy:2"],"Containers":0}
        ]"#;
        let podman = Podman::replying(&[
            ("system df", 0, r#"[{"Type":"Images","RawSize":5000}]"#),
            ("system df", 0, r#"[{"Type":"Images","RawSize":2000}]"#),
            ("system prune -f", 0, ""),
            ("images --format json", 0, images),
            ("rmi eee", 1, ""),
        ]);
        let cleaned = prune_sandbox(podman.as_ref(), "localhost/difu-sandbox")?;
        assert_eq!(cleaned.freed, 3000);
        let calls = podman.calls();
        assert!(calls.contains(&"system prune -f".to_owned()), "{calls:?}");
        assert!(calls.contains(&"rmi bbb".to_owned()));
        assert!(calls.contains(&"rmi ddd".to_owned()));
        assert!(
            !calls.iter().any(|c| c == "rmi aaa" || c == "rmi ccc"),
            "{calls:?}"
        );
        assert!(
            cleaned.summary.contains("removed 2 unused images"),
            "{}",
            cleaned.summary
        );
        assert!(
            cleaned.summary.contains("1 image still in use"),
            "{}",
            cleaned.summary
        );
        let missing = Podman {
            missing: true,
            ..Podman::default()
        };
        assert!(prune_sandbox(&missing, "x").is_err());
        assert!(missing.calls().is_empty());
        assert!(same_image("ubuntu:24.04", "docker.io/library/ubuntu:24.04"));
        assert!(!same_image(
            "localhost/difu-sandbox:old",
            "localhost/difu-sandbox"
        ));
        Ok(())
    }

    #[test]
    fn monitor_reports_live_space_and_measures_in_the_background() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = storage(dir.path(), true)?;
        let home = crate::agents::server::home(&storage)?;
        write(&home.join("worktrees/one/file"), 5000)?;
        pretend(&home, Some(critical()));
        let monitor = Arc::new(Monitor::new(
            home.clone(),
            storage.cache.clone(),
            Podman::replying(&[]),
            Box::new(|| {
                Some(vec![SessionInfo {
                    id: "one".into(),
                    title: "One".into(),
                    ..SessionInfo::default()
                }])
            }),
        ));
        let mut report = monitor.report(false)?;
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.free, 2 * GB);
        let deadline = Instant::now() + Duration::from_secs(10);
        while report.measured_at.is_none() {
            anyhow::ensure!(Instant::now() < deadline, "never measured");
            thread::sleep(Duration::from_millis(20));
            report = monitor.report(false)?;
        }
        pretend(&home, None);
        assert_eq!(report.breakdown.sessions.len(), 1);
        assert!(
            monitor.report(true)?.measuring,
            "a refresh shows as measuring"
        );
        let value = serde_json::to_value(&report)?;
        for key in [
            "path",
            "total",
            "used",
            "free",
            "status",
            "measuredAt",
            "measuring",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
        for pointer in [
            "/breakdown/sessions/0/id",
            "/breakdown/sessions/0/title",
            "/breakdown/sessions/0/archived",
            "/breakdown/sessions/0/active",
            "/breakdown/sessions/0/bytes",
            "/breakdown/sessions/0/buildOutputs",
            "/breakdown/repositories",
            "/breakdown/attachments",
            "/breakdown/caches",
            "/breakdown/sandbox/images",
            "/breakdown/sandbox/containers",
            "/breakdown/sandbox/volumes",
            "/breakdown/other",
        ] {
            assert!(value.pointer(pointer).is_some(), "{pointer}");
        }
        // Lenient: a report from another version still reads.
        let partial: Report = serde_json::from_value(serde_json::json!({
            "total": 10, "free": 5, "status": "low", "extra": true, "breakdown": {"sessions": [{"id": "x"}]}
        }))?;
        assert_eq!(partial.status, Status::Low);
        assert_eq!(partial.breakdown.sessions.first().map(|s| s.bytes), Some(0));
        // One cleanup at a time.
        let nested = monitor.cleaning(|| monitor.cleaning(|| Ok(())));
        assert!(nested.is_err_and(|e| e.to_string().contains("Another disk cleanup")));
        Ok(())
    }

    #[test]
    fn actions_use_camel_case_on_the_wire_and_kebab_case_on_the_command_line() -> Result<()> {
        assert_eq!(
            serde_json::to_value(Action::ArchivedWorktrees)?,
            serde_json::json!("archivedWorktrees")
        );
        assert_eq!(
            serde_json::from_value::<Action>(serde_json::json!("buildOutputs"))?,
            Action::BuildOutputs
        );
        for action in Action::ALL {
            assert_eq!(
                <Action as clap::ValueEnum>::from_str(action.argument(), false).ok(),
                Some(action)
            );
        }
        Ok(())
    }

    #[test]
    fn rendering_shows_breakdown_and_cleanups() {
        let report = Report {
            path: "/srv/difu/agents".into(),
            total: 100 * GB,
            used: 97 * GB,
            free: 3 * GB,
            status: Status::Critical,
            measured_at: Some(now()),
            measuring: false,
            breakdown: Breakdown {
                sessions: vec![SessionUsage {
                    id: "1".into(),
                    title: "Widget".into(),
                    archived: true,
                    active: false,
                    bytes: 4 * GB,
                    build_outputs: GB,
                }],
                repositories: 2 * GB,
                ..Breakdown::default()
            },
        };
        let text = render(&report);
        assert!(
            text.contains("97.0 GB used of 100 GB · 3.0 GB free (3%) · critical"),
            "{text}"
        );
        assert!(text.contains("Widget · archived"), "{text}");
        assert!(text.contains("build outputs 1.0 GB"), "{text}");
        assert!(
            text.contains("difu disk clean archived-worktrees"),
            "{text}"
        );
        assert!(text.contains("about 4.0 GB"), "{text}");
    }
}
