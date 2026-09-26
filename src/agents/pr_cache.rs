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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionLink {
    pub workspace: std::path::PathBuf,
    pub pr: github::SessionPr,
}

pub(crate) type SessionLinks = std::collections::HashMap<String, Vec<SessionLink>>;

/// Accept the single-PR cache written by earlier versions without losing history.
pub(crate) fn load_links(path: &std::path::Path) -> SessionLinks {
    let Ok(bytes) = std::fs::read(path) else {
        return Default::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        serde_json::from_slice::<std::collections::HashMap<String, SessionLink>>(&bytes)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, link)| (id, vec![link]))
            .collect()
    })
}

pub(crate) fn merge_links(target: &mut Vec<SessionLink>, incoming: Vec<SessionLink>) {
    for link in incoming {
        if let Some(old) = target.iter_mut().find(|old| old.pr.key == link.pr.key) {
            if link.pr.updated >= old.pr.updated {
                *old = link;
            }
        } else {
            target.push(link);
        }
    }
}

pub(crate) fn current(
    link: &SessionLink,
    workspace: &std::path::Path,
    branch: Option<&str>,
) -> bool {
    link.workspace == workspace
        && (link.pr.head_branch.is_empty() || branch == Some(link.pr.head_branch.as_str()))
}

pub(crate) fn sort_links(
    links: &mut [SessionLink],
    workspace: &std::path::Path,
    branch: Option<&str>,
) {
    links.sort_by(|a, b| {
        let rank = |link: &SessionLink| {
            let relevant = current(link, workspace, branch);
            (
                if link.pr.state == "OPEN" {
                    if relevant {
                        0
                    } else {
                        1
                    }
                } else {
                    2
                },
                !relevant,
            )
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| b.pr.updated.cmp(&a.pr.updated))
            .then_with(|| a.pr.key.id().cmp(&b.pr.key.id()))
    });
}

pub(crate) fn refresh_session(
    session: &super::Summary,
    mut links: Vec<SessionLink>,
    cancel: &Cancel,
) -> anyhow::Result<Vec<SessionLink>> {
    // Refresh historical PRs by URL even if their worktree has been deleted.
    for link in &mut links {
        cancel.check()?;
        if let Ok(pr) = github::session_pr(&link.workspace, None, Some(&link.pr.key), cancel) {
            link.pr = pr;
        }
    }
    let mut workspaces = session.workspaces.clone();
    workspaces.retain(|w| w.path != session.workspace || w.branch != session.branch);
    workspaces.push(super::workspace::Workspace {
        path: session.workspace.clone(),
        branch: session.branch.clone(),
        base: None,
    });
    for workspace in workspaces {
        cancel.check()?;
        let Some(branch) = workspace.branch.as_deref() else {
            continue;
        };
        if let Ok(prs) = github::session_prs(&workspace.path, branch, cancel) {
            merge_links(
                &mut links,
                prs.into_iter()
                    .map(|pr| SessionLink {
                        workspace: workspace.path.clone(),
                        pr,
                    })
                    .collect(),
            );
        }
    }
    cancel.check()?;
    sort_links(&mut links, &session.workspace, session.branch.as_deref());
    Ok(links)
}

fn refresh_links(
    storage: &Storage,
    store: &super::server::Store,
    cancel: &Cancel,
) -> anyhow::Result<()> {
    let path = storage.cache.join(SESSION_LINKS);
    let mut links = load_links(&path);
    // Include UI discoveries made before the background cache existed.
    for (id, prs) in load_links(&storage.cache.join("agent-prs.json")) {
        merge_links(links.entry(id).or_default(), prs);
    }
    let sessions = store.list()?;
    links.retain(|id, _| sessions.iter().any(|s| &s.id == id));
    for session in sessions.iter().filter(|s| s.kind == "Coding") {
        let known = links.remove(&session.id).unwrap_or_default();
        links.insert(session.id.clone(), refresh_session(session, known, cancel)?);
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
    fn migrates_single_pr_cache_and_orders_retained_history() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("links.json");
        let make = |number, state: &str, workspace: &str, updated: &str| SessionLink {
            workspace: workspace.into(),
            pr: github::SessionPr {
                key: PrKey {
                    owner: "example".into(),
                    repo: "project".into(),
                    number,
                },
                state: state.into(),
                draft: false,
                conflicts: false,
                head_branch: String::new(),
                head: String::new(),
                updated: updated.into(),
            },
        };
        let old = make(1, "MERGED", "/old", "2026-09-20");
        crate::storage::atomic_json(
            &path,
            &std::collections::HashMap::from([("session", old.clone())]),
        )?;
        let mut cache = load_links(&path);
        let links = cache
            .get_mut("session")
            .ok_or_else(|| anyhow::anyhow!("missing migrated history"))?;
        merge_links(
            links,
            vec![
                make(2, "OPEN", "/old", "2026-09-25"),
                make(3, "OPEN", "/current", "2026-09-24"),
                make(4, "MERGED", "/current", "2026-09-19"),
            ],
        );
        sort_links(links, std::path::Path::new("/current"), None);
        assert_eq!(
            links
                .iter()
                .map(|link| link.pr.key.number)
                .collect::<Vec<_>>(),
            vec![3, 2, 4, 1]
        );
        merge_links(links, vec![make(3, "MERGED", "/current", "2026-09-26")]);
        // An older UI snapshot cannot overwrite the newer merged status.
        merge_links(links, vec![make(3, "OPEN", "/current", "2026-09-24")]);
        sort_links(links, std::path::Path::new("/current"), None);
        assert_eq!(
            links
                .iter()
                .map(|link| link.pr.key.number)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 1]
        );
        assert_eq!(links.len(), 4);
        Ok(())
    }

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
