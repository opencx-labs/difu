//! Real PTY input and rendering with isolated storage and no provider/service tick.
use crate::{
    settings::{BindingKey, KeyChord, Motion},
    shell::Shell,
    storage::{Config, Storage},
};
use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
};
use std::{fs, io, path::PathBuf, process::Command, time::Duration};

#[test]
fn settings_terminal_lifecycle_uses_real_pty() -> Result<()> {
    let output = Command::new("python3")
        .args([
            "-c",
            include_str!("../../../tests/fixtures/settings_terminal.py"),
        ])
        .arg(std::env::current_exe()?)
        .output()?;
    ensure!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn settings_terminal_child() -> Result<()> {
    let Some(directory) = std::env::var_os("DIFU_SETTINGS_PTY_ROOT") else {
        return Ok(());
    };
    let root = PathBuf::from(directory);
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.join("cache"),
    };
    let mut config = Config::default();
    config.preferences.keyboard.bindings.insert(
        "settings.open".into(),
        KeyChord {
            key: BindingKey::Function(12),
            modifiers: Default::default(),
        },
    );
    storage.update_config(|saved| *saved = config.clone())?;
    let mut shell = Shell::new(storage.clone(), config, None);
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let result = (|| -> Result<()> {
        terminal.draw(|frame| shell.draw(frame))?;
        fs::write(root.join("phase"), "0")?;
        let mut replay = None;
        for step in 0..13 {
            loop {
                ensure!(
                    event::poll(Duration::from_secs(5))?,
                    "Missing PTY input at {step}"
                );
                match event::read()? {
                    Event::Key(key) => shell.key(key),
                    Event::Paste(value) => shell.paste(value),
                    _ => continue,
                }
                break;
            }
            let should_open = matches!(step, 1..=8 | 11);
            ensure!(
                shell.settings.is_some() == should_open,
                "Settings visibility at {step}"
            );
            if let Some(panel) = shell.settings.as_ref() {
                match step {
                    2 => {
                        assert_eq!(panel.query, "stop flashing");
                        assert_eq!(
                            panel.rows().first().context("search row")?.id,
                            super::SettingId::Motion
                        );
                    }
                    4 | 6 | 8 | 11 => assert_eq!(
                        panel.draft.config().preferences.motion.level,
                        Motion::Reduced
                    ),
                    7 => assert_eq!(panel.draft.config().preferences.motion.level, Motion::Off),
                    _ => {}
                }
                if step == 4 {
                    replay = Some(panel.replay);
                    assert!(!panel.draft.edited().is_empty());
                }
                if step == 5 {
                    ensure!(
                        panel.replay > replay.context("previous replay")?,
                        "Preview was not replayed"
                    );
                }
                if matches!(step, 6 | 8 | 11) {
                    assert!(panel.draft.edited().is_empty());
                }
            }
            assert_eq!(
                storage.load_config()?.preferences.motion.level,
                if step >= 6 {
                    Motion::Reduced
                } else {
                    Motion::Full
                }
            );
            let frame = terminal.draw(|frame| shell.draw(frame))?;
            let text: String = frame
                .buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            if should_open {
                ensure!(
                    text.contains("Settings"),
                    "Settings was not rendered at {step}"
                );
            }
            fs::write(root.join("phase"), (step + 1).to_string())?;
        }
        Ok(())
    })();
    execute!(io::stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    result?;
    fs::write(root.join("restored"), "ok")?;
    Ok(())
}
