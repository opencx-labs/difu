//! Public-event regression for Meta-prefixed terminal arrow sequences.
use anyhow::{Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal,
};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn meta_arrows_are_keys_instead_of_escape_navigation() -> Result<()> {
    let output = Command::new("python3")
        .arg("-c")
        .arg(include_str!("fixtures/terminal_meta.py"))
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
fn terminal_meta_child() -> Result<()> {
    let Ok(expected) = std::env::var("DIFU_TEST_META_EXPECTED") else {
        return Ok(());
    };
    terminal::enable_raw_mode()?;
    fs::write(std::env::var("DIFU_TEST_META_READY")?, "ready")?;
    if let Ok(prefix) = std::env::var("DIFU_TEST_META_PREFIX_SENT") {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !std::path::Path::new(&prefix).exists() {
            ensure!(Instant::now() < deadline, "Prefix was not sent");
            std::thread::sleep(Duration::from_millis(1));
        }
        for timeout in [Duration::ZERO, Duration::from_millis(1)] {
            if event::poll(timeout)? {
                let early = event::read()?;
                fs::write(
                    std::env::var("DIFU_TEST_META_RESULT")?,
                    format!("early {early:?}"),
                )?;
                anyhow::bail!("Incomplete Meta prefix produced {early:?} during {timeout:?} poll");
            }
        }
        fs::write(std::env::var("DIFU_TEST_META_PREFIX_READ")?, "read")?;
    }
    ensure!(
        event::poll(Duration::from_secs(2))?,
        "No terminal key arrived"
    );
    let received = event::read()?;
    fs::write(
        std::env::var("DIFU_TEST_META_RESULT")?,
        format!("{received:?}"),
    )?;
    let Event::Key(key) = received else {
        anyhow::bail!("Expected key, received {received:?}");
    };
    let wanted = match expected.as_str() {
        "alt-up" => (KeyCode::Up, KeyModifiers::ALT),
        "alt-down" => (KeyCode::Down, KeyModifiers::ALT),
        "up" => (KeyCode::Up, KeyModifiers::NONE),
        "shift-left" => (KeyCode::Left, KeyModifiers::SHIFT),
        "ctrl-enter" => (KeyCode::Enter, KeyModifiers::CONTROL),
        "backtab" => (KeyCode::BackTab, KeyModifiers::SHIFT),
        "esc" | "esc-then-up" => (KeyCode::Esc, KeyModifiers::NONE),
        _ => anyhow::bail!("Unknown expected key"),
    };
    ensure!(
        (key.code, key.modifiers) == wanted,
        "Expected {wanted:?}, received {key:?}"
    );
    if expected == "esc-then-up" {
        // The parent sends plain Up only after genuine Esc was delivered.
        fs::write(std::env::var("DIFU_TEST_META_PREFIX_READ")?, "read")?;
        ensure!(event::poll(Duration::from_secs(2))?, "Missing Up after Esc");
        let next = event::read()?;
        fs::write(
            std::env::var("DIFU_TEST_META_RESULT")?,
            format!("{received:?}; {next:?}"),
        )?;
        ensure!(
            matches!(next,Event::Key(key) if key.code == KeyCode::Up && key.modifiers.is_empty()),
            "Expected ordinary Up, received {next:?}"
        );
    }
    terminal::disable_raw_mode()?;
    std::process::exit(0)
}
