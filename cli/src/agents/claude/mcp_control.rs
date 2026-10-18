use super::*;
use crate::agents::mcp::{self as inventory, Action, Snapshot};

pub(super) fn execute(
    store: &Store,
    id: &str,
    rpc: &mut Connection,
    action: Action,
    cancel: &Cancel,
) -> Result<Snapshot> {
    let value = rpc.call_preserving_requests(
        inventory::claude_request(&Action::Refresh),
        cancel,
        |frame| mcp(store, id, frame),
    )?;
    let mut servers = inventory::claude(&value)?;
    let session = store.get(id)?;
    let cwd = session.workspace.as_deref().unwrap_or(session.job.root());
    inventory::claude_locations(&mut servers, cwd);
    inventory::validate(&servers, &action)?;
    if action == Action::Refresh {
        return Ok(Snapshot {
            servers,
            ..Snapshot::default()
        });
    }
    let result =
        rpc.call_preserving_requests(inventory::claude_request(&action), cancel, |frame| {
            mcp(store, id, frame)
        })?;
    let authorization_url = inventory::authorization_url(&result);
    let value = rpc.call_preserving_requests(
        inventory::claude_request(&Action::Refresh),
        cancel,
        |frame| mcp(store, id, frame),
    )?;
    let message = if matches!(action, Action::Authenticate { .. }) {
        if authorization_url.is_some() {
            "Authentication started. Open the sign-in link, then refresh to check the connection."
        } else {
            anyhow::ensure!(
                result.get("requiresUserAction").and_then(Value::as_bool) == Some(false),
                "Claude did not return an authentication link; refresh and retry"
            );
            "Authentication completed."
        }
    } else {
        "MCP server updated."
    };
    let mut servers = inventory::claude(&value)?;
    inventory::claude_locations(&mut servers, cwd);
    Ok(Snapshot {
        servers,
        authentication_server: authorization_url
            .as_ref()
            .and_then(|_| action.name().map(str::to_owned)),
        authorization_url,
        message: Some(message.into()),
    })
}
