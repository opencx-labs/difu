//! A full device driving a real agent service through `rpc` and `rpc.watch`,
//! the way a Mac reaches sessions on a cloud server.
mod support;

use anyhow::{Context, Result, ensure};
use difu::{
    agents::{Reply, Request, client, remote::Command},
    cloud::{
        client::{Connection, pair_with_link},
        servers::SavedServer,
    },
    storage::Storage,
};
use std::time::Duration;
use support::Service;

#[test]
fn full_devices_drive_the_service_over_rpc() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = Storage {
        config: temp.path().join("config/config.json"),
        cache: temp.path().join("cache"),
    };
    std::fs::create_dir_all(temp.path().join("config"))?;
    std::fs::create_dir_all(&storage.cache)?;
    let mut service = Service::start(&storage, |command| {
        command.env("DIFU_REMOTE_BIND", "127.0.0.1");
    })?;
    let Reply::Remote(status) = client::request(
        &storage,
        Request::Remote {
            command: Command::Open,
        },
    )?
    else {
        anyhow::bail!("Unexpected remote control reply");
    };
    let link = status.pairing.context("Missing pairing link")?;
    let paired = pair_with_link(&link, "Build Mac", true)?;
    let connection = Connection::connect(&SavedServer {
        name: paired.name.clone(),
        id: paired.server_id.clone(),
        hosts: paired.hosts.clone(),
        port: paired.port,
        fingerprint: paired.fingerprint.clone(),
        device: paired.device.clone(),
        token: paired.token.clone(),
        ssh: None,
    })?;
    ensure!(connection.welcome().full);

    let Reply::Sessions(sessions) = connection.rpc(&Request::List)? else {
        anyhow::bail!("Expected the session list");
    };
    ensure!(sessions.iter().any(|s| s.id == "orchestrator"));
    ensure!(matches!(
        connection.rpc(&Request::ServiceVersion)?,
        Reply::ServiceVersion { version } if version == env!("CARGO_PKG_VERSION")
    ));
    let error = connection.rpc(&Request::Delete {
        id: "missing".into(),
    });
    ensure!(error.is_err(), "Service errors must surface");

    let watch = connection.watch("orchestrator", None)?;
    let first = watch.replies.recv_timeout(Duration::from_secs(10))??;
    ensure!(
        matches!(&first, Reply::Session(session) if session.id == "orchestrator"),
        "Expected a baseline, got {first:?}"
    );
    // The service's heartbeat keeps arriving while the watch lives.
    watch.replies.recv_timeout(Duration::from_secs(10))??;
    drop(watch);

    let missing = connection.watch("missing", None)?;
    ensure!(matches!(
        missing.replies.recv_timeout(Duration::from_secs(10))?,
        Ok(Reply::Error(_))
    ));
    let closed = missing.replies.recv_timeout(Duration::from_secs(10))?;
    ensure!(closed.is_err_and(|e| e.to_string().contains("Session not found")));
    service.stop()
}
