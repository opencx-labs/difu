//! Servers saved on a client machine (`<config>/servers.json`, 0600) and the
//! client commands that manage them (`join`, `list`, `remove`).
use crate::storage::Storage;
use anyhow::Result;
use serde::{Deserialize, Serialize};

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

pub fn load(storage: &Storage) -> Result<Vec<SavedServer>> {
    let _ = storage;
    Ok(Vec::new())
}

/// Insert or replace (by `id`, then `name`).
pub fn save(storage: &Storage, server: SavedServer) -> Result<()> {
    let _ = (storage, server);
    anyhow::bail!("Saving servers is not available in this build yet")
}

pub fn join(storage: &Storage, address: &str, name: Option<String>) -> Result<()> {
    let _ = (storage, address, name);
    anyhow::bail!("`difu server join` is not available in this build yet")
}

pub fn list(storage: &Storage) -> Result<()> {
    let _ = storage;
    anyhow::bail!("`difu server list` is not available in this build yet")
}

pub fn remove(storage: &Storage, name: &str) -> Result<()> {
    let _ = (storage, name);
    anyhow::bail!("`difu server remove` is not available in this build yet")
}
