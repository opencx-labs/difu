use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
};
use difu::{app::App, github, model::PrKey, process::Cancel, storage::Storage};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "Your diff shifu · a terminal inbox and guided PR reviewer"
)]
struct Args {
    /// Optional GitHub PR URL or number (numbers use the current repository).
    pr: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
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
    let mut app = App::new(storage, config);
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
            DisableBracketedPaste
        );
        ratatui::restore();
        prior(info);
    }));
    let result = (|| -> Result<()> {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            SetCursorStyle::BlinkingBar,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        app.images.detect();
        app.start(pr);
        while !app.quit && !terminated.load(std::sync::atomic::Ordering::Relaxed) {
            app.tick();
            terminal.draw(|frame| difu::ui::draw(frame, &mut app))?;
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => app.key_event(key),
                    Event::Mouse(mouse) => app.mouse(mouse),
                    Event::Paste(text) => app.paste(text),
                    Event::Resize(..) => app.invalidate(),
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
        DisableBracketedPaste
    );
    ratatui::restore();
    app.shutdown();
    result
}
