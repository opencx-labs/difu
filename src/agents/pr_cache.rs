//! Personal PR metadata stays fresh even when no terminal UI is connected.
use crate::{github, model::PrState, process::Cancel, storage::Storage};
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(crate) const PERSONAL: &str = "palette-personal";
pub(crate) const LOOKUPS: &str = "palette-lookups";
pub(crate) const SESSION_LINKS: &str = "palette-session-prs.json";
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionLink {
    pub workspace: std::path::PathBuf,
    pub pr: github::SessionPr,
}

fn refresh_links(
    storage: &Storage,
    store: &super::server::Store,
    cancel: &Cancel,
) -> anyhow::Result<()> {
    let path = storage.cache.join(SESSION_LINKS);
    let mut links: std::collections::HashMap<String, SessionLink> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let sessions = store.list()?;
    links.retain(|id, link| {
        sessions
            .iter()
            .any(|s| &s.id == id && s.workspace == link.workspace)
    });
    for session in sessions
        .iter()
        .filter(|s| s.kind == "Coding" && s.branch.is_some())
    {
        cancel.check()?;
        let known = links.get(&session.id).map(|link| &link.pr.key);
        if let Ok(pr) =
            github::session_pr(&session.workspace, session.branch.as_deref(), known, cancel)
        {
            links.insert(
                session.id.clone(),
                SessionLink {
                    workspace: session.workspace.clone(),
                    pr,
                },
            );
        }
    }
    cancel.check()?;
    std::fs::create_dir_all(&storage.cache)?;
    crate::storage::atomic_json(&path, &links)
}

pub(super) struct Refresher {
    _personal: Worker,
    _links: Worker,
}
impl Refresher {
    pub fn start(
        storage: Storage,
        store: std::sync::Arc<super::server::Store>,
    ) -> anyhow::Result<Self> {
        let interval = Duration::from_secs(30);
        let personal = Worker::start_with(storage.clone(), interval, |cancel| {
            github::my_prs(PrState::All, cancel)
        })?;
        let links = Worker::spawn(interval, move |cancel| {
            refresh_links(&storage, &store, cancel)
        })?;
        Ok(Self {
            _personal: personal,
            _links: links,
        })
    }
}
struct Worker {
    cancel: Cancel,
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Worker {
    fn start_with(
        storage: Storage,
        interval: Duration,
        fetch: impl Fn(&Cancel) -> anyhow::Result<Vec<crate::model::PrSummary>> + Send + 'static,
    ) -> anyhow::Result<Self> {
        Self::spawn(interval, move |cancel| {
            let prs = fetch(cancel)?;
            cancel.check()?;
            storage.save_inbox(PERSONAL, &prs)
        })
    }
    fn spawn(
        interval: Duration,
        refresh: impl Fn(&Cancel) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let cancel = Cancel::default();
        let token = cancel.clone();
        let (stop, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("difu-pr-cache".into())
            .spawn(move || {
                while !token.cancelled() {
                    let started = Instant::now();
                    let result = refresh(&token);
                    if let Err(error) = result {
                        if token.cancelled() {
                            break;
                        }
                        // A failed refresh preserves the last successful snapshot.
                        eprintln!("Cannot refresh personal PR cache: {error:#}");
                    }
                    let remaining = interval.saturating_sub(started.elapsed());
                    if receiver.recv_timeout(remaining).is_ok() || token.cancelled() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            cancel,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PrKey, PrSummary};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn refreshes_without_a_ui_and_preserves_cache_after_failure() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = Worker::start_with(storage.clone(), Duration::from_millis(10), move |_| {
            let index = count.fetch_add(1, Ordering::SeqCst);
            let _ = sender.send(index);
            if index > 0 {
                anyhow::bail!("Offline");
            }
            Ok(vec![PrSummary {
                key: PrKey {
                    owner: "example".into(),
                    repo: "project".into(),
                    number: 42,
                },
                title: "Cached review request".into(),
                author: "author".into(),
                updated: String::new(),
                created: String::new(),
                stats: None,
                stats_error: false,
                draft: false,
            }])
        })?;
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2))?, 0);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2))?, 1);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2))?, 2);
        drop(worker);
        let prs = storage.load_inbox(PERSONAL)?.unwrap_or_default();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs.first().map(|pr| pr.key.number), Some(42));
        Ok(())
    }
}
