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
fn archiving_and_deleting_stop_provider_shells_and_their_servers() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-shell-cleanup-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    git(&repo, &["remote", "add", "origin", "."])?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    for (name, source) in [
        ("codex", include_str!("fixtures/agent_codex.py")),
        ("claude", include_str!("fixtures/agent_claude.py")),
        ("git", include_str!("fixtures/slow_worktree_git.py")),
        ("gh", "#!/bin/sh\nprintf '[]\\n'\n"),
    ] {
        let file = bin.join(name);
        fs::write(&file, source)?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o700))?;
    }
    let shell = root.join("session_shell.py");
    fs::write(&shell, include_str!("fixtures/session_shell.py"))?;
    let real_git = std::env::split_paths(&std::env::var_os("PATH").context("PATH")?)
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .context("Git executable")?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").context("PATH")?,
    )))?;
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    let mut daemon = support::Service::start(&storage, |c| {
        c.env("PATH", &path)
            .env("DIFU_AGENT_FIXTURE", root)
            .env("DIFU_REAL_GIT", &real_git);
    })?;
    let unrelated = std::net::TcpListener::bind("127.0.0.1:0")?;
    for model in ["fixture-model", "claude/sonnet"] {
        for archive in [true, false] {
            let Reply::Launched(id) = client::request(
                &storage,
                Request::Launch {
                    job: Box::new(Job::Coding(Launch {
                        repository: repo.clone(),
                        base: "HEAD".into(),
                        isolated: true,
                        prompt: format!("fixture shell: {}", shell.display()),
                        model: Some(model.into()),
                        effort: None,
                    })),
                },
            )?
            else {
                anyhow::bail!("Missing shell session");
            };
            let current = wait(&storage, &id, |s| {
                s.status == Status::Idle
                    && s.workspace
                        .as_ref()
                        .is_some_and(|w| w.join("shell-server.json").is_file())
            })?;
            let workspace = current.workspace.context("Missing shell workspace")?;
            let server: serde_json::Value =
                serde_json::from_slice(&fs::read(workspace.join("shell-server.json"))?)?;
            let port = server
                .get("port")
                .and_then(|v| v.as_u64())
                .context("Missing port")?;
            let address = format!("127.0.0.1:{port}").parse()?;
            let serving = || {
                std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok()
            };
            assert!(serving());
            // Exercise shutdown both between turns and during a running tool.
            if !archive {
                control(
                    &storage,
                    &id,
                    Control::Message {
                        text: if model.starts_with("claude") {
                            "claude wait"
                        } else {
                            "wait"
                        }
                        .into(),
                        queue: false,
                        skills: Vec::new(),
                        attachments: Vec::new(),
                    },
                )?;
                wait(&storage, &id, |s| s.status == Status::Running)?;
            }
            if archive {
                client::request(
                    &storage,
                    Request::Archive {
                        id: id.clone(),
                        archived: true,
                    },
                )?;
            } else {
                fs::write(root.join("hold-removal"), "hold")?;
                let deleting_storage = storage.clone();
                let deleting_id = id.clone();
                let deleting = std::thread::spawn(move || {
                    client::request(&deleting_storage, Request::Delete { id: deleting_id })
                });
                let progress = wait(&storage, &id, |s| {
                    s.deletion_progress == Some(difu::agents::DeletionStage::Worktree)
                })?;
                assert!(workspace.exists());
                assert!(progress.shells.is_empty());
                let stopped = Instant::now();
                while serving() {
                    ensure!(
                        stopped.elapsed() < Duration::from_secs(5),
                        "Server survived the shell shutdown stage"
                    );
                    std::thread::sleep(Duration::from_millis(30));
                }
                fs::remove_file(root.join("hold-removal"))?;
                deleting
                    .join()
                    .map_err(|_| anyhow::anyhow!("Deletion thread failed"))??;
            }
            let started = Instant::now();
            while serving() {
                ensure!(
                    started.elapsed() < Duration::from_secs(5),
                    "Session server survived shutdown"
                );
                std::thread::sleep(Duration::from_millis(30));
            }
            assert!(std::net::TcpStream::connect(unrelated.local_addr()?).is_ok());
            if archive {
                let archived = session(&storage, &id)?;
                assert!(archived.archived && archived.shells.is_empty());
                assert!(workspace.exists());
                client::request(
                    &storage,
                    Request::Archive {
                        id: id.clone(),
                        archived: false,
                    },
                )?;
                assert!(!serving());
                client::request(&storage, Request::Delete { id })?;
            } else {
                assert!(!workspace.exists());
            }
        }
    }
    daemon.stop()?;
    Ok(())
}

#[test]
fn durable_agents_keep_approvals_queue_steer_and_recover_without_replay() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-agent-test-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    git(&repo, &["remote", "add", "origin", "."])?;
    let base = git(&repo, &["rev-parse", "HEAD"])?;
    git(&repo, &["update-ref", "refs/remotes/origin/main", &base])?;
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    fs::write(repo.join("tracked.txt"), "precious local edit\n")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nprintf '[]\\n'\n")?;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
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

    // Escape's interrupt-and-send action waits for completion, sends the local queue,
    // and never replays steering that Codex already accepted.
    let escape_id = launch(&storage, &repo, "wait for escape")?;
    wait(&storage, &escape_id, |s| s.status == Status::Running)?;
    let message = |text: &str, queue| Control::Message {
        text: text.into(),
        queue,
        skills: Vec::new(),
        attachments: Vec::new(),
    };
    control(
        &storage,
        &escape_id,
        message("accepted steering before escape", false),
    )?;
    control(
        &storage,
        &escape_id,
        message("wait first escape message", true),
    )?;
    control(&storage, &escape_id, message("second escape message", true))?;
    control(&storage, &escape_id, Control::InterruptAndSend)?;
    let sent = wait(&storage, &escape_id, |s| {
        s.queue.is_empty()
            && s.entries
                .iter()
                .any(|e| e.kind == "userMessage" && e.text == "second escape message")
    })?;
    for text in [
        "accepted steering before escape",
        "wait first escape message",
        "second escape message",
    ] {
        assert_eq!(
            sent.entries
                .iter()
                .filter(|e| e.kind == "userMessage" && e.text == text)
                .count(),
            1
        );
        assert!(
            !sent
                .entries
                .iter()
                .any(|e| e.kind == "unsent" && e.text == text)
        );
    }
    control(&storage, &escape_id, Control::InterruptAndSend)?;
    wait(&storage, &escape_id, |s| s.status == Status::Idle)?;
    let wire = fs::read_to_string(root.join("protocol.jsonl"))?;
    for text in [
        "accepted steering before escape",
        "wait first escape message",
        "second escape message",
    ] {
        assert_eq!(
            wire.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(
                    |v| v.pointer("/params/input/0/text").and_then(|t| t.as_str()) == Some(text)
                )
                .count(),
            1
        );
    }
    // A repeated Escape after delivery must not interrupt a new, unrelated turn.
    control(&storage, &escape_id, Control::InterruptAndSend)?;
    let interrupts = |log: &str| {
        log.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("method").and_then(|m| m.as_str()) == Some("turn/interrupt"))
            .count()
    };
    assert_eq!(
        interrupts(&wire),
        interrupts(&fs::read_to_string(root.join("protocol.jsonl"))?)
    );

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
    let suggested = wait(&storage, &id, |s| s.suggestion.is_some())?;
    assert_eq!(
        suggested.suggestion.as_ref().context("suggestion")?.text,
        "Okay, implement the plan."
    );
    let suggestions = fs::read_to_string(root.join("suggestions.jsonl"))?;
    assert!(suggestions.contains("gpt-5.6-luna") && suggestions.contains("medium"));
    assert!(
        !suggested
            .entries
            .iter()
            .any(|e| e.kind == "userMessage" && e.text == "Okay, implement the plan.")
    );
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
        assert!(!latest.text.contains("Difu question state"));
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
                        && v.pointer("/params/input/1/text")
                            .and_then(|v| v.as_str())
                            .is_some_and(|text| text
                                .contains(&format!("Remaining pending questions ({remaining})"))
                                && text.contains(
                                    "Do not ask them again, including reworded versions"
                                ))
                )
        );
    }
    let answered = wait(&storage, &questions, |s| s.pending.is_empty())?;
    assert_eq!(answered.status, Status::Running);
    control(&storage, &questions, Control::Interrupt)?;
    wait(&storage, &questions, |s| s.status == Status::Interrupted)?;
    control(
        &storage,
        &questions,
        Control::Message {
            text: "chat only: continue with this new message".into(),
            queue: false,
            skills: Vec::new(),
            attachments: Vec::new(),
        },
    )?;
    let resumed = wait(&storage, &questions, |s| s.status == Status::Idle)?;
    assert_eq!(resumed.thread_id, answered.thread_id);
    assert_eq!(
        resumed
            .entries
            .iter()
            .filter(|e| e.kind == "userMessage"
                && e.text == "chat only: continue with this new message")
            .count(),
        1
    );
    control(
        &storage,
        &questions,
        Control::Message {
            text: "fixture fail turn".into(),
            queue: false,
            skills: Vec::new(),
            attachments: Vec::new(),
        },
    )?;
    wait(&storage, &questions, |s| s.status == Status::Failed)?;
    control(
        &storage,
        &questions,
        Control::Message {
            text: "chat only: recover failed turn".into(),
            queue: false,
            skills: Vec::new(),
            attachments: Vec::new(),
        },
    )?;
    let recovered = wait(&storage, &questions, |s| s.status == Status::Idle)?;
    assert_eq!(recovered.thread_id, answered.thread_id);
    assert!(recovered.error.is_none());
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
    control(
        &storage,
        &second,
        Control::MessageWithAttachments {
            text: "continue after service restart".into(),
            queue: false,
            skills: Vec::new(),
            attachments: Vec::new(),
        },
    )?;
    let resumed = wait(&storage, &second, |s| s.status == Status::Idle)?;
    assert_eq!(resumed.thread_id, recovered.thread_id);
    assert_eq!(
        resumed
            .entries
            .iter()
            .filter(|e| e.kind == "userMessage" && e.text == "continue after service restart")
            .count(),
        1
    );
    let protocol = fs::read_to_string(root.join("protocol.jsonl"))?;
    assert!(
        !protocol
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(
                |v| v.pointer("/params/input/0/text").and_then(|v| v.as_str())
                    == Some("must not replay")
            )
    );
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

    // Explicit deletion stops the agent and discards dirty managed worktrees.
    let clean = launch(&storage, &repo, "wait for deletion")?;
    let current = wait(&storage, &clean, |s| s.status == Status::Running)?;
    let tree = current.workspace.context("Missing deletion worktree")?;
    let Reply::Shells(shells) = client::request(&storage, Request::Shells { id: clean.clone() })?
    else {
        anyhow::bail!("Missing shells");
    };
    assert_eq!(
        shells
            .first()
            .and_then(|s| s.get("processId"))
            .and_then(|v| v.as_str()),
        Some("123")
    );
    client::request(&storage, Request::Delete { id: clean.clone() })?;
    assert!(!tree.exists());
    assert!(session(&storage, &clean).is_err());
    assert!(
        !difu::agents::server::home(&storage)?
            .join(format!("{clean}.json"))
            .exists()
    );
    let dirty = launch(&storage, &repo, "wait with changes")?;
    let current = wait(&storage, &dirty, |s| s.status == Status::Running)?;
    let tree = current.workspace.context("Missing protected worktree")?;
    fs::write(tree.join("precious.txt"), "preserve me")?;
    git(&repo, &["worktree", "lock", tree.to_str().context("path")?])?;
    assert!(client::request(&storage, Request::Delete { id: dirty.clone() }).is_err());
    assert!(tree.exists());
    assert!(session(&storage, &dirty)?.deletion_progress.is_none());
    git(
        &repo,
        &["worktree", "unlock", tree.to_str().context("path")?],
    )?;
    fs::write(tree.join("tracked.txt"), "discard tracked edits")?;
    fs::write(tree.join(".gitignore"), "ignored.txt\n")?;
    fs::write(tree.join("ignored.txt"), "discard ignored files")?;
    client::request(&storage, Request::Delete { id: dirty.clone() })?;
    assert!(!tree.exists());
    assert!(session(&storage, &dirty).is_err());
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))?,
        "precious local edit\n"
    );
    let mut config = storage.load_config()?;
    config.repository_rules.insert(
        repo.canonicalize()?,
        "Keep repository rule fixture in initial instructions.".into(),
    );
    storage.save_config(&config)?;
    let removal = launch(&storage, &repo, "remove stale questions")?;
    let current = wait(&storage, &removal, |s| {
        s.status == Status::Idle && s.completed_turn.is_some()
    })?;
    assert!(current.question_tools);
    assert_eq!(
        current.repository_rules,
        "Keep repository rule fixture in initial instructions."
    );
    let protocol = fs::read_to_string(root.join("protocol.jsonl"))?;
    assert!(
        protocol
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|frame| {
                frame["method"] == "thread/start"
                    && frame["params"]["developerInstructions"]
                        .as_str()
                        .is_some_and(|text| {
                            text.contains("Keep repository rule fixture in initial instructions.")
                        })
            })
    );
    assert_eq!(current.pending_question_count(), 1);
    let durable: Session = serde_json::from_slice(&fs::read(
        difu::agents::server::home(&storage)?.join(format!("{removal}.json")),
    )?)?;
    let pending = durable.pending.first().context("remaining question")?;
    assert_eq!(pending.unanswered_questions().len(), 1);
    assert_eq!(
        pending.params["difuAnswers"]["1"],
        serde_json::json!({"answers":[]})
    );
    let artifact = launch(&storage, &repo, "artifact report")?;
    // Connecting briefly reports Idle before the initial turn starts. Only a
    // completed turn proves the artifact tool has had an opportunity to run.
    let current = wait(&storage, &artifact, |s| {
        s.status == Status::Idle && s.completed_turn.is_some()
    })?;
    assert!(current.artifact_tools);
    assert_eq!(
        current
            .artifacts
            .first()
            .context("Artifact not registered")?
            .title,
        "Fixture report"
    );
    assert!(current.pending.is_empty());
    let durable: Session = serde_json::from_slice(&fs::read(
        difu::agents::server::home(&storage)?.join(format!("{artifact}.json")),
    )?)?;
    assert_eq!(durable.artifacts.len(), 1);
    let Reply::Launched(existing) = client::request(
        &storage,
        Request::Launch {
            job: Box::new(Job::Coding(Launch {
                repository: repo.clone(),
                base: "HEAD".into(),
                isolated: false,
                prompt: "wait in existing directory".into(),
                model: None,
                effort: None,
            })),
        },
    )?
    else {
        anyhow::bail!("No existing-directory session");
    };
    wait(&storage, &existing, |s| s.status == Status::Running)?;
    client::request(&storage, Request::Delete { id: existing })?;
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))?,
        "precious local edit\n"
    );

    daemon.stop()?;
    Ok(())
}

#[test]
fn new_sessions_accept_input_while_worktrees_are_preparing() -> Result<()> {
    for model in [None, Some("claude/sonnet")] {
        let tmp = tempfile::Builder::new()
            .prefix("difu-startup-")
            .tempdir_in("/tmp")?;
        let root = tmp.path();
        let repo = root.join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "-b", "main"])?;
        fs::write(repo.join("tracked.txt"), "committed\n")?;
        git(&repo, &["add", "."])?;
        git(&repo, &["commit", "-m", "base"])?;
        git(&repo, &["remote", "add", "origin", "."])?;
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )?;
        fs::write(repo.join("tracked.txt"), "preserve local edit\n")?;
        let inherited_path = std::env::var_os("PATH").context("PATH")?;
        let real_git = std::env::split_paths(&inherited_path)
            .map(|p| p.join("git"))
            .find(|p| p.is_file())
            .context("Git executable")?
            .canonicalize()?;
        let bin = root.join("bin");
        fs::create_dir(&bin)?;
        for (name, source) in [
            ("git", include_str!("fixtures/slow_worktree_git.py")),
            ("codex", include_str!("fixtures/agent_codex.py")),
            ("claude", include_str!("fixtures/agent_claude.py")),
            ("gh", "#!/bin/sh\nprintf '[]\\n'\n"),
        ] {
            fs::write(bin.join(name), source)?;
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o700))?;
        }
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&inherited_path)),
        )?;
        let storage = Storage {
            config: root.join("config.json"),
            cache: root.join("cache"),
        };
        let start_service = || {
            support::Service::start(&storage, |c| {
                c.env("PATH", &path)
                    .env("DIFU_AGENT_FIXTURE", root)
                    .env("DIFU_REAL_GIT", &real_git);
            })
        };
        let mut daemon = start_service()?;
        let launch_empty = || -> Result<String> {
            fs::write(root.join("hold-worktree"), "hold")?;
            let Reply::Launched(id) = client::request(
                &storage,
                Request::NewAgent {
                    defaults: difu::storage::AgentDefaults {
                        repository: Some(repo.clone()),
                        model: model.map(String::from),
                        ..Default::default()
                    },
                    cwd: root.into(),
                    remember_repository: false,
                },
            )?
            else {
                anyhow::bail!("Expected immediate session ID");
            };
            let preparing = wait(&storage, &id, Session::preparing)?;
            assert_eq!(preparing.status, Status::Starting);
            assert!(!preparing.workspace_ready);
            assert!(!preparing.summary().can_read_changes);
            assert!(preparing.thread_id.is_none());
            assert!(matches!(
                client::request(&storage, Request::Changes { id: id.clone() })?,
                Reply::Changes(patch) if patch.is_empty()
            ));
            assert!(matches!(
                client::request(&storage, Request::Statistics { id: id.clone() })?,
                Reply::Statistics(stats) if stats == Default::default()
            ));
            Ok(id)
        };
        let send = |id: &str, text: &str| {
            control(
                &storage,
                id,
                Control::Message {
                    text: text.into(),
                    queue: false,
                    skills: Vec::new(),
                    attachments: Vec::new(),
                },
            )
        };

        let id = launch_empty()?;
        // These requests must be acknowledged before Git is released.
        send(&id, "chat only: first queued message")?;
        send(&id, "chat only: second queued message")?;
        send(&id, "discard this queued message")?;
        let discard = session(&storage, &id)?
            .queue
            .last()
            .context("queued message")?
            .clone();
        control(
            &storage,
            &id,
            Control::ReplaceQueued {
                index: 2,
                expected: discard,
                replacement: None,
            },
        )?;
        let pending = session(&storage, &id)?;
        assert_eq!(pending.queue.len(), 2);
        assert!(!pending.can_send_waiting());
        let saved: Session = serde_json::from_slice(&fs::read(
            difu::agents::server::home(&storage)?.join(format!("{id}.json")),
        )?)?;
        assert_eq!(saved.queue, pending.queue);
        assert!(!root.join("protocol.jsonl").exists());
        assert!(!root.join("claude-starts.jsonl").exists());
        fs::remove_file(root.join("hold-worktree"))?;
        let ready = wait(&storage, &id, |s| {
            s.workspace_ready
                && s.status == Status::Idle
                && s.queue.is_empty()
                && s.entries.iter().filter(|e| e.kind == "userMessage").count() == 2
        })?;
        assert_eq!(
            ready
                .entries
                .iter()
                .filter(|e| e.kind == "userMessage")
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            [
                "chat only: first queued message",
                "chat only: second queued message"
            ]
        );
        assert_eq!(
            fs::read_to_string(repo.join("tracked.txt"))?,
            "preserve local edit\n"
        );
        send(&id, "chat only: normal send after setup")?;
        wait(&storage, &id, |s| {
            s.status == Status::Idle
                && s.entries.iter().any(|e| {
                    e.kind == "userMessage" && e.text == "chat only: normal send after setup"
                })
        })?;

        // Failed setup preserves accepted input without running the provider.
        let failed = launch_empty()?;
        let image = root.join("attachment.png");
        image::RgbaImage::new(1, 1).save(&image)?;
        let difu::agents::media::Paste::Attachments(attachments) =
            difu::agents::media::files(&storage, &failed, vec![image])?
        else {
            anyhow::bail!("Expected attachment");
        };
        control(
            &storage,
            &failed,
            Control::MessageWithAttachments {
                text: "chat only: preserve after failure".into(),
                queue: false,
                skills: Vec::new(),
                attachments: attachments.clone(),
            },
        )?;
        fs::write(root.join("fail-worktree"), "fail")?;
        fs::remove_file(root.join("hold-worktree"))?;
        let failed_state = wait(&storage, &failed, |s| s.status == Status::Failed)?;
        assert!(failed_state.thread_id.is_none() && failed_state.queue.is_empty());
        assert!(
            failed_state
                .entries
                .iter()
                .any(|e| e.kind == "unsent" && e.text == "chat only: preserve after failure")
        );
        let unsent = failed_state
            .entries
            .iter()
            .find(|e| e.kind == "unsent")
            .context("unsent prompt")?;
        let prompt: difu::agents::Prompt = serde_json::from_value(
            unsent
                .data
                .get("prompt")
                .context("Missing unsent prompt")?
                .clone(),
        )?;
        assert_eq!(prompt.attachments(), attachments);
        fs::remove_file(root.join("fail-worktree"))?;

        let other = root.join("other");
        fs::create_dir(&other)?;
        git(&other, &["init", "-b", "main"])?;
        fs::write(other.join("other.txt"), "new repository\n")?;
        git(&other, &["add", "."])?;
        git(&other, &["commit", "-m", "other base"])?;
        git(&other, &["remote", "add", "origin", "."])?;
        for stage in ["fetch", "add", "created"] {
            let after_creation = stage == "created";
            if stage == "fetch" {
                fs::write(root.join("hold-fetch"), "hold")?;
            }
            if after_creation {
                fs::write(root.join("hold-after-worktree"), "hold")?;
            }
            let switching = launch_empty()?;
            let prepared = wait(&storage, &switching, |s| {
                if stage == "fetch" {
                    root.join("fetch-started").exists()
                } else {
                    s.branch.is_some()
                }
            })?;
            if after_creation {
                fs::remove_file(root.join("hold-worktree"))?;
                wait(&storage, &switching, |_| {
                    root.join("worktree-created").exists()
                })?;
            }
            send(&switching, "chat only: retain cancelled setup input")?;
            assert!(
                client::request(
                    &storage,
                    Request::Repository {
                        id: switching.clone(),
                        repository: root.join("missing-repository"),
                    },
                )
                .is_err()
            );
            assert!(session(&storage, &switching)?.preparing());
            let started = Instant::now();
            client::request(
                &storage,
                Request::Repository {
                    id: switching.clone(),
                    repository: other.clone(),
                },
            )?;
            assert!(started.elapsed() < Duration::from_secs(5));
            let changed = session(&storage, &switching)?;
            assert_eq!(changed.status, Status::Idle);
            assert_eq!(changed.job.root(), &other.canonicalize()?);
            assert!(!changed.workspace_ready && changed.branch.is_none());
            assert!(
                !difu::agents::server::home(&storage)?
                    .join("worktrees")
                    .join(&switching)
                    .exists()
            );
            assert!(changed.queue.is_empty());
            assert!(changed.entries.iter().any(|entry| {
                entry.kind == "unsent" && entry.text == "chat only: retain cancelled setup input"
            }));
            if let Some(branch) = prepared.branch {
                assert!(
                    git(
                        &repo,
                        &["show-ref", "--verify", &format!("refs/heads/{branch}")]
                    )
                    .is_err()
                );
            }
            fs::remove_file(root.join(if after_creation {
                "hold-after-worktree"
            } else {
                "hold-worktree"
            }))?;
            if stage == "fetch" {
                fs::remove_file(root.join("hold-fetch"))?;
            }
            send(&switching, "chat only: continue in selected repository")?;
            let resumed = wait(&storage, &switching, |s| {
                s.status == Status::Idle && s.thread_id.is_some() && s.workspace_ready
            })?;
            assert!(
                resumed
                    .workspace
                    .context("New workspace")?
                    .join("other.txt")
                    .exists()
            );
        }

        // Restart interrupts preparation and never replays its queued messages.
        let interrupted = launch_empty()?;
        send(&interrupted, "chat only: preserve after restart")?;
        daemon.stop()?;
        fs::remove_file(root.join("hold-worktree"))?;
        let mut daemon = start_service()?;
        let restored = session(&storage, &interrupted)?;
        assert_eq!(restored.status, Status::Interrupted);
        assert!(!restored.workspace_ready && restored.thread_id.is_none());
        assert!(restored.queue.is_empty());
        assert!(
            restored
                .entries
                .iter()
                .any(|e| e.kind == "unsent" && e.text == "chat only: preserve after restart")
        );
        daemon.stop()?;
    }
    Ok(())
}

#[test]
fn empty_sessions_create_worktrees_before_the_first_turn_and_keep_provider_context() -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("difu-lazy-agent-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    git(&repo, &["remote", "add", "origin", "."])?;
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    fs::write(repo.join("tracked.txt"), "precious local edit\n")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nprintf '[]\\n'\n")?;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
    let claude = bin.join("claude");
    fs::write(&claude, include_str!("fixtures/agent_claude.py"))?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
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
    let empty = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    assert_eq!(empty.status, Status::Idle);
    assert!(empty.thread_id.is_none() && empty.workspace_ready);
    let workspace = empty.workspace.clone().context("worktree")?;
    assert_ne!(workspace, repo.canonicalize()?);
    assert!(!root.join("protocol.jsonl").exists());
    assert_eq!(
        fs::read_to_string(workspace.join("tracked.txt"))?,
        "original\n"
    );
    assert_eq!(
        storage.load_config()?.agent_defaults.repository,
        Some(repo.canonicalize()?)
    );
    assert!(
        client::request(
            &storage,
            Request::Repository {
                id: id.clone(),
                repository: root.join("missing-repository")
            }
        )
        .is_err()
    );
    assert!(workspace.exists());
    assert_eq!(session(&storage, &id)?.workspace, Some(workspace.clone()));
    assert!(
        client::request(
            &storage,
            Request::Repository {
                id: id.clone(),
                repository: workspace.canonicalize()?,
            }
        )
        .is_err()
    );
    assert!(workspace.exists());
    let old_branch = empty.branch.context("Initial branch")?;
    fs::write(workspace.join("committed.txt"), "discard this work\n")?;
    git(&workspace, &["add", "."])?;
    git(&workspace, &["commit", "-m", "Discarded session work"])?;
    fs::write(workspace.join("tracked.txt"), "discard dirty edit\n")?;
    fs::write(workspace.join(".gitignore"), "ignored.txt\n")?;
    fs::write(workspace.join("ignored.txt"), "discard ignored file\n")?;
    let other = root.join("other-repository");
    fs::create_dir(&other)?;
    git(&other, &["init", "-b", "main"])?;
    fs::write(other.join("other.txt"), "other repository\n")?;
    git(&other, &["add", "."])?;
    git(&other, &["commit", "-m", "Other base"])?;
    git(&other, &["remote", "add", "origin", "."])?;
    git(
        &repo,
        &["worktree", "lock", workspace.to_str().context("path")?],
    )?;
    assert!(
        client::request(
            &storage,
            Request::Repository {
                id: id.clone(),
                repository: other.clone(),
            }
        )
        .is_err()
    );
    assert!(workspace.exists());
    assert_eq!(session(&storage, &id)?.job.root(), &repo.canonicalize()?);
    git(
        &repo,
        &["worktree", "unlock", workspace.to_str().context("path")?],
    )?;
    client::request(
        &storage,
        Request::Repository {
            id: id.clone(),
            repository: other.clone(),
        },
    )?;
    let changed = session(&storage, &id)?;
    assert!(!workspace.exists());
    assert!(!changed.workspace_ready && changed.branch.is_none());
    assert_eq!(changed.job.root(), &other.canonicalize()?);
    assert_eq!(changed.baseline, Some(git(&other, &["rev-parse", "HEAD"])?));
    assert!(
        git(
            &repo,
            &["show-ref", "--verify", &format!("refs/heads/{old_branch}")]
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))?,
        "precious local edit\n"
    );
    // Returning to a repository must not reuse its old work or PR branch.
    client::request(
        &storage,
        Request::Repository {
            id: id.clone(),
            repository: repo.clone(),
        },
    )?;
    let Reply::Changes(patch) = client::request(&storage, Request::Changes { id: id.clone() })?
    else {
        anyhow::bail!("Changes");
    };
    assert!(patch.is_empty());
    let message = |text: &str| Control::Message {
        text: text.into(),
        queue: false,
        skills: Vec::new(),
        attachments: Vec::new(),
    };
    control(&storage, &id, message("chat only: explain this repository"))?;
    let codex = wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.thread_id.is_some()
            && s.entries
                .iter()
                .any(|e| e.kind == "agentMessage" && e.text == "Finished fixture task")
    })?;
    assert_eq!(
        codex
            .permissions
            .pointer("/sandbox/type")
            .and_then(|v| v.as_str()),
        Some("workspaceWrite")
    );
    assert_ne!(codex.branch.as_ref(), Some(&old_branch));
    assert!(!workspace.join("committed.txt").exists());
    assert!(!workspace.join("ignored.txt").exists());
    control(&storage, &id, message("artifact report"))?;
    let with_artifact = wait(&storage, &id, |s| {
        s.status == Status::Idle && !s.artifacts.is_empty()
    })?;
    let artifact_branch = with_artifact.branch.clone().context("Artifact branch")?;
    client::request(
        &storage,
        Request::Repository {
            id: id.clone(),
            repository: repo.clone(),
        },
    )?;
    let reset = session(&storage, &id)?;
    assert_eq!(reset.thread_id, with_artifact.thread_id);
    assert!(
        reset
            .entries
            .iter()
            .any(|e| e.text.contains("explain this repository"))
    );
    assert!(reset.artifacts.is_empty() && reset.workspaces.is_empty());
    assert!(reset.comparison_base.is_none() && reset.branch.is_none());
    assert!(!workspace.exists());
    assert!(
        git(
            &repo,
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{artifact_branch}")
            ]
        )
        .is_err()
    );
    let before_usage = session(&storage, &id)?.entries.len();
    let Reply::Usage(usage) = client::request(&storage, Request::Usage { id: id.clone() })? else {
        anyhow::bail!("usage response");
    };
    assert_eq!(
        usage
            .pointer("/limits/rateLimits/primary/usedPercent")
            .and_then(|v| v.as_u64()),
        Some(25)
    );
    assert_eq!(session(&storage, &id)?.entries.len(), before_usage);
    control(
        &storage,
        &id,
        message("app approval: perform my authorized action"),
    )?;
    let awaiting = wait(&storage, &id, |s| !s.pending.is_empty())?;
    let request = awaiting.pending.first().context("approval")?.id.clone();
    control(
        &storage,
        &id,
        Control::Respond {
            request,
            response: serde_json::json!({"action":"accept","content":{}}),
        },
    )?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.pending.is_empty()
    })?;

    control(
        &storage,
        &id,
        Control::Model {
            model: Some("claude/sonnet".into()),
            effort: None,
        },
    )?;
    let switched = session(&storage, &id)?;
    assert_eq!(switched.provider, difu::agents::provider::Provider::Claude);
    assert_eq!(switched.workspace.as_ref(), Some(&workspace));
    assert!(switched.thread_id.is_none());
    assert!(
        switched
            .provider_context
            .as_deref()
            .is_some_and(|c| c.contains("explain this repository"))
    );
    control(
        &storage,
        &id,
        message("claude tool: execute the approved task"),
    )?;
    let awaiting = wait(&storage, &id, |s| !s.pending.is_empty())?;
    assert!(awaiting.tool_running());
    assert!(
        control(
            &storage,
            &id,
            Control::Model {
                model: Some("fixture-model".into()),
                effort: None
            }
        )
        .is_err()
    );
    control(
        &storage,
        &id,
        Control::Respond {
            request: awaiting
                .pending
                .first()
                .context("Claude approval")?
                .id
                .clone(),
            response: serde_json::json!({"action":"accept"}),
        },
    )?;
    let claude = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.pending.is_empty()
    })?;
    let before_usage = session(&storage, &id)?.entries.len();
    let Reply::Usage(usage) = client::request(&storage, Request::Usage { id: id.clone() })? else {
        anyhow::bail!("Claude usage response");
    };
    assert_eq!(
        usage
            .pointer("/rate_limits/five_hour/utilization")
            .and_then(|v| v.as_u64()),
        Some(40)
    );
    assert_eq!(session(&storage, &id)?.entries.len(), before_usage);
    assert!(!claude.tool_running());
    assert!(claude.provider_context.is_none());
    assert_eq!(
        claude
            .entries
            .iter()
            .filter(
                |e| e.kind == "userMessage" && e.text == "claude tool: execute the approved task"
            )
            .count(),
        1
    );
    assert_eq!(
        claude
            .entries
            .iter()
            .filter(|e| e.kind == "agentMessage" && e.text == "Finished Claude task")
            .count(),
        1
    );
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model".into()),
            effort: None,
        },
    )?;
    let restored = session(&storage, &id)?;
    assert_eq!(restored.thread_id, codex.thread_id);
    assert!(
        restored
            .provider_context
            .as_deref()
            .is_some_and(|c| c.contains("Finished Claude task"))
    );
    control(&storage, &id, message("chat only: resume Codex"))?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && s.provider_context.is_none()
    })?;
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("claude/sonnet".into()),
            effort: Some("high".into()),
        },
    )?;
    assert_eq!(session(&storage, &id)?.thread_id, claude.thread_id);
    control(&storage, &id, message("finish with Claude"))?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && workspace.join("claude.txt").exists()
    })?;
    let starts = fs::read_to_string(root.join("claude-starts.jsonl"))?;
    assert!(starts.contains("--resume=fixture-claude") && starts.contains("--effort=high"));
    assert!(!repo.join("claude.txt").exists());
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))?,
        "precious local edit\n"
    );

    let registered = root.join("manual worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "manual",
            registered.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    let registered = registered.canonicalize()?;
    let Reply::Session(changed) = client::request(
        &storage,
        Request::RegisterWorktree {
            id: id.clone(),
            path: registered.clone(),
            base: Some("origin/main".into()),
        },
    )?
    else {
        anyhow::bail!("registered session");
    };
    assert_eq!(changed.thread_id, claude.thread_id);
    assert_eq!(changed.workspace.as_ref(), Some(&registered));
    assert!(changed.workspaces.iter().any(|w| w.path == workspace));
    control(&storage, &id, message("continue in registered worktree"))?;
    wait(&storage, &id, |s| {
        s.status == Status::Idle && registered.join("claude.txt").exists()
    })?;
    let claude_tree = root.join("claude registered");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "claude-registered",
            claude_tree.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    let claude_tree = claude_tree.canonicalize()?;
    control(
        &storage,
        &id,
        message(&format!(
            "claude register worktree: {}",
            claude_tree.display()
        )),
    )?;
    let moved = wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.workspace.as_ref() == Some(&claude_tree)
            && claude_tree.join("claude.txt").exists()
    })?;
    assert_eq!(moved.thread_id, claude.thread_id);
    control(
        &storage,
        &id,
        Control::Model {
            model: Some("fixture-model".into()),
            effort: None,
        },
    )?;
    let codex_tree = root.join("codex registered");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "codex-registered",
            codex_tree.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    let codex_tree = codex_tree.canonicalize()?;
    control(
        &storage,
        &id,
        message(&format!("register worktree: {}", codex_tree.display())),
    )?;
    let moved = wait(&storage, &id, |s| {
        s.status == Status::Idle
            && s.workspace.as_ref() == Some(&codex_tree)
            && codex_tree.join("new.txt").exists()
    })?;
    assert_eq!(moved.thread_id, codex.thread_id);
    assert_eq!(moved.workspaces.len(), 4);

    // Both providers isolate immediately, including configurations saved before
    // isolation became mandatory for new sessions.
    let Reply::Launched(second) = client::request(
        &storage,
        Request::NewAgent {
            defaults: difu::storage::AgentDefaults {
                isolated: false,
                model: Some("claude/sonnet".into()),
                ..defaults
            },
            cwd: root.into(),
            remember_repository: false,
        },
    )?
    else {
        anyhow::bail!("Expected Claude session");
    };
    let second = wait(&storage, &second, |s| {
        s.status == Status::Idle && s.workspace_ready
    })?;
    assert!(second.workspace_ready && second.thread_id.is_none());
    assert_ne!(second.workspace, Some(repo.canonicalize()?));
    assert_eq!(second.provider, difu::agents::provider::Provider::Claude);
    for model in ["fixture-model", "claude/sonnet"] {
        let Reply::Launched(deleting) = client::request(
            &storage,
            Request::Launch {
                job: Box::new(Job::Coding(Launch {
                    repository: repo.clone(),
                    base: "HEAD".into(),
                    isolated: true,
                    prompt: "delete this session".into(),
                    model: Some(model.into()),
                    effort: None,
                })),
            },
        )?
        else {
            anyhow::bail!("Missing deletion session");
        };
        let started = Instant::now();
        loop {
            let Reply::Sessions(sessions) = client::request(&storage, Request::List)? else {
                anyhow::bail!("Missing sessions");
            };
            if !sessions.iter().any(|s| s.id == deleting) {
                break;
            }
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "Self deletion did not finish for {model}"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        let home = difu::agents::server::home(&storage)?;
        assert!(!home.join(format!("{deleting}.json")).exists());
        assert!(!home.join("worktrees").join(&deleting).exists());
    }
    daemon.stop()?;
    Ok(())
}

#[test]
fn guidance_wait_survives_frontend_reconnect_and_service_restart_without_permission() -> Result<()>
{
    let tmp = tempfile::Builder::new()
        .prefix("difu-guidance-reconnect-")
        .tempdir_in("/tmp")?;
    let root = tmp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::write(repo.join("tracked.txt"), "original\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    git(&repo, &["remote", "add", "origin", "."])?;
    fs::write(repo.join("AGENTS.md"), "Untracked repository guidance\n")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nprintf '[]\\n'\n")?;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
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
    let start = || {
        support::Service::start(&storage, |c| {
            c.env("PATH", &path).env("DIFU_AGENT_FIXTURE", root);
        })
    };
    let mut daemon = start()?;
    let id = launch(&storage, &repo, "finish guided task")?;
    let waiting = wait(&storage, &id, |s| s.status == Status::Waiting)?;
    let workspace = waiting.workspace.context("workspace")?;
    // Each read uses a fresh frontend connection. Disconnecting does not answer
    // questions or copy files; the persisted request remains actionable.
    for _ in 0..3 {
        let current = session(&storage, &id)?;
        assert_eq!(current.pending_question_count(), 1);
        assert!(!current.guidance_checked);
        assert!(!workspace.join("AGENTS.md").exists());
        assert!(!root.join("protocol.jsonl").exists());
    }
    daemon.stop()?;
    let mut daemon = start()?;
    assert_eq!(session(&storage, &id)?.status, Status::Interrupted);
    assert!(!workspace.join("AGENTS.md").exists());
    assert!(!root.join("protocol.jsonl").exists());
    control(
        &storage,
        &id,
        Control::Message {
            text: "finish guided task from this new message".into(),
            queue: false,
            skills: Vec::new(),
            attachments: Vec::new(),
        },
    )?;
    let resumed = wait(&storage, &id, |s| s.status == Status::Waiting)?;
    assert_eq!(resumed.pending_question_count(), 1);
    assert_eq!(resumed.workspace.as_ref(), Some(&workspace));
    assert!(!resumed.guidance_checked);
    assert!(!root.join("protocol.jsonl").exists());
    control(
        &storage,
        &id,
        Control::AnswerQuestion {
            request: serde_json::json!("difu-missing-guidance"),
            question: "copy_guidance".into(),
            answer: Some("Continue without copying".into()),
        },
    )?;
    let ready = wait(&storage, &id, |s| {
        s.status == Status::Idle && s.thread_id.is_some()
    })?;
    assert!(ready.guidance_checked);
    assert_eq!(ready.pending_question_count(), 0);
    assert!(!workspace.join("AGENTS.md").exists());
    daemon.stop()?;
    Ok(())
}
