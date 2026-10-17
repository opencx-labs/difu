//! The iOS app's view of the agent service: pairing over pinned TLS, device
//! tokens, JSON-RPC, and session subscriptions against a real service process.
mod support;

use anyhow::{Context, Result, ensure};
use difu::{
    agents::{
        Reply, Request, client,
        remote::{Command, Status},
    },
    storage::Storage,
};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::TcpStream,
    sync::Arc,
    time::{Duration, Instant},
};
use support::Service;
use tungstenite::Message;

#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        use base64::Engine;
        let actual =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(end_entity));
        if actual == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
    value.pointer(pointer).unwrap_or(&Value::Null)
}

type Phone = tungstenite::WebSocket<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;

fn connect(port: u16, fingerprint: &str) -> Result<Phone> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint: fingerprint.into(),
            provider,
        }))
        .with_no_client_auth();
    let tcp = TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(20)))?;
    let tls = rustls::ClientConnection::new(Arc::new(config), ServerName::try_from("difu.local")?)?;
    let stream = rustls::StreamOwned::new(tls, tcp);
    let (socket, _) = tungstenite::client::client(format!("wss://127.0.0.1:{port}/"), stream)
        .map_err(|e| anyhow::anyhow!("WebSocket handshake: {e}"))?;
    Ok(socket)
}

fn send(socket: &mut Phone, value: Value) -> Result<()> {
    socket.send(Message::text(value.to_string()))?;
    Ok(())
}

fn next(socket: &mut Phone) -> Result<Value> {
    loop {
        if let Message::Text(text) = socket.read()? {
            return Ok(serde_json::from_str(&text)?);
        }
    }
}

/// Read until the reply to `id`, keeping pushed events for later checks.
fn call(
    socket: &mut Phone,
    id: u64,
    method: &str,
    params: Value,
    events: &mut Vec<Value>,
) -> Result<Value> {
    send(
        socket,
        json!({"id": id, "method": method, "params": params}),
    )?;
    loop {
        let message = next(socket)?;
        if *at(&message, "/id") == json!(id) {
            if let Some(error) = message.get("error") {
                anyhow::bail!("{method}: {error}");
            }
            return Ok(at(&message, "/result").clone());
        }
        events.push(message);
    }
}

fn remote(storage: &Storage, command: Command) -> Result<Status> {
    match client::request(storage, Request::Remote { command })? {
        Reply::Remote(status) => Ok(status),
        other => anyhow::bail!("Unexpected reply {other:?}"),
    }
}

#[test]
fn phones_pair_once_authenticate_and_control_sessions() -> Result<()> {
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
    let status = remote(&storage, Command::Open)?;
    ensure!(
        status.enabled && status.listening,
        "Not listening: {status:?}"
    );
    let uri = url::Url::parse(status.pairing.as_deref().context("Missing pairing code")?)?;
    let query: BTreeMap<_, _> = uri.query_pairs().into_owned().collect();
    let (port, fingerprint, code) = (
        query.get("p").context("port")?.parse::<u16>()?,
        query.get("f").context("fingerprint")?.clone(),
        query.get("c").context("code")?.clone(),
    );
    ensure!(port == status.port);

    // A pinned client pairs with the single-use code and receives a device token.
    let mut phone = connect(port, &fingerprint)?;
    send(
        &mut phone,
        json!({"type": "pair", "code": code, "device": {"id": "test-phone-1", "name": "Test iPhone"}}),
    )?;
    let welcome = next(&mut phone)?;
    ensure!(at(&welcome, "/type") == "welcome", "{welcome}");
    let token = at(&welcome, "/token")
        .as_str()
        .context("Missing token")?
        .to_owned();
    ensure!(*at(&welcome, "/server/fingerprint") == json!(fingerprint));

    let mut events = Vec::new();
    let info = call(&mut phone, 1, "server.info", json!({}), &mut events)?;
    ensure!(at(&info, "/version") == env!("CARGO_PKG_VERSION"));
    let listed = call(&mut phone, 2, "sessions.subscribe", json!({}), &mut events)?;
    let sessions = at(&listed, "/sessions").as_array().context("sessions")?;
    ensure!(
        sessions
            .first()
            .is_some_and(|s| at(s, "/id") == "orchestrator" && at(s, "/orchestrator") == true),
        "{listed}"
    );
    call(
        &mut phone,
        3,
        "session.watch",
        json!({"id": "orchestrator"}),
        &mut events,
    )?;
    let started = Instant::now();
    while !events
        .iter()
        .any(|e| at(e, "/event") == "session" && at(e, "/data/id") == "orchestrator")
    {
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "No session event: {events:?}"
        );
        events.push(next(&mut phone)?);
    }
    let error = call(&mut phone, 4, "no.such.method", json!({}), &mut events);
    ensure!(error.is_err_and(|e| e.to_string().contains("Unknown method")));
    let error = call(
        &mut phone,
        5,
        "session.rename",
        json!({"id": "missing", "title": "x"}),
        &mut events,
    );
    ensure!(error.is_err());
    ensure!(
        remote(&storage, Command::Status)?
            .devices
            .iter()
            .any(|d| d.name == "Test iPhone" && d.connected)
    );
    drop(phone);

    // The code is spent; the token keeps working until the phone is forgotten.
    let mut reused = connect(port, &fingerprint)?;
    send(
        &mut reused,
        json!({"type": "pair", "code": code, "device": {"id": "test-phone-2", "name": "Other"}}),
    )?;
    ensure!(at(&next(&mut reused)?, "/type") == "denied");
    let mut phone = connect(port, &fingerprint)?;
    send(&mut phone, json!({"type": "auth", "token": token}))?;
    ensure!(at(&next(&mut phone)?, "/type") == "welcome");
    let mut guess = connect(port, &fingerprint)?;
    send(&mut guess, json!({"type": "auth", "token": "guess"}))?;
    ensure!(at(&next(&mut guess)?, "/type") == "denied");

    // A different certificate fingerprint is rejected by the pinned client.
    ensure!(connect(port, "AAAA").is_err());

    remote(
        &storage,
        Command::Forget {
            device: "test-phone-1".into(),
        },
    )?;
    let started = Instant::now();
    while next(&mut phone).is_ok() {
        ensure!(
            started.elapsed() < Duration::from_secs(5),
            "Forgotten phone stayed connected"
        );
    }
    let mut forgotten = connect(port, &fingerprint)?;
    send(&mut forgotten, json!({"type": "auth", "token": token}))?;
    ensure!(at(&next(&mut forgotten)?, "/type") == "denied");

    let status = remote(&storage, Command::Disable)?;
    ensure!(!status.enabled && !status.listening && status.pairing.is_none());
    ensure!(connect(port, &fingerprint).is_err());
    service.stop()
}

fn git(root: &std::path::Path, args: &[&str]) -> Result<String> {
    difu::process::checked(
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args),
        &difu::process::Cancel::default(),
    )
    .map(|s| s.trim().to_owned())
}

/// Apply one pushed event to the app-side view, as the iOS app does.
fn apply(view: &mut Value, message: &Value) -> Result<()> {
    match at(message, "/event").as_str() {
        Some("session") => *view = at(message, "/data").clone(),
        Some("session.delta") => {
            let delta = &at(message, "/data");
            ensure!(
                at(delta, "/base") == at(view, "/version"),
                "Delta base mismatch"
            );
            let mut entries = at(view, "/entries").as_array().cloned().unwrap_or_default();
            let count = at(delta, "/count").as_u64().context("count")? as usize;
            entries.truncate(count);
            for change in at(delta, "/entries").as_array().context("entries")? {
                let index = at(change, "/0").as_u64().context("index")? as usize;
                if index < entries.len() {
                    if let Some(slot) = entries.get_mut(index) {
                        *slot = at(change, "/1").clone();
                    }
                } else {
                    ensure!(index == entries.len(), "Delta skipped an entry");
                    entries.push(at(change, "/1").clone());
                }
            }
            *view = at(delta, "/session").clone();
            if let Some(object) = view.as_object_mut() {
                object.insert("entries".into(), Value::Array(entries));
            }
        }
        Some("session.closed") => anyhow::bail!("Watch closed: {message}"),
        _ => {}
    }
    Ok(())
}

/// Apply events buffered during calls, then wait for a view matching `ready`.
fn watch_until(
    phone: &mut Phone,
    view: &mut Value,
    events: &mut Vec<Value>,
    ready: impl Fn(&Value) -> bool,
) -> Result<()> {
    for event in std::mem::take(events) {
        apply(view, &event)?;
    }
    let started = Instant::now();
    while !ready(view) {
        ensure!(
            started.elapsed() < Duration::from_secs(20),
            "Timed out waiting for the session view: {view}"
        );
        apply(view, &next(phone)?)?;
    }
    Ok(())
}

#[test]
fn phones_answer_approvals_send_images_and_read_changes() -> Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt};
    let temp = tempfile::Builder::new()
        .prefix("difu-remote-agent-")
        .tempdir_in("/tmp")?;
    let root = temp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    git(&repo, &["remote", "add", "origin", "."])?;
    let base = git(&repo, &["rev-parse", "HEAD"])?;
    git(&repo, &["update-ref", "refs/remotes/origin/main", &base])?;
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    for (name, source) in [
        ("codex", include_str!("fixtures/agent_codex.py")),
        ("gh", "#!/bin/sh\nprintf '[]\\n'\n"),
    ] {
        let file = bin.join(name);
        fs::write(&file, source)?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o700))?;
    }
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").context("PATH")?,
    )))?;
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    fs::create_dir(&storage.cache)?;
    let mut service = Service::start(&storage, |c| {
        c.env("PATH", &path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("DIFU_REMOTE_BIND", "127.0.0.1");
    })?;
    let Reply::Launched(id) = client::request(
        &storage,
        Request::Launch {
            job: Box::new(difu::agents::Job::Coding(difu::agents::Launch {
                repository: repo.clone(),
                base: "HEAD".into(),
                isolated: true,
                prompt: "approval please".into(),
                model: None,
                effort: None,
            })),
        },
    )?
    else {
        anyhow::bail!("Missing launch ID");
    };

    let status = remote(&storage, Command::Open)?;
    let uri = url::Url::parse(status.pairing.as_deref().context("Missing pairing code")?)?;
    let query: BTreeMap<_, _> = uri.query_pairs().into_owned().collect();
    let mut phone = connect(status.port, query.get("f").context("fingerprint")?)?;
    send(
        &mut phone,
        json!({"type": "pair", "code": query.get("c").context("code")?, "device": {"id": "test-phone-1", "name": "Phone"}}),
    )?;
    ensure!(at(&next(&mut phone)?, "/type") == "welcome");
    let mut events = Vec::new();
    call(
        &mut phone,
        1,
        "session.watch",
        json!({"id": id}),
        &mut events,
    )?;
    let mut view = Value::Null;

    // The approval arrives with the terminal's choices; deciding resumes the turn.
    watch_until(&mut phone, &mut view, &mut events, |v| {
        at(v, "/pending").as_array().is_some_and(|p| !p.is_empty())
    })?;
    let pending = at(&view, "/pending/0").clone();
    ensure!(at(&pending, "/kind") == "approval", "{pending}");
    ensure!(
        at(&pending, "/detail")
            .as_str()
            .is_some_and(|d| d.contains("git diff --check"))
    );
    ensure!(
        at(&pending, "/choices")
            .as_array()
            .is_some_and(|c| c.len() == 4)
    );
    call(
        &mut phone,
        2,
        "session.decide",
        json!({"id": id, "request": at(&pending, "/request"), "choice": "accept"}),
        &mut events,
    )?;
    watch_until(&mut phone, &mut view, &mut events, |v| {
        at(v, "/status") == "Idle" && at(v, "/pending").as_array().is_some_and(Vec::is_empty)
    })?;
    ensure!(
        at(&view, "/entries")
            .as_array()
            .is_some_and(|entries| entries.iter().any(
                |e| at(e, "/role") == "assistant" && at(e, "/text") == "Finished fixture task"
            )),
        "{view}"
    );

    // A photo from the phone becomes a private session attachment.
    let mut png = Vec::new();
    image::RgbImage::new(2, 2)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    use base64::Engine;
    call(
        &mut phone,
        3,
        "session.send",
        json!({"id": id, "text": "make an edit", "attachments": [{"name": "photo.png", "data": base64::engine::general_purpose::STANDARD.encode(&png)}]}),
        &mut events,
    )?;
    watch_until(&mut phone, &mut view, &mut events, |v| {
        at(v, "/status") == "Idle"
            && at(v, "/entries").as_array().is_some_and(|entries| {
                entries.iter().any(|e| {
                    at(e, "/role") == "user"
                        && at(e, "/text")
                            .as_str()
                            .is_some_and(|t| t.contains("[image 1]"))
                })
            })
    })?;
    let changes = call(
        &mut phone,
        4,
        "session.changes",
        json!({"id": id}),
        &mut events,
    )?;
    let paths: Vec<_> = at(&changes, "/files")
        .as_array()
        .context("files")?
        .iter()
        .filter_map(|f| at(f, "/path").as_str())
        .collect();
    ensure!(
        paths.contains(&"new.txt") && paths.contains(&"approved.txt"),
        "{changes}"
    );
    let repositories = call(&mut phone, 5, "agents.repositories", json!({}), &mut events)?;
    ensure!(
        at(&repositories, "/repositories")
            .as_array()
            .is_some_and(|r| r.iter().any(|r| at(r, "/name") == "repo")),
        "{repositories}"
    );
    call(
        &mut phone,
        6,
        "session.rename",
        json!({"id": id, "title": "Renamed from phone"}),
        &mut events,
    )?;
    let listed = call(&mut phone, 7, "sessions.list", json!({}), &mut events)?;
    ensure!(
        at(&listed, "/sessions").as_array().is_some_and(|s| s
            .iter()
            .any(|s| *at(s, "/id") == json!(id) && at(s, "/title") == "Renamed from phone")),
        "{listed}"
    );
    drop(phone);
    service.stop()
}
