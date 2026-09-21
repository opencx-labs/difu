use super::{Job, Reply, Request, Session, Status};
use crate::{process::Cancel, storage::Storage};
use anyhow::{Context, Result, ensure};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
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
    if matches!(request(storage, Request::Ping), Ok(Reply::Ok)) {
        return Ok(());
    }
    let home = super::server::home(storage)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(home.join("service.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--agent-service")
        .arg("--agent-config")
        .arg(&storage.config)
        .arg("--agent-cache")
        .arg(&storage.cache)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let child = command
        .spawn()
        .context("Cannot start the difu background agent service")?;
    // Reap the bootstrap process when it eventually exits; dropping the UI is not cancellation.
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        if matches!(request(storage, Request::Ping), Ok(Reply::Ok)) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(80));
    }
    anyhow::bail!(
        "Agent service did not start. See {}",
        home.join("service.log").display()
    )
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
