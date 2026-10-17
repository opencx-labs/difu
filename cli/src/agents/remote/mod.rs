//! Wi-Fi remote control for the difu iOS app. The agent service listens on the
//! local network only while enabled. A phone pairs once by scanning a short-lived,
//! single-use QR code; afterwards it authenticates with its own device token.
//! TLS uses a self-signed certificate whose fingerprint travels in the QR code,
//! so the phone pins this Mac without trusting any certificate authority.
mod api;
mod identity;
mod listener;
mod network;
mod reviews;
mod view;

use crate::storage::{self, Storage};
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    pub paired: i64,
    pub seen: Option<i64>,
    pub connected: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    enabled: bool,
    port: u16,
    devices: Vec<Device>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Device {
    id: String,
    name: String,
    /// SHA-256 of the device token; the token itself is only on the phone.
    token: String,
    paired: i64,
    #[serde(default)]
    seen: Option<i64>,
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
    identity: Mutex<Option<Arc<identity::Identity>>>,
    state: Mutex<State>,
    peers: listener::Peers,
}

impl Remote {
    /// Restore the saved setting; an enabled remote starts listening right away.
    pub fn start(storage: Storage) -> Arc<Self> {
        let home = super::server::home(&storage)
            .map(|home| home.join("remote"))
            .unwrap_or_else(|_| storage.cache.join("remote"));
        let mut error = None;
        let settings = load(&home).unwrap_or_else(|e| {
            error = Some(format!("{e:#}"));
            Settings::default()
        });
        let remote = Arc::new(Self {
            storage,
            home,
            identity: Mutex::new(None),
            state: Mutex::new(State {
                settings,
                pairing: None,
                server: None,
                error,
            }),
            peers: listener::Peers::default(),
        });
        if remote.lock().map(|s| s.settings.enabled).unwrap_or(false)
            && let Err(error) = remote.listen()
        {
            eprintln!("Remote control: {error:#}");
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
            Command::Disable => {
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
            }
            Command::Forget { device } => {
                let mut state = self.lock()?;
                state.settings.devices.retain(|d| d.id != device);
                save(&self.home, &state.settings)?;
                drop(state);
                self.peers.close_device(&device);
            }
        }
        self.status()
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
            network::hosts()
        } else {
            Vec::new()
        };
        let name = network::name();
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
                })
                .collect(),
            error: state.error.clone(),
        })
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
            let (server, port) = listener::Server::bind(self.clone(), identity, preferred)?;
            let mut state = self.lock()?;
            if state.settings.port != port {
                state.settings.port = port;
                save(&self.home, &state.settings)?;
            }
            state.server = Some(server);
            state.error = None;
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
    fn pair(&self, code: &str, id: &str, name: &str) -> Result<(String, String)> {
        let mut state = self.lock()?;
        let now = chrono::Utc::now().timestamp_millis();
        let valid = state
            .pairing
            .as_ref()
            .is_some_and(|p| p.expires > now && identity::same(&p.code, code));
        ensure!(
            valid,
            "This pairing code expired or was already used. Open /remote-control on your Mac for a new code."
        );
        state.pairing = None;
        let id = if valid_device_id(id) {
            id.to_owned()
        } else {
            identity::random_token(12)?
        };
        let token = identity::random_token(32)?;
        let name: String = name.trim().chars().take(80).collect();
        state.settings.devices.retain(|d| d.id != id);
        state.settings.devices.push(Device {
            id: id.clone(),
            name: if name.is_empty() {
                "iPhone".into()
            } else {
                name
            },
            token: storage::hash(&token),
            paired: now,
            seen: Some(now),
        });
        save(&self.home, &state.settings)?;
        Ok((id, token))
    }

    fn authenticate(&self, token: &str) -> Result<String> {
        let hash = storage::hash(token);
        let mut state = self.lock()?;
        let device = state
            .settings
            .devices
            .iter_mut()
            .find(|d| identity::same(&d.token, &hash))
            .context(
                "This phone is no longer paired. Scan the QR code in /remote-control again.",
            )?;
        device.seen = Some(chrono::Utc::now().timestamp_millis());
        let id = device.id.clone();
        save(&self.home, &state.settings)?;
        Ok(id)
    }

    fn paired(&self, device: &str) -> bool {
        self.lock()
            .is_ok_and(|s| s.settings.devices.iter().any(|d| d.id == device))
    }

    fn enabled(&self) -> bool {
        self.lock().is_ok_and(|s| s.settings.enabled)
    }
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
            identity: Mutex::new(None),
            state: Mutex::new(State {
                settings: Settings::default(),
                pairing: None,
                server: None,
                error: None,
            }),
            peers: listener::Peers::default(),
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
        assert!(remote.pair("wrong", "phone-0001", "Phone").is_err());
        let (device, token) = remote.pair("code", "phone-0001", "Phone")?;
        assert_eq!(device, "phone-0001");
        assert!(remote.pair("code", "phone-0002", "Other").is_err());
        assert_eq!(remote.authenticate(&token)?, "phone-0001");
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
        assert!(remote.pair("code", "phone-0001", "Phone").is_err());
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
