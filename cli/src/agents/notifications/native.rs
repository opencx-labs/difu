//! Native platform boundary; no shell interpolation or implicit permission request.
use super::{Capabilities, Delivery, Event};
use anyhow::{Context, Result};
#[cfg(any(target_os = "macos", test))]
use std::path::Path;
use std::path::PathBuf;
pub struct Native;
#[cfg(any(target_os = "macos", test))]
fn helper(executable: &Path) -> Result<PathBuf> {
    let executable = executable
        .canonicalize()
        .context("Cannot locate difu executable")?;
    let directory = executable
        .parent()
        .context("Missing executable directory")?;
    let bundle = "DifuNotifications.app/Contents/MacOS/DifuNotifications";
    [
        directory.join(bundle),
        directory.join("../libexec").join(bundle),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .context("Bundled notification helper unavailable; reinstall the complete difu package")?
    .canonicalize()
    .context("Cannot resolve notification helper")
}
fn bounded(command: &mut std::process::Command, input: Vec<u8>, seconds: u64) -> Result<Vec<u8>> {
    let cancel = crate::process::Cancel::default();
    let (done, wait) = std::sync::mpsc::channel();
    let output = std::thread::scope(|scope| {
        let token = cancel.clone();
        scope.spawn(move || {
            if wait
                .recv_timeout(std::time::Duration::from_secs(seconds))
                .is_err()
            {
                token.cancel();
            }
        });
        let output = crate::process::run(command, Some(input), &cancel);
        let _ = done.send(());
        output
    })?;
    anyhow::ensure!(
        output.code == 0,
        "Notification helper failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
#[cfg(target_os = "macos")]
fn call(path: &Path, action: &str, event: Option<&Event>) -> Result<Capabilities> {
    let request = serde_json::json!({"action":action,"title":event.map(|e|e.title.as_str()),"event":event.map(|e|e.kind.label())});
    let bytes = bounded(
        &mut std::process::Command::new(path),
        serde_json::to_vec(&request)?,
        if action == "authorize" { 30 } else { 5 },
    )?;
    serde_json::from_slice(&bytes).context("Invalid notification helper reply")
}
#[cfg(target_os = "macos")]
fn invoke(action: &str, event: Option<&Event>) -> Result<Capabilities> {
    call(&helper(&std::env::current_exe()?)?, action, event)
}
#[cfg(target_os = "macos")]
impl Delivery for Native {
    fn status(&mut self) -> Result<Capabilities> {
        Ok(invoke("status", None).unwrap_or_else(|error| Capabilities {
            desktop: "Unavailable".into(),
            sound: "Unavailable".into(),
            error: Some(format!("{error:#}")),
        }))
    }
    fn authorize(&mut self) -> Result<Capabilities> {
        invoke("authorize", None)
    }
    fn desktop(&mut self, event: &Event) -> Result<()> {
        let response = invoke("deliver", Some(event))?;
        anyhow::ensure!(
            response.error.is_none(),
            "{}",
            response.error.unwrap_or_default()
        );
        Ok(())
    }
    fn sound(&mut self) -> Result<()> {
        let response = invoke("sound", None)?;
        anyhow::ensure!(
            response.error.is_none(),
            "{}",
            response.error.unwrap_or_default()
        );
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn paplay() -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("paplay"))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .context("Standalone sound unavailable: paplay is not installed")
}
#[cfg(any(target_os = "linux", test))]
fn wave() -> Vec<u8> {
    // A short, quiet sine tone generated locally; no user audio or external assets.
    let samples = 4410u32;
    let data = samples * 2;
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((data + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(44100u32.to_le_bytes());
    wav.extend(88200u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data.to_le_bytes());
    for n in 0..samples {
        let envelope = (std::f32::consts::PI * n as f32 / samples as f32).sin();
        let sample =
            (3000.0 * envelope * (std::f32::consts::TAU * 660.0 * n as f32 / 44100.0).sin()) as i16;
        wav.extend(sample.to_le_bytes());
    }
    wav
}
#[cfg(any(target_os = "linux", test))]
fn play(path: &std::path::Path) -> Result<()> {
    bounded(
        std::process::Command::new(path).args([
            "--client-name=Difu",
            "--stream-name=Notification",
            "--file-format=wav",
        ]),
        wave(),
        5,
    )
    .context("Standalone sound playback failed")?;
    Ok(())
}
#[cfg(target_os = "linux")]
impl Delivery for Native {
    fn status(&mut self) -> Result<Capabilities> {
        let desktop = notify_rust::get_capabilities();
        Ok(Capabilities {
            desktop: if desktop.is_ok() {
                "Available"
            } else {
                "Unavailable"
            }
            .into(),
            sound: if paplay().is_ok() {
                "paplay available; playback checked on test"
            } else {
                "Unavailable: install paplay for standalone sound"
            }
            .into(),
            error: desktop
                .err()
                .map(|e| format!("Desktop notifications unavailable: {e}")),
        })
    }
    fn authorize(&mut self) -> Result<Capabilities> {
        self.status()
    }
    fn desktop(&mut self, event: &Event) -> Result<()> {
        notify_rust::Notification::new()
            .appname("Difu")
            .summary(&event.title)
            .body(event.kind.label())
            .hint(notify_rust::Hint::SuppressSound(true))
            .timeout(5000)
            .show()
            .context("Desktop notification delivery failed")?;
        Ok(())
    }
    fn sound(&mut self) -> Result<()> {
        play(&paplay()?)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_helper_protocol_is_structured_and_reports_denied_or_malformed_replies() -> Result<()> {
        use crate::process::fixture::{Expected, WorkerScope};
        let temp = tempfile::tempdir()?;
        let storage = crate::storage::Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let fixture=WorkerScope::new(storage,vec![Expected{program:"/not-a-real-notification-helper".into(),args:vec![],input:Some(br#"{"action":"deliver","event":"Question or approval needed","title":"Example; $(touch forbidden)"}"#.to_vec()),stdout:br#"{"desktop":"Denied","sound":"Available","error":"Permission denied"}"#.to_vec(),code:0,execute_local_git:false},Expected{program:"/not-a-real-notification-helper".into(),args:vec![],input:Some(br#"{"action":"status","event":null,"title":null}"#.to_vec()),stdout:b"invalid".to_vec(),code:0,execute_local_git:false}]);
        let event = Event {
            identity: "private identity".into(),
            session: "private session".into(),
            title: "Example; $(touch forbidden)".into(),
            kind: super::super::Kind::Question,
        };
        let reply = call(
            Path::new("/not-a-real-notification-helper"),
            "deliver",
            Some(&event),
        )?;
        assert_eq!(reply.desktop, "Denied");
        assert_eq!(reply.error.as_deref(), Some("Permission denied"));
        assert!(call(Path::new("/not-a-real-notification-helper"), "status", None).is_err());
        fixture.verify()?;
        Ok(())
    }
    #[test]
    fn standalone_audio_process_receives_only_generated_wav_and_reports_playback_failure()
    -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let player = temp.path().join("paplay");
        std::fs::write(&player,b"#!/bin/sh\nbase=$(dirname -- \"$0\")\nprintf '%s\\n' \"$@\" > \"$base/args\"\ncat > \"$base/audio.wav\"\n")?;
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o700))?;
        play(&player)?;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("args"))?,
            "--client-name=Difu\n--stream-name=Notification\n--file-format=wav\n"
        );
        let bytes = std::fs::read(temp.path().join("audio.wav"))?;
        assert_eq!(bytes.len(), 8864);
        assert_eq!(bytes.get(..4), Some(b"RIFF".as_slice()));
        assert_eq!(bytes.get(8..16), Some(b"WAVEfmt ".as_slice()));
        assert_eq!(bytes.get(22..24), Some([1, 0].as_slice()));
        assert_eq!(bytes.get(24..28), Some([68, 172, 0, 0].as_slice()));
        std::fs::write(
            &player,
            b"#!/bin/sh\ncat >/dev/null\necho 'audio server unavailable' >&2\nexit 1\n",
        )?;
        assert!(
            play(&player)
                .err()
                .is_some_and(|e| format!("{e:#}").contains("audio server unavailable"))
        );
        assert!(play(&temp.path().join("missing-player")).is_err());
        Ok(())
    }
    #[test]
    fn helper_follows_executable_symlink_into_relocated_cellar_and_archive() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("archive/difu");
        std::fs::create_dir_all(executable.parent().context("parent")?)?;
        std::fs::write(&executable, [])?;
        let app = temp
            .path()
            .join("archive/DifuNotifications.app/Contents/MacOS/DifuNotifications");
        std::fs::create_dir_all(app.parent().context("parent")?)?;
        std::fs::write(&app, [])?;
        assert_eq!(helper(&executable)?, app.canonicalize()?);
        let cellar = temp.path().join("relocated/Cellar/difu/0.25/bin/difu");
        std::fs::create_dir_all(cellar.parent().context("parent")?)?;
        std::fs::write(&cellar, [])?;
        let app=temp.path().join("relocated/Cellar/difu/0.25/libexec/DifuNotifications.app/Contents/MacOS/DifuNotifications");
        std::fs::create_dir_all(app.parent().context("parent")?)?;
        std::fs::write(&app, [])?;
        let link = temp.path().join("difu");
        std::os::unix::fs::symlink(&cellar, &link)?;
        assert_eq!(helper(&link)?, app.canonicalize()?);
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests;
