//! Explicit PR associations, including PRs opened in a different repository.
use super::server::Store;
use crate::{model::PrKey, process::Cancel};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(super) const TOOL: &str = "difu_attach_pr";
pub(super) const INSTRUCTIONS: &str = "After creating a pull request, call difu_attach_pr with its full GitHub URL to attach it to this session, including PRs in other repositories. Attached PRs appear in the session list, chat footer, and Option+D diff selector. Attach only PRs created or worked on for this session, not every PR mentioned in conversation.";

pub(super) fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Attach a pull request to this session using its full GitHub URL, including PRs in another repository. Makes its badge and published diff available in difu. Does not modify the PR or switch workspaces.","inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"],"additionalProperties":false}})
}

pub(super) fn handle(store: &Store, id: &str, args: &Value, cancel: &Cancel) -> Result<Value> {
    let key = key(args)?;
    let session = store.get(id)?;
    ensure!(
        !session.orchestrator,
        "Attach PRs from their coding session"
    );
    let summary = session.summary();
    let pr = crate::github::session_pr(&summary.workspace, None, Some(&key), cancel)?;
    super::pr_cache::attach(&store.storage, &summary, pr, cancel)?;
    Ok(json!({"attached":key.id(),"url":key.url()}))
}

fn key(args: &Value) -> Result<PrKey> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .context("Missing full GitHub PR URL")?;
    PrKey::from_url(url).context("Provide a full GitHub pull request URL")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_a_full_pr_identity() -> Result<()> {
        assert_eq!(
            key(&json!({"url":"https://github.com/other/repo/pull/42"}))?.id(),
            "other/repo#42"
        );
        for url in [
            "42",
            "other/repo#42",
            "https://example.com/other/repo/pull/42",
            "https://github.com/other/repo/issues/42",
        ] {
            assert!(key(&json!({"url":url})).is_err());
        }
        Ok(())
    }
}
