//! Servers saved on a client machine (`<config>/servers.json`, 0600) and the
//! client commands that manage them (`join`, `list`, `remove`).
use super::client::{self, Connection};
use crate::storage::Storage;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufRead, IsTerminal, Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

/// One paired server. `id` is the server's certificate fingerprint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    pub name: String,
    pub id: String,
    pub hosts: Vec<String>,
    pub port: u16,
    pub fingerprint: String,
    pub device: String,
    pub token: String,
    /// SSH details, present when this computer onboarded the server.
    #[serde(default)]
    pub ssh: Option<SshAccess>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshAccess {
    pub address: String,
    pub port: u16,
    /// Private key generated for this server during onboarding.
    pub key: std::path::PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    servers: Vec<SavedServer>,
}

/// How long `list` and `remove` wait for a server before calling it unreachable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

pub fn path(storage: &Storage) -> Result<PathBuf> {
    Ok(storage
        .config
        .parent()
        .context("Missing config directory")?
        .join("servers.json"))
}

pub fn load(storage: &Storage) -> Result<Vec<SavedServer>> {
    let path = path(storage)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot read {}", path.display()));
        }
    };
    let document: Document = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "Cannot read {}; fix its JSON or move it aside",
            path.display()
        )
    })?;
    Ok(document.servers)
}

/// Insert or replace (by `id`, then `name`).
pub fn save(storage: &Storage, server: SavedServer) -> Result<()> {
    anyhow::ensure!(
        valid_name(&server.name),
        "Server names use lowercase letters, digits and dashes: {}",
        server.name
    );
    update(storage, |servers| {
        let slot = servers
            .iter()
            .position(|saved| saved.id == server.id)
            .or_else(|| servers.iter().position(|saved| saved.name == server.name));
        match slot.and_then(|index| servers.get_mut(index)) {
            Some(saved) => *saved = server,
            None => servers.push(server),
        }
        // A replaced id can leave a second entry with the same name behind.
        let mut names = std::collections::BTreeSet::new();
        servers.retain(|saved| names.insert(saved.name.clone()));
        Ok(())
    })
}

/// Forget a saved server; returns it when it existed.
pub fn forget(storage: &Storage, name: &str) -> Result<Option<SavedServer>> {
    let mut removed = None;
    update(storage, |servers| {
        if let Some(index) = servers.iter().position(|saved| saved.name == name) {
            removed = Some(servers.remove(index));
        }
        Ok(())
    })?;
    Ok(removed)
}

/// Locked read/modify/write of `servers.json`, which holds device tokens and
/// therefore stays 0600 (`atomic_json` writes through a private temp file).
fn update(
    storage: &Storage,
    change: impl FnOnce(&mut Vec<SavedServer>) -> Result<()>,
) -> Result<()> {
    let path = path(storage)?;
    let directory = path.parent().context("Missing config directory")?;
    fs::create_dir_all(directory)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))?;
    let _lock = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, error)| anyhow::anyhow!("Cannot lock saved servers: {error}"))?;
    let mut servers = load(storage)?;
    change(&mut servers)?;
    crate::storage::atomic_json(&path, &Document { servers })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `Ali's Hetzner Box` → `ali-s-hetzner-box`. Names prefix session ids
/// (`<name>:<session>`), so they never contain `:`.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-')
        .chars()
        .take(40)
        .collect::<String>()
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`; a bare IPv6 address has no port.
pub fn parse_address(address: &str) -> Result<(String, u16)> {
    let address = address.trim();
    anyhow::ensure!(!address.is_empty(), "Missing server address");
    let parse_port = |port: &str| -> Result<u16> {
        port.parse()
            .ok()
            .filter(|port| *port != 0)
            .with_context(|| format!("Invalid port in {address}"))
    };
    if let Some(rest) = address.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .with_context(|| format!("Invalid address {address}"))?;
        let port = match rest.strip_prefix(':') {
            Some(port) => parse_port(port)?,
            None if rest.is_empty() => super::DEFAULT_PORT,
            None => anyhow::bail!("Invalid address {address}"),
        };
        return Ok((host.to_owned(), port));
    }
    match address.split_once(':') {
        Some((host, port)) if !port.contains(':') => {
            anyhow::ensure!(!host.is_empty(), "Missing host in {address}");
            Ok((host.to_owned(), parse_port(port)?))
        }
        _ => Ok((address.to_owned(), super::DEFAULT_PORT)),
    }
}

/// The name a server shows for this computer in its device list.
pub fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        && output.status.success()
        && let Ok(name) = String::from_utf8(output.stdout)
        && !name.trim().is_empty()
    {
        return name.trim().to_owned();
    }
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .map(|name| name.trim_end_matches(".local").to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "difu".into())
}

pub fn join(storage: &Storage, address: &str, name: Option<String>) -> Result<()> {
    let (host, port) = parse_address(address)?;
    let saved = load(storage)?;
    if let Some(name) = &name {
        anyhow::ensure!(
            valid_name(name),
            "Server names use lowercase letters, digits and dashes, e.g. {}",
            slug(name)
        );
    }
    let password = read_password(&format!("Server password for {host}: "))?;
    anyhow::ensure!(!password.is_empty(), "No password entered");
    let paired = client::pair_with_password(&host, port, &password, &computer_name(), true)
        .with_context(|| format!("Cannot pair with {host}:{port}"))?;
    let name = name
        .or_else(|| Some(slug(&paired.name)).filter(|name| !name.is_empty()))
        .or_else(|| Some(slug(&host)).filter(|name| !name.is_empty()))
        .unwrap_or_else(|| "server".into());
    if let Some(other) = saved
        .iter()
        .find(|server| server.name == name && server.id != paired.server_id)
    {
        anyhow::bail!(
            "Another server is already saved as {} ({}). Choose a different name with --name, or run `difu server remove {}` first.",
            other.name,
            other.hosts.join(", "),
            other.name
        );
    }
    let previous = saved.iter().find(|server| server.id == paired.server_id);
    let server = SavedServer {
        name: name.clone(),
        id: paired.server_id.clone(),
        hosts: hosts(&host, &paired.hosts),
        port: if paired.port == 0 { port } else { paired.port },
        fingerprint: paired.fingerprint.clone(),
        device: paired.device.clone(),
        token: paired.token.clone(),
        ssh: previous.and_then(|server| server.ssh.clone()),
    };
    save(storage, server)?;
    println!(
        "Joined {name} ({host}:{port}) running difu {}.",
        paired.version
    );
    if !paired.full {
        println!(
            "warning: {name} granted standard access only; its sessions cannot appear in difu on this computer."
        );
    }
    if let Some(warning) = version_warning(&name, &paired.version) {
        println!("warning: {warning}");
    }
    println!("Its sessions appear in difu's session list while it is reachable.");
    Ok(())
}

/// The address that just worked first, then what the server advertises.
fn hosts(used: &str, advertised: &[String]) -> Vec<String> {
    let mut hosts = vec![used.to_owned()];
    for host in advertised {
        if !hosts.contains(host) {
            hosts.push(host.clone());
        }
    }
    hosts
}

pub fn version_warning(name: &str, version: &str) -> Option<String> {
    let ours = env!("CARGO_PKG_VERSION");
    (version != ours).then(|| {
        format!(
            "{name} runs difu {version} and this computer runs difu {ours}; run `difu server upgrade {name}` before using it from here."
        )
    })
}

pub fn list(storage: &Storage) -> Result<()> {
    let servers = load(storage)?;
    if servers.is_empty() {
        println!(
            "No servers saved. Set one up with `difu server add <ip>` or pair with `difu server join <host>`."
        );
        return Ok(());
    }
    let checks: Vec<_> = servers
        .iter()
        .map(|server| {
            let server = server.clone();
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(Connection::connect(&server).map(|c| c.welcome()));
            });
            rx
        })
        .collect();
    let mut rows = vec![[
        "NAME".to_owned(),
        "HOSTS".to_owned(),
        "VERSION".to_owned(),
        "STATUS".to_owned(),
    ]];
    let mut warnings = Vec::new();
    for (server, check) in servers.iter().zip(checks) {
        let hosts = format!("{} (port {})", server.hosts.join(", "), server.port);
        let (version, status) = match check.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(welcome)) => {
                if let Some(warning) = version_warning(&server.name, &welcome.version) {
                    warnings.push(warning);
                }
                let status = if !welcome.full {
                    "connected (standard access)".to_owned()
                } else if welcome.version != env!("CARGO_PKG_VERSION") {
                    "connected (version differs)".to_owned()
                } else {
                    "connected".to_owned()
                };
                (welcome.version, status)
            }
            Ok(Err(error)) => ("-".to_owned(), format!("unreachable: {error:#}")),
            Err(_) => ("-".to_owned(), "unreachable: timed out".to_owned()),
        };
        rows.push([server.name.clone(), hosts, version, status]);
    }
    print!("{}", table(&rows));
    for warning in warnings {
        println!("warning: {warning}");
    }
    Ok(())
}

fn table(rows: &[[String; 4]]) -> String {
    let mut widths = [0usize; 4];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (index, (cell, width)) in row.iter().zip(widths).enumerate() {
            if index + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  "));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

pub fn remove(storage: &Storage, name: &str) -> Result<()> {
    let servers = load(storage)?;
    let Some(server) = servers.iter().find(|server| server.name == name).cloned() else {
        let names: Vec<_> = servers.iter().map(|server| server.name.as_str()).collect();
        anyhow::bail!(
            "No saved server named {name}{}",
            if names.is_empty() {
                String::new()
            } else {
                format!(" (saved: {})", names.join(", "))
            }
        );
    };
    let (tx, rx) = mpsc::channel();
    let target = server.clone();
    thread::spawn(move || {
        let result = Connection::connect(&target)
            .and_then(|connection| connection.call("device.unpair", serde_json::json!({})));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(CONNECT_TIMEOUT) {
        Ok(Ok(_)) => println!("Revoked this computer on {name}."),
        Ok(Err(error)) => println!(
            "Could not revoke this computer on {name} ({error:#}). Revoke device {} later with `difu remote forget` on the server.",
            server.device
        ),
        Err(_) => println!(
            "Could not reach {name}. Revoke device {} later with `difu remote forget` on the server.",
            server.device
        ),
    }
    forget(storage, name)?;
    println!("Forgot {name}.");
    Ok(())
}

/// Reads a secret without echo when stdin is a terminal, or one line otherwise
/// (`echo "$PASSWORD" | difu server join host`).
fn read_password(prompt: &str) -> Result<String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    struct Raw;
    impl Drop for Raw {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    crossterm::terminal::enable_raw_mode()?;
    let raw = Raw;
    let mut bytes = Vec::new();
    let mut cancelled = false;
    for byte in stdin.lock().bytes() {
        match byte? {
            b'\r' | b'\n' => break,
            // Ctrl-C, or Ctrl-D on an empty line.
            3 => {
                cancelled = true;
                break;
            }
            4 if bytes.is_empty() => {
                cancelled = true;
                break;
            }
            // Backspace removes one whole UTF-8 character.
            8 | 127 => {
                while let Some(byte) = bytes.pop() {
                    if byte & 0xC0 != 0x80 {
                        break;
                    }
                }
            }
            // Ctrl-U clears the line.
            21 => bytes.clear(),
            byte => bytes.push(byte),
        }
    }
    drop(raw);
    eprintln!();
    anyhow::ensure!(!cancelled, "Cancelled");
    String::from_utf8(bytes).context("The password is not valid UTF-8")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn storage() -> Result<(tempfile::TempDir, Storage)> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        Ok((dir, storage))
    }

    pub(crate) fn server(name: &str, id: &str) -> SavedServer {
        SavedServer {
            name: name.into(),
            id: id.into(),
            hosts: vec!["203.0.113.7".into()],
            port: super::super::DEFAULT_PORT,
            fingerprint: format!("fingerprint-{id}"),
            device: "device".into(),
            token: format!("token-{id}"),
            ssh: None,
        }
    }

    #[test]
    fn saved_servers_round_trip_privately_and_replace_by_id_then_name() -> Result<()> {
        let (_dir, storage) = storage()?;
        assert!(load(&storage)?.is_empty());
        save(&storage, server("hetzner", "a"))?;
        save(&storage, server("home", "b"))?;
        let path = path(&storage)?;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            load(&storage)?,
            vec![server("hetzner", "a"), server("home", "b")]
        );
        // Same id: renamed and re-tokened in place.
        let mut renamed = server("box", "a");
        renamed.token = "fresh".into();
        save(&storage, renamed.clone())?;
        // Same name, new id (the server was rebuilt): replaced.
        save(&storage, server("home", "c"))?;
        assert_eq!(load(&storage)?, vec![renamed, server("home", "c")]);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert!(save(&storage, server("Not:Valid", "d")).is_err());
        assert_eq!(forget(&storage, "box")?.map(|s| s.id), Some("a".into()));
        assert_eq!(forget(&storage, "box")?, None);
        assert_eq!(load(&storage)?, vec![server("home", "c")]);
        Ok(())
    }

    #[test]
    fn saved_server_ssh_details_are_optional_in_the_file() -> Result<()> {
        let (_dir, storage) = storage()?;
        fs::write(
            path(&storage)?,
            r#"{"servers":[{"name":"box","id":"x","hosts":["h"],"port":3438,"fingerprint":"f","device":"d","token":"t"}]}"#,
        )?;
        let servers = load(&storage)?;
        let [saved] = servers.as_slice() else {
            anyhow::bail!("expected one server");
        };
        assert_eq!(saved.ssh, None);
        fs::write(path(&storage)?, "{broken")?;
        assert!(load(&storage).is_err());
        Ok(())
    }

    #[test]
    fn addresses_default_to_the_server_port() -> Result<()> {
        let default = super::super::DEFAULT_PORT;
        assert_eq!(
            parse_address("203.0.113.7")?,
            ("203.0.113.7".into(), default)
        );
        assert_eq!(
            parse_address(" box.example:4000 ")?,
            ("box.example".into(), 4000)
        );
        assert_eq!(
            parse_address("[2001:db8::1]:4000")?,
            ("2001:db8::1".into(), 4000)
        );
        assert_eq!(
            parse_address("[2001:db8::1]")?,
            ("2001:db8::1".into(), default)
        );
        assert_eq!(
            parse_address("2001:db8::1")?,
            ("2001:db8::1".into(), default)
        );
        assert!(parse_address("box:0").is_err());
        assert!(parse_address("box:http").is_err());
        assert!(parse_address(":4000").is_err());
        assert!(parse_address("").is_err());
        Ok(())
    }

    #[test]
    fn names_are_slugs_without_session_separators() -> Result<()> {
        assert_eq!(slug("Ali's Hetzner Box"), "ali-s-hetzner-box");
        assert_eq!(
            slug("  --ubuntu-4gb-fsn1-1.local "),
            "ubuntu-4gb-fsn1-1-local"
        );
        assert_eq!(slug("::"), "");
        assert!(valid_name("hetzner-2"));
        assert!(!valid_name("hetzner:2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Box"));
        Ok(())
    }

    #[test]
    fn version_warnings_name_the_upgrade_command() -> Result<()> {
        assert_eq!(version_warning("box", env!("CARGO_PKG_VERSION")), None);
        let warning = version_warning("box", "0.0.1").context("warning")?;
        assert!(warning.contains("difu server upgrade box"), "{warning}");
        assert_eq!(
            hosts("10.0.0.2", &["box.ts.net".into(), "10.0.0.2".into()]),
            vec!["10.0.0.2".to_owned(), "box.ts.net".to_owned()]
        );
        let text = table(&[
            [
                "NAME".into(),
                "HOSTS".into(),
                "VERSION".into(),
                "STATUS".into(),
            ],
            [
                "box".into(),
                "h :1".into(),
                "1.0.0".into(),
                "connected".into(),
            ],
        ]);
        assert_eq!(
            text,
            "NAME  HOSTS  VERSION  STATUS\nbox   h :1   1.0.0    connected\n"
        );
        Ok(())
    }
}
