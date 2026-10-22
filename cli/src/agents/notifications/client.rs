//! Bounded UI-side requests. Presence and permission/test requests never block each other.
use super::{Command, Cursor, Snapshot};
use crate::{
    agents::{Reply, Request},
    storage::Storage,
};
use anyhow::Result;
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
pub(crate) trait Rpc: Send + Sync {
    fn request(&self, command: Command) -> Result<Snapshot>;
}
struct Socket(Storage);
impl Rpc for Socket {
    fn request(&self, command: Command) -> Result<Snapshot> {
        match crate::agents::client::request(&self.0, Request::Notifications(command))? {
            Reply::Notifications(snapshot) => Ok(snapshot),
            _ => anyhow::bail!("Unexpected notification service response"),
        }
    }
}
enum Response {
    Poll(Result<Snapshot>),
    Action(Result<Snapshot>),
}
pub(crate) struct Client {
    rpc: Arc<dyn Rpc>,
    id: String,
    cursor: Option<Cursor>,
    presence: Option<(bool, Option<String>)>,
    renewed: Option<Instant>,
    polling: bool,
    sender: mpsc::Sender<Response>,
    receiver: mpsc::Receiver<Response>,
    actions: Option<mpsc::SyncSender<Command>>,
    polls: Option<mpsc::SyncSender<Command>>,
}
impl Client {
    pub fn new(storage: Storage) -> Self {
        Self::with_rpc(Arc::new(Socket(storage)))
    }
    pub(crate) fn with_rpc(rpc: Arc<dyn Rpc>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let (sender, receiver) = mpsc::channel();
        Self {
            rpc,
            id: format!(
                "{}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            cursor: None,
            presence: None,
            renewed: None,
            polling: false,
            sender,
            receiver,
            actions: None,
            polls: None,
        }
    }
    fn worker(&self, action: bool) -> mpsc::SyncSender<Command> {
        let (sender, receiver) = mpsc::sync_channel(8);
        let rpc = self.rpc.clone();
        let responses = self.sender.clone();
        std::thread::spawn(move || {
            for command in receiver {
                let reply = rpc.request(command);
                let _ = responses.send(if action {
                    Response::Action(reply)
                } else {
                    Response::Poll(reply)
                });
                crate::repaint::request();
            }
        });
        sender
    }
    pub fn action(&mut self, command: Command) -> Result<()> {
        if self.actions.is_none() {
            self.actions = Some(self.worker(true));
        }
        self.actions
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Notification worker unavailable"))?
            .try_send(command)
            .map_err(|_| anyhow::anyhow!("Wait for the current notification action to finish"))?;
        Ok(())
    }
    pub fn tick(&mut self, focused: bool, viewed: Option<String>) -> Vec<(bool, Result<Snapshot>)> {
        let mut replies = Vec::new();
        for reply in self.receiver.try_iter() {
            match reply {
                Response::Poll(reply) => {
                    self.polling = false;
                    self.cursor = reply
                        .as_ref()
                        .ok()
                        .and_then(|snapshot| snapshot.cursor.clone());
                    replies.push((false, reply));
                }
                Response::Action(reply) => {
                    replies.push((true, reply));
                }
            }
        }
        let presence = (focused, viewed);
        if !self.polling
            && (self.presence.as_ref() != Some(&presence)
                || self
                    .renewed
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(2)))
        {
            let command = Command::Poll {
                client: self.id.clone(),
                focused,
                viewed: presence.1.clone(),
                cursor: self.cursor.clone(),
            };
            if self.polls.is_none() {
                self.polls = Some(self.worker(false));
            }
            if self
                .polls
                .as_ref()
                .is_some_and(|sender| sender.try_send(command).is_ok())
            {
                self.polling = true;
                self.renewed = Some(Instant::now());
                self.presence = Some(presence);
            }
        }
        replies
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Some(sender) = &self.polls {
            let _ = sender.try_send(Command::Close {
                client: self.id.clone(),
            });
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    #[test]
    fn reconnect_resets_feed_cursor_instead_of_replaying_missed_activity() -> Result<()> {
        struct Boundary(std::sync::Mutex<Vec<Option<Cursor>>>);
        impl Rpc for Boundary {
            fn request(&self, command: Command) -> Result<Snapshot> {
                if let Command::Poll { cursor, .. } = command {
                    let mut calls = self.0.lock().map_err(|_| anyhow::anyhow!("lock"))?;
                    calls.push(cursor);
                    if calls.len() == 2 {
                        anyhow::bail!("disconnected");
                    }
                    return Ok(Snapshot {
                        cursor: Some(Cursor {
                            epoch: "service".into(),
                            sequence: 10,
                        }),
                        ..Default::default()
                    });
                }
                Ok(Snapshot::default())
            }
        }
        let boundary = Arc::new(Boundary(std::sync::Mutex::new(Vec::new())));
        let mut client = Client::with_rpc(boundary.clone());
        for _ in 0..3 {
            client.renewed = None;
            client.tick(false, None);
            let deadline = Instant::now() + Duration::from_secs(1);
            while client.polling {
                anyhow::ensure!(Instant::now() < deadline, "Poll did not finish");
                std::thread::sleep(Duration::from_millis(2));
                client.tick(false, None);
            }
        }
        let calls = boundary.0.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        assert_eq!(calls.len(), 3);
        assert!(calls.first().is_some_and(Option::is_none));
        assert!(calls.get(1).is_some_and(Option::is_some));
        assert!(calls.get(2).is_some_and(Option::is_none));
        Ok(())
    }
    #[test]
    fn queued_explicit_enable_survives_client_close_without_a_ui_tick() -> Result<()> {
        struct Boundary(mpsc::Sender<()>);
        impl Rpc for Boundary {
            fn request(&self, command: Command) -> Result<Snapshot> {
                match command {
                    Command::Status => std::thread::sleep(Duration::from_millis(30)),
                    Command::Enable => {
                        let _ = self.0.send(());
                    }
                    _ => {}
                }
                Ok(Snapshot::default())
            }
        }
        let (sent, seen) = mpsc::channel();
        let mut client = Client::with_rpc(Arc::new(Boundary(sent)));
        client.action(Command::Status)?;
        client.action(Command::Enable)?;
        drop(client);
        seen.recv_timeout(Duration::from_secs(1))
            .context("Saved native enable was abandoned when the client closed")?;
        Ok(())
    }
}
