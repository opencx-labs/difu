//! Claude's native permission settings, kept in their original scopes.
use super::{Scope, SettingKind, SettingRow, SettingsEdit, Snapshot};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const KINDS: [SettingKind; 8] = [
    SettingKind::Allow,
    SettingKind::Ask,
    SettingKind::Deny,
    SettingKind::AutoAllow,
    SettingKind::AutoSoftDeny,
    SettingKind::AutoHardDeny,
    SettingKind::AutoEnvironment,
    SettingKind::Directory,
];
const SCOPES: [Scope; 3] = [Scope::User, Scope::Project, Scope::Local];
const MAX_CONFIG_SIZE: u64 = 4 * 1024 * 1024;

struct Paths {
    cwd: PathBuf,
    user: PathBuf,
    project: PathBuf,
    local: PathBuf,
    managed: Vec<PathBuf>,
}
impl Paths {
    fn discover(cwd: &Path) -> Result<Self> {
        let cwd = fs::canonicalize(cwd).context("Resolve Claude working directory")?;
        let user_root = user_root(std::env::var_os("CLAUDE_CONFIG_DIR"))?;
        let user_root = resolve_parent(&user_root)?;
        let local_root = local_root(&cwd);
        let system = if cfg!(target_os = "macos") {
            PathBuf::from("/Library/Application Support/ClaudeCode")
        } else if cfg!(target_os = "windows") {
            PathBuf::from(
                std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
            )
            .join("ClaudeCode")
        } else {
            PathBuf::from("/etc/claude-code")
        };
        let mut managed = vec![
            user_root.join("remote-settings.json"),
            system.join("managed-settings.json"),
        ];
        if let Ok(entries) = fs::read_dir(system.join("managed-settings.d")) {
            let mut drop_ins = entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension().is_some_and(|ext| ext == "json")
                        && !path
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                })
                .collect::<Vec<_>>();
            drop_ins.sort();
            managed.extend(drop_ins);
        }
        Ok(Self {
            project: cwd.join(".claude/settings.json"),
            local: local_root.join(".claude/settings.local.json"),
            user: user_root.join("settings.json"),
            cwd,
            managed,
        })
    }
    fn path(&self, scope: Scope) -> Result<&Path> {
        match scope {
            Scope::User => Ok(&self.user),
            Scope::Project => Ok(&self.project),
            Scope::Local => Ok(&self.local),
            Scope::Session => bail!("Session settings have no file"),
        }
    }
}
fn resolve_parent(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .context("Settings directory has no parent")?;
    if parent.exists() {
        return Ok(fs::canonicalize(parent)?.join(
            absolute
                .file_name()
                .context("Settings directory has no name")?,
        ));
    }
    Ok(resolve_parent(parent)?.join(
        absolute
            .file_name()
            .context("Settings directory has no name")?,
    ))
}
fn user_root(config: Option<OsString>) -> Result<PathBuf> {
    match config.filter(|value| !value.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(dirs::home_dir()
            .context("Find home directory for Claude settings")?
            .join(".claude")),
    }
}
/// Claude stores local approvals at the main repository root, including when
/// launched from a subdirectory or linked worktree. Shared project settings
/// remain relative to the session's primary working directory.
fn local_root(cwd: &Path) -> PathBuf {
    let Some(root) = cwd.ancestors().find(|path| path.join(".git").exists()) else {
        return cwd.to_path_buf();
    };
    if dirs::home_dir().is_some_and(|home| home == root) || cfg!(target_os = "windows") {
        return cwd.to_path_buf();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = fs::metadata(cwd).ok().map(|meta| meta.uid());
        if [root.to_path_buf(), root.join(".git"), root.join(".claude")]
            .iter()
            .filter_map(|path| fs::metadata(path).ok())
            .any(|meta| Some(meta.uid()) != owner)
        {
            return cwd.to_path_buf();
        }
    }
    if root.join(".git").is_file()
        && let Ok(git) = fs::read_to_string(root.join(".git"))
        && let Some(directory) = git.trim().strip_prefix("gitdir: ")
    {
        let gitdir = root.join(directory);
        if let Ok(common) = fs::read_to_string(gitdir.join("commondir"))
            && let Ok(common) = fs::canonicalize(gitdir.join(common.trim()))
            && let Some(main) = common.parent()
        {
            return main.to_path_buf();
        }
    }
    root.to_path_buf()
}
fn location(kind: SettingKind) -> (&'static str, &'static str) {
    match kind {
        SettingKind::Allow => ("permissions", "allow"),
        SettingKind::Ask => ("permissions", "ask"),
        SettingKind::Deny => ("permissions", "deny"),
        SettingKind::Directory => ("permissions", "additionalDirectories"),
        SettingKind::AutoAllow => ("autoMode", "allow"),
        SettingKind::AutoSoftDeny => ("autoMode", "soft_deny"),
        SettingKind::AutoHardDeny => ("autoMode", "hard_deny"),
        SettingKind::AutoEnvironment => ("autoMode", "environment"),
    }
}
fn is_rule(kind: SettingKind) -> bool {
    matches!(
        kind,
        SettingKind::Allow | SettingKind::Ask | SettingKind::Deny
    )
}
fn is_auto(kind: SettingKind) -> bool {
    location(kind).0 == "autoMode"
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn session_revision(value: &Value) -> Result<String> {
    Ok(revision(&serde_json::to_vec(value)?))
}
fn validate(value: &Value) -> Result<()> {
    ensure!(value.is_object(), "Claude settings must be a JSON object");
    for section in ["permissions", "autoMode"] {
        if let Some(value) = value.get(section) {
            ensure!(
                value.is_object(),
                "Claude {section} settings must be an object"
            );
        }
    }
    for kind in KINDS {
        let (section, key) = location(kind);
        if let Some(array) = value.get(section).and_then(|value| value.get(key)) {
            ensure!(
                array
                    .as_array()
                    .is_some_and(|array| array.iter().all(Value::is_string)),
                "Claude {section}.{key} must be an array of strings"
            );
        }
    }
    Ok(())
}
fn overlay(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Object(Map::new()));
    }
    validate(value)?;
    Ok(value.clone())
}
struct Source {
    value: Value,
    revision: String,
}
fn read_source(path: &Path) -> Result<Source> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Source {
                value: Value::Object(Map::new()),
                revision: "missing".into(),
            });
        }
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_SIZE + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_SIZE,
        "Claude settings file is too large: {}",
        path.display()
    );
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("Malformed Claude settings: {}", path.display()))?;
    validate(&value).with_context(|| format!("Invalid Claude settings: {}", path.display()))?;
    Ok(Source {
        value,
        revision: revision(&bytes),
    })
}
fn values(value: &Value, kind: SettingKind) -> impl Iterator<Item = &str> {
    let (section, key) = location(kind);
    value
        .get(section)
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}
fn message(snapshot: &mut Snapshot, text: impl AsRef<str>) {
    match &mut snapshot.message {
        Some(message) => {
            message.push_str("; ");
            message.push_str(text.as_ref());
        }
        None => snapshot.message = Some(text.as_ref().into()),
    }
}
fn managed_only(paths: &Paths) -> bool {
    paths
        .managed
        .iter()
        .filter_map(|path| read_source(path).ok())
        .any(|source| {
            source.value.get("allowManagedPermissionRulesOnly") == Some(&Value::Bool(true))
        })
}
fn push_rows(
    snapshot: &mut Snapshot,
    source: &Source,
    scope: Option<Scope>,
    name: &str,
    editable: bool,
    auto: bool,
) {
    for kind in KINDS {
        if is_auto(kind) && !auto {
            continue;
        }
        for value in values(&source.value, kind) {
            if is_auto(kind) && value == "$defaults" {
                continue;
            }
            snapshot.rows.push(SettingRow {
                kind,
                scope,
                value: value.into(),
                editable: editable && (!is_auto(kind) || scope == Some(Scope::User)),
                source: name.into(),
                revision: source.revision.clone(),
            });
        }
    }
}
fn populate_sources(snapshot: &mut Snapshot, paths: &Paths, session: &Value) -> Result<()> {
    snapshot.rows.clear();
    snapshot.revisions.clear();
    let managed_only = managed_only(paths);
    for scope in SCOPES {
        let path = paths.path(scope)?;
        match read_source(path) {
            Ok(source) => {
                snapshot.revisions.insert(scope, source.revision.clone());
                let editable = check_destination(path).is_ok();
                push_rows(
                    snapshot,
                    &source,
                    Some(scope),
                    &path.display().to_string(),
                    editable,
                    scope == Scope::User,
                );
            }
            Err(error) => message(snapshot, format!("{error:#}")),
        }
    }
    for path in &paths.managed {
        match read_source(path) {
            Ok(source) => push_rows(
                snapshot,
                &source,
                None,
                &format!("{} (managed; read-only)", path.display()),
                false,
                true,
            ),
            Err(error) => message(snapshot, format!("{error:#}")),
        }
    }
    let value = overlay(session)?;
    let revision = session_revision(session)?;
    snapshot.revisions.insert(Scope::Session, revision.clone());
    push_rows(
        snapshot,
        &Source { value, revision },
        Some(Scope::Session),
        "Session override (--settings)",
        true,
        true,
    );
    if managed_only {
        for row in &mut snapshot.rows {
            if row.scope.is_some() && is_rule(row.kind) {
                row.editable = false;
                row.source
                    .push_str(" (ignored: managed permission rules only)");
            }
        }
    }
    snapshot.rows.push(SettingRow {
        kind: SettingKind::Directory,
        scope: None,
        value: paths.cwd.display().to_string(),
        editable: false,
        source: "Original working directory (read-only)".into(),
        revision: String::new(),
    });
    Ok(())
}
/// Read only permission-related fields; malformed files remain untouched and
/// appear as notices, while the remaining sources can still be inspected.
pub fn populate(snapshot: &mut Snapshot, cwd: &Path, session_settings: &Value) -> Result<()> {
    let paths = Paths::discover(cwd)?;
    populate_sources(snapshot, &paths, session_settings)?;
    match auto_config(&paths, session_settings) {
        Ok((defaults, effective)) => populate_auto(snapshot, &defaults, &effective),
        Err(error) => message(
            snapshot,
            format!("Auto mode configuration unavailable: {error:#}"),
        ),
    }
    Ok(())
}
fn check_destination(path: &Path) -> Result<()> {
    // Canonicalize the trusted working-directory base during discovery, but
    // refuse symlinks in the settings filename and every remaining parent.
    for component in path.ancestors() {
        match fs::symlink_metadata(component) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "Refusing symlink settings destination: {}",
                component.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn apply(value: &mut Value, edit: &SettingsEdit) -> Result<()> {
    ensure!(
        edit.before.is_some() || edit.after.is_some(),
        "No settings change supplied"
    );
    if let Some(value) = edit.after.as_deref() {
        ensure!(
            !value.trim().is_empty(),
            "A permission entry cannot be empty"
        );
        ensure!(
            !value.contains('\0'),
            "A permission entry cannot contain NUL"
        );
    }
    if is_auto(edit.kind) {
        ensure!(
            edit.scope == Scope::User,
            "Auto mode custom entries can only be changed in User settings"
        );
        ensure!(
            edit.before.as_deref() != Some("$defaults")
                && edit.after.as_deref() != Some("$defaults"),
            "Claude built-in defaults are read-only"
        );
    }
    let (section, key) = location(edit.kind);
    let object = value
        .as_object_mut()
        .context("Claude settings must be an object")?;
    let section = object
        .entry(section)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("Claude settings section must be an object")?;
    let first_auto = is_auto(edit.kind) && !section.contains_key(key);
    let entries = section
        .entry(key)
        .or_insert_with(|| {
            if first_auto {
                serde_json::json!(["$defaults"])
            } else {
                serde_json::json!([])
            }
        })
        .as_array_mut()
        .context("Claude settings entries must be an array")?;
    if let Some(before) = edit.before.as_deref() {
        let index = entries
            .iter()
            .position(|value| value.as_str() == Some(before))
            .context("Entry is no longer present in the selected scope; refresh permissions")?;
        if let Some(after) = edit.after.as_deref() {
            *entries.get_mut(index).context("Missing permission entry")? =
                Value::String(after.into());
        } else {
            entries.remove(index);
        }
    } else if let Some(after) = edit.after.as_deref() {
        ensure!(
            !entries.iter().any(|value| value.as_str() == Some(after)),
            "Entry already exists in the selected scope"
        );
        entries.push(Value::String(after.into()));
    }
    Ok(())
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn edit_with(paths: &Paths, session: &Value, edit: &SettingsEdit) -> Result<Value> {
    ensure!(
        !is_rule(edit.kind) || !managed_only(paths),
        "Managed settings only permit organization permission rules"
    );
    let mut normalized = edit.clone();
    if edit.kind == SettingKind::Directory
        && let Some(after) = edit.after.as_deref()
    {
        let after = after.trim();
        ensure!(
            !after.starts_with("//") && !after.starts_with("\\\\"),
            "Network paths cannot be added as working directories"
        );
        let path = if after == "~" {
            dirs::home_dir().context("Find home directory")?
        } else if let Some(relative) = after.strip_prefix("~/") {
            dirs::home_dir()
                .context("Find home directory")?
                .join(relative)
        } else {
            paths.cwd.join(after)
        };
        let path = fs::canonicalize(&path)
            .with_context(|| format!("Working directory does not exist: {}", path.display()))?;
        ensure!(
            path.is_dir(),
            "Working directory must be a directory: {}",
            path.display()
        );
        normalized.after = Some(
            path.to_str()
                .context("Working directory must be UTF-8")?
                .into(),
        );
    }
    let edit = &normalized;
    if edit.scope == Scope::Session {
        ensure!(
            session_revision(session)? == edit.revision,
            "Session settings changed; refresh permissions"
        );
        let mut value = overlay(session)?;
        apply(&mut value, edit)?;
        return Ok(value);
    }
    let path = paths.path(edit.scope)?;
    check_destination(path)?;
    let mut source = read_source(path)?;
    ensure!(
        source.revision == edit.revision,
        "Settings changed on disk; refresh permissions before editing {}",
        path.display()
    );
    apply(&mut source.value, edit)?;
    let parent = path
        .parent()
        .context("Claude settings file has no parent directory")?;
    fs::create_dir_all(parent)?;
    check_destination(path)?;
    let lockpath = path.with_extension("json.difu-lock");
    let _lockfile = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lockpath)
        .context(
            "Claude settings are being edited by another Difu process; retry after it finishes",
        )?;
    let _lock = Lock(lockpath);
    ensure!(
        read_source(path)?.revision == edit.revision,
        "Settings changed on disk; refresh permissions"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    serde_json::to_writer_pretty(temporary.as_file_mut(), &source.value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    check_destination(path)?;
    ensure!(
        read_source(path)?.revision == edit.revision,
        "Settings changed on disk; refresh permissions"
    );
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Save {}", path.display()))?;
    Ok(session.clone())
}
/// Return a session overlay or atomically update exactly one selected file.
pub fn edit(cwd: &Path, session_settings: &Value, edit: &SettingsEdit) -> Result<Value> {
    edit_with(&Paths::discover(cwd)?, session_settings, edit)
}
fn disabled(cwd: &Path, key: &str) -> bool {
    let Ok(paths) = Paths::discover(cwd) else {
        return false;
    };
    SCOPES
        .into_iter()
        .filter_map(|scope| paths.path(scope).ok())
        .chain(paths.managed.iter().map(PathBuf::as_path))
        .filter_map(|path| read_source(path).ok())
        .any(|source| {
            source.value.get(key).and_then(Value::as_str) == Some("disable")
                || source
                    .value
                    .get("permissions")
                    .and_then(|value| value.get(key))
                    .and_then(Value::as_str)
                    == Some("disable")
        })
}
pub fn has_auto_disabled(cwd: &Path) -> bool {
    disabled(cwd, "disableAutoMode")
}
pub fn has_bypass_disabled(cwd: &Path) -> bool {
    disabled(cwd, "disableBypassPermissionsMode")
}

/// Only the two documented read-only inspection subcommands are accepted.
/// Spool stdout to a private temporary file, avoiding pipe-buffer deadlocks.
fn inspect(
    program: &std::ffi::OsStr,
    paths: &Paths,
    session: &Value,
    subcommand: &str,
    timeout: Duration,
) -> Result<Value> {
    ensure!(
        matches!(subcommand, "config" | "defaults"),
        "Unsupported auto mode inspection command"
    );
    let mut output = tempfile::tempfile()?;
    let mut command = Command::new(program);
    command
        .current_dir(&paths.cwd)
        .env(
            "CLAUDE_CONFIG_DIR",
            paths.user.parent().context("User settings directory")?,
        )
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null());
    let session = overlay(session)?;
    let mut selected = Map::new();
    // Never send unrelated settings (including hooks or environment secrets) to
    // an auxiliary inspection process.
    for key in ["permissions", "autoMode"] {
        if let Some(value) = session.get(key) {
            selected.insert(key.into(), value.clone());
        }
    }
    if !selected.is_empty() {
        command
            .arg("--settings")
            .arg(serde_json::to_string(&selected)?);
    }
    command.args(["auto-mode", subcommand]);
    let mut child = command
        .spawn()
        .context("Start Claude auto mode inspection")?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Claude auto mode inspection timed out");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
    };
    ensure!(
        status.success(),
        "Claude auto-mode {subcommand} failed ({status})"
    );
    use std::io::{Seek, SeekFrom};
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output.take(MAX_CONFIG_SIZE + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_SIZE,
        "Claude auto mode inspection output is too large"
    );
    let value: Value = serde_json::from_slice(&bytes)
        .context("Claude returned invalid auto mode configuration JSON")?;
    ensure!(
        value.is_object(),
        "Claude auto mode configuration is not an object"
    );
    for key in ["allow", "soft_deny", "hard_deny", "environment"] {
        ensure!(
            value
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().all(Value::is_string)),
            "Claude auto mode {key} is not an array of strings"
        );
    }
    Ok(value)
}
fn auto_config(paths: &Paths, session: &Value) -> Result<(Value, Value)> {
    let defaults = inspect(
        std::ffi::OsStr::new("claude"),
        paths,
        &Value::Null,
        "defaults",
        Duration::from_secs(3),
    )?;
    let config = inspect(
        std::ffi::OsStr::new("claude"),
        paths,
        session,
        "config",
        Duration::from_secs(3),
    )?;
    Ok((defaults, config))
}
fn auto_values(value: &Value, kind: SettingKind) -> impl Iterator<Item = &str> {
    value
        .get(location(kind).1)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}
fn populate_auto(snapshot: &mut Snapshot, defaults: &Value, effective: &Value) {
    for row in &mut snapshot.rows {
        if is_auto(row.kind) && !auto_values(effective, row.kind).any(|value| value == row.value) {
            row.source
                .push_str(" (inactive in native effective configuration)");
        }
    }
    for kind in KINDS.into_iter().filter(|kind| is_auto(*kind)) {
        for value in auto_values(defaults, kind) {
            let active = auto_values(effective, kind).any(|entry| entry == value);
            snapshot.rows.push(SettingRow {
                kind,
                scope: None,
                value: value.into(),
                editable: false,
                source: if active {
                    "Claude built-in default (read-only)"
                } else {
                    "Claude built-in default (inactive: replaced by configuration)"
                }
                .into(),
                revision: String::new(),
            });
        }
        for value in auto_values(effective, kind) {
            if !snapshot
                .rows
                .iter()
                .any(|row| row.kind == kind && row.value == value)
            {
                snapshot.rows.push(SettingRow {
                    kind,
                    scope: None,
                    value: value.into(),
                    editable: false,
                    source: "Claude effective configuration (managed or external; read-only)"
                        .into(),
                    revision: String::new(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> Result<(TempDir, Paths)> {
        let temp = tempfile::tempdir()?;
        let base = fs::canonicalize(temp.path())?;
        let cwd = base.join("project");
        fs::create_dir_all(cwd.join(".claude"))?;
        fs::create_dir_all(base.join("user"))?;
        let paths = Paths {
            project: cwd.join(".claude/settings.json"),
            local: cwd.join(".claude/settings.local.json"),
            user: base.join("user/settings.json"),
            managed: vec![base.join("managed-settings.json")],
            cwd,
        };
        Ok((temp, paths))
    }
    fn snapshot(paths: &Paths, session: &Value) -> Result<Snapshot> {
        let mut result = Snapshot::default();
        populate_sources(&mut result, paths, session)?;
        Ok(result)
    }
    fn change(
        paths: &Paths,
        session: &Value,
        scope: Scope,
        kind: SettingKind,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<SettingsEdit> {
        let snapshot = snapshot(paths, session)?;
        Ok(SettingsEdit {
            scope,
            kind,
            before: before.map(String::from),
            after: after.map(String::from),
            revision: snapshot
                .revisions
                .get(&scope)
                .context("scope revision")?
                .clone(),
        })
    }

    #[test]
    fn editing_one_source_preserves_other_scopes_and_unknown_fields() -> Result<()> {
        let (_temp, paths) = fixture()?;
        let original = json!({"permissions":{"allow":["Read"],"deny":["Write"],"defaultMode":"plan"},"hooks":{"untouched":true},"unknown":[1,2]});
        fs::write(&paths.project, serde_json::to_vec(&original)?)?;
        fs::write(
            &paths.user,
            r#"{"permissions":{"allow":["Bash(git status)"]}}"#,
        )?;
        let session = json!({"permissions":{"ask":["WebFetch"]}});
        let edit = change(
            &paths,
            &session,
            Scope::Project,
            SettingKind::Allow,
            Some("Read"),
            Some("Read(src/**)"),
        )?;
        assert_eq!(edit_with(&paths, &session, &edit)?, session);
        let saved: Value = serde_json::from_slice(&fs::read(&paths.project)?)?;
        assert_eq!(
            saved.pointer("/permissions/allow"),
            Some(&json!(["Read(src/**)"]))
        );
        assert_eq!(saved.get("hooks"), original.get("hooks"));
        assert_eq!(saved.get("unknown"), original.get("unknown"));
        assert_eq!(
            saved.pointer("/permissions/deny"),
            original.pointer("/permissions/deny")
        );
        assert_eq!(
            saved.pointer("/permissions/defaultMode"),
            original.pointer("/permissions/defaultMode")
        );
        assert_eq!(
            fs::read_to_string(&paths.user)?,
            r#"{"permissions":{"allow":["Bash(git status)"]}}"#
        );
        Ok(())
    }

    #[test]
    fn session_edits_never_flatten_or_remove_inherited_rules() -> Result<()> {
        let (_temp, paths) = fixture()?;
        fs::write(&paths.user, r#"{"permissions":{"allow":["Read"]}}"#)?;
        let session = json!({"model":"keep"});
        let remove = change(
            &paths,
            &session,
            Scope::Session,
            SettingKind::Allow,
            Some("Read"),
            None,
        )?;
        assert!(edit_with(&paths, &session, &remove).is_err());
        let add = change(
            &paths,
            &session,
            Scope::Session,
            SettingKind::Ask,
            None,
            Some("Bash(git push *)"),
        )?;
        let updated = edit_with(&paths, &session, &add)?;
        assert_eq!(
            updated,
            json!({"model":"keep","permissions":{"ask":["Bash(git push *)"]}})
        );
        assert_eq!(
            fs::read_to_string(&paths.user)?,
            r#"{"permissions":{"allow":["Read"]}}"#
        );
        Ok(())
    }

    #[test]
    fn stale_revision_rejects_changes_including_unknown_fields() -> Result<()> {
        let (_temp, paths) = fixture()?;
        let session = Value::Null;
        let edit = change(
            &paths,
            &session,
            Scope::Local,
            SettingKind::Deny,
            None,
            Some("Write"),
        )?;
        fs::write(&paths.local, r#"{"theme":"dark"}"#)?;
        assert!(edit_with(&paths, &session, &edit).is_err());
        assert_eq!(fs::read_to_string(&paths.local)?, r#"{"theme":"dark"}"#);
        Ok(())
    }

    #[test]
    fn malformed_settings_are_not_replaced() -> Result<()> {
        let (_temp, paths) = fixture()?;
        let edit = change(
            &paths,
            &Value::Null,
            Scope::User,
            SettingKind::Allow,
            None,
            Some("Read"),
        )?;
        for malformed in [
            "{",
            "[]",
            r#"{"permissions":false}"#,
            r#"{"permissions":{"allow":"Read"}}"#,
            r#"{"permissions":{"deny":[3]}}"#,
        ] {
            fs::write(&paths.user, malformed)?;
            assert!(edit_with(&paths, &Value::Null, &edit).is_err());
            assert_eq!(fs::read_to_string(&paths.user)?, malformed);
        }
        Ok(())
    }

    #[test]
    fn auto_custom_rules_are_user_only_and_keep_native_defaults() -> Result<()> {
        let (_temp, paths) = fixture()?;
        let session = Value::Null;
        for kind in [
            SettingKind::AutoAllow,
            SettingKind::AutoSoftDeny,
            SettingKind::AutoHardDeny,
            SettingKind::AutoEnvironment,
        ] {
            let add = change(
                &paths,
                &session,
                Scope::User,
                kind,
                None,
                Some("Trusted\nteam environment"),
            )?;
            edit_with(&paths, &session, &add)?;
            let illegal = change(
                &paths,
                &session,
                Scope::Project,
                kind,
                None,
                Some("bad scope"),
            )?;
            assert!(edit_with(&paths, &session, &illegal).is_err());
        }
        let saved: Value = serde_json::from_slice(&fs::read(&paths.user)?)?;
        for key in ["allow", "soft_deny", "hard_deny", "environment"] {
            assert_eq!(
                saved.get("autoMode").and_then(|v| v.get(key)),
                Some(&json!(["$defaults", "Trusted\nteam environment"]))
            );
        }
        let builtin = change(
            &paths,
            &session,
            Scope::User,
            SettingKind::AutoAllow,
            Some("$defaults"),
            None,
        )?;
        assert!(edit_with(&paths, &session, &builtin).is_err());
        Ok(())
    }

    #[test]
    fn snapshot_keeps_provenance_managed_read_only_and_original_directory() -> Result<()> {
        let (_temp, paths) = fixture()?;
        fs::write(
            &paths.user,
            r#"{"permissions":{"allow":["Read"],"additionalDirectories":["../shared"]},"autoMode":{"environment":["my team"]}}"#,
        )?;
        fs::write(
            &paths.project,
            r#"{"permissions":{"allow":["Read"]},"autoMode":{"allow":["ignored repo rule"]}}"#,
        )?;
        fs::write(
            paths.managed.first().context("managed")?,
            r#"{"permissions":{"deny":["Write"]},"autoMode":{"hard_deny":["organization rule"]}}"#,
        )?;
        let result = snapshot(&paths, &json!({"permissions":{"ask":["Bash"]}}))?;
        assert_eq!(result.rows.iter().filter(|r| r.value == "Read").count(), 2);
        assert!(
            result
                .rows
                .iter()
                .any(|r| r.value == "Bash" && r.scope == Some(Scope::Session) && r.editable)
        );
        assert!(result.rows.iter().any(|r| r.value == "Write"
            && r.scope.is_none()
            && !r.editable
            && r.source.contains("managed-settings.json")));
        assert!(
            result
                .rows
                .iter()
                .any(|r| r.value == paths.cwd.to_string_lossy() && !r.editable)
        );
        assert!(result.rows.iter().any(|r| r.value == "../shared"
            && r.kind == SettingKind::Directory
            && r.scope == Some(Scope::User)));
        assert!(!result.rows.iter().any(|r| r.value == "ignored repo rule"));
        assert_eq!(result.revisions.len(), 4);
        Ok(())
    }

    #[test]
    fn managed_permission_only_does_not_disable_auto_customization() -> Result<()> {
        let (_temp, paths) = fixture()?;
        fs::write(
            paths.managed.first().context("managed")?,
            r#"{"allowManagedPermissionRulesOnly":true,"permissions":{"deny":["Write"]}}"#,
        )?;
        let denied = change(
            &paths,
            &Value::Null,
            Scope::User,
            SettingKind::Allow,
            None,
            Some("Read"),
        )?;
        assert!(edit_with(&paths, &Value::Null, &denied).is_err());
        let auto = change(
            &paths,
            &Value::Null,
            Scope::User,
            SettingKind::AutoEnvironment,
            None,
            Some("My organization"),
        )?;
        edit_with(&paths, &Value::Null, &auto)?;
        let rows = snapshot(&paths, &Value::Null)?.rows;
        assert!(
            rows.iter()
                .any(|row| row.value == "My organization" && row.editable)
        );
        Ok(())
    }

    #[test]
    fn native_auto_defaults_remain_read_only_and_report_replaced_sections() -> Result<()> {
        let mut snapshot = Snapshot::default();
        snapshot.rows.push(SettingRow {
            kind: SettingKind::AutoAllow,
            scope: Some(Scope::User),
            value: "my exception".into(),
            editable: true,
            source: "user/settings.json".into(),
            revision: "abc".into(),
        });
        populate_auto(
            &mut snapshot,
            &json!({"allow":["native exception"],"soft_deny":["native block"],"hard_deny":[],"environment":["native context"]}),
            &json!({"allow":["my exception"],"soft_deny":["native block"],"hard_deny":["remote policy"],"environment":["native context"]}),
        );
        assert!(
            snapshot
                .rows
                .iter()
                .any(|row| row.value == "native exception"
                    && !row.editable
                    && row.source.contains("inactive"))
        );
        assert!(snapshot.rows.iter().any(|row| row.value == "native block"
            && !row.editable
            && !row.source.contains("inactive")));
        assert!(
            snapshot
                .rows
                .iter()
                .any(|row| row.value == "remote policy" && !row.editable && row.scope.is_none())
        );
        assert_eq!(
            snapshot
                .rows
                .iter()
                .filter(|row| row.value == "my exception")
                .count(),
            1
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn native_inspection_only_runs_read_commands_with_bounded_timeout() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, paths) = fixture()?;
        let fake = paths.cwd.join("claude");
        fs::write(
            &fake,
            "#!/bin/sh\ncase \"$*\" in\n*auto-mode\\ defaults*|*auto-mode\\ config*) printf '%s' '{\"allow\":[],\"soft_deny\":[],\"hard_deny\":[],\"environment\":[\"native\"]}';;\n*) exit 4;;\nesac\n",
        )?;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700))?;
        let result = inspect(
            fake.as_os_str(),
            &paths,
            &json!({"autoMode":{"allow":["custom"]}}),
            "config",
            Duration::from_secs(1),
        )?;
        assert_eq!(result.get("environment"), Some(&json!(["native"])));
        assert!(
            inspect(
                fake.as_os_str(),
                &paths,
                &Value::Null,
                "reset",
                Duration::from_secs(1)
            )
            .is_err()
        );
        fs::write(&fake, "#!/bin/sh\nexec /bin/sleep 5\n")?;
        let start = Instant::now();
        assert!(
            inspect(
                fake.as_os_str(),
                &paths,
                &Value::Null,
                "defaults",
                Duration::from_millis(30)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        Ok(())
    }

    #[test]
    fn workspace_additions_validate_directories_and_store_canonical_paths() -> Result<()> {
        let (_temp, paths) = fixture()?;
        let shared = paths.cwd.join("shared");
        fs::create_dir(&shared)?;
        for invalid in ["missing", "./file.txt", "//server/share"] {
            fs::write(paths.cwd.join("file.txt"), "file")?;
            let edit = change(
                &paths,
                &Value::Null,
                Scope::Session,
                SettingKind::Directory,
                None,
                Some(invalid),
            )?;
            assert!(edit_with(&paths, &Value::Null, &edit).is_err(), "{invalid}");
        }
        let add = change(
            &paths,
            &Value::Null,
            Scope::Session,
            SettingKind::Directory,
            None,
            Some("./shared/../shared"),
        )?;
        let updated = edit_with(&paths, &Value::Null, &add)?;
        assert_eq!(
            updated.pointer("/permissions/additionalDirectories"),
            Some(&json!([fs::canonicalize(shared)?.to_string_lossy()]))
        );
        Ok(())
    }

    #[test]
    fn local_settings_follow_main_root_in_subdirectories_and_linked_worktrees() -> Result<()> {
        let (_temp, paths) = fixture()?;
        fs::create_dir(paths.cwd.join(".git"))?;
        let sub = paths.cwd.join("sub");
        fs::create_dir(&sub)?;
        assert_eq!(local_root(&sub), paths.cwd);
        let linked = paths.cwd.parent().context("parent")?.join("linked");
        fs::create_dir(&linked)?;
        let gitdir = paths.cwd.join(".git/worktrees/linked");
        fs::create_dir_all(&gitdir)?;
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )?;
        fs::write(gitdir.join("commondir"), "../..\n")?;
        assert_eq!(local_root(&linked), paths.cwd);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_file_and_parent_destinations_are_rejected() -> Result<()> {
        let (_temp, paths) = fixture()?;
        let target = paths.cwd.join("other.json");
        fs::write(&target, "{}")?;
        let edit = change(
            &paths,
            &Value::Null,
            Scope::User,
            SettingKind::Allow,
            None,
            Some("Read"),
        )?;
        std::os::unix::fs::symlink(&target, &paths.user)?;
        assert!(edit_with(&paths, &Value::Null, &edit).is_err());
        fs::remove_file(&paths.user)?;
        fs::remove_dir(paths.user.parent().context("parent")?)?;
        std::os::unix::fs::symlink(&paths.cwd, paths.user.parent().context("parent")?)?;
        assert!(edit_with(&paths, &Value::Null, &edit).is_err());
        assert_eq!(fs::read_to_string(target)?, "{}");
        Ok(())
    }
}
