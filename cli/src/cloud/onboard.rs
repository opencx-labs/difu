//! `difu server add`: one-time onboarding of a fresh Linux server over SSH, plus
//! the SSH-based helpers (`ssh`, `pair`, `upgrade`) for saved servers.
use crate::storage::Storage;
use anyhow::Result;

#[derive(Clone, Debug)]
pub struct Options {
    pub address: String,
    pub name: Option<String>,
    pub user: String,
    pub ssh_port: u16,
    pub tailscale: bool,
    pub sandbox: bool,
}

pub fn add(storage: &Storage, options: Options) -> Result<()> {
    let _ = (storage, options);
    anyhow::bail!("`difu server add` is not available in this build yet")
}

pub fn ssh(storage: &Storage, name: &str) -> Result<()> {
    let _ = (storage, name);
    anyhow::bail!("`difu server ssh` is not available in this build yet")
}

pub fn pair(storage: &Storage, name: &str) -> Result<()> {
    let _ = (storage, name);
    anyhow::bail!("`difu server pair` is not available in this build yet")
}

pub fn upgrade(storage: &Storage, name: &str) -> Result<()> {
    let _ = (storage, name);
    anyhow::bail!("`difu server upgrade` is not available in this build yet")
}
