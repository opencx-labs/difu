//! Server repositories: one base clone per `owner/name`, refreshed in the
//! background so new sessions start without a network clone.
use super::cli::ReposCommand;
use crate::storage::Storage;
use anyhow::Result;

pub fn command(storage: &Storage, command: ReposCommand) -> Result<()> {
    let _ = (storage, command);
    anyhow::bail!("`difu repos` is not available in this build yet")
}
