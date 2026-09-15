use anyhow::{Context, Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::os::unix::process::CommandExt;
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Default, Debug)]
pub struct Cancel(pub Arc<AtomicBool>);
impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled() {
            bail!("Cancelled")
        }
        Ok(())
    }
}

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: i32,
}

/// Owns the process group until all of its streams have been drained. Any error
/// path terminates and reaps the child; a bare Child would leave it running.
pub struct ChildGroup {
    pub child: Child,
    pid: Pid,
    stopped: bool,
}
impl ChildGroup {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = command.process_group(0).spawn()?;
        let raw = match i32::try_from(child.id()) {
            Ok(pid) => pid,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        Ok(Self {
            child,
            pid: Pid::from_raw(raw),
            stopped: false,
        })
    }
    pub fn stop(&mut self) -> Result<()> {
        if !self.stopped {
            match killpg(self.pid, Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
            self.child.wait()?;
            self.stopped = true;
        }
        Ok(())
    }
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Every external job owns a process group, so cancellation also reaches helpers.
pub fn run(command: &mut Command, input: Option<Vec<u8>>, cancel: &Cancel) -> Result<Output> {
    streaming(command, input, cancel, |_| {})
}

pub fn streaming(
    command: &mut Command,
    input: Option<Vec<u8>>,
    cancel: &Cancel,
    progress: impl Fn(&str) + Send + 'static,
) -> Result<Output> {
    cancel.check()?;
    let name = command.get_program().to_string_lossy().to_string();
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut group = ChildGroup::spawn(command).with_context(|| {
        format!("Could not start {name}. Check that it is installed and on PATH.")
    })?;
    let stdout = group
        .child
        .stdout
        .take()
        .context("Missing subprocess output pipe")?;
    let mut stderr = group
        .child
        .stderr
        .take()
        .context("Missing subprocess error pipe")?;
    let out = thread::Builder::new().name("difu-output".into()).spawn(
        move || -> std::io::Result<Vec<u8>> {
            let mut reader = BufReader::new(stdout);
            let mut all = Vec::new();
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line)? > 0 {
                progress(&String::from_utf8_lossy(&line));
                all.extend_from_slice(&line);
                line.clear();
            }
            Ok(all)
        },
    )?;
    let err = thread::Builder::new()
        .name("difu-errors".into())
        .spawn(move || {
            let mut b = Vec::new();
            stderr.read_to_end(&mut b).map(|_| b)
        })?;
    let writer = if let Some(bytes) = input {
        let mut pipe = group
            .child
            .stdin
            .take()
            .context("Missing subprocess input pipe")?;
        Some(
            thread::Builder::new()
                .name("difu-input".into())
                .spawn(move || pipe.write_all(&bytes))?,
        )
    } else {
        None
    };
    let status = loop {
        if cancel.cancelled() {
            group.stop()?;
            break group.child.wait()?;
        }
        if let Some(status) = group.child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(30));
    };
    // Owned jobs must not leave descendants holding pipes or files after exit.
    group.stop()?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("Output reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("Error reader failed"))??;
    cancel.check()?;
    Ok(Output {
        stdout,
        stderr,
        code: status.code().unwrap_or(-1),
    })
}

pub fn checked(command: &mut Command, cancel: &Cancel) -> Result<String> {
    let output = run(command, None, cancel)?;
    if output.code != 0 {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    String::from_utf8(output.stdout)
        .context("This repository contains text that is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drains_both_pipes_and_sends_stdin() -> Result<()> {
        let output = run(
            Command::new("sh").args(["-c", "cat; printf error >&2"]),
            Some(b"input".to_vec()),
            &Cancel::default(),
        )?;
        assert_eq!(output.stdout, b"input");
        assert_eq!(output.stderr, b"error");
        assert_eq!(output.code, 0);
        Ok(())
    }
    #[test]
    fn cancellation_stops_children_holding_pipes() -> Result<()> {
        let cancel = Cancel::default();
        let token = cancel.clone();
        let start = std::time::Instant::now();
        let worker = thread::Builder::new().spawn(move || {
            run(
                Command::new("sh").args(["-c", "sleep 60 & wait"]),
                None,
                &token,
            )
        })?;
        thread::sleep(Duration::from_millis(100));
        cancel.cancel();
        assert!(
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Test worker failed"))?
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        Ok(())
    }
    #[test]
    fn missing_executable_is_an_error() {
        assert!(
            run(
                &mut Command::new("/difu-no-such-executable"),
                None,
                &Cancel::default()
            )
            .is_err()
        );
    }
}
