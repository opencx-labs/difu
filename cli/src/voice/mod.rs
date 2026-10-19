//! Hold-to-talk voice through Codex's realtime voice and the Codex login, so no
//! API key is needed. Codex's helper owns the microphone and speaker; difu sees
//! only levels and transcript text, and nothing is written to disk.
mod host;
mod signal;

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The microphone is live.
    Listening,
    /// Peak levels since the previous sample, sampled every 100 ms.
    Levels {
        microphone: u16,
        speaker: u16,
    },
    /// Text appended to the current transcript segment.
    Delta(Role, String),
    /// The final text of a transcript segment.
    Segment(Role, String),
    Error(String),
}

const LIVE: u8 = 0;
const RELEASED: u8 = 1;
const CANCELLED: u8 = 2;
const SAMPLE: Duration = Duration::from_millis(100);
/// After release, wait at least this long for words still in flight…
const SETTLE: Duration = Duration::from_millis(700);
/// …and at most this long for the last segment to be finalised.
const FINISH: Duration = Duration::from_secs(5);

/// One voice session. Dropping it ends the session at once.
pub struct Session {
    pub events: mpsc::Receiver<Event>,
    control: Arc<AtomicU8>,
}
impl Session {
    /// Mutes the microphone. Dictation then ends once the last words are
    /// transcribed, and the channel disconnects.
    pub fn release(&self) {
        let _ = self
            .control
            .compare_exchange(LIVE, RELEASED, Ordering::AcqRel, Ordering::Acquire);
    }
    /// Unmutes the microphone of a conversation that is still connected.
    pub fn resume(&self) {
        let _ = self
            .control
            .compare_exchange(RELEASED, LIVE, Ordering::AcqRel, Ordering::Acquire);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.control.store(CANCELLED, Ordering::Release);
    }
}

/// Fails with an actionable message when the installed Codex has no voice.
pub fn available() -> anyhow::Result<()> {
    host::package().map(drop)
}

/// Starts dictation: speech becomes text, and the voice never talks back.
pub fn dictate() -> Session {
    let (sender, events) = mpsc::channel();
    let control = Arc::new(AtomicU8::new(LIVE));
    let signal = control.clone();
    thread::spawn(move || {
        if let Err(error) = dictate_with(&signal, &sender) {
            let _ = sender.send(Event::Error(format!("{error:#}")));
        }
    });
    Session { events, control }
}

/// Starts a spoken conversation with a Codex session's agent. `start` hands
/// the helper's offer to the session and returns OpenAI's answer; `stop` ends
/// the session's side. The voice's replies play through the speaker, and its
/// transcripts reach the session's chat rather than this channel.
pub fn converse(
    start: impl FnOnce(&str) -> anyhow::Result<String> + Send + 'static,
    stop: impl FnOnce() + Send + 'static,
) -> Session {
    let (sender, events) = mpsc::channel();
    let control = Arc::new(AtomicU8::new(LIVE));
    let signal = control.clone();
    thread::spawn(move || {
        if let Err(error) = converse_with(&signal, &sender, start, stop) {
            let _ = sender.send(Event::Error(format!("{error:#}")));
        }
    });
    Session { events, control }
}

fn converse_with(
    control: &AtomicU8,
    events: &mpsc::Sender<Event>,
    start: impl FnOnce(&str) -> anyhow::Result<String>,
    stop: impl FnOnce(),
) -> anyhow::Result<()> {
    let mut host = host::Host::start()?;
    let offer = host.offer()?;
    if control.load(Ordering::Acquire) == CANCELLED {
        return Ok(());
    }
    let result = start(&offer).and_then(|answer| {
        host.answer(&answer)?;
        host.open_devices()?;
        let mut muted = false;
        host.controls(muted, false)?;
        let _ = events.send(Event::Listening);
        loop {
            let state = control.load(Ordering::Acquire);
            if state == CANCELLED {
                return Ok(());
            }
            if muted != (state == RELEASED) {
                muted = state == RELEASED;
                host.controls(muted, false)?;
            }
            let (microphone, speaker) = host.peaks()?;
            let _ = events.send(Event::Levels {
                microphone: if muted { 0 } else { microphone },
                speaker,
            });
            thread::sleep(SAMPLE);
        }
    });
    stop();
    host.close();
    result
}

fn dictate_with(control: &AtomicU8, events: &mpsc::Sender<Event>) -> anyhow::Result<()> {
    // WebRTC negotiation dominates startup, so start Codex alongside the helper.
    let codex = thread::spawn(signal::Local::open);
    let mut host = host::Host::start()?;
    let offer = host.offer()?;
    let mut codex = codex
        .join()
        .map_err(|_| anyhow::anyhow!("Codex voice setup failed"))??;
    if control.load(Ordering::Acquire) == CANCELLED {
        return Ok(());
    }
    let answer = codex.start(&offer)?;
    let result = (|| {
        host.answer(&answer)?;
        if control.load(Ordering::Acquire) == CANCELLED {
            return Ok(());
        }
        host.open_devices()?;
        host.controls(false, true)?;
        let _ = events.send(Event::Listening);
        listen(&mut host, &mut codex, control, events)
    })();
    codex.stop();
    host.close();
    result
}

fn listen(
    host: &mut host::Host,
    codex: &mut signal::Local,
    control: &AtomicU8,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let mut released = None;
    let mut speaking = false;
    loop {
        match control.load(Ordering::Acquire) {
            CANCELLED => return Ok(()),
            RELEASED if released.is_none() => {
                // Stop capturing at once; words already sent still finish.
                host.controls(true, true)?;
                released = Some(Instant::now());
            }
            _ => {}
        }
        let (microphone, _) = host.peaks()?;
        let _ = events.send(Event::Levels {
            microphone,
            speaker: 0,
        });
        for notification in codex.poll()? {
            let params = notification.get("params").cloned().unwrap_or_default();
            let text = |key| {
                params
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let user = params.get("role").and_then(serde_json::Value::as_str) == Some("user");
            match signal::method(&notification) {
                "thread/realtime/transcript/delta" if user => {
                    speaking = true;
                    let _ = events.send(Event::Delta(Role::User, text("delta")));
                }
                "thread/realtime/transcript/done" if user => {
                    speaking = false;
                    let _ = events.send(Event::Segment(Role::User, text("text")));
                }
                "thread/realtime/error" => anyhow::bail!("Codex voice failed: {}", text("message")),
                "thread/realtime/closed" => anyhow::bail!("Codex closed the voice connection"),
                _ => {}
            }
        }
        if let Some(at) = released {
            let elapsed = at.elapsed();
            if elapsed >= FINISH || (elapsed >= SETTLE && !speaking) {
                return Ok(());
            }
        }
        thread::sleep(SAMPLE);
    }
}

/// Live check against the installed Codex and your login: an isolated difu
/// service negotiates voice on a real Codex session. It never opens the
/// microphone or speaker. Run with `cargo test -- --ignored live_voice`.
#[cfg(test)]
#[test]
#[ignore = "needs a Codex login, network access and a debug build of difu"]
fn live_voice_negotiates_through_the_session_service() -> anyhow::Result<()> {
    use crate::agents::{Job, Launch, Reply, Request, VoiceAction, client};
    use std::process::Command;
    let dir = tempfile::tempdir_in("/tmp")?;
    let storage = crate::storage::Storage {
        config: dir.path().join("config.json"),
        cache: dir.path().join("cache"),
    };
    let repo = dir.path().join("repo");
    let origin = dir.path().join("origin.git");
    std::fs::create_dir_all(&repo)?;
    let origin_path = origin.to_string_lossy().into_owned();
    anyhow::ensure!(
        Command::new("git")
            .args(["init", "-q", "--bare", &origin_path])
            .status()?
            .success()
    );
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
        &["remote", "add", "origin", &origin_path],
        &["push", "-q", "origin", "main"],
    ] {
        anyhow::ensure!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()?
                .success()
        );
    }
    let mut service =
        Command::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/difu"))
            .arg("--agent-service")
            .arg("--agent-config")
            .arg(&storage.config)
            .arg("--agent-cache")
            .arg(&storage.cache)
            .spawn()?;
    let result = (|| -> anyhow::Result<()> {
        let ready = Instant::now();
        while client::request(&storage, Request::Ping).is_err() {
            anyhow::ensure!(
                ready.elapsed() < Duration::from_secs(10),
                "service did not start"
            );
            thread::sleep(Duration::from_millis(100));
        }
        let Reply::Launched(id) = client::request(
            &storage,
            Request::Launch {
                job: Box::new(Job::Coding(Launch {
                    repository: repo.clone(),
                    isolated: false,
                    base: "HEAD".into(),
                    prompt: "Reply with the single word ready.".into(),
                    model: None,
                    effort: None,
                })),
            },
        )?
        else {
            anyhow::bail!("no session launched");
        };
        let session =
            |storage: &crate::storage::Storage| -> anyhow::Result<crate::agents::Session> {
                match client::request(
                    storage,
                    Request::Read {
                        id: id.clone(),
                        version: None,
                    },
                )? {
                    Reply::Session(session) => Ok(*session),
                    _ => anyhow::bail!("no session"),
                }
            };
        let started = Instant::now();
        while session(&storage)?.thread_id.is_none() || session(&storage)?.status.active() {
            let current = session(&storage)?;
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(120)
                    && current.status != crate::agents::Status::Failed,
                "session did not settle: {:?} thread {:?} error {:?} last {:?}",
                current.status,
                current.thread_id,
                current.error,
                current
                    .entries
                    .last()
                    .map(|e| (e.kind.clone(), e.text.to_string()))
            );
            thread::sleep(Duration::from_millis(500));
        }
        let mut helper = host::Host::start()?;
        let offer = helper.offer()?;
        let Reply::Voice(Some(answer)) = client::request(
            &storage,
            Request::Voice {
                id: id.clone(),
                action: VoiceAction::Start { offer },
            },
        )?
        else {
            anyhow::bail!("no voice answer");
        };
        helper.answer(&answer)?;
        anyhow::ensure!(
            session(&storage)?.voice,
            "session should show voice connected"
        );
        client::request(
            &storage,
            Request::Voice {
                id: id.clone(),
                action: VoiceAction::Stop,
            },
        )?;
        helper.close();
        let stopped = Instant::now();
        while session(&storage)?.voice {
            anyhow::ensure!(
                stopped.elapsed() < Duration::from_secs(10),
                "voice did not close"
            );
            thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    })();
    let _ = service.kill();
    let _ = service.wait();
    result
}

#[cfg(test)]
pub(crate) fn fixture() -> (Session, mpsc::Sender<Event>) {
    let (sender, events) = mpsc::channel();
    (
        Session {
            events,
            control: Arc::new(AtomicU8::new(LIVE)),
        },
        sender,
    )
}
