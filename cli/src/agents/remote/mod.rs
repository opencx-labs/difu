//! Remote control for the difu iOS app on the same Wi-Fi and, on a cloud server
//! (`difu serve`), for any paired phone or computer. The agent service listens
//! only while enabled. A device pairs once by scanning a short-lived, single-use
//! QR code or with the server password; afterwards it authenticates with its own
//! device token. TLS uses a self-signed certificate whose fingerprint travels in
//! the QR code (or is bound by the password proof), so clients pin this machine
//! without trusting any certificate authority. See docs/remote-protocol.md and
//! docs/cloud-agents.md.
mod api;
mod commands;
mod defaults;
mod identity;
mod listener;
mod network;
mod reviews;
mod server_api;
mod view;

use crate::{
    cloud::password::Verifier,
    storage::{self, Storage},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// A pairing code shown in the QR modal stays valid this long and works once.
const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);
/// Why a server refuses to turn remote control off.
pub const SERVER_STAYS_ON: &str = "This is a difu server: remote control stays on so its computers and phones can always reach their sessions. Revoke one device with `difu remote forget`, turn password pairing off with `difu remote password --disable`, or stop the server with `systemctl stop difu-agent`.";
pub const PROTOCOL: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    /// Enable remote control if needed and show a fresh pairing code.
    Open,
    Disable,
    Forget {
        device: String,
    },
    /// Store the server password's verifier, or turn password pairing off.
    Password {
        verifier: Option<Verifier>,
    },
    /// Add advertised hosts (after clearing them, if asked), change the port
    /// and set the display name (an empty name returns to the host name).
    Configure {
        hosts: Vec<String>,
        clear_hosts: bool,
        port: Option<u16>,
        #[serde(default)]
        name: Option<String>,
    },
}

/// `standard` devices use the phone API; `full` devices may also use `rpc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    #[default]
    Standard,
    Full,
}

impl Access {
    fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub listening: bool,
    pub port: u16,
    pub name: String,
    pub hosts: Vec<String>,
    /// `difu://pair?...` for the QR code, while a pairing code is valid.
    pub pairing: Option<String>,
    pub pairing_expires: Option<i64>,
    pub devices: Vec<DeviceStatus>,
    pub error: Option<String>,
    /// Hosts from `difu remote config`, advertised ahead of discovered ones
    /// (instead of them on a server).
    #[serde(default)]
    pub configured_hosts: Vec<String>,
    /// A server password is set, so password pairing is on.
    #[serde(default)]
    pub password: bool,
    /// Server mode (`difu serve`).
    #[serde(default)]
    pub server: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    pub paired: i64,
    pub seen: Option<i64>,
    pub connected: bool,
    #[serde(default)]
    pub access: Access,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    enabled: bool,
    port: u16,
    devices: Vec<Device>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hosts: Vec<String>,
    /// Shown to clients instead of the host name (`difu remote config --name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// The server password's verifier; never the password itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<Verifier>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Device {
    id: String,
    name: String,
    /// SHA-256 of the device token; the token itself is only on the device.
    token: String,
    paired: i64,
    #[serde(default)]
    seen: Option<i64>,
    #[serde(default, skip_serializing_if = "Access::is_standard")]
    access: Access,
}

/// A device that just paired, as announced to the other connected devices.
struct Enrolled {
    id: String,
    name: String,
    token: String,
    paired: i64,
    access: Access,
}

struct Pairing {
    code: String,
    expires: i64,
}

struct State {
    settings: Settings,
    pairing: Option<Pairing>,
    server: Option<listener::Server>,
    error: Option<String>,
}

pub struct Remote {
    storage: Storage,
    home: PathBuf,
    /// Listen only on this address; tests bind loopback.
    bind: Option<String>,
    identity: Mutex<Option<Arc<identity::Identity>>>,
    state: Mutex<State>,
    peers: listener::Peers,
    guard: listener::Guard,
}

impl Remote {
    /// Restore the saved setting; an enabled remote starts listening right away.
    pub fn start(storage: Storage) -> Arc<Self> {
        Self::open(storage, std::env::var("DIFU_REMOTE_BIND").ok())
    }

    fn open(storage: Storage, bind: Option<String>) -> Arc<Self> {
        let home = home(&storage);
        let mut error = None;
        let settings = load(&home).unwrap_or_else(|e| {
            error = Some(format!("{e:#}"));
            Settings::default()
        });
        let remote = Arc::new(Self {
            storage,
            home,
            bind,
            identity: Mutex::new(None),
            state: Mutex::new(State {
                settings,
                pairing: None,
                server: None,
                error,
            }),
            peers: listener::Peers::default(),
            guard: listener::Guard::default(),
        });
        if remote.lock().map(|s| s.settings.enabled).unwrap_or(false)
            && let Err(error) = remote.listen()
        {
            eprintln!("Remote control: {error:#}");
            // A server has no one to press "retry"; keep trying while the port is busy.
            if remote.server_mode() {
                let remote = remote.clone();
                let _ = std::thread::Builder::new()
                    .name("difu-remote-retry".into())
                    .spawn(move || {
                        while remote.enabled() && !remote.listening() {
                            std::thread::sleep(Duration::from_secs(5));
                            let _ = remote.listen();
                        }
                    });
            }
        }
        remote
    }

    pub fn command(self: &Arc<Self>, command: Command) -> Result<Status> {
        match command {
            Command::Status => {}
            Command::Open => {
                {
                    let mut state = self.lock()?;
                    if !state.settings.enabled {
                        state.settings.enabled = true;
                        save(&self.home, &state.settings)?;
                    }
                    let now = chrono::Utc::now().timestamp_millis();
                    // Keep a code that is still being scanned; replace one about to expire.
                    if state
                        .pairing
                        .as_ref()
                        .is_none_or(|p| p.expires - now < 60_000)
                    {
                        state.pairing = Some(Pairing {
                            code: identity::random_token(16)?,
                            expires: now + PAIRING_TTL.as_millis() as i64,
                        });
                    }
                }
                self.listen()?;
            }
            // `difu remote`, `rpc` from a full device and the terminal app on
            // the server all arrive here.
            Command::Disable => {
                ensure!(!self.server_mode(), SERVER_STAYS_ON);
                self.stop_listening()?;
            }
            Command::Forget { device } => {
                self.forget(&device, true)?;
            }
            Command::Password { verifier } => {
                let mut state = self.lock()?;
                state.settings.password = verifier;
                save(&self.home, &state.settings)?;
            }
            Command::Configure {
                hosts,
                clear_hosts,
                port,
                name,
            } => self.configure(hosts, clear_hosts, port, name)?,
        }
        self.status()
    }

    /// Turns remote control off and disconnects every device.
    fn stop_listening(&self) -> Result<()> {
        let server = {
            let mut state = self.lock()?;
            state.settings.enabled = false;
            state.pairing = None;
            state.error = None;
            save(&self.home, &state.settings)?;
            state.server.take()
        };
        if let Some(server) = server {
            server.stop();
        }
        self.peers.close_all();
        Ok(())
    }

    /// Tests stop their listener even in server mode, where `Disable` is refused.
    #[cfg(test)]
    pub(crate) fn shut_down(&self) -> Result<()> {
        self.stop_listening()
    }

    fn configure(
        self: &Arc<Self>,
        hosts: Vec<String>,
        clear_hosts: bool,
        port: Option<u16>,
        name: Option<String>,
    ) -> Result<()> {
        let hosts = hosts
            .iter()
            .map(|host| advertised_host(host))
            .collect::<Result<Vec<_>>>()?;
        let name = name.as_deref().map(display_name).transpose()?;
        ensure!(port != Some(0), "Choose a port between 1 and 65535");
        let moved = {
            let state = self.lock()?;
            port.filter(|port| {
                *port != state.settings.port
                    && state.server.as_ref().is_some_and(listener::Server::running)
            })
        };
        // Bind the new port before letting go of the old one.
        let replacement = match moved {
            Some(port) => Some(listener::Server::bind(
                self.clone(),
                self.identity()?,
                port,
                true,
            )?),
            None => None,
        };
        let previous = {
            let mut state = self.lock()?;
            if clear_hosts {
                state.settings.hosts.clear();
            }
            for host in hosts {
                if !state.settings.hosts.contains(&host) {
                    state.settings.hosts.push(host);
                }
            }
            if let Some(port) = port {
                state.settings.port = port;
            }
            if let Some(name) = name {
                state.settings.name = name;
            }
            let previous = match replacement {
                Some((server, port)) => {
                    state.settings.port = port;
                    if self.server_mode() {
                        eprintln!("Remote control listening on port {port}");
                    }
                    state.server.replace(server)
                }
                None => None,
            };
            save(&self.home, &state.settings)?;
            previous
        };
        if let Some(server) = previous {
            server.stop();
        }
        Ok(())
    }

    /// Revoke a device. Its other connections notice within a second; `close`
    /// also drops them right away (not wanted while answering its own request).
    fn forget(&self, device: &str, close: bool) -> Result<()> {
        let mut state = self.lock()?;
        state.settings.devices.retain(|d| d.id != device);
        save(&self.home, &state.settings)?;
        drop(state);
        if close {
            self.peers.close_device(device);
        }
        Ok(())
    }

    pub fn status(&self) -> Result<Status> {
        let identity = self.identity().ok();
        let mut state = self.lock()?;
        let now = chrono::Utc::now().timestamp_millis();
        if state.pairing.as_ref().is_some_and(|p| p.expires <= now) {
            state.pairing = None;
        }
        let listening = state.server.as_ref().is_some_and(listener::Server::running);
        let hosts = if listening {
            self.advertised(&state.settings.hosts)
        } else {
            Vec::new()
        };
        let name = state.settings.name.clone().unwrap_or_else(network::name);
        let pairing = match (&state.pairing, &identity, listening) {
            (Some(pairing), Some(identity), true) => Some(pairing_uri(
                &name,
                &hosts,
                state.settings.port,
                &identity.fingerprint,
                &pairing.code,
            )),
            _ => None,
        };
        let connected = self.peers.devices();
        Ok(Status {
            enabled: state.settings.enabled,
            listening,
            port: state.settings.port,
            name,
            hosts,
            pairing_expires: pairing
                .as_ref()
                .and(state.pairing.as_ref())
                .map(|p| p.expires),
            pairing,
            devices: state
                .settings
                .devices
                .iter()
                .map(|d| DeviceStatus {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    paired: d.paired,
                    seen: d.seen,
                    connected: connected.contains(&d.id),
                    access: d.access,
                })
                .collect(),
            error: state.error.clone(),
            configured_hosts: state.settings.hosts.clone(),
            password: state.settings.password.is_some(),
            server: self.server_mode(),
        })
    }

    /// The name clients show: the configured one, else the computer's name.
    fn name(&self) -> String {
        self.lock()
            .ok()
            .and_then(|s| s.settings.name.clone())
            .unwrap_or_else(network::name)
    }

    /// The hosts in pairing links, `welcome` and `server.info`; see [`advertise`].
    fn hosts(&self) -> Vec<String> {
        let configured = self
            .lock()
            .map(|s| s.settings.hosts.clone())
            .unwrap_or_default();
        self.advertised(&configured)
    }

    fn advertised(&self, configured: &[String]) -> Vec<String> {
        let discovered = match &self.bind {
            Some(bind) => vec![bind.clone()],
            None => network::hosts(),
        };
        advertise(configured, discovered, self.server_mode())
    }

    /// `difu serve` turned server mode on in this installation's settings.
    fn server_mode(&self) -> bool {
        api::config(&self.storage).server.enabled
    }

    fn verifier(&self) -> Option<Verifier> {
        self.lock().ok().and_then(|s| s.settings.password.clone())
    }

    fn listening(&self) -> bool {
        self.lock()
            .is_ok_and(|s| s.server.as_ref().is_some_and(listener::Server::running))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("Remote control state lock failed"))
    }

    fn identity(&self) -> Result<Arc<identity::Identity>> {
        let mut cached = self
            .identity
            .lock()
            .map_err(|_| anyhow::anyhow!("Remote identity lock failed"))?;
        if let Some(identity) = cached.as_ref() {
            return Ok(identity.clone());
        }
        let identity = Arc::new(identity::Identity::load_or_create(&self.home)?);
        *cached = Some(identity.clone());
        Ok(identity)
    }

    fn listen(self: &Arc<Self>) -> Result<()> {
        let result = (|| -> Result<()> {
            if self
                .lock()?
                .server
                .as_ref()
                .is_some_and(listener::Server::running)
            {
                return Ok(());
            }
            let identity = self.identity()?;
            let preferred = self.lock()?.settings.port;
            // Clients of a server expect its fixed port; a computer may move.
            let server_mode = self.server_mode();
            let (server, port) =
                listener::Server::bind(self.clone(), identity, preferred, server_mode)?;
            let mut state = self.lock()?;
            if state.settings.port != port {
                state.settings.port = port;
                save(&self.home, &state.settings)?;
            }
            state.server = Some(server);
            state.error = None;
            if server_mode {
                eprintln!("Remote control listening on port {port}");
            }
            Ok(())
        })();
        if let Err(error) = &result
            && let Ok(mut state) = self.lock()
        {
            state.error = Some(format!("{error:#}"));
        }
        result
    }

    /// Exchange the QR code for a long-lived device token. The code works once.
    fn pair(&self, code: &str, id: &str, name: &str, access: Access) -> Result<Enrolled> {
        let mut state = self.lock()?;
        let now = chrono::Utc::now().timestamp_millis();
        let valid = state
            .pairing
            .as_ref()
            .is_some_and(|p| p.expires > now && identity::same(&p.code, code));
        ensure!(
            valid,
            "{}",
            if self.server_mode() {
                "This pairing code expired or was already used. Run `difu remote pair` on the server for a new code."
            } else {
                "This pairing code expired or was already used. Open /remote-control on your Mac for a new code."
            }
        );
        state.pairing = None;
        self.enroll(&mut state, id, name, access)
    }

    /// Pair a device whose password proof the listener already checked.
    fn pair_with_password(&self, id: &str, name: &str, access: Access) -> Result<Enrolled> {
        let mut state = self.lock()?;
        ensure!(
            state.settings.password.is_some(),
            "Password pairing is off on this server"
        );
        self.enroll(&mut state, id, name, access)
    }

    fn enroll(&self, state: &mut State, id: &str, name: &str, access: Access) -> Result<Enrolled> {
        let now = chrono::Utc::now().timestamp_millis();
        let id = if valid_device_id(id) {
            id.to_owned()
        } else {
            identity::random_token(12)?
        };
        let token = identity::random_token(32)?;
        let name: String = name.trim().chars().take(80).collect();
        let name = if name.is_empty() {
            "iPhone".into()
        } else {
            name
        };
        state.settings.devices.retain(|d| d.id != id);
        state.settings.devices.push(Device {
            id: id.clone(),
            name: name.clone(),
            token: storage::hash(&token),
            paired: now,
            seen: Some(now),
            access,
        });
        save(&self.home, &state.settings)?;
        Ok(Enrolled {
            id,
            name,
            token,
            paired: now,
            access,
        })
    }

    fn authenticate(&self, token: &str) -> Result<(String, Access)> {
        let hash = storage::hash(token);
        let mut state = self.lock()?;
        let device = state
            .settings
            .devices
            .iter_mut()
            .find(|d| identity::same(&d.token, &hash))
            .with_context(|| {
                if self.server_mode() {
                    "This device is no longer paired with this server. Pair it again."
                } else {
                    "This phone is no longer paired. Scan the QR code in /remote-control again."
                }
            })?;
        device.seen = Some(chrono::Utc::now().timestamp_millis());
        let found = (device.id.clone(), device.access);
        save(&self.home, &state.settings)?;
        Ok(found)
    }

    fn paired(&self, device: &str) -> bool {
        self.lock()
            .is_ok_and(|s| s.settings.devices.iter().any(|d| d.id == device))
    }

    fn enabled(&self) -> bool {
        self.lock().is_ok_and(|s| s.settings.enabled)
    }
}

/// `difu serve`: remote control on, at the port from the flag, the saved
/// settings or the default, in that order. Runs before the service starts.
pub fn prepare_server(storage: &Storage, port: Option<u16>) -> Result<u16> {
    let home = home(storage);
    let mut settings = load(&home)?;
    settings.enabled = true;
    settings.port = port
        .filter(|port| *port != 0)
        .or(Some(settings.port).filter(|port| *port != 0))
        .unwrap_or(crate::cloud::DEFAULT_PORT);
    save(&home, &settings)?;
    Ok(settings.port)
}

fn home(storage: &Storage) -> PathBuf {
    super::server::home(storage)
        .map(|home| home.join("remote"))
        .unwrap_or_else(|_| storage.cache.join("remote"))
}

/// Configured hosts first, then the addresses discovered on this machine. A
/// server with configured hosts (`difu remote config --host`) advertises only
/// those: what it discovers are private cloud addresses (`ip-172-31-….local`,
/// `172.31.….`) that clients would save and try in vain. A computer keeps its
/// discovered addresses, which are how phones on its network reach it.
fn advertise(configured: &[String], discovered: Vec<String>, server: bool) -> Vec<String> {
    let mut hosts = configured.to_vec();
    if server && !configured.is_empty() {
        return hosts;
    }
    for host in discovered {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

/// A display name for clients, or `None` (an empty name) to show the
/// computer's name again.
fn display_name(name: &str) -> Result<Option<String>> {
    let name = name.trim();
    ensure!(
        name.chars().count() <= 64 && !name.chars().any(char::is_control),
        "Use a name of at most 64 characters without control characters"
    );
    Ok((!name.is_empty()).then(|| name.to_owned()))
}

/// A host name or IP address for pairing links; the port is separate.
fn advertised_host(host: &str) -> Result<String> {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    ensure!(
        !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':')),
        "`{host}` is not a host name or IP address"
    );
    ensure!(
        host.matches(':').count() != 1,
        "Give only the host in `{host}`; set the port with --port"
    );
    Ok(host.to_owned())
}

fn valid_device_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn pairing_uri(name: &str, hosts: &[String], port: u16, fingerprint: &str, code: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("v", &PROTOCOL.to_string())
        .append_pair("n", name)
        // A few candidates keep the QR code small enough to scan from a terminal.
        .append_pair(
            "h",
            &hosts.iter().take(4).cloned().collect::<Vec<_>>().join(","),
        )
        .append_pair("p", &port.to_string())
        .append_pair("f", fingerprint)
        .append_pair("c", code)
        .finish();
    format!("difu://pair?{query}")
}

fn load(home: &std::path::Path) -> Result<Settings> {
    let path = home.join("remote.json");
    if !path.exists() {
        return Ok(Settings::default());
    }
    serde_json::from_slice(&fs::read(&path)?).context("Cannot read remote control settings")
}

fn save(home: &std::path::Path, settings: &Settings) -> Result<()> {
    private_dir(home)?;
    storage::atomic_json(&home.join("remote.json"), settings)
}

fn private_dir(home: &std::path::Path) -> Result<()> {
    fs::create_dir_all(home)?;
    ensure!(
        !fs::symlink_metadata(home)?.file_type().is_symlink(),
        "Remote control storage must not be a symlink"
    );
    fs::set_permissions(home, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// A remote listening on loopback only, for end-to-end tests of clients.
#[cfg(test)]
pub(crate) fn loopback(storage: Storage) -> Arc<Remote> {
    Remote::open(storage, Some("127.0.0.1".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(temp: &tempfile::TempDir) -> Arc<Remote> {
        Arc::new(Remote {
            storage: Storage {
                config: temp.path().join("config.json"),
                cache: temp.path().join("cache"),
            },
            home: temp.path().join("remote"),
            bind: Some("127.0.0.1".into()),
            identity: Mutex::new(None),
            state: Mutex::new(State {
                settings: Settings::default(),
                pairing: None,
                server: None,
                error: None,
            }),
            peers: listener::Peers::default(),
            guard: listener::Guard::default(),
        })
    }

    #[test]
    fn pairing_codes_work_once_and_tokens_are_stored_hashed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let remote = remote(&temp);
        remote.lock()?.pairing = Some(Pairing {
            code: "code".into(),
            expires: chrono::Utc::now().timestamp_millis() + 60_000,
        });
        assert!(
            remote
                .pair("wrong", "phone-0001", "Phone", Access::Standard)
                .is_err()
        );
        let Enrolled {
            id: device, token, ..
        } = remote.pair("code", "phone-0001", "Phone", Access::Standard)?;
        assert_eq!(device, "phone-0001");
        assert!(
            remote
                .pair("code", "phone-0002", "Other", Access::Full)
                .is_err()
        );
        assert_eq!(
            remote.authenticate(&token)?,
            ("phone-0001".into(), Access::Standard)
        );
        assert!(remote.authenticate("guess").is_err());
        let saved = fs::read_to_string(temp.path().join("remote/remote.json"))?;
        assert!(!saved.contains(&token));
        assert!(saved.contains(&storage::hash(&token)));
        remote.command(Command::Forget {
            device: device.clone(),
        })?;
        assert!(remote.authenticate(&token).is_err());
        Ok(())
    }

    #[test]
    fn expired_codes_are_rejected() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let remote = remote(&temp);
        remote.lock()?.pairing = Some(Pairing {
            code: "code".into(),
            expires: chrono::Utc::now().timestamp_millis() - 1,
        });
        assert!(
            remote
                .pair("code", "phone-0001", "Phone", Access::Standard)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn legacy_devices_are_standard_and_ordinary_settings_keep_their_shape() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("remote");
        fs::create_dir_all(&home)?;
        fs::write(
            home.join("remote.json"),
            r#"{"enabled":true,"port":50000,"devices":[{"id":"phone-0001","name":"Phone","token":"abc","paired":1}]}"#,
        )?;
        let settings = load(&home)?;
        assert_eq!(
            settings.devices.first().map(|d| d.access),
            Some(Access::Standard)
        );
        save(&home, &settings)?;
        let saved = fs::read_to_string(home.join("remote.json"))?;
        assert!(!saved.contains("access") && !saved.contains("hosts"));
        assert!(!saved.contains("password"));
        Ok(())
    }

    #[test]
    fn advertised_hosts_are_plain_names_or_addresses() {
        let host = |text: &str| advertised_host(text).ok();
        assert_eq!(host(" 203.0.113.4 ").as_deref(), Some("203.0.113.4"));
        assert_eq!(host("[2001:db8::1]").as_deref(), Some("2001:db8::1"));
        assert_eq!(
            host("box.tail1.ts.net").as_deref(),
            Some("box.tail1.ts.net")
        );
        assert!(host("box.example.com:3438").is_none());
        assert!(host("a,b").is_none());
        assert!(host("").is_none());
    }

    #[test]
    fn servers_advertise_only_configured_hosts_and_computers_add_discovered_ones() {
        let discovered = || vec!["ip-172-31-33-39.local".into(), "172.31.33.39".into()];
        let configured = ["63.185.13.36".to_owned()];
        assert_eq!(advertise(&configured, discovered(), true), ["63.185.13.36"]);
        // Nothing configured yet: discovered addresses are all a server has.
        assert_eq!(
            advertise(&[], discovered(), true),
            ["ip-172-31-33-39.local", "172.31.33.39"]
        );
        // A computer on a LAN keeps them after its configured hosts.
        assert_eq!(
            advertise(
                &["mac.tail1.ts.net".into(), "172.31.33.39".into()],
                discovered(),
                false
            ),
            ["mac.tail1.ts.net", "172.31.33.39", "ip-172-31-33-39.local"]
        );
    }

    #[test]
    fn the_display_name_reaches_links_and_is_saved() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let remote = remote(&temp);
        let configure = |name: &str| {
            remote.command(Command::Configure {
                hosts: Vec::new(),
                clear_hosts: false,
                port: None,
                name: Some(name.into()),
            })
        };
        assert_eq!(configure(" aws ")?.name, "aws");
        assert_eq!(remote.name(), "aws");
        let link = remote.command(Command::Open)?.pairing.context("link")?;
        let pairs: std::collections::BTreeMap<_, _> =
            url::Url::parse(&link)?.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("n").map(String::as_str), Some("aws"));
        assert_eq!(load(&remote.home)?.name.as_deref(), Some("aws"));
        // Other settings changes keep it.
        remote.command(Command::Configure {
            hosts: vec!["203.0.113.4".into()],
            clear_hosts: false,
            port: None,
            name: None,
        })?;
        assert_eq!(load(&remote.home)?.name.as_deref(), Some("aws"));
        assert!(configure("bad\nname").is_err());
        assert!(configure(&"x".repeat(65)).is_err());
        assert_eq!(configure("")?.name, network::name());
        assert_eq!(load(&remote.home)?.name, None);
        remote.shut_down()?;
        Ok(())
    }

    #[test]
    fn changing_the_port_moves_the_listener() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let remote = remote(&temp);
        let first = remote.command(Command::Open)?.port;
        let free = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let status = remote.command(Command::Configure {
            hosts: Vec::new(),
            clear_hosts: false,
            port: Some(free),
            name: None,
        })?;
        assert!(status.listening);
        assert_eq!(status.port, free);
        assert_eq!(load(&remote.home)?.port, free);
        assert!(std::net::TcpStream::connect(("127.0.0.1", first)).is_err());
        remote.command(Command::Disable)?;
        Ok(())
    }

    #[test]
    fn servers_never_turn_remote_control_off() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let remote = remote(&temp);
        fs::write(&remote.storage.config, r#"{"server":{"enabled":true}}"#)?;
        let port = remote.command(Command::Open)?.port;
        let error = remote
            .command(Command::Disable)
            .err()
            .context("A server must refuse to stop remote control")?;
        assert!(error.to_string().contains("difu server"), "{error:#}");
        let status = remote.status()?;
        assert!(status.enabled && status.listening && status.server);
        assert!(load(&remote.home)?.enabled);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        // Revoking devices and password pairing stay available.
        remote.command(Command::Password { verifier: None })?;
        remote.shut_down()?;
        assert!(!remote.status()?.listening);
        Ok(())
    }

    #[test]
    fn pairing_uri_round_trips_through_url_parsing() -> Result<()> {
        let uri = pairing_uri(
            "Ali's Mac",
            &["mac.local".into(), "192.168.1.4".into()],
            50123,
            "abc_-",
            "c0de",
        );
        let parsed = url::Url::parse(&uri)?;
        assert_eq!(parsed.scheme(), "difu");
        let pairs: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("n").map(String::as_str), Some("Ali's Mac"));
        assert_eq!(
            pairs.get("h").map(String::as_str),
            Some("mac.local,192.168.1.4")
        );
        assert_eq!(pairs.get("p").map(String::as_str), Some("50123"));
        Ok(())
    }
}
