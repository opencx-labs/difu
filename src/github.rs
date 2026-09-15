use crate::{
    model::*,
    process::{self, Cancel},
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::process::Command;

fn command() -> Command {
    let mut cmd = Command::new("gh");
    cmd.env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("NO_COLOR", "1");
    cmd
}

fn json(args: &[&str], cancel: &Cancel) -> Result<Value> {
    let output = process::run(command().args(args), None, cancel)?;
    if output.code != 0 {
        bail!("GitHub: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    serde_json::from_slice(&output.stdout).context("GitHub returned invalid JSON")
}
fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .unwrap_or(&Value::Null)
        .as_str()
        .unwrap_or_default()
        .into()
}

pub fn inbox(
    tab: InboxTab,
    state: PrState,
    repositories: &[String],
    cancel: &Cancel,
) -> Result<Vec<PrSummary>> {
    // Empty selections must never fall through to a global GitHub search.
    if tab == InboxTab::Repositories {
        let mut combined = Vec::new();
        for repository in repositories {
            validate_repository(repository)?;
            if cancel.cancelled() {
                bail!("Cancelled");
            }
            combined.extend(search_prs(&format!("--repo={repository}"), state, cancel)?);
        }
        combined.sort_by(|a, b| {
            b.updated
                .cmp(&a.updated)
                .then_with(|| a.key.id().cmp(&b.key.id()))
        });
        combined.dedup_by(|a, b| a.key == b.key);
        return Ok(combined);
    }
    let (scope, state) = match tab {
        InboxTab::ReviewRequests => ("--review-requested=@me", PrState::Open),
        _ => ("--author=@me", state),
    };
    search_prs(scope, state, cancel)
}

fn search_prs(scope: &str, state: PrState, cancel: &Cancel) -> Result<Vec<PrSummary>> {
    let mut args = vec![
        "search",
        "prs",
        scope,
        "--sort=updated",
        "--order=desc",
        "--limit=1000",
        "--json=number,title,url,author,updatedAt,isDraft",
    ];
    match state {
        PrState::Open => args.push("--state=open"),
        PrState::Merged => args.push("--merged"),
        PrState::Closed => args.extend(["--state=closed", "--merged=false"]),
        PrState::All => {}
    }
    let value = json(&args, cancel)?;
    value
        .as_array()
        .context("Invalid inbox response")?
        .iter()
        .map(|v| {
            Ok(PrSummary {
                key: PrKey::from_url(&text(v, "url"))?,
                title: text(v, "title"),
                author: text(v.get("author").unwrap_or(&Value::Null), "login"),
                updated: text(v, "updatedAt"),
                draft: v
                    .get("isDraft")
                    .unwrap_or(&Value::Null)
                    .as_bool()
                    .unwrap_or(false),
            })
        })
        .collect()
}

pub fn repositories(cancel: &Cancel) -> Result<Vec<String>> {
    let value = json(
        &[
            "api",
            "--paginate",
            "--slurp",
            "user/repos?per_page=100&sort=full_name&direction=asc&affiliation=owner,collaborator,organization_member",
        ],
        cancel,
    )?;
    let mut repositories = Vec::new();
    for page in value.as_array().context("Invalid repository list")? {
        for item in page.as_array().context("Invalid repository page")? {
            let name = text(item, "full_name");
            validate_repository(&name)?;
            repositories.push(name);
        }
    }
    repositories.sort_by_key(|r| r.to_lowercase());
    repositories.dedup();
    Ok(repositories)
}

pub fn current_repository(cancel: &Cancel) -> Result<String> {
    let v = json(&["repo", "view", "--json=nameWithOwner"], cancel)?;
    v.get("nameWithOwner")
        .unwrap_or(&Value::Null)
        .as_str()
        .map(String::from)
        .context("Run inside a GitHub clone when using only a PR number")
}

pub fn detail(key: &PrKey, cancel: &Cancel) -> Result<PrDetail> {
    key.validate()?;
    let v = json(
        &[
            "api",
            &format!("repos/{}/pulls/{}", key.repository(), key.number),
        ],
        cancel,
    )?;
    Ok(PrDetail {
        key: key.clone(),
        title: text(&v, "title"),
        body: text(&v, "body"),
        author: text(v.get("user").unwrap_or(&Value::Null), "login"),
        head: text(v.get("head").unwrap_or(&Value::Null), "sha"),
        base: text(v.get("base").unwrap_or(&Value::Null), "sha"),
        head_branch: text(v.get("head").unwrap_or(&Value::Null), "ref"),
        base_branch: text(v.get("base").unwrap_or(&Value::Null), "ref"),
        state: if v.get("merged").unwrap_or(&Value::Null).as_bool() == Some(true) {
            "merged".into()
        } else {
            text(&v, "state")
        },
        additions: v
            .get("additions")
            .unwrap_or(&Value::Null)
            .as_u64()
            .unwrap_or(0),
        deletions: v
            .get("deletions")
            .unwrap_or(&Value::Null)
            .as_u64()
            .unwrap_or(0),
        changed_files: v
            .get("changed_files")
            .unwrap_or(&Value::Null)
            .as_u64()
            .unwrap_or(0),
    })
}

pub fn timeline(key: &PrKey, cancel: &Cancel) -> Result<Vec<TimelineItem>> {
    let pages = json(
        &[
            "api",
            "--paginate",
            "--slurp",
            &format!(
                "repos/{}/issues/{}/timeline?per_page=100",
                key.repository(),
                key.number
            ),
        ],
        cancel,
    )?;
    let mut items = Vec::new();
    for page in pages.as_array().context("Invalid timeline pages")? {
        for event in page.as_array().context("Invalid timeline page")? {
            let kind = text(event, "event");
            let (body, author, date) = match kind.as_str() {
                "commented" | "reviewed" => (
                    text(event, "body"),
                    text(event.get("user").unwrap_or(&Value::Null), "login"),
                    event
                        .get("submitted_at")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or(
                            event
                                .get("created_at")
                                .unwrap_or(&Value::Null)
                                .as_str()
                                .unwrap_or_default(),
                        )
                        .into(),
                ),
                "committed" => (
                    format!(
                        "{}  {}",
                        text(event, "sha").chars().take(8).collect::<String>(),
                        text(event, "message")
                    ),
                    text(event.get("author").unwrap_or(&Value::Null), "name"),
                    text(event.get("committer").unwrap_or(&Value::Null), "date"),
                ),
                _ => (
                    match kind.as_str() {
                        "head_ref_force_pushed" => "Updated the PR branch with a force push".into(),
                        "review_requested" => format!(
                            "Requested review from {}",
                            text(
                                event.get("requested_reviewer").unwrap_or(&Value::Null),
                                "login"
                            )
                        ),
                        "labeled" | "unlabeled" => {
                            text(event.get("label").unwrap_or(&Value::Null), "name")
                        }
                        _ => String::new(),
                    },
                    text(event.get("actor").unwrap_or(&Value::Null), "login"),
                    text(event, "created_at"),
                ),
            };
            let kind = if kind == "reviewed" {
                format!("review · {}", text(event, "state").to_lowercase())
            } else {
                kind.replace('_', " ")
            };
            if !kind.is_empty() {
                items.push(TimelineItem {
                    date,
                    author,
                    kind,
                    body,
                    url: event
                        .get("html_url")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or(&key.url())
                        .into(),
                });
            }
        }
    }
    // Inline review comments are a separate GitHub resource, not timeline comments.
    let pages = json(
        &[
            "api",
            "--paginate",
            "--slurp",
            &format!(
                "repos/{}/pulls/{}/comments?per_page=100",
                key.repository(),
                key.number
            ),
        ],
        cancel,
    )?;
    for page in pages.as_array().context("Invalid review-comment pages")? {
        for comment in page.as_array().context("Invalid review-comment page")? {
            items.push(TimelineItem {
                date: text(comment, "created_at"),
                author: text(comment.get("user").unwrap_or(&Value::Null), "login"),
                kind: format!(
                    "comment on {}:{}",
                    text(comment, "path"),
                    comment
                        .get("line")
                        .unwrap_or(&Value::Null)
                        .as_u64()
                        .or(comment
                            .get("original_line")
                            .unwrap_or(&Value::Null)
                            .as_u64())
                        .unwrap_or(0)
                ),
                body: text(comment, "body"),
                url: text(comment, "html_url"),
            });
        }
    }
    items.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(items)
}

pub fn checks(key: &PrKey, cancel: &Cancel) -> Result<Vec<Check>> {
    let output = process::run(
        command().args([
            "pr",
            "checks",
            &key.url(),
            "--json=name,state,bucket,startedAt,completedAt,link",
        ]),
        None,
        cancel,
    )?;
    // gh uses nonzero statuses for valid failed/pending checks.
    let value: Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "Could not load checks: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    value
        .as_array()
        .context("Invalid check response")?
        .iter()
        .map(|v| {
            Ok(Check {
                name: text(v, "name"),
                state: text(v, "bucket"),
                started: text(v, "startedAt"),
                completed: text(v, "completedAt"),
                url: text(v, "link"),
            })
        })
        .collect()
}

pub fn open_url(url: &str, cancel: &Cancel) -> Result<()> {
    let parsed = url::Url::parse(url)?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "https" | "http"),
        "Only web links can be opened"
    );
    let executable = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    cancel.check()?;
    // Browser windows outlive this launcher and belong to the user.
    let mut child = Command::new(executable)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    loop {
        if cancel.cancelled() {
            let _ = child.kill();
            child.wait()?;
            cancel.check()?;
        }
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "Browser launcher failed: {status}");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    Ok(())
}

#[cfg(test)]
mod inbox_tests {
    use super::*;

    #[test]
    fn empty_repository_selection_never_queries_github() -> Result<()> {
        assert!(
            inbox(
                InboxTab::Repositories,
                PrState::All,
                &[],
                &Cancel::default()
            )?
            .is_empty()
        );
        Ok(())
    }
}
