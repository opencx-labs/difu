//! Personal PR metadata stays fresh even when no terminal UI is connected.
use crate::{github, model::PrState, process::Cancel, storage::Storage};
use std::{sync::mpsc, thread, time::Duration};

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
                    if relevant { 0 } else { 1 }
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refresh {
    Background,
    Visible,
    Open,
}

pub(crate) fn should_poll(session: &super::Summary, _links: &[SessionLink]) -> bool {
    !session.archived
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Snapshot {
    pub links: Vec<SessionLink>,
    pub checked_at: u64,
    attempted_at: u64,
    revision: u64,
    history_at: u64,
    pub error: Option<String>,
}
fn snapshot_path(storage: &Storage, id: &str) -> std::path::PathBuf {
    storage
        .cache
        .join("session-pr-refresh")
        .join(format!("{}.json", crate::storage::hash(id)))
}
pub(crate) fn snapshot(storage: &Storage, id: &str) -> Snapshot {
    std::fs::read(snapshot_path(storage, id))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The UI and service share both the result and the refresh lease. A failed
/// attempt preserves known links and is explicitly marked stale for every UI.
pub(crate) fn refresh_session(
    storage: &Storage,
    session: &super::Summary,
    links: Vec<SessionLink>,
    refresh: Refresh,
    cancel: &Cancel,
) -> anyhow::Result<Vec<SessionLink>> {
    if session.status == super::Status::Starting && !session.worktree {
        return Ok(links);
    }
    let path = snapshot_path(storage, &session.id);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("Missing PR cache directory"))?,
    )?;
    let _lease = github::polling::lock(&path.with_extension("lock"), cancel)?;
    let mut saved = snapshot(storage, &session.id);
    let incoming = links
        .into_iter()
        .filter(|link| {
            saved
                .links
                .iter()
                .find(|old| old.pr.key == link.pr.key)
                .is_none_or(|old| link.pr.updated > old.pr.updated)
        })
        .collect();
    merge_links(&mut saved.links, incoming);
    let at = github::polling::now();
    let interval = if refresh != Refresh::Open { 300 } else { 30 };
    let same_workspace = saved.revision == session.workspace_revision;
    if same_workspace
        && at.saturating_sub(saved.attempted_at) < interval
        && (refresh != Refresh::Open || at.saturating_sub(saved.history_at) < 30)
    {
        if let Some(error) = &saved.error {
            anyhow::bail!("{error}");
        }
        return Ok(saved.links);
    }
    if refresh != Refresh::Open && !should_poll(session, &saved.links) {
        if let Some(error) = &saved.error {
            anyhow::bail!("{error}");
        }
        return Ok(saved.links);
    }
    let history = refresh == Refresh::Open;
    let result = update_session(session, &mut saved.links, history, cancel);
    cancel.check()?;
    saved.attempted_at = github::polling::now();
    saved.revision = session.workspace_revision;
    if result.is_ok() {
        saved.checked_at = saved.attempted_at;
        if history {
            saved.history_at = saved.checked_at;
        }
    }
    saved.error = result.as_ref().err().map(|error| format!("{error:#}"));
    sort_links(
        &mut saved.links,
        &session.workspace,
        session.branch.as_deref(),
    );
    crate::storage::atomic_json(&path, &saved)?;
    result?;
    Ok(saved.links)
}

fn update_session(
    session: &super::Summary,
    links: &mut Vec<SessionLink>,
    history: bool,
    cancel: &Cancel,
) -> anyhow::Result<()> {
    // Closed/merged history is checked only on opening or manual refresh.
    for link in links
        .iter_mut()
        .filter(|link| history || link.pr.state == "OPEN")
    {
        cancel.check()?;
        link.pr = github::session_pr(&link.workspace, None, Some(&link.pr.key), cancel)?;
    }
    let mut workspaces = if history {
        session.workspaces.clone()
    } else {
        Vec::new()
    };
    workspaces.retain(|w| w.path != session.workspace || w.branch != session.branch);
    workspaces.push(super::workspace::Workspace {
        path: session.workspace.clone(),
        branch: session.branch.clone(),
        base: None,
    });
    for workspace in workspaces {
        cancel.check()?;
        let Some(branch) = workspace
            .branch
            .as_deref()
            .filter(|b| !b.is_empty() && *b != "HEAD")
        else {
            continue;
        };
        // Historical worktrees may have been removed; their PR URLs still work.
        if !workspace.path.is_dir() {
            continue;
        }
        let prs = if history {
            github::session_prs(&workspace.path, branch, cancel)?
        } else {
            github::open_session_prs(&workspace.path, branch, cancel)?
        };
        merge_links(
            links,
            prs.into_iter()
                .map(|pr| SessionLink {
                    workspace: workspace.path.clone(),
                    pr,
                })
                .collect(),
        );
    }
    Ok(())
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
    for session in sessions
        .iter()
        .filter(|s| s.kind == "Coding" && !s.archived)
    {
        let known = links.remove(&session.id).unwrap_or_default();
        let refreshed =
            refresh_session(storage, session, known.clone(), Refresh::Background, cancel);
        links.insert(
            session.id.clone(),
            refreshed.as_ref().cloned().unwrap_or(known),
        );
        // Persist progress before returning an error (including a shared cooldown).
        std::fs::create_dir_all(&storage.cache)?;
        crate::storage::atomic_json(&path, &links)?;
        if let Err(error) = refreshed {
            cancel.check()?;
            if github::polling::paused() {
                return Err(error);
            }
            eprintln!("Cannot refresh session PRs: {error:#}");
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
        let interval = Duration::from_secs(300);
        let personal_storage = storage.clone();
        let personal = Worker::start_with(storage.clone(), interval, move |cancel| {
            let mut prs = github::my_prs(PrState::Open, cancel)?;
            // Retain historical entries without repeatedly searching closed PRs.
            prs.extend(personal_storage.load_inbox(PERSONAL)?.unwrap_or_default());
            prs.sort_by(|a, b| b.updated.cmp(&a.updated));
            let mut seen = std::collections::HashSet::new();
            prs.retain(|pr| seen.insert(pr.key.id()));
            let mut tracked = prs
                .iter()
                .filter(|pr| {
                    pr.metadata
                        .as_ref()
                        .is_none_or(|m| m.state == "OPEN" || m.checked_at == 0)
                })
                .cloned()
                .collect::<Vec<_>>();
            github::enrich_prs(&mut tracked, cancel)?;
            for fresh in tracked {
                if let Some(pr) = prs.iter_mut().find(|pr| pr.key == fresh.key) {
                    *pr = fresh;
                }
            }
            Ok(prs)
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
                    let result = refresh(&token);
                    if let Err(error) = result {
                        if token.cancelled() {
                            break;
                        }
                        // A failed refresh preserves the last successful snapshot.
                        eprintln!("Cannot refresh personal PR cache: {error:#}");
                    }
                    if receiver.recv_timeout(interval).is_ok() || token.cancelled() {
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
    fn shared_snapshot_preserves_fresh_conflict_status_and_reports_failures() -> anyhow::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut session = super::super::Session::new(
            "shared".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: dir.path().into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = super::super::Status::Idle;
        let summary = session.summary();
        let link = SessionLink {
            workspace: summary.workspace.clone(),
            pr: github::SessionPr {
                summary: None,
                key: PrKey {
                    owner: "example".into(),
                    repo: "project".into(),
                    number: 1,
                },
                state: "OPEN".into(),
                draft: false,
                conflicts: false,
                head_branch: String::new(),
                head: String::new(),
                updated: "same timestamp".into(),
            },
        };
        let path = snapshot_path(&storage, &summary.id);
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("cache parent"))?,
        )?;
        let mut saved = Snapshot {
            links: vec![link.clone()],
            checked_at: github::polling::now(),
            attempted_at: github::polling::now(),
            revision: summary.workspace_revision,
            history_at: github::polling::now(),
            error: None,
        };
        crate::storage::atomic_json(&path, &saved)?;
        let mut stale = link;
        stale.pr.conflicts = true;
        for refresh in [Refresh::Visible, Refresh::Background, Refresh::Open] {
            let links = refresh_session(
                &storage,
                &summary,
                vec![stale.clone()],
                refresh,
                &Cancel::default(),
            )?;
            assert_eq!(links.first().map(|link| link.pr.conflicts), Some(false));
        }
        saved.attempted_at = github::polling::now().saturating_sub(60);
        crate::storage::atomic_json(&path, &saved)?;
        // Automatic visibility refresh must still use the shared five-minute lease.
        assert!(
            refresh_session(
                &storage,
                &summary,
                Vec::new(),
                Refresh::Visible,
                &Cancel::default()
            )
            .is_ok()
        );
        let mut closed = saved.links.clone();
        for link in &mut closed {
            link.pr.state = "MERGED".into();
        }
        assert!(should_poll(&summary, &closed));
        saved.error = Some("GitHub rate limit; refresh paused".into());
        crate::storage::atomic_json(&path, &saved)?;
        assert!(
            refresh_session(
                &storage,
                &summary,
                Vec::new(),
                Refresh::Visible,
                &Cancel::default()
            )
            .is_err()
        );
        assert_eq!(snapshot(&storage, &summary.id).links.len(), 1);
        Ok(())
    }

    #[test]
    fn archived_sessions_do_not_request_github_in_the_background() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut session = super::super::Session::new(
            "archived".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: dir.path().into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = super::super::Status::Idle;
        session.archived = true;
        let link = SessionLink {
            workspace: dir.path().into(),
            pr: github::SessionPr {
                summary: None,
                key: PrKey {
                    owner: "example".into(),
                    repo: "project".into(),
                    number: 1,
                },
                state: "OPEN".into(),
                draft: false,
                conflicts: false,
                head_branch: String::new(),
                head: String::new(),
                updated: String::new(),
            },
        };
        let links = refresh_session(
            &storage,
            &session.summary(),
            vec![link.clone()],
            Refresh::Background,
            &Cancel::default(),
        )?;
        assert_eq!(links, vec![link]);
        Ok(())
    }

    #[test]
    fn migrates_single_pr_cache_and_orders_retained_history() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("links.json");
        let make = |number, state: &str, workspace: &str, updated: &str| SessionLink {
            workspace: workspace.into(),
            pr: github::SessionPr {
                summary: None,
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
                metadata: None,
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
