use crate::{model::*, process::Cancel};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::process::Command;

pub(crate) mod polling;
pub(crate) mod store;

pub(crate) const PR_REFRESH_SECONDS: u64 = 120;

pub(crate) fn view_detail(key: &PrKey, cancel: &Cancel) -> Result<PrDetail> {
    store::refresh(key, cancel, |pr| pr.detail, || detail(key, cancel))
}
pub(crate) fn view_timeline(key: &PrKey, cancel: &Cancel) -> Result<Vec<TimelineItem>> {
    store::refresh(key, cancel, |pr| pr.timeline, || timeline(key, cancel))
}
pub(crate) fn view_checks(key: &PrKey, cancel: &Cancel) -> Result<CheckReport> {
    store::refresh(key, cancel, |pr| pr.checks, || checks(key, cancel))
}

pub(crate) fn command() -> Command {
    let mut cmd = Command::new("gh");
    cmd.env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("NO_COLOR", "1");
    cmd
}

pub(crate) fn json(args: &[&str], cancel: &Cancel) -> Result<Value> {
    read_json(args, cancel, 0)
}

fn cached_json(args: &[&str], cancel: &Cancel) -> Result<Value> {
    read_json(args, cancel, 30)
}

fn read_json(args: &[&str], cancel: &Cancel, ttl: u64) -> Result<Value> {
    let output = polling::run(command().args(args), None, cancel, ttl, false, None)?;
    if output.code != 0 {
        bail!("GitHub: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    serde_json::from_slice(&output.stdout).context("GitHub returned invalid JSON")
}
pub(crate) fn write(
    command: &mut Command,
    input: Option<Vec<u8>>,
    cancel: &Cancel,
) -> Result<crate::process::Output> {
    polling::run(command, input, cancel, 0, true, None)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .unwrap_or(&Value::Null)
        .as_str()
        .unwrap_or_default()
        .into()
}

/// The personal inbox is the union of authored PRs and review requests. Keep
/// the freshest copy if a PR changes between the two GitHub search responses.
pub fn my_prs(state: PrState, cancel: &Cancel) -> Result<Vec<PrSummary>> {
    let mut prs = search_prs("--author=@me", state, cancel)?;
    cancel.check()?;
    prs.extend(search_prs("--review-requested=@me", state, cancel)?);
    let mut prs = ordered_unique(prs);
    enrich_prs(&mut prs, cancel)?;
    Ok(prs)
}

fn ordered_unique(mut prs: Vec<PrSummary>) -> Vec<PrSummary> {
    prs.sort_by(|a, b| {
        b.updated
            .cmp(&a.updated)
            .then_with(|| a.key.id().cmp(&b.key.id()))
    });
    let mut seen = std::collections::HashSet::new();
    prs.retain(|pr| seen.insert(pr.key.id()));
    prs
}

pub fn inbox(
    tab: InboxTab,
    state: PrState,
    repositories: &[String],
    cancel: &Cancel,
) -> Result<Vec<PrSummary>> {
    if tab == InboxTab::Diffs {
        return Ok(Vec::new());
    }
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
        let mut prs = ordered_unique(combined);
        enrich_prs(&mut prs, cancel)?;
        return Ok(prs);
    }
    my_prs(state, cancel)
}

fn search_prs(scope: &str, state: PrState, cancel: &Cancel) -> Result<Vec<PrSummary>> {
    let mut args = vec![
        "search",
        "prs",
        scope,
        "--sort=updated",
        "--order=desc",
        "--limit=1000",
        "--json=number,title,url,author,updatedAt,createdAt,isDraft,state",
    ];
    match state {
        PrState::Open => args.push("--state=open"),
        PrState::Merged => args.push("--merged"),
        PrState::Closed => args.extend(["--state=closed", "--merged=false"]),
        PrState::All => {}
    }
    let value = cached_json(&args, cancel)?;
    value
        .as_array()
        .context("Invalid inbox response")?
        .iter()
        .map(pr_summary_value)
        .collect()
}

fn pr_summary_value(v: &Value) -> Result<PrSummary> {
    Ok(PrSummary {
        key: PrKey::from_url(&text(v, "url"))?,
        title: text(v, "title"),
        author: text(v.get("author").unwrap_or(&Value::Null), "login"),
        updated: text(v, "updatedAt"),
        created: text(v, "createdAt"),
        stats: None,
        stats_error: false,
        metadata: Some(pr_metadata(v)),
        draft: v
            .get("isDraft")
            .unwrap_or(&Value::Null)
            .as_bool()
            .unwrap_or(false),
    })
}

pub(crate) fn pr_summary(key: &PrKey, cancel: &Cancel) -> Result<PrSummary> {
    key.validate()?;
    let summary = pr_summary_value(&json(
        &[
            "pr",
            "view",
            &key.url(),
            "--json=number,title,url,author,updatedAt,createdAt,isDraft,state,mergeable,reviewRequests,latestReviews",
        ],
        cancel,
    )?)?;
    store::summary(&crate::storage::Storage::discover()?, &summary)?;
    Ok(summary)
}

fn pr_metadata(value: &Value) -> PrMetadata {
    let mut reviewers = Vec::new();
    for field in ["reviewRequests", "latestReviews"] {
        let entries = value.get(field).and_then(|v| {
            v.as_array()
                .or_else(|| v.get("nodes").and_then(Value::as_array))
        });
        for entry in entries.into_iter().flatten() {
            let reviewer = entry
                .get("requestedReviewer")
                .or_else(|| entry.get("author"))
                .unwrap_or(entry);
            if let Some(name) = reviewer
                .get("login")
                .or_else(|| reviewer.get("slug"))
                .and_then(Value::as_str)
            {
                reviewers.push(name.to_owned());
            }
        }
    }
    reviewers.sort();
    reviewers.dedup();
    PrMetadata {
        state: text(value, "state").to_ascii_uppercase(),
        conflicts: text(value, "mergeable") == "CONFLICTING",
        reviewers,
        checked_at: if value.get("mergeable").is_some() {
            polling::now()
        } else {
            0
        },
    }
}

/// Fetch reviewer and lifecycle metadata in bounded batches, never from typing.
/// Counts share this request so the inbox need not request them separately.
pub(crate) fn enrich_prs(prs: &mut [PrSummary], cancel: &Cancel) -> Result<()> {
    let storage = crate::storage::Storage::discover()?;
    for pr in prs.iter_mut() {
        store::summary(&storage, pr)?;
        if let Some(summary) = store::load(&storage, &pr.key).and_then(|p| p.summary) {
            *pr = summary;
        }
    }
    let mut missing = prs
        .iter()
        .enumerate()
        .filter(|(_, pr)| {
            pr.metadata
                .as_ref()
                .is_none_or(|m| polling::now().saturating_sub(m.checked_at) >= PR_REFRESH_SECONDS)
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    missing.sort_by_key(|i| prs.get(*i).map(|pr| pr.key.id()));
    for batch in missing.chunks(25) {
        cancel.check()?;
        let mut query = String::from("query {");
        for (alias, index) in batch.iter().enumerate() {
            let pr = prs.get(*index).context("Missing tracked PR")?;
            pr.key.validate()?;
            query.push_str(&format!(
                "r{alias}: repository(owner:{}, name:{}) {{ pullRequest(number:{}) {{ state mergeable title author {{ login }} updatedAt isDraft additions deletions changedFiles reviewRequests(first:100) {{ nodes {{ requestedReviewer {{ ... on User {{ login }} ... on Team {{ slug }} }} }} }} latestReviews(first:100) {{ nodes {{ author {{ login }} }} }} }} }}",
                serde_json::to_string(&pr.key.owner)?, serde_json::to_string(&pr.key.repo)?, pr.key.number
            ));
        }
        query.push('}');
        let response = read_json(
            &["api", "graphql", "-f", &format!("query={query}")],
            cancel,
            30,
        )?;
        if response
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            bail!("GitHub could not refresh tracked PR metadata");
        }
        for (alias, index) in batch.iter().enumerate() {
            let Some(value) = response
                .pointer(&format!("/data/r{alias}/pullRequest"))
                .filter(|v| !v.is_null())
            else {
                continue;
            };
            let pr = prs.get_mut(*index).context("Missing tracked PR")?;
            pr.metadata = Some(pr_metadata(value));
            if let Some(title) = value.get("title").and_then(Value::as_str) {
                pr.title = title.into();
            }
            if let Some(author) = value.pointer("/author/login").and_then(Value::as_str) {
                pr.author = author.into();
            }
            if let Some(updated) = value.get("updatedAt").and_then(Value::as_str) {
                pr.updated = updated.into();
            }
            if let Some(draft) = value.get("isDraft").and_then(Value::as_bool) {
                pr.draft = draft;
            }
            if let Ok(stats) = serde_json::from_value(value.clone()) {
                pr.stats = Some(stats);
            }
            store::summary(&storage, pr)?;
        }
    }
    Ok(())
}

/// Resolve a bounded batch without downloading patches or Git objects.
pub fn stats(keys: &[PrKey], cancel: &Cancel) -> Result<Vec<Option<PrStats>>> {
    anyhow::ensure!(keys.len() <= 25, "Too many PRs in a metadata batch");
    let mut query = String::from("query {");
    for (index, key) in keys.iter().enumerate() {
        key.validate()?;
        query.push_str(&format!(
            "r{index}: repository(owner:{}, name:{}) {{ pullRequest(number:{}) {{ additions deletions changedFiles }} }}",
            serde_json::to_string(&key.owner)?, serde_json::to_string(&key.repo)?, key.number
        ));
    }
    query.push('}');
    let value = cached_json(&["api", "graphql", "-f", &format!("query={query}")], cancel)?;
    Ok(keys
        .iter()
        .enumerate()
        .map(|(index, _)| {
            value
                .pointer(&format!("/data/r{index}/pullRequest"))
                .and_then(|v| serde_json::from_value(v.clone()).ok())
        })
        .collect())
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

/// Let gh resolve the current branch, including its configured fork/upstream.
pub fn current_branch_pr(cancel: &Cancel) -> Result<PrKey> {
    let value = json(&["pr", "view", "--json=url"], cancel)
        .context("Could not find a pull request for the current branch")?;
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .context("GitHub did not return a pull request URL for the current branch")?;
    PrKey::from_url(url)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionPr {
    #[serde(default)]
    pub summary: Option<PrSummary>,
    pub key: PrKey,
    pub state: String,
    pub draft: bool,
    pub conflicts: bool,
    #[serde(default)]
    pub head_branch: String,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub updated: String,
}
impl SessionPr {
    pub fn label(&self) -> &'static str {
        match self.state.as_str() {
            "MERGED" => "Merged",
            "CLOSED" => "Closed",
            "OPEN" if self.conflicts => "Has conflicts",
            "OPEN" if self.draft => "Draft",
            "OPEN" => "Open",
            _ => "Unknown",
        }
    }
}

/// Discover using gh's branch/upstream resolution, then follow the remembered PR
/// by URL even after its branch or worktree has been removed.
pub fn session_pr(
    workspace: &std::path::Path,
    branch: Option<&str>,
    known: Option<&PrKey>,
    cancel: &Cancel,
) -> Result<SessionPr> {
    let mut cmd = command();
    cmd.args(["pr", "view"]);
    if let Some(key) = known {
        key.validate()?;
        cmd.arg(key.url());
    } else {
        cmd.current_dir(workspace);
        if let Some(branch) = branch.filter(|b| !b.is_empty() && *b != "HEAD") {
            anyhow::ensure!(!branch.starts_with('-'), "Invalid Git branch");
            cmd.arg(branch);
        }
    }
    cmd.arg("--json=url,state,isDraft,mergeable,headRefName,headRefOid,updatedAt,title,author,reviewRequests,latestReviews");
    let output = polling::run(&mut cmd, None, cancel, 30, false, None)?;
    anyhow::ensure!(
        output.code == 0,
        "GitHub: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let pr = session_pr_value(&value)?;
    store::session(&crate::storage::Storage::discover()?, &pr)?;
    Ok(pr)
}

fn session_pr_value(value: &Value) -> Result<SessionPr> {
    let state = text(value, "state");
    anyhow::ensure!(
        matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED"),
        "Invalid PR state"
    );
    Ok(SessionPr {
        summary: Some(pr_summary_value(value)?),
        key: PrKey::from_url(&text(value, "url"))?,
        state,
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .context("Missing PR draft status")?,
        conflicts: text(value, "mergeable") == "CONFLICTING",
        head_branch: text(value, "headRefName"),
        head: text(value, "headRefOid"),
        updated: text(value, "updatedAt"),
    })
}

/// The published PR patch, independent of local worktree state.
pub fn pr_diff(key: &PrKey, cancel: &Cancel) -> Result<String> {
    store::refresh(key, cancel, |pr| pr.diff, || fetch_pr_diff(key, cancel))
}
fn fetch_pr_diff(key: &PrKey, cancel: &Cancel) -> Result<String> {
    key.validate()?;
    let output = polling::run(
        command().args(["pr", "diff", &key.url(), "--color=never"]),
        None,
        cancel,
        0,
        false,
        Some(32 * 1024 * 1024),
    )?;
    let patch = String::from_utf8(output.stdout).context("PR diff is not UTF-8")?;
    store::diff(&crate::storage::Storage::discover()?, key, &patch)?;
    Ok(patch)
}

/// Discover new PRs independently of previously remembered PR URLs.
pub fn session_prs(
    workspace: &std::path::Path,
    branch: &str,
    cancel: &Cancel,
) -> Result<Vec<SessionPr>> {
    branch_prs(workspace, branch, "all", cancel)
}

pub(crate) fn open_session_prs(
    workspace: &std::path::Path,
    branch: &str,
    cancel: &Cancel,
) -> Result<Vec<SessionPr>> {
    branch_prs(workspace, branch, "open", cancel)
}

fn branch_prs(
    workspace: &std::path::Path,
    branch: &str,
    state: &str,
    cancel: &Cancel,
) -> Result<Vec<SessionPr>> {
    anyhow::ensure!(
        !branch.is_empty() && branch != "HEAD" && !branch.starts_with('-'),
        "No named workspace branch"
    );
    let output = polling::run(
        command().current_dir(workspace).args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            state,
            "--limit",
            "1000",
            "--json=url,state,isDraft,mergeable,headRefName,headRefOid,updatedAt,title,author,reviewRequests,latestReviews",
        ]),
        None,
        cancel,
        30,
        false,
        None,
    )?;
    anyhow::ensure!(
        output.code == 0,
        "GitHub: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let prs = value
        .as_array()
        .context("Invalid session PR response")?
        .iter()
        .map(session_pr_value)
        .collect::<Result<Vec<_>>>()?;
    let storage = crate::storage::Storage::discover()?;
    for pr in &prs {
        store::session(&storage, pr)?;
    }
    Ok(prs)
}

pub fn current_repository(cancel: &Cancel) -> Result<String> {
    let v = json(&["repo", "view", "--json=nameWithOwner"], cancel)?;
    v.get("nameWithOwner")
        .unwrap_or(&Value::Null)
        .as_str()
        .map(String::from)
        .context("Run inside a GitHub clone when using only a PR number")
}

/// Poll only revision identities; download full metadata only when they change.
pub fn updated_revision(pr: &PrDetail, cancel: &Cancel) -> Result<Option<PrDetail>> {
    pr.key.validate()?;
    let query = format!(
        "query {{ repository(owner:{},name:{}) {{ pullRequest(number:{}) {{ headRefOid baseRefOid }} }} }}",
        serde_json::to_string(&pr.key.owner)?,
        serde_json::to_string(&pr.key.repo)?,
        pr.key.number,
    );
    let value = cached_json(&["api", "graphql", "-f", &format!("query={query}")], cancel)?;
    let revision = value
        .pointer("/data/repository/pullRequest")
        .context("Missing PR revision response")?;
    let head = revision
        .get("headRefOid")
        .and_then(Value::as_str)
        .context("Missing PR head revision")?;
    let base = revision
        .get("baseRefOid")
        .and_then(Value::as_str)
        .context("Missing PR base revision")?;
    if head == pr.head && base == pr.base {
        Ok(None)
    } else {
        detail(&pr.key, cancel).map(Some)
    }
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
    let detail = parse_detail(key, &v)?;
    store::detail(&crate::storage::Storage::discover()?, &detail, &v)?;
    Ok(detail)
}

pub(crate) fn parse_detail(key: &PrKey, v: &Value) -> Result<PrDetail> {
    let names = |field: &str, name: &str| {
        v.get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get(name).and_then(Value::as_str))
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    };
    Ok(PrDetail {
        draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
        requested_reviewers: names("requested_reviewers", "login"),
        requested_teams: names("requested_teams", "slug"),
        key: key.clone(),
        title: text(v, "title"),
        body: text(v, "body"),
        author: text(v.get("user").unwrap_or(&Value::Null), "login"),
        head: text(v.get("head").unwrap_or(&Value::Null), "sha"),
        base: text(v.get("base").unwrap_or(&Value::Null), "sha"),
        head_branch: text(v.get("head").unwrap_or(&Value::Null), "ref"),
        base_branch: text(v.get("base").unwrap_or(&Value::Null), "ref"),
        state: if v.get("merged").unwrap_or(&Value::Null).as_bool() == Some(true) {
            "merged".into()
        } else {
            text(v, "state")
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
    store::timeline(&crate::storage::Storage::discover()?, key, &items)?;
    Ok(items)
}

const CHECK_QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){mergeable mergeStateStatus headRefOid baseRefOid state baseRef{name branchProtectionRule{requiredStatusCheckContexts}} commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100,after:$cursor){nodes{__typename ... on CheckRun{name status conclusion startedAt completedAt detailsUrl checkSuite{app{databaseId}}} ... on StatusContext{context state targetUrl createdAt}} pageInfo{hasNextPage endCursor}}}}}}}}}";

fn required_checks(value: &Value) -> Result<Vec<(String, Option<u64>)>> {
    let mut checks = std::collections::BTreeSet::new();
    for page in value.as_array().context("Invalid branch rules pages")? {
        for rule in page.as_array().context("Invalid branch rules")? {
            if text(rule, "type") == "required_status_checks" {
                for check in rule
                    .pointer("/parameters/required_status_checks")
                    .and_then(Value::as_array)
                    .context("Missing required check contexts")?
                {
                    let name = check
                        .get("context")
                        .and_then(Value::as_str)
                        .context("Missing required check name")?;
                    checks.insert((
                        name.to_owned(),
                        check.get("integration_id").and_then(Value::as_u64),
                    ));
                }
            }
        }
    }
    Ok(checks.into_iter().collect())
}

fn parse_check(v: &Value) -> Result<(Check, Option<u64>)> {
    let (name, status, started, completed, url, app) = match text(v, "__typename").as_str() {
        "CheckRun" => (
            text(v, "name"),
            if text(v, "status") == "COMPLETED" {
                text(v, "conclusion")
            } else {
                "PENDING".into()
            },
            text(v, "startedAt"),
            text(v, "completedAt"),
            text(v, "detailsUrl"),
            v.pointer("/checkSuite/app/databaseId")
                .and_then(Value::as_u64),
        ),
        "StatusContext" => (
            text(v, "context"),
            text(v, "state"),
            text(v, "createdAt"),
            String::new(),
            text(v, "targetUrl"),
            None,
        ),
        _ => bail!("Unknown GitHub check type"),
    };
    let state = match status.as_str() {
        "SUCCESS" => "pass",
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => "fail",
        "CANCELLED" => "cancelled",
        "SKIPPED" | "NEUTRAL" => "skipping",
        _ => "pending",
    };
    Ok((
        Check {
            name,
            state: state.into(),
            started,
            completed,
            url,
        },
        app,
    ))
}

/// Only PR-associated runs awaiting fork approval, pinned to the current head.
pub fn awaiting_workflows(key: &PrKey, head: &str, cancel: &Cancel) -> Result<Vec<WorkflowRun>> {
    key.validate()?;
    anyhow::ensure!(
        !head.is_empty() && head.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid PR head"
    );
    let endpoint = format!(
        "repos/{}/actions/runs?head_sha={head}&event=pull_request&status=action_required&per_page=100",
        key.repository()
    );
    let pages = json(&["api", "--paginate", "--slurp", &endpoint], cancel)?;
    let mut runs = Vec::new();
    let mut source = None;
    for page in pages.as_array().context("Invalid workflow pages")? {
        for run in page
            .get("workflow_runs")
            .and_then(Value::as_array)
            .context("Missing workflow runs")?
        {
            if run.get("head_sha").and_then(Value::as_str) != Some(head)
                || run.get("event").and_then(Value::as_str) != Some("pull_request")
                || !["status", "conclusion"]
                    .iter()
                    .any(|field| run.get(*field).and_then(Value::as_str) == Some("action_required"))
            {
                continue;
            }
            let prs = run
                .get("pull_requests")
                .and_then(Value::as_array)
                .context("Missing workflow PR associations")?;
            if prs.is_empty() {
                // GitHub may omit PR associations for fork runs. Verify the source
                // repository, branch and SHA against the PR before offering approval.
                if source.is_none() {
                    source = Some(json(
                        &[
                            "api",
                            &format!("repos/{}/pulls/{}", key.repository(), key.number),
                        ],
                        cancel,
                    )?);
                }
                let pr = source.as_ref().context("Missing PR source")?;
                let repository = pr
                    .pointer("/head/repo/full_name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let branch = pr
                    .pointer("/head/ref")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                if pr.pointer("/head/sha").and_then(Value::as_str) != Some(head)
                    || repository.is_none()
                    || branch.is_none()
                    || repository
                        != run
                            .pointer("/head_repository/full_name")
                            .and_then(Value::as_str)
                    || branch != run.get("head_branch").and_then(Value::as_str)
                {
                    continue;
                }
            } else if !prs
                .iter()
                .any(|pr| pr.get("number").and_then(Value::as_u64) == Some(key.number))
            {
                continue;
            }
            let id = run
                .get("id")
                .and_then(Value::as_u64)
                .context("Missing workflow run ID")?;
            if !runs.iter().any(|run: &WorkflowRun| run.id == id) {
                runs.push(WorkflowRun {
                    id,
                    name: text(run, "name"),
                    url: text(run, "html_url"),
                });
            }
        }
    }
    Ok(runs)
}

pub fn checks(key: &PrKey, cancel: &Cancel) -> Result<CheckReport> {
    key.validate()?;
    let mut report = CheckReport::default();
    let mut cursor: Option<String> = None;
    let mut actual = Vec::new();
    let mut required = std::collections::BTreeSet::new();
    let mut base_branch = String::new();
    loop {
        let mut args = vec![
            "api".to_owned(),
            "graphql".into(),
            "-f".into(),
            format!("query={CHECK_QUERY}"),
            "-f".into(),
            format!("owner={}", key.owner),
            "-f".into(),
            format!("repo={}", key.repo),
            "-F".into(),
            format!("number={}", key.number),
        ];
        if let Some(cursor) = &cursor {
            args.extend(["-f".into(), format!("cursor={cursor}")]);
        }
        let value = cached_json(&args.iter().map(String::as_str).collect::<Vec<_>>(), cancel)?;
        ensure_no_graphql_errors(&value)?;
        let pr = value
            .pointer("/data/repository/pullRequest")
            .filter(|v| !v.is_null())
            .context("Missing PR check status")?;
        let head = text(pr, "headRefOid");
        let base = text(pr, "baseRefOid");
        anyhow::ensure!(
            report.head.is_empty() || (report.head == head && report.base == base),
            "PR revisions changed while reading checks; refresh again"
        );
        report.head = head;
        report.base = base;
        report.state = text(pr, "state");
        report.mergeable = text(pr, "mergeable");
        report.merge_state = text(pr, "mergeStateStatus");
        if let Some(name) = pr.pointer("/baseRef/name").and_then(Value::as_str) {
            base_branch = name.into();
        }
        if let Some(contexts) = pr
            .pointer("/baseRef/branchProtectionRule/requiredStatusCheckContexts")
            .and_then(Value::as_array)
        {
            for context in contexts {
                if let Some(name) = context.as_str() {
                    required.insert((name.to_owned(), None));
                }
            }
        }
        let contexts = pr.pointer("/commits/nodes/0/commit/statusCheckRollup/contexts");
        if let Some(contexts) = contexts.filter(|v| !v.is_null()) {
            for v in contexts
                .get("nodes")
                .and_then(Value::as_array)
                .context("Missing check contexts")?
            {
                actual.push(parse_check(v)?);
            }
            if contexts
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                == Some(true)
            {
                let next = contexts
                    .pointer("/pageInfo/endCursor")
                    .and_then(Value::as_str)
                    .context("Missing check cursor")?;
                anyhow::ensure!(
                    cursor.as_deref() != Some(next),
                    "Repeated GitHub check cursor"
                );
                cursor = Some(next.into());
                continue;
            }
        }
        break;
    }
    if !base_branch.is_empty() && report.state == "OPEN" {
        let mut url = url::Url::parse("https://api.github.com/repos/")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid rules URL"))?
            .pop_if_empty()
            .extend([
                key.owner.as_str(),
                key.repo.as_str(),
                "rules",
                "branches",
                &base_branch,
            ]);
        match cached_json(
            &[
                "api",
                "--paginate",
                "--slurp",
                url.path().trim_start_matches('/'),
            ],
            cancel,
        )
        .and_then(|v| required_checks(&v))
        {
            Ok(checks) => required.extend(checks),
            Err(error) => {
                report.rules_error = Some(format!("Could not load required checks: {error:#}"))
            }
        }
    }
    for (name, app) in required {
        if !actual
            .iter()
            .any(|(check, id)| check.name == name && (app.is_none() || app == *id))
        {
            report.checks.push(Check {
                name,
                state: "expected".into(),
                ..Check::default()
            });
        }
    }
    report
        .checks
        .dedup_by(|a, b| a.name == b.name && a.state == b.state);
    report
        .checks
        .extend(actual.into_iter().map(|(check, _)| check));
    if report.state == "OPEN" {
        match awaiting_workflows(key, &report.head, cancel) {
            Ok(runs) => report.awaiting_workflows = runs,
            Err(error) => {
                report.workflows_error =
                    Some(format!("Could not load workflow approvals: {error:#}"))
            }
        }
    }
    store::checks(&crate::storage::Storage::discover()?, key, &report)?;
    Ok(report)
}

fn ensure_no_graphql_errors(value: &Value) -> Result<()> {
    anyhow::ensure!(
        value.get("errors").is_none(),
        "GitHub: {}",
        value.get("errors").unwrap_or(&Value::Null)
    );
    Ok(())
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
    fn metadata_indexes_requested_and_completed_reviewers_and_status() {
        let metadata = pr_metadata(&serde_json::json!({
            "state":"OPEN","mergeable":"CONFLICTING",
            "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"faltawy"}},{"requestedReviewer":{"slug":"platform"}}]},
            "latestReviews":{"nodes":[{"author":{"login":"reviewer"}},{"author":{"login":"faltawy"}}]}
        }));
        assert_eq!(metadata.reviewers, vec!["faltawy", "platform", "reviewer"]);
        assert_eq!(metadata.label(), "Has conflicts");
        let metadata = pr_metadata(&serde_json::json!({
            "state":"MERGED","mergeable":"CONFLICTING",
            "reviewRequests":[{"login":"requested"}],
            "latestReviews":[{"author":{"login":"reviewed"}}]
        }));
        assert_eq!(metadata.label(), "Merged");
        assert_eq!(metadata.reviewers, vec!["requested", "reviewed"]);
    }

    #[test]
    fn personal_inbox_deduplicates_across_updates_and_repositories() -> Result<()> {
        let pr = |repo: &str, number: u64, updated: &str, title: &str| PrSummary {
            key: PrKey {
                owner: "example".into(),
                repo: repo.into(),
                number,
            },
            title: title.into(),
            author: "author".into(),
            updated: updated.into(),
            created: "2026-09-01T00:00:00Z".into(),
            stats: None,
            stats_error: false,
            metadata: None,
            draft: false,
        };
        let prs = ordered_unique(vec![
            pr("one", 1, "2026-09-17T00:00:00Z", "Old copy"),
            pr("one", 2, "2026-09-18T00:00:00Z", "Another PR"),
            pr("one", 1, "2026-09-19T00:00:00Z", "Fresh copy"),
            pr("two", 1, "2026-09-18T00:00:00Z", "Other repository"),
            pr("one", 2, "2026-09-18T00:00:00Z", "Duplicate"),
        ]);
        assert_eq!(prs.len(), 3);
        assert_eq!(prs.first().context("Missing first PR")?.title, "Fresh copy");
        assert_eq!(
            prs.iter().map(|p| p.key.id()).collect::<Vec<_>>(),
            ["example/one#1", "example/one#2", "example/two#1"]
        );
        Ok(())
    }

    #[test]
    fn empty_rollups_required_rules_and_failed_checks_are_distinct() -> Result<()> {
        let rules = serde_json::json!([[{"type":"required_status_checks", "parameters":{"required_status_checks":[{"context":"lint"},{"context":"lint"},{"context":"tests","integration_id":15368}]}}]]);
        assert_eq!(
            required_checks(&rules)?,
            vec![("lint".into(), None), ("tests".into(), Some(15368))]
        );
        let (run, app) = parse_check(
            &serde_json::json!({"__typename":"CheckRun","name":"tests","status":"COMPLETED","conclusion":"FAILURE","checkSuite":{"app":{"databaseId":15368}}}),
        )?;
        assert_eq!(run.state, "fail");
        assert_eq!(app, Some(15368));
        let (context, _) = parse_check(
            &serde_json::json!({"__typename":"StatusContext","context":"deploy","state":"PENDING"}),
        )?;
        assert_eq!(context.state, "pending");
        assert!(
            ensure_no_graphql_errors(&serde_json::json!({"errors":[{"message":"denied"}]}))
                .is_err()
        );
        Ok(())
    }

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
