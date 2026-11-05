use anyhow::{Context, Result, ensure};
use difu::{
    agents::{Job, Launch, Prompt, Reply, Request, Session, Status, client, server},
    storage::Storage,
};
use std::{
    fs,
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const FIXTURE: &str = r#"
import fcntl, json, pathlib, socket, sys
home = pathlib.Path(sys.argv[1])
lock = (home/'service.lock').open('a')
fcntl.flock(lock, fcntl.LOCK_EX)
listener = socket.socket(socket.AF_UNIX)
listener.bind(str(home/'service.sock'))
listener.listen()
while True:
    stream, _ = listener.accept()
    with stream:
        line = stream.makefile('rb').readline()
        if not line: continue
        request = json.loads(line)
        if request == 'Ping': reply = 'Ok'
        elif request == 'ServiceVersion' and sys.argv[2] != 'legacy':
            reply = {'ServiceVersion': {'version': sys.argv[2]}}
        else: continue
        stream.sendall((json.dumps(reply)+'\n').encode())
"#;

struct Cleanup(Storage);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Ok(pid) = daemon_pid(&self.0) {
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        }
    }
}
fn daemon_pid(storage: &Storage) -> Result<nix::unistd::Pid> {
    let stream = UnixStream::connect(server::socket(storage)?)?;
    #[cfg(target_os = "macos")]
    let pid = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerPid)?;
    #[cfg(target_os = "linux")]
    let pid =
        nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)?.pid();
    Ok(nix::unistd::Pid::from_raw(pid))
}
fn refresh(storage: &Storage) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_difu"));
    command
        .args(["--refresh-agent-service", "--agent-config"])
        .arg(&storage.config)
        .arg("--agent-cache")
        .arg(&storage.cache)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn fixture(storage: &Storage, version: &str) -> Result<Fixture> {
    let child = Command::new("python3")
        .args(["-c", FIXTURE])
        .arg(server::home(storage)?)
        .arg(version)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut child = Fixture(child);
    let start = Instant::now();
    while !matches!(client::request(storage, Request::Ping), Ok(Reply::Ok)) {
        ensure!(child.0.try_wait()?.is_none(), "Fixture exited");
        ensure!(
            start.elapsed() < Duration::from_secs(5),
            "Fixture startup timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(child)
}
fn storage() -> Result<(tempfile::TempDir, Storage)> {
    let dir = tempfile::Builder::new()
        .prefix("difu-upgrade-")
        .tempdir_in("/tmp")?;
    let storage = Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().join("cache"),
    };
    Ok((dir, storage))
}
fn success(command: &mut Command) -> Result<()> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "Refresh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn refresh_does_not_start_a_service_on_first_install() -> Result<()> {
    let (_dir, storage) = storage()?;
    success(&mut refresh(&storage))?;
    assert!(!server::socket(&storage)?.exists());
    Ok(())
}

#[test]
fn same_or_newer_service_is_not_restarted() -> Result<()> {
    for version in [env!("CARGO_PKG_VERSION"), "999.0.0"] {
        let (_dir, storage) = storage()?;
        let mut old = fixture(&storage, version)?;
        let _cleanup = Cleanup(storage.clone());
        let before = daemon_pid(&storage)?;
        success(&mut refresh(&storage))?;
        assert_eq!(daemon_pid(&storage)?, before);
        assert!(old.0.try_wait()?.is_none());
    }
    Ok(())
}

#[test]
fn legacy_and_older_services_upgrade_once_and_preserve_interrupted_work() -> Result<()> {
    // Earlier 0.20.0 builds used an incompatible MCP reply shape; they must be upgraded.
    for version in ["legacy", "0.0.1", "0.20.0"] {
        let (dir, storage) = storage()?;
        let mut old = fixture(&storage, version)?;
        let _cleanup = Cleanup(storage.clone());
        let before = daemon_pid(&storage)?;
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace)?;
        fs::write(workspace.join("precious.txt"), "keep edits")?;
        let mut session = Session::new(
            "saved".into(),
            Job::Coding(Launch {
                repository: workspace.clone(),
                base: "HEAD".into(),
                isolated: false,
                prompt: "never replay this".into(),
                model: None,
                effort: None,
            }),
        );
        session.workspace = Some(workspace.clone());
        session.status = Status::Running;
        session.turn_id = Some("old-turn".into());
        session
            .queue
            .push(Prompt::Text("never replay queue".into()));
        session.note("userMessage", "saved prompt");
        fs::write(
            server::home(&storage)?.join("saved.json"),
            serde_json::to_vec(&session)?,
        )?;
        // Concurrent installers/UI connections must agree on a single replacement.
        let first = refresh(&storage).spawn()?;
        let second = refresh(&storage).spawn()?;
        for child in [first, second] {
            let output = child.wait_with_output()?;
            ensure!(
                output.status.success(),
                "Upgrade failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_ne!(daemon_pid(&storage)?, before);
        assert!(old.0.try_wait()?.is_some());
        assert!(
            matches!(client::request(&storage, Request::ServiceVersion)?, Reply::ServiceVersion { version } if version == env!("CARGO_PKG_VERSION"))
        );
        let Reply::Session(restored) = client::request(
            &storage,
            Request::Read {
                id: "saved".into(),
                version: None,
            },
        )?
        else {
            anyhow::bail!("Missing restored session");
        };
        assert_eq!(restored.status, Status::Interrupted);
        assert!(restored.turn_id.is_none());
        assert!(restored.queue.is_empty());
        assert!(
            restored
                .entries
                .iter()
                .any(|entry| entry.text == "saved prompt")
        );
        assert!(
            restored
                .entries
                .iter()
                .any(|entry| entry.kind == "unsent" && entry.text == "never replay queue")
        );
        assert_eq!(
            fs::read_to_string(workspace.join("precious.txt"))?,
            "keep edits"
        );
        let after = daemon_pid(&storage)?;
        success(&mut refresh(&storage))?;
        assert_eq!(daemon_pid(&storage)?, after);
        // No worker was started by restoration, even for a previously active session.
        thread::sleep(Duration::from_millis(100));
        let Reply::Session(again) = client::request(
            &storage,
            Request::Read {
                id: "saved".into(),
                version: None,
            },
        )?
        else {
            anyhow::bail!("Missing session after restart");
        };
        assert_eq!(again.status, Status::Interrupted);
        assert_eq!(again.entries.len(), restored.entries.len());
        assert!(again.workspace.as_ref().context("Workspace")?.is_dir());
    }
    Ok(())
}

/// A service replacing itself keeps its process, which is already the leader
/// of its session, so detaching again must not stop it. A process group leader
/// is refused the same way.
#[test]
fn a_service_that_cannot_detach_again_still_starts() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let (_dir, storage) = storage()?;
    let _cleanup = Cleanup(storage.clone());
    let mut child = Command::new(env!("CARGO_BIN_EXE_difu"))
        .arg("--agent-service")
        .arg("--agent-config")
        .arg(&storage.config)
        .arg("--agent-cache")
        .arg(&storage.cache)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Ok(Reply::ServiceVersion { version }) =
            client::request(&storage, Request::ServiceVersion)
        {
            assert_eq!(version, env!("CARGO_PKG_VERSION"));
            break;
        }
        ensure!(
            child.try_wait()?.is_none(),
            "The service stopped instead of starting"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "The service did not start"
        );
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// On a server, systemd runs `difu serve`. A terminal app or installer of
/// another difu version keeps that service and warns instead of replacing it.
#[test]
fn a_servers_older_service_is_kept_with_a_warning() -> Result<()> {
    for version in ["legacy", "0.0.1"] {
        let (_dir, storage) = storage()?;
        fs::write(&storage.config, r#"{"server":{"enabled":true}}"#)?;
        let mut old = fixture(&storage, version)?;
        let _cleanup = Cleanup(storage.clone());
        let before = daemon_pid(&storage)?;
        let output = refresh(&storage).output()?;
        ensure!(
            output.status.success(),
            "Refresh failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            stderr.contains("difu server upgrade"),
            "Expected a warning, got {stderr}"
        );
        // The terminal app's own check behaves the same and warns once.
        client::ensure_running(&storage)?;
        client::ensure_running(&storage)?;
        assert_eq!(daemon_pid(&storage)?, before);
        assert!(
            old.0.try_wait()?.is_none(),
            "The server's service was stopped"
        );
    }
    let warning = client::take_warning().context("Expected one warning")?;
    ensure!(warning.contains("difu server upgrade"), "{warning}");
    ensure!(client::take_warning().is_none(), "The warning repeats");
    Ok(())
}
