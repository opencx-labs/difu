//! Per-session rootless Podman sandboxes on cloud servers. The agent and its
//! shells run inside; difu stays on the host and talks to them over stdio.
use crate::{cloud::cli::SandboxCommand, storage::Storage};
use anyhow::Result;

pub fn command(storage: &Storage, command: SandboxCommand) -> Result<()> {
    let _ = (storage, command);
    anyhow::bail!("`difu sandbox` is not available in this build yet")
}
