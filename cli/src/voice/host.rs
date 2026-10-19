//! Codex's packaged voice helper owns WebRTC, the microphone and the speaker,
//! so audio never passes through difu: the helper reports only peak levels.
//! difu drives it over the framed-JSON pipe protocol Codex's own TUI uses.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufReader, Read, Write},
    path::PathBuf,
    process::{ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const MAX_FRAME: usize = 128 * 1024;
const QUICK: Duration = Duration::from_secs(5);
const NEGOTIATION: Duration = Duration::from_secs(20);
const STARTUP: Duration = Duration::from_secs(30);
const UNSUPPORTED: &str =
    "This Codex install has no voice support; update Codex (brew upgrade --cask codex)";

pub(crate) struct Host {
    _child: crate::process::ChildGroup,
    input: ChildStdin,
    replies: mpsc::Receiver<Value>,
}

/// The installed Codex package root, its helper and the build they shipped in.
pub(super) struct Package {
    root: PathBuf,
    helper: PathBuf,
    commit: String,
}

pub(super) fn package() -> Result<Package> {
    let codex = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ])
        .map(|dir| dir.join("codex"))
        .find(|path| path.is_file())
        .context("Voice needs the Codex CLI; install it and run codex login")?
        .canonicalize()?;
    let root = codex
        .ancestors()
        .skip(1)
        .take(4)
        .find(|dir| dir.join("codex-package.json").is_file())
        .context(UNSUPPORTED)?
        .to_owned();
    let voice = root.join("codex-resources/voice");
    let helper = voice.join("bin/codex-voice-host");
    ensure!(helper.is_file(), UNSUPPORTED);
    let manifest: Value =
        serde_json::from_slice(&fs::read(voice.join("manifest.json")).context(UNSUPPORTED)?)?;
    let commit = manifest
        .get("buildCommit")
        .and_then(Value::as_str)
        .context(UNSUPPORTED)?
        .to_owned();
    Ok(Package {
        root,
        helper,
        commit,
    })
}

/// The helper gets only what audio, networking and proxies need, plus the
/// settings that stop GStreamer from loading system plugins.
fn environment() -> Vec<(String, String)> {
    const KEEP: [&str; 14] = [
        "HOME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_RUNTIME_DIR",
        "PULSE_SERVER",
        "PULSE_COOKIE",
        "PIPEWIRE_REMOTE",
        "DBUS_SESSION_BUS_ADDRESS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
    ];
    std::env::vars()
        .filter(|(key, _)| KEEP.contains(&key.to_ascii_uppercase().as_str()))
        .chain(
            [
                ("GST_PLUGIN_PATH", ""),
                ("GST_PLUGIN_PATH_1_0", ""),
                ("GST_PLUGIN_SYSTEM_PATH", ""),
                ("GST_PLUGIN_SYSTEM_PATH_1_0", ""),
                ("GST_REGISTRY", "/dev/null"),
                ("GST_REGISTRY_UPDATE", "no"),
                ("GST_REGISTRY_FORK", "no"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned())),
        )
        .collect()
}

fn read_frame(reader: &mut impl Read) -> Result<Value> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(length <= MAX_FRAME, "Invalid voice helper message");
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}

impl Host {
    /// Starts the helper and its audio runtime. No device is opened yet.
    pub(crate) fn start() -> Result<Self> {
        let package = package()?;
        let mut child = crate::process::ChildGroup::spawn(
            Command::new(&package.helper)
                .current_dir(&package.root)
                .env_clear()
                .envs(environment())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .context("Cannot start Codex's voice helper")?;
        let input = child
            .child
            .stdin
            .take()
            .context("Missing voice helper input")?;
        let output = child
            .child
            .stdout
            .take()
            .context("Missing voice helper output")?;
        let (sender, replies) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            while let Ok(frame) = read_frame(&mut reader) {
                if sender.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut host = Self {
            _child: child,
            input,
            replies,
        };
        host.exchange(
            json!({"type":"hello","protocol":1,"buildCommit":package.commit}),
            "ready",
            STARTUP,
        )?;
        host.exchange(json!({"type":"initializeRuntime"}), "runtimeReady", STARTUP)?;
        Ok(host)
    }
    /// Gathers the WebRTC offer that Codex's app-server forwards to OpenAI.
    pub(crate) fn offer(&mut self) -> Result<String> {
        let reply = self.request(json!({"type":"startTransport"}), NEGOTIATION)?;
        ensure!(kind(&reply) == "offer", "Unexpected voice helper reply");
        reply
            .get("sdp")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("Voice helper returned no offer")
    }
    /// Returns once the peer connection's event channel is open.
    pub(crate) fn answer(&mut self, sdp: &str) -> Result<()> {
        let reply = self.request(json!({"type":"applyAnswer","sdp":sdp}), NEGOTIATION)?;
        match kind(&reply) {
            "transportReady" => Ok(()),
            "transportTimedOut" => bail!("The voice connection timed out; try again"),
            _ => bail!("Unexpected voice helper reply"),
        }
    }
    /// Opens the default microphone and speaker.
    pub(crate) fn open_devices(&mut self) -> Result<()> {
        self.exchange(
            json!({"type":"openDevices","selection":{"microphone":null,"speaker":null,"channel":null}}),
            "devicesOpened",
            QUICK,
        )
        .context("Cannot open the microphone or speaker; check macOS Sound and Privacy settings")
    }
    pub(crate) fn controls(
        &mut self,
        microphone_muted: bool,
        speaker_suppressed: bool,
    ) -> Result<()> {
        self.exchange(
            json!({"type":"setAudioControls","controls":{"microphoneMuted":microphone_muted,"speakerSuppressed":speaker_suppressed}}),
            "audioControlsApplied",
            QUICK,
        )
    }
    /// Microphone and speaker peaks since the previous call.
    pub(crate) fn peaks(&mut self) -> Result<(u16, u16)> {
        let reply = self.request(json!({"type":"inspectAudio"}), QUICK)?;
        ensure!(
            kind(&reply) == "audioState",
            "Unexpected voice helper reply"
        );
        let peak = |key| {
            reply
                .pointer(key)
                .and_then(Value::as_u64)
                .map_or(0, |peak| u16::try_from(peak).unwrap_or(u16::MAX))
        };
        Ok((peak("/state/microphonePeak"), peak("/state/speakerPeak")))
    }
    /// Closes devices and the connection; dropping the host kills it regardless.
    pub(crate) fn close(mut self) {
        let _ = self.exchange(json!({"type":"close"}), "closed", QUICK);
    }
    fn exchange(&mut self, request: Value, expected: &str, deadline: Duration) -> Result<()> {
        let reply = self.request(request, deadline)?;
        ensure!(kind(&reply) == expected, "Unexpected voice helper reply");
        Ok(())
    }
    fn request(&mut self, request: Value, deadline: Duration) -> Result<Value> {
        let payload = serde_json::to_vec(&request)?;
        let length = u32::try_from(payload.len())?;
        self.input
            .write_all(&length.to_be_bytes())
            .and_then(|()| self.input.write_all(&payload))
            .and_then(|()| self.input.flush())
            .context("Codex's voice helper stopped")?;
        match self.replies.recv_timeout(deadline) {
            Ok(reply) => Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("Codex's voice helper stopped responding")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("Codex's voice helper stopped"),
        }
    }
}

fn kind(reply: &Value) -> &str {
    reply
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}
