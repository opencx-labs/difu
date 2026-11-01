//! Server settings methods for paired clients (`server.repos.*`, `server.rules.*`,
//! `server.logins`, `server.login.*`, `server.logout`); see docs/cloud-agents.md.
//! Returns `None` for methods this module does not own.
use crate::storage::Storage;
use anyhow::Result;
use serde_json::Value;

pub fn dispatch(storage: &Storage, method: &str, params: &Value) -> Option<Result<Value>> {
    let _ = (storage, method, params);
    None
}
