//! Regression coverage for the terminal dependency's dead-TTY read loop.
use anyhow::{Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal,
};
use std::{fs, process::Command, sync::Arc, sync::atomic::AtomicBool, time::Duration};

#[test]
fn terminal_disconnect_does_not_trap_input_or_shutdown() -> Result<()> {
    let output = Command::new("python3")
        .arg("-c")
        .arg(include_str!("fixtures/terminal_disconnect.py"))
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
fn terminal_input_child() -> Result<()> {
    let Ok(mode) = std::env::var("DIFU_TEST_TERMINAL_MODE") else {
        return Ok(());
    };
    // Match difu's cooperative signal handling: hangup sets a flag rather
    // than killing the process and hiding a stuck dependency read.
    let terminated = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGHUP, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, terminated.clone())?;
    }
    terminal::enable_raw_mode()?;
    event::poll(Duration::ZERO)?;
    fs::write(std::env::var("DIFU_TEST_TERMINAL_READY")?, "ready")?;
    match mode.as_str() {
        "poll" | "poll-no-hangup" => {
            ensure!(
                event::poll(Duration::from_secs(5)).is_err(),
                "A disconnected terminal must return an error"
            );
        }
        "read" => {
            ensure!(event::read().is_err(), "Reading a dead terminal must fail");
        }
        "cursor" => {
            ensure!(
                crossterm::cursor::position().is_err(),
                "A cursor query on a dead terminal must fail"
            );
        }
        "ctrl-c" => {
            ensure!(event::poll(Duration::from_secs(5))?, "Missing Ctrl+C");
            ensure!(
                matches!(event::read()?, Event::Key(key)
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)),
                "Ctrl+C must still arrive as a key"
            );
            terminal::disable_raw_mode()?;
        }
        _ => anyhow::bail!("Unknown terminal test mode"),
    }
    // The test runner cannot print its result to revoked stdout after hangup.
    // Exit only after the assertions succeed; the parent checks this exit code.
    std::process::exit(0)
}
