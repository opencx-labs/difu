//! Native CLI account status; only explicit sign-in uses native credential storage.
use super::provider::Provider;
use crate::process::{self, Cancel};
use anyhow::Result;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connected,
    SignedOut,
    Unavailable,
}
impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::SignedOut => "Signed out",
            Self::Unavailable => "Unavailable — check CLI installation/access",
        }
    }
}
pub fn status(provider: Provider, cancel: &Cancel) -> Result<Status> {
    #[cfg(test)]
    anyhow::ensure!(
        crate::process::fixture::worker().is_some()
            || std::env::var_os("DIFU_CONNECTIONS_FIXTURE").is_some(),
        "Unscoped native status fixture"
    );
    let _deadline = Deadline::new(cancel.clone());
    let mut command = Command::new(provider.key());
    command.args(match provider {
        Provider::Codex => vec!["login", "status"],
        Provider::Claude => vec!["auth", "status", "--json"],
    });
    let output = process::run_limited(&mut command, cancel, 64 * 1024)?;
    Ok(parse(provider, &output))
}
fn parse(provider: Provider, output: &process::Output) -> Status {
    match provider {
        Provider::Claude => match serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|v| v.get("loggedIn").and_then(serde_json::Value::as_bool))
        {
            Some(true) if output.code == 0 => Status::Connected,
            Some(false) if output.code == 0 || output.code == 1 => Status::SignedOut,
            _ => Status::Unavailable,
        },
        Provider::Codex => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if output.code == 0
                && text.lines().any(|line| {
                    line.trim() == "Logged in using ChatGPT"
                        || line.trim().starts_with("Logged in using an API key")
                })
            {
                Status::Connected
            } else if output.code == 1 && text.lines().any(|line| line.trim() == "Not logged in") {
                Status::SignedOut
            } else {
                Status::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_status_contracts_are_sanitized_and_exit_aware() {
        for (provider, code, stdout, stderr, expected) in [
            (
                Provider::Codex,
                0,
                "",
                "WARNING: PATH unavailable\nLogged in using ChatGPT\n",
                Status::Connected,
            ),
            (
                Provider::Codex,
                0,
                "",
                "Logged in using an API key - sk-secret\n",
                Status::Connected,
            ),
            (Provider::Codex, 1, "", "Not logged in\n", Status::SignedOut),
            (
                Provider::Codex,
                1,
                "",
                "unexpected token sk-secret",
                Status::Unavailable,
            ),
            (
                Provider::Claude,
                0,
                r#"{"loggedIn":true,"email":"private@example.invalid"}"#,
                "",
                Status::Connected,
            ),
            (
                Provider::Claude,
                1,
                r#"{"loggedIn":false,"authMethod":"none"}"#,
                "",
                Status::SignedOut,
            ),
            (
                Provider::Claude,
                1,
                r#"{"loggedIn":true}"#,
                "",
                Status::Unavailable,
            ),
            (
                Provider::Claude,
                0,
                "invalid private@example.invalid",
                "",
                Status::Unavailable,
            ),
        ] {
            let actual = parse(
                provider,
                &process::Output {
                    code,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: stderr.as_bytes().to_vec(),
                },
            );
            assert_eq!(actual, expected);
            assert!(!actual.label().contains("secret") && !actual.label().contains('@'));
        }
    }
}

pub struct Capabilities {
    pub models: Vec<crate::model::ModelInfo>,
    pub modes: Vec<super::permissions::Choice>,
}
pub fn capabilities(
    defaults: &crate::storage::AgentDefaults,
    cancel: &Cancel,
) -> Result<Capabilities> {
    #[cfg(test)]
    anyhow::ensure!(
        std::env::var_os("DIFU_CONNECTIONS_FIXTURE").is_some(),
        "Unscoped native capability fixture"
    );
    let _deadline = Deadline::new(cancel.clone());
    let provider = defaults.resolved_provider();
    let (models, modes) = match provider {
        Provider::Codex => crate::codex::model_capabilities(cancel)?,
        Provider::Claude => (super::claude::models(cancel)?, serde_json::Value::Null),
    };
    let mut session = super::Session::new(
        "settings-capabilities".into(),
        super::Job::Coding(super::Launch {
            repository: defaults
                .repository
                .clone()
                .unwrap_or(std::env::current_dir()?),
            isolated: true,
            base: "HEAD".into(),
            prompt: String::new(),
            model: defaults.model.clone(),
            effort: defaults.effort.clone(),
        }),
    );
    session.provider = provider;
    session.permission_state.capabilities = serde_json::json!({"collaborationModes":modes});
    Ok(Capabilities {
        models,
        modes: super::permissions::snapshot(&session).modes,
    })
}

struct Deadline(std::sync::mpsc::Sender<()>);
impl Deadline {
    fn new(cancel: Cancel) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if rx.recv_timeout(std::time::Duration::from_secs(10))
                == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                cancel.cancel();
            }
        });
        Self(tx)
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[cfg(test)]
pub(crate) mod boundary_tests {
    use super::*;
    use anyhow::{Context, ensure};
    use std::{fs, os::unix::fs::PermissionsExt};
    #[test]
    fn capability_and_status_subprocess_contracts() -> Result<()> {
        isolated("agents::connections::boundary_tests::isolated_native_cli_child")
    }
    pub(crate) fn isolated(test: &str) -> Result<()> {
        let root = tempfile::tempdir()?;
        for name in ["codex", "claude"] {
            let path = root.path().join(name);
            fs::write(&path, include_str!("../../tests/fixtures/connections.py"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", test, "--nocapture"])
            .env("DIFU_CONNECTIONS_FIXTURE", root.path())
            .env("CLAUDE_CONFIG_DIR", root.path())
            .env(
                "PATH",
                if test.ends_with("missing_cli_child") {
                    std::ffi::OsString::new()
                } else {
                    std::env::join_paths(std::iter::once(root.path().to_path_buf()).chain(
                        std::env::split_paths(&std::env::var_os("PATH").context("PATH")?),
                    ))?
                },
            )
            .output()?;
        ensure!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
    #[test]
    fn isolated_native_cli_child() -> Result<()> {
        let Some(root) = std::env::var_os("DIFU_CONNECTIONS_FIXTURE") else {
            return Ok(());
        };
        for provider in [Provider::Codex, Provider::Claude] {
            assert_eq!(status(provider, &Cancel::default())?, Status::Connected);
            let caps = capabilities(
                &crate::storage::AgentDefaults {
                    provider: Some(provider),
                    repository: Some(root.clone().into()),
                    ..Default::default()
                },
                &Cancel::default(),
            )?;
            assert!(caps.models.iter().any(|m| m.id
                == if provider == Provider::Codex {
                    "gpt-fixture"
                } else {
                    "claude/sonnet"
                }));
            assert!(caps.modes.iter().any(|m| m.id == "plan" && m.available));
            if provider == Provider::Claude {
                assert!(caps.modes.iter().any(|m| m.id == "dontAsk"));
                // Exercise the production user-settings resolver with its explicit
                // provider directory; no HOME override or home config access.
                let user_settings = std::path::PathBuf::from(&root).join("settings.json");
                fs::write(
                    &user_settings,
                    r#"{"permissions":{"disableAutoMode":"disable"}}"#,
                )?;
                let configured = capabilities(
                    &crate::storage::AgentDefaults {
                        provider: Some(provider),
                        repository: Some(root.clone().into()),
                        ..Default::default()
                    },
                    &Cancel::default(),
                )?;
                assert!(
                    configured
                        .modes
                        .iter()
                        .any(|mode| mode.id == "auto" && !mode.available)
                );
                fs::remove_file(user_settings)?;
                let repository = std::path::PathBuf::from(&root).join("restricted-repository");
                fs::create_dir_all(repository.join(".claude"))?;
                fs::write(
                    repository.join(".claude/settings.json"),
                    r#"{"permissions":{"disableAutoMode":"disable","disableBypassPermissionsMode":"disable"}}"#,
                )?;
                let restricted = capabilities(
                    &crate::storage::AgentDefaults {
                        provider: Some(provider),
                        repository: Some(repository),
                        ..Default::default()
                    },
                    &Cancel::default(),
                )?;
                for id in ["auto", "bypassPermissions"] {
                    assert!(restricted.modes.iter().any(|m| m.id == id && !m.available));
                }
                assert!(
                    restricted
                        .modes
                        .iter()
                        .any(|m| m.id == "dontAsk" && m.available)
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use anyhow::{Context, ensure};
    #[test]
    fn native_status_absent_malformed_signed_out_and_timeout_are_distinct() -> Result<()> {
        super::boundary_tests::isolated("agents::connections::failure_tests::missing_cli_child")?;
        super::boundary_tests::isolated("agents::connections::failure_tests::status_failure_child")
    }
    #[test]
    fn missing_cli_child() -> Result<()> {
        if std::env::var_os("DIFU_CONNECTIONS_FIXTURE").is_none() {
            return Ok(());
        }
        for provider in [Provider::Codex, Provider::Claude] {
            assert!(status(provider, &Cancel::default()).is_err());
        }
        Ok(())
    }
    #[test]
    fn status_failure_child() -> Result<()> {
        let Some(root) = std::env::var_os("DIFU_CONNECTIONS_FIXTURE").map(std::path::PathBuf::from)
        else {
            return Ok(());
        };
        std::fs::write(root.join("signed-out"), "")?;
        for provider in [Provider::Codex, Provider::Claude] {
            assert_eq!(status(provider, &Cancel::default())?, Status::SignedOut);
        }
        std::fs::remove_file(root.join("signed-out"))?;
        std::fs::write(root.join("malformed-status"), "")?;
        assert_eq!(
            status(Provider::Claude, &Cancel::default())?,
            Status::Unavailable
        );
        std::fs::remove_file(root.join("malformed-status"))?;
        std::fs::write(root.join("hang-status"), "")?;
        let start = std::time::Instant::now();
        let error = status(Provider::Codex, &Cancel::default())
            .err()
            .context("Timeout did not cancel")?;
        ensure!(
            start.elapsed() < std::time::Duration::from_secs(12),
            "Unbounded status worker"
        );
        assert!(!error.to_string().contains("private@example.invalid"));
        Ok(())
    }
}
