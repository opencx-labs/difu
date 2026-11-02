//! Server settings methods for paired clients (`server.repos.*`, `server.rules.*`,
//! `server.logins`, `server.login.*`, `server.logout`, `server.disk*`); see
//! docs/cloud-agents.md.
//! Returns `None` for methods this module does not own.
use crate::{
    agents::{Reply, Request},
    cloud::{disk, logins, repos},
    storage::Storage,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, thread};

pub fn dispatch(storage: &Storage, method: &str, params: &Value) -> Option<Result<Value>> {
    let server_only = method.starts_with("server.repos")
        || method.starts_with("server.login")
        || method.starts_with("server.disk")
        || method == "server.logout";
    let result = match method {
        _ if server_only && !super::api::config(storage).server.enabled => Err(anyhow::anyhow!(
            "This computer is not a difu server; `{method}` is only available on servers"
        )),
        "server.repos" => repos::list(storage).map(|repositories| {
            json!({
                "repositories": repositories,
                "refreshMinutes": super::api::config(storage).server.refresh_minutes(),
            })
        }),
        "server.repos.available" => crate::github::repositories(&crate::process::Cancel::default())
            .map(|names| {
                let names: Vec<Value> = names
                    .into_iter()
                    .map(|name| json!({"name": name}))
                    .collect();
                json!({"repositories": names})
            }),
        "server.repos.add" => text(params, "name").and_then(|name| {
            let name = repos::add(storage, &name)?;
            // Refreshing clones a listed repository that is missing, and skips
            // one removed in the meantime.
            background("difu-repos-add", storage, move |storage| {
                repos::refresh(storage, Some(&name))
            })
        }),
        "server.repos.remove" => {
            text(params, "name").and_then(|name| repos::remove(storage, &name).map(|()| json!({})))
        }
        "server.repos.refresh" => optional(params, "name").and_then(|name| {
            repos::check(storage, name.as_deref())?;
            background("difu-repos-refresh", storage, move |storage| {
                repos::refresh(storage, name.as_deref())
            })
        }),
        "server.disk" => flag(params, "refresh").and_then(|refresh| {
            match crate::agents::client::request(
                storage,
                Request::Disk {
                    server: None,
                    refresh,
                },
            )? {
                Reply::Disk(report) => Ok(serde_json::to_value(report)?),
                _ => anyhow::bail!("Unexpected disk reply"),
            }
        }),
        "server.disk.clean" => text(params, "action").and_then(|name| {
            let action: disk::Action = serde_json::from_value(json!(name)).with_context(|| {
                format!(
                    "Unknown cleanup `{name}`; expected archivedWorktrees, buildOutputs or sandbox"
                )
            })?;
            match crate::agents::client::request(
                storage,
                Request::DiskClean {
                    server: None,
                    action,
                },
            )? {
                Reply::DiskCleaned(cleaned) => {
                    Ok(json!({"freed": cleaned.freed, "summary": cleaned.summary}))
                }
                _ => anyhow::bail!("Unexpected cleanup reply"),
            }
        }),
        "server.rules.get" => rules(storage, params, None),
        "server.rules.set" => {
            text(params, "rules").and_then(|text| rules(storage, params, Some(text)))
        }
        "server.logins" => Ok(json!({"logins": logins::status(storage)})),
        "server.login.start" => text(params, "tool")
            .and_then(|tool| logins::start(&tool))
            .and_then(|code| Ok(serde_json::to_value(code)?)),
        "server.login.token" => text(params, "tool").and_then(|tool| {
            logins::token(storage, &tool, &text(params, "token")?).map(|()| json!({}))
        }),
        "server.logout" => text(params, "tool")
            .and_then(|tool| logins::logout(storage, &tool))
            .map(|()| json!({})),
        _ => return None,
    };
    Some(result)
}

fn text(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("Invalid or missing parameter `{key}`"))
}

/// An optional boolean, false when absent.
fn flag(params: &Value, key: &str) -> Result<bool> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => anyhow::bail!("Invalid parameter `{key}`"),
    }
}

fn optional(params: &Value, key: &str) -> Result<Option<String>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => anyhow::bail!("Invalid parameter `{key}`"),
    }
}

/// Answers `{}` at once; the work's outcome shows up in `server.repos`.
fn background(
    name: &str,
    storage: &Storage,
    work: impl FnOnce(&Storage) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let storage = storage.clone();
    thread::Builder::new().name(name.into()).spawn(move || {
        if let Err(error) = work(&storage) {
            eprintln!("Server repositories: {error:#}");
        }
    })?;
    Ok(json!({}))
}

/// `owner/name` is a server repository's base clone; anything else is a path
/// from `agents.repositories`.
fn repository(storage: &Storage, params: &Value) -> Result<PathBuf> {
    let repository = text(params, "repository")?;
    let repository = repository.trim();
    ensure!(!repository.is_empty(), "Choose a repository");
    if !repository.starts_with(['/', '~', '.']) && repos::validate(repository).is_ok() {
        let path = repos::path(storage, repository)?;
        ensure!(
            path.join(".git").is_dir(),
            "{repository} is not cloned on this server yet"
        );
        return Ok(path);
    }
    Ok(PathBuf::from(repository))
}

fn rules(storage: &Storage, params: &Value, rules: Option<String>) -> Result<Value> {
    let set = rules.is_some();
    match super::super::rules::request(storage, &repository(storage, params)?, rules)? {
        Reply::RepositoryRules { .. } if set => Ok(json!({})),
        Reply::RepositoryRules { rules, .. } => Ok(json!({"rules": rules})),
        _ => anyhow::bail!("Unexpected repository rules reply"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path, process::Command};

    fn call(storage: &Storage, method: &str, params: Value) -> Result<Value> {
        dispatch(storage, method, &params).context("Method not handled")?
    }

    fn git(root: &Path, args: &[&str]) -> Result<()> {
        crate::process::checked(
            Command::new("git").arg("-C").arg(root).args(args),
            &crate::process::Cancel::default(),
        )?;
        Ok(())
    }

    #[test]
    fn server_methods_need_server_mode_except_rules() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        assert!(dispatch(&storage, "sessions.list", &json!({})).is_none());
        for method in [
            "server.repos",
            "server.login.start",
            "server.logout",
            "server.disk",
            "server.disk.clean",
        ] {
            let error = call(&storage, method, json!({"tool": "codex"}))
                .err()
                .context("Expected a refusal")?;
            assert!(error.to_string().contains("not a difu server"), "{method}");
        }
        let checkout = dir.path().join("checkout");
        fs::create_dir(&checkout)?;
        git(&checkout, &["init", "-q"])?;
        let path = json!(checkout.to_str().context("UTF-8 path")?);
        call(
            &storage,
            "server.rules.set",
            json!({"repository": path, "rules": "Use tabs."}),
        )?;
        assert_eq!(
            call(&storage, "server.rules.get", json!({"repository": path}))?,
            json!({"rules": "Use tabs."})
        );
        Ok(())
    }

    /// Answers the next service request on the socket with `reply` and
    /// returns the request.
    fn answer(storage: &Storage, reply: Reply) -> Result<thread::JoinHandle<Result<Request>>> {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixListener,
        };
        let listener = UnixListener::bind(crate::agents::server::socket(storage)?)?;
        Ok(thread::spawn(move || -> Result<Request> {
            let (mut stream, _) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            serde_json::to_writer(&mut stream, &reply)?;
            stream.write_all(b"\n")?;
            Ok(serde_json::from_str(&line)?)
        }))
    }

    #[test]
    fn disk_status_and_cleanups_reach_the_service_with_the_documented_shapes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        storage.update_config(|config| config.server.enabled = true)?;
        let report = disk::Report {
            path: "/srv/difu/agents".into(),
            total: 100,
            used: 96,
            free: 4,
            status: disk::Status::Critical,
            measured_at: Some(1),
            measuring: true,
            breakdown: disk::Breakdown {
                sessions: vec![disk::SessionUsage {
                    id: "1".into(),
                    title: "Task".into(),
                    archived: true,
                    active: false,
                    bytes: 50,
                    build_outputs: 20,
                }],
                ..disk::Breakdown::default()
            },
        };
        let served = answer(&storage, Reply::Disk(report))?;
        let value = call(&storage, "server.disk", json!({"refresh": true}))?;
        let request = served.join().map_err(|_| anyhow::anyhow!("socket"))??;
        assert!(matches!(
            request,
            Request::Disk {
                server: None,
                refresh: true
            }
        ));
        assert_eq!(value.get("status"), Some(&json!("critical")));
        assert_eq!(value.get("measuredAt"), Some(&json!(1)));
        assert_eq!(value.get("measuring"), Some(&json!(true)));
        assert_eq!(
            value.pointer("/breakdown/sessions/0/buildOutputs"),
            Some(&json!(20))
        );
        for key in ["repositories", "attachments", "caches", "sandbox", "other"] {
            assert!(
                value.pointer(&format!("/breakdown/{key}")).is_some(),
                "{key}"
            );
        }
        fs::remove_file(crate::agents::server::socket(&storage)?)?;
        let served = answer(
            &storage,
            Reply::DiskCleaned(disk::Cleaned {
                freed: 7,
                summary: "Done".into(),
            }),
        )?;
        let value = call(
            &storage,
            "server.disk.clean",
            json!({"action": "archivedWorktrees"}),
        )?;
        let request = served.join().map_err(|_| anyhow::anyhow!("socket"))??;
        assert!(matches!(
            request,
            Request::DiskClean {
                server: None,
                action: disk::Action::ArchivedWorktrees
            }
        ));
        assert_eq!(value, json!({"freed": 7, "summary": "Done"}));
        for params in [json!({"action": "everything"}), json!({})] {
            assert!(call(&storage, "server.disk.clean", params).is_err());
        }
        assert!(call(&storage, "server.disk", json!({"refresh": "yes"})).is_err());
        Ok(())
    }

    #[test]
    fn server_repository_rules_logins_and_repos_shapes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        storage.update_config(|config| config.server.enabled = true)?;
        assert_eq!(
            call(&storage, "server.repos", json!({}))?,
            json!({"repositories": [], "refreshMinutes": 10})
        );
        let clone = repos::path(&storage, "owner/repo")?;
        let origin = dir.path().join("origin.git");
        fs::create_dir_all(clone.parent().context("Missing owner directory")?)?;
        git(dir.path(), &["init", "-q", "--bare", "origin.git"])?;
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                origin.to_str().context("UTF-8 path")?,
                clone.to_str().context("UTF-8 path")?,
            ],
        )?;
        assert_eq!(
            call(&storage, "server.repos.add", json!({"name": "owner/repo"}))?,
            json!({})
        );
        let listed = call(&storage, "server.repos", json!({}))?;
        assert_eq!(
            listed.pointer("/repositories/0/name"),
            Some(&json!("owner/repo"))
        );
        assert_eq!(listed.pointer("/repositories/0/cloned"), Some(&json!(true)));
        for key in ["path", "fetchedAt", "error"] {
            assert!(
                listed
                    .pointer("/repositories/0")
                    .and_then(|r| r.get(key))
                    .is_some(),
                "{key}"
            );
        }
        assert!(call(&storage, "server.repos.add", json!({"name": "../x"})).is_err());
        assert!(call(&storage, "server.repos.add", json!({})).is_err());
        assert!(
            call(
                &storage,
                "server.repos.refresh",
                json!({"name": "owner/other"})
            )
            .is_err()
        );

        call(
            &storage,
            "server.rules.set",
            json!({"repository": "owner/repo", "rules": "Run cargo test -p core."}),
        )?;
        assert_eq!(
            call(
                &storage,
                "server.rules.get",
                json!({"repository": "owner/repo"})
            )?,
            json!({"rules": "Run cargo test -p core."})
        );
        assert_eq!(
            storage
                .load_config()?
                .repository_rules
                .get(&clone.canonicalize()?)
                .map(String::as_str),
            Some("Run cargo test -p core.")
        );
        assert!(
            call(
                &storage,
                "server.rules.get",
                json!({"repository": "owner/missing"})
            )
            .is_err()
        );

        assert_eq!(
            call(
                &storage,
                "server.repos.remove",
                json!({"name": "owner/repo"})
            )?,
            json!({})
        );
        assert!(storage.load_config()?.server.repositories.is_empty());
        assert!(clone.join(".git").is_dir());
        assert_eq!(
            call(&storage, "server.repos.refresh", json!({}))?,
            json!({})
        );

        assert_eq!(
            call(
                &storage,
                "server.login.token",
                json!({"tool": "claude", "token": "sk-ant-oat01-abcdefghijklmnop"}),
            )?,
            json!({})
        );
        let env = logins::env_path(&storage)?;
        assert!(fs::read_to_string(&env)?.contains("CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-"));
        assert!(call(&storage, "server.login.start", json!({"tool": "claude"})).is_err());
        assert_eq!(
            call(&storage, "server.logout", json!({"tool": "claude"}))?,
            json!({})
        );
        assert!(!fs::read_to_string(&env)?.contains("CLAUDE_CODE_OAUTH_TOKEN"));
        Ok(())
    }
}
