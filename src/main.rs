use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use difu::{github, model::PrKey, process::Cancel, shell::Shell, storage::Storage};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "Your diff shifu · Codex agents and guided PR reviews"
)]
struct Args {
    /// Optional GitHub PR URL or number (numbers use the current repository).
    pr: Option<String>,
    #[arg(long, hide = true)]
    agent_service: bool,
    #[arg(long, hide = true)]
    agent_config: Option<std::path::PathBuf>,
    #[arg(long, hide = true)]
    agent_cache: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.agent_service {
        nix::unistd::setsid().context("Could not detach the background agent service")?;
        let storage = match (args.agent_config, args.agent_cache) {
            (Some(config), Some(cache)) => Storage { config, cache },
            _ => Storage::discover()?,
        };
        return difu::agents::server::run(storage);
    }
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "difu needs an interactive terminal. Run `difu` directly in your terminal."
    );
    let pr = match args.pr {
        Some(value) if value.chars().all(|c| c.is_ascii_digit()) => {
            let repository = github::current_repository(&Cancel::default())?;
            let (owner, repo) = repository
                .split_once('/')
                .context("Invalid GitHub repository")?;
            let key = PrKey {
                owner: owner.into(),
                repo: repo.into(),
                number: value.parse()?,
            };
            key.validate()?;
            Some(key)
        }
        Some(url) => Some(PrKey::from_url(&url)?),
        None => None,
    };
    let storage = Storage::discover()?;
    let config = storage.load_config()?;
    let mut app = Shell::new(storage, config, pr);
    let terminated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        signal_hook::flag::register(signal, terminated.clone())?;
    }
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error.into());
        }
    };
    let prior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            io::stdout(),
            SetCursorStyle::DefaultUserShape,
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableFocusChange,
            DisableBracketedPaste
        );
        ratatui::restore();
        prior(info);
    }));
    let result = (|| -> Result<()> {
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
        app.reviews.images.detect();
        while !app.reviews.quit && !terminated.load(std::sync::atomic::Ordering::Relaxed) {
            app.tick();
            app.clipboard(&mut io::stdout().lock());
            let frame = terminal.draw(|frame| app.draw(frame))?;
            app.reviews
                .hover
                .render(&mut io::stdout().lock(), frame.buffer)?;
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) => app.key(key),
                    Event::FocusLost => {
                        app.reviews.hover = Default::default();
                        app.agents.cancel_voice();
                    }
                    Event::Mouse(mouse) => app.mouse(mouse),
                    Event::Paste(text) => app.paste(text),
                    Event::Resize(..) => app.reviews.invalidate(),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    let _ = execute!(
        io::stdout(),
        SetCursorStyle::DefaultUserShape,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableFocusChange,
        DisableBracketedPaste
    );
    ratatui::restore();
    app.reviews.shutdown();
    result
}
