//! WebSocket client for cloud servers: pinned TLS, QR/password pairing, token
//! authentication and the `rpc` methods (docs/cloud-agents.md).
use anyhow::Result;

/// Device credentials returned by a successful pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paired {
    pub server_id: String,
    pub name: String,
    pub hosts: Vec<String>,
    pub port: u16,
    pub fingerprint: String,
    pub device: String,
    pub token: String,
    pub version: String,
}

/// Pair with a `difu://pair?...` link (from `difu remote pair --json`).
pub fn pair_with_link(link: &str, device_name: &str, full: bool) -> Result<Paired> {
    let _ = (link, device_name, full);
    anyhow::bail!("Pairing with a server is not available in this build yet")
}

/// Pair with an address and the server password.
pub fn pair_with_password(
    host: &str,
    port: u16,
    password: &str,
    device_name: &str,
    full: bool,
) -> Result<Paired> {
    let _ = (host, port, password, device_name, full);
    anyhow::bail!("Pairing with a server is not available in this build yet")
}
