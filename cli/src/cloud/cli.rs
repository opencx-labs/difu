//! Command-line surface for cloud servers. Each subcommand is handled by the
//! module that owns it; see docs/cloud-agents.md for the full table.
use crate::storage::Storage;
use anyhow::Result;
use clap::Subcommand;

/// Run on a client (your Mac): connect to and manage cloud servers.
#[derive(Clone, Debug, Subcommand)]
pub enum ServerCommand {
    /// Set up a new server from its IP address and root password (used once).
    Add {
        /// IP address or host name of the server.
        address: String,
        /// Name shown in difu; defaults to the server's host name.
        #[arg(long)]
        name: Option<String>,
        /// Account used for the first login.
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long, default_value_t = 22)]
        ssh_port: u16,
        /// Join the server to Tailscale and close its public ports.
        #[arg(long)]
        tailscale: bool,
        /// Run agents directly on the server instead of one container per session.
        #[arg(long)]
        no_sandbox: bool,
    },
    /// Pair this computer with an already configured server using its password.
    Join {
        /// `host` or `host:port` (default port 3438).
        address: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// List saved servers and whether they are reachable.
    List,
    /// Revoke this computer on a server and forget it.
    Remove { name: String },
    /// Open a shell on a server as the `difu` user.
    Ssh { name: String },
    /// Show a phone pairing QR code for a server.
    Pair { name: String },
    /// Install this computer's difu version on a server and restart it.
    Upgrade { name: String },
}

/// Run on a server: remote-control administration.
#[derive(Clone, Debug, Subcommand)]
pub enum RemoteCommand {
    /// Print a pairing QR code and link from the running service.
    Pair {
        #[arg(long)]
        json: bool,
    },
    /// List paired devices.
    Devices,
    /// Revoke a paired device.
    Forget { device: String },
    /// Set the server password new clients use with the server's address.
    Password {
        /// Generate a strong password and print it.
        #[arg(long, conflicts_with = "disable")]
        generate: bool,
        /// Turn password pairing off (QR pairing keeps working).
        #[arg(long)]
        disable: bool,
    },
    /// Addresses advertised to clients and the listening port.
    Config {
        /// Public IP, DNS or Tailscale name; repeat for several.
        #[arg(long = "host")]
        hosts: Vec<String>,
        #[arg(long)]
        clear_hosts: bool,
        #[arg(long)]
        port: Option<u16>,
    },
}

/// Run on a server: repositories kept cloned for new sessions.
#[derive(Clone, Debug, Subcommand)]
pub enum ReposCommand {
    List,
    /// Add `owner/name`, or pick from `gh repo list` when omitted.
    Add {
        name: Option<String>,
    },
    Remove {
        name: String,
    },
    /// Fetch one repository, or all of them.
    Refresh {
        name: Option<String>,
    },
}

/// Run on a server: per-session containers.
#[derive(Clone, Debug, Subcommand)]
pub enum SandboxCommand {
    /// Build the sandbox image, or pull `server.sandbox.image` from its registry.
    Build,
    /// Show the runtime, image and running sandboxes.
    Status,
}

pub fn server(storage: &Storage, command: ServerCommand) -> Result<()> {
    match command {
        ServerCommand::Add {
            address,
            name,
            user,
            ssh_port,
            tailscale,
            no_sandbox,
        } => super::onboard::add(
            storage,
            super::onboard::Options {
                address,
                name,
                user,
                ssh_port,
                tailscale,
                sandbox: !no_sandbox,
            },
        ),
        ServerCommand::Join { address, name } => super::servers::join(storage, &address, name),
        ServerCommand::List => super::servers::list(storage),
        ServerCommand::Remove { name } => super::servers::remove(storage, &name),
        ServerCommand::Ssh { name } => super::onboard::ssh(storage, &name),
        ServerCommand::Pair { name } => super::onboard::pair(storage, &name),
        ServerCommand::Upgrade { name } => super::onboard::upgrade(storage, &name),
    }
}

pub fn remote(storage: &Storage, command: RemoteCommand) -> Result<()> {
    super::serve::remote(storage, command)
}

pub fn repos(storage: &Storage, command: ReposCommand) -> Result<()> {
    super::repos::command(storage, command)
}

pub fn sandbox(storage: &Storage, command: SandboxCommand) -> Result<()> {
    crate::agents::sandbox::command(storage, command)
}
