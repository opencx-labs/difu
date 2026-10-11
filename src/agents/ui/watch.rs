//! A bounded, replaceable subscription for the visible session.
use super::*;
use std::{
    io::{BufReader, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};

pub(super) struct Watch {
    pub id: String,
    latest: Arc<Mutex<Option<ResultMessage>>>,
    socket: Arc<Mutex<Option<UnixStream>>>,
    cancel: crate::process::Cancel,
}
impl Watch {
    pub fn new(storage: Storage, id: String, mut version: Option<u64>) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let socket = Arc::new(Mutex::new(None));
        let cancel = crate::process::Cancel::default();
        let watch = Self {
            id: id.clone(),
            latest: latest.clone(),
            socket: socket.clone(),
            cancel: cancel.clone(),
        };
        thread::spawn(move || {
            while !cancel.cancelled() {
                let result = (|| -> anyhow::Result<()> {
                    let mut stream = UnixStream::connect(super::super::server::socket(&storage)?)?;
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    {
                        let mut socket = socket.lock().unwrap_or_else(|e| e.into_inner());
                        if cancel.cancelled() {
                            return Ok(());
                        }
                        *socket = Some(stream.try_clone()?);
                    }
                    serde_json::to_writer(
                        &mut stream,
                        &Request::Watch {
                            id: id.clone(),
                            version,
                        },
                    )?;
                    stream.write_all(b"\n")?;
                    let mut reader = BufReader::new(stream);
                    while !cancel.cancelled() {
                        let reply: Reply = serde_json::from_str(&client::read_line(&mut reader)?)?;
                        match reply {
                            Reply::Session(session) => {
                                version = Some(session.version);
                                *latest.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(ResultMessage {
                                        kind: Task::Read(id.clone()),
                                        result: Ok(Reply::Session(session)),
                                    });
                                crate::repaint::request();
                            }
                            Reply::Unchanged => {}
                            Reply::Error(error) => anyhow::bail!("{error}"),
                            _ => anyhow::bail!("Unexpected session subscription reply"),
                        }
                    }
                    Ok(())
                })();
                socket.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Err(error) = result
                    && !cancel.cancelled()
                {
                    // Keep an unconsumed snapshot; the UI must see the version we
                    // will resume from. A later retry can report a connection error.
                    latest
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert_with(|| ResultMessage {
                            kind: Task::Read(id.clone()),
                            result: Err(format!("Session updates disconnected: {error:#}")),
                        });
                    crate::repaint::request();
                }
                cancel.wake().wait_timeout(Duration::from_secs(1));
            }
        });
        watch
    }
    pub fn take(&self) -> Option<ResultMessage> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(stream) = self.socket.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

impl Ui {
    pub(super) fn tick_watch(&mut self, visible: bool) {
        let selected = self
            .selected
            .as_ref()
            .filter(|id| visible && !self.deleting.contains(*id));
        if self.watch.as_ref().map(|watch| &watch.id) != selected {
            self.watch = selected.map(|id| {
                Watch::new(
                    self.storage.clone(),
                    id.clone(),
                    self.sessions.get(id).map(|s| s.version),
                )
            });
        }
        if let Some(message) = self.watch.as_ref().and_then(Watch::take) {
            let _ = self.sender.send(message);
        }
    }
}
