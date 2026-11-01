//! `difu serve`: the agent service in the foreground with server mode and remote
//! control on, plus `difu remote` administration of the running service.
use super::cli::RemoteCommand;
use crate::storage::Storage;
use anyhow::Result;

pub fn run(storage: Storage, port: Option<u16>) -> Result<()> {
    let _ = (storage, port);
    anyhow::bail!("`difu serve` is not available in this build yet")
}

pub fn remote(storage: &Storage, command: RemoteCommand) -> Result<()> {
    let _ = (storage, command);
    anyhow::bail!("`difu remote` is not available in this build yet")
}
