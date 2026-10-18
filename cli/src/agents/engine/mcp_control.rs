use super::*;
use crate::agents::mcp::{self as inventory, Action, Snapshot};

fn list(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    cancel: &Cancel,
) -> Result<Vec<inventory::Server>> {
    let session = store.get(id)?;
    let thread = session
        .thread_id
        .context("Start the agent session before opening /mcp")?;
    let mut values = Vec::new();
    let mut cursor = Value::Null;
    let mut seen = std::collections::HashSet::new();
    loop {
        let result = rpc.call(
            "mcpServerStatus/list",
            json!({"threadId":thread,"cursor":cursor,"limit":100,"detail":"toolsAndAuthOnly"}),
            cancel,
            |v| event(store, id, v),
        )?;
        values.extend(
            result
                .get("data")
                .and_then(Value::as_array)
                .context("Codex did not return an MCP inventory; update Codex and retry")?
                .iter()
                .cloned(),
        );
        cursor = result.get("nextCursor").cloned().unwrap_or(Value::Null);
        if cursor.is_null() {
            break;
        }
        ensure!(
            seen.insert(cursor.to_string()),
            "Codex returned a repeated MCP pagination cursor"
        );
    }
    inventory::codex(&values)
}

pub(super) fn execute(
    rpc: &mut Connection,
    store: &Store,
    id: &str,
    action: Action,
    cancel: &Cancel,
) -> Result<Snapshot> {
    let servers = list(rpc, store, id, cancel)?;
    inventory::validate(&servers, &action)?;
    if action == Action::Refresh {
        return Ok(Snapshot {
            servers,
            ..Snapshot::default()
        });
    }
    let session = store.get(id)?;
    ensure!(
        session.turn_id.is_none(),
        "Wait for the current Codex turn to finish before changing MCP connections"
    );
    let mut authorization_url = None;
    let message = match &action {
        Action::Toggle { name, enabled } => {
            // Quote the name as a TOML key segment, never interpret dots as nesting.
            let key = format!("mcp_servers.{}.enabled", serde_json::to_string(name)?);
            let result = rpc.call(
                "config/value/write",
                json!({"keyPath":key,"value":enabled,"mergeStrategy":"replace"}),
                cancel,
                |v| event(store, id, v),
            )?;
            ensure!(
                result.get("overriddenMetadata").is_none_or(Value::is_null),
                "Codex saved the user setting, but a higher-priority configuration overrides it. Update that configuration and refresh"
            );
            rpc.call("config/mcpServer/reload", Value::Null, cancel, |v| event(store,id,v))
                .context("The Codex setting was saved, but runtime reload failed. Reconnect to retry the reload")?;
            "Saved in Codex user configuration and reloaded this agent's MCP connections. Other running agents refresh separately."
        }
        Action::Reconnect { .. } => {
            rpc.call("config/mcpServer/reload", Value::Null, cancel, |v| {
                event(store, id, v)
            })?;
            "Reloaded this Codex agent's MCP connections (Codex reloads all servers together)."
        }
        Action::Authenticate { name } => {
            let result = rpc.call(
                "mcpServer/oauth/login",
                json!({"name":name,"threadId":session.thread_id}),
                cancel,
                |v| event(store, id, v),
            )?;
            authorization_url = inventory::authorization_url(&result);
            ensure!(
                authorization_url.is_some(),
                "Codex did not return an authentication link; refresh and retry"
            );
            "Open the sign-in link, complete authentication, then reconnect to refresh the tools."
        }
        Action::ClearAuth { .. } => {
            anyhow::bail!("Clear authentication is not exposed by this Codex protocol")
        }
        Action::Refresh => "MCP status refreshed.",
    };
    Ok(Snapshot {
        servers: list(rpc, store, id, cancel)?,
        message: Some(message.into()),
        authentication_server: authorization_url
            .as_ref()
            .and_then(|_| action.name().map(str::to_owned)),
        authorization_url,
    })
}
