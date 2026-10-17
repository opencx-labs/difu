//! TLS WebSocket listener. Each phone connection is one thread that alternates
//! between short socket reads and draining replies/events queued by workers.
use super::{Remote, identity::Identity};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeSet,
    io,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, protocol::WebSocketConfig};

const MAX_PEERS: usize = 32;

pub struct Server {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    /// Bind the saved port, or a new random high port when it is unset or taken.
    pub fn bind(
        remote: Arc<Remote>,
        identity: Arc<Identity>,
        preferred: u16,
    ) -> Result<(Self, u16)> {
        let mut candidates = Vec::new();
        if preferred != 0 {
            candidates.push(preferred);
        }
        for _ in 0..12 {
            let mut bytes = [0u8; 2];
            rustls::crypto::ring::default_provider()
                .secure_random
                .fill(&mut bytes)
                .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
            candidates.push(49152 + u16::from_le_bytes(bytes) % 16000);
        }
        // Tests bind loopback only, which never triggers a firewall prompt.
        let bind = std::env::var("DIFU_REMOTE_BIND").ok();
        let mut last = None;
        let listener = candidates.into_iter().find_map(|port| {
            let bound = match &bind {
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
                return Err(last
                    .map(anyhow::Error::from)
                    .unwrap_or_else(|| anyhow::anyhow!("No port available"))
                    .context("Cannot listen for the iOS app on the local network"));
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
        match listener.accept() {
            // Unauthenticated sockets hold a thread until their handshake times out.
            Ok(_) if remote.peers.count() >= MAX_PEERS => {}
            Ok((stream, address)) => {
                let remote = remote.clone();
                let identity = identity.clone();
                let _ = thread::Builder::new()
                    .name("difu-remote-peer".into())
                    .spawn(move || {
                        if let Err(error) = serve(&remote, &identity, stream) {
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

/// Live connections, so disabling remote control or forgetting a phone also
/// disconnects it immediately.
#[derive(Default)]
pub struct Peers {
    next: AtomicU64,
    peers: Mutex<Vec<Peer>>,
}
struct Peer {
    serial: u64,
    device: Option<String>,
    socket: TcpStream,
}
impl Peers {
    fn add(&self, socket: &TcpStream) -> Result<u64> {
        let serial = self.next.fetch_add(1, Ordering::Relaxed);
        let socket = socket.try_clone()?;
        if let Ok(mut peers) = self.peers.lock() {
            peers.push(Peer {
                serial,
                device: None,
                socket,
            });
        }
        Ok(serial)
    }
    fn identify(&self, serial: u64, device: &str) {
        if let Ok(mut peers) = self.peers.lock()
            && let Some(peer) = peers.iter_mut().find(|p| p.serial == serial)
        {
            peer.device = Some(device.to_owned());
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Hello {
    Auth { token: String },
    Pair { code: String, device: DeviceHello },
}
#[derive(Deserialize)]
struct DeviceHello {
    id: String,
    name: String,
}

type Socket = tungstenite::WebSocket<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>;

fn serve(remote: &Arc<Remote>, identity: &Identity, tcp: TcpStream) -> Result<()> {
    tcp.set_nonblocking(false)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(20)))?;
    let serial = remote.peers.add(&tcp)?;
    let result = (|| {
        let tls = rustls::ServerConnection::new(identity.tls.clone())?;
        let stream = rustls::StreamOwned::new(tls, tcp);
        let config = WebSocketConfig::default()
            .max_message_size(Some(48 << 20))
            .max_frame_size(Some(16 << 20));
        let mut socket = tungstenite::accept_with_config(stream, Some(config))
            .map_err(|error| anyhow::anyhow!("WebSocket handshake failed: {error}"))?;
        let Some(device) = authenticate(remote, identity, &mut socket)? else {
            return Ok(());
        };
        remote.peers.identify(serial, &device);
        run(remote, &device, &mut socket)
    })();
    remote.peers.remove(serial);
    result
}

fn authenticate(
    remote: &Remote,
    identity: &Identity,
    socket: &mut Socket,
) -> Result<Option<String>> {
    let text = match socket
        .read()
        .context("Waiting for the phone to identify itself")?
    {
        Message::Text(text) => text,
        _ => anyhow::bail!("Expected an authentication message"),
    };
    let outcome = match serde_json::from_str::<Hello>(&text) {
        Ok(Hello::Auth { token }) => remote.authenticate(&token).map(|id| (id, None)),
        Ok(Hello::Pair { code, device }) => remote
            .pair(&code, &device.id, &device.name)
            .map(|(id, token)| (id, Some(token))),
        Err(error) => Err(anyhow::anyhow!("Invalid authentication message: {error}")),
    };
    match outcome {
        Ok((device, token)) => {
            let welcome = json!({
                "type": "welcome",
                "protocol": super::PROTOCOL,
                "device": device,
                "token": token,
                "server": {
                    "name": super::network::name(),
                    "version": env!("CARGO_PKG_VERSION"),
                    "hosts": super::network::hosts(),
                    "port": socket.get_ref().sock.local_addr()?.port(),
                    "fingerprint": identity.fingerprint,
                },
            });
            socket.send(Message::text(welcome.to_string()))?;
            Ok(Some(device))
        }
        Err(error) => {
            // Slow down guessing; tokens and codes are too long to brute-force anyway.
            thread::sleep(Duration::from_secs(1));
            let denied = json!({"type": "denied", "message": format!("{error:#}")});
            let _ = socket.send(Message::text(denied.to_string()));
            let _ = socket.close(None);
            let _ = socket.flush();
            Ok(None)
        }
    }
}

fn run(remote: &Arc<Remote>, device: &str, socket: &mut Socket) -> Result<()> {
    socket
        .get_ref()
        .sock
        .set_read_timeout(Some(Duration::from_millis(25)))?;
    let (sender, outgoing) = mpsc::sync_channel::<String>(512);
    let port = socket.get_ref().sock.local_addr()?.port();
    let connection = super::api::Connection::new(remote.storage.clone(), port, sender);
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
