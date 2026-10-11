//! Optional, user-scoped Slack access. Credentials live only in macOS Keychain.
#[cfg(target_os = "macos")]
mod api;
#[cfg(any(target_os = "macos", test))]
mod commands;
#[cfg(any(target_os = "macos", test))]
mod protocol;
#[cfg(target_os = "macos")]
mod runtime;
#[cfg(target_os = "macos")]
mod socket;

use crate::storage::Storage;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub team: String,
    pub bot: String,
    pub user: String,
    pub channel: String,
    pub account: String,
}

// Deliberately no Debug implementation: tokens must never enter logs or session history.
#[cfg(target_os = "macos")]
#[derive(Serialize, Deserialize)]
struct Credentials {
    bot: String,
    app: String,
}

pub fn configure(
    storage: &Storage,
    bot: String,
    app: String,
    user: String,
    channel: String,
) -> Result<Settings> {
    ensure!(
        valid_id(&user, &['U', 'W']),
        "Enter your Slack member ID (U…)"
    );
    ensure!(
        valid_id(&channel, &['C', 'G']),
        "Enter the channel ID (C… or G…)"
    );
    #[cfg(target_os = "macos")]
    {
        let api = api::Api::new(bot.clone(), app.clone())?;
        let identity = api.identity()?;
        api.socket_url()?;
        // Each configuration gets its own item so a failed config write cannot
        // replace the credentials of a currently running connection.
        let nonce = tempfile::NamedTempFile::new()?;
        let account = crate::storage::hash(format!(
            "{}:{}:{}",
            storage.config.display(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            nonce.path().display()
        ));
        let settings = Settings {
            enabled: true,
            team: identity.team,
            bot: identity.bot,
            user,
            channel,
            account,
        };
        let bytes = serde_json::to_vec(&Credentials { bot, app })?;
        security_framework::passwords::set_generic_password(
            "app.difu.slack",
            &settings.account,
            &bytes,
        )
        .map_err(|_| anyhow::anyhow!("Cannot save Slack tokens in macOS Keychain"))?;
        Ok(settings)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (storage, bot, app);
        anyhow::bail!("Slack is available on macOS in this release")
    }
}

fn valid_id(id: &str, prefixes: &[char]) -> bool {
    id.len() > 1
        && id.starts_with(prefixes)
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

#[cfg(target_os = "macos")]
fn credentials(settings: &Settings) -> Result<api::Api> {
    let bytes =
        security_framework::passwords::get_generic_password("app.difu.slack", &settings.account)
            .map_err(|_| {
                anyhow::anyhow!("Cannot read Slack tokens from Keychain; open /slack to reconnect")
            })?;
    let credentials: Credentials = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid Slack Keychain entry; open /slack to reconnect"))?;
    api::Api::new(credentials.bot, credentials.app)
}

pub(super) fn start(
    store: std::sync::Arc<super::server::Store>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    #[cfg(target_os = "macos")]
    runtime::start(store, stopped);
    #[cfg(not(target_os = "macos"))]
    let _ = (store, stopped);
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Route {
    pub account: String,
    pub channel: String,
    pub thread: String,
    pub event: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slack_scope_requires_explicit_ids_and_serializes_no_tokens() -> Result<()> {
        assert!(valid_id("UALI123", &['U', 'W']));
        assert!(valid_id("CPRIVATE123", &['C', 'G']));
        for id in ["", "*", "ali", "U", "U123 C123", "<@U123>"] {
            assert!(!valid_id(id, &['U', 'W']));
        }
        let settings = Settings {
            enabled: true,
            team: "T1".into(),
            bot: "UBOT".into(),
            user: "UALI".into(),
            channel: "C1".into(),
            account: "keychain-reference".into(),
        };
        let json = serde_json::to_value(settings)?;
        assert!(json.get("bot_token").is_none() && json.get("app_token").is_none());
        Ok(())
    }
}
