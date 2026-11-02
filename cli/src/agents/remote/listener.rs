//! TLS WebSocket listener. Each connection is one thread that alternates
//! between short socket reads and draining replies/events queued by workers.
//! Failed pairing is limited per client address (and password guesses
//! globally) before any secret is checked; token authentication of paired
//! devices is never locked out. A connection must finish TLS, the WebSocket
//! handshake and authentication (including a password challenge) within
//! [`AUTH_DEADLINE`] of being accepted, so slow clients cannot hold the
//! per-address or global connection slots.
use super::{Access, Enrolled, Remote, identity::Identity};
use crate::cloud::password;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    io,
    net::{IpAddr, Ipv6Addr, Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, protocol::WebSocketConfig};

const MAX_PEERS: usize = 32;
/// Connections one address may hold open before authenticating.
const MAX_PENDING: usize = 4;
/// This many failures within the window lock an address out of pairing.
const MAX_FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);
const LOCKOUT: Duration = Duration::from_secs(10 * 60);
/// Wrong server passwords from all addresses together, per hour.
const MAX_PASSWORD_FAILURES: usize = 30;
const PASSWORD_WINDOW: Duration = Duration::from_secs(60 * 60);
/// Slows guessing; tokens and codes are too long to brute-force anyway.
const DENIAL_DELAY: Duration = Duration::from_millis(if cfg!(test) { 20 } else { 1000 });
const LIMITED: &str = "Too many failed attempts from this address. Try again in a few minutes.";
/// From accept until the client is authenticated, however it spreads its bytes.
const AUTH_DEADLINE: Duration = Duration::from_secs(15);
const MAX_MESSAGE: usize = 64 << 20;

pub struct Server {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    /// Bind the saved port, or (unless `fixed`) a random high port when it is
    /// unset or taken.
    pub fn bind(
        remote: Arc<Remote>,
        identity: Arc<Identity>,
        preferred: u16,
        fixed: bool,
    ) -> Result<(Self, u16)> {
        let mut candidates = Vec::new();
        if preferred != 0 || fixed {
            candidates.push(preferred);
        }
        if !fixed {
            for _ in 0..12 {
                let mut bytes = [0u8; 2];
                rustls::crypto::ring::default_provider()
                    .secure_random
                    .fill(&mut bytes)
                    .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
                candidates.push(49152 + u16::from_le_bytes(bytes) % 16000);
            }
        }
        let mut last = None;
        let listener = candidates.into_iter().find_map(|port| {
            let bound = match &remote.bind {
                Some(host) => TcpListener::bind((host.as_str(), port)),
                // A dual-stack socket serves `.local` names that resolve to IPv6 too.
                None => TcpListener::bind(("::", port))
                    .or_else(|_| TcpListener::bind(("0.0.0.0", port))),
            };
            match bound {
                Ok(listener) => Some(listener),
                Err(error) => {
                    last = Some(error);
                    None
                }
            }
        });
        let listener = match listener {
            Some(listener) => listener,
            None => {
                let error = last
                    .map(anyhow::Error::from)
                    .unwrap_or_else(|| anyhow::anyhow!("No port available"));
                return Err(if fixed {
                    error.context(format!("Cannot listen for clients on port {preferred}"))
                } else {
                    error.context("Cannot listen for the iOS app on the local network")
                });
            }
        };
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::Builder::new()
            .name("difu-remote".into())
            .spawn(move || accept(listener, remote, identity, flag))?;
        Ok((
            Self {
                stop,
                handle: Some(handle),
            },
            port,
        ))
    }

    pub fn running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed) && self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn accept(
    listener: TcpListener,
    remote: Arc<Remote>,
    identity: Arc<Identity>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        remote.peers.expire(Instant::now());
        match listener.accept() {
            // Unauthenticated sockets hold a thread until their handshake times out.
            Ok(_) if remote.peers.count() >= MAX_PEERS => {}
            Ok((stream, address)) => {
                let address = address.ip();
                if !remote.guard.admit(address) {
                    continue;
                }
                // Moved into the thread, so the slot is released even if spawning fails.
                let admission = Admission {
                    remote: remote.clone(),
                    address,
                };
                let identity = identity.clone();
                let _ = thread::Builder::new()
                    .name("difu-remote-peer".into())
                    .spawn(move || {
                        let remote = admission.remote.clone();
                        if let Err(error) = serve(&remote, &identity, stream, admission) {
                            eprintln!("Remote control client {address}: {error:#}");
                        }
                    });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                eprintln!("Remote control listener: {error}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// An unauthenticated connection's slot in the per-address limit.
struct Admission {
    remote: Arc<Remote>,
    address: IpAddr,
}
impl Drop for Admission {
    fn drop(&mut self) {
        self.remote.guard.release(self.address);
    }
}

/// Live connections, so disabling remote control or forgetting a device also
/// disconnects it immediately, pairing events reach every other device, and
/// a connection still unauthenticated at its deadline is cut off.
pub struct Peers {
    next: AtomicU64,
    peers: Mutex<Vec<Peer>>,
    /// Time from accept to authentication.
    limit: Duration,
}
impl Default for Peers {
    fn default() -> Self {
        Self::with_deadline(AUTH_DEADLINE)
    }
}
struct Peer {
    serial: u64,
    device: Option<String>,
    socket: TcpStream,
    events: Option<SyncSender<String>>,
    /// Cleared once authenticated.
    deadline: Option<Instant>,
    expired: bool,
}
impl Peers {
    fn with_deadline(limit: Duration) -> Self {
        Self {
            next: AtomicU64::new(0),
            peers: Mutex::new(Vec::new()),
            limit,
        }
    }
    fn add(&self, socket: &TcpStream) -> Result<u64> {
        let serial = self.next.fetch_add(1, Ordering::Relaxed);
        let socket = socket.try_clone()?;
        if let Ok(mut peers) = self.peers.lock() {
            peers.push(Peer {
                serial,
                device: None,
                socket,
                events: None,
                deadline: Some(Instant::now() + self.limit),
                expired: false,
            });
        }
        Ok(serial)
    }
    /// The connection authenticated in time; it stays until it closes.
    fn authenticated(&self, serial: u64) {
        if let Ok(mut peers) = self.peers.lock()
            && let Some(peer) = peers.iter_mut().find(|p| p.serial == serial)
        {
            peer.deadline = None;
        }
    }
    /// Shuts down connections still unauthenticated at their deadline. Their
    /// blocked reads and writes fail at once, wherever the handshake is.
    fn expire(&self, now: Instant) {
        if let Ok(mut peers) = self.peers.lock() {
            for peer in peers
                .iter_mut()
                .filter(|p| p.deadline.is_some_and(|deadline| deadline <= now))
            {
                peer.deadline = None;
                peer.expired = true;
                let _ = peer.socket.shutdown(Shutdown::Both);
            }
        }
    }
    fn expired(&self, serial: u64) -> bool {
        self.peers
            .lock()
            .is_ok_and(|peers| peers.iter().any(|p| p.serial == serial && p.expired))
    }
    fn identify(&self, serial: u64, device: &str, events: SyncSender<String>) {
        if let Ok(mut peers) = self.peers.lock()
            && let Some(peer) = peers.iter_mut().find(|p| p.serial == serial)
        {
            peer.device = Some(device.to_owned());
            peer.events = Some(events);
        }
    }
    fn remove(&self, serial: u64) {
        if let Ok(mut peers) = self.peers.lock() {
            peers.retain(|p| p.serial != serial);
        }
    }
    fn count(&self) -> usize {
        self.peers.lock().map(|peers| peers.len()).unwrap_or(0)
    }
    /// Queue an event for every authenticated connection except `serial`.
    fn broadcast(&self, serial: u64, event: &Value) {
        let text = event.to_string();
        if let Ok(peers) = self.peers.lock() {
            for events in peers
                .iter()
                .filter(|p| p.serial != serial)
                .filter_map(|p| p.events.as_ref())
            {
                let _ = events.try_send(text.clone());
            }
        }
    }
    pub fn devices(&self) -> BTreeSet<String> {
        self.peers
            .lock()
            .map(|peers| peers.iter().filter_map(|p| p.device.clone()).collect())
            .unwrap_or_default()
    }
    pub fn close_all(&self) {
        if let Ok(peers) = self.peers.lock() {
            for peer in peers.iter() {
                let _ = peer.socket.shutdown(Shutdown::Both);
            }
        }
    }
    pub fn close_device(&self, device: &str) {
        if let Ok(peers) = self.peers.lock() {
            for peer in peers.iter().filter(|p| p.device.as_deref() == Some(device)) {
                let _ = peer.socket.shutdown(Shutdown::Both);
            }
        }
    }
}

/// Failure counts per client address (IPv6 per /64) and for passwords overall.
#[derive(Default)]
pub struct Guard(Mutex<Limits>);
#[derive(Default)]
struct Limits {
    failures: HashMap<IpAddr, Vec<Instant>>,
    locked: HashMap<IpAddr, Instant>,
    passwords: VecDeque<Instant>,
    pending: HashMap<IpAddr, usize>,
}
impl Guard {
    fn limits(&self) -> std::sync::MutexGuard<'_, Limits> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Reserve an unauthenticated connection slot for this address.
    fn admit(&self, address: IpAddr) -> bool {
        let mut limits = self.limits();
        let pending = limits.pending.entry(group(address)).or_default();
        if *pending >= MAX_PENDING {
            return false;
        }
        *pending += 1;
        true
    }
    fn release(&self, address: IpAddr) {
        let mut limits = self.limits();
        let address = group(address);
        if let Some(pending) = limits.pending.get_mut(&address) {
            *pending = pending.saturating_sub(1);
            if *pending == 0 {
                limits.pending.remove(&address);
            }
        }
    }
    /// Pairing from this address is refused without checking the secret.
    fn locked(&self, address: IpAddr, now: Instant) -> bool {
        let mut limits = self.limits();
        let address = group(address);
        match limits.locked.get(&address) {
            Some(since) if now.saturating_duration_since(*since) < LOCKOUT => true,
            Some(_) => {
                limits.locked.remove(&address);
                false
            }
            None => false,
        }
    }
    /// Too many wrong passwords overall: password pairing pauses for everyone.
    fn passwords_paused(&self, now: Instant) -> bool {
        let mut limits = self.limits();
        prune(&mut limits.passwords, now, PASSWORD_WINDOW);
        limits.passwords.len() >= MAX_PASSWORD_FAILURES
    }
    fn failed(&self, address: IpAddr, password: bool, now: Instant) {
        let mut limits = self.limits();
        let address = group(address);
        if password {
            limits.passwords.push_back(now);
            prune(&mut limits.passwords, now, PASSWORD_WINDOW);
        }
        if limits.failures.len() > 4096 {
            limits.failures.retain(|_, times| {
                times.retain(|t| now.saturating_duration_since(*t) < FAILURE_WINDOW);
                !times.is_empty()
            });
        }
        let times = limits.failures.entry(address).or_default();
        times.retain(|t| now.saturating_duration_since(*t) < FAILURE_WINDOW);
        times.push(now);
        if times.len() >= MAX_FAILURES {
            limits.failures.remove(&address);
            limits.locked.insert(address, now);
        }
    }
}

fn prune(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while times
        .front()
        .is_some_and(|t| now.saturating_duration_since(*t) >= window)
    {
        times.pop_front();
    }
}

/// One key per IPv4 address and per IPv6 /64, which one client can rotate within.
fn group(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        IpAddr::V6(v6) => {
            let [a, b, c, d, ..] = v6.segments();
            IpAddr::V6(Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Hello {
    Auth {
        token: String,
    },
    Pair {
        code: String,
        device: DeviceHello,
        #[serde(default)]
        access: Access,
    },
    Password {
        device: DeviceHello,
        #[serde(default)]
        access: Access,
        nonce: String,
    },
}
#[derive(Deserialize)]
struct DeviceHello {
    id: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Proof { proof: String },
}

/// A successful first exchange.
struct Authenticated {
    device: String,
    access: Access,
    /// Set when this connection paired a new device.
    enrolled: Option<Enrolled>,
    /// The server's password proof.
    proof: Option<String>,
}

type Socket = tungstenite::WebSocket<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>;

fn serve(
    remote: &Arc<Remote>,
    identity: &Identity,
    tcp: TcpStream,
    admission: Admission,
) -> Result<()> {
    tcp.set_nonblocking(false)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(20)))?;
    let address = admission.address;
    let serial = remote.peers.add(&tcp)?;
    let result = (|| {
        let opened = (|| {
            let tls = rustls::ServerConnection::new(identity.tls.clone())?;
            let stream = rustls::StreamOwned::new(tls, tcp);
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE))
                .max_frame_size(Some(MAX_MESSAGE));
            let mut socket = tungstenite::accept_with_config(stream, Some(config))
                .map_err(|error| anyhow::anyhow!("WebSocket handshake failed: {error}"))?;
            let authenticated = authenticate(remote, identity, &mut socket, address, serial)?;
            anyhow::Ok(authenticated.map(|a| (socket, a)))
        })();
        drop(admission);
        let opened = match opened {
            Err(_) if remote.peers.expired(serial) => anyhow::bail!(
                "Disconnected: not authenticated within {} seconds",
                remote.peers.limit.as_secs_f32()
            ),
            opened => opened?,
        };
        let Some((mut socket, authenticated)) = opened else {
            return Ok(());
        };
        remote.peers.authenticated(serial);
        run(
            remote,
            serial,
            &authenticated.device,
            authenticated.access,
            &mut socket,
        )
    })();
    remote.peers.remove(serial);
    result
}

fn read_text(socket: &mut Socket, waiting: &str) -> Result<String> {
    loop {
        match socket.read().with_context(|| waiting.to_owned())? {
            Message::Text(text) => return Ok(text.as_str().to_owned()),
            Message::Ping(_) | Message::Pong(_) => {}
            _ => anyhow::bail!("Expected an authentication message"),
        }
    }
}

fn authenticate(
    remote: &Remote,
    identity: &Identity,
    socket: &mut Socket,
    address: IpAddr,
    serial: u64,
) -> Result<Option<Authenticated>> {
    let text = read_text(socket, "Waiting for the client to identify itself")?;
    let guard = &remote.guard;
    let outcome = match serde_json::from_str::<Hello>(&text) {
        // Tokens carry 256 bits, so they are neither limited nor counted: a
        // forgotten phone retrying its old token must not lock its owner out.
        Ok(Hello::Auth { token }) => {
            remote
                .authenticate(&token)
                .map(|(device, access)| Authenticated {
                    device,
                    access,
                    enrolled: None,
                    proof: None,
                })
        }
        Ok(Hello::Pair { .. }) if guard.locked(address, Instant::now()) => {
            Err(anyhow::anyhow!(LIMITED))
        }
        Ok(Hello::Pair {
            code,
            device,
            access,
        }) => remote
            .pair(&code, &device.id, &device.name, access)
            .inspect_err(|_| guard.failed(address, false, Instant::now()))
            .map(|enrolled| Authenticated {
                device: enrolled.id.clone(),
                access: enrolled.access,
                enrolled: Some(enrolled),
                proof: None,
            }),
        Ok(Hello::Password {
            device,
            access,
            nonce,
        }) => password_pairing(remote, identity, socket, address, &device, access, &nonce),
        Err(error) => Err(anyhow::anyhow!("Invalid authentication message: {error}")),
    };
    let authenticated = match outcome {
        Ok(authenticated) => authenticated,
        Err(error) => {
            thread::sleep(DENIAL_DELAY);
            let denied = json!({"type": "denied", "message": format!("{error:#}")});
            let _ = socket.send(Message::text(denied.to_string()));
            let _ = socket.close(None);
            let _ = socket.flush();
            return Ok(None);
        }
    };
    let mut welcome = json!({
        "type": "welcome",
        "protocol": super::PROTOCOL,
        "device": authenticated.device,
        "token": authenticated.enrolled.as_ref().map(|e| e.token.clone()),
        "access": authenticated.access,
        "server": {
            "name": super::network::name(),
            "version": env!("CARGO_PKG_VERSION"),
            "hosts": remote.hosts(),
            "port": socket.get_ref().sock.local_addr()?.port(),
            "fingerprint": identity.fingerprint,
            "id": identity.fingerprint,
            "mode": if remote.server_mode() { "server" } else { "computer" },
            "passwordPairing": remote.verifier().is_some(),
        },
    });
    if let (Some(proof), Some(fields)) = (&authenticated.proof, welcome.as_object_mut()) {
        fields.insert("proof".into(), json!(proof));
    }
    socket.send(Message::text(welcome.to_string()))?;
    if let Some(enrolled) = &authenticated.enrolled {
        remote.peers.broadcast(
            serial,
            &json!({"event": "device.paired", "data": {
                "id": enrolled.id,
                "name": enrolled.name,
                "access": enrolled.access,
                "paired": enrolled.paired,
            }}),
        );
    }
    Ok(Some(authenticated))
}

/// `password` → `challenge` → `proof` → `welcome` with the server's proof. Both
/// proofs cover this server's certificate fingerprint, so a relay is refused.
fn password_pairing(
    remote: &Remote,
    identity: &Identity,
    socket: &mut Socket,
    address: IpAddr,
    device: &DeviceHello,
    access: Access,
    client_nonce: &str,
) -> Result<Authenticated> {
    let now = Instant::now();
    ensure!(
        !remote.guard.locked(address, now) && !remote.guard.passwords_paused(now),
        "{LIMITED}"
    );
    let verifier = remote.verifier().context(
        "Password pairing is off on this server. Set a password with `difu remote password` or pair with a QR code.",
    )?;
    ensure!(
        (16..=128).contains(&client_nonce.len())
            && client_nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid pairing nonce"
    );
    let server_nonce = password::nonce()?;
    let challenge = json!({
        "type": "challenge",
        "salt": verifier.salt,
        "iterations": verifier.iterations,
        "nonce": server_nonce,
    });
    socket.send(Message::text(challenge.to_string()))?;
    // Deriving the key takes a phone about a second; the authentication
    // deadline bounds the whole exchange.
    let text = read_text(socket, "Waiting for the password proof")?;
    let Answer::Proof { proof } =
        serde_json::from_str(&text).context("Expected a password proof")?;
    if !verifier.accepts(&identity.fingerprint, &server_nonce, client_nonce, &proof) {
        remote.guard.failed(address, true, Instant::now());
        anyhow::bail!("Wrong server password.");
    }
    let enrolled = remote.pair_with_password(&device.id, &device.name, access)?;
    Ok(Authenticated {
        device: enrolled.id.clone(),
        access: enrolled.access,
        proof: Some(verifier.server_proof(&identity.fingerprint, &server_nonce, client_nonce)?),
        enrolled: Some(enrolled),
    })
}

fn run(
    remote: &Arc<Remote>,
    serial: u64,
    device: &str,
    access: Access,
    socket: &mut Socket,
) -> Result<()> {
    socket
        .get_ref()
        .sock
        .set_read_timeout(Some(Duration::from_millis(25)))?;
    let (sender, outgoing) = mpsc::sync_channel::<String>(512);
    let port = socket.get_ref().sock.local_addr()?.port();
    remote.peers.identify(serial, device, sender.clone());
    let connection =
        super::api::Connection::new(remote.clone(), device.to_owned(), access, port, sender);
    let mut pinged = Instant::now();
    let mut checked = Instant::now();
    let result = (|| -> Result<()> {
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => connection.handle(text.as_str()),
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(());
                }
                // Phones vanish without closing when the app is suspended.
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
            let mut wrote = false;
            while let Ok(text) = outgoing.try_recv() {
                socket.write(Message::text(text))?;
                wrote = true;
            }
            if pinged.elapsed() >= Duration::from_secs(15) {
                socket.write(Message::Ping(Vec::new().into()))?;
                pinged = Instant::now();
                wrote = true;
            }
            if wrote {
                socket.flush()?;
            }
            // `device.unpair` was answered above; now the server hangs up.
            if connection.revoked() {
                let _ = socket.close(None);
                let _ = socket.flush();
                return Ok(());
            }
            if checked.elapsed() >= Duration::from_secs(1) {
                checked = Instant::now();
                if !remote.enabled() || !remote.paired(device) {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return Ok(());
                }
            }
        }
    })();
    connection.close();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::io::{Read, Write};

    const LIMIT: Duration = Duration::from_millis(800);

    /// Listens on loopback with a short authentication deadline.
    fn listening(temp: &tempfile::TempDir) -> Result<(Arc<Remote>, u16)> {
        let remote = Arc::new(Remote {
            storage: Storage {
                config: temp.path().join("config.json"),
                cache: temp.path().join("cache"),
            },
            home: temp.path().join("remote"),
            bind: Some("127.0.0.1".into()),
            identity: Mutex::new(None),
            state: Mutex::new(super::super::State {
                settings: super::super::Settings::default(),
                pairing: None,
                server: None,
                error: None,
            }),
            peers: Peers::with_deadline(LIMIT),
            guard: Guard::default(),
        });
        let port = remote.command(super::super::Command::Open)?.port;
        Ok((remote, port))
    }

    /// Waits until the server dropped every connection and released its
    /// unauthenticated slots.
    fn released(remote: &Remote) -> Result<()> {
        let started = Instant::now();
        while remote.peers.count() > 0 || !remote.guard.limits().pending.is_empty() {
            ensure!(
                started.elapsed() < Duration::from_secs(5),
                "The server kept the connection's slots"
            );
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    #[test]
    fn a_client_trickling_its_tls_handshake_is_cut_off_at_the_deadline() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (remote, port) = listening(&temp)?;
        let mut client = TcpStream::connect(("127.0.0.1", port))?;
        client.set_read_timeout(Some(Duration::from_millis(50)))?;
        let started = Instant::now();
        // A TLS record header announcing a 512-byte handshake, then its body
        // one byte at a time: every read on the server gets data well within
        // its own timeout, so only the overall deadline ends this.
        let mut bytes = [0x16u8, 0x03, 0x01, 0x02, 0x00]
            .into_iter()
            .chain(std::iter::repeat(0));
        let closed = loop {
            let byte = bytes.next().unwrap_or_default();
            if client.write_all(&[byte]).is_err() {
                break started.elapsed();
            }
            match client.read(&mut [0u8; 64]) {
                Ok(0) | Err(_) if started.elapsed() > LIMIT + Duration::from_secs(5) => {
                    anyhow::bail!("The slow client was never cut off")
                }
                Ok(0) => break started.elapsed(),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break started.elapsed(),
                Ok(_) => {}
            }
            thread::sleep(Duration::from_millis(50));
        };
        ensure!(closed >= LIMIT, "Closed before the deadline: {closed:?}");
        ensure!(
            closed < LIMIT + Duration::from_secs(2),
            "Closed late: {closed:?}"
        );
        released(&remote)?;
        remote.shut_down()
    }

    #[test]
    fn a_client_that_never_answers_the_password_challenge_is_cut_off() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (remote, port) = listening(&temp)?;
        remote.command(super::super::Command::Password {
            verifier: Some(password::Verifier::with_iterations(
                "correct horse battery",
                100_000,
            )?),
        })?;
        let (mut socket, _) = crate::cloud::client::open("127.0.0.1", port, None)?;
        let started = Instant::now();
        socket.send(Message::text(
            json!({
                "type": "password",
                "device": {"id": "slow-client-1", "name": "Slow"},
                "nonce": "abcdefghijklmnopqrstuvwxyz012345",
            })
            .to_string(),
        ))?;
        let challenge = socket.read()?;
        ensure!(
            challenge.to_text()?.contains("challenge"),
            "Expected a challenge, got {challenge:?}"
        );
        // No proof follows. The server hangs up at the deadline, long before
        // its per-read timeout.
        loop {
            match socket.read() {
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(other) => anyhow::bail!("Unexpected message {other:?}"),
                Err(_) => break,
            }
        }
        let closed = started.elapsed();
        ensure!(
            closed < LIMIT + Duration::from_secs(2),
            "Closed late: {closed:?}"
        );
        released(&remote)?;
        // Nothing was paired.
        assert!(remote.status()?.devices.is_empty());
        remote.shut_down()
    }

    #[test]
    fn only_unauthenticated_connections_expire() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let peers = Peers::with_deadline(Duration::from_secs(60));
        let mut clients = Vec::new();
        let mut serials = Vec::new();
        for _ in 0..2 {
            let client = TcpStream::connect(address)?;
            client.set_read_timeout(Some(Duration::from_secs(5)))?;
            let (server, _) = listener.accept()?;
            serials.push(peers.add(&server)?);
            clients.push((client, server));
        }
        let [waiting, signed_in] = serials.as_slice() else {
            anyhow::bail!("Expected two connections");
        };
        peers.authenticated(*signed_in);
        peers.expire(Instant::now());
        ensure!(!peers.expired(*waiting), "Expired before its deadline");
        peers.expire(Instant::now() + Duration::from_secs(61));
        ensure!(peers.expired(*waiting) && !peers.expired(*signed_in));
        let [(first, _), (second, _)] = clients.as_mut_slice() else {
            anyhow::bail!("Expected two clients");
        };
        assert_eq!(
            first.read(&mut [0u8; 8])?,
            0,
            "The waiting client is closed"
        );
        second.set_read_timeout(Some(Duration::from_millis(100)))?;
        ensure!(
            second.read(&mut [0u8; 8]).is_err(),
            "The authenticated client stays open"
        );
        Ok(())
    }

    #[test]
    fn repeated_failures_lock_an_address_out_for_a_while() {
        let guard = Guard::default();
        let start = Instant::now();
        let phone = IpAddr::from([192, 168, 1, 20]);
        let other = IpAddr::from([192, 168, 1, 21]);
        for _ in 0..MAX_FAILURES - 1 {
            guard.failed(phone, false, start);
        }
        assert!(!guard.locked(phone, start));
        guard.failed(phone, true, start);
        assert!(guard.locked(phone, start));
        assert!(!guard.locked(other, start));
        assert!(guard.locked(phone, start + LOCKOUT - Duration::from_secs(1)));
        assert!(!guard.locked(phone, start + LOCKOUT));
        // Failures spread wider than the window never add up to a lockout.
        let later = start + LOCKOUT;
        for step in 0..MAX_FAILURES as u64 {
            guard.failed(other, false, later + Duration::from_secs(step * 180));
        }
        assert!(!guard.locked(other, later + Duration::from_secs(13 * 60)));
    }

    #[test]
    fn password_failures_are_capped_globally() {
        let guard = Guard::default();
        let start = Instant::now();
        for index in 0..MAX_PASSWORD_FAILURES {
            assert!(!guard.passwords_paused(start));
            guard.failed(IpAddr::from([10, 0, 0, index as u8]), true, start);
        }
        assert!(guard.passwords_paused(start));
        assert!(!guard.passwords_paused(start + PASSWORD_WINDOW));
    }

    #[test]
    fn unauthenticated_connections_are_capped_per_address() {
        let guard = Guard::default();
        let v6 = |last: u16| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 3, 4, 5, last));
        for index in 0..MAX_PENDING as u16 {
            assert!(guard.admit(v6(index)));
        }
        // Another address in the same /64.
        assert!(!guard.admit(v6(999)));
        assert!(guard.admit(IpAddr::from([192, 168, 1, 2])));
        guard.release(v6(0));
        assert!(guard.admit(v6(1000)));
        let mapped = IpAddr::V6(Ipv6Addr::from([0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0102]));
        assert_eq!(group(mapped), IpAddr::from([192, 168, 1, 2]));
    }
}
