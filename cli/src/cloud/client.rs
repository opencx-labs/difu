//! WebSocket client for cloud servers: pinned TLS, QR/password pairing, token
//! authentication and the `rpc` methods (docs/cloud-agents.md).
use super::servers::SavedServer;
use crate::agents::{Reply, Request};
use anyhow::Result;
use serde_json::Value;
use std::sync::mpsc;

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
    pub full: bool,
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

/// What the server said in `welcome` after token authentication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Welcome {
    pub device: String,
    pub version: String,
    pub full: bool,
    pub hosts: Vec<String>,
    pub port: u16,
}

/// One authenticated connection. Cheap to clone and safe to use from several
/// threads: a background reader routes replies by request id and `rpc.watch`
/// events to their streams. It reports itself closed when the socket drops.
#[derive(Clone)]
pub struct Connection {
    _private: (),
}

impl Connection {
    /// Try the saved hosts in order with the pinned fingerprint and token.
    pub fn connect(server: &SavedServer) -> Result<Self> {
        let _ = server;
        anyhow::bail!("Connecting to servers is not available in this build yet")
    }

    pub fn welcome(&self) -> Welcome {
        Welcome {
            device: String::new(),
            version: String::new(),
            full: false,
            hosts: Vec::new(),
            port: 0,
        }
    }

    pub fn closed(&self) -> bool {
        true
    }

    /// A phone-API method (`server.devices`, `device.unpair`, ...).
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let _ = (method, params);
        anyhow::bail!("Server connection closed")
    }

    /// The internal service protocol over `rpc` (full devices only).
    pub fn rpc(&self, request: &Request) -> Result<Reply> {
        let _ = request;
        anyhow::bail!("Server connection closed")
    }

    /// `rpc.watch`: replies arrive on the receiver until the watch is dropped,
    /// the server closes it, or the connection drops (the channel then closes).
    pub fn watch(&self, id: &str, version: Option<u64>) -> Result<Watch> {
        let _ = (id, version);
        anyhow::bail!("Server connection closed")
    }
}

/// A live `rpc.watch` stream; dropping it sends `rpc.unwatch`.
pub struct Watch {
    pub replies: mpsc::Receiver<Result<Reply>>,
}
