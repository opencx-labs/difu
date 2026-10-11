//! Slack API calls never include tokens, signed URLs, or response bodies in errors.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) struct Api {
    agent: ureq::Agent,
    bot_token: String,
    app_token: String,
    retry_after: Mutex<Option<Instant>>,
}

pub(super) struct Identity {
    pub team: String,
    pub bot: String,
}

impl Api {
    pub fn new(bot_token: String, app_token: String) -> Result<Self> {
        for (token, prefix) in [(&bot_token, "xoxb-"), (&app_token, "xapp-")] {
            ensure!(
                token.starts_with(prefix) && !token.chars().any(char::is_whitespace),
                "Enter a bot token beginning with xoxb- and an app token beginning with xapp-"
            );
        }
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .build();
        Ok(Self {
            agent: config.into(),
            bot_token,
            app_token,
            retry_after: Mutex::new(None),
        })
    }

    fn call(&self, method: &str, token: &str, body: Value) -> Result<Value> {
        ensure!(
            !self
                .retry_after
                .lock()
                .map_err(|_| anyhow::anyhow!("Slack rate-limit lock failed"))?
                .is_some_and(|until| Instant::now() < until),
            "Slack rate limit reached; waiting before retrying"
        );
        let body = serde_json::to_vec(&body)?;
        let mut response = self
            .agent
            .post(format!("https://slack.com/api/{method}"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json; charset=utf-8")
            .send(body.as_slice())
            .map_err(|_| anyhow::anyhow!("Slack request failed or timed out"))?;
        if response.status().as_u16() == 429 {
            let seconds = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30);
            *self
                .retry_after
                .lock()
                .map_err(|_| anyhow::anyhow!("Slack rate-limit lock failed"))? =
                Instant::now().checked_add(Duration::from_secs(seconds.max(1)));
            anyhow::bail!("Slack rate limit reached; waiting before retrying");
        }
        ensure!(response.status().is_success(), "Slack request was rejected");
        let bytes = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_vec()
            .map_err(|_| anyhow::anyhow!("Cannot read Slack response"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Slack returned an invalid response"))?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            anyhow::bail!("{}", api_error(value.get("error").and_then(Value::as_str)));
        }
        Ok(value)
    }

    pub fn identity(&self) -> Result<Identity> {
        let value = self.call("auth.test", &self.bot_token, json!({}))?;
        ensure!(
            value.get("bot_id").is_some(),
            "The Slack token must belong to a bot"
        );
        Ok(Identity {
            team: field(&value, "team_id")?,
            bot: field(&value, "user_id")?,
        })
    }

    pub fn socket_url(&self) -> Result<url::Url> {
        let value = self.call("apps.connections.open", &self.app_token, json!({}))?;
        let url = url::Url::parse(&field(&value, "url")?)
            .map_err(|_| anyhow::anyhow!("Slack returned an invalid connection URL"))?;
        ensure!(
            valid_socket_url(&url),
            "Slack returned an unexpected connection host"
        );
        Ok(url)
    }

    pub fn reply(&self, channel: &str, thread: &str, text: &str) -> Result<()> {
        self.call(
            "chat.postMessage",
            &self.bot_token,
            json!({
                "channel": channel, "thread_ts": thread, "text": text,
                "reply_broadcast": false, "unfurl_links": false, "unfurl_media": false,
                "parse": "none", "link_names": false, "mrkdwn": false
            }),
        )?;
        Ok(())
    }
}

fn field(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .context("Slack response is missing a required field")
}

fn valid_socket_url(url: &url::Url) -> bool {
    url.scheme() == "wss"
        && url
            .host_str()
            .is_some_and(|host| host.ends_with(".slack.com"))
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn api_error(code: Option<&str>) -> &'static str {
    match code {
        Some("invalid_auth" | "not_authed" | "token_revoked" | "token_expired") => {
            "Slack authentication failed; open /slack to replace the tokens"
        }
        Some("missing_scope" | "not_allowed_token_type") => {
            "Slack permissions are incomplete; check the app manifest and reinstall the app"
        }
        Some("not_in_channel" | "channel_not_found") => {
            "Slack cannot access the configured channel; invite the difu app to it"
        }
        Some("is_archived") => "The configured Slack channel is archived",
        Some("ratelimited") => "Slack rate limit reached; wait before retrying",
        _ => "Slack rejected the request",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_urls_cannot_redirect_credentials_or_execution_to_other_hosts() -> Result<()> {
        for (input, valid) in [
            ("wss://wss-primary.slack.com/link?ticket=secret", true),
            ("ws://wss.slack.com/link", false),
            ("wss://slack.com.attacker.test/link", false),
            ("wss://attacker-slack.com/link", false),
            ("wss://user@wss.slack.com/link", false),
            ("wss://wss.slack.com:444/link", false),
            ("wss://127.0.0.1/link", false),
        ] {
            assert_eq!(valid_socket_url(&url::Url::parse(input)?), valid);
        }
        assert_eq!(
            api_error(Some("xoxb-sensitive-token")),
            "Slack rejected the request"
        );
        Ok(())
    }
}
