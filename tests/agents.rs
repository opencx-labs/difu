mod support;
use anyhow::{Context, Result, ensure};
use difu::{
    agents::{Control, Job, Launch, Reply, Request, Session, Status, client},
    process::{self, Cancel},
    storage::Storage,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
fn git(root: &Path, args: &[&str]) -> Result<String> {
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
    )
    .map(|s| s.trim().to_owned())
}
fn session(storage: &Storage, id: &str) -> Result<Session> {
    let Reply::Session(session) = client::request(
        storage,
        Request::Read {
            id: id.into(),
            version: None,
        },
    )?
    else {
        anyhow::bail!("No session response");
    };
    Ok(*session)
}
fn wait(storage: &Storage, id: &str, ready: impl Fn(&Session) -> bool) -> Result<Session> {
    let start = Instant::now();
    loop {
        let session = session(storage, id)?;
        if ready(&session) {
            return Ok(session);
        }
        ensure!(
            session.status != Status::Failed,
            "Session failed: {:?}",
            session.error
        );
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "Timed out: {:?}",
            session.status
        );
        std::thread::sleep(Duration::from_millis(30));
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
fn launch(storage: &Storage, root: &Path, prompt: &str) -> Result<String> {
    let Reply::Launched(id) = client::request(
        storage,
        Request::Launch {
            job: Box::new(Job::Coding(Launch {
                repository: root.into(),
                base: "HEAD".into(),
                isolated: true,
                prompt: prompt.into(),
                model: None,
                effort: None,
            })),
        },
    )?
    else {
        anyhow::bail!("Missing launch ID");
    };
    Ok(id)
}
#[test]
fn durable_agents_keep_approvals_queue_steer_and_recover_without_replay() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-agent-test-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    let base = git(&repo, &["rev-parse", "HEAD"])?;
    fs::write(repo.join("tracked.txt"), "precious local edit\n")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let codex = bin.join("codex");
    fs::write(&codex, include_str!("fixtures/agent_codex.py"))?;
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700))?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").context("PATH")?,
    )))?;
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    fs::create_dir(&storage.cache)?;
    let start_service = || {
        support::Service::start(&storage, |c| {
            c.env("PATH", &path).env("DIFU_AGENT_FIXTURE", root);
        })
    };
    let mut daemon = start_service()?;
    // macOS inherits the listener's nonblocking flag on accepted sockets. A
    // request split across writes must wait for the rest instead of closing.
    {
        use std::io::Write;
        let mut stream =
            std::os::unix::net::UnixStream::connect(difu::agents::server::socket(&storage)?)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"\"Pi")?;
        std::thread::sleep(Duration::from_millis(150));
        stream.write_all(b"ng\"\n")?;
        let reply: Reply =
            serde_json::from_str(&client::read_line(&mut std::io::BufReader::new(stream))?)?;
        assert!(matches!(reply, Reply::Ok));
    }

    let id = launch(&storage, &repo, "approval please")?;
    let waiting = wait(&storage, &id, |s| s.status == Status::Waiting)?;
    assert_eq!(waiting.baseline.as_deref(), Some(base.as_str()));
    let workspace = waiting.workspace.clone().context("No worktree")?;
    assert_ne!(workspace, repo);
    assert_eq!(
        fs::read_to_string(workspace.join("tracked.txt"))?,
        "original\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))?,
        "precious local edit\n"
    );
    // Every request uses a new connection: closing the frontend does not own the turn.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session(&storage, &id)?.pending.len(), 1);
    control(
        &storage,
        &id,
        Control::Message {
            attachments: Vec::new(),
            skills: Vec::new(),
            text: "steer this task".into(),
            queue: false,
        },
    )?;
    let steered = wait(&storage, &id, |s| {
        s.entries
            .iter()
            .any(|entry| entry.kind == "userMessage" && entry.text == "steer this task")
    })?;
    assert_eq!(
        steered
            .entries
            .iter()
            .rev()
            .find(|e| e.kind == "userMessage")
            .map(|e| e.text.as_str()),
        Some("steer this task")
    );
    control(
        &storage,
        &id,
        Control::Message {
            attachments: Vec::new(),
            skills: Vec::new(),
            text: "queued follow-up".into(),
            queue: true,
        },
    )?;
    assert_eq!(session(&storage, &id)?.queue.len(), 1);
    let Reply::Skills { skills, errors } = client::request(
        &storage,
        Request::Skills {
            id: id.clone(),
            force: true,
        },
    )?
    else {
        anyhow::bail!("Missing skills");
    };
    assert!(errors.is_empty());
    let skill = skills.first().context("skill")?.clone();
    assert!(skill.path.starts_with(&workspace));
    let expected = session(&storage, &id)?
        .queue
        .first()
        .context("queued")?
        .clone();
    let replacement = difu::agents::Prompt::WithSkills {
        attachments: Vec::new(),
        text: "edited queued follow-up".into(),
        skills: vec![skill.clone()],
    };
    control(
        &storage,
        &id,
        Control::ReplaceQueued {
            index: 0,
            expected: expected.clone(),
            replacement: Some(replacement),
        },
    )?;
    assert!(
        control(
            &storage,
            &id,
            Control::ReplaceQueued {
                index: 0,
                expected,
                replacement: None
            }
        )
        .is_err()
    );
    assert!(control(&storage, &id, Control::Compact).is_err());
    let request = waiting
        .pending
        .first()
        .context("Missing approval")?
        .id
        .clone();
    control(
        &storage,
        &id,
        Control::Respond {
            request,
            response: serde_json::json!({"decision":"accept"}),
        },
    )?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.queue.is_empty() && workspace.join("new.txt").exists()
    })?;
    let named = wait(&storage, &id, |s| s.title == "Fixture coding session")?;
    assert!(named.title_attempted);
    let title_log = fs::read_to_string(root.join("titles.jsonl"))?;
    assert!(title_log.contains("gpt-5.6-luna") && title_log.contains("medium"));
    let image_path = root.join("attachment.png");
    image::RgbaImage::new(2, 2).save(&image_path)?;
    let video_path = root.join("clip.mp4");
    fs::write(&video_path, b"fixture video")?;
    let difu::agents::media::Paste::Attachments(mut attachments) =
        difu::agents::media::files(&storage, &id, vec![image_path, video_path])?
    else {
        anyhow::bail!("media copies");
    };
    for (a, label) in attachments.iter_mut().zip(["image 1", "video 1"]) {
        a.label = label.into();
    }
    control(
        &storage,
        &id,
        Control::MessageWithAttachments {
            text: "Inspect [image 1] and [video 1]".into(),
            queue: false,
            skills: Vec::new(),
            attachments,
        },
    )?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "Inspect [image 1] and [video 1]")
    })?;
    let wire = fs::read_to_string(root.join("protocol.jsonl"))?;
    assert!(wire.contains("localImage") && wire.contains("Local video path"));
    assert_eq!(fs::read_to_string(root.join("titles.jsonl"))?, title_log);
    control(&storage, &id, Control::Compact)?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.entries.iter().any(|e| e.kind == "contextCompaction")
    })?;
    let protocol = fs::read_to_string(root.join("protocol.jsonl"))?;
    let values = protocol
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(values.iter().any(
        |v| v.pointer("/params/input/1/type").and_then(|v| v.as_str()) == Some("skill")
            && v.pointer("/params/input/1/path").and_then(|v| v.as_str()) == skill.path.to_str()
    ));
    let Reply::Changes(patch) = client::request(&storage, Request::Changes { id: id.clone() })?
    else {
        anyhow::bail!("No changes")
    };
    assert!(patch.contains("+approved change") && patch.contains("+agent change"));
    git(&workspace, &["add", "."])?;
    git(&workspace, &["commit", "-m", "agent commit"])?;
    fs::write(workspace.join("tracked.txt"), "unstaged\n")?;
    fs::write(workspace.join("staged.txt"), "staged\n")?;
    git(&workspace, &["add", "staged.txt"])?;
    fs::write(workspace.join(".gitignore"), "ignored.txt\n")?;
    fs::write(workspace.join("ignored.txt"), "not in diff\n")?;
    let git_dir = git(&workspace, &["rev-parse", "--absolute-git-dir"])?;
    let index = std::path::PathBuf::from(git_dir).join("index");
    let before = fs::read(&index)?;
    let Reply::Changes(patch) = client::request(&storage, Request::Changes { id: id.clone() })?
    else {
        anyhow::bail!("No changes")
    };
    assert!(
        patch.contains("+approved change")
            && patch.contains("+unstaged")
            && patch.contains("+staged")
    );
    assert!(!patch.contains("not in diff"));
    let Reply::Statistics(stats) =
        client::request(&storage, Request::Statistics { id: id.clone() })?
    else {
        anyhow::bail!("No statistics")
    };
    assert_eq!(
        stats.added,
        patch
            .lines()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++ "))
            .count() as u64
    );
    assert_eq!(
        stats.removed,
        patch
            .lines()
            .filter(|line| line.starts_with('-') && !line.starts_with("--- "))
            .count() as u64
    );
    let Reply::WorkspacePaths(paths) =
        client::request(&storage, Request::WorkspacePaths { id: id.clone() })?
    else {
        anyhow::bail!("No workspace paths")
    };
    assert!(paths.iter().any(|p| p == "tracked.txt"));
    assert!(paths.iter().any(|p| p == "staged.txt"));
    assert!(!paths.iter().any(|p| p == "ignored.txt"));
    assert_eq!(fs::read(&index)?, before);
    assert!(client::request(&storage, Request::Cleanup { id: id.clone() }).is_err());
    fs::write(
        repo.join("AGENTS.md"),
        "Follow native repository guidance.\n",
    )?;
    let guided = launch(&storage, &repo, "finish guided task")?;
    let awaiting = wait(&storage, &guided, |s| s.status == Status::Waiting)?;
    assert!(awaiting.thread_id.is_none());
    let guided_path = awaiting.workspace.as_ref().context("guided workspace")?;
    assert!(!guided_path.join("AGENTS.md").exists());
    control(
        &storage,
        &guided,
        Control::AnswerQuestion {
            request: serde_json::json!("difu-missing-guidance"),
            question: "copy_guidance".into(),
            answer: Some("Copy missing guidance".into()),
        },
    )?;
    wait(&storage, &guided, |s| s.status == Status::Idle)?;
    assert_eq!(
        fs::read(guided_path.join("AGENTS.md"))?,
        fs::read(repo.join("AGENTS.md"))?
    );
    fs::remove_file(repo.join("AGENTS.md"))?;
    let questions = launch(&storage, &repo, "async questions active")?;
    let asking = wait(&storage, &questions, |s| s.pending_question_count() == 3)?;
    assert_eq!(asking.status, Status::Running);
    let response = serde_json::json!({"answers":{"0":{"answers":["Explore"]},"1":{"answers":["Full"]},"2":{"answers":["Keep my draft"]}}});
    for (question, answer, remaining) in [
        ("0", "Explore", 2),
        ("1", "Full", 1),
        ("2", "Keep my draft", 0),
    ] {
        control(
            &storage,
            &questions,
            Control::AnswerQuestion {
                request: asking.pending.first().context("async request")?.id.clone(),
                question: question.into(),
                answer: Some(answer.into()),
            },
        )?;
        let current = wait(&storage, &questions, |s| {
            s.pending_question_count() == remaining
        })?;
        assert_eq!(current.status, Status::Running);
        let latest = current
            .entries
            .iter()
            .rev()
            .find(|e| e.kind == "userMessage")
            .context("Accepted question answer")?;
        assert!(latest.text.contains(answer));
        assert!(!latest.text.contains("Act on this answer now"));
        let wire = fs::read_to_string(root.join("protocol.jsonl"))?;
        assert!(
            wire.lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .any(
                    |v| v.get("method").and_then(|v| v.as_str()) == Some("turn/steer")
                        && v.pointer("/params/input/0/text")
                            .and_then(|v| v.as_str())
                            .is_some_and(|text| text.contains(answer)
                                && !text.contains("Act on this answer now"))
                )
        );
    }
    let answered = wait(&storage, &questions, |s| s.pending.is_empty())?;
    assert_eq!(answered.status, Status::Running);
    control(&storage, &questions, Control::Interrupt)?;
    wait(&storage, &questions, |s| s.status == Status::Interrupted)?;
    control(&storage, &questions, Control::Resume)?;
    wait(&storage, &questions, |s| s.status == Status::Idle)?;
    control(
        &storage,
        &questions,
        Control::Message {
            text: "async questions idle".into(),
            queue: false,
            skills: Vec::new(),
            attachments: Vec::new(),
        },
    )?;
    let asking = wait(&storage, &questions, |s| {
        s.status == Status::Idle && s.pending_question_count() == 3
    })?;
    let async_request = asking.pending.first().context("async request")?.id.clone();
    let incremental = launch(&storage, &repo, "async questions idle")?;
    let pending = wait(&storage, &incremental, |s| {
        s.status == Status::Idle && s.pending_question_count() == 3
    })?;
    let incremental_request = pending.pending.first().context("questions")?.id.clone();
    control(
        &storage,
        &incremental,
        Control::AnswerQuestion {
            request: incremental_request.clone(),
            question: "0".into(),
            answer: Some("Review".into()),
        },
    )?;
    wait(&storage, &incremental, |s| {
        s.status == Status::Idle && s.pending_question_count() == 2
    })?;
    let wire = fs::read_to_string(root.join("protocol.jsonl"))?;
    assert!(
        wire.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(
                |v| v.get("method").and_then(|v| v.as_str()) == Some("turn/start")
                    && v.pointer("/params/input/0/text")
                        .and_then(|v| v.as_str())
                        .is_some_and(|text| text.contains("Review")
                            && !text.contains("Act on this answer now"))
            )
    );

    control(
        &storage,
        &incremental,
        Control::AnswerQuestion {
            request: incremental_request.clone(),
            question: "1".into(),
            answer: None,
        },
    )?;
    assert_eq!(session(&storage, &incremental)?.pending_question_count(), 1);
    // A second session runs immediately; no difu queue or shared working directory.
    let second = launch(&storage, &repo, "wait forever")?;
    wait(&storage, &second, |s| s.status == Status::Running)?;
    control(
        &storage,
        &second,
        Control::Message {
            attachments: Vec::new(),
            skills: Vec::new(),
            text: "must not replay".into(),
            queue: true,
        },
    )?;
    // Kill the service abruptly; accepted queued work must still not replay.
    daemon.0.kill()?;
    daemon.0.wait()?;
    let log_before = fs::read_to_string(root.join("protocol.jsonl"))?;
    let mut daemon = start_service()?;
    let recovered = session(&storage, &second)?;
    assert_eq!(recovered.status, Status::Interrupted);
    assert!(recovered.queue.is_empty());
    assert!(
        recovered
            .entries
            .iter()
            .any(|e| e.kind == "unsent" && e.text == "must not replay")
    );
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::read_to_string(root.join("protocol.jsonl"))?, log_before);
    assert_eq!(session(&storage, &questions)?.pending_question_count(), 3);
    control(
        &storage,
        &questions,
        Control::Respond {
            request: async_request.clone(),
            response: response.clone(),
        },
    )?;
    wait(&storage, &questions, |s| {
        s.status == Status::Idle && s.pending.is_empty()
    })?;
    assert!(
        control(
            &storage,
            &questions,
            Control::Respond {
                request: async_request,
                response
            }
        )
        .is_err()
    );
    let protocol = fs::read_to_string(root.join("protocol.jsonl"))?;
    let answers: Vec<serde_json::Value> = protocol
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|v| {
            v.pointer("/params/input/0/text")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with("Answers to your questions:") || s.starts_with("> "))
        })
        .collect();
    assert_eq!(answers.len(), 5);
    assert!(
        answers
            .iter()
            .any(|v| v.get("method").and_then(|v| v.as_str()) == Some("turn/steer"))
    );
    assert!(
        answers
            .iter()
            .any(|v| v.get("method").and_then(|v| v.as_str()) == Some("turn/start"))
    );
    assert_eq!(session(&storage, &incremental)?.pending_question_count(), 1);
    let skipped_log = fs::read_to_string(root.join("protocol.jsonl"))?;
    control(
        &storage,
        &incremental,
        Control::AnswerQuestion {
            request: incremental_request.clone(),
            question: "2".into(),
            answer: None,
        },
    )?;
    assert_eq!(session(&storage, &incremental)?.pending_question_count(), 0);
    assert_eq!(
        fs::read_to_string(root.join("protocol.jsonl"))?,
        skipped_log
    );
    assert!(
        control(
            &storage,
            &incremental,
            Control::AnswerQuestion {
                request: incremental_request,
                question: "0".into(),
                answer: Some("duplicate".into())
            }
        )
        .is_err()
    );
    control(&storage, &second, Control::Resume)?;
    wait(&storage, &second, |s| s.status == Status::Idle)?;
    control(
        &storage,
        &id,
        Control::Message {
            attachments: Vec::new(),
            skills: Vec::new(),
            text: "New explicit message after reconnect".into(),
            queue: false,
        },
    )?;
    wait(&storage, &id, |s| s.status == Status::Idle)?;
    let log = fs::read_to_string(root.join("protocol.jsonl"))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for call in events.iter().filter(|e| {
        matches!(
            e.get("method").and_then(|v| v.as_str()),
            Some("thread/start" | "thread/resume")
        )
    }) {
        let instructions = call
            .pointer("/params/developerInstructions")
            .and_then(|v| v.as_str())
            .context("Missing policy")?;
        assert!(
            instructions.contains("Preserve inherited guidance")
                && instructions.contains("Do not run local tests")
        );
        assert!(
            call.pointer("/params/approvalPolicy").is_none()
                && call.pointer("/params/sandbox").is_none()
        );
    }
    assert!(
        events
            .iter()
            .any(|e| e.get("method").and_then(|v| v.as_str()) == Some("turn/steer"))
    );
    client::request(
        &storage,
        Request::Rename {
            id: second.clone(),
            title: "Renamed".into(),
        },
    )?;
    client::request(
        &storage,
        Request::Archive {
            id: second.clone(),
            archived: true,
        },
    )?;
    assert!(session(&storage, &second)?.archived);
    client::request(
        &storage,
        Request::Archive {
            id: second.clone(),
            archived: false,
        },
    )?;
    let second_tree = session(&storage, &second)?
        .workspace
        .context("Missing second workspace")?;
    git(&second_tree, &["add", "."])?;
    git(&second_tree, &["commit", "-m", "Retained work"])?;
    let branch = session(&storage, &second)?
        .branch
        .context("Missing branch")?;
    let committed = git(&second_tree, &["rev-parse", "HEAD"])?;
    client::request(&storage, Request::Cleanup { id: second.clone() })?;
    assert!(!second_tree.exists());
    assert_eq!(git(&repo, &["rev-parse", &branch])?, committed);
    daemon.stop()?;
    Ok(())
}

#[test]
fn empty_sessions_defer_worktrees_until_editing_and_restore_permissions() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-lazy-agent-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    fs::write(repo.join("tracked.txt"), "precious local edit\n")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let codex = bin.join("codex");
    fs::write(&codex, include_str!("fixtures/agent_codex.py"))?;
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700))?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").context("PATH")?,
    )))?;
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    let mut daemon = support::Service::start(&storage, |c| {
        c.env("PATH", &path).env("DIFU_AGENT_FIXTURE", root);
    })?;
    let Reply::ChooseRepository = client::request(
        &storage,
        Request::NewAgent {
            defaults: Default::default(),
            cwd: root.into(),
            remember_repository: false,
        },
    )?
    else {
        anyhow::bail!("Expected repository picker outside Git");
    };
    let defaults = difu::storage::AgentDefaults {
        repository: Some(repo.clone()),
        ..Default::default()
    };
    let Reply::Launched(id) = client::request(
        &storage,
        Request::NewAgent {
            defaults: defaults.clone(),
            cwd: root.into(),
            remember_repository: true,
        },
    )?
    else {
        anyhow::bail!("Expected empty session");
    };
    let empty = session(&storage, &id)?;
    assert_eq!(empty.status, Status::Idle);
    assert!(empty.thread_id.is_none() && empty.waiting_for_workspace());
    assert_eq!(
        storage.load_config()?.agent_defaults.repository,
        Some(repo.canonicalize()?)
    );
    assert!(!root.join("protocol.jsonl").exists());
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"])?
            .matches("worktree ")
            .count(),
        1
    );
    let Reply::Changes(patch) = client::request(&storage, Request::Changes { id: id.clone() })?
    else {
        anyhow::bail!("Changes");
    };
    assert!(patch.is_empty()); // Never attribute original checkout edits to this session.
    let message = |text: &str| Control::Message {
        text: text.into(),
        queue: false,
        skills: Vec::new(),
        attachments: Vec::new(),
    };
    control(&storage, &id, message("chat only: explain this repository"))?;
    let chatting = wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.thread_id.is_some()
            && s.entries.iter().any(|entry| {
                entry.kind == "userMessage" && entry.text == "chat only: explain this repository"
            })
            && s.entries
                .iter()
                .any(|entry| entry.kind == "agentMessage" && entry.text == "Finished fixture task")
    })?;
    assert!(chatting.waiting_for_workspace());
    assert_eq!(
        chatting
            .permissions
            .pointer("/sandbox/type")
            .and_then(|v| v.as_str()),
        Some("readOnly")
    );
    let thread = chatting.thread_id.clone();
    assert!(!repo.join("new.txt").exists());
    // Guidance must still be checked when a live read-only thread switches workspaces.
    fs::write(repo.join("AGENTS.md"), "Keep this repository guidance.\n")?;
    control(
        &storage,
        &id,
        message("need edit: make the requested change"),
    )?;
    let awaiting = wait(&storage, &id, |s| s.status == Status::Waiting)?;
    assert_eq!(awaiting.thread_id, thread);
    control(
        &storage,
        &id,
        Control::Respond {
            request: serde_json::json!("difu-missing-guidance"),
            response: serde_json::json!({"answers":{"copy_guidance":{"answers":["Copy missing guidance"]}}}),
        },
    )?;
    let editing = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    assert_eq!(editing.thread_id, thread);
    assert_eq!(
        editing
            .permissions
            .pointer("/sandbox/type")
            .and_then(|v| v.as_str()),
        Some("workspaceWrite")
    );
    assert_eq!(
        editing
            .permissions
            .get("approvalPolicy")
            .and_then(|v| v.as_str()),
        Some("on-request")
    );
    let workspace = editing.workspace.context("worktree")?;
    assert_ne!(workspace, repo.canonicalize()?);
    assert_eq!(
        fs::read_to_string(workspace.join("new.txt"))?,
        "agent change\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md"))?,
        "Keep this repository guidance.\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))?,
        "precious local edit\n"
    );
    assert!(!repo.join("new.txt").exists());
    assert_eq!(
        editing
            .entries
            .iter()
            .filter(|e| e.kind == "userMessage")
            .count(),
        2
    );
    // Isolation disabled uses the selected checkout without a worktree or read-only transition.
    let Reply::Launched(direct) = client::request(
        &storage,
        Request::NewAgent {
            defaults: difu::storage::AgentDefaults {
                isolated: false,
                ..defaults
            },
            cwd: root.into(),
            remember_repository: false,
        },
    )?
    else {
        anyhow::bail!("Expected direct session");
    };
    control(&storage, &direct, message("make change directly"))?;
    let direct = wait(&storage, &direct, |s| {
        s.status == Status::Idle && s.thread_id.is_some() && repo.join("new.txt").exists()
    })?;
    assert_eq!(direct.workspace, Some(repo.canonicalize()?));
    assert!(repo.join("new.txt").exists());
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"])?
            .matches("worktree ")
            .count(),
        2
    );
    daemon.stop()?;
    Ok(())
}
