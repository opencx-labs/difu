//! Provider-independent MCP inventory. Deliberately excludes credentials and raw configuration.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    Refresh,
    Reconnect { name: String },
    Toggle { name: String, enabled: bool },
    Authenticate { name: String },
    ClearAuth { name: String },
}
impl Action {
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Refresh => None,
            Self::Reconnect { name }
            | Self::Toggle { name, .. }
            | Self::Authenticate { name }
            | Self::ClearAuth { name } => Some(name),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub servers: Vec<Server>,
    pub message: Option<String>,
    pub authorization_url: Option<String>,
    pub authentication_server: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    pub status: String,
    pub auth: Option<String>,
    pub scope: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub config_location: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub transport: String,
    pub endpoint: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
    pub tools: Vec<Tool>,
    pub can_toggle: bool,
    pub can_authenticate: bool,
    pub can_reconnect: bool,
    pub can_clear_auth: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Option<Value>,
}
fn string(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}
fn tools(value: &Value) -> Vec<Tool> {
    let mut tools = match value {
        Value::Array(values) => values.iter().collect::<Vec<_>>(),
        Value::Object(values) => values.values().collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter_map(|tool| {
        Some(Tool {
            name: string(tool, "name")?,
            description: string(tool, "description").unwrap_or_default(),
            schema: tool.get("inputSchema").filter(|v| !v.is_null()).cloned(),
        })
    })
    .collect::<Vec<_>>();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}
/// Display only the origin: URL paths, query strings and userinfo can contain secrets.
fn origin(value: Option<&str>) -> Option<String> {
    let url = url::Url::parse(value?).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| {
        let origin = url.origin().ascii_serialization();
        // Conventional MCP paths are useful to display; arbitrary paths may hold tokens.
        if matches!(url.path(), "/mcp" | "/sse") {
            format!("{origin}{}", url.path())
        } else {
            origin
        }
    })
}
pub fn claude(value: &Value) -> Result<Vec<Server>> {
    let values = value
        .get("mcpServers")
        .and_then(Value::as_array)
        .context("Claude did not return an MCP inventory; update Claude Code and retry")?;
    let mut servers = Vec::new();
    for value in values {
        let name = string(value, "name").context("MCP server has no name")?;
        let transport = value
            .pointer("/config/type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let internal =
            transport == "sdk" || value.get("source").and_then(Value::as_str) == Some("sdk");
        let status = string(value, "status").unwrap_or_else(|| "unknown".into());
        let remote = matches!(transport.as_str(), "http" | "sse" | "claudeai-proxy")
            || status == "needs-auth";
        servers.push(Server {
            name,
            status,
            auth: string(value, "authStatus").or_else(|| {
                if value.get("status").and_then(Value::as_str) == Some("needs-auth") {
                    Some("notLoggedIn".into())
                } else if value.get("status").and_then(Value::as_str) == Some("connected")
                    && value
                        .pointer("/config/headers")
                        .and_then(Value::as_object)
                        .is_some_and(|headers| {
                            headers
                                .keys()
                                .any(|k| k.eq_ignore_ascii_case("authorization"))
                        })
                {
                    Some("authenticated".into())
                } else {
                    None
                }
            }),
            source: string(value, "source")
                .or_else(|| string(value, "scope"))
                .unwrap_or_default(),
            config_location: None,
            capabilities: capabilities(value.get("capabilities"), value.get("tools").is_some()),
            scope: string(value, "scope").unwrap_or_else(|| "session".into()),
            endpoint: origin(value.pointer("/config/url").and_then(Value::as_str)),
            transport,
            version: value
                .pointer("/serverInfo/version")
                .and_then(Value::as_str)
                .map(str::to_owned),
            error: string(value, "error"),
            tools: tools(&value["tools"]),
            can_toggle: !internal,
            can_reconnect: !internal,
            can_authenticate: remote && !internal,
            can_clear_auth: remote && !internal,
        });
    }
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(servers)
}
pub fn codex(values: &[Value]) -> Result<Vec<Server>> {
    let mut servers = Vec::new();
    for value in values {
        let name = string(value, "name").context("MCP server has no name")?;
        let auth = string(value, "authStatus");
        let plugin = string(value, "pluginId");
        servers.push(Server {
            name,
            // A cached catalog (even a populated one) is not proof of a live connection.
            status: string(value, "runtimeStatus").unwrap_or_else(|| "unknown".into()),
            can_authenticate: auth
                .as_deref()
                .is_some_and(|a| matches!(a, "notLoggedIn" | "oAuth")),
            auth,
            source: if plugin.is_some() {
                "plugin"
            } else {
                "configured"
            }
            .into(),
            config_location: None,
            capabilities: capabilities(
                value.get("serverCapabilities"),
                value
                    .get("tools")
                    .and_then(Value::as_object)
                    .is_some_and(|tools| !tools.is_empty()),
            ),
            scope: plugin
                .clone()
                .map(|p| format!("Plugin: {p}"))
                .unwrap_or_else(|| "Codex configuration".into()),
            transport: if value.get("httpOrigin").is_some_and(Value::is_string) {
                "http"
            } else {
                "stdio / provider"
            }
            .into(),
            endpoint: origin(value.get("httpOrigin").and_then(Value::as_str)),
            version: value
                .pointer("/serverInfo/version")
                .and_then(Value::as_str)
                .map(str::to_owned),
            error: string(value, "toolsError"),
            tools: tools(&value["tools"]),
            can_toggle: plugin.is_none(),
            can_reconnect: true,
            can_clear_auth: false,
        });
    }
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(servers)
}
fn capabilities(value: Option<&Value>, has_tools: bool) -> Vec<String> {
    let mut names = ["tools", "resources", "prompts", "logging"]
        .into_iter()
        .filter(|key| value.and_then(|v| v.get(*key)).is_some())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if has_tools && !names.iter().any(|s| s == "tools") {
        names.insert(0, "tools".into());
    }
    names
}
/// Use the provider's reported scope, without reading or copying credential-bearing configuration.
pub fn claude_locations(servers: &mut [Server], cwd: &std::path::Path) {
    let home_config = if std::env::var_os("CLAUDE_CONFIG_DIR").is_none() {
        dirs::home_dir()
            .map(|home| home.join(".claude.json"))
            .filter(|path| path.is_file())
    } else {
        None
    };
    for server in servers {
        let location = match server.source.as_str() {
            "local" => home_config
                .as_ref()
                .map(|path| format!("{} [project: {}]", path.display(), cwd.display()))
                .unwrap_or_else(|| format!("Local configuration [project: {}]", cwd.display())),
            "project" => cwd.join(".mcp.json").display().to_string(),
            "user" => home_config
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "Claude user configuration".into()),
            "claudeai" => "claude.ai connectors".into(),
            "plugin" => "Claude Code plugin configuration".into(),
            "sdk" => "Provided by Difu".into(),
            _ => server.scope.clone(),
        };
        server.config_location = Some(location);
    }
}
pub fn validate<'a>(servers: &'a [Server], action: &Action) -> Result<Option<&'a Server>> {
    let Some(name) = action.name() else {
        return Ok(None);
    };
    let server = servers
        .iter()
        .find(|s| s.name == name)
        .context("MCP server is no longer available; refresh the list")?;
    let allowed = match action {
        Action::Refresh => true,
        Action::Reconnect { .. } => server.can_reconnect && server.status != "disabled",
        Action::Toggle { .. } => server.can_toggle,
        Action::Authenticate { .. } => server.can_authenticate && server.status != "disabled",
        Action::ClearAuth { .. } => server.can_clear_auth,
    };
    ensure!(
        allowed,
        "This action is unavailable for this MCP server; use its provider or plugin settings"
    );
    Ok(Some(server))
}
pub fn claude_request(action: &Action) -> Value {
    match action {
        Action::Refresh => json!({"subtype":"mcp_status"}),
        Action::Reconnect { name } => json!({"subtype":"mcp_reconnect","serverName":name}),
        Action::Toggle { name, enabled } => {
            json!({"subtype":"mcp_toggle","serverName":name,"enabled":enabled})
        }
        Action::Authenticate { name } => json!({"subtype":"mcp_authenticate","serverName":name}),
        Action::ClearAuth { name } => json!({"subtype":"mcp_clear_auth","serverName":name}),
    }
}
pub fn authorization_url(value: &Value) -> Option<String> {
    ["authorizationUrl", "authUrl", "url"]
        .iter()
        .find_map(|key| {
            let raw = value.get(key)?.as_str()?;
            let url = url::Url::parse(raw).ok()?;
            matches!(url.scheme(), "https" | "http").then(|| raw.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_retains_failures_and_disabled_servers_without_exposing_config() -> Result<()> {
        let servers = claude(&json!({"mcpServers":[
            {"name":"remote","status":"failed","error":"Connection refused","scope":"project",
             "config":{"type":"http","url":"https://user:password@example.com/private/token?key=secret","headers":{"Authorization":"secret"}},
             "tools":[{"name":"cached"}]},
            {"name":"off","status":"disabled","config":{"type":"stdio","env":{"API_KEY":"secret"}}},
            {"name":"difu","status":"connected","config":{"type":"sdk"}}
        ]}))?;
        let remote = servers
            .iter()
            .find(|s| s.name == "remote")
            .context("remote")?;
        assert_eq!(remote.status, "failed");
        assert_eq!(remote.endpoint.as_deref(), Some("https://example.com"));
        assert_eq!(remote.error.as_deref(), Some("Connection refused"));
        let serialized = serde_json::to_string(&servers)?;
        assert!(
            !serialized.contains("secret")
                && !serialized.contains("password")
                && !serialized.contains("API_KEY")
        );
        assert!(
            servers
                .iter()
                .any(|s| s.name == "off" && s.status == "disabled")
        );
        assert!(
            validate(
                &servers,
                &Action::Toggle {
                    name: "difu".into(),
                    enabled: false
                }
            )
            .is_err()
        );
        assert!(validate(&servers, &Action::Reconnect { name: "off".into() }).is_err());
        Ok(())
    }
    #[test]
    fn codex_does_not_confuse_cached_tools_with_a_connection() -> Result<()> {
        let servers = codex(&[
            json!({"name":"cached","runtimeStatus":null,"tools":{"hello":{"name":"hello","inputSchema":{"type":"object"}}},"authStatus":"notLoggedIn"}),
        ])?;
        let server = servers.first().context("server")?;
        assert_eq!(server.status, "unknown");
        assert_eq!(server.tools.len(), 1);
        assert!(server.can_authenticate);
        assert!(
            validate(
                &servers,
                &Action::Toggle {
                    name: "missing".into(),
                    enabled: false
                }
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn malformed_inventory_is_an_error_not_an_empty_success() {
        assert!(claude(&json!({"error":"unsupported"})).is_err());
        assert!(codex(&[json!({"tools":{}})]).is_err());
    }
    #[test]
    fn auth_links_must_use_browser_protocols() {
        assert_eq!(
            authorization_url(&json!({"authUrl":"https://auth.example.com/login?state=foo"})),
            Some("https://auth.example.com/login?state=foo".into())
        );
        assert!(authorization_url(&json!({"authorizationUrl":"file:///etc/passwd"})).is_none());
        assert!(authorization_url(&json!({"authUrl":"javascript:alert(1)"})).is_none());
    }
}
