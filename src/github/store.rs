//! Shared, durable GitHub data. Lists and sessions keep references to these PRs.
//! The existing request directory also holds raw responses and the rate-limit gate.
use super::{SessionPr, polling};
use crate::{
    model::*,
    process::Cancel,
    storage::{self, Storage},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Fetched<T> {
    pub at: u64,
    #[serde(default)]
    generation: u64,
    pub value: T,
}
impl<T> Fetched<T> {
    fn new(storage: &Storage, value: T) -> Self {
        Self {
            at: polling::now(),
            generation: polling::generation_in(storage),
            value,
        }
    }
    pub(crate) fn fresh(&self, storage: &Storage) -> bool {
        self.generation == polling::generation_in(storage)
            && polling::now().saturating_sub(self.at) < 30
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PullRequest {
    pub revision: u64,
    pub summary: Option<PrSummary>,
    pub session: Option<SessionPr>,
    pub detail: Option<Fetched<PrDetail>>,
    pub timeline: Option<Fetched<Vec<TimelineItem>>>,
    pub checks: Option<Fetched<CheckReport>>,
    pub diff: Option<Fetched<String>>,
}

pub(crate) fn root(storage: &Storage) -> PathBuf {
    storage.cache.join("github-requests")
}
fn path(storage: &Storage, key: &PrKey) -> PathBuf {
    root(storage)
        .join("prs")
        .join(format!("{}.json", storage::hash(key.id())))
}
pub(crate) fn load(storage: &Storage, key: &PrKey) -> Option<PullRequest> {
    key.validate().ok()?;
    serde_json::from_slice(&fs::read(path(storage, key)).ok()?).ok()
}

/// View reads share a short freshness window and a cross-process refresh lease.
/// Action preconditions use the fresh GitHub readers directly.
pub(crate) fn refresh<T: Clone>(
    key: &PrKey,
    cancel: &Cancel,
    field: impl FnOnce(PullRequest) -> Option<Fetched<T>>,
    fetch: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let storage = Storage::discover()?;
    refresh_in(&storage, key, cancel, field, fetch)
}
fn refresh_in<T: Clone>(
    storage: &Storage,
    key: &PrKey,
    cancel: &Cancel,
    field: impl FnOnce(PullRequest) -> Option<Fetched<T>>,
    fetch: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let path = path(storage, key);
    fs::create_dir_all(path.parent().context("Missing PR store directory")?)?;
    let _lease = polling::lock(&path.with_extension("refresh.lock"), cancel)?;
    if let Some(cached) = load(storage, key).and_then(field)
        && cached.fresh(storage)
    {
        return Ok(cached.value);
    }
    fetch()
}
fn update(storage: &Storage, key: &PrKey, change: impl FnOnce(&mut PullRequest)) -> Result<()> {
    key.validate()?;
    let path = path(storage, key);
    fs::create_dir_all(path.parent().context("Missing PR store directory")?)?;
    let _lease = polling::lock(&path.with_extension("lock"), &Cancel::default())?;
    let mut pr = load(storage, key).unwrap_or_default();
    change(&mut pr);
    pr.revision = pr.revision.saturating_add(1);
    storage::atomic_json(&path, &pr)
}

impl PullRequest {
    fn invalidate_revision(&mut self) {
        if let Some(detail) = &mut self.detail {
            detail.at = 0;
        }
        if let Some(timeline) = &mut self.timeline {
            timeline.at = 0;
        }
        if let Some(checks) = &mut self.checks {
            checks.at = 0;
        }
        if let Some(diff) = &mut self.diff {
            diff.at = 0;
        }
    }
    fn merge_summary(&mut self, mut summary: PrSummary) {
        // A summary can announce a revision before its details and patch arrive.
        if self
            .summary
            .as_ref()
            .is_some_and(|old| old.updated < summary.updated)
        {
            self.invalidate_revision();
        }
        if let Some(old) = &self.summary {
            if old.updated > summary.updated {
                return;
            }
            if summary.created.is_empty() {
                summary.created = old.created.clone();
            }
            if summary.stats.is_none() {
                summary.stats = old.stats.clone();
            }
            if summary.updated == old.updated
                && summary.metadata.as_ref().map_or(0, |m| m.checked_at)
                    < old.metadata.as_ref().map_or(0, |m| m.checked_at)
            {
                summary.metadata = old.metadata.clone();
            }
            // Merging is terminal. Delayed list responses cannot reopen a PR.
            if old.metadata.as_ref().is_some_and(|m| m.state == "MERGED") {
                summary.metadata.get_or_insert_with(Default::default).state = "MERGED".into();
            }
        }
        if let Some(session) = &mut self.session {
            session.draft = summary.draft;
            session.updated = summary.updated.clone();
            if let Some(metadata) = &summary.metadata {
                session.state = metadata.state.clone();
                session.conflicts = metadata.conflicts;
            }
            session.summary = Some(summary.clone());
        }
        if let Some(metadata) = &summary.metadata {
            if let Some(detail) = &mut self.detail {
                detail.value.state = metadata.state.to_ascii_lowercase();
            }
            if let Some(checks) = &mut self.checks {
                checks.value.state = metadata.state.clone();
            }
        }
        self.summary = Some(summary);
    }
    fn state(&mut self, state: &str, conflicts: Option<bool>) {
        if state.is_empty() {
            return;
        }
        let state = if self
            .summary
            .as_ref()
            .and_then(|s| s.metadata.as_ref())
            .is_some_and(|m| m.state == "MERGED")
        {
            "MERGED".to_owned()
        } else {
            state.to_ascii_uppercase()
        };
        if let Some(summary) = &mut self.summary {
            let metadata = summary.metadata.get_or_insert_with(Default::default);
            metadata.state = state.clone();
            if let Some(conflicts) = conflicts {
                metadata.conflicts = conflicts;
            }
            metadata.checked_at = polling::now();
        }
        if let Some(session) = &mut self.session {
            session.state = state.clone();
            if let Some(conflicts) = conflicts {
                session.conflicts = conflicts;
            }
            session.summary = self.summary.clone();
        }
        if let Some(detail) = &mut self.detail {
            detail.value.state = state.to_ascii_lowercase();
        }
        if let Some(checks) = &mut self.checks {
            checks.value.state = state;
        }
    }
    pub(crate) fn session(&self, key: &PrKey) -> Option<SessionPr> {
        self.session.clone().or_else(|| {
            let summary = self.summary.as_ref()?;
            let metadata = summary.metadata.as_ref()?;
            Some(SessionPr {
                key: key.clone(),
                summary: Some(summary.clone()),
                state: metadata.state.clone(),
                draft: summary.draft,
                conflicts: metadata.conflicts,
                head_branch: self
                    .detail
                    .as_ref()
                    .map(|d| d.value.head_branch.clone())
                    .unwrap_or_default(),
                head: self
                    .detail
                    .as_ref()
                    .map(|d| d.value.head.clone())
                    .unwrap_or_default(),
                updated: summary.updated.clone(),
            })
        })
    }
}

pub(crate) fn summary(storage: &Storage, value: &PrSummary) -> Result<()> {
    update(storage, &value.key, |pr| pr.merge_summary(value.clone()))
}
pub(crate) fn session(storage: &Storage, value: &SessionPr) -> Result<()> {
    update(storage, &value.key, |pr| {
        if pr
            .session
            .as_ref()
            .is_some_and(|old| old.updated > value.updated)
        {
            return;
        }
        if !value.head.is_empty()
            && pr
                .session
                .as_ref()
                .is_some_and(|old| old.head != value.head)
        {
            pr.invalidate_revision();
        }
        pr.session = Some(value.clone());
        if let Some(summary) = &value.summary {
            pr.merge_summary(summary.clone());
        }
        // Also support older session records without a summary.
        if let Some(summary) = &pr.summary
            && let Some(session) = &mut pr.session
        {
            session.summary = Some(summary.clone());
            session.draft = summary.draft;
            if let Some(metadata) = &summary.metadata {
                session.state = metadata.state.clone();
                session.conflicts = metadata.conflicts;
            }
        }
    })
}
pub(crate) fn detail(storage: &Storage, value: &PrDetail, raw: &serde_json::Value) -> Result<()> {
    update(storage, &value.key, |pr| {
        if pr
            .detail
            .as_ref()
            .is_some_and(|old| old.value.head != value.head || old.value.base != value.base)
        {
            pr.invalidate_revision();
        }
        let mut summary = PrSummary {
            key: value.key.clone(),
            title: value.title.clone(),
            author: value.author.clone(),
            updated: super::text(raw, "updated_at"),
            created: super::text(raw, "created_at"),
            stats: Some(PrStats {
                additions: value.additions,
                deletions: value.deletions,
                changed_files: value.changed_files,
            }),
            stats_error: false,
            draft: value.draft,
            metadata: Some(PrMetadata {
                state: value.state.to_ascii_uppercase(),
                conflicts: raw.get("mergeable").and_then(serde_json::Value::as_bool) == Some(false),
                reviewers: value
                    .requested_reviewers
                    .iter()
                    .chain(&value.requested_teams)
                    .cloned()
                    .collect(),
                checked_at: polling::now(),
            }),
        };
        if raw.get("mergeable").is_none_or(serde_json::Value::is_null)
            && let Some(metadata) = &mut summary.metadata
        {
            metadata.conflicts = pr
                .summary
                .as_ref()
                .and_then(|s| s.metadata.as_ref())
                .is_some_and(|m| m.conflicts);
        }
        // REST details contain pending requests, but omit completed reviewers.
        if let Some(metadata) = &mut summary.metadata {
            if let Some(previous) = pr.summary.as_ref().and_then(|s| s.metadata.as_ref()) {
                metadata.reviewers.extend(previous.reviewers.iter().cloned());
            }
            metadata.reviewers.sort();
            metadata.reviewers.dedup();
        }
        pr.merge_summary(summary);
        pr.detail = Some(Fetched::new(storage, value.clone()));
        if let Some(session) = &mut pr.session
            && session.updated <= super::text(raw, "updated_at")
        {
            session.head = value.head.clone();
            session.head_branch = value.head_branch.clone();
        }
        let merged = pr
            .summary
            .as_ref()
            .and_then(|s| s.metadata.as_ref())
            .is_some_and(|m| m.state == "MERGED");
        if merged {
            pr.state("MERGED", None);
        }
    })
}
pub(crate) fn timeline(storage: &Storage, key: &PrKey, value: &[TimelineItem]) -> Result<()> {
    update(storage, key, |pr| {
        pr.timeline = Some(Fetched::new(storage, value.to_vec()))
    })
}
pub(crate) fn checks(storage: &Storage, key: &PrKey, value: &CheckReport) -> Result<()> {
    update(storage, key, |pr| {
        pr.checks = Some(Fetched::new(storage, value.clone()));
        pr.state(
            &value.state,
            match value.mergeable.as_str() {
                "CONFLICTING" => Some(true),
                "MERGEABLE" => Some(false),
                _ => None,
            },
        );
    })
}
pub(crate) fn diff(storage: &Storage, key: &PrKey, value: &str) -> Result<()> {
    update(storage, key, |pr| {
        pr.diff = Some(Fetched::new(storage, value.to_owned()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> PrKey {
        PrKey {
            owner: "example".into(),
            repo: "project".into(),
            number: 1,
        }
    }
    fn summary_value() -> PrSummary {
        PrSummary {
            key: key(),
            title: "A change".into(),
            author: "author".into(),
            updated: "2026-09-28T00:00:00Z".into(),
            created: "2026-09-20T00:00:00Z".into(),
            stats: None,
            stats_error: false,
            draft: false,
            metadata: Some(PrMetadata {
                state: "OPEN".into(),
                ..Default::default()
            }),
        }
    }
    #[test]
    fn lists_and_session_badges_resolve_the_same_persisted_pr() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let summary = summary_value();
        storage.save_inbox("reviews", std::slice::from_ref(&summary))?;
        storage.save_inbox("palette-personal", std::slice::from_ref(&summary))?;
        let linked = SessionPr {
            key: key(),
            summary: Some(summary.clone()),
            state: "OPEN".into(),
            draft: false,
            conflicts: false,
            head: "sha".into(),
            head_branch: "feature".into(),
            updated: summary.updated.clone(),
        };
        session(&storage, &linked)?;
        let links = crate::agents::pr_cache::SessionLinks::from([(
            "session".into(),
            vec![crate::agents::pr_cache::SessionLink {
                workspace: dir.path().into(),
                pr: linked,
                workspace_revision: 0,
            }],
        )]);
        let links_path = storage.cache.join("agent-prs.json");
        crate::agents::pr_cache::save_links(&storage, &links_path, &links)?;
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&links_path)?)?;
        assert!(
            saved.pointer("/session/0/pr/state").is_none(),
            "session files contain references only"
        );
        checks(
            &storage,
            &key(),
            &CheckReport {
                state: "MERGED".into(),
                merge_state: "CLEAN".into(),
                head: "sha".into(),
                checks: vec![Check {
                    name: "CI".into(),
                    state: "pass".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )?;
        // A late list save must not undo the merge or erase the full check report.
        storage.save_inbox("reviews", &[summary])?;
        let reopened = Storage {
            config: storage.config.clone(),
            cache: storage.cache.clone(),
        };
        for name in ["reviews", "palette-personal"] {
            assert_eq!(
                reopened
                    .load_inbox(name)?
                    .context("list")?
                    .first()
                    .and_then(|s| s.metadata.as_ref())
                    .map(|m| m.state.as_str()),
                Some("MERGED")
            );
        }
        let links = crate::agents::pr_cache::load_links(&links_path);
        assert_eq!(
            links
                .get("session")
                .and_then(|links| links.first())
                .map(|l| l.pr.label()),
            Some("Merged")
        );
        assert_eq!(
            load(&reopened, &key())
                .context("PR")?
                .checks
                .context("checks")?
                .value
                .checks
                .len(),
            1
        );
        Ok(())
    }
    #[test]
    fn concurrent_component_updates_preserve_details_events_and_patch() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let raw = serde_json::json!({"title":"Title", "body":"Full description", "state":"open",
            "updated_at":"2026-09-28T00:00:00Z", "head":{"sha":"head"}, "base":{"sha":"base"}});
        let detail_value = super::super::parse_detail(&key(), &raw)?;
        detail(&storage, &detail_value, &raw)?;
        let mut workers = Vec::new();
        for component in 0..3 {
            let storage = storage.clone();
            workers.push(std::thread::spawn(move || match component {
                0 => timeline(
                    &storage,
                    &key(),
                    &[TimelineItem {
                        body: "Review comment".into(),
                        ..Default::default()
                    }],
                ),
                1 => checks(
                    &storage,
                    &key(),
                    &CheckReport {
                        state: "OPEN".into(),
                        ..Default::default()
                    },
                ),
                _ => diff(&storage, &key(), "published patch"),
            }));
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("writer failed"))??;
        }
        let saved = load(&storage, &key()).context("PR")?;
        assert_eq!(
            saved.detail.context("details")?.value.body,
            "Full description"
        );
        assert_eq!(
            saved
                .timeline
                .context("events")?
                .value
                .first()
                .map(|e| e.body.as_str()),
            Some("Review comment")
        );
        assert!(saved.checks.is_some());
        assert_eq!(saved.diff.context("patch")?.value, "published patch");
        Ok(())
    }
    #[test]
    fn newer_summaries_invalidate_richer_data_and_migrate_legacy_lists() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut old = summary_value();
        storage::atomic_json(
            &storage.cache.join("inbox-legacy.json"),
            &std::slice::from_ref(&old),
        )?;
        assert_eq!(storage.load_inbox("legacy")?.context("list")?.len(), 1);
        assert!(!storage.cache.join("inbox-legacy.json").exists());
        diff(&storage, &key(), "previous revision")?;
        old.updated = "2026-09-29T00:00:00Z".into();
        summary(&storage, &old)?;
        let cached = load(&storage, &key()).context("PR")?.diff.context("diff")?;
        assert!(!cached.fresh(&storage));
        assert_eq!(
            cached.value, "previous revision",
            "stale data stays available until refresh succeeds"
        );
        Ok(())
    }
    #[test]
    fn opening_reuses_fresh_data_and_retries_stale_data_without_losing_it() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let cancel = Cancel::default();
        diff(&storage, &key(), "cached patch")?;
        let reuse = refresh_in(
            &storage,
            &key(),
            &cancel,
            |p| p.diff,
            || anyhow::bail!("must reuse fresh data"),
        )?;
        assert_eq!(reuse, "cached patch");
        update(&storage, &key(), |pr| {
            if let Some(diff) = &mut pr.diff {
                diff.at = 0;
            }
        })?;
        assert!(refresh_in(
            &storage,
            &key(),
            &cancel,
            |p| p.diff,
            || anyhow::bail!("offline")
        )
        .is_err());
        assert_eq!(
            load(&storage, &key())
                .context("PR")?
                .diff
                .context("diff")?
                .value,
            "cached patch"
        );
        let refreshed = refresh_in(
            &storage,
            &key(),
            &cancel,
            |p| p.diff,
            || {
                diff(&storage, &key(), "fresh patch")?;
                Ok("fresh patch".to_owned())
            },
        )?;
        assert_eq!(refreshed, "fresh patch");
        storage::atomic_json(
            &root(&storage).join("cooldown.json"),
            &serde_json::json!({"generation":1,"until":0,"failures":0}),
        )?;
        assert!(
            !load(&storage, &key())
                .context("PR")?
                .diff
                .context("diff")?
                .fresh(&storage),
            "mutations invalidate view data too"
        );
        Ok(())
    }
}
