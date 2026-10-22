use anyhow::{Result, ensure};
use difu::{
    agents::{
        self, Entry, Job, Launch, Reply, Request, Session, Status,
        notifications::{Command, Snapshot},
    },
    storage::Storage,
};
use std::{
    process::{Child, Command as Process, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start(storage: &Storage) -> Result<Service> {
    let child = Process::new(env!("CARGO_BIN_EXE_difu"))
        .arg("--agent-service")
        .arg("--agent-config")
        .arg(&storage.config)
        .arg("--agent-cache")
        .arg(&storage.cache)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut service = Service(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(
        agents::client::request(storage, Request::Ping),
        Ok(Reply::Ok)
    ) {
        ensure!(service.0.try_wait()?.is_none(), "Service exited");
        ensure!(Instant::now() < deadline, "Service startup timed out");
        thread::sleep(Duration::from_millis(20));
    }
    Ok(service)
}
fn poll(storage: &Storage, cursor: Option<agents::notifications::Cursor>) -> Result<Snapshot> {
    match agents::client::request(
        storage,
        Request::Notifications(Command::Poll {
            client: "fixture".into(),
            focused: false,
            viewed: None,
            cursor,
        }),
    )? {
        Reply::Notifications(snapshot) => Ok(snapshot),
        _ => anyhow::bail!("Wrong reply"),
    }
}
#[test]
fn real_service_restores_sessions_without_historical_notifications_or_client_lifetime_dependency()
-> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix("difu-notify-")
        .tempdir_in("/tmp")?;
    let storage = Storage {
        config: temp.path().join("config.json"),
        cache: temp.path().join("cache"),
    };
    let mut session = Session::new(
        "one".into(),
        Job::Coding(Launch {
            repository: temp.path().into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Example".into(),
            model: None,
            effort: None,
        }),
    );
    session.status = Status::Running;
    session.completed_turn = Some("old-success".into());
    session.completed_turn_successful = true;
    session.turn_id = Some("interrupted".into());
    session.entries.push(Entry{id:"old-question".into(),kind:"agentMessage".into(),text:"PRIVATE".into(),data:serde_json::json!({"delivery":"async","questions":[{"title":"Old question","options":["Yes","No"]}]}).into(),..Default::default()});
    let home = agents::server::home(&storage)?;
    std::fs::write(home.join("one.json"), serde_json::to_vec(&session)?)?;
    std::fs::create_dir_all(home.join("notifications"))?;
    std::fs::write(home.join("notifications/dispatched.json"), b"[]")?;
    for _ in 0..2 {
        let service = start(&storage)?;
        let first = poll(&storage, None)?;
        let Reply::Session(restored) = agents::client::request(
            &storage,
            Request::Read {
                id: "one".into(),
                version: None,
            },
        )?
        else {
            anyhow::bail!("Missing session")
        };
        assert_eq!(restored.status, Status::Interrupted);
        assert!(
            restored
                .pending
                .iter()
                .any(|p| p.id == "difu-async:old-question")
        );
        agents::client::request(
            &storage,
            Request::Rename {
                id: "one".into(),
                title: "Renamed".into(),
            },
        )?;
        thread::sleep(Duration::from_millis(80));
        assert!(poll(&storage, first.cursor)?.events.is_empty());
        agents::client::request(
            &storage,
            Request::Notifications(Command::Close {
                client: "fixture".into(),
            }),
        )?;
        thread::sleep(Duration::from_millis(80));
        assert!(matches!(
            agents::client::request(&storage, Request::Ping)?,
            Reply::Ok
        ));
        let pid = nix::unistd::Pid::from_raw(i32::try_from(service.0.id())?);
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM)?;
        let mut service = service;
        let deadline = Instant::now() + Duration::from_secs(5);
        while service.0.try_wait()?.is_none() {
            ensure!(Instant::now() < deadline, "Shutdown timed out");
            thread::sleep(Duration::from_millis(10));
        }
        ensure!(service.0.wait()?.success(), "Service shutdown failed");
    }
    let saved: Session = serde_json::from_slice(&std::fs::read(home.join("one.json"))?)?;
    assert_eq!(saved.title, "Renamed");
    assert!(home.join("notifications/dispatched.json").is_file());
    Ok(())
}
