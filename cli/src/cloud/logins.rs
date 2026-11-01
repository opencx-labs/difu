//! Codex, Claude Code and GitHub logins on a cloud server: status, device-code
//! logins started from any client, and the Claude token kept in `server.env`.

/// Extra environment for agent processes (e.g. `CLAUDE_CODE_OAUTH_TOKEN` from
/// the server's 0600 `server.env`). Empty on ordinary installations.
pub fn agent_env() -> Vec<(String, String)> {
    Vec::new()
}
