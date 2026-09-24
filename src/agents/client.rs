use super::{Job, Reply, Request, Session, Status};
use crate::{process::Cancel, storage::Storage};
use anyhow::{Context, Result, ensure};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn read_line(reader: &mut impl BufRead) -> Result<String> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        ensure!(!buffer.is_empty(), "Connection closed");
        let end = buffer.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = end.unwrap_or(buffer.len());
        ensure!(
            line.len().saturating_add(n) <= 64 * 1024 * 1024,
            "Protocol message exceeds 64 MiB"
        );
        line.extend(buffer.iter().take(n));
        reader.consume(n);
        if end.is_some() {
            return Ok(String::from_utf8(line)?);
        }
    }
}
pub fn request(storage: &Storage, request: Request) -> Result<Reply> {
    let mut stream = UnixStream::connect(super::server::socket(storage)?)
        .context("Agent service is disconnected; refresh to reconnect")?;
    stream.set_read_timeout(Some(Duration::from_secs(65)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    let response = serde_json::from_str(&read_line(&mut BufReader::new(stream))?)?;
    match response {
        Reply::Error(error) => anyhow::bail!("{error}"),
        other => Ok(other),
    }
}
pub fn ensure_running(storage: &Storage) -> Result<()> {
    ensure_version(storage, &std::env::current_exe()?, false)
}

/// Homebrew invokes this after installing a new binary. A fresh install does not
/// start a service; an existing older service is replaced even with active jobs.
pub fn refresh_running(storage: &Storage) -> Result<()> {
    ensure_version(storage, &std::env::current_exe()?, true)
}

fn version_number(version: &str) -> Result<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next().context("Missing major version")?.parse()?;
    let minor = parts.next().context("Missing minor version")?.parse()?;
    let patch = parts.next().context("Missing patch version")?.parse()?;
    ensure!(parts.next().is_none(), "Invalid service version: {version}");
    Ok((major, minor, patch))
}

fn service_version(stream: &mut UnixStream) -> Result<Option<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(&mut *stream, &Request::ServiceVersion)?;
    stream.write_all(b"\n")?;
    // Legacy services close the connection on an unknown request variant.
    let mut reader = BufReader::new(stream);
    match reader.fill_buf() {
        Ok([]) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let response: Reply = serde_json::from_str(&read_line(&mut reader)?)?;
    match response {
        Reply::ServiceVersion { version } => Ok(Some(version)),
        other => anyhow::bail!("Unexpected service version reply: {other:?}"),
    }
}

fn peer_pid(stream: &UnixStream) -> Result<nix::unistd::Pid> {
    #[cfg(target_os = "macos")]
    let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)?;
    #[cfg(target_os = "linux")]
    let pid =
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)?.pid();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    anyhow::bail!("Automatic service upgrades require macOS or Linux");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        ensure!(
            pid > 1 && pid != std::process::id() as i32,
            "Invalid agent service PID"
        );
        Ok(nix::unistd::Pid::from_raw(pid))
    }
}

fn stop_service(pid: nix::unistd::Pid, service_lock: &Path) -> Result<()> {
    use nix::{
        fcntl::{Flock, FlockArg},
        sys::signal::{Signal, kill},
    };
    match kill(pid, Signal::SIGTERM) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
        Err(error) => return Err(error.into()),
    }
    let started = Instant::now();
    let mut forced = false;
    loop {
        if matches!(kill(pid, None), Err(nix::errno::Errno::ESRCH)) {
            return Ok(());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(service_lock)?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(_) => return Ok(()),
            Err((_, nix::errno::Errno::EWOULDBLOCK)) => {}
            Err((_, error)) => return Err(error.into()),
        }
        if started.elapsed() >= Duration::from_secs(10) && !forced {
            // Upgrades are explicitly allowed to interrupt a stuck service too.
            match kill(pid, Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
            forced = true;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(15),
            "Old agent service did not release its lock"
        );
        thread::sleep(Duration::from_millis(40));
    }
}

fn ensure_version(storage: &Storage, executable: &Path, only_existing: bool) -> Result<()> {
    use nix::fcntl::{Flock, FlockArg};
    let home = super::server::home(storage)?;
    let socket = super::server::socket(storage)?;
    // Every upgraded UI and installer serializes its check-and-replace operation.
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(home.join("service-upgrade.lock"))?;
    let _lease = Flock::lock(lease, FlockArg::LockExclusive).map_err(|(_, e)| e)?;
    let mut existed = false;
    for _ in 0..3 {
        match UnixStream::connect(&socket) {
            Ok(mut stream) => {
                existed = true;
                // Get the PID from the connected socket, never from a stale PID file
                // or a broad process-name match that could affect another installation.
                let pid = peer_pid(&stream)?;
                let version = service_version(&mut stream)?;
                if let Some(version) = version
                    && version_number(&version)? >= version_number(env!("CARGO_PKG_VERSION"))?
                {
                    return Ok(());
                }
                stop_service(pid, &home.join("service.lock"))?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                if only_existing && !existed {
                    return Ok(());
                }
            }
            Err(error) => return Err(error.into()),
        }
        start_service(storage, executable, &home)?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(8) {
            if let Ok(mut stream) = UnixStream::connect(&socket) {
                if let Some(version) = service_version(&mut stream)?
                    && version_number(&version)? >= version_number(env!("CARGO_PKG_VERSION"))?
                {
                    return Ok(());
                }
                // An old UI may have won the race to restart a pre-handshake service.
                // Recheck it under the upgrade lock before making another attempt.
                break;
            }
            thread::sleep(Duration::from_millis(80));
        }
    }
    anyhow::bail!(
        "Agent service upgrade did not complete. See {}",
        home.join("service.log").display()
    )
}

fn start_service(storage: &Storage, executable: &Path, home: &Path) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(home.join("service.log"))?;
    let child = Command::new(executable)
        .arg("--agent-service")
        .arg("--agent-config")
        .arg(&storage.config)
        .arg("--agent-cache")
        .arg(&storage.cache)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .context("Cannot start the difu background agent service")?;
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Review observers can disconnect without cancelling the durable job. Only the
/// separate user cancellation token sends an interrupt to the service.
pub fn review_job(
    storage: &Storage,
    job: Job,
    observer: &Cancel,
    user_cancel: &Cancel,
    progress: impl Fn(String),
) -> Result<Session> {
    ensure_running(storage)?;
    let Reply::Launched(id) = request(storage, Request::Launch { job: Box::new(job) })? else {
        anyhow::bail!("Missing background job identifier");
    };
    let mut cancelled = false;
    let mut version = None;
    loop {
        observer.check()?;
        if user_cancel.cancelled() && !cancelled {
            request(
                storage,
                Request::Control {
                    id: id.clone(),
                    control: super::Control::Interrupt,
                },
            )?;
            cancelled = true;
        }
        match request(
            storage,
            Request::Read {
                id: id.clone(),
                version,
            },
        )? {
            Reply::Session(session) => {
                version = Some(session.version);
                if let Some(entry) = session.entries.last() {
                    progress(entry.text.clone());
                }
                match session.status {
                    Status::Completed => return Ok(*session),
                    Status::Failed | Status::Interrupted => anyhow::bail!(
                        "{}",
                        session
                            .error
                            .as_deref()
                            .unwrap_or("Background job interrupted; retry explicitly")
                    ),
                    _ => {}
                }
            }
            Reply::Unchanged => {}
            _ => anyhow::bail!("Invalid background job response"),
        }
        for _ in 0..5 {
            observer.check()?;
            thread::sleep(Duration::from_millis(100));
        }
    }
}
