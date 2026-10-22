//! Native permission controls: protocol fixtures never run inference.
mod support;
use anyhow::{Context, Result, ensure};
use difu::{
    agents::{
        Control, Job, Launch, Reply, Request, Session, Status, client,
        permissions::{Action, CodexPreset, Scope, SettingKind, SettingsEdit, Snapshot},
    },
    process::{self, Cancel},
    storage::Storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
fn git(root: &Path, args: &[&str]) -> Result<()> {
    process::checked(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args),
        &Cancel::default(),
    )?;
    Ok(())
}
fn read(storage: &Storage, id: &str) -> Result<Session> {
    match client::request(
        storage,
        Request::Read {
            id: id.into(),
            version: None,
        },
    )? {
        Reply::Session(s) => Ok(*s),
        _ => anyhow::bail!("Expected session"),
    }
}
fn wait(storage: &Storage, id: &str, predicate: impl Fn(&Session) -> bool) -> Result<Session> {
    let start = Instant::now();
    loop {
        let s = read(storage, id)?;
        if predicate(&s) {
            return Ok(s);
        }
        ensure!(s.status != Status::Failed, "Session failed: {:?}", s.error);
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "Timed out: {:?}",
            s.status
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn control(storage: &Storage, id: &str, control: Control) -> Result<()> {
    client::request(
        storage,
        Request::Control {
            id: id.into(),
            control,
        },
    )?;
    Ok(())
}
fn permissions(storage: &Storage, id: &str, action: Action) -> Result<Snapshot> {
    match client::request(
        storage,
        Request::Permissions {
            id: id.into(),
            action,
        },
    )? {
        Reply::Permissions { snapshot } => Ok(snapshot),
        _ => anyhow::bail!("Expected permissions"),
    }
}
fn launch(storage: &Storage, root: &Path, model: &str, prompt: &str) -> Result<String> {
    match client::request(
        storage,
        Request::Launch {
            job: Box::new(Job::Coding(Launch {
                repository: root.into(),
                base: "HEAD".into(),
                isolated: true,
                prompt: prompt.into(),
                model: Some(model.into()),
                effort: Some("high".into()),
            })),
        },
    )? {
        Reply::Launched(id) => Ok(id),
        _ => anyhow::bail!("Expected launch"),
    }
}
fn message(storage: &Storage, id: &str, text: &str) -> Result<()> {
    control(
        storage,
        id,
        Control::Message {
            text: text.into(),
            queue: false,
            skills: vec![],
            attachments: vec![],
        },
    )
}
fn frames(root: &Path, file: &str) -> Result<Vec<Value>> {
    fs::read_to_string(root.join(file))?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
fn setup(root: &Path) -> Result<(Storage, support::Service, std::path::PathBuf)> {
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    git(&repo, &["remote", "add", "origin", "."])?;
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    for (name, content) in [
        ("codex", include_str!("fixtures/agent_codex.py")),
        ("claude", include_str!("fixtures/agent_claude.py")),
        ("gh", "#!/bin/sh\nprintf '[]\\n'\n"),
    ] {
        let file = bin.join(name);
        fs::write(&file, content)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700))?;
    }
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").context("PATH")?,
    )))?;
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    let service = support::Service::start(&storage, |command| {
        command
            .env("PATH", path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("CLAUDE_CONFIG_DIR", root.join("claude-config"));
    })?;
    Ok((storage, service, repo))
}

#[test]
fn codex_completed_plan_choices_require_approval_and_preserve_workspace() -> Result<()> {
    use difu::agents::plan::Choice;
    for choice in [
        Choice::ContinuePlanning,
        Choice::Implement,
        Choice::FreshContext,
    ] {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("distinct-threads"), "")?;
        let (storage, _service, repo) = setup(temp.path())?;
        let id = launch(&storage, &repo, "fixture-model", "")?;
        wait(&storage, &id, |s| s.status == Status::Idle)?;
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into(),
            },
        )?;
        message(&storage, &id, "fixture final plan")?;
        let before = wait(&storage, &id, |s| s.plan_prompt.is_some())?;
        let proposal = before.plan_prompt.as_ref().context("Missing proposal")?;
        let workspace = before.workspace.as_ref().context("workspace")?;
        fs::write(workspace.join("marker.txt"), "unchanged\n")?;
        fs::write(workspace.join("dirty.txt"), "preserve me\n")?;
        let count = frames(temp.path(), "protocol.jsonl")?
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count();
        assert_eq!(count, 1, "Offering must never start execution");
        control(
            &storage,
            &id,
            Control::DecidePlan {
                plan_id: proposal.id.clone(),
                choice,
            },
        )?;
        let after = wait(&storage, &id, |s| {
            s.status == Status::Idle && s.plan_prompt.is_none()
        })?;
        assert_eq!(after.id, before.id);
        assert_eq!(after.workspace, before.workspace);
        assert_eq!(after.model, before.model);
        assert_eq!(after.effort, before.effort);
        assert_eq!(after.permissions, before.permissions);
        assert!(
            before
                .entries
                .iter()
                .all(|e| after.entries.iter().any(|new| new.id == e.id))
        );
        assert_eq!(
            fs::read_to_string(workspace.join("dirty.txt"))?,
            "preserve me\n"
        );
        let wire = frames(temp.path(), "protocol.jsonl")?;
        let starts = wire
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .collect::<Vec<_>>();
        if choice == Choice::ContinuePlanning {
            assert_eq!(starts.len(), 1);
            assert_eq!(after.thread_id, before.thread_id);
            assert_eq!(after.permission_state.mode.as_deref(), Some("plan"));
            assert_eq!(
                fs::read_to_string(workspace.join("marker.txt"))?,
                "unchanged\n"
            );
        } else {
            assert_eq!(starts.len(), 2);
            assert_eq!(after.permission_state.mode.as_deref(), Some("default"));
            assert_eq!(
                fs::read_to_string(workspace.join("marker.txt"))?,
                "implemented\n"
            );
            let execution = starts.last().context("execution")?;
            assert!(
                execution
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(&proposal.text))
            );
            assert_eq!(
                execution
                    .pointer("/params/input")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                Some(1),
                "Fresh context may not replay chat"
            );
            assert_eq!(
                after.thread_id == before.thread_id,
                choice == Choice::Implement
            );
            assert!(after.entries.iter().any(|entry| entry.kind == "userMessage"
                && entry.text
                    == if choice == Choice::FreshContext {
                        "Clear context and implement this plan."
                    } else {
                        "Implement this plan."
                    }));
        }
        assert!(
            control(
                &storage,
                &id,
                Control::DecidePlan {
                    plan_id: proposal.id.clone(),
                    choice
                }
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn codex_plan_preparation_failure_retains_original_context_and_can_retry() -> Result<()> {
    use difu::agents::plan::Choice;
    for (choice, marker) in [
        (Choice::Implement, "reject-mode-update"),
        (Choice::FreshContext, "reject-mode-update"),
        (Choice::FreshContext, "reject-plan-start"),
    ] {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("distinct-threads"), "")?;
        let (storage, _service, repo) = setup(temp.path())?;
        let id = launch(&storage, &repo, "fixture-model", "")?;
        wait(&storage, &id, |s| s.status == Status::Idle)?;
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into(),
            },
        )?;
        message(&storage, &id, "fixture final plan")?;
        let before = wait(&storage, &id, |s| s.plan_prompt.is_some())?;
        let proposal = before.plan_prompt.as_ref().context("proposal")?;
        fs::write(temp.path().join(marker), "")?;
        assert!(
            control(
                &storage,
                &id,
                Control::DecidePlan {
                    plan_id: proposal.id.clone(),
                    choice
                }
            )
            .is_err()
        );
        let failed = read(&storage, &id)?;
        assert_eq!(failed.plan_prompt, before.plan_prompt);
        assert_eq!(failed.thread_id, before.thread_id);
        assert_eq!(failed.workspace, before.workspace);
        assert_eq!(failed.permission_state.mode.as_deref(), Some("plan"));
        assert_eq!(
            frames(temp.path(), "protocol.jsonl")?
                .iter()
                .filter(|v| v.get("method") == Some(&json!("turn/start")))
                .count(),
            1
        );
        fs::remove_file(temp.path().join(marker))?;
        control(
            &storage,
            &id,
            Control::DecidePlan {
                plan_id: proposal.id.clone(),
                choice,
            },
        )?;
        let done = wait(&storage, &id, |s| {
            s.status == Status::Idle && s.plan_prompt.is_none()
        })?;
        assert_eq!(done.id, before.id);
        assert_eq!(done.workspace, before.workspace);
    }
    Ok(())
}

#[test]
fn codex_plan_refinement_checklist_and_failed_turns_do_not_execute_stale_choices() -> Result<()> {
    use difu::agents::plan::Choice;
    let temp = tempfile::tempdir()?;
    let (storage, _service, repo) = setup(temp.path())?;
    let id = launch(&storage, &repo, "fixture-model", "")?;
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    message(&storage, &id, "fixture checklist only")?;
    assert!(
        wait(&storage, &id, |s| s.status == Status::Idle)?
            .plan_prompt
            .is_none()
    );
    message(&storage, &id, "fixture final plan")?;
    let before = wait(&storage, &id, |s| s.plan_prompt.is_some())?;
    let old = before.plan_prompt.context("proposal")?;
    message(&storage, &id, "chat only refine this plan")?;
    assert!(
        wait(&storage, &id, |s| s.status == Status::Idle)?
            .plan_prompt
            .is_none()
    );
    message(&storage, &id, "fixture final plan")?;
    let newer = wait(&storage, &id, |s| s.plan_prompt.is_some())?
        .plan_prompt
        .context("new proposal")?;
    assert_ne!(old.id, newer.id);
    assert!(
        control(
            &storage,
            &id,
            Control::DecidePlan {
                plan_id: old.id,
                choice: Choice::Implement
            }
        )
        .is_err()
    );
    assert_eq!(read(&storage, &id)?.plan_prompt, Some(newer.clone()));
    control(
        &storage,
        &id,
        Control::DecidePlan {
            plan_id: newer.id,
            choice: Choice::ContinuePlanning,
        },
    )?;
    permissions(&storage, &id, Action::Refresh)?;
    assert!(read(&storage, &id)?.plan_prompt.is_none());
    message(&storage, &id, "fixture final plan failed")?;
    let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
    assert!(failed.plan_prompt.is_none());
    Ok(())
}

#[test]
fn codex_plan_unknown_send_ack_is_consumed_and_never_replayed() -> Result<()> {
    use difu::agents::plan::Choice;
    for choice in [Choice::Implement, Choice::FreshContext] {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("distinct-threads"), "")?;
        let (storage, _service, repo) = setup(temp.path())?;
        let id = launch(&storage, &repo, "fixture-model", "")?;
        wait(&storage, &id, |s| s.status == Status::Idle)?;
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into(),
            },
        )?;
        message(&storage, &id, "fixture final plan")?;
        let before = wait(&storage, &id, |s| s.plan_prompt.is_some())?;
        let proposal = before.plan_prompt.context("proposal")?;
        fs::write(temp.path().join("drop-first-turn-before-ack"), "")?;
        assert!(
            control(
                &storage,
                &id,
                Control::DecidePlan {
                    plan_id: proposal.id.clone(),
                    choice
                }
            )
            .is_err()
        );
        let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
        assert!(failed.plan_prompt.is_none());
        assert!(failed.permission_state.codex_turn_attempted);
        assert_eq!(
            failed.thread_id == before.thread_id,
            choice == Choice::Implement
        );
        let accepted: Value =
            serde_json::from_slice(&fs::read(temp.path().join("accepted-first-turn.json"))?)?;
        assert_eq!(
            accepted.get("threadId").and_then(Value::as_str),
            failed.thread_id.as_deref()
        );
        assert!(failed.entries.iter().any(|e| {
            e.kind == "unsent or unacknowledged"
                && e.data
                    .get("wire_text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(&proposal.text))
        }));
        permissions(&storage, &id, Action::Refresh)?;
        let restored = read(&storage, &id)?;
        assert_eq!(restored.thread_id, failed.thread_id);
        assert!(restored.plan_prompt.is_none());
        assert_eq!(
            frames(temp.path(), "protocol.jsonl")?
                .iter()
                .filter(|v| v.get("method") == Some(&json!("turn/start")))
                .count(),
            2
        );
        assert!(
            control(
                &storage,
                &id,
                Control::DecidePlan {
                    plan_id: proposal.id,
                    choice
                }
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn codex_plan_legacy_backfill_and_dismissal_survive_restart_without_a_turn() -> Result<()> {
    use difu::agents::plan::Choice;
    let temp = tempfile::tempdir()?;
    let (storage, mut service, repo) = setup(temp.path())?;
    let id = launch(&storage, &repo, "fixture-model", "")?;
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    message(&storage, &id, "fixture final plan")?;
    let before = wait(&storage, &id, |s| s.plan_prompt.is_some())?;
    service.stop()?;
    update_fixture_session(&storage, &id, |s| {
        s.plan_prompt = None;
        s.plan_handled = None;
        for entry in &mut s.entries {
            if entry.kind == "plan" && entry.data.get("type") == Some(&json!("plan")) {
                if let Some(data) = entry.data.as_object_mut() {
                    for key in [
                        "difuPlanObserved",
                        "difuPlanThread",
                        "difuPlanTurn",
                        "difuPlanSucceeded",
                    ] {
                        data.remove(key);
                    }
                }
                entry.text = entry.data.to_string().into();
            }
        }
    })?;
    service = restart_fixture(&storage, temp.path())?;
    let restored = read(&storage, &id)?;
    let proposal = restored
        .plan_prompt
        .as_ref()
        .context("backfilled proposal")?;
    assert_eq!(proposal.text, before.plan_prompt.context("original")?.text);
    let count = frames(temp.path(), "protocol.jsonl")?.len();
    control(
        &storage,
        &id,
        Control::DecidePlan {
            plan_id: proposal.id.clone(),
            choice: Choice::ContinuePlanning,
        },
    )?;
    assert_eq!(
        frames(temp.path(), "protocol.jsonl")?.len(),
        count,
        "Stay must not initialize a provider after restart"
    );
    service.stop()?;
    let _service = restart_fixture(&storage, temp.path())?;
    let dismissed = read(&storage, &id)?;
    assert!(dismissed.plan_prompt.is_none());
    assert_eq!(dismissed.thread_id, before.thread_id);
    assert_eq!(dismissed.workspace, before.workspace);
    Ok(())
}

#[test]
fn codex_plan_becomes_available_after_last_local_question_is_skipped() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (storage, _service, repo) = setup(temp.path())?;
    let id = launch(&storage, &repo, "fixture-model", "")?;
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    message(&storage, &id, "fixture final plan with question")?;
    let before = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.pending_question_count() == 1
    })?;
    assert!(before.plan_prompt.is_none());
    let request = before
        .pending
        .first()
        .context("pending question")?
        .id
        .clone();
    let count = frames(temp.path(), "protocol.jsonl")?.len();
    control(
        &storage,
        &id,
        Control::AnswerQuestion {
            request,
            question: "0".into(),
            answer: None,
        },
    )?;
    let after = read(&storage, &id)?;
    assert!(after.plan_prompt.is_some());
    assert_eq!(after.pending_question_count(), 0);
    assert_eq!(
        frames(temp.path(), "protocol.jsonl")?.len(),
        count,
        "Skip must not send another native turn"
    );
    Ok(())
}

#[test]
fn codex_plan_choice_applies_queued_preset_before_execution_without_hiding_menu() -> Result<()> {
    use difu::agents::plan::Choice;
    for choice in [Choice::Implement, Choice::FreshContext] {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("distinct-threads"), "")?;
        let (storage, _service, repo) = setup(temp.path())?;
        let id = launch(&storage, &repo, "fixture-model", "")?;
        wait(&storage, &id, |s| s.status == Status::Idle)?;
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into(),
            },
        )?;
        permissions(
            &storage,
            &id,
            Action::SetPreset {
                preset: CodexPreset::ReadOnly,
            },
        )?;
        message(&storage, &id, "fixture final plan deferred")?;
        wait(&storage, &id, |s| {
            s.turn_id.is_some()
                && s.entries
                    .iter()
                    .any(|e| e.kind == "plan" && e.data.get("type") == Some(&json!("plan")))
        })?;
        let pending = permissions(
            &storage,
            &id,
            Action::SetPreset {
                preset: CodexPreset::Auto,
            },
        )?;
        assert_eq!(pending.pending_preset, Some(CodexPreset::Auto));
        fs::write(temp.path().join("finish-deferred-plan"), "")?;
        client::request(&storage, Request::Usage { id: id.clone() })?;
        let plan = wait(&storage, &id, |s| s.plan_prompt.is_some())?;
        assert_eq!(
            plan.permission_state.pending_preset,
            Some(CodexPreset::Auto)
        );
        let proposal = plan.plan_prompt.context("proposal")?;
        control(
            &storage,
            &id,
            Control::DecidePlan {
                plan_id: proposal.id,
                choice,
            },
        )?;
        let done = wait(&storage, &id, |s| {
            s.status == Status::Idle && s.plan_prompt.is_none()
        })?;
        assert_eq!(done.permission_state.pending_preset, None);
        assert_eq!(done.permission_state.preset, Some(CodexPreset::Auto));
        assert_eq!(
            done.permissions.get("approvalsReviewer"),
            Some(&json!("auto_review"))
        );
        assert_eq!(
            done.permissions.pointer("/sandbox/type"),
            Some(&json!("workspaceWrite"))
        );
        assert_eq!(
            fs::read_to_string(done.workspace.context("workspace")?.join("marker.txt"))?,
            "implemented\n"
        );
    }
    Ok(())
}
#[test]
fn codex_permissions_use_native_rpc_and_mode_does_not_send_a_turn() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permissions-codex-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only initial")?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.thread_id.is_some()
    })?;
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    assert!(snapshot.modes.iter().all(|c| c.available));
    assert!(snapshot.presets.iter().all(|c| c.available));
    let before = frames(root, "protocol.jsonl")?
        .iter()
        .filter(|v| v.get("method") == Some(&json!("turn/start")))
        .count();
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    assert_eq!(snapshot.active_mode.as_deref(), Some("plan"));
    assert!(snapshot.pending_mode.is_none());
    assert_eq!(
        frames(root, "protocol.jsonl")?
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        before
    );
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Auto,
        },
    )?;
    assert_eq!(snapshot.active_preset, Some(CodexPreset::Auto));
    assert_eq!(snapshot.active_mode.as_deref(), Some("plan"));
    assert!(snapshot.pending_mode.is_none());
    message(&storage, &id, "chat only plan")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.mode.as_deref() == Some("plan")
    })?;
    let wire = frames(root, "protocol.jsonl")?;
    let turn = wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("turn/start")))
        .context("turn")?;
    assert_eq!(
        turn.pointer("/params/collaborationMode"),
        Some(
            &json!({"mode":"plan","settings":{"model":"fixture-model","reasoning_effort":"high","developer_instructions":null}})
        )
    );
    message(&storage, &id, "wait pending preset")?;
    wait(&storage, &id, |s| s.turn_id.is_some())?;
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::ReadOnly,
        },
    )?;
    assert_eq!(snapshot.pending_preset, Some(CodexPreset::ReadOnly));
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Ask,
        },
    )?;
    assert_eq!(snapshot.pending_preset, Some(CodexPreset::Ask));
    control(
        &storage,
        &id,
        Control::Message {
            text: "chat only queued after permissions".into(),
            queue: true,
            skills: vec![],
            attachments: vec![],
        },
    )?;
    control(&storage, &id, Control::InterruptAndSend)?;
    let final_state = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.pending_preset.is_none()
    })?;
    assert_eq!(final_state.permission_state.preset, Some(CodexPreset::Ask));
    assert_eq!(initial.workspace, final_state.workspace);
    fs::write(root.join("restrict-permissions"), "")?;
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    assert!(
        !snapshot
            .presets
            .iter()
            .find(|c| c.id == "Full")
            .context("Full")?
            .available
    );
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetPreset {
                preset: CodexPreset::Full
            }
        )
        .is_err()
    );
    service.stop()?;
    Ok(())
}
#[test]
fn claude_live_modes_preserve_approvals_and_reload_without_inference() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permissions-claude-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "claude tool")?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Waiting && !s.pending.is_empty()
    })?;
    let approval = initial.pending.first().context("approval")?.id.clone();
    let mode = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "auto".into(),
        },
    )?;
    assert_eq!(mode.active_mode.as_deref(), Some("auto"));
    assert!(
        read(&storage, &id)?
            .pending
            .iter()
            .any(|p| p.id == approval)
    );
    assert!(
        !frames(root, "claude-protocol.jsonl")?
            .iter()
            .any(|v| v.pointer("/response/request_id") == Some(&approval))
    );
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    let revision = snapshot
        .revisions
        .get(&Scope::Session)
        .context("session revision")?
        .clone();
    let snapshot = permissions(
        &storage,
        &id,
        Action::Edit {
            edit: SettingsEdit {
                kind: SettingKind::Allow,
                scope: Scope::Session,
                before: None,
                after: Some("Read(/tmp/**)".into()),
                revision,
            },
        },
    )?;
    assert!(snapshot.reload_pending);
    assert!(read(&storage, &id)?.permission_state.reload_pending);
    let starts_before = frames(root, "claude-starts.jsonl")?.len();
    let user_before = frames(root, "claude-protocol.jsonl")?
        .iter()
        .filter(|v| v.get("type") == Some(&json!("user")))
        .count();
    control(
        &storage,
        &id,
        Control::AnswerQuestion {
            request: approval,
            question: "difu-decision".into(),
            answer: Some("Deny".into()),
        },
    )?;
    let reloaded = wait(&storage, &id, |s| {
        s.status == Status::Idle && !s.permission_state.reload_pending
    })?;
    assert_eq!(reloaded.thread_id, initial.thread_id);
    assert!(!reloaded.permission_state.denials.is_empty());
    assert_eq!(
        frames(root, "claude-protocol.jsonl")?
            .iter()
            .filter(|v| v.get("type") == Some(&json!("user")))
            .count(),
        user_before
    );
    assert_eq!(
        frames(root, "claude-starts.jsonl")?.len(),
        starts_before + 1
    );
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "bypassPermissions".into(),
        },
    )?;
    assert_eq!(snapshot.active_mode.as_deref(), Some("bypassPermissions"));
    let starts = frames(root, "claude-starts.jsonl")?;
    let args = starts
        .last()
        .and_then(|v| v.get("args"))
        .and_then(Value::as_array)
        .context("args")?;
    assert!(args.contains(&json!("--allow-dangerously-skip-permissions")));
    assert!(args.contains(&json!("--resume=fixture-claude")));
    fs::write(root.join("reject-auto"), "")?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "auto".into()
            }
        )
        .is_err()
    );
    assert_eq!(
        read(&storage, &id)?.permission_state.mode.as_deref(),
        Some("bypassPermissions")
    );
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    assert!(
        !snapshot
            .modes
            .iter()
            .find(|c| c.id == "auto")
            .context("auto")?
            .available
    );
    service.stop()?;
    Ok(())
}

#[test]
fn codex_preset_before_first_prompt_uses_exact_empty_thread_start() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permissions-first-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("no-rollout"), "")?;
    let id = launch(&storage, &repo, "fixture-model", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Auto,
        },
    )?;
    assert_eq!(snapshot.active_preset, Some(CodexPreset::Auto));
    assert!(snapshot.pending_preset.is_none());
    assert!(
        !frames(root, "protocol.jsonl")?
            .iter()
            .any(|v| v.get("method") == Some(&json!("thread/resume")))
    );
    message(&storage, &id, "chat only first prompt")?;
    let accepted = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.pending_preset.is_none()
    })?;
    assert_eq!(accepted.permission_state.preset, Some(CodexPreset::Auto));
    let wire = frames(root, "protocol.jsonl")?;
    let turn = wire
        .iter()
        .find(|v| v.get("method") == Some(&json!("turn/start")))
        .context("turn")?;
    assert!(turn.pointer("/params/approvalsReviewer").is_none());
    let configured = wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("thread/start")))
        .context("configured")?;
    assert_eq!(
        configured.pointer("/params/approvalsReviewer"),
        Some(&json!("auto_review"))
    );
    assert_eq!(
        configured.pointer("/params/sandbox"),
        Some(&json!("workspace-write"))
    );
    assert!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("thread/resume")))
            .all(|v| v.pointer("/params/excludeTurns") == Some(&json!(true))
                && v.pointer("/params/sandbox").is_none())
    );
    service.stop()?;
    Ok(())
}

#[test]
fn claude_reload_failure_retains_thread_and_never_replays_a_prompt() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permissions-reload-failure-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "initial Claude task")?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.thread_id.is_some()
    })?;
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    let revision = snapshot
        .revisions
        .get(&Scope::Session)
        .context("revision")?
        .clone();
    let before = frames(root, "claude-protocol.jsonl")?
        .iter()
        .filter(|v| v.get("type") == Some(&json!("user")))
        .count();
    fs::write(root.join("reject-settings"), "")?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::Edit {
                edit: SettingsEdit {
                    kind: SettingKind::Deny,
                    scope: Scope::Session,
                    before: None,
                    after: Some("Bash(rm:*)".into()),
                    revision
                }
            }
        )
        .is_err()
    );
    let start = Instant::now();
    loop {
        if read(&storage, &id)?.status == Status::Failed {
            break;
        }
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "Reload failure not surfaced"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let failed = read(&storage, &id)?;
    assert_eq!(failed.thread_id, initial.thread_id);
    assert!(failed.permission_state.reload_pending);
    assert_eq!(
        frames(root, "claude-protocol.jsonl")?
            .iter()
            .filter(|v| v.get("type") == Some(&json!("user")))
            .count(),
        before
    );
    fs::remove_file(root.join("reject-settings"))?;
    permissions(&storage, &id, Action::Refresh)?;
    let recovered = read(&storage, &id)?;
    assert!(!recovered.permission_state.reload_pending);
    assert_eq!(recovered.thread_id, initial.thread_id);
    assert_eq!(
        frames(root, "claude-protocol.jsonl")?
            .iter()
            .filter(|v| v.get("type") == Some(&json!("user")))
            .count(),
        before
    );
    service.stop()?;
    Ok(())
}
#[test]
fn switching_from_claude_to_unused_codex_keeps_first_preset_pending() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permissions-switch-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "initial Claude task")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.thread_id.is_some()
    })?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model".into()),
            effort: Some("high".into()),
        },
    )?;
    fs::write(root.join("no-rollout"), "")?;
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Auto,
        },
    )?;
    assert_eq!(snapshot.active_preset, Some(CodexPreset::Auto));
    assert!(snapshot.pending_preset.is_none());
    assert!(
        !frames(root, "protocol.jsonl")?
            .iter()
            .any(|v| v.get("method") == Some(&json!("thread/resume")))
    );
    assert!(!read(&storage, &id)?.permission_state.codex_turn_attempted);
    message(&storage, &id, "chat only codex first")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("claude/sonnet".into()),
            effort: Some("high".into()),
        },
    )?;
    let restored = read(&storage, &id)?;
    assert_eq!(restored.permission_state.mode.as_deref(), Some("plan"));
    service.stop()?;
    Ok(())
}
#[test]
fn restarting_empty_codex_preserves_selected_preset_without_resuming_no_rollout() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permissions-empty-restart-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("no-rollout"), "")?;
    let id = launch(&storage, &repo, "fixture-model", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Auto,
        },
    )?;
    let before = read(&storage, &id)?;
    assert!(
        before.permission_state.preset == Some(CodexPreset::Auto)
            && before.permission_state.pending_preset.is_none()
            && !before.permission_state.codex_rollout
    );
    service.stop()?;
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH")?),
    ))?;
    let mut service = support::Service::start(&storage, |command| {
        command
            .env("PATH", path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("CLAUDE_CONFIG_DIR", root.join("claude-config"));
    })?;
    message(&storage, &id, "chat only after restart")?;
    let after = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    assert_eq!(after.permission_state.preset, Some(CodexPreset::Auto));
    assert_eq!(after.workspace, before.workspace);
    assert!(
        !frames(root, "protocol.jsonl")?
            .iter()
            .any(|v| v.get("method") == Some(&json!("thread/resume")))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn ordinary_codex_turns_work_without_optional_collaboration_discovery() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-mode-discovery-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("no-collaboration-modes"), "")?;
    let id = launch(
        &storage,
        &repo,
        "fixture-model",
        "chat only unsupported discovery",
    )?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    message(&storage, &id, "chat only ordinary followup")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "chat only ordinary followup")
    })?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into()
            }
        )
        .is_err()
    );
    let wire = frames(root, "protocol.jsonl")?;
    let turns = wire
        .iter()
        .filter(|v| v.get("method") == Some(&json!("turn/start")))
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 2);
    assert!(
        turns
            .iter()
            .all(|v| v.pointer("/params/collaborationMode").is_none())
    );
    service.stop()?;
    Ok(())
}
#[test]
fn transient_claude_auto_error_can_retry_without_switching_models() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-auto-retry-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "initial Claude task")?;
    let before = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.thread_id.is_some()
    })?;
    fs::write(root.join("transient-auto"), "")?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "auto".into()
            }
        )
        .is_err()
    );
    let failed = read(&storage, &id)?;
    assert_eq!(failed.permission_state.mode, before.permission_state.mode);
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    assert!(
        snapshot
            .modes
            .iter()
            .find(|c| c.id == "auto")
            .context("auto")?
            .available
    );
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "auto".into(),
        },
    )?;
    assert_eq!(snapshot.active_mode.as_deref(), Some("auto"));
    assert_eq!(read(&storage, &id)?.model, before.model);
    service.stop()?;
    Ok(())
}
#[test]
fn legacy_worktree_model_change_preserves_read_only_and_pending_mode() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-legacy-policy-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only legacy task")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    service.stop()?;
    let saved = difu::agents::server::home(&storage)?.join(format!("{id}.json"));
    let mut legacy: Session = serde_json::from_slice(&fs::read(&saved)?)?;
    legacy.native_setup = false;
    legacy.deferred_workspace = true;
    legacy.inherited_permissions = json!({"sandbox":{"type":"workspaceWrite"},"approvalPolicy":"on-request","approvalsReviewer":"user"});
    legacy.permission_state.preset = None;
    legacy.permission_state.codex_rollout = false;
    legacy.permission_state.codex_thread_started = false;
    fs::write(&saved, serde_json::to_vec_pretty(&legacy)?)?;
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH")?),
    ))?;
    let mut service = support::Service::start(&storage, |command| {
        command
            .env("PATH", path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("CLAUDE_CONFIG_DIR", root.join("claude-config"));
    })?;
    permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::ReadOnly,
        },
    )?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model-updated".into()),
            effort: Some("medium".into()),
        },
    )?;
    let current = read(&storage, &id)?;
    assert_eq!(current.permission_state.mode.as_deref(), Some("plan"));
    assert!(current.permission_state.pending_mode.is_none());
    assert_eq!(
        current.permissions.pointer("/sandbox/type"),
        Some(&json!("readOnly"))
    );
    assert_eq!(current.permission_state.preset, Some(CodexPreset::ReadOnly));
    let wire = frames(root, "protocol.jsonl")?;
    let changed = wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("thread/resume")))
        .context("resume")?;
    assert_eq!(
        changed.pointer("/params/sandbox"),
        Some(&json!("read-only"))
    );
    assert_eq!(current.workspace, legacy.workspace);
    let native_mode: Value =
        serde_json::from_slice(&fs::read(root.join("codex-collaboration.json"))?)?;
    assert_eq!(
        native_mode.pointer("/settings/model"),
        Some(&json!("fixture-model-updated"))
    );
    assert_eq!(
        native_mode.pointer("/settings/reasoning_effort"),
        Some(&json!("medium"))
    );

    message(&storage, &id, "chat only verify selected model")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "chat only verify selected model")
    })?;
    let model_wire = frames(root, "protocol.jsonl")?;
    let model_turn = model_wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("turn/start")))
        .context("model turn")?;
    assert_eq!(
        model_turn.pointer("/params/collaborationMode/settings/model"),
        Some(&json!("fixture-model-updated"))
    );
    assert_eq!(
        model_turn.pointer("/params/collaborationMode/settings/reasoning_effort"),
        Some(&json!("medium"))
    );

    message(&storage, &id, "wait legacy queued policy")?;
    wait(&storage, &id, |s| s.turn_id.is_some())?;
    permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Ask,
        },
    )?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "default".into(),
        },
    )?;
    control(&storage, &id, Control::Interrupt)?;
    wait(&storage, &id, |s| s.turn_id.is_none())?;
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model-third".into()),
            effort: Some("low".into()),
        },
    )?;
    let queued = read(&storage, &id)?;
    assert_eq!(queued.permission_state.preset, Some(CodexPreset::ReadOnly));
    assert_eq!(
        queued.permission_state.pending_preset,
        Some(CodexPreset::Ask)
    );
    assert_eq!(queued.permission_state.mode.as_deref(), Some("default"));
    assert!(queued.permission_state.pending_mode.is_none());
    assert_eq!(
        queued.permissions.pointer("/sandbox/type"),
        Some(&json!("readOnly"))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn native_inherited_claude_bypass_is_enabled_in_the_mode_cycle() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-inherited-bypass-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("claude-inherited-bypass"), "")?;
    let id = launch(&storage, &repo, "claude/sonnet", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    let initialized = permissions(&storage, &id, Action::Refresh)?;
    assert_eq!(
        initialized.active_mode.as_deref(),
        Some("bypassPermissions")
    );
    assert!(read(&storage, &id)?.permission_state.bypass_enabled);
    let next = permissions(&storage, &id, Action::CycleMode)?;
    assert_eq!(next.active_mode.as_deref(), Some("auto"));
    assert!(
        !frames(root, "claude-protocol.jsonl")?
            .iter()
            .any(|frame| frame.get("type") == Some(&json!("user")))
    );
    service.stop()?;
    Ok(())
}
#[test]
fn canonical_codex_preset_overrides_custom_workspace_config_and_preserves_effort() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-custom-preset-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("custom-permissions"), "")?;
    let id = launch(&storage, &repo, "fixture-model", "chat only custom policy")?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    assert_eq!(
        initial.permissions.pointer("/sandbox/networkAccess"),
        Some(&json!(true))
    );
    assert!(initial.permission_state.preset.is_none());
    let snapshot = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Ask,
        },
    )?;
    assert_eq!(snapshot.active_preset, Some(CodexPreset::Ask));
    let selected = read(&storage, &id)?;
    assert_eq!(
        selected.permissions.pointer("/sandbox/networkAccess"),
        Some(&json!(false))
    );
    assert_eq!(
        selected.permissions.pointer("/sandbox/writableRoots"),
        Some(&json!([]))
    );
    assert_eq!(selected.effort.as_deref(), Some("high"));
    let wire = frames(root, "protocol.jsonl")?;
    let apply = wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("thread/resume")))
        .context("apply")?;
    assert_eq!(
        apply.pointer("/params/config/model_reasoning_effort"),
        Some(&json!("high"))
    );
    assert_eq!(
        apply.pointer("/params/config/sandbox_workspace_write/network_access"),
        Some(&json!(false))
    );
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model-updated".into()),
            effort: Some("medium".into()),
        },
    )?;
    let wire = frames(root, "protocol.jsonl")?;
    let changed = wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("thread/resume")))
        .context("changed")?;
    assert_eq!(
        changed.pointer("/params/config/model_reasoning_effort"),
        Some(&json!("medium"))
    );
    assert_eq!(
        changed.pointer("/params/config/sandbox_workspace_write/network_access"),
        Some(&json!(false))
    );
    service.stop()?;
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH")?),
    ))?;
    let mut service = support::Service::start(&storage, |command| {
        command
            .env("PATH", path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("CLAUDE_CONFIG_DIR", root.join("claude-config"));
    })?;
    let restored = permissions(&storage, &id, Action::Refresh)?;
    assert_eq!(restored.active_preset, Some(CodexPreset::Ask));
    let wire = frames(root, "protocol.jsonl")?;
    let resumed = wire
        .iter()
        .rev()
        .find(|v| {
            v.get("method") == Some(&json!("thread/resume"))
                && v.pointer("/params/config/model_reasoning_effort").is_some()
        })
        .context("resumed")?;
    assert_eq!(
        resumed.pointer("/params/config/model_reasoning_effort"),
        Some(&json!("medium"))
    );
    assert_eq!(
        resumed.pointer("/params/config/sandbox_workspace_write/network_access"),
        Some(&json!(false))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn loaded_codex_preset_and_model_changes_release_then_resume_same_thread() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-loaded-policy-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only loaded policy")?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    let before = frames(root, "protocol.jsonl")?
        .iter()
        .filter(|v| v.get("method") == Some(&json!("turn/start")))
        .count();
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    assert_eq!(
        permissions(
            &storage,
            &id,
            Action::SetPreset {
                preset: CodexPreset::ReadOnly
            }
        )?
        .active_preset,
        Some(CodexPreset::ReadOnly)
    );
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "default".into(),
        },
    )?;
    let changed = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Auto,
        },
    )?;
    assert_eq!(changed.active_preset, Some(CodexPreset::Auto));
    assert_eq!(changed.active_mode.as_deref(), Some("default"));
    assert!(changed.pending_mode.is_none());
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model-updated".into()),
            effort: Some("medium".into()),
        },
    )?;
    let current = read(&storage, &id)?;
    assert_eq!(current.status, Status::Idle);
    assert_eq!(current.thread_id, initial.thread_id);
    assert_eq!(current.model.as_deref(), Some("fixture-model-updated"));
    assert_eq!(current.effort.as_deref(), Some("medium"));
    assert_eq!(
        current.permissions.get("approvalsReviewer"),
        Some(&json!("auto_review"))
    );
    assert_eq!(current.permission_state.mode.as_deref(), Some("default"));
    assert!(current.permission_state.pending_mode.is_none());
    assert!(
        current.artifact_tools
            && current.question_tools
            && current.registration_tools
            && current.peer_tools
    );
    let wire = frames(root, "protocol.jsonl")?;
    assert_eq!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        before
    );
    for (index, call) in wire
        .iter()
        .enumerate()
        .filter(|(_, v)| v.get("method") == Some(&json!("thread/unsubscribe")))
    {
        let resumed = wire
            .iter()
            .skip(index + 1)
            .find(|v| v.get("method") == Some(&json!("thread/resume")))
            .context("resumed")?;
        assert_eq!(
            resumed.pointer("/params/threadId"),
            call.pointer("/params/threadId")
        );
        assert!(
            resumed
                .pointer("/params/developerInstructions")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains("Preserve inherited guidance"))
        );
        assert_eq!(
            resumed.pointer("/params/runtimeWorkspaceRoots"),
            Some(&json!([initial.workspace]))
        );
    }
    service.stop()?;
    Ok(())
}
#[test]
fn unsupported_codex_unsubscribe_reconnects_before_policy_confirmation() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-unsubscribe-old-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only older provider")?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    fs::write(root.join("no-unsubscribe"), "")?;
    let changed = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::Auto,
        },
    )?;
    assert_eq!(changed.active_preset, Some(CodexPreset::Auto));
    assert!(changed.pending_preset.is_none());
    let reloaded = read(&storage, &id)?;
    assert_eq!(reloaded.thread_id, initial.thread_id);
    assert_eq!(
        reloaded.permissions.get("approvalsReviewer"),
        Some(&json!("auto_review"))
    );
    assert_eq!(
        frames(root, "protocol.jsonl")?
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        1
    );
    message(&storage, &id, "chat only actual auto turn")?;
    let applied = wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "chat only actual auto turn")
    })?;
    assert_eq!(
        applied.permissions.get("approvalsReviewer"),
        Some(&json!("auto_review"))
    );
    let wire = frames(root, "protocol.jsonl")?;
    let resumed = wire
        .iter()
        .rev()
        .find(|v| v.get("method") == Some(&json!("thread/resume")))
        .context("fresh resume")?;
    assert_eq!(
        resumed.pointer("/params/approvalsReviewer"),
        Some(&json!("auto_review"))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn codex_failed_resume_after_unsubscribe_recovers_without_prompt_replay() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-resume-retry-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(
        &storage,
        &repo,
        "fixture-model",
        "chat only before reload failure",
    )?;
    let initial = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::ReadOnly,
        },
    )?;
    let before = frames(root, "protocol.jsonl")?
        .iter()
        .filter(|v| v.get("method") == Some(&json!("turn/start")))
        .count();
    fs::write(root.join("fail-resume-after-unsubscribe"), "")?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetPreset {
                preset: CodexPreset::Auto
            }
        )
        .is_err()
    );
    let failed = read(&storage, &id)?;
    assert_eq!(failed.status, Status::Failed);
    assert_eq!(failed.thread_id, initial.thread_id);
    assert_eq!(
        failed.permission_state.pending_preset,
        Some(CodexPreset::Auto)
    );
    assert_eq!(
        failed.permissions.pointer("/sandbox/type"),
        Some(&json!("readOnly"))
    );
    let recovered = permissions(&storage, &id, Action::Refresh)?;
    assert_eq!(recovered.active_preset, Some(CodexPreset::Auto));
    assert!(recovered.pending_preset.is_none());
    assert_eq!(read(&storage, &id)?.status, Status::Idle);
    assert_eq!(read(&storage, &id)?.thread_id, initial.thread_id);
    assert_eq!(
        frames(root, "protocol.jsonl")?
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        before
    );
    message(&storage, &id, "chat only after explicit retry")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "chat only after explicit retry")
    })?;
    assert_eq!(
        frames(root, "protocol.jsonl")?
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        before + 1
    );
    service.stop()?;
    Ok(())
}

#[test]
fn first_codex_prompt_confirms_exact_preset_before_inference() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-first-reviewer-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("inherit-auto-reviewer"), "")?;
    let id = launch(&storage, &repo, "fixture-model", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    let configured = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::ReadOnly,
        },
    )?;
    assert_eq!(configured.active_preset, Some(CodexPreset::ReadOnly));
    assert!(configured.pending_preset.is_none());
    assert_eq!(configured.active_mode.as_deref(), Some("plan"));
    assert!(configured.pending_mode.is_none());
    assert_eq!(
        read(&storage, &id)?.permissions.get("approvalsReviewer"),
        Some(&json!("user"))
    );
    message(&storage, &id, "chat only first exact policy")?;
    let accepted = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    assert_eq!(
        accepted.permissions.get("approvalsReviewer"),
        Some(&json!("user"))
    );
    assert_eq!(
        accepted.permissions.pointer("/sandbox/type"),
        Some(&json!("readOnly"))
    );
    assert!(accepted.permission_state.pending_preset.is_none());
    assert_eq!(accepted.permission_state.mode.as_deref(), Some("plan"));
    let wire = frames(root, "protocol.jsonl")?;
    assert_eq!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        1
    );
    let turn = wire
        .iter()
        .position(|v| v.get("method") == Some(&json!("turn/start")))
        .context("turn")?;
    let configured = wire
        .iter()
        .take(turn)
        .rev()
        .find(|v| v.get("method") == Some(&json!("thread/start")))
        .context("configured thread")?;
    assert_eq!(
        configured.pointer("/params/approvalsReviewer"),
        Some(&json!("user"))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn uncertain_first_codex_turn_preserves_native_thread_without_replay() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-uncertain-turn-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    fs::write(root.join("drop-first-turn-before-ack"), "")?;
    let id = launch(
        &storage,
        &repo,
        "fixture-model",
        "chat only uncertain first request",
    )?;
    let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
    let accepted: Value =
        serde_json::from_slice(&fs::read(root.join("accepted-first-turn.json"))?)?;
    assert!(failed.permission_state.codex_turn_attempted);
    assert!(!failed.permission_state.codex_rollout);
    assert_eq!(
        accepted.get("threadId").and_then(Value::as_str),
        failed.thread_id.as_deref()
    );
    service.stop()?;
    let persisted: Session = serde_json::from_slice(&fs::read(
        difu::agents::server::home(&storage)?.join(format!("{id}.json")),
    )?)?;
    assert!(persisted.permission_state.codex_turn_attempted);
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH")?),
    ))?;
    let mut service = support::Service::start(&storage, |command| {
        command
            .env("PATH", path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("CLAUDE_CONFIG_DIR", root.join("claude-config"));
    })?;
    let restored = permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::ReadOnly,
        },
    )?;
    assert_eq!(restored.active_preset, Some(CodexPreset::ReadOnly));
    let current = read(&storage, &id)?;
    assert_eq!(current.thread_id, failed.thread_id);
    assert!(current.permission_state.codex_turn_attempted);
    let wire = frames(root, "protocol.jsonl")?;
    assert_eq!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("thread/start")))
            .count(),
        1
    );
    assert_eq!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        1
    );
    assert!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("thread/resume")))
            .all(|v| v.pointer("/params/threadId").and_then(Value::as_str)
                == failed.thread_id.as_deref())
    );
    service.stop()?;
    Ok(())
}

fn restart_fixture(storage: &Storage, root: &Path) -> Result<support::Service> {
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH")?),
    ))?;
    support::Service::start(storage, |command| {
        command
            .env("PATH", path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("CLAUDE_CONFIG_DIR", root.join("claude-config"));
    })
}
fn update_fixture_session(
    storage: &Storage,
    id: &str,
    edit: impl FnOnce(&mut Session),
) -> Result<()> {
    let path = difu::agents::server::home(storage)?.join(format!("{id}.json"));
    let mut saved: Session = serde_json::from_slice(&fs::read(&path)?)?;
    edit(&mut saved);
    fs::write(path, serde_json::to_vec_pretty(&saved)?)?;
    Ok(())
}
#[test]
fn claude_bypass_and_settings_before_first_prompt_do_not_resume_empty_id() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-claude-empty-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    permissions(&storage, &id, Action::Refresh)?;
    let original = wait(&storage, &id, |s| s.thread_id.is_some())?;
    permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "auto".into(),
        },
    )?;
    let bypass = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "bypassPermissions".into(),
        },
    )?;
    assert_eq!(bypass.active_mode.as_deref(), Some("bypassPermissions"));
    assert!(!bypass.reload_pending);
    let snapshot = permissions(&storage, &id, Action::Refresh)?;
    let revision = snapshot
        .revisions
        .get(&Scope::Session)
        .context("revision")?
        .clone();
    permissions(
        &storage,
        &id,
        Action::Edit {
            edit: SettingsEdit {
                kind: SettingKind::Ask,
                scope: Scope::Session,
                before: None,
                after: Some("Edit".into()),
                revision,
            },
        },
    )?;
    let current = read(&storage, &id)?;
    assert_eq!(current.id, original.id);
    assert_eq!(current.workspace, original.workspace);
    let starts = frames(root, "claude-starts.jsonl")?;
    assert!(starts.iter().all(|frame| {
        !frame
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|args| {
                args.iter()
                    .any(|v| v.as_str().is_some_and(|s| s.starts_with("--resume=")))
            })
    }));
    assert!(
        !frames(root, "claude-protocol.jsonl")?
            .iter()
            .any(|v| v.get("type") == Some(&json!("user")))
    );
    service.stop()?;
    Ok(())
}
#[test]
fn missing_legacy_admin_only_provider_ids_recover_without_sending_saved_draft() -> Result<()> {
    for model in ["fixture-model", "claude/sonnet"] {
        let tmp = tempfile::Builder::new()
            .prefix("difu-legacy-empty-")
            .tempdir_in("/tmp")?;
        let root = tmp.path();
        let (storage, mut service, repo) = setup(root)?;
        let id = launch(&storage, &repo, model, "")?;
        let original = wait(&storage, &id, |s| {
            s.status == Status::Idle && s.workspace_ready
        })?;
        service.stop()?;
        let stale = if model.starts_with("claude") {
            "missing-claude"
        } else {
            "missing-codex"
        };
        update_fixture_session(&storage, &id, |s| {
            s.thread_id = Some(stale.into());
            s.permission_state = Default::default();
            s.note("system", "Prepared workspace");
            s.note("error", "Previous connection failed");
            s.error = Some("Previous native resume failed".into());
            s.note("awaiting connection", "hello?");
        })?;
        let mut service = restart_fixture(&storage, root)?;
        permissions(&storage, &id, Action::Refresh)?;
        let recovered = wait(&storage, &id, |s| {
            s.status == Status::Idle && s.thread_id.as_deref() != Some(stale)
        })?;
        assert_eq!(recovered.id, original.id);
        assert_eq!(recovered.workspace, original.workspace);
        assert!(recovered.error.is_none());
        assert!(
            recovered
                .entries
                .iter()
                .any(|e| e.kind == "awaiting connection" && e.text == "hello?")
        );
        if model.starts_with("claude") {
            assert!(
                !frames(root, "claude-protocol.jsonl")?
                    .iter()
                    .any(|v| v.get("type") == Some(&json!("user")))
            );
        } else {
            assert!(
                !frames(root, "protocol.jsonl")?
                    .iter()
                    .any(|v| v.get("method") == Some(&json!("turn/start")))
            );
        }
        service.stop()?;
    }
    Ok(())
}
#[test]
fn missing_real_claude_conversation_surfaces_provider_error_without_recreating() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-legacy-history-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    service.stop()?;
    update_fixture_session(&storage, &id, |s| {
        s.thread_id = Some("missing-claude".into());
        s.permission_state = Default::default();
        s.note("userMessage", "An actual previous request");
    })?;
    let mut service = restart_fixture(&storage, root)?;
    let error = permissions(&storage, &id, Action::Refresh)
        .err()
        .context("expected missing native conversation")?
        .to_string();
    assert!(
        error.contains("No conversation found with session ID"),
        "{error}"
    );
    assert!(!error.contains("sending half is closed"), "{error}");
    assert_eq!(
        read(&storage, &id)?.thread_id.as_deref(),
        Some("missing-claude")
    );
    assert_eq!(frames(root, "claude-starts.jsonl")?.len(), 1);
    service.stop()?;
    Ok(())
}
#[test]
fn uncertain_first_claude_turn_is_persisted_and_resumed_without_replay() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-claude-uncertain-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    permissions(&storage, &id, Action::Refresh)?;
    wait(&storage, &id, |s| s.thread_id.is_some())?;
    fs::write(root.join("drop-claude-before-ack"), "")?;
    message(&storage, &id, "uncertain Claude request")?;
    let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
    assert!(failed.permission_state.claude_turn_attempted);
    assert!(!failed.permission_state.claude_turn_acknowledged);
    service.stop()?;
    let mut service = restart_fixture(&storage, root)?;
    permissions(&storage, &id, Action::Refresh)?;
    let current = read(&storage, &id)?;
    assert_eq!(current.id, failed.id);
    assert_eq!(current.workspace, failed.workspace);
    assert_eq!(current.thread_id, failed.thread_id);
    assert_eq!(
        frames(root, "claude-protocol.jsonl")?
            .iter()
            .filter(|v| v.get("type") == Some(&json!("user")))
            .count(),
        1
    );
    let starts = frames(root, "claude-starts.jsonl")?;
    assert!(
        starts
            .last()
            .and_then(|v| v.get("args"))
            .and_then(Value::as_array)
            .is_some_and(|args| args.contains(&json!("--resume=fixture-claude")))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn explicit_claude_permission_interrupt_keeps_interrupted_and_safe_audit() -> Result<()> {
    use difu::agents::permissions::PermissionEventKind as Kind;
    let tmp = tempfile::Builder::new()
        .prefix("difu-permission-stop-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "claude tool")?;
    let waiting = wait(&storage, &id, |s| {
        s.status == Status::Waiting && !s.pending.is_empty()
    })?;
    let request = waiting
        .pending
        .first()
        .context("approval")?
        .id
        .as_str()
        .context("request")?
        .to_owned();
    control(&storage, &id, Control::Interrupt)?;
    let stopped = wait(&storage, &id, |s| s.turn_id.is_none())?;
    assert_eq!(stopped.status, Status::Interrupted);
    assert!(stopped.error.is_none());
    assert_eq!(stopped.thread_id, waiting.thread_id);
    assert_eq!(stopped.workspace, waiting.workspace);
    assert!(
        stopped
            .permission_state
            .cancellation_diagnostics
            .iter()
            .any(|s| s.starts_with("[ede_diagnostic]"))
    );
    for kind in [
        Kind::RequestSurfaced,
        Kind::InterruptRequested,
        Kind::NativeCancel,
        Kind::ResultCancelled,
    ] {
        assert!(
            stopped
                .permission_state
                .permission_events
                .iter()
                .any(|event| event.event == kind && event.request_id.as_deref() == Some(&request))
        );
    }
    let audit = serde_json::to_string(&stopped.permission_state.permission_events)?;
    assert!(!audit.contains("echo fixture") && !audit.contains("claude tool"));
    message(&storage, &id, "Continue safely after stop")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "Continue safely after stop")
    })?;
    service.stop()?;
    Ok(())
}
#[test]
fn claude_interrupt_and_send_drains_cancel_and_keeps_later_failures() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-permission-send-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "claude tool")?;
    wait(&storage, &id, |s| s.status == Status::Waiting)?;
    control(
        &storage,
        &id,
        Control::Message {
            text: "queued after explicit cancellation".into(),
            queue: true,
            skills: vec![],
            attachments: vec![],
        },
    )?;
    control(&storage, &id, Control::InterruptAndSend)?;
    let finished = wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "queued after explicit cancellation")
    })?;
    assert!(finished.error.is_none());
    assert!(finished.queue.is_empty());
    message(&storage, &id, "claude fatal after cancellation")?;
    let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Fatal provider failure"))
    );
    service.stop()?;
    Ok(())
}
#[test]
fn claude_usage_does_not_reject_a_concurrent_permission_request() -> Result<()> {
    use difu::agents::permissions::PermissionEventKind as Kind;
    let tmp = tempfile::Builder::new()
        .prefix("difu-usage-approval-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "claude/sonnet", "claude wait")?;
    wait(&storage, &id, |s| s.status == Status::Running)?;
    fs::write(root.join("usage-interleave"), "")?;
    assert!(matches!(
        client::request(&storage, Request::Usage { id: id.clone() })?,
        Reply::Usage(_)
    ));
    let waiting = wait(&storage, &id, |s| {
        s.status == Status::Waiting
            && s.pending
                .iter()
                .any(|p| p.id == json!("usage-interleaved-approval"))
    })?;
    assert!(
        !frames(root, "claude-protocol.jsonl")?.iter().any(
            |v| v.pointer("/response/request_id") == Some(&json!("usage-interleaved-approval"))
        )
    );
    let audit = serde_json::to_string(&waiting.permission_state.permission_events)?;
    assert!(!audit.contains("SECRET_FIXTURE_ARG"));
    assert!(
        waiting
            .permission_state
            .permission_events
            .iter()
            .any(|event| event.event == Kind::RequestSurfaced
                && event.request_id.as_deref() == Some("usage-interleaved-approval"))
    );
    control(
        &storage,
        &id,
        Control::AnswerQuestion {
            request: json!("usage-interleaved-approval"),
            question: "difu-decision".into(),
            answer: Some("Deny".into()),
        },
    )?;
    let denied = wait(&storage, &id, |s| s.status == Status::Idle)?;
    assert!(denied.error.is_none());
    assert!(
        denied
            .permission_state
            .permission_events
            .iter()
            .any(|event| event.event == Kind::AnswerAction)
    );
    service.stop()?;
    Ok(())
}

#[test]
fn codex_mode_updates_immediately_idle_running_and_while_approval_waits() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-mode-immediate-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only initial mode")?;
    let original = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    let idle = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    assert_eq!(idle.active_mode.as_deref(), Some("plan"));
    assert!(idle.pending_mode.is_none());
    permissions(
        &storage,
        &id,
        Action::SetPreset {
            preset: CodexPreset::ReadOnly,
        },
    )?;
    assert_eq!(
        read(&storage, &id)?.permission_state.mode.as_deref(),
        Some("plan")
    );
    message(&storage, &id, "wait running mode")?;
    let running = wait(&storage, &id, |s| s.turn_id.is_some())?;
    let cycled = permissions(&storage, &id, Action::CycleMode)?;
    assert_eq!(cycled.active_mode.as_deref(), Some("default"));
    assert!(cycled.pending_mode.is_none());
    let still_running = read(&storage, &id)?;
    assert_eq!(still_running.turn_id, running.turn_id);
    assert_eq!(still_running.status, Status::Running);
    control(&storage, &id, Control::Interrupt)?;
    wait(&storage, &id, |s| s.turn_id.is_none())?;
    message(&storage, &id, "approval mode test")?;
    let waiting = wait(&storage, &id, |s| {
        s.status == Status::Waiting && !s.pending.is_empty()
    })?;
    let changed = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    assert_eq!(changed.active_mode.as_deref(), Some("plan"));
    assert!(changed.pending_mode.is_none());
    let current = read(&storage, &id)?;
    assert_eq!(current.thread_id, original.thread_id);
    assert_eq!(current.model, original.model);
    assert_eq!(current.effort, original.effort);
    assert_eq!(current.workspace, original.workspace);
    assert_eq!(
        current.pending.first().map(|p| &p.id),
        waiting.pending.first().map(|p| &p.id)
    );
    assert_eq!(current.status, Status::Waiting);
    let wire = frames(root, "protocol.jsonl")?;
    assert!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("thread/settings/update")))
            .count()
            >= 3
    );
    assert_eq!(
        wire.iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        3
    );
    service.stop()?;
    Ok(())
}
#[test]
fn rejected_codex_mode_update_has_no_pending_or_later_override() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-mode-rejected-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only mode rejection")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    fs::write(root.join("reject-mode-update"), "")?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into()
            }
        )
        .is_err()
    );
    let current = read(&storage, &id)?;
    assert_eq!(current.permission_state.mode.as_deref(), Some("default"));
    assert!(current.permission_state.pending_mode.is_none());
    message(&storage, &id, "chat only after rejection")?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "chat only after rejection")
    })?;
    let wire = frames(root, "protocol.jsonl")?;
    assert!(
        !wire
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .any(|v| v.pointer("/params/collaborationMode/mode") == Some(&json!("plan")))
    );
    service.stop()?;
    Ok(())
}

#[test]
fn immediate_codex_mode_recovers_same_unloaded_thread_without_applying_queued_preset() -> Result<()>
{
    let tmp = tempfile::Builder::new()
        .prefix("difu-mode-recovery-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let (storage, mut service, repo) = setup(root)?;
    let id = launch(&storage, &repo, "fixture-model", "chat only mode recovery")?;
    let original = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.permission_state.codex_rollout
    })?;
    fs::write(root.join("fail-resume-after-unsubscribe"), "")?;
    assert!(
        permissions(
            &storage,
            &id,
            Action::SetPreset {
                preset: CodexPreset::Auto
            }
        )
        .is_err()
    );
    let recovered = permissions(
        &storage,
        &id,
        Action::SetMode {
            mode: "plan".into(),
        },
    )?;
    assert_eq!(recovered.active_mode.as_deref(), Some("plan"));
    assert!(recovered.pending_mode.is_none());
    assert_eq!(recovered.pending_preset, Some(CodexPreset::Auto));
    let current = read(&storage, &id)?;
    assert_eq!(current.thread_id, original.thread_id);
    assert_eq!(current.model, original.model);
    assert_eq!(current.effort, original.effort);
    assert_eq!(
        frames(root, "protocol.jsonl")?
            .iter()
            .filter(|v| v.get("method") == Some(&json!("turn/start")))
            .count(),
        1
    );
    service.stop()?;
    Ok(())
}

#[test]
fn claude_exit_plan_choices_use_native_permission_updates_and_feedback() -> Result<()> {
    use difu::agents::plan::ClaudeChoice;
    let temp = tempfile::tempdir()?;
    let (storage, mut service, repo) = setup(temp.path())?;
    let id = launch(&storage, &repo, "claude/sonnet", "")?;
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    for (choice, mode) in [
        (ClaudeChoice::Auto, "auto"),
        (ClaudeChoice::Manual, "default"),
        (
            ClaudeChoice::Feedback {
                text: "Preserve the newline".into(),
            },
            "plan",
        ),
    ] {
        permissions(
            &storage,
            &id,
            Action::SetMode {
                mode: "plan".into(),
            },
        )?;
        message(&storage, &id, "claude plan exit")?;
        let before = wait(&storage, &id, |s| {
            s.status == Status::Waiting
                && s.pending
                    .iter()
                    .any(|p| p.params.get("difuClaudePlan").is_some())
        })?;
        let pending = before.pending.first().context("native plan request")?;
        assert_eq!(
            pending
                .params
                .pointer("/difuClaudePlan/text")
                .and_then(Value::as_str),
            Some("# Test plan\n\nKeep every existing file.\n")
        );
        control(
            &storage,
            &id,
            Control::DecideClaudePlan {
                request: pending.id.clone(),
                choice: choice.clone(),
            },
        )?;
        let after = wait(&storage, &id, |s| {
            s.status == Status::Idle && s.pending.is_empty()
        })?;
        assert_eq!(after.thread_id, before.thread_id);
        assert_eq!(after.workspace, before.workspace);
        assert_eq!(after.permission_state.mode.as_deref(), Some(mode));
        assert!(
            control(
                &storage,
                &id,
                Control::DecideClaudePlan {
                    request: pending.id.clone(),
                    choice: choice.clone()
                }
            )
            .is_err()
        );
        let wire = frames(temp.path(), "claude-protocol.jsonl")?;
        let reply = wire
            .iter()
            .rev()
            .find(|f| f.pointer("/response/request_id") == Some(&pending.id))
            .context("native response")?;
        if matches!(choice, ClaudeChoice::Feedback { .. }) {
            assert_eq!(
                reply.pointer("/response/response/interrupt"),
                Some(&json!(false))
            );
            assert_eq!(
                reply.pointer("/response/response/behavior"),
                Some(&json!("deny"))
            );
        } else {
            assert_eq!(
                reply.pointer("/response/response/updatedPermissions/0/mode"),
                Some(&json!(mode))
            );
            assert_eq!(
                reply.pointer("/response/response/updatedInput"),
                pending.params.get("claudeInput")
            );
        }
    }
    service.stop()?;
    Ok(())
}

#[test]
fn new_defaults_choose_explicit_provider_and_acknowledge_mode_before_first_turn() -> Result<()> {
    use difu::{agents::provider::Provider, storage::AgentDefaults};
    for provider in [Provider::Codex, Provider::Claude] {
        let temp = tempfile::tempdir()?;
        let (storage, _service, repo) = setup(temp.path())?;
        let Reply::Launched(id) = client::request(
            &storage,
            Request::NewAgent {
                defaults: AgentDefaults {
                    provider: Some(provider),
                    starting_mode: Some("plan".into()),
                    repository: Some(repo.clone()),
                    model: None,
                    ..Default::default()
                },
                cwd: repo,
                remember_repository: false,
            },
        )?
        else {
            anyhow::bail!("Expected new session")
        };
        let ready = wait(&storage, &id, |s| s.status == Status::Idle)?;
        assert_eq!(ready.provider, provider);
        assert_eq!(ready.permission_state.pending_mode.as_deref(), Some("plan"));
        assert!(matches!(
            ready.job,
            Job::Coding(Launch { isolated: true, .. })
        ));
        assert!(ready.entries.iter().all(|e| e.kind != "user"));
        message(&storage, &id, "settings first turn")?;
        let completed = wait(&storage, &id, |s| s.completed_turn.is_some())?;
        assert_eq!(completed.permission_state.mode.as_deref(), Some("plan"));
        assert!(completed.permission_state.pending_mode.is_none());
        if provider == Provider::Codex {
            let frames = frames(temp.path(), "protocol.jsonl")?;
            let turn = frames
                .iter()
                .find(|v| v.get("method").and_then(Value::as_str) == Some("turn/start"))
                .context("first turn")?;
            assert_eq!(
                turn.pointer("/params/collaborationMode/mode")
                    .and_then(Value::as_str),
                Some("plan")
            );
        }
    }
    Ok(())
}

#[test]
fn rejected_starting_mode_stays_pending_and_sends_no_first_turn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("reject-mode-update"), "")?;
    let (storage, _service, repo) = setup(temp.path())?;
    let Reply::Launched(id) = client::request(
        &storage,
        Request::NewAgent {
            defaults: difu::storage::AgentDefaults {
                provider: Some(difu::agents::provider::Provider::Codex),
                starting_mode: Some("plan".into()),
                repository: Some(repo.clone()),
                ..Default::default()
            },
            cwd: repo,
            remember_repository: false,
        },
    )?
    else {
        anyhow::bail!("new session")
    };
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    message(&storage, &id, "blocked first turn")?;
    let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
    assert_eq!(
        failed.permission_state.pending_mode.as_deref(),
        Some("plan")
    );
    assert!(
        frames(temp.path(), "protocol.jsonl")?
            .iter()
            .all(|f| f.get("method").and_then(Value::as_str) != Some("turn/start"))
    );
    fs::remove_file(temp.path().join("reject-mode-update"))?;
    control(&storage, &id, Control::Resume)?;
    let ready = wait(&storage, &id, |s| s.status == Status::Idle)?;
    assert_eq!(ready.permission_state.mode.as_deref(), Some("plan"));
    assert!(ready.permission_state.pending_mode.is_none());
    Ok(())
}

#[test]
fn claude_starting_modes_use_native_arguments_and_repository_restrictions() -> Result<()> {
    for mode in [
        "default",
        "acceptEdits",
        "auto",
        "dontAsk",
        "bypassPermissions",
    ] {
        let temp = tempfile::tempdir()?;
        let (storage, _service, repo) = setup(temp.path())?;
        let Reply::Launched(id) = client::request(
            &storage,
            Request::NewAgent {
                defaults: difu::storage::AgentDefaults {
                    provider: Some(difu::agents::provider::Provider::Claude),
                    starting_mode: Some(mode.into()),
                    repository: Some(repo.clone()),
                    ..Default::default()
                },
                cwd: repo,
                remember_repository: false,
            },
        )?
        else {
            anyhow::bail!("new session")
        };
        wait(&storage, &id, |s| s.status == Status::Idle)?;
        message(&storage, &id, "native mode first turn")?;
        let completed = wait(&storage, &id, |s| s.completed_turn.is_some())
            .with_context(|| format!("Starting mode {mode}"))?;
        assert_eq!(completed.permission_state.mode.as_deref(), Some(mode));
        let starts = frames(temp.path(), "claude-starts.jsonl")?;
        let args = starts
            .first()
            .and_then(|f| f.get("args"))
            .and_then(Value::as_array)
            .context("native args")?;
        assert!(args.windows(2).any(|pair|matches!(pair,[flag,value] if flag.as_str()==Some("--permission-mode") && value.as_str()==Some(mode))));
        assert_eq!(
            args.iter()
                .any(|v| v.as_str() == Some("--allow-dangerously-skip-permissions")),
            mode == "bypassPermissions"
        );
    }
    let temp = tempfile::tempdir()?;
    let (storage, _service, repo) = setup(temp.path())?;
    fs::create_dir(repo.join(".claude"))?;
    fs::write(
        repo.join(".claude/settings.json"),
        r#"{"permissions":{"disableBypassPermissionsMode":"disable"}}"#,
    )?;
    git(&repo, &["add", ".claude/settings.json"])?;
    git(&repo, &["commit", "-m", "native permission restriction"])?;
    let Reply::Launched(id) = client::request(
        &storage,
        Request::NewAgent {
            defaults: difu::storage::AgentDefaults {
                provider: Some(difu::agents::provider::Provider::Claude),
                starting_mode: Some("bypassPermissions".into()),
                repository: Some(repo.clone()),
                ..Default::default()
            },
            cwd: repo,
            remember_repository: false,
        },
    )?
    else {
        anyhow::bail!("new session")
    };
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    message(&storage, &id, "must not send")?;
    let failed = wait(&storage, &id, |s| s.status == Status::Failed)?;
    assert_eq!(
        failed.permission_state.pending_mode.as_deref(),
        Some("bypassPermissions")
    );
    assert!(!temp.path().join("claude-starts.jsonl").exists());
    Ok(())
}
