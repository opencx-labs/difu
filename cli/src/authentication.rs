//! Temporarily hand the foreground terminal to an explicitly requested native CLI.
use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use std::{
    io,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct Signals {
    pub terminated: Arc<AtomicBool>,
    pub interrupted: Arc<AtomicBool>,
}
impl Signals {
    pub fn register() -> Result<Self> {
        let signals = Self {
            terminated: Arc::new(AtomicBool::new(false)),
            interrupted: Arc::new(AtomicBool::new(false)),
        };
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
            signal_hook::flag::register(signal, signals.terminated.clone())?;
        }
        signal_hook::flag::register(signal_hook::consts::SIGINT, signals.interrupted.clone())?;
        Ok(signals)
    }
    pub fn quitting(&self) -> bool {
        self.terminated.load(Ordering::Relaxed) || self.interrupted.load(Ordering::Relaxed)
    }
}
pub fn enable() -> Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        ),
        SetCursorStyle::BlinkingBlock,
        EnableMouseCapture,
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    Ok(())
}
pub fn disable() -> Result<()> {
    execute!(
        io::stdout(),
        SetCursorStyle::DefaultUserShape,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableFocusChange,
        DisableBracketedPaste
    )?;
    Ok(())
}
pub fn command(provider: difu::agents::provider::Provider) -> Command {
    let mut command = Command::new(provider.key());
    command.args(match provider {
        difu::agents::provider::Provider::Codex => vec!["login", "--device-auth"],
        difu::agents::provider::Provider::Claude => vec!["auth", "login"],
    });
    command
}
pub fn run(
    terminal: &mut ratatui::DefaultTerminal,
    command: &mut Command,
    signals: &Signals,
    tick: impl FnMut(),
) -> Result<(bool, bool)> {
    // An interrupt received before the explicit handoff retains ordinary quit semantics.
    if signals.quitting() {
        return Ok((false, false));
    }
    let suspended = disable();
    ratatui::restore();
    let outcome =
        suspended.and_then(|()| difu::process::foreground(command, || signals.quitting(), tick));
    // Only SIGINT received while the native CLI owned input is consumed here.
    let cancelled = signals.interrupted.swap(false, Ordering::Relaxed);
    let restored = (|| -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
        *terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
        enable()?;
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0)
        )?;
        Ok(())
    })();
    if restored.is_err() {
        let _ = disable();
        ratatui::restore();
    }
    restored?;
    outcome.map(|success| (success, cancelled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, ensure};
    use std::{
        fs,
        time::{Duration, Instant},
    };
    #[test]
    fn auth_terminal_lifecycle_uses_real_pty() -> Result<()> {
        let output = Command::new("python3")
            .args(["-c", include_str!("../tests/fixtures/authentication.py")])
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
    fn auth_terminal_child() -> Result<()> {
        let Ok(mode) = std::env::var("DIFU_AUTH_TEST_MODE") else {
            return Ok(());
        };
        let root =
            std::path::PathBuf::from(std::env::var_os("DIFU_AUTH_TEST_ROOT").context("root")?);
        let signals = Signals::register()?;
        let mut terminal = ratatui::try_init()?;
        enable()?;
        let mut command = Command::new(if mode == "spawn-failure" {
            root.join("missing")
        } else {
            root.join("fake-login")
        });
        let native = super::command(if mode == "claude-success" {
            difu::agents::provider::Provider::Claude
        } else {
            difu::agents::provider::Provider::Codex
        });
        command.args(native.get_args());
        let start = Instant::now();
        let result = run(&mut terminal, &mut command, &signals, || {});
        assert!(crossterm::terminal::is_raw_mode_enabled()?);
        match mode.as_str() {
            "success" | "claude-success" => assert_eq!(result?, (true, false)),
            "failure" => assert_eq!(result?, (false, false)),
            "cancel" => {
                assert_eq!(result?, (false, true));
                assert!(!signals.quitting());
            }
            "hup" | "term" => {
                assert_eq!(result?, (false, false));
                assert!(signals.quitting());
            }
            "spawn-failure" => assert!(result.is_err()),
            _ => anyhow::bail!("unknown case"),
        }
        ensure!(
            start.elapsed() < Duration::from_secs(4),
            "Auth cancellation exceeded bound"
        );
        disable()?;
        ratatui::restore();
        fs::write(root.join("restored"), "ok")?;
        Ok(())
    }
}
