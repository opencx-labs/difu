//! Frontend-only dictation. Credentials and audio never enter session storage.
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
    mpsc,
};

pub enum Event {
    Listening,
    Level(f32),
    Partial(String),
    Done(String),
    Error(String),
}
pub struct Recording {
    pub events: mpsc::Receiver<Event>,
    control: Arc<AtomicU8>, // 0 recording, 1 finish, 2 cancel
}
impl Recording {
    pub fn finish(&self) {
        self.control.store(1, Ordering::Release);
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        self.control.store(2, Ordering::Release);
    }
}
pub fn start(key: Option<String>) -> Recording {
    let (sender, events) = mpsc::channel();
    let control = Arc::new(AtomicU8::new(0));
    let signal = control.clone();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let result = credential(key).and_then(|key| macos::run(key, &signal, &sender));
        #[cfg(not(target_os = "macos"))]
        let result: anyhow::Result<()> = {
            let _ = (key, signal);
            Err(anyhow::anyhow!(
                "Voice dictation is available on macOS in this release"
            ))
        };
        if let Err(error) = result {
            let _ = sender.send(Event::Error(format!("{error:#}")));
        }
    });
    Recording { events, control }
}
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
fn credential(entered: Option<String>) -> anyhow::Result<String> {
    use anyhow::Context;
    if let Some(key) = entered
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(key);
    }
    let bytes =
        security_framework::passwords::get_generic_password("app.difu.voice", "openai-api-key")
            .map_err(|_| {
                anyhow::anyhow!(
                    "No accessible voice API key. Add one in /voice or set OPENAI_API_KEY."
                )
            })?;
    String::from_utf8(bytes).context("Stored voice API key is not valid text")
}
pub fn save_key(key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !key.trim().is_empty() && !key.chars().any(char::is_control),
        "Enter a valid API key"
    );
    #[cfg(target_os = "macos")]
    return security_framework::passwords::set_generic_password(
        "app.difu.voice",
        "openai-api-key",
        key.trim().as_bytes(),
    )
    .map_err(|_| {
        anyhow::anyhow!(
            "Could not save the API key in macOS Keychain. Check Keychain access and try again."
        )
    });
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Voice API-key settings are available on macOS in this release")
}
#[cfg(test)]
pub(crate) fn fixture() -> (Recording, mpsc::Sender<Event>) {
    let (sender, events) = mpsc::channel();
    (
        Recording {
            events,
            control: Arc::new(AtomicU8::new(0)),
        },
        sender,
    )
}
