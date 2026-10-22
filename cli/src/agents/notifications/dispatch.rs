use super::{Event, Kind};
use crate::{
    settings::{NotificationDelivery, NotificationScope, Notifications},
    storage::{self, Storage},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

pub const LEASE: Duration = Duration::from_secs(6);
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub desktop: String,
    pub sound: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub epoch: String,
    pub sequence: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub cursor: Option<Cursor>,
    pub events: Vec<Event>,
    pub capabilities: Capabilities,
    pub notice: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    Poll {
        client: String,
        focused: bool,
        viewed: Option<String>,
        cursor: Option<Cursor>,
    },
    Close {
        client: String,
    },
    Status,
    Enable,
    Test {
        preferences: Notifications,
        event: Option<Kind>,
    },
}
pub trait Delivery: Send + 'static {
    fn status(&mut self) -> Result<Capabilities>;
    fn authorize(&mut self) -> Result<Capabilities>;
    fn desktop(&mut self, event: &Event) -> Result<()>;
    fn sound(&mut self) -> Result<()>;
}
struct Presence {
    focused: bool,
    viewed: Option<String>,
    updated: Instant,
}
struct State {
    epoch: String,
    sequence: u64,
    feed: VecDeque<(u64, Event)>,
    clients: BTreeMap<String, Presence>,
    capabilities: Capabilities,
    last_error: Option<String>,
}
impl State {
    fn new() -> Self {
        Self {
            epoch: format!(
                "{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            sequence: 0,
            feed: VecDeque::new(),
            clients: BTreeMap::new(),
            capabilities: Capabilities {
                desktop: "Not checked".into(),
                sound: "Not checked".into(),
                error: None,
            },
            last_error: None,
        }
    }
    fn snapshot(&self, cursor: Option<&Cursor>) -> Snapshot {
        let mut capabilities = self.capabilities.clone();
        capabilities.error = capabilities.error.or_else(|| self.last_error.clone());
        Snapshot {
            cursor: Some(Cursor {
                epoch: self.epoch.clone(),
                sequence: self.sequence,
            }),
            events: cursor
                .filter(|c| c.epoch == self.epoch)
                .map(|c| {
                    self.feed
                        .iter()
                        .filter(|(n, _)| *n > c.sequence)
                        .map(|(_, e)| e.clone())
                        .collect()
                })
                .unwrap_or_default(),
            capabilities,
            notice: None,
        }
    }
}
enum Work {
    Event(Event),
    Action(Command, mpsc::Sender<Snapshot>),
}
pub struct Dispatcher {
    sender: mpsc::Sender<Work>,
    state: Arc<Mutex<State>>,
}
impl Dispatcher {
    pub fn start(storage: Storage, delivery: impl Delivery) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(State::new()));
        let shared = state.clone();
        thread::Builder::new()
            .name("difu-notifications".into())
            .spawn(move || {
                let mut worker = Worker {
                    storage,
                    delivery,
                    state: shared,
                    markers: BTreeSet::new(),
                    persistence_error: None,
                };
                worker.run(receiver);
            })?;
        Ok(Self { sender, state })
    }
    pub(crate) fn emit(&self, events: Vec<Event>) {
        for event in events {
            let _ = self.sender.send(Work::Event(event));
        }
    }
    pub fn request(&self, command: Command) -> Result<Snapshot> {
        match command {
            Command::Poll {
                client,
                focused,
                viewed,
                cursor,
            } => {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Notification state unavailable"))?;
                let now = Instant::now();
                state
                    .clients
                    .retain(|_, p| now.saturating_duration_since(p.updated) < LEASE);
                state.clients.insert(
                    client,
                    Presence {
                        focused,
                        viewed,
                        updated: now,
                    },
                );
                Ok(state.snapshot(cursor.as_ref()))
            }
            Command::Close { client } => {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Notification state unavailable"))?;
                state.clients.remove(&client);
                Ok(state.snapshot(None))
            }
            action => {
                let (sender, receiver) = mpsc::channel();
                self.sender.send(Work::Action(action, sender))?;
                receiver
                    .recv_timeout(Duration::from_secs(35))
                    .context("Notification action timed out")
            }
        }
    }
}
struct Worker<D: Delivery> {
    storage: Storage,
    delivery: D,
    state: Arc<Mutex<State>>,
    markers: BTreeSet<String>,
    persistence_error: Option<String>,
}
impl<D: Delivery> Worker<D> {
    fn run(&mut self, receiver: mpsc::Receiver<Work>) {
        let path = self.marker_path();
        let loaded = (|| -> Result<()> {
            if path.exists() {
                self.markers = serde_json::from_slice(&std::fs::read(&path)?)?;
            }
            Ok(())
        })();
        let available = loaded.is_ok();
        if let Err(error) = loaded {
            let message =
                format!("Cannot restore notification deduplication; delivery disabled: {error:#}");
            self.persistence_error = Some(message.clone());
            self.failure(anyhow::anyhow!(message));
        }
        for work in receiver {
            match work {
                Work::Event(event) => {
                    if available && let Err(error) = self.event(event) {
                        self.failure(error);
                    }
                }
                Work::Action(command, reply) => {
                    let notice = match self.action(command) {
                        Ok(notice) => Some(notice),
                        Err(error) => {
                            self.failure(error);
                            None
                        }
                    };
                    if let Ok(state) = self.state.lock() {
                        let mut snapshot = state.snapshot(None);
                        snapshot.notice = notice;
                        if let Some(error) = &self.persistence_error {
                            snapshot.capabilities.error = Some(error.clone());
                        }
                        let _ = reply.send(snapshot);
                    }
                }
            }
        }
    }
    fn failure(&self, error: anyhow::Error) {
        if let Ok(mut state) = self.state.lock() {
            state.last_error = Some(format!("{error:#}"));
        }
    }
    fn marker_path(&self) -> std::path::PathBuf {
        self.storage
            .config
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("agents/notifications/dispatched.json")
    }
    fn event(&mut self, event: Event) -> Result<()> {
        if !self.markers.insert(event.identity.clone()) {
            return Ok(());
        }
        let path = self.marker_path();
        std::fs::create_dir_all(path.parent().context("Missing notification directory")?)?;
        // Mark before external IO: no duplicate or historical retries after a crash.
        storage::atomic_json(&path, &self.markers)?;
        let preferences = self.storage.load_config()?.preferences.notifications;
        let enabled = match event.kind {
            Kind::Completion => preferences.completion,
            Kind::Question => preferences.question_approval,
            Kind::Error => preferences.error,
        };
        if !enabled {
            return Ok(());
        }
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Notification state unavailable"))?;
            let now = Instant::now();
            state
                .clients
                .retain(|_, p| now.saturating_duration_since(p.updated) < LEASE);
            let suppressed = state.clients.values().any(|p| {
                p.focused
                    && match preferences.scope {
                        NotificationScope::All => false,
                        NotificationScope::AppUnfocused => true,
                        NotificationScope::BackgroundSessions => {
                            p.viewed.as_deref() == Some(&event.session)
                        }
                    }
            });
            if suppressed {
                return Ok(());
            }
            if preferences.quiet || preferences.delivery != NotificationDelivery::Desktop {
                state.sequence = state.sequence.saturating_add(1);
                let sequence = state.sequence;
                state.feed.push_back((sequence, event.clone()));
                while state.feed.len() > 256 {
                    state.feed.pop_front();
                }
            }
        }
        self.deliver(&event, &preferences)
    }
    fn deliver(&mut self, event: &Event, preferences: &Notifications) -> Result<()> {
        if preferences.quiet {
            return Ok(());
        }
        let native = if preferences.delivery != NotificationDelivery::InApp {
            self.delivery.desktop(event)
        } else {
            Ok(())
        };
        // Standalone audio is independent: never send an unwanted toast for sound.
        let audio = if preferences.sound {
            self.delivery.sound()
        } else {
            Ok(())
        };
        let errors = [native.err(), audio.err()]
            .into_iter()
            .flatten()
            .map(|e| format!("{e:#}"))
            .collect::<Vec<_>>();
        anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
        if (preferences.delivery != NotificationDelivery::InApp || preferences.sound)
            && let Ok(mut state) = self.state.lock()
        {
            state.last_error = None;
        }
        Ok(())
    }
    fn action(&mut self, command: Command) -> Result<String> {
        if let Command::Test { preferences, event } = command {
            if preferences.quiet {
                return Ok("Quiet is on: native and sound preview are silent; in-app sample remains visible".into());
            }
            if let Some(kind) = event
                && !match kind {
                    Kind::Completion => preferences.completion,
                    Kind::Question => preferences.question_approval,
                    Kind::Error => preferences.error,
                }
            {
                return Ok(format!(
                    "{} is disabled in this draft; test is silent",
                    kind.label()
                ));
            }
            if preferences.delivery != NotificationDelivery::InApp {
                let capabilities = self.delivery.authorize()?;
                self.state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Notification state unavailable"))?
                    .capabilities = capabilities;
            }
            let sample = Event {
                identity: "preview".into(),
                session: "preview".into(),
                title: "Example".into(),
                kind: event.unwrap_or(Kind::Completion),
            };
            self.deliver(&sample, &preferences)?;
            return Ok(if event.is_none() && !preferences.sound {
                "Sound is off in this draft; preview is silent".into()
            } else {
                format!(
                    "{} sample tested · {}{}",
                    sample.kind.label(),
                    preferences.delivery.key(),
                    if preferences.sound {
                        " · sound on"
                    } else {
                        " · sound off"
                    }
                )
            });
        }
        let capabilities = if matches!(command, Command::Enable) {
            self.delivery.authorize()?
        } else {
            self.delivery.status()?
        };
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("Notification state unavailable"))?
            .capabilities = capabilities;
        Ok("Notification status refreshed".into())
    }
}
#[cfg(test)]
mod tests;
