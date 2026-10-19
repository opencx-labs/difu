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
    #[serde(default)]
    pub channel: String,
    pub account: String,
    /// The tokens are stored through macOS `security`, so reading them never
    /// asks for the login password after difu updates. Older setups stored
    /// them in items owned by difu's binary and migrate on first connection.
    #[serde(default)]
    pub portable: bool,
}

#[cfg(any(target_os = "macos", test))]
impl Settings {
    pub(super) fn accepts_route(&self, route: &Route) -> bool {
        route.account == self.account
            && (valid_id(&route.channel, &['D'])
                || (!self.channel.is_empty() && route.channel == self.channel))
    }
}

// Deliberately no Debug implementation: tokens must never enter logs or session history.
#[cfg(target_os = "macos")]
#[derive(Serialize, Deserialize)]
struct Credentials {
    bot: String,
    app: String,
}

/// Validates and saves a Slack setup. Blank tokens keep the saved ones.
pub fn configure(
    storage: &Storage,
    bot: String,
    app: String,
    user: String,
    channel: String,
    saved: Option<Settings>,
) -> Result<Settings> {
    ensure!(
        valid_id(&user, &['U', 'W']),
        "Enter your Slack member ID (U…)"
    );
    ensure!(
        channel.is_empty() || valid_id(&channel, &['C', 'G']),
        "Enter a channel ID (C… or G…), or leave it blank for direct messages"
    );
    #[cfg(target_os = "macos")]
    {
        let (bot, app) = match saved {
            Some(saved) if bot.is_empty() && app.is_empty() => {
                let credentials = tokens(&saved)?;
                (credentials.bot, credentials.app)
            }
            _ => (bot, app),
        };
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
            portable: true,
        };
        store(&settings.account, &Credentials { bot, app })?;
        Ok(settings)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (storage, bot, app, saved);
        anyhow::bail!("Slack is available on macOS in this release")
    }
}

#[cfg(target_os = "macos")]
fn store(account: &str, credentials: &Credentials) -> Result<()> {
    crate::keychain::save(SERVICE, account, &serde_json::to_vec(credentials)?)
        .map_err(|_| anyhow::anyhow!("Cannot save Slack tokens in macOS Keychain"))
}

/// Items stored through `security`; legacy difu-owned items use `LEGACY_SERVICE`.
#[cfg(target_os = "macos")]
const SERVICE: &str = "app.difu.slack.tokens";
#[cfg(target_os = "macos")]
const LEGACY_SERVICE: &str = "app.difu.slack";

fn valid_id(id: &str, prefixes: &[char]) -> bool {
    id.len() > 1
        && id.starts_with(prefixes)
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

#[cfg(target_os = "macos")]
fn tokens(settings: &Settings) -> Result<Credentials> {
    let bytes = if settings.portable {
        crate::keychain::read(SERVICE, &settings.account).ok()
    } else {
        security_framework::passwords::get_generic_password(LEGACY_SERVICE, &settings.account).ok()
    }
    .ok_or_else(|| {
        anyhow::anyhow!("Cannot read Slack tokens from Keychain; open /slack to reconnect")
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid Slack Keychain entry; open /slack to reconnect"))
}

#[cfg(target_os = "macos")]
fn credentials(settings: &Settings) -> Result<api::Api> {
    let credentials = tokens(settings)?;
    api::Api::new(credentials.bot, credentials.app)
}

/// Copies a legacy difu-owned item to one stored through `security` under the
/// same account, so Slack threads keep routing, and marks the setup portable.
/// macOS asks for the login password this one last time.
#[cfg(target_os = "macos")]
pub(super) fn migrate(storage: &Storage, settings: &Settings) -> Result<()> {
    let credentials = tokens(settings)?;
    store(&settings.account, &credentials)?;
    storage.update_config(|config| {
        if let Some(current) = config.slack.as_mut().filter(|current| *current == settings) {
            current.portable = true;
        }
    })?;
    Ok(())
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
            portable: true,
        };
        let direct = Route {
            account: settings.account.clone(),
            channel: "DDIRECT".into(),
            thread: "1.0".into(),
            event: "Ev1".into(),
        };
        assert!(settings.accepts_route(&direct));
        assert!(!settings.accepts_route(&Route {
            account: "other".into(),
            ..direct.clone()
        }));
        assert!(!settings.accepts_route(&Route {
            channel: "COTHER".into(),
            ..direct.clone()
        }));
        let direct_only = Settings {
            channel: String::new(),
            ..settings.clone()
        };
        assert!(direct_only.accepts_route(&direct));
        assert!(!direct_only.accepts_route(&Route {
            channel: "C1".into(),
            ..direct
        }));
        let json = serde_json::to_value(settings)?;
        assert!(json.get("bot_token").is_none() && json.get("app_token").is_none());
        Ok(())
    }
}
