use anyhow::{Context, Result, ensure};
use difu::{
    agents::{Reply, Request, client},
    storage::Storage,
};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct Service(pub Child);
impl Service {
    pub fn start(storage: &Storage, configure: impl FnOnce(&mut Command)) -> Result<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_difu"));
        command
            .arg("--agent-service")
            .arg("--agent-config")
            .arg(&storage.config)
            .arg("--agent-cache")
            .arg(&storage.cache)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        configure(&mut command);
        let mut service = Self(command.spawn()?);
        let start = Instant::now();
        loop {
            if matches!(client::request(storage, Request::Ping), Ok(Reply::Ok)) {
                return Ok(service);
            }
            ensure!(
                service.0.try_wait()?.is_none(),
                "Service exited before accepting connections"
            );
            ensure!(
                start.elapsed() < Duration::from_secs(10),
                "Service startup timed out"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_some() {
            return Ok(());
        }
        let pid = i32::try_from(self.0.id()).context("Invalid service pid")?;
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGTERM,
        );
        let start = Instant::now();
        while self.0.try_wait()?.is_none() {
            if start.elapsed() > Duration::from_secs(8) {
                self.0.kill()?;
                self.0.wait()?;
                anyhow::bail!("Service failed to stop gracefully");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
