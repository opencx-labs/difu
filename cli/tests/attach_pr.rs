mod support;

use anyhow::{Context, Result, ensure};
use difu::{
    agents::{Job, Launch, Reply, Request, Session, Status, client, server},
    storage::{self, Storage},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn isolated(command: &mut Command, root: &Path) {
    command
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"));
}

#[test]
fn command_attaches_cross_repo_pr_without_a_terminal_or_conversation_reset() -> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix("difu-pr-")
        .tempdir_in("/tmp")?;
    let root = temp.path();
    #[cfg(target_os = "macos")]
    let storage = Storage {
        config: root.join("Library/Application Support/difu/config.json"),
        cache: root.join("Library/Caches/difu"),
    };
    #[cfg(not(target_os = "macos"))]
    let storage = Storage {
        config: root.join("config/difu/config.json"),
        cache: root.join("cache/difu"),
    };
    let mut session = Session::new(
        "existing".into(),
        Job::Coding(Launch {
            repository: root.into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: String::new(),
            model: None,
            effort: None,
        }),
    );
    session.status = Status::Idle;
    session.thread_id = Some("original-codex-thread".into());
    session.note("agentMessage", "Existing conversation");
    fs::write(
        server::home(&storage)?.join("existing.json"),
        serde_json::to_vec(&session)?,
    )?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let gh = bin.join("gh");
    fs::write(
        &gh,
        r#"#!/usr/bin/env python3
import json, sys
if sys.argv[1:3] == ['pr', 'view']:
    print(json.dumps({'url': sys.argv[3], 'state': 'OPEN', 'isDraft': False,
        'mergeable': 'MERGEABLE', 'headRefName': 'feature', 'headRefOid': 'abc',
        'updatedAt': '2026-10-27T00:00:00Z', 'title': 'Cross-repo fixture'}))
else:
    print('[]')
"#,
    )?;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").context("PATH")?,
    )))?;
    let mut service = support::Service::start(&storage, |command| {
        isolated(command, root);
        command.env("PATH", &path);
    })?;
    let attach = |url: &str, id: &str| -> Result<std::process::Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_difu"));
        isolated(&mut command, root);
        Ok(command
            .args(["attach-pr", url, "--session", id])
            .current_dir(root)
            .output()?)
    };
    // The same number in two repositories stays distinct; repeated calls deduplicate.
    for repo in ["other/first", "other/second", "other/first"] {
        let url = format!("https://github.com/{repo}/pull/42");
        let output = attach(&url, "existing")?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8(output.stdout)?.contains(&url));
    }
    let saved: Value = serde_json::from_slice(&fs::read(
        storage
            .cache
            .join("session-pr-refresh")
            .join(format!("{}.json", storage::hash("existing"))),
    )?)?;
    let links = saved
        .get("links")
        .and_then(Value::as_array)
        .context("links")?;
    assert_eq!(links.len(), 2);
    for repo in ["first", "second"] {
        assert!(links.iter().any(|link| link.get("pr")
            == Some(&json!({"owner":"other", "repo":repo, "number":42}))
            && link.get("attached") == Some(&json!(true))));
    }
    let Reply::Session(after) = client::request(
        &storage,
        Request::Read {
            id: "existing".into(),
            version: None,
        },
    )?
    else {
        anyhow::bail!("Missing session");
    };
    assert_eq!(after.thread_id, session.thread_id);
    assert_eq!(after.workspace, session.workspace);
    assert!(!after.pr_tools);
    assert!(
        after
            .entries
            .iter()
            .any(|entry| entry.text == "Existing conversation")
    );
    let missing = attach("https://github.com/other/repo/pull/42", "missing")?;
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stderr)?.contains("Session not found"));
    service.stop()?;
    Ok(())
}

#[test]
fn invalid_attachment_is_rejected_before_service_startup() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_difu"));
    isolated(&mut command, temp.path());
    let output = command
        .args(["attach-pr", "42", "--session", "existing"])
        .output()?;
    assert!(!output.status.success());
    assert!(!String::from_utf8(output.stderr)?.contains("interactive terminal"));
    assert_eq!(fs::read_dir(temp.path())?.count(), 0);
    Ok(())
}
