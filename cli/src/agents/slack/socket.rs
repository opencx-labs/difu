use super::api::Api;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

pub(super) struct Socket(WebSocket<MaybeTlsStream<TcpStream>>);

impl Socket {
    pub fn connect(api: &Api) -> Result<Self> {
        let url = api.socket_url()?;
        let host = url.host_str().context("Slack connection host is missing")?;
        let timeout = Duration::from_secs(10);
        let addresses = (host, 443)
            .to_socket_addrs()
            .map_err(|_| anyhow::anyhow!("Cannot resolve Slack connection host"))?;
        let stream = addresses
            .filter_map(|address| TcpStream::connect_timeout(&address, timeout).ok())
            .next()
            .context("Cannot connect to Slack")?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let (mut socket, _) = tungstenite::client_tls(url.as_str(), stream)
            .map_err(|_| anyhow::anyhow!("Slack connection handshake failed"))?;
        let stream = match socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream,
            MaybeTlsStream::Rustls(tls) => &mut tls.sock,
            _ => anyhow::bail!("Unsupported Slack connection transport"),
        };
        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
        Ok(Self(socket))
    }

    pub fn read(&mut self) -> Result<Option<Value>> {
        match self.0.read() {
            Ok(Message::Text(text)) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| anyhow::anyhow!("Slack sent an invalid event")),
            Ok(Message::Close(_)) => anyhow::bail!("Slack connection closed"),
            Ok(_) => {
                // Flush automatic Pong responses even when no application data is sent.
                self.0
                    .flush()
                    .map_err(|_| anyhow::anyhow!("Slack connection closed"))?;
                Ok(None)
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(_) => anyhow::bail!("Slack connection failed"),
        }
    }

    /// Call only after accepting an event durably, or deciding to ignore it.
    /// Agent execution must run separately so a long turn never delays the ACK.
    pub fn acknowledge(&mut self, envelope_id: &str) -> Result<()> {
        self.0
            .send(Message::Text(
                json!({"envelope_id": envelope_id}).to_string().into(),
            ))
            .map_err(|_| anyhow::anyhow!("Cannot acknowledge Slack event"))
    }
}
