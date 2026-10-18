//! Reviews for the phone: the same GitHub reads, cached lists, guides and PR
//! actions the terminal uses, called from the service.
use super::super::{Job, Reply, Request, client, pr_cache};
use crate::{
    diff::{LineKind, Snapshot},
    github,
    model::{InboxTab, PrKey, PrState, PrSummary},
    process::Cancel,
    review::Operation,
    storage::Storage,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub fn key(params: &Value) -> Result<PrKey> {
    let repo = params
        .get("repo")
        .and_then(Value::as_str)
        .context("Missing repo")?;
    let (owner, name) = repo.split_once('/').context("repo must be owner/name")?;
    let key = PrKey {
        owner: owner.into(),
        repo: name.into(),
        number: params
            .get("number")
            .and_then(Value::as_u64)
            .context("Missing PR number")?,
    };
    key.validate()?;
    Ok(key)
}

fn pr(summary: &PrSummary) -> Value {
    let metadata = summary.metadata.clone().unwrap_or_default();
    json!({
        "repo": summary.key.repository(),
        "number": summary.key.number,
        "title": summary.title,
        "author": summary.author,
        "updated": summary.updated,
        "created": summary.created,
        "draft": summary.draft,
        "state": if metadata.state.is_empty() { "OPEN".into() } else { metadata.state },
        "conflicts": metadata.conflicts,
        "reviewers": metadata.reviewers,
        "additions": summary.stats.as_ref().map(|s| s.additions),
        "deletions": summary.stats.as_ref().map(|s| s.deletions),
        "changedFiles": summary.stats.as_ref().map(|s| s.changed_files),
        "url": summary.key.url(),
    })
}

fn open(summary: &PrSummary) -> bool {
    summary
        .metadata
        .as_ref()
        .is_none_or(|m| m.state.is_empty() || m.state == "OPEN")
}

/// Open PRs you authored and PRs waiting for your review. The background
/// service refreshes the cached list every few minutes; `refresh` fetches now.
pub fn inbox(storage: &Storage, refresh: bool) -> Result<Value> {
    let cancel = Cancel::default();
    let mut list = if refresh {
        github::my_prs(PrState::Open, &cancel)?
    } else {
        storage.load_inbox(pr_cache::PERSONAL)?.unwrap_or_default()
    };
    list.retain(open);
    list.sort_by(|a, b| b.updated.cmp(&a.updated));
    let viewer = github::viewer_login(&cancel).ok();
    let (mine, reviewing): (Vec<_>, Vec<_>) = list.iter().partition(|summary| {
        viewer
            .as_deref()
            .is_none_or(|viewer| summary.author.eq_ignore_ascii_case(viewer))
    });
    Ok(json!({
        "viewer": viewer,
        "mine": mine.into_iter().map(pr).collect::<Vec<_>>(),
        "reviewing": reviewing.into_iter().map(pr).collect::<Vec<_>>(),
    }))
}

pub fn repositories(storage: &Storage) -> Result<Value> {
    let names = match storage.load_repositories()? {
        Some(names) => names,
        None => {
            let names = github::repositories(&Cancel::default())?;
            storage.save_repositories(&names)?;
            names
        }
    };
    let pinned = super::api::config(storage).pinned_repositories;
    let mut repositories: Vec<_> = names
        .into_iter()
        .map(|name| (pinned.contains(&name), name))
        .collect();
    repositories.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(json!({
        "repositories": repositories
            .into_iter()
            .map(|(pinned, name)| json!({"name": name, "pinned": pinned}))
            .collect::<Vec<_>>(),
    }))
}

pub fn repository(params: &Value) -> Result<Value> {
    let repo = params
        .get("repo")
        .and_then(Value::as_str)
        .context("Missing repo")?;
    crate::model::validate_repository(repo)?;
    let list = github::inbox(
        InboxTab::Repositories,
        PrState::Open,
        &[repo.to_owned()],
        &Cancel::default(),
    )?;
    Ok(json!({"prs": list.iter().map(pr).collect::<Vec<_>>()}))
}

pub fn overview(key: &PrKey) -> Result<Value> {
    let cancel = Cancel::default();
    let (detail, timeline, checks) = std::thread::scope(|scope| {
        let timeline = scope.spawn(|| github::view_timeline(key, &cancel));
        let checks = scope.spawn(|| github::view_checks(key, &cancel));
        let detail = github::view_detail(key, &cancel);
        (
            detail,
            timeline
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Timeline request stopped"))),
            checks
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Checks request stopped"))),
        )
    });
    let detail = detail?;
    let timeline = timeline.unwrap_or_default();
    let checks = checks.ok();
    // Same reviewer summary as the terminal overview: latest review per person.
    let mut reviewers = std::collections::BTreeMap::new();
    for item in &timeline {
        if let Some(state) = item.kind.strip_prefix("review · ") {
            reviewers.insert(item.author.clone(), state.replace('_', " "));
        }
    }
    for login in &detail.requested_reviewers {
        reviewers.insert(login.clone(), "requested".into());
    }
    for team in &detail.requested_teams {
        reviewers.insert(
            format!("{}/{} (team)", detail.key.owner, team),
            "requested".into(),
        );
    }
    Ok(json!({
        "repo": detail.key.repository(),
        "number": detail.key.number,
        "url": detail.key.url(),
        "title": detail.title,
        "body": detail.body,
        "author": detail.author,
        "state": detail.state,
        "draft": detail.draft,
        "head": detail.head,
        "base": detail.base,
        "headBranch": detail.head_branch,
        "baseBranch": detail.base_branch,
        "additions": detail.additions,
        "deletions": detail.deletions,
        "changedFiles": detail.changed_files,
        "mergeable": checks.as_ref().map(|c| c.mergeable.clone()),
        "mergeState": checks.as_ref().map(|c| c.merge_state.clone()),
        "reviewers": reviewers
            .into_iter()
            .map(|(name, state)| json!({"name": name, "state": state}))
            .collect::<Vec<_>>(),
        "checks": checks
            .as_ref()
            .map(|c| c.checks.iter().map(|check| json!({
                "name": check.name, "state": check.state, "url": check.url,
                "started": check.started, "completed": check.completed,
            })).collect::<Vec<_>>())
            .unwrap_or_default(),
        "timeline": timeline
            .iter()
            .map(|item| json!({
                "date": item.date, "author": item.author, "kind": item.kind,
                "body": item.body, "url": item.url,
            }))
            .collect::<Vec<_>>(),
    }))
}

pub fn diff(key: &PrKey) -> Result<Value> {
    let patch = github::pr_diff(key, &Cancel::default())?;
    Ok(json!({"files": super::api::patch_files(&patch)}))
}

/// The newest guide job for this PR: running, ready, failed, or none.
pub fn guide(storage: &Storage, key: &PrKey) -> Result<Value> {
    let Reply::Sessions(list) = client::request(storage, Request::List)? else {
        anyhow::bail!("Unexpected session list reply");
    };
    let title = format!("Guide · {}", key.id());
    let mut jobs: Vec<_> = list
        .into_iter()
        .filter(|s| s.kind == "Guide" && s.title == title)
        .collect();
    jobs.sort_by(|a, b| {
        b.status
            .active()
            .cmp(&a.status.active())
            .then(b.updated.cmp(&a.updated))
    });
    let Some(job) = jobs.first() else {
        return Ok(json!({"status": "none", "chapters": []}));
    };
    let Reply::Session(session) = client::request(
        storage,
        Request::Read {
            id: job.id.clone(),
            version: None,
        },
    )?
    else {
        anyhow::bail!("Unexpected guide job reply");
    };
    let progress = session
        .entries
        .last()
        .map(|e| e.text.lines().next().unwrap_or_default().to_owned());
    if session.status.active() {
        return Ok(json!({
            "status": "running", "session": session.id,
            "message": progress.unwrap_or_else(|| "Starting".into()), "chapters": [],
        }));
    }
    match (&session.guide, &session.job) {
        (Some(guide), Job::Guide { snapshot, .. }) => {
            let current = github::view_detail(key, &Cancel::default())
                .ok()
                .map(|detail| detail.head);
            Ok(json!({
                "status": "ready",
                "session": session.id,
                "outdated": current.is_some_and(|head| head != snapshot.head),
                "chapters": chapters(guide, snapshot),
            }))
        }
        _ => Ok(json!({
            "status": "failed",
            "session": session.id,
            "message": session.error.clone().or(progress).unwrap_or_else(|| "Guide generation stopped".into()),
            "chapters": [],
        })),
    }
}

/// Generate with the same clone, model and cache as the terminal's `g`.
pub fn generate(storage: &Storage, key: &PrKey) -> Result<Value> {
    let config = super::api::config(storage);
    let root = config
        .repositories
        .get(&key.repository())
        .cloned()
        .context(
            "Open this PR once in difu on your computer so it can find the local clone, then retry",
        )?;
    let cancel = Cancel::default();
    let detail = github::detail(key, &cancel)?;
    let snapshot = crate::repo::snapshot(&root, &detail, &cancel)?;
    let model = config.model;
    if let Some(guide) =
        storage.load_guide(&crate::codex::cache_key(&detail, &snapshot, &model)?)?
        && guide.validate(&snapshot).is_ok()
    {
        return Ok(json!({
            "status": "ready", "outdated": false,
            "chapters": chapters(&guide, &snapshot),
        }));
    }
    let Reply::Launched(id) = client::request(
        storage,
        Request::Launch {
            job: Box::new(Job::Guide {
                root,
                pr: Box::new(detail),
                snapshot: Box::new(snapshot),
                model,
            }),
        },
    )?
    else {
        anyhow::bail!("Missing guide job identifier");
    };
    Ok(json!({"status": "running", "session": id, "message": "Starting", "chapters": []}))
}

fn chapters(guide: &crate::codex::Guide, snapshot: &Snapshot) -> Vec<Value> {
    guide
        .chapters
        .iter()
        .map(|chapter| {
            json!({
                "category": serde_json::to_value(chapter.category).unwrap_or(Value::Null),
                "title": chapter.title,
                "explanation": chapter.explanation,
                "hunks": chapter.hunks.iter().filter_map(|id| snapshot.find(id)).map(|(file, hunk)| json!({
                    "file": file.path,
                    "header": hunk.header,
                    "lines": hunk.lines.iter().map(|line| json!({
                        "kind": match line.kind {
                            LineKind::Context => "context",
                            LineKind::Add => "add",
                            LineKind::Remove => "remove",
                            LineKind::Meta => "meta",
                        },
                        "old": line.old, "new": line.new, "text": line.text,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

pub fn act(method: &str, params: &Value) -> Result<Value> {
    let key = key(params)?;
    let head = params
        .get("head")
        .and_then(Value::as_str)
        .context("Missing head; reload the PR")?;
    let text = |name: &str| {
        params
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let flag = |name: &str| params.get(name).and_then(Value::as_bool).unwrap_or(false);
    let operation = match method {
        "pr.comment" => Operation::PrComment { body: text("body") },
        "pr.review" => {
            let event = text("event");
            anyhow::ensure!(
                matches!(event.as_str(), "APPROVE" | "COMMENT" | "REQUEST_CHANGES"),
                "Review event must be APPROVE, COMMENT or REQUEST_CHANGES"
            );
            Operation::Review {
                event,
                body: text("body"),
            }
        }
        "pr.merge" => Operation::Merge {
            squash: flag("squash"),
            admin: flag("admin"),
        },
        "pr.close" => Operation::Close { body: text("body") },
        "pr.ready" => Operation::Draft {
            draft: flag("draft"),
        },
        _ => anyhow::bail!("Unknown PR action"),
    };
    if let Operation::PrComment { body } = &operation {
        anyhow::ensure!(!body.trim().is_empty(), "Write a comment first");
    }
    let message = crate::review::execute(&key, head, &operation, &Cancel::default())?;
    Ok(json!({"message": message}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_require_owner_repo_and_number() {
        assert!(key(&json!({"repo": "o/r", "number": 4})).is_ok());
        assert!(key(&json!({"repo": "or", "number": 4})).is_err());
        assert!(key(&json!({"repo": "o/r"})).is_err());
    }

    #[test]
    fn writes_need_the_viewed_head() {
        let error = act(
            "pr.comment",
            &json!({"repo": "o/r", "number": 1, "body": "x"}),
        );
        assert!(error.is_err_and(|e| e.to_string().contains("head")));
        let error = act(
            "pr.review",
            &json!({"repo": "o/r", "number": 1, "head": "abc", "event": "MERGE"}),
        );
        assert!(error.is_err());
    }
}
