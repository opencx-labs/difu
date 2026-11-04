//! Cloud agents: difu running headless on a Linux server (`difu serve`) and the
//! clients that connect to any number of such servers. Design and wire contract:
//! docs/cloud-agents.md.
pub mod claude_config;
pub mod cli;
pub mod client;
pub mod disk;
pub mod environments;
pub mod federation;
pub mod logins;
pub mod onboard;
pub mod password;
pub mod repos;
pub mod serve;
pub mod servers;

use serde::{Deserialize, Serialize};

/// A server-mode service listens here unless configured otherwise ("difu" on a
/// phone keypad).
pub const DEFAULT_PORT: u16 = 3438;

/// Server-side settings, saved in the server's `config.json`. Unused on ordinary
/// installations, where `enabled` stays false.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    /// Set by `difu serve`: server-mode agent instructions apply.
    pub enabled: bool,
    /// `owner/name` entries, each with one base clone (see `repos`).
    pub repositories: Vec<String>,
    /// Background fetch interval; zero means the default.
    pub refresh_minutes: u32,
    pub sandbox: SandboxSettings,
}

impl ServerSettings {
    pub const DEFAULT_REFRESH_MINUTES: u32 = 10;

    pub fn refresh_minutes(&self) -> u32 {
        if self.refresh_minutes == 0 {
            Self::DEFAULT_REFRESH_MINUTES
        } else {
            self.refresh_minutes
        }
    }
}

/// Per-session containers on a server (see `agents::sandbox`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxSettings {
    pub enabled: bool,
    /// Container image; empty means `localhost/difu-sandbox:latest`.
    pub image: String,
    /// CPU limit per session, e.g. `4`; empty means unlimited.
    pub cpus: String,
    /// Memory limit per session, e.g. `8g`; empty means unlimited.
    pub memory: String,
}

impl SandboxSettings {
    pub const DEFAULT_IMAGE: &str = "localhost/difu-sandbox:latest";

    pub fn image(&self) -> &str {
        if self.image.trim().is_empty() {
            Self::DEFAULT_IMAGE
        } else {
            self.image.trim()
        }
    }
}
