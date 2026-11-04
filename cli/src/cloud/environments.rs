//! Environment profiles: what a server repository's sandboxes need to build
//! and test (tools and their versions, setup commands, non-secret variables
//! and notes), remembered on the server per `owner/name` so every session
//! starts ready instead of rediscovering it. See docs/cloud-agents.md,
//! *Environments*.
//!
//! * Profiles live in `<data>/agents/repos/.environments.json`, next to the
//!   base clones, keyed by `owner/name` (matched case-insensitively), and
//!   change under `.environments.lock`. Each keeps a short history of who
//!   changed what, and when.
//! * [`detect`] reads the version files a worktree declares, as mise does
//!   (plus `package.json` and `go.mod`); [`resolve`] overlays them on the
//!   profile: the repository's files win and the profile fills the gaps.
//! * Each worktree keeps its generated mise config and the fingerprints of its
//!   last toolchain install and setup in its own Git directory
//!   (`<git dir>/difu-environment`), so removing the worktree removes them.
//! * Nothing here may hold a secret: profiles are plain text that every client
//!   of the server reads, so variables, commands and notes that look like
//!   tokens or passwords are refused.
use crate::{
    process::{self, Cancel},
    storage::{self, Storage},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const FILE: &str = ".environments.json";
const LOCK: &str = ".environments.lock";
/// Changes kept per profile.
pub const HISTORY: usize = 20;
const MAX_TOOLS: usize = 40;
const MAX_SETUP: usize = 20;
const MAX_ENV: usize = 50;
const MAX_NOTES: usize = 30;
const COMMAND_LIMIT: usize = 1000;
const VALUE_LIMIT: usize = 2000;
const NOTE_LIMIT: usize = 1000;
/// Version files are small; anything bigger is not one.
const VERSION_FILE_LIMIT: u64 = 256 << 10;
const LOCKFILE_LIMIT: u64 = 128 << 20;
/// The per-worktree directory inside the worktree's Git directory.
pub const STATE: &str = "difu-environment";
/// Where tools from the profile come from, in [`Detected::source`].
pub const PROFILE: &str = "environment profile";
/// Marks the setup script's progress in its output.
pub const MARK: &str = "::difu-environment::";
/// Tools whose idiomatic version files (`.nvmrc`, `.python-version`, ...)
/// mise reads in every sandbox.
const IDIOMATIC: [&str; 9] = [
    "node",
    "python",
    "ruby",
    "go",
    "java",
    "rust",
    "bun",
    "deno",
    "terraform",
];
/// Files whose contents decide whether setup commands run again.
pub const LOCKFILES: [&str; 20] = [
    "package.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    ".yarnrc.yml",
    "bun.lock",
    "bun.lockb",
    "Cargo.lock",
    "go.sum",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "requirements.txt",
    "requirements-dev.txt",
    "pyproject.toml",
    "Gemfile.lock",
    "composer.lock",
    "mix.lock",
];
/// Variables difu or the sandbox sets, or that would change how every program
/// in the sandbox runs.
const RESERVED: [&str; 21] = [
    "PATH",
    "HOME",
    "SHELL",
    "USER",
    "LANG",
    "TERM",
    "IFS",
    "ENV",
    "BASH_ENV",
    "PROMPT_COMMAND",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "IS_SANDBOX",
    "DISABLE_AUTOUPDATER",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "SSH_AUTH_SOCK",
];
const RESERVED_PREFIXES: [&str; 2] = ["MISE_", "DIFU_"];
/// Where the shared toolchain caches live inside every sandbox.
pub const CACHE_MOUNT: &str = "/opt/difu/toolchains";
/// Variables pointing every package manager and toolchain at the shared
/// caches, with each one's directory under [`CACHE_MOUNT`].
pub const CACHES: [(&str, &str); 17] = [
    ("MISE_DATA_DIR", "mise"),
    ("MISE_CACHE_DIR", "cache/mise"),
    ("MISE_STATE_DIR", "state/mise"),
    ("COREPACK_HOME", "corepack"),
    ("PNPM_HOME", "pnpm"),
    ("npm_config_cache", "npm"),
    ("YARN_CACHE_FOLDER", "yarn"),
    ("YARN_GLOBAL_FOLDER", "yarn-berry"),
    ("PIP_CACHE_DIR", "pip"),
    ("UV_CACHE_DIR", "uv"),
    ("UV_PYTHON_INSTALL_DIR", "uv-python"),
    ("CARGO_HOME", "cargo"),
    ("RUSTUP_HOME", "rustup"),
    ("GOPATH", "go"),
    ("GOMODCACHE", "go/pkg/mod"),
    ("GOCACHE", "go/build"),
    ("BUN_INSTALL_CACHE_DIR", "bun"),
];
/// Directories under [`CACHE_MOUNT`] that hold programs, in `PATH` order.
pub const CACHE_PATH: [&str; 4] = ["mise/shims", "cargo/bin", "pnpm", "go/bin"];

/// What one repository's sandboxes need, as the server remembers it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    /// mise tool name → version, e.g. `node` → `24`, `npm:prettier` → `3`.
    pub tools: BTreeMap<String, String>,
    /// Commands run in each new worktree before its agent starts.
    pub setup: Vec<String>,
    /// Non-secret variables for the agent and its shells.
    pub env: BTreeMap<String, String>,
    /// Free text for agents ("backend tests need Postgres via docker compose").
    pub notes: Vec<String>,
    /// The latest changes, oldest first. Clients cannot rewrite it.
    pub history: Vec<Change>,
    /// Milliseconds since the epoch of the last change.
    pub updated_at: Option<i64>,
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.setup.is_empty()
            && self.env.is_empty()
            && self.notes.is_empty()
    }
}

/// One change to a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Change {
    /// Milliseconds since the epoch.
    pub at: i64,
    /// Who changed it: a device, an agent in a session, or difu itself.
    pub by: String,
    pub summary: String,
}

/// A tool and version a worktree's files declare, or (in [`resolve`]'s
/// result) that its sandbox provides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Detected {
    pub tool: String,
    pub version: String,
    /// The file it comes from, relative to the worktree (`.nvmrc`,
    /// `package.json packageManager`), or [`PROFILE`].
    pub source: String,
    /// Provided by corepack from `package.json` `packageManager`, not by mise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub corepack: bool,
}

/// A repository's profile and what its base clone declares, for clients
/// (`server.environment.get`, `Reply::Environment`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct View {
    pub repository: String,
    pub profile: Profile,
    /// The version files on the base clone's checkout (its default branch).
    pub detected: Vec<Detected>,
}

/// What an agent asked `difu_environment` to remember or forget.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    pub tools: Vec<(String, String)>,
    pub setup: Vec<String>,
    pub env: Vec<(String, String)>,
    pub notes: Vec<String>,
    pub forget_tools: Vec<String>,
    pub forget_setup: Vec<String>,
    pub forget_env: Vec<String>,
    pub forget_notes: Vec<String>,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.setup.is_empty()
            && self.env.is_empty()
            && self.notes.is_empty()
            && self.forget_tools.is_empty()
            && self.forget_setup.is_empty()
            && self.forget_env.is_empty()
            && self.forget_notes.is_empty()
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// At most `limit` characters of `text`, with an ellipsis when cut.
pub fn short(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(limit.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

// Validation.

fn valid_tool_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
}

fn valid_version(version: &str) -> bool {
    (1..=64).contains(&version.len())
        && version.starts_with(|c: char| c.is_ascii_alphanumeric())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+:/".contains(&b))
}

/// A mise tool name (`node`, `python`, `npm:prettier`, `ubi:owner/repo`) and a
/// version mise understands (`24`, `3.12.4`, `latest`, `lts`).
pub fn validate_tool(name: &str, version: &str) -> Result<()> {
    ensure!(
        valid_tool_name(name),
        "Invalid tool name {name:?}: use a mise tool name such as node, python, go or npm:prettier"
    );
    ensure!(
        valid_version(version),
        "Invalid version {version:?} for {name}: use a version mise understands, such as 24, 3.12.4, lts or latest"
    );
    Ok(())
}

/// One command line, run with `bash -lc` in the worktree.
pub fn validate_command(command: &str) -> Result<()> {
    let command = command.trim();
    ensure!(!command.is_empty(), "Setup commands cannot be empty");
    ensure!(
        command.chars().count() <= COMMAND_LIMIT,
        "Setup commands are limited to {COMMAND_LIMIT} characters"
    );
    ensure!(
        !command.chars().any(char::is_control),
        "Write each setup command on one line: {}",
        short(command, 80)
    );
    if let Some(word) = secret_word(command) {
        bail!(
            "This setup command contains what looks like a secret ({}). Environment profiles are plain text every client of this server can read; keep secrets out of them",
            redacted(word)
        );
    }
    Ok(())
}

pub fn validate_note(note: &str) -> Result<()> {
    let note = note.trim();
    ensure!(!note.is_empty(), "Notes cannot be empty");
    ensure!(
        note.chars().count() <= NOTE_LIMIT,
        "Notes are limited to {NOTE_LIMIT} characters"
    );
    if let Some(word) = secret_word(note) {
        bail!(
            "This note contains what looks like a secret ({}). Environment profiles are plain text every client of this server can read; keep secrets out of them",
            redacted(word)
        );
    }
    Ok(())
}

fn valid_variable_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Set by difu or the sandbox, or able to change how every program runs.
pub fn reserved(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    RESERVED.contains(&upper.as_str())
        || CACHES
            .iter()
            .any(|(variable, _)| variable.eq_ignore_ascii_case(name))
        || ["COREPACK_HOME", "COREPACK_ENABLE_AUTO_PIN"].contains(&upper.as_str())
        || RESERVED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// Names that say they hold a secret.
fn secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "CREDENTIAL",
        "PRIVATE",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "AUTH_KEY",
        "SESSION_KEY",
        "SIGNING_KEY",
        "ENCRYPTION_KEY",
        "CLIENT_KEY",
    ]
    .iter()
    .any(|word| upper.contains(word))
        || upper == "KEY"
        || upper.ends_with("_KEY")
}

/// Values that look like a token, key or password: well-known token prefixes,
/// private keys, JWTs and long opaque strings that mix cases and digits.
pub fn secret_value(value: &str) -> bool {
    const PREFIXES: [&str; 26] = [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "sk-",
        "sk_live_",
        "sk_test_",
        "rk_live_",
        "pk_live_",
        "xoxa-",
        "xoxb-",
        "xoxp-",
        "xoxr-",
        "xapp-",
        "AKIA",
        "ASIA",
        "AIza",
        "ya29.",
        "npm_",
        "pypi-",
        "dop_v1_",
        "hf_",
        "SG.",
    ];
    let value = value.trim();
    if value.contains("-----BEGIN") {
        return true;
    }
    // A known prefix followed by something random (not `npm_config_cache`).
    if let Some(rest) = PREFIXES
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        && rest.len() >= 8
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        && rest
            .bytes()
            .any(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
    {
        return true;
    }
    if value.starts_with("eyJ") && value.matches('.').count() >= 2 && value.len() >= 24 {
        return true;
    }
    value.len() >= 32
        && !value.contains("://")
        && !value.starts_with(['/', '-', '.'])
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-+/=.".contains(&b))
        && value.bytes().any(|b| b.is_ascii_uppercase())
        && value.bytes().any(|b| b.is_ascii_lowercase())
        && value.bytes().any(|b| b.is_ascii_digit())
}

/// The first word of `text` (split at whitespace, quotes and `=`) that looks
/// like a secret.
fn secret_word(text: &str) -> Option<&str> {
    text.split(|c: char| c.is_whitespace() || "=\"'`,;:".contains(c))
        .find(|word| secret_value(word))
}

fn redacted(word: &str) -> String {
    let start: String = word.chars().take(4).collect();
    format!("{start}…")
}

/// A non-secret variable for the agent and its shells.
pub fn validate_variable(name: &str, value: &str) -> Result<()> {
    ensure!(
        valid_variable_name(name),
        "Invalid variable name {name:?}: use letters, digits and underscores"
    );
    ensure!(
        !reserved(name),
        "{name} is set by difu or the sandbox and cannot be changed in an environment profile"
    );
    ensure!(
        value.chars().count() <= VALUE_LIMIT,
        "{name} is longer than {VALUE_LIMIT} characters"
    );
    ensure!(
        !value.contains(['\0', '\n', '\r']),
        "{name} must be a single line"
    );
    ensure!(
        !secret_name(name) && !secret_value(value),
        "{name} looks like a secret. Environment profiles are plain text every client of this server can read; keep secrets out of them (the server's logins hold its credentials)"
    );
    Ok(())
}

/// Every entry and the limits on how many there are.
pub fn validate(profile: &Profile) -> Result<()> {
    ensure!(
        profile.tools.len() <= MAX_TOOLS,
        "An environment profile holds at most {MAX_TOOLS} tools"
    );
    ensure!(
        profile.setup.len() <= MAX_SETUP,
        "An environment profile holds at most {MAX_SETUP} setup commands"
    );
    ensure!(
        profile.env.len() <= MAX_ENV,
        "An environment profile holds at most {MAX_ENV} variables"
    );
    ensure!(
        profile.notes.len() <= MAX_NOTES,
        "An environment profile holds at most {MAX_NOTES} notes"
    );
    for (tool, version) in &profile.tools {
        validate_tool(tool, version)?;
    }
    for command in &profile.setup {
        validate_command(command)?;
    }
    for (name, value) in &profile.env {
        validate_variable(name, value)?;
    }
    for note in &profile.notes {
        validate_note(note)?;
    }
    Ok(())
}

/// Trimmed entries without blank setup lines or notes; history is dropped
/// (only difu writes it).
pub fn normalized(profile: &Profile) -> Profile {
    let lines = |items: &[String]| -> Vec<String> {
        let mut kept: Vec<String> = Vec::new();
        for item in items.iter().map(|item| item.trim()) {
            if !item.is_empty() && !kept.iter().any(|existing| existing == item) {
                kept.push(item.to_owned());
            }
        }
        kept
    };
    Profile {
        tools: profile
            .tools
            .iter()
            .map(|(tool, version)| (tool.trim().to_owned(), version.trim().to_owned()))
            .collect(),
        setup: lines(&profile.setup),
        env: profile
            .env
            .iter()
            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
            .collect(),
        notes: lines(&profile.notes),
        history: Vec::new(),
        updated_at: None,
    }
}

/// Applies `edit` after validating it; returns what changed, for the history
/// and the chat. Forgetting comes first, so an edit can replace an entry.
pub fn apply(profile: &mut Profile, edit: &Edit) -> Result<Vec<String>> {
    for (tool, version) in &edit.tools {
        validate_tool(tool.trim(), version.trim())?;
    }
    for command in &edit.setup {
        validate_command(command)?;
    }
    for (name, value) in &edit.env {
        validate_variable(name.trim(), value.trim())?;
    }
    for note in &edit.notes {
        validate_note(note)?;
    }
    let mut changes = Vec::new();
    for tool in &edit.forget_tools {
        if profile.tools.remove(tool.trim()).is_some() {
            changes.push(format!("removed tool {}", tool.trim()));
        }
    }
    for command in &edit.forget_setup {
        let before = profile.setup.len();
        profile.setup.retain(|existing| existing != command.trim());
        if profile.setup.len() != before {
            changes.push(format!("removed setup `{}`", short(command, 80)));
        }
    }
    for name in &edit.forget_env {
        if profile.env.remove(name.trim()).is_some() {
            changes.push(format!("removed {}", name.trim()));
        }
    }
    for note in &edit.forget_notes {
        let before = profile.notes.len();
        profile.notes.retain(|existing| existing != note.trim());
        if profile.notes.len() != before {
            changes.push(format!("removed note \"{}\"", short(note, 60)));
        }
    }
    for (tool, version) in &edit.tools {
        let (tool, version) = (tool.trim(), version.trim());
        match profile.tools.insert(tool.to_owned(), version.to_owned()) {
            None => changes.push(format!("{tool} {version}")),
            Some(old) if old != version => changes.push(format!("{tool} {old} → {version}")),
            Some(_) => {}
        }
    }
    for command in &edit.setup {
        let command = command.trim();
        if !profile.setup.iter().any(|existing| existing == command) {
            profile.setup.push(command.to_owned());
            changes.push(format!("setup `{}`", short(command, 80)));
        }
    }
    for (name, value) in &edit.env {
        let (name, value) = (name.trim(), value.trim());
        if profile.env.get(name).map(String::as_str) != Some(value) {
            profile.env.insert(name.to_owned(), value.to_owned());
            changes.push(format!("{name}={}", short(value, 60)));
        }
    }
    for note in &edit.notes {
        let note = note.trim();
        if !profile.notes.iter().any(|existing| existing == note) {
            profile.notes.push(note.to_owned());
            changes.push(format!("note \"{}\"", short(note, 60)));
        }
    }
    validate(profile)?;
    Ok(changes)
}

/// What changed from `old` to `new`, in the words `apply` uses.
pub fn diff(old: &Profile, new: &Profile) -> Vec<String> {
    let mut changes = Vec::new();
    for (tool, version) in &old.tools {
        match new.tools.get(tool) {
            None => changes.push(format!("removed tool {tool}")),
            Some(changed) if changed != version => {
                changes.push(format!("{tool} {version} → {changed}"))
            }
            Some(_) => {}
        }
    }
    for (tool, version) in &new.tools {
        if !old.tools.contains_key(tool) {
            changes.push(format!("{tool} {version}"));
        }
    }
    for command in old.setup.iter().filter(|c| !new.setup.contains(c)) {
        changes.push(format!("removed setup `{}`", short(command, 80)));
    }
    for command in new.setup.iter().filter(|c| !old.setup.contains(c)) {
        changes.push(format!("setup `{}`", short(command, 80)));
    }
    if changes.is_empty() && old.setup != new.setup {
        changes.push("reordered setup".into());
    }
    for name in old.env.keys().filter(|name| !new.env.contains_key(*name)) {
        changes.push(format!("removed {name}"));
    }
    for (name, value) in &new.env {
        if old.env.get(name) != Some(value) {
            changes.push(format!("{name}={}", short(value, 60)));
        }
    }
    for note in old.notes.iter().filter(|n| !new.notes.contains(n)) {
        changes.push(format!("removed note \"{}\"", short(note, 60)));
    }
    for note in new.notes.iter().filter(|n| !old.notes.contains(n)) {
        changes.push(format!("note \"{}\"", short(note, 60)));
    }
    changes
}

/// Changes as one line.
pub fn summary(changes: &[String]) -> String {
    short(&changes.join("; "), 500)
}

// Storage.

fn read_all(path: &Path) -> Result<BTreeMap<String, Profile>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("Cannot read environment profiles in {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error).with_context(|| format!("Cannot read {}", path.display())),
    }
}

/// The saved key for `repository`: an existing spelling, the listed one, or
/// `repository` itself.
fn key(storage: &Storage, all: &BTreeMap<String, Profile>, repository: &str) -> String {
    if let Some(existing) = all.keys().find(|k| k.eq_ignore_ascii_case(repository)) {
        return existing.clone();
    }
    storage
        .load_config()
        .ok()
        .and_then(|config| {
            config
                .server
                .repositories
                .into_iter()
                .find(|listed| listed.eq_ignore_ascii_case(repository))
        })
        .unwrap_or_else(|| repository.to_owned())
}

/// `repository`'s profile; empty when none is saved.
pub fn load(storage: &Storage, repository: &str) -> Result<Profile> {
    super::repos::validate(repository)?;
    let all = read_all(&super::repos::root(storage)?.join(FILE))?;
    Ok(all
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(repository))
        .map(|(_, profile)| profile.clone())
        .unwrap_or_default())
}

/// Every saved profile, keyed as saved.
pub fn all(storage: &Storage) -> Result<BTreeMap<String, Profile>> {
    read_all(&super::repos::root(storage)?.join(FILE))
}

/// Changes `repository`'s profile under the profiles' lock. `change` returns
/// what it changed; any change is validated, recorded in the history with
/// `by` and saved. Returns the profile and the changes.
pub fn update(
    storage: &Storage,
    repository: &str,
    by: &str,
    change: impl FnOnce(&mut Profile) -> Result<Vec<String>>,
) -> Result<(Profile, Vec<String>)> {
    super::repos::validate(repository)?;
    let root = super::repos::root(storage)?;
    let _lock = super::repos::lock(&root.join(LOCK), &Cancel::default())?;
    let path = root.join(FILE);
    let mut all = read_all(&path)?;
    let key = key(storage, &all, repository);
    let mut profile = all.get(&key).cloned().unwrap_or_default();
    let changes = change(&mut profile)?;
    if changes.is_empty() {
        return Ok((profile, changes));
    }
    validate(&profile)?;
    let at = now();
    profile.history.push(Change {
        at,
        by: short(by, 200),
        summary: summary(&changes),
    });
    let excess = profile.history.len().saturating_sub(HISTORY);
    profile.history.drain(..excess);
    profile.updated_at = Some(at);
    all.insert(key, profile.clone());
    storage::atomic_json(&path, &all)?;
    Ok((profile, changes))
}

/// Replaces `repository`'s tools, setup, variables and notes with `new`'s
/// (a client's edit); the history is kept and extended.
pub fn replace(
    storage: &Storage,
    repository: &str,
    by: &str,
    new: &Profile,
) -> Result<(Profile, Vec<String>)> {
    let new = normalized(new);
    validate(&new)?;
    update(storage, repository, by, |profile| {
        let changes = diff(profile, &new);
        profile.tools = new.tools;
        profile.setup = new.setup;
        profile.env = new.env;
        profile.notes = new.notes;
        Ok(changes)
    })
}

// Detection and resolution.

/// The contents of `file` under `root`, when it is a small regular file inside
/// `root`. Symlinks are never followed: the agent controls the worktree, and
/// difu reads it on the host.
fn read_inside(root: &Path, file: &str, limit: u64) -> Option<Vec<u8>> {
    let path = root.join(file);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    let canonical_root = root.canonicalize().ok()?;
    if !path.canonicalize().ok()?.starts_with(&canonical_root) {
        return None;
    }
    fs::read(&path).ok()
}

fn read_text(root: &Path, file: &str) -> Option<String> {
    String::from_utf8(read_inside(root, file, VERSION_FILE_LIMIT)?).ok()
}

/// A version as a file wrote it: trimmed, unquoted, without a leading `v`.
fn clean(version: &str) -> String {
    let version = version.trim().trim_matches(['"', '\'']).trim();
    match version.strip_prefix('v') {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest.to_owned(),
        _ => version.to_owned(),
    }
}

/// The text before a `#` that is outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            (None, '#') => return line.get(..index).unwrap_or(line),
            _ => {}
        }
    }
    line
}

fn unquote(text: &str) -> String {
    text.trim().trim_matches(['"', '\'']).trim().to_owned()
}

/// The first quoted string in `text`.
fn first_quoted(text: &str) -> Option<String> {
    let start = text.find(['"', '\''])?;
    let quote = text.get(start..start + 1)?;
    let rest = text.get(start + 1..)?;
    let end = rest.find(quote)?;
    rest.get(..end).map(str::to_owned)
}

/// `[tools]` of a mise config: `node = "24"`, `node = ["24", "22"]` (the first),
/// `node = { version = "24" }` and `[tools.node]` tables with `version`.
fn mise_tools(text: &str) -> Vec<(String, String)> {
    let mut tools = Vec::new();
    let mut inside = false;
    let mut table: Option<String> = None;
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && !line.contains('=') {
            let header = line.trim_matches(['[', ']']).trim();
            inside = header == "tools";
            table = header.strip_prefix("tools.").map(unquote);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if let Some(tool) = &table {
            if key.trim() == "version" {
                tools.push((tool.clone(), unquote(value)));
            }
            continue;
        }
        if !inside {
            continue;
        }
        let version = if value.starts_with('[') {
            first_quoted(value)
        } else if value.starts_with('{') {
            value
                .split_once("version")
                .and_then(|(_, rest)| rest.split_once('='))
                .and_then(|(_, rest)| first_quoted(rest))
        } else {
            Some(unquote(value))
        };
        if let Some(version) = version {
            tools.push((unquote(key), version));
        }
    }
    tools
}

/// The version an `engines` range most likely wants: the major version of
/// its last alternative's lower bound (`^20.10.0 || >=22` → `22`), or an exact
/// version as written.
fn engine_version(range: &str) -> Option<String> {
    let last = range.split("||").last()?.trim();
    let first = last.split_whitespace().next()?;
    let operator: String = first
        .chars()
        .take_while(|c| "<>=^~v".contains(*c))
        .collect();
    if operator.contains('<') {
        return None;
    }
    let version = first.trim_start_matches(|c: char| "<>=^~v".contains(c));
    let parts: Vec<&str> = version
        .split('.')
        .take_while(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let major = parts.first()?;
    if operator.is_empty() || operator == "=" {
        Some(parts.join("."))
    } else {
        Some((*major).to_owned())
    }
}

/// Version files `root` declares, each tool once, in mise's order of
/// precedence: mise configs, `.tool-versions`, idiomatic version files, then
/// `go.mod` and `package.json` (which mise does not read).
pub fn detect(root: &Path) -> Vec<Detected> {
    let mut found: Vec<Detected> = Vec::new();
    let mut add = |tool: &str, version: &str, source: &str, corepack: bool| {
        let version = clean(version);
        if version == "system"
            || !valid_tool_name(tool)
            || !valid_version(&version)
            || found.iter().any(|known| known.tool == tool)
        {
            return;
        }
        found.push(Detected {
            tool: tool.to_owned(),
            version,
            source: source.to_owned(),
            corepack,
        });
    };
    for file in [
        "mise.toml",
        ".mise.toml",
        "mise/config.toml",
        ".mise/config.toml",
        ".config/mise.toml",
        ".config/mise/config.toml",
    ] {
        if let Some(text) = read_text(root, file) {
            for (tool, version) in mise_tools(&text) {
                add(&tool, &version, file, false);
            }
        }
    }
    if let Some(text) = read_text(root, ".tool-versions") {
        for line in text.lines() {
            let mut words = strip_comment(line).split_whitespace();
            if let (Some(tool), Some(version)) = (words.next(), words.next()) {
                add(tool, version, ".tool-versions", false);
            }
        }
    }
    for (file, tool) in [
        (".nvmrc", "node"),
        (".node-version", "node"),
        (".python-version", "python"),
        (".ruby-version", "ruby"),
        (".go-version", "go"),
        (".java-version", "java"),
        (".terraform-version", "terraform"),
        (".bun-version", "bun"),
    ] {
        if let Some(line) = read_text(root, file).as_deref().and_then(first_line) {
            add(tool, &line, file, false);
        }
    }
    if let Some(text) = read_text(root, "rust-toolchain.toml") {
        let mut inside = false;
        for line in text.lines() {
            let line = strip_comment(line).trim();
            if line.starts_with('[') {
                inside = line == "[toolchain]";
            } else if inside
                && let Some((key, value)) = line.split_once('=')
                && key.trim() == "channel"
            {
                add("rust", &unquote(value), "rust-toolchain.toml", false);
            }
        }
    } else if let Some(line) = read_text(root, "rust-toolchain")
        .as_deref()
        .and_then(first_line)
    {
        add("rust", &line, "rust-toolchain", false);
    }
    if let Some(text) = read_text(root, "go.mod") {
        let directive = |name: &str| {
            text.lines()
                .filter_map(|line| line.trim().strip_prefix(name))
                .find_map(|rest| rest.starts_with(' ').then(|| rest.trim().to_owned()))
        };
        if let Some(toolchain) = directive("toolchain") {
            add(
                "go",
                toolchain.trim_start_matches("go"),
                "go.mod toolchain",
                false,
            );
        } else if let Some(go) = directive("go") {
            add("go", &go, "go.mod", false);
        }
    }
    if let Some(package) =
        read_text(root, "package.json").and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        if let Some(manager) = package.get("packageManager").and_then(Value::as_str)
            && let Some((tool, version)) = manager.split_once('@')
        {
            let version = version.split('+').next().unwrap_or(version);
            if ["pnpm", "yarn"].contains(&tool) {
                add(tool, version, "package.json packageManager", true);
            }
        }
        if let Some(version) = package
            .pointer("/engines/node")
            .and_then(Value::as_str)
            .and_then(engine_version)
        {
            add("node", &version, "package.json engines", false);
        }
    }
    found
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| strip_comment(line).trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The tools a sandbox provides: the repository's files win, the profile
/// fills the gaps.
pub fn resolve(profile: &Profile, detected: &[Detected]) -> Vec<Detected> {
    let mut resolved = detected.to_vec();
    for (tool, version) in &profile.tools {
        if !resolved.iter().any(|known| &known.tool == tool) {
            resolved.push(Detected {
                tool: tool.clone(),
                version: version.clone(),
                source: PROFILE.into(),
                corepack: false,
            });
        }
    }
    resolved
}

/// Repository files that declare tools the profile does not know yet.
pub fn learnable<'a>(profile: &Profile, detected: &'a [Detected]) -> Vec<&'a Detected> {
    detected
        .iter()
        .filter(|tool| tool.source != PROFILE && !profile.tools.contains_key(&tool.tool))
        .collect()
}

/// `node 24 (.nvmrc), python 3.12 (environment profile)`.
pub fn describe(resolved: &[Detected]) -> String {
    resolved
        .iter()
        .map(|tool| {
            format!(
                "{} {} ({}{})",
                tool.tool,
                tool.version,
                tool.source,
                if tool.corepack { ", via corepack" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn toml_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The worktree's global mise config (`MISE_GLOBAL_CONFIG_FILE`): every
/// resolved tool mise provides. mise reads the worktree's own files over it,
/// so a version file changed during the session still wins.
pub fn mise_config(repository: &str, resolved: &[Detected]) -> String {
    let mut text = format!(
        "# Written by difu for {repository} from the repository's version files and its\n\
         # environment profile. Files in the worktree override it; record changes with\n\
         # difu_environment instead of editing this file.\n\
         [settings]\n\
         idiomatic_version_file_enable_tools = [{}]\n\n\
         [tools]\n",
        IDIOMATIC
            .iter()
            .map(|tool| toml_string(tool))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for tool in resolved.iter().filter(|tool| !tool.corepack) {
        text.push_str(&format!(
            "{} = {}\n",
            toml_string(&tool.tool),
            toml_string(&tool.version)
        ));
    }
    text
}

/// The package managers corepack provides (`pnpm`, `yarn`).
pub fn corepack(resolved: &[Detected]) -> Vec<String> {
    resolved
        .iter()
        .filter(|tool| tool.corepack)
        .map(|tool| tool.tool.clone())
        .collect()
}

/// Identifies what the toolchain step installs.
pub fn tools_key(config: &str, resolved: &[Detected]) -> String {
    storage::hash(json!([config, corepack(resolved)]).to_string())
}

/// Identifies what setup commands depend on: the tools, the commands and
/// variables, and the worktree's lockfiles and manifests.
pub fn setup_key(root: &Path, tools_key: &str, profile: &Profile) -> String {
    let files: Vec<(&str, String)> = LOCKFILES
        .iter()
        .filter_map(|file| {
            read_inside(root, file, LOCKFILE_LIMIT).map(|bytes| (*file, storage::hash(bytes)))
        })
        .collect();
    storage::hash(
        json!({"tools": tools_key, "setup": profile.setup, "env": profile.env, "files": files})
            .to_string(),
    )
}

/// mise's install directory name for a tool (`npm:prettier` → `npm-prettier`).
fn install_name(tool: &str) -> String {
    tool.replace([':', '/'], "-")
}

/// Whether every resolved tool looks installed in the shared cache (`cache`
/// is its host path) and in the worktree's corepack shims (`state`).
pub fn installed(cache: &Path, state: &Path, resolved: &[Detected]) -> bool {
    resolved.iter().all(|tool| {
        if tool.corepack {
            state.join("bin").join(&tool.tool).exists()
        } else {
            let directory = cache
                .join("mise/installs")
                .join(install_name(&tool.tool))
                .join(&tool.version);
            fs::symlink_metadata(directory).is_ok()
        }
    })
}

// Per-worktree state.

/// What the last preparation of a worktree did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// [`tools_key`] of the last successful toolchain install.
    pub tools: Option<String>,
    /// [`setup_key`] of the last successful setup.
    pub setup: Option<String>,
    pub at: Option<i64>,
}

impl State {
    pub fn load(directory: &Path) -> Self {
        fs::read(directory.join("state.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&mut self, directory: &Path) -> Result<()> {
        self.at = Some(now());
        fs::create_dir_all(directory)?;
        storage::atomic_json(&directory.join("state.json"), self)
    }
}

/// `<git dir>/difu-environment` of the worktree at `workspace`.
pub fn state_dir(workspace: &Path) -> Option<PathBuf> {
    let output = process::run(
        crate::repo::git(workspace).args(["rev-parse", "--absolute-git-dir"]),
        None,
        &Cancel::default(),
    )
    .ok()
    .filter(|output| output.code == 0)?;
    let directory = PathBuf::from(
        String::from_utf8(output.stdout)
            .ok()?
            .trim_end_matches('\n'),
    );
    directory.is_absolute().then(|| directory.join(STATE))
}

/// Writes the worktree's mise config when it changed; returns its path.
pub fn write_config(directory: &Path, text: &str) -> Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let path = directory.join("mise.toml");
    if fs::read_to_string(&path).ok().as_deref() != Some(text) {
        let mut temp = tempfile::NamedTempFile::new_in(directory)?;
        temp.write_all(text.as_bytes())?;
        temp.persist(&path).map_err(|error| error.error)?;
    }
    Ok(path)
}

// The setup script and its outcome.

/// What one preparation runs in the sandbox.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub workspace: PathBuf,
    pub state: PathBuf,
    /// Run `mise install` (and enable corepack for `corepack`).
    pub tools: bool,
    pub corepack: Vec<String>,
    pub setup: Vec<String>,
}

/// `text` as one single-quoted shell word.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl Plan {
    /// A bash script for `bash -s`. Every step reads `/dev/null`, never the
    /// script, and prints [`MARK`] lines so [`parse`] can tell the steps apart.
    pub fn script(&self) -> String {
        let mut script = String::from("set -u\nexec 2>&1\n");
        script.push_str(&format!(
            "cd {} </dev/null || exit 97\n",
            shell_quote(&self.workspace.to_string_lossy())
        ));
        let mut step = |name: &str, command: &str| {
            script.push_str(&format!(
                "printf '\\n%s start %s\\n' '{MARK}' {name}\n{command} </dev/null\ndifu_code=$?\nprintf '\\n%s done %s %d\\n' '{MARK}' {name} \"$difu_code\"\n[ \"$difu_code\" -eq 0 ] || exit \"$difu_code\"\n",
                name = shell_quote(name),
            ));
        };
        if self.tools {
            step("tools", "mise install");
            if !self.corepack.is_empty() {
                let bin = self.state.join("bin");
                step(
                    "corepack",
                    &format!(
                        "mkdir -p {bin} && corepack enable --install-directory {bin} {}",
                        self.corepack.join(" "),
                        bin = shell_quote(&bin.to_string_lossy())
                    ),
                );
            }
        }
        for (index, command) in self.setup.iter().enumerate() {
            step(
                &format!("setup-{}", index + 1),
                &format!("bash -lc {}", shell_quote(command)),
            );
        }
        script.push_str(&format!(
            "printf '\\n%s report\\n' '{MARK}'\nmise ls --current --json </dev/null 2>/dev/null\nexit 0\n"
        ));
        script
    }

    /// The step a [`MARK`] name refers to, in words.
    pub fn label(&self, step: &str) -> String {
        match step {
            "tools" => "mise install".into(),
            "corepack" => "corepack enable".into(),
            _ => step
                .strip_prefix("setup-")
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1))
                .and_then(|index| self.setup.get(index))
                .map(|command| format!("`{}`", short(command, 120)))
                .unwrap_or_else(|| step.to_owned()),
        }
    }
}

/// How a setup script ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Steps that finished, with their exit codes.
    pub steps: Vec<(String, i32)>,
    /// The step that failed (or never finished), its exit code and the last
    /// lines it printed.
    pub failed: Option<(String, i32, String)>,
    /// `mise ls --current --json`, when the script got that far.
    pub report: Option<String>,
}

/// The last lines of a step's output, without blank lines or progress bars.
fn tail(lines: &[&str]) -> String {
    let kept: Vec<&str> = lines
        .iter()
        .flat_map(|line| line.split('\r'))
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = kept.len().saturating_sub(15);
    short(&kept.get(start..).unwrap_or(&[]).join("\n"), 2000)
}

/// Reads a setup script's output (stdout with stderr merged) and exit code.
pub fn parse(output: &str, code: i32) -> Outcome {
    let mut outcome = Outcome::default();
    let mut current: Option<(String, Vec<&str>)> = None;
    let mut report: Option<Vec<&str>> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(MARK) {
            let words: Vec<&str> = rest.split_whitespace().collect();
            match words.as_slice() {
                ["start", name] => {
                    current = Some(((*name).to_owned(), Vec::new()));
                }
                ["done", name, status] => {
                    let status = status.parse().unwrap_or(-1);
                    let lines = current.take().map(|(_, lines)| lines).unwrap_or_default();
                    outcome.steps.push(((*name).to_owned(), status));
                    if status != 0 && outcome.failed.is_none() {
                        outcome.failed = Some(((*name).to_owned(), status, tail(&lines)));
                    }
                }
                ["report"] => report = Some(Vec::new()),
                _ => {}
            }
            continue;
        }
        if let Some(lines) = report.as_mut() {
            lines.push(line);
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    if outcome.failed.is_none() {
        if let Some((name, lines)) = current {
            outcome.failed = Some((name, code, tail(&lines)));
        } else if report.is_none() && code != 0 {
            outcome.failed = Some((
                "start".into(),
                code,
                tail(&output.lines().collect::<Vec<_>>()),
            ));
        }
    }
    outcome.report = report
        .map(|lines| lines.join("\n"))
        .filter(|text| !text.trim().is_empty());
    outcome
}

/// Tools `mise ls --current --json` resolved from files inside `workspace`
/// (not from difu's config), as detections.
pub fn reported(report: &str, workspace: &Path) -> Vec<Detected> {
    let Ok(Value::Object(tools)) = serde_json::from_str::<Value>(report) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (tool, entries) in tools {
        let entries = match entries {
            Value::Array(entries) => entries,
            entry @ Value::Object(_) => vec![entry],
            _ => continue,
        };
        for entry in entries {
            let Some(path) = entry.pointer("/source/path").and_then(Value::as_str) else {
                continue;
            };
            let Ok(relative) = Path::new(path).strip_prefix(workspace) else {
                continue;
            };
            if relative.starts_with(".git") {
                continue;
            }
            let version = entry
                .get("requested_version")
                .or_else(|| entry.get("version"))
                .and_then(Value::as_str)
                .map(clean)
                .unwrap_or_default();
            if valid_tool_name(&tool)
                && valid_version(&version)
                && !found.iter().any(|known: &Detected| known.tool == tool)
            {
                found.push(Detected {
                    tool: tool.clone(),
                    version,
                    source: relative.display().to_string(),
                    corepack: false,
                });
            }
        }
    }
    found.sort_by(|a, b| a.tool.cmp(&b.tool));
    found
}

// The text form the terminal app edits.

const SECTIONS: [&str; 4] = ["tools", "setup", "env", "notes"];

/// The editable part of a profile as sectioned text (see [`from_text`]).
pub fn to_text(profile: &Profile) -> String {
    let mut text = String::from(
        "# Tools: `name = version`, as mise names them (node = 24, python = 3.12, npm:prettier = 3).\n[tools]\n",
    );
    for (tool, version) in &profile.tools {
        text.push_str(&format!("{tool} = {version}\n"));
    }
    text.push_str("\n# Setup: one command per line, run in each new worktree before the agent starts.\n[setup]\n");
    for command in &profile.setup {
        text.push_str(command);
        text.push('\n');
    }
    text.push_str("\n# Environment variables: `NAME = value`. Never secrets.\n[env]\n");
    for (name, value) in &profile.env {
        text.push_str(&format!("{name} = {value}\n"));
    }
    text.push_str("\n# Notes for agents, one per line.\n[notes]\n");
    for note in &profile.notes {
        text.push_str(note);
        text.push('\n');
    }
    text
}

/// Parses [`to_text`]'s format: `[tools]`, `[setup]`, `[env]` and `[notes]`
/// sections; blank lines and lines starting with `#` are ignored.
pub fn from_text(text: &str) -> Result<Profile> {
    let mut profile = Profile::default();
    let mut section: Option<&str> = None;
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim().to_ascii_lowercase();
            section = Some(
                SECTIONS
                    .iter()
                    .copied()
                    .find(|known| *known == name)
                    .with_context(|| {
                        format!(
                            "Line {}: unknown section [{name}]; use [tools], [setup], [env] or [notes]",
                            number + 1
                        )
                    })?,
            );
            continue;
        }
        let pair = || -> Result<(String, String)> {
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("Line {}: write `name = value`", number + 1))?;
            Ok((unquote(key), unquote(value)))
        };
        match section {
            Some("tools") => {
                let (tool, version) = pair()?;
                validate_tool(&tool, &version).with_context(|| format!("Line {}", number + 1))?;
                profile.tools.insert(tool, version);
            }
            Some("setup") => profile.setup.push(line.to_owned()),
            Some("env") => {
                let (name, value) = pair()?;
                profile.env.insert(name, value);
            }
            Some("notes") => profile.notes.push(line.to_owned()),
            _ => bail!(
                "Line {}: put each line under [tools], [setup], [env] or [notes]",
                number + 1
            ),
        }
    }
    let profile = normalized(&profile);
    validate(&profile)?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Result<(tempfile::TempDir, Storage)> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("config"))?;
        let storage = Storage {
            config: dir.path().join("config").join("config.json"),
            cache: dir.path().join("cache"),
        };
        Ok((dir, storage))
    }

    fn tool(tool: &str, version: &str, source: &str) -> Detected {
        Detected {
            tool: tool.into(),
            version: version.into(),
            source: source.into(),
            corepack: false,
        }
    }

    fn write(root: &Path, file: &str, text: &str) -> Result<()> {
        let path = root.join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
        Ok(())
    }

    #[test]
    fn profiles_are_stored_per_repository_with_a_bounded_history() -> Result<()> {
        let (_dir, storage) = fixture()?;
        storage.update_config(|config| config.server.repositories = vec!["Acme/Widgets".into()])?;
        assert_eq!(load(&storage, "acme/widgets")?, Profile::default());
        let edit = Edit {
            tools: vec![("node".into(), "24".into()), ("pnpm".into(), "9".into())],
            setup: vec!["pnpm install --frozen-lockfile".into()],
            env: vec![("NODE_OPTIONS".into(), "--max-old-space-size=4096".into())],
            notes: vec!["backend tests need Postgres via docker compose".into()],
            ..Edit::default()
        };
        let (saved, changes) = update(&storage, "acme/widgets", "Codex in session 1", |p| {
            apply(p, &edit)
        })?;
        assert_eq!(changes.len(), 5, "{changes:?}");
        assert_eq!(saved.tools.get("node").map(String::as_str), Some("24"));
        // Saved under the listed spelling and found case-insensitively.
        let all = all(&storage)?;
        assert_eq!(all.keys().collect::<Vec<_>>(), ["Acme/Widgets"]);
        let loaded = load(&storage, "ACME/widgets")?;
        assert_eq!(loaded, saved);
        let change = loaded.history.last().context("history")?;
        assert_eq!(change.by, "Codex in session 1");
        assert!(change.summary.contains("node 24"), "{}", change.summary);
        assert!(loaded.updated_at.is_some());
        // Nothing changed: no history entry.
        let (_, unchanged) = update(&storage, "acme/widgets", "again", |p| apply(p, &edit))?;
        assert!(unchanged.is_empty());
        assert_eq!(load(&storage, "acme/widgets")?.history.len(), 1);
        // A version change and a removal are recorded as such.
        let bump = Edit {
            tools: vec![("node".into(), "22".into())],
            forget_setup: vec!["pnpm install --frozen-lockfile".into()],
            ..Edit::default()
        };
        let (_, changes) = update(&storage, "acme/widgets", "user", |p| apply(p, &bump))?;
        assert_eq!(
            changes,
            [
                "removed setup `pnpm install --frozen-lockfile`",
                "node 24 → 22"
            ]
        );
        for round in 0..(HISTORY + 5) {
            update(&storage, "acme/widgets", "loop", |p| {
                apply(
                    p,
                    &Edit {
                        notes: vec![format!("note {round}")],
                        ..Edit::default()
                    },
                )
            })?;
        }
        let profile = load(&storage, "acme/widgets")?;
        assert_eq!(profile.history.len(), HISTORY);
        assert!(
            profile
                .history
                .last()
                .is_some_and(|change| change.summary.contains(&format!("note {}", HISTORY + 4)))
        );
        assert!(load(&storage, "../escape").is_err());
        Ok(())
    }

    #[test]
    fn replacing_keeps_the_history_and_ignores_client_history() -> Result<()> {
        let (_dir, storage) = fixture()?;
        let mut new = Profile::default();
        new.tools.insert(" python ".into(), " 3.12 ".into());
        new.setup = vec!["  uv sync ".into(), String::new(), "uv sync".into()];
        new.history = vec![Change {
            at: 1,
            by: "forged".into(),
            summary: "forged".into(),
        }];
        let (saved, changes) = replace(&storage, "acme/api", "Ali's iPhone", &new)?;
        assert_eq!(changes, ["python 3.12", "setup `uv sync`"]);
        assert_eq!(saved.setup, ["uv sync"]);
        assert_eq!(saved.history.len(), 1);
        assert_eq!(
            saved.history.first().map(|c| c.by.as_str()),
            Some("Ali's iPhone")
        );
        let (_, changes) = replace(&storage, "acme/api", "Mac", &Profile::default())?;
        assert_eq!(changes, ["removed tool python", "removed setup `uv sync`"]);
        assert_eq!(load(&storage, "acme/api")?.history.len(), 2);
        Ok(())
    }

    #[test]
    fn validation_refuses_bad_names_versions_commands_and_secrets() -> Result<()> {
        for (name, version) in [
            ("node", "24"),
            ("python", "3.12.4"),
            ("npm:prettier", "3"),
            ("ubi:BurntSushi/ripgrep", "latest"),
            ("node", "lts/iron"),
            ("java", "temurin-21"),
        ] {
            validate_tool(name, version)?;
        }
        for (name, version) in [
            ("", "1"),
            ("-node", "1"),
            ("node js", "1"),
            ("node;rm", "1"),
            ("node", ""),
            ("node", "24 && curl x"),
            ("node", "$(id)"),
            ("node", "-1"),
        ] {
            assert!(
                validate_tool(name, version).is_err(),
                "{name:?} {version:?}"
            );
        }
        validate_command("pnpm install --frozen-lockfile")?;
        for command in [
            "",
            "  ",
            "a\nb",
            "npm config set //r/:_authToken=npm_aB3dEf9hIjKlMnOpQrSt",
        ] {
            assert!(validate_command(command).is_err(), "{command:?}");
        }
        for (name, value) in [
            ("NODE_OPTIONS", "--max-old-space-size=4096"),
            (
                "DATABASE_URL",
                "postgres://postgres:postgres@localhost:5432/test",
            ),
            ("RUST_LOG", "info"),
            ("COMMIT", "0123456789abcdef0123456789abcdef01234567"),
            ("JAVA_HOME", "/opt/difu/toolchains/mise/installs/java/21"),
        ] {
            validate_variable(name, value)?;
        }
        for (name, value) in [
            ("GITHUB_TOKEN", "x"),
            ("NPM_TOKEN", "abc"),
            ("STRIPE_SECRET", "x"),
            ("DB_PASSWORD", "postgres"),
            ("AWS_ACCESS_KEY_ID", "x"),
            ("OPENAI_API_KEY", "x"),
            ("SIGNING_KEY", "x"),
            ("VALUE", "ghp_0123456789abcdefghijklmnopqrstuvwxyz"),
            ("VALUE", "sk-ant-oat01-abcdefghijklmnop"),
            ("VALUE", "AKIAABCDEFGHIJKLMNOP"),
            ("VALUE", "-----BEGIN OPENSSH PRIVATE KEY-----"),
            ("VALUE", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl"),
            ("VALUE", "Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MGFiY2RlZg"),
            ("PATH", "/usr/bin"),
            ("MISE_DATA_DIR", "/tmp"),
            ("LD_PRELOAD", "/x.so"),
            ("1BAD", "x"),
            ("BAD-NAME", "x"),
            ("MULTI", "a\nb"),
        ] {
            assert!(
                validate_variable(name, value).is_err(),
                "{name}={value} was accepted"
            );
        }
        validate_note("backend tests need Postgres via docker compose")?;
        assert!(validate_note("the token is ghp_0123456789abcdefghijklmnopqrstuvwxyz").is_err());
        assert!(validate_note("").is_err());
        let mut many = Profile::default();
        for index in 0..=MAX_TOOLS {
            many.tools.insert(format!("tool{index}"), "1".into());
        }
        assert!(validate(&many).is_err());
        // An invalid edit changes nothing.
        let mut profile = Profile::default();
        let error = apply(
            &mut profile,
            &Edit {
                tools: vec![("node".into(), "24".into())],
                env: vec![("API_TOKEN".into(), "x".into())],
                ..Edit::default()
            },
        );
        assert!(error.is_err());
        assert!(profile.is_empty());
        Ok(())
    }

    #[test]
    fn detection_reads_version_files_in_mises_order() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        write(
            root,
            "mise.toml",
            "[env]\nnode = \"not a tool\"\n\n[tools]\npython = \"3.12\" # comment\n\"npm:prettier\" = [\"3\", \"2\"]\nterraform = { version = \"1.9\", os = [\"linux\"] }\n\n[tools.go]\nversion = \"1.23\"\n",
        )?;
        write(
            root,
            ".tool-versions",
            "# pinned\nnode 22.11.0 20\npython 3.11\nruby system\n",
        )?;
        write(root, ".nvmrc", "v24.1.0\n")?;
        write(root, ".python-version", "3.10\n")?;
        write(
            root,
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"1.83.0\"\ncomponents = [\"clippy\"]\n",
        )?;
        write(
            root,
            "package.json",
            r#"{"packageManager":"pnpm@9.12.0+sha512.abcdef","engines":{"node":">=24"}}"#,
        )?;
        let detected = detect(root);
        assert_eq!(
            detected,
            [
                tool("python", "3.12", "mise.toml"),
                tool("npm:prettier", "3", "mise.toml"),
                tool("terraform", "1.9", "mise.toml"),
                tool("go", "1.23", "mise.toml"),
                tool("node", "22.11.0", ".tool-versions"),
                tool("rust", "1.83.0", "rust-toolchain.toml"),
                Detected {
                    corepack: true,
                    ..tool("pnpm", "9.12.0", "package.json packageManager")
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn detection_handles_idiomatic_files_go_mod_and_engines() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        write(root, ".node-version", "\n20.18.0\n")?;
        write(root, ".ruby-version", "ruby-3.3.0\n")?;
        write(root, "rust-toolchain", "stable\n")?;
        write(
            root,
            "go.mod",
            "module x\n\ngo 1.22\n\ntoolchain go1.22.5\n",
        )?;
        write(
            root,
            "package.json",
            r#"{"packageManager":"yarn@4.5.0","engines":{"node":"^20.10.0 || >=22"}}"#,
        )?;
        let detected = detect(root);
        assert_eq!(
            detected,
            [
                tool("node", "20.18.0", ".node-version"),
                tool("ruby", "ruby-3.3.0", ".ruby-version"),
                tool("rust", "stable", "rust-toolchain"),
                tool("go", "1.22.5", "go.mod toolchain"),
                Detected {
                    corepack: true,
                    ..tool("yarn", "4.5.0", "package.json packageManager")
                },
            ]
        );
        let only = tempfile::tempdir()?;
        write(only.path(), "go.mod", "module x\ngo 1.21.3\n")?;
        write(
            only.path(),
            "package.json",
            r#"{"engines":{"node":"24.x"}}"#,
        )?;
        assert_eq!(
            detect(only.path()),
            [
                tool("go", "1.21.3", "go.mod"),
                tool("node", "24", "package.json engines")
            ]
        );
        assert_eq!(engine_version(">=20.10.0"), Some("20".into()));
        assert_eq!(engine_version("22.11.0"), Some("22.11.0".into()));
        assert_eq!(engine_version("<20"), None);
        assert_eq!(engine_version("*"), None);
        Ok(())
    }

    #[test]
    fn detection_never_follows_symlinks_out_of_the_worktree() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let outside = dir.path().join("secret");
        fs::create_dir_all(&outside)?;
        fs::write(outside.join("token"), "abcdefTOKEN123\n")?;
        fs::write(outside.join("mise.toml"), "[tools]\nnode = \"99\"\n")?;
        let root = dir.path().join("worktree");
        fs::create_dir_all(&root)?;
        std::os::unix::fs::symlink(outside.join("token"), root.join(".nvmrc"))?;
        std::os::unix::fs::symlink(&outside, root.join(".config"))?;
        assert!(detect(&root).is_empty(), "{:?}", detect(&root));
        assert_eq!(
            setup_key(&root, "t", &Profile::default()),
            setup_key(&root, "t", &Profile::default())
        );
        Ok(())
    }

    #[test]
    fn repository_files_win_and_the_profile_fills_gaps() {
        let mut profile = Profile::default();
        profile.tools.insert("node".into(), "22".into());
        profile.tools.insert("python".into(), "3.12".into());
        profile.tools.insert("pnpm".into(), "8".into());
        let detected = [
            tool("node", "24", ".nvmrc"),
            Detected {
                corepack: true,
                ..tool("pnpm", "9.12.0", "package.json packageManager")
            },
        ];
        let resolved = resolve(&profile, &detected);
        assert_eq!(
            resolved,
            [
                tool("node", "24", ".nvmrc"),
                Detected {
                    corepack: true,
                    ..tool("pnpm", "9.12.0", "package.json packageManager")
                },
                tool("python", "3.12", PROFILE),
            ]
        );
        let config = mise_config("acme/widgets", &resolved);
        assert!(config.contains("[tools]\n\"node\" = \"24\"\n\"python\" = \"3.12\"\n"));
        assert!(!config.contains("pnpm"), "corepack provides pnpm: {config}");
        assert!(config.contains("idiomatic_version_file_enable_tools = [\"node\""));
        assert_eq!(corepack(&resolved), ["pnpm"]);
        // Without packageManager the profile's pnpm comes from mise.
        let resolved = resolve(&profile, &[]);
        assert!(mise_config("acme/widgets", &resolved).contains("\"pnpm\" = \"8\""));
        assert!(corepack(&resolved).is_empty());
        assert_eq!(
            describe(&[tool("node", "24", ".nvmrc")]),
            "node 24 (.nvmrc)"
        );
        // Only tools the profile lacks are learned.
        let declared = [tool("node", "24", ".nvmrc"), tool("go", "1.23", "go.mod")];
        let learned = learnable(&profile, &declared);
        assert_eq!(learned, [&tool("go", "1.23", "go.mod")]);
    }

    #[test]
    fn setup_runs_again_only_when_its_inputs_change() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let mut profile = Profile {
            setup: vec!["pnpm install --frozen-lockfile".into()],
            ..Profile::default()
        };
        write(root, "pnpm-lock.yaml", "lockfileVersion: 9\n")?;
        let first = setup_key(root, "tools", &profile);
        assert_eq!(first, setup_key(root, "tools", &profile));
        // Unrelated files do not matter.
        write(root, "src/main.ts", "console.log(1)\n")?;
        assert_eq!(first, setup_key(root, "tools", &profile));
        write(root, "pnpm-lock.yaml", "lockfileVersion: 9\npackages: {}\n")?;
        let changed = setup_key(root, "tools", &profile);
        assert_ne!(first, changed);
        assert_ne!(changed, setup_key(root, "other tools", &profile));
        profile.env.insert("CI".into(), "1".into());
        assert_ne!(changed, setup_key(root, "tools", &profile));
        // The state round-trips in the worktree's environment directory.
        let state_dir = root.join(".git-fixture").join(STATE);
        let mut state = State {
            tools: Some("t".into()),
            setup: Some(changed.clone()),
            at: None,
        };
        state.save(&state_dir)?;
        let loaded = State::load(&state_dir);
        assert_eq!(loaded.setup, Some(changed));
        assert!(loaded.at.is_some());
        assert_eq!(State::load(&root.join("missing")), State::default());
        Ok(())
    }

    #[test]
    fn installs_are_checked_in_the_cache_and_corepack_shims() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let cache = dir.path().join("cache");
        let state = dir.path().join("state");
        let resolved = [
            tool("node", "24", ".nvmrc"),
            tool("npm:prettier", "3", PROFILE),
            Detected {
                corepack: true,
                ..tool("pnpm", "9", "package.json packageManager")
            },
        ];
        assert!(!installed(&cache, &state, &resolved));
        fs::create_dir_all(cache.join("mise/installs/node/24.11.0"))?;
        std::os::unix::fs::symlink("24.11.0", cache.join("mise/installs/node/24"))?;
        fs::create_dir_all(cache.join("mise/installs/npm-prettier/3"))?;
        assert!(!installed(&cache, &state, &resolved));
        fs::create_dir_all(state.join("bin"))?;
        fs::write(state.join("bin/pnpm"), "")?;
        assert!(installed(&cache, &state, &resolved));
        Ok(())
    }

    #[test]
    fn the_state_directory_lives_in_the_worktrees_git_directory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        let base = root.join("base");
        fs::create_dir_all(&base)?;
        let git = |dir: &Path, args: &[&str]| -> Result<()> {
            process::checked(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir)
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
            )?;
            Ok(())
        };
        git(&base, &["init", "-q", "-b", "main"])?;
        git(&base, &["commit", "-q", "--allow-empty", "-m", "init"])?;
        let tree = root.join("tree");
        git(
            &base,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent",
                tree.to_str().context("path")?,
            ],
        )?;
        let state = state_dir(&tree).context("state directory")?;
        assert_eq!(state, base.join(".git/worktrees/tree").join(STATE));
        assert_eq!(
            state_dir(&base).context("base state")?,
            base.join(".git").join(STATE)
        );
        assert!(state_dir(&root.join("missing")).is_none());
        let path = write_config(&state, "[tools]\n")?;
        assert_eq!(fs::read_to_string(path)?, "[tools]\n");
        Ok(())
    }

    #[test]
    fn the_script_marks_each_step_and_parsing_finds_failures() {
        let plan = Plan {
            workspace: "/srv/worktrees/it's".into(),
            state: "/srv/repos/a/b/.git/worktrees/x/difu-environment".into(),
            tools: true,
            corepack: vec!["pnpm".into()],
            setup: vec!["pnpm install --frozen-lockfile".into(), "echo 'hi'".into()],
        };
        let script = plan.script();
        assert!(script.contains("cd '/srv/worktrees/it'\\''s' </dev/null || exit 97"));
        assert!(script.contains("\nmise install </dev/null\n"));
        assert!(script.contains(
            "corepack enable --install-directory '/srv/repos/a/b/.git/worktrees/x/difu-environment/bin' pnpm </dev/null"
        ));
        assert!(script.contains("bash -lc 'pnpm install --frozen-lockfile' </dev/null"));
        assert!(script.contains("bash -lc 'echo '\\''hi'\\''' </dev/null"));
        assert!(script.contains("mise ls --current --json"));
        assert_eq!(plan.label("setup-1"), "`pnpm install --frozen-lockfile`");
        assert_eq!(plan.label("tools"), "mise install");
        // No tools to install: only the setup commands run.
        let setup_only = Plan {
            tools: false,
            ..plan.clone()
        };
        assert!(!setup_only.script().contains("mise install"));

        let output = format!(
            "{MARK} start tools\nmise node@24 installed\n{MARK} done tools 0\n{MARK} start setup-1\nProgress 10%\rProgress 100%\nERR_PNPM_OUTDATED_LOCKFILE\n{MARK} done setup-1 1\n"
        );
        let outcome = parse(&output, 1);
        assert_eq!(
            outcome.steps,
            [("tools".to_owned(), 0), ("setup-1".to_owned(), 1)]
        );
        let (step, code, tail) = outcome.failed.clone().unwrap_or_default();
        assert_eq!((step.as_str(), code), ("setup-1", 1));
        assert!(tail.ends_with("ERR_PNPM_OUTDATED_LOCKFILE"), "{tail}");
        assert!(outcome.report.is_none());
        // A step that never finished (killed or timed out) is the failure.
        let outcome = parse(&format!("{MARK} start tools\ndownloading\n"), -1);
        assert_eq!(
            outcome.failed.map(|(step, code, _)| (step, code)),
            Some(("tools".to_owned(), -1))
        );
        let report = r#"{"node":[{"version":"24.11.0","requested_version":"24","source":{"type":".nvmrc","path":"/w/.nvmrc"}}]}"#;
        let outcome = parse(
            &format!("{MARK} start tools\n{MARK} done tools 0\n{MARK} report\n{report}\n"),
            0,
        );
        assert!(outcome.failed.is_none());
        assert_eq!(outcome.report.as_deref(), Some(report));
    }

    #[test]
    fn mise_reports_add_tools_declared_inside_the_worktree() {
        let report = r#"{
            "node": [{"version": "24.11.0", "requested_version": "24", "source": {"type": ".nvmrc", "path": "/w/.nvmrc"}}],
            "python": [{"version": "3.12.7", "source": {"type": "mise.toml", "path": "/w/.git/worktrees/x/difu-environment/mise.toml"}}],
            "deno": [{"version": "2.1.0", "requested_version": "2", "source": {"type": "mise.toml", "path": "/w/services/mise.toml"}}],
            "go": [{"version": "1.23.0"}]
        }"#;
        assert_eq!(
            reported(report, Path::new("/w")),
            [
                tool("deno", "2", "services/mise.toml"),
                tool("node", "24", ".nvmrc"),
            ]
        );
        assert!(reported("not json", Path::new("/w")).is_empty());
    }

    #[test]
    fn the_text_form_round_trips_and_explains_mistakes() -> Result<()> {
        let mut profile = Profile::default();
        profile.tools.insert("node".into(), "24".into());
        profile.tools.insert("npm:prettier".into(), "3".into());
        profile.setup = vec!["pnpm install --frozen-lockfile".into()];
        profile
            .env
            .insert("NODE_OPTIONS".into(), "--max-old-space-size=4096".into());
        profile.notes = vec!["backend tests need Postgres via docker compose".into()];
        let text = to_text(&profile);
        assert_eq!(from_text(&text)?, profile);
        assert_eq!(
            from_text(&to_text(&Profile::default()))?,
            Profile::default()
        );
        assert_eq!(
            from_text("[tools]\nnode = \"24\"\n[ENV]\nCI = true\n")?
                .env
                .get("CI"),
            Some(&"true".to_owned())
        );
        for (text, expected) in [
            ("node = 24\n", "Line 1: put each line under"),
            ("[tools]\nnode\n", "Line 2: write `name = value`"),
            ("[stuff]\n", "unknown section [stuff]"),
            ("[tools]\nnode = 24 && x\n", "Line 2"),
            ("[env]\nNPM_TOKEN = abc\n", "looks like a secret"),
        ] {
            let error = from_text(text).err().context("expected an error")?;
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
        Ok(())
    }
}
