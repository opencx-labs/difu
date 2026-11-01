//! WebSocket client for cloud servers: pinned TLS, QR/password pairing, token
//! authentication and the `rpc` methods (docs/cloud-agents.md).
use super::{
    password::{self, ClientKey},
    servers::SavedServer,
};
use crate::agents::{Reply, Request};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::SecureRandom;
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, protocol::WebSocketConfig};

/// Per host; clients try every advertised host in order.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Initial transcripts can be several MB.
const MAX_MESSAGE: usize = 64 << 20;
/// The server pings every 15 s; this much silence means the connection is gone.
const SILENCE: Duration = Duration::from_secs(45);
const CLOSED: &str = "Server connection closed";

type Socket = tungstenite::WebSocket<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;

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
    let link = Link::parse(link)?;
    let hello = json!({
        "type": "pair",
        "code": link.code,
        "device": {"id": device_id()?, "name": device_name},
        "access": access(full),
    });
    let mut errors = Vec::new();
    for host in &link.hosts {
        let mut socket = match open(host, link.port, Some(&link.fingerprint)) {
            Ok((socket, _)) => socket,
            Err(error) => {
                errors.push(format!("{host}: {error:#}"));
                continue;
            }
        };
        // The code works once, so a refusal here is final.
        send(&mut socket, &hello)?;
        let Answer::Welcome(welcome) = answer(&mut socket)? else {
            anyhow::bail!("The server answered pairing with a password challenge");
        };
        hang_up(&mut socket);
        return welcome.paired(host, link.fingerprint.clone());
    }
    anyhow::bail!("{}", unreachable(&errors))
}

/// Pair with an address and the server password.
pub fn pair_with_password(
    host: &str,
    port: u16,
    password: &str,
    device_name: &str,
    full: bool,
) -> Result<Paired> {
    // Nothing is pinned yet: the password proofs bind whatever certificate came.
    let (mut socket, fingerprint) = open(host, port, None)?;
    let client_nonce = password::nonce()?;
    send(
        &mut socket,
        &json!({
            "type": "password",
            "device": {"id": device_id()?, "name": device_name},
            "access": access(full),
            "nonce": client_nonce,
        }),
    )?;
    let Answer::Challenge {
        salt,
        iterations,
        nonce: server_nonce,
    } = answer(&mut socket)?
    else {
        anyhow::bail!("The server did not ask for the password");
    };
    let key = ClientKey::derive(password, &salt, iterations)?;
    send(
        &mut socket,
        &json!({"type": "proof", "proof": key.proof(&fingerprint, &server_nonce, &client_nonce)}),
    )?;
    let Answer::Welcome(welcome) = answer(&mut socket)? else {
        anyhow::bail!("Unexpected answer to the password proof");
    };
    hang_up(&mut socket);
    let proven = welcome
        .proof
        .as_deref()
        .is_some_and(|proof| key.verify_server(&fingerprint, &server_nonce, &client_nonce, proof));
    ensure!(
        proven,
        "The server could not prove it knows the password. Someone may be intercepting the connection, so nothing was saved."
    );
    welcome.paired(host, fingerprint)
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
    shared: Arc<Shared>,
    /// Frames for the socket thread; it hangs up once every clone is gone.
    outgoing: mpsc::Sender<String>,
}

struct Shared {
    welcome: Welcome,
    next: AtomicU64,
    closed: AtomicBool,
    pending: Mutex<BTreeMap<u64, mpsc::Sender<Result<Value>>>>,
    /// `rpc.watch` requests whose stream number has not arrived yet.
    opening: Mutex<BTreeMap<u64, mpsc::Sender<Result<Reply>>>>,
    streams: Mutex<BTreeMap<u64, mpsc::Sender<Result<Reply>>>>,
    events: Mutex<Vec<mpsc::Sender<Value>>>,
}

impl Connection {
    /// Try the saved hosts in order with the pinned fingerprint and token.
    pub fn connect(server: &SavedServer) -> Result<Self> {
        let mut errors = Vec::new();
        for host in &server.hosts {
            let attempt =
                open(host, server.port, Some(&server.fingerprint)).and_then(|(mut socket, _)| {
                    send(&mut socket, &json!({"type": "auth", "token": server.token}))?;
                    Ok((answer(&mut socket)?, socket))
                });
            match attempt {
                Ok((Answer::Welcome(welcome), socket)) => {
                    return Self::start(
                        socket,
                        Welcome {
                            device: welcome.device,
                            version: welcome.server.version,
                            full: welcome.access.as_deref() == Some("full"),
                            hosts: welcome.server.hosts,
                            port: welcome.server.port,
                        },
                    );
                }
                Ok(_) => anyhow::bail!("Unexpected answer to authentication"),
                // A refused token stays refused on every address.
                Err(error) if error.is::<Denied>() => {
                    return Err(error.context(format!("{} refused this computer", server.name)));
                }
                Err(error) => errors.push(format!("{host}: {error:#}")),
            }
        }
        ensure!(
            !server.hosts.is_empty(),
            "No address saved for {}",
            server.name
        );
        anyhow::bail!("{}", unreachable(&errors))
    }

    fn start(socket: Socket, welcome: Welcome) -> Result<Self> {
        socket
            .get_ref()
            .sock
            .set_read_timeout(Some(Duration::from_millis(25)))?;
        let (outgoing, queue) = mpsc::channel();
        let shared = Arc::new(Shared {
            welcome,
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            pending: Mutex::new(BTreeMap::new()),
            opening: Mutex::new(BTreeMap::new()),
            streams: Mutex::new(BTreeMap::new()),
            events: Mutex::new(Vec::new()),
        });
        let reader = shared.clone();
        thread::Builder::new()
            .name("difu-server-connection".into())
            .spawn(move || pump(socket, &queue, &reader))?;
        Ok(Self { shared, outgoing })
    }

    pub fn welcome(&self) -> Welcome {
        self.shared.welcome.clone()
    }

    pub fn closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// Push events other than `rpc.watch` (`device.paired`, `sessions`, ...) as
    /// `{"event", "data"}`, until the connection closes.
    pub fn events(&self) -> mpsc::Receiver<Value> {
        let (sender, receiver) = mpsc::channel();
        lock(&self.shared.events).push(sender);
        receiver
    }

    /// A phone-API method (`server.devices`, `device.unpair`, ...).
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        self.request(id, method, params)
    }

    fn request(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        let (sender, receiver) = mpsc::channel();
        lock(&self.shared.pending).insert(id, sender);
        // The socket thread marks the connection closed before it drops waiters.
        let frame = json!({"id": id, "method": method, "params": params}).to_string();
        if self.closed() || self.outgoing.send(frame).is_err() {
            lock(&self.shared.pending).remove(&id);
            anyhow::bail!(CLOSED);
        }
        receiver.recv().map_err(|_| anyhow::anyhow!(CLOSED))?
    }

    /// The internal service protocol over `rpc` (full devices only).
    pub fn rpc(&self, request: &Request) -> Result<Reply> {
        let mut result = self.call("rpc", json!({"request": request}))?;
        let reply = result
            .get_mut("reply")
            .map(Value::take)
            .context("The server sent no reply")?;
        serde_json::from_value(reply).context(
            "The server's reply does not match this difu version; run `difu server upgrade`",
        )
    }

    /// `rpc.watch`: replies arrive on the receiver until the watch is dropped,
    /// the server closes it, or the connection drops (the channel then closes).
    pub fn watch(&self, id: &str, version: Option<u64>) -> Result<Watch> {
        let (sender, replies) = mpsc::channel();
        let request = self.shared.next.fetch_add(1, Ordering::Relaxed);
        // The socket thread files the stream before handling any later frame.
        lock(&self.shared.opening).insert(request, sender);
        let result = self.request(request, "rpc.watch", json!({"id": id, "version": version}));
        lock(&self.shared.opening).remove(&request);
        let stream = result?
            .get("stream")
            .and_then(Value::as_u64)
            .context("The server opened no watch stream")?;
        Ok(Watch {
            replies,
            stream,
            connection: self.clone(),
        })
    }
}

/// A live `rpc.watch` stream; dropping it sends `rpc.unwatch`.
pub struct Watch {
    pub replies: mpsc::Receiver<Result<Reply>>,
    stream: u64,
    connection: Connection,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let shared = &self.connection.shared;
        if lock(&shared.streams).remove(&self.stream).is_some() && !self.connection.closed() {
            let id = shared.next.fetch_add(1, Ordering::Relaxed);
            let frame =
                json!({"id": id, "method": "rpc.unwatch", "params": {"stream": self.stream}});
            let _ = self.connection.outgoing.send(frame.to_string());
        }
    }
}

impl Shared {
    fn route(&self, text: &str) {
        let Ok(mut message) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let mut take = |key: &str| message.get_mut(key).map(Value::take);
        match take("event") {
            Some(Value::String(event)) if event == "rpc.watch" || event == "rpc.watch.closed" => {
                let mut data = take("data").unwrap_or_default();
                let Some(stream) = data.get("stream").and_then(Value::as_u64) else {
                    return;
                };
                let mut streams = lock(&self.streams);
                if event == "rpc.watch.closed" {
                    if let Some(sender) = streams.remove(&stream) {
                        let message = data.get("message").and_then(Value::as_str);
                        let _ = sender.send(Err(anyhow::anyhow!(
                            "{}",
                            message.unwrap_or("The server ended the watch")
                        )));
                    }
                    return;
                }
                let reply = data
                    .get_mut("reply")
                    .map(Value::take)
                    .context("Missing watch reply")
                    .and_then(|reply| serde_json::from_value(reply).map_err(Into::into));
                if let Some(sender) = streams.get(&stream)
                    && sender.send(reply).is_err()
                {
                    streams.remove(&stream);
                }
            }
            Some(event) => {
                let event = json!({"event": event, "data": take("data")});
                lock(&self.events).retain(|sender| sender.send(event.clone()).is_ok());
            }
            None => {
                let Some(id) = message.get("id").and_then(Value::as_u64) else {
                    return;
                };
                let error = message.get("error").map(|e| {
                    e.as_str()
                        .unwrap_or("The server reported an error")
                        .to_owned()
                });
                let result = match error {
                    Some(error) => Err(anyhow::anyhow!(error)),
                    None => Ok(message
                        .get_mut("result")
                        .map(Value::take)
                        .unwrap_or_default()),
                };
                if let Some(sender) = lock(&self.opening).remove(&id)
                    && let Some(stream) = result
                        .as_ref()
                        .ok()
                        .and_then(|r| r.get("stream"))
                        .and_then(Value::as_u64)
                {
                    lock(&self.streams).insert(stream, sender);
                }
                if let Some(sender) = lock(&self.pending).remove(&id) {
                    let _ = sender.send(result);
                }
            }
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        lock(&self.pending).clear();
        lock(&self.opening).clear();
        lock(&self.streams).clear();
        lock(&self.events).clear();
    }
}

/// The socket thread: short reads alternate with sending queued frames.
fn pump(mut socket: Socket, queue: &mpsc::Receiver<String>, shared: &Shared) {
    let mut heard = Instant::now();
    let _ = (|| -> Result<()> {
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    heard = Instant::now();
                    shared.route(text.as_str());
                }
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => heard = Instant::now(),
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error.into()),
            }
            loop {
                match queue.try_recv() {
                    Ok(frame) => socket.write(Message::text(frame))?,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        hang_up(&mut socket);
                        return Ok(());
                    }
                }
            }
            // Also sends the pongs queued while reading the server's pings.
            socket.flush()?;
            ensure!(heard.elapsed() < SILENCE, "The server stopped responding");
        }
    })();
    shared.close();
}

/// A parsed `difu://pair?...` link.
struct Link {
    hosts: Vec<String>,
    port: u16,
    fingerprint: String,
    code: String,
}

impl Link {
    fn parse(link: &str) -> Result<Self> {
        let url = url::Url::parse(link.trim()).context("This is not a difu pairing link")?;
        ensure!(
            url.scheme() == "difu" && url.host_str() == Some("pair"),
            "This is not a difu pairing link"
        );
        let pairs: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
        let get = |key: &str| {
            pairs
                .get(key)
                .filter(|value| !value.is_empty())
                .with_context(|| format!("The pairing link has no `{key}`"))
        };
        ensure!(
            get("v")?.parse::<u32>().ok() == Some(crate::agents::remote::PROTOCOL),
            "This pairing link needs a newer difu"
        );
        let hosts: Vec<String> = get("h")?
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned)
            .collect();
        ensure!(!hosts.is_empty(), "The pairing link has no address");
        Ok(Self {
            hosts,
            port: get("p")?
                .parse()
                .context("Invalid port in the pairing link")?,
            fingerprint: get("f")?.clone(),
            code: get("c")?.clone(),
        })
    }
}

/// Accepts only the pinned certificate, or with nothing pinned (password
/// pairing) any certificate, whose fingerprint the proofs then bind. Handshake
/// signatures are always verified, so the server must hold the certificate's key.
#[derive(Debug)]
struct Pinned {
    fingerprint: Option<String>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match &self.fingerprint {
            Some(pinned) if *pinned != fingerprint(end_entity) => {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::ApplicationVerificationFailure,
                ))
            }
            _ => Ok(ServerCertVerified::assertion()),
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

/// Base64url (no padding) SHA-256 of the leaf certificate, as in the QR code.
fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(certificate.as_ref()))
}

/// Connect, finish TLS and the WebSocket handshake, and report the fingerprint
/// of the certificate the server presented.
fn open(host: &str, port: u16, pinned: Option<&str>) -> Result<(Socket, String)> {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let mut last = None;
    let tcp = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("Cannot resolve {host}"))?
        .find_map(
            |address| match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
                Ok(tcp) => Some(tcp),
                Err(error) => {
                    last = Some(error);
                    None
                }
            },
        );
    let tcp = match tcp {
        Some(tcp) => tcp,
        None => {
            return Err(last
                .map(anyhow::Error::from)
                .unwrap_or_else(|| anyhow::anyhow!("No address found for {host}")));
        }
    };
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(20)))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("Cannot configure TLS")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint: pinned.map(str::to_owned),
            provider,
        }))
        .with_no_client_auth();
    let name =
        ServerName::try_from(host.to_owned()).or_else(|_| ServerName::try_from("difu.local"))?;
    let mut tls =
        rustls::StreamOwned::new(rustls::ClientConnection::new(Arc::new(config), name)?, tcp);
    while tls.conn.is_handshaking() {
        if let Err(error) = tls.conn.complete_io(&mut tls.sock) {
            let changed = error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                .is_some_and(|inner| {
                    matches!(
                        inner,
                        rustls::Error::InvalidCertificate(
                            rustls::CertificateError::ApplicationVerificationFailure
                        )
                    )
                });
            if changed {
                anyhow::bail!(
                    "{host} presented a different certificate than the paired server; pair again if it was reinstalled"
                );
            }
            return Err(anyhow::Error::from(error).context("TLS handshake failed"));
        }
    }
    let presented = tls
        .conn
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .map(fingerprint)
        .context("The server sent no certificate")?;
    let url = if host.contains(':') {
        format!("wss://[{host}]:{port}/")
    } else {
        format!("wss://{host}:{port}/")
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let (socket, _) = tungstenite::client::client_with_config(url.as_str(), tls, Some(config))
        .map_err(|error| anyhow::anyhow!("WebSocket handshake failed: {error}"))?;
    Ok((socket, presented))
}

fn send(socket: &mut Socket, message: &Value) -> Result<()> {
    socket.send(Message::text(message.to_string()))?;
    Ok(())
}

fn hang_up(socket: &mut Socket) {
    let _ = socket.close(None);
    let _ = socket.flush();
}

/// The server refused the credentials (`denied`).
#[derive(Debug)]
struct Denied(String);
impl std::fmt::Display for Denied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Denied {}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Welcome(Box<WelcomeMessage>),
    Challenge {
        salt: String,
        iterations: u32,
        nonce: String,
    },
    Denied {
        #[serde(default)]
        message: String,
    },
}

#[derive(Deserialize)]
struct WelcomeMessage {
    device: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    proof: Option<String>,
    server: ServerHello,
}

#[derive(Deserialize)]
struct ServerHello {
    name: String,
    version: String,
    #[serde(default)]
    hosts: Vec<String>,
    port: u16,
    #[serde(default)]
    id: Option<String>,
}

impl WelcomeMessage {
    /// The address that worked comes first, even when the server does not
    /// advertise it.
    fn paired(self, host: &str, fingerprint: String) -> Result<Paired> {
        let mut hosts = vec![host.to_owned()];
        for advertised in self.server.hosts {
            if !hosts.contains(&advertised) {
                hosts.push(advertised);
            }
        }
        Ok(Paired {
            server_id: self.server.id.unwrap_or_else(|| fingerprint.clone()),
            name: self.server.name,
            hosts,
            port: self.server.port,
            fingerprint,
            device: self.device,
            token: self
                .token
                .context("The server did not issue a device token")?,
            version: self.server.version,
            full: self.access.as_deref() == Some("full"),
        })
    }
}

/// Read the server's answer to an authentication message.
fn answer(socket: &mut Socket) -> Result<Answer> {
    let text = loop {
        match socket.read().context("Waiting for the server")? {
            Message::Text(text) => break text,
            Message::Close(_) => anyhow::bail!("The server closed the connection"),
            _ => {}
        }
    };
    match serde_json::from_str(text.as_str()).context("Unexpected message from the server")? {
        Answer::Denied { message } => Err(Denied(if message.is_empty() {
            "The server refused the connection".into()
        } else {
            message
        })
        .into()),
        answer => Ok(answer),
    }
}

fn access(full: bool) -> &'static str {
    if full { "full" } else { "standard" }
}

/// A fresh random device id, 16 characters of `[A-Za-z0-9_-]`.
fn device_id() -> Result<String> {
    let mut bytes = [0u8; 12];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn unreachable(errors: &[String]) -> String {
    format!("Cannot reach the server ({})", errors.join("; "))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::remote::{self, Command, Remote},
        cloud::password::Verifier,
        storage::Storage,
    };
    use std::{
        io::{BufReader, Write},
        net::TcpListener,
        os::unix::net::UnixListener,
    };

    const PASSWORD: &str = "correct horse battery";

    struct Fixture {
        _temp: tempfile::TempDir,
        storage: Storage,
        remote: Arc<Remote>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.remote.command(Command::Disable);
        }
    }

    fn fixture() -> Result<Fixture> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        Ok(Fixture {
            remote: remote::loopback(storage.clone()),
            storage,
            _temp: temp,
        })
    }

    impl Fixture {
        /// A fresh pairing link; each one works once.
        fn link(&self) -> Result<String> {
            self.remote
                .command(Command::Open)?
                .pairing
                .context("Missing pairing link")
        }
        fn port(&self) -> Result<u16> {
            self.link()?;
            Ok(self.remote.status()?.port)
        }
        fn password(&self) -> Result<()> {
            self.remote.command(Command::Password {
                verifier: Some(Verifier::with_iterations(PASSWORD, 100_000)?),
            })?;
            Ok(())
        }
        fn pair(&self, name: &str, full: bool) -> Result<(Paired, Connection)> {
            let paired = pair_with_link(&self.link()?, name, full)?;
            let connection = Connection::connect(&saved(&paired))?;
            Ok((paired, connection))
        }
    }

    fn saved(paired: &Paired) -> SavedServer {
        SavedServer {
            name: paired.name.clone(),
            id: paired.server_id.clone(),
            hosts: paired.hosts.clone(),
            port: paired.port,
            fingerprint: paired.fingerprint.clone(),
            device: paired.device.clone(),
            token: paired.token.clone(),
            ssh: None,
        }
    }

    fn query(link: &str, key: &str) -> Result<String> {
        url::Url::parse(link)?
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .with_context(|| format!("Missing `{key}`"))
    }

    fn read_value<S: io::Read + io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> Result<Value> {
        loop {
            if let Message::Text(text) = socket.read()? {
                return Ok(serde_json::from_str(text.as_str())?);
            }
        }
    }

    fn kind(value: &Value) -> &str {
        value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// One wrong password attempt over the raw protocol, without key derivation.
    fn guess(port: u16) -> Result<Value> {
        let (mut socket, _) = open("127.0.0.1", port, None)?;
        send(
            &mut socket,
            &json!({"type": "password", "device": {"id": "guesser-0001", "name": "Guesser"},
                    "nonce": password::nonce()?}),
        )?;
        let first = read_value(&mut socket)?;
        if kind(&first) != "challenge" {
            return Ok(first);
        }
        send(&mut socket, &json!({"type": "proof", "proof": "AAAA"}))?;
        read_value(&mut socket)
    }

    fn wait(condition: impl Fn() -> bool) -> Result<()> {
        let started = Instant::now();
        while !condition() {
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "Timed out waiting"
            );
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    #[test]
    fn qr_pairing_grants_the_requested_access_and_the_token_reconnects() -> Result<()> {
        let fixture = fixture()?;
        let link = fixture.link()?;
        let paired = pair_with_link(&link, "Ali's Mac", true)?;
        assert!(paired.full);
        assert_eq!(paired.fingerprint, query(&link, "f")?);
        assert_eq!(paired.server_id, paired.fingerprint);
        assert_eq!(paired.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(paired.hosts, vec!["127.0.0.1".to_owned()]);
        // The code worked once.
        assert!(pair_with_link(&link, "Again", false).is_err());
        let connection = Connection::connect(&saved(&paired))?;
        let welcome = connection.welcome();
        assert_eq!(welcome.device, paired.device);
        assert!(welcome.full);
        assert_eq!(welcome.port, paired.port);
        let devices = connection.call("server.devices", json!({}))?;
        let first = devices.pointer("/devices/0").context("Missing device")?;
        assert_eq!(first.get("access"), Some(&json!("full")));
        assert_eq!(first.get("current"), Some(&json!(true)));
        assert_eq!(first.get("connected"), Some(&json!(true)));
        let phone = pair_with_link(&fixture.link()?, "Ali's iPhone", false)?;
        assert!(!phone.full);
        let info = connection.call("server.info", json!({}))?;
        assert_eq!(info.get("hosts"), Some(&json!(["127.0.0.1"])));
        Ok(())
    }

    #[test]
    fn password_pairing_proves_both_sides_and_wrong_passwords_are_denied() -> Result<()> {
        let fixture = fixture()?;
        let port = fixture.port()?;
        let Err(off) = pair_with_password("127.0.0.1", port, PASSWORD, "iPad", false) else {
            anyhow::bail!("Password pairing must be off without a password");
        };
        assert!(format!("{off:#}").contains("Password pairing is off"));
        fixture.password()?;
        let Err(wrong) = pair_with_password("127.0.0.1", port, "incorrect horse", "iPad", false)
        else {
            anyhow::bail!("A wrong password must be denied");
        };
        assert!(format!("{wrong:#}").contains("Wrong server password"));
        let paired = pair_with_password("127.0.0.1", port, PASSWORD, "Ali's iPad", false)?;
        assert!(!paired.full);
        assert_eq!(paired.fingerprint, query(&fixture.link()?, "f")?);
        let connection = Connection::connect(&saved(&paired))?;
        assert!(!connection.welcome().full);
        let full = pair_with_password("127.0.0.1", port, PASSWORD, "Build Mac", true)?;
        assert!(full.full);
        Ok(())
    }

    #[test]
    fn welcome_reports_identity_mode_and_configured_hosts_first() -> Result<()> {
        let fixture = fixture()?;
        std::fs::write(&fixture.storage.config, r#"{"server": {"enabled": true}}"#)?;
        fixture.password()?;
        let status = fixture.remote.command(Command::Configure {
            hosts: vec!["localhost".into()],
            clear_hosts: false,
            port: None,
        })?;
        assert_eq!(status.configured_hosts, vec!["localhost".to_owned()]);
        let link = fixture.link()?;
        assert_eq!(
            fixture.remote.status()?.hosts,
            vec!["localhost", "127.0.0.1"]
        );
        assert_eq!(query(&link, "h")?, "localhost,127.0.0.1");
        let (mut socket, fingerprint) = open("127.0.0.1", query(&link, "p")?.parse()?, None)?;
        send(
            &mut socket,
            &json!({"type": "pair", "code": query(&link, "c")?,
                    "device": {"id": "tablet-0001", "name": "Tablet"}}),
        )?;
        let welcome = read_value(&mut socket)?;
        assert_eq!(kind(&welcome), "welcome");
        assert_eq!(welcome.get("access"), Some(&json!("standard")));
        assert_eq!(welcome.get("proof"), None);
        let server = welcome.get("server").context("Missing server")?;
        assert_eq!(server.get("id"), Some(&json!(fingerprint)));
        assert_eq!(server.get("fingerprint"), Some(&json!(fingerprint)));
        assert_eq!(server.get("mode"), Some(&json!("server")));
        assert_eq!(server.get("passwordPairing"), Some(&json!(true)));
        assert_eq!(
            server.get("hosts"),
            Some(&json!(["localhost", "127.0.0.1"]))
        );
        assert!(
            fixture
                .remote
                .command(Command::Configure {
                    hosts: vec!["example.com:80".into()],
                    clear_hosts: false,
                    port: None,
                })
                .is_err()
        );
        let cleared = fixture.remote.command(Command::Configure {
            hosts: Vec::new(),
            clear_hosts: true,
            port: None,
        })?;
        assert!(cleared.configured_hosts.is_empty());
        Ok(())
    }

    #[test]
    fn client_rejects_a_server_that_cannot_prove_the_password() -> Result<()> {
        let key = rcgen::KeyPair::generate()?;
        let certificate =
            rcgen::CertificateParams::new(vec!["difu.local".to_owned()])?.self_signed(&key)?;
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let verifier = Verifier::with_iterations(PASSWORD, 100_000)?;
        // Even knowing the password, a relay proves the real server's certificate,
        // not the one it showed this client.
        let impostor = thread::spawn(move || -> Result<()> {
            let (tcp, _) = listener.accept()?;
            let tls =
                rustls::StreamOwned::new(rustls::ServerConnection::new(Arc::new(config))?, tcp);
            let mut socket = tungstenite::accept(tls).map_err(|e| anyhow::anyhow!("{e}"))?;
            let hello = read_value(&mut socket)?;
            let client_nonce = hello
                .get("nonce")
                .and_then(Value::as_str)
                .context("nonce")?
                .to_owned();
            let server_nonce = password::nonce()?;
            socket.send(Message::text(
                json!({"type": "challenge", "salt": verifier.salt,
                       "iterations": verifier.iterations, "nonce": server_nonce})
                .to_string(),
            ))?;
            read_value(&mut socket)?;
            let proof = verifier.server_proof("the-real-server", &server_nonce, &client_nonce)?;
            socket.send(Message::text(
                json!({"type": "welcome", "protocol": 1, "device": "x", "token": "t",
                       "access": "full", "proof": proof,
                       "server": {"name": "box", "version": "1.0.0", "hosts": [],
                                  "port": port, "fingerprint": "f"}})
                .to_string(),
            ))?;
            Ok(())
        });
        let Err(error) = pair_with_password("127.0.0.1", port, PASSWORD, "Mac", true) else {
            anyhow::bail!("An unproven server must be rejected");
        };
        assert!(format!("{error:#}").contains("could not prove"));
        impostor
            .join()
            .map_err(|_| anyhow::anyhow!("impostor panicked"))??;
        Ok(())
    }

    #[test]
    fn pinned_fingerprints_and_revoked_tokens_are_refused() -> Result<()> {
        let fixture = fixture()?;
        let (paired, _connection) = fixture.pair("Mac", true)?;
        let mut other = saved(&paired);
        other.fingerprint = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let Err(error) = Connection::connect(&other) else {
            anyhow::bail!("A different certificate must be refused");
        };
        assert!(format!("{error:#}").contains("different certificate"));
        let mut stale = saved(&paired);
        stale.token = "not-a-token".into();
        let Err(error) = Connection::connect(&stale) else {
            anyhow::bail!("An unknown token must be refused");
        };
        assert!(error.chain().any(|cause| cause.is::<Denied>()));
        // A forgotten device retrying its old token does not block pairing.
        for _ in 0..6 {
            assert!(Connection::connect(&stale).is_err());
        }
        fixture.pair("Mac again", true)?;
        Ok(())
    }

    #[test]
    fn standard_devices_cannot_reach_the_service_protocol() -> Result<()> {
        let fixture = fixture()?;
        let (_, connection) = fixture.pair("Ali's iPhone", false)?;
        let Err(error) = connection.rpc(&Request::List) else {
            anyhow::bail!("Standard devices must not use rpc");
        };
        assert!(format!("{error:#}").contains("standard access"));
        assert!(connection.watch("one", None).is_err());
        assert!(
            connection
                .call("rpc.unwatch", json!({"stream": 1}))
                .is_err()
        );
        // The phone API keeps working.
        assert!(connection.call("server.devices", json!({})).is_ok());
        Ok(())
    }

    #[test]
    fn failed_attempts_lock_out_password_pairing_but_not_paired_tokens() -> Result<()> {
        let fixture = fixture()?;
        let (paired, _) = fixture.pair("Mac", true)?;
        fixture.password()?;
        let port = paired.port;
        for _ in 0..5 {
            let denied = guess(port)?;
            assert_eq!(kind(&denied), "denied");
            assert_eq!(
                denied.get("message"),
                Some(&json!("Wrong server password."))
            );
        }
        // Denied before any challenge, even with the right password.
        let limited = guess(port)?;
        assert_eq!(kind(&limited), "denied");
        assert!(
            limited
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("Too many failed attempts"))
        );
        let Err(error) = pair_with_password("127.0.0.1", port, PASSWORD, "iPad", false) else {
            anyhow::bail!("A locked-out address must not pair");
        };
        assert!(format!("{error:#}").contains("Too many failed attempts"));
        assert!(pair_with_link(&fixture.link()?, "Phone", false).is_err());
        let connection = Connection::connect(&saved(&paired))?;
        assert!(connection.call("server.devices", json!({})).is_ok());
        Ok(())
    }

    #[test]
    fn unpairing_revokes_the_caller_and_others_hear_about_pairings() -> Result<()> {
        let fixture = fixture()?;
        let (mac, connection) = fixture.pair("Mac", true)?;
        let events = connection.events();
        let (phone, phone_connection) = fixture.pair("Ali's iPhone", false)?;
        let paired = events.recv_timeout(Duration::from_secs(5))?;
        assert_eq!(paired.get("event"), Some(&json!("device.paired")));
        assert_eq!(paired.pointer("/data/id"), Some(&json!(phone.device)));
        assert_eq!(paired.pointer("/data/name"), Some(&json!("Ali's iPhone")));
        assert_eq!(paired.pointer("/data/access"), Some(&json!("standard")));
        let devices = connection.call("server.devices", json!({}))?;
        let listed = devices
            .get("devices")
            .and_then(Value::as_array)
            .context("devices")?;
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|d| {
            d.get("id") == Some(&json!(phone.device)) && d.get("current") == Some(&json!(false))
        }));
        connection.call("server.devices.forget", json!({"device": phone.device}))?;
        wait(|| phone_connection.closed())?;
        assert!(Connection::connect(&saved(&phone)).is_err());
        assert_eq!(connection.call("device.unpair", json!({}))?, json!({}));
        wait(|| connection.closed())?;
        assert!(connection.call("server.devices", json!({})).is_err());
        assert!(Connection::connect(&saved(&mac)).is_err());
        assert!(fixture.remote.status()?.devices.is_empty());
        Ok(())
    }

    /// Answers like the agent service on its Unix socket: lists, a large
    /// changes reply, and watches that end or run until the client leaves.
    fn fake_service(storage: &Storage, large: usize) -> Result<mpsc::Receiver<String>> {
        let listener = UnixListener::bind(crate::agents::server::socket(storage)?)?;
        let (seen, observed) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                thread::spawn(move || -> Result<()> {
                    let mut reader = BufReader::new(stream.try_clone()?);
                    let request: Request =
                        serde_json::from_str(&crate::agents::client::read_line(&mut reader)?)?;
                    let mut stream = stream;
                    let mut reply = |reply: &Reply| -> io::Result<()> {
                        let mut line = serde_json::to_vec(reply)?;
                        line.push(b'\n');
                        stream.write_all(&line)
                    };
                    match request {
                        Request::List => reply(&Reply::Sessions(Vec::new()))?,
                        Request::Changes { .. } => reply(&Reply::Changes("x".repeat(large)))?,
                        Request::Watch { id, .. } if id == "gone" => {
                            reply(&Reply::Unchanged)?;
                            reply(&Reply::Error("Session not found".into()))?;
                        }
                        Request::Watch { id, .. } => {
                            while reply(&Reply::Unchanged).is_ok() {
                                thread::sleep(Duration::from_millis(20));
                            }
                            let _ = seen.send(format!("unwatched {id}"));
                        }
                        _ => reply(&Reply::Error("Unsupported".into()))?,
                    }
                    Ok(())
                });
            }
        });
        Ok(observed)
    }

    #[test]
    fn full_devices_tunnel_service_requests_and_watches() -> Result<()> {
        let fixture = fixture()?;
        let large = 17 << 20;
        let observed = fake_service(&fixture.storage, large)?;
        let (_, connection) = fixture.pair("Mac", true)?;
        assert!(
            matches!(connection.rpc(&Request::List)?, Reply::Sessions(list) if list.is_empty())
        );
        // Larger than a single 16 MiB frame.
        let Reply::Changes(patch) = connection.rpc(&Request::Changes { id: "one".into() })? else {
            anyhow::bail!("Expected changes");
        };
        assert_eq!(patch.len(), large);
        let Err(error) = connection.rpc(&Request::Delete { id: "one".into() }) else {
            anyhow::bail!("Service errors must surface");
        };
        assert!(format!("{error:#}").contains("Unsupported"));
        let watching = Request::Watch {
            id: "one".into(),
            version: None,
        };
        assert!(connection.rpc(&watching).is_err());

        let ending = connection.watch("gone", None)?;
        assert!(matches!(
            ending.replies.recv_timeout(Duration::from_secs(5))?,
            Ok(Reply::Unchanged)
        ));
        assert!(matches!(
            ending.replies.recv_timeout(Duration::from_secs(5))?,
            Ok(Reply::Error(_))
        ));
        let Err(closed) = ending.replies.recv_timeout(Duration::from_secs(5))? else {
            anyhow::bail!("Expected the watch to close");
        };
        assert!(closed.to_string().contains("Session not found"));
        assert!(ending.replies.recv_timeout(Duration::from_secs(5)).is_err());

        let first = connection.watch("one", None)?;
        let second = connection.watch("two", Some(3))?;
        for watch in [&first, &second] {
            assert!(matches!(
                watch.replies.recv_timeout(Duration::from_secs(5))?,
                Ok(Reply::Unchanged)
            ));
        }
        drop(first);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(5))?,
            "unwatched one"
        );
        assert!(matches!(
            second.replies.recv_timeout(Duration::from_secs(5))?,
            Ok(Reply::Unchanged)
        ));
        // Leaving closes the remaining service subscription too.
        drop(second);
        drop(connection);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(5))?,
            "unwatched two"
        );
        Ok(())
    }

    #[test]
    fn pairing_links_are_validated() {
        assert!(Link::parse("https://example.com").is_err());
        assert!(Link::parse("difu://pair?v=2&h=a&p=1&f=f&c=c").is_err());
        assert!(Link::parse("difu://pair?v=1&h=&p=1&f=f&c=c").is_err());
        let link = Link::parse("difu://pair?v=1&n=Box&h=a.example,10.0.0.2&p=3438&f=fp&c=code");
        assert!(link.is_ok_and(|l| l.hosts == ["a.example", "10.0.0.2"] && l.port == 3438));
    }
}
