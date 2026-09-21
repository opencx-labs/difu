//! GitHub review operations. Mutations are explicit, revision checked, and never retried.
use crate::{
    github,
    model::PrKey,
    process::{self, Cancel},
    storage::{self, Storage},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Left,
    #[default]
    Right,
}
impl Side {
    pub fn api(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub side: Side,
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Debug)]
pub enum Operation {
    Review {
        event: String,
        body: String,
    },
    Comment {
        anchor: Anchor,
        body: String,
        pending: bool,
    },
    Merge {
        squash: bool,
        admin: bool,
    },
    Close {
        body: String,
    },
    Viewed {
        path: String,
        viewed: bool,
    },
}
impl Operation {
    pub fn label(&self) -> String {
        match self {
            Self::Review { event, .. } => format!("Submit review: {event}"),
            Self::Comment { pending, .. } => if *pending {
                "Add to pending review"
            } else {
                "Publish line comment"
            }
            .into(),
            Self::Merge { squash, admin } => format!(
                "{}{}",
                if *squash { "Squash merge" } else { "Merge" },
                if *admin { " with admin override" } else { "" }
            ),
            Self::Close { .. } => "Close PR".into(),
            Self::Viewed { viewed, .. } => if *viewed {
                "Mark file Viewed"
            } else {
                "Mark file not Viewed"
            }
            .into(),
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct State {
    pub id: String,
    pub head: String,
    pub pending: Option<String>,
    pub viewed: BTreeSet<String>,
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("GitHub response is missing {name}"))
}
fn api(endpoint: &str, method: &str, body: Option<Value>, cancel: &Cancel) -> Result<Value> {
    let mut command = github::command();
    command.args(["api", "--method", method, endpoint]);
    let input = if let Some(body) = body {
        command.args(["--input", "-"]);
        Some(serde_json::to_vec(&body)?)
    } else {
        None
    };
    let output = process::run(&mut command, input, cancel)?;
    ensure!(
        output.code == 0,
        "GitHub: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    if output.stdout.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&output.stdout).context("GitHub returned invalid JSON")
}
fn graphql(query: &str, variables: Value, cancel: &Cancel) -> Result<Value> {
    let value = api(
        "graphql",
        "POST",
        Some(json!({"query":query,"variables":variables})),
        cancel,
    )?;
    ensure!(
        value.get("errors").is_none(),
        "GitHub: {}",
        value.get("errors").unwrap_or(&Value::Null)
    );
    value.get("data").cloned().context("Missing GitHub data")
}
pub fn state(key: &PrKey, cancel: &Cancel) -> Result<State> {
    key.validate()?;
    let viewer = api("user", "GET", None, cancel)?;
    let login = field(&viewer, "login")?;
    let query = "query($owner:String!,$repo:String!,$number:Int!,$login:String!,$cursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){id headRefOid reviews(first:1,states:[PENDING],author:$login){nodes{id}} files(first:100,after:$cursor){nodes{path viewerViewedState} pageInfo{hasNextPage endCursor}}}}}";
    let mut cursor = Value::Null;
    let mut result = State::default();
    loop {
        let value = graphql(
            query,
            json!({"owner":key.owner,"repo":key.repo,"number":key.number,"login":login,"cursor":cursor}),
            cancel,
        )?;
        let pr = value
            .pointer("/repository/pullRequest")
            .context("Missing PR")?;
        let head = field(pr, "headRefOid")?;
        ensure!(
            result.head.is_empty() || result.head == head,
            "PR changed while reading review state; refresh and retry"
        );
        result.head = head.into();
        result.id = field(pr, "id")?.into();
        result.pending = pr
            .pointer("/reviews/nodes/0/id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let files = pr
            .pointer("/files/nodes")
            .and_then(Value::as_array)
            .context("Missing files")?;
        for file in files {
            if file.get("viewerViewedState").and_then(Value::as_str) == Some("VIEWED") {
                result.viewed.insert(field(file, "path")?.into());
            }
        }
        if pr
            .pointer("/files/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            != Some(true)
        {
            break;
        }
        let next = pr
            .pointer("/files/pageInfo/endCursor")
            .cloned()
            .context("Missing file cursor")?;
        ensure!(
            !next.is_null() && next != cursor,
            "Invalid GitHub pagination cursor"
        );
        cursor = next;
    }
    Ok(result)
}
fn matching(key: &PrKey, head: &str, cancel: &Cancel) -> Result<()> {
    let current = github::detail(key, cancel)?;
    ensure!(
        current.head == head,
        "PR head changed. Close this dialog with Esc, press r to load the latest revision, then confirm again. Your draft is retained."
    );
    Ok(())
}
/// The UI supplies the pinned revision; merge also uses GitHub's atomic head guard.
pub fn execute(key: &PrKey, head: &str, operation: &Operation, cancel: &Cancel) -> Result<String> {
    key.validate()?;
    matching(key, head, cancel)?;
    let endpoint = format!("repos/{}/{}/pulls/{}", key.owner, key.repo, key.number);
    match operation {
        Operation::Merge { squash, admin } => {
            let mut command = github::command();
            command.args([
                "pr",
                "merge",
                &key.url(),
                "--match-head-commit",
                head,
                if *squash { "--squash" } else { "--merge" },
            ]);
            if *admin {
                command.arg("--admin");
            }
            let output = process::checked(&mut command, cancel)?;
            let current = github::detail(key, cancel)?;
            return Ok(format!("{} · {}", current.state, output.trim()));
        }
        Operation::Close { body } => {
            // Closing and commenting are separate API operations; report partial success explicitly.
            api(&endpoint, "PATCH", Some(json!({"state":"closed"})), cancel)?;
            if !body.trim().is_empty() {
                api(&format!("repos/{}/{}/issues/{}/comments",key.owner,key.repo,key.number),"POST",Some(json!({"body":body})),cancel)
                    .context("PR was closed, but its closing comment could not be confirmed. Check GitHub before retrying")?;
            }
        }
        Operation::Comment {
            anchor,
            body,
            pending: false,
        } => {
            ensure!(!body.trim().is_empty(), "Comment cannot be empty");
            ensure!(
                anchor.start > 0 && anchor.end >= anchor.start,
                "Invalid line range"
            );
            let mut data = json!({"body":body,"commit_id":head,"path":anchor.path,"line":anchor.end,"side":anchor.side.api()});
            if anchor.start != anchor.end {
                data.as_object_mut()
                    .context("Expected JSON object")?
                    .insert("start_line".into(), json!(anchor.start));
                data.as_object_mut()
                    .context("Expected JSON object")?
                    .insert("start_side".into(), json!(anchor.side.api()));
            }
            api(&format!("{endpoint}/comments"), "POST", Some(data), cancel)?;
        }
        _ => {
            let state = state(key, cancel)?;
            ensure!(
                state.head == head,
                "PR head changed; refresh before submitting. Your draft is retained."
            );
            match operation {
                Operation::Viewed { path, viewed } => {
                    let name = if *viewed {
                        "markFileAsViewed"
                    } else {
                        "unmarkFileAsViewed"
                    };
                    let input = if *viewed {
                        "MarkFileAsViewedInput"
                    } else {
                        "UnmarkFileAsViewedInput"
                    };
                    graphql(
                        &format!(
                            "mutation($input:{input}!){{{name}(input:$input){{clientMutationId}}}}"
                        ),
                        json!({"input":{"pullRequestId":state.id,"path":path}}),
                        cancel,
                    )?;
                }
                Operation::Review { event, body } => {
                    ensure!(
                        ["APPROVE", "COMMENT", "REQUEST_CHANGES"].contains(&event.as_str()),
                        "Invalid review event"
                    );
                    ensure!(
                        event == "APPROVE" || !body.trim().is_empty() || state.pending.is_some(),
                        "Review message is required"
                    );
                    if let Some(id) = state.pending {
                        graphql(
                            "mutation($input:SubmitPullRequestReviewInput!){submitPullRequestReview(input:$input){clientMutationId}}",
                            json!({"input":{"pullRequestReviewId":id,"event":event,"body":body}}),
                            cancel,
                        )?;
                    } else {
                        graphql(
                            "mutation($input:AddPullRequestReviewInput!){addPullRequestReview(input:$input){clientMutationId}}",
                            json!({"input":{"pullRequestId":state.id,"commitOID":head,"event":event,"body":body}}),
                            cancel,
                        )?;
                    }
                }
                Operation::Comment {
                    anchor,
                    body,
                    pending: true,
                } => {
                    ensure!(!body.trim().is_empty(), "Comment cannot be empty");
                    ensure!(
                        anchor.start > 0 && anchor.end >= anchor.start,
                        "Invalid line range"
                    );
                    let mut thread = json!({"path":anchor.path,"line":anchor.end,"side":anchor.side.api(),"body":body});
                    if anchor.start != anchor.end {
                        thread
                            .as_object_mut()
                            .context("Expected JSON object")?
                            .insert("startLine".into(), json!(anchor.start));
                        thread
                            .as_object_mut()
                            .context("Expected JSON object")?
                            .insert("startSide".into(), json!(anchor.side.api()));
                    }
                    if let Some(id) = state.pending {
                        thread
                            .as_object_mut()
                            .context("Expected JSON object")?
                            .insert("pullRequestReviewId".into(), json!(id));
                        graphql(
                            "mutation($input:AddPullRequestReviewThreadInput!){addPullRequestReviewThread(input:$input){clientMutationId}}",
                            json!({"input":thread}),
                            cancel,
                        )?;
                    } else {
                        // Create the pending review with its first thread in one request.
                        graphql(
                            "mutation($input:AddPullRequestReviewInput!){addPullRequestReview(input:$input){clientMutationId}}",
                            json!({"input":{"pullRequestId":state.id,"commitOID":head,"threads":[thread]}}),
                            cancel,
                        )?;
                    }
                }
                _ => bail!("Unsupported operation"),
            }
        }
    }
    Ok(format!("{} succeeded", operation.label()))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Mentions {
    pub fetched: i64,
    pub users: Vec<String>,
}
fn paged_users(endpoint: &str, cancel: &Cancel) -> Result<BTreeSet<String>> {
    let mut users = BTreeSet::new();
    for page in 1.. {
        let values = api(
            &format!("{endpoint}?per_page=100&page={page}"),
            "GET",
            None,
            cancel,
        )?;
        let values = values.as_array().context("Missing GitHub users")?;
        for value in values {
            if let Some(login) = value.get("login").and_then(Value::as_str) {
                users.insert(login.into());
            }
        }
        if values.len() < 100 {
            break;
        }
    }
    Ok(users)
}
pub fn mention_key(key: &PrKey) -> String {
    storage::hash(format!("mentions:{}", key.id()))
}
pub fn mentions(key: &PrKey, cancel: &Cancel) -> Result<Mentions> {
    key.validate()?;
    let owner = api(&format!("users/{}", key.owner), "GET", None, cancel)?;
    let mut users = if owner.get("type").and_then(Value::as_str) == Some("Organization") {
        paged_users(&format!("orgs/{}/members", key.owner), cancel)?
    } else {
        BTreeSet::new()
    };
    let query = "query($owner:String!,$repo:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){participants(first:100,after:$cursor){nodes{login} pageInfo{hasNextPage endCursor}}}}}";
    let mut cursor = Value::Null;
    loop {
        let value = graphql(
            query,
            json!({"owner":key.owner,"repo":key.repo,"number":key.number,"cursor":cursor}),
            cancel,
        )?;
        let participants = value
            .pointer("/repository/pullRequest/participants")
            .context("Missing PR participants")?;
        for user in participants
            .get("nodes")
            .and_then(Value::as_array)
            .context("Missing participants")?
        {
            users.insert(field(user, "login")?.into());
        }
        if participants
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            != Some(true)
        {
            break;
        }
        let next = participants
            .pointer("/pageInfo/endCursor")
            .cloned()
            .context("Missing cursor")?;
        ensure!(
            !next.is_null() && next != cursor,
            "Invalid participant cursor"
        );
        cursor = next;
    }
    Ok(Mentions {
        fetched: chrono::Utc::now().timestamp(),
        users: users.into_iter().collect(),
    })
}
pub fn cached_mentions(storage: &Storage, key: &PrKey) -> Result<Mentions> {
    let path = storage.cache.join(format!("{}.json", mention_key(key)));
    if !path.exists() {
        return Ok(Mentions::default());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
pub fn save_mentions(storage: &Storage, key: &PrKey, value: &Mentions) -> Result<()> {
    storage::atomic_json(
        &storage.cache.join(format!("{}.json", mention_key(key))),
        value,
    )
}
