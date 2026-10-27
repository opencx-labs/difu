mod authentication;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, Event,
        PopKeyboardEnhancementFlags,
    },
    execute,
};
use difu::{github, model::PrKey, process::Cancel, shell::Shell, storage::Storage};
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    version,
    args_conflicts_with_subcommands = true,
    about = "Your diff shifu · Codex agents and guided PR reviews"
)]
struct Args {
    /// Optional GitHub PR URL or number (numbers use the current repository).
    pr: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(long, hide = true)]
    agent_service: bool,
    #[arg(long, hide = true, conflicts_with = "agent_service")]
    refresh_agent_service: bool,
    #[arg(long, hide = true)]
    agent_config: Option<std::path::PathBuf>,
    #[arg(long, hide = true)]
    agent_cache: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect a session's shared shells using difu_shell action JSON.
    Shell {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        session: String,
        /// JSON action, for example '{"action":"list"}'.
        #[arg(value_parser = parse_shell_action)]
        action: serde_json::Value,
    },
    /// Attach a GitHub pull request to a session, including another repository.
    AttachPr {
        /// Full GitHub pull request URL.
        #[arg(value_parser = PrKey::from_url)]
        url: PrKey,
        /// Difu session ID to receive the PR badge and diff tab.
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        session: String,
    },
    /// Open the pull request associated with the current Git branch.
    Pr {
        /// Use the repository and branch in the current directory.
        #[arg(value_parser = ["."])]
        target: String,
    },
    /// Open the local diff for the current repository or worktree.
    Diff {
        /// Use the checkout in the current directory.
        #[arg(value_parser = ["."])]
        target: String,
    },
}

fn parse_shell_action(text: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(text)
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.agent_service || args.refresh_agent_service {
        if args.agent_service {
            nix::unistd::setsid().context("Could not detach the background agent service")?;
        }
        let storage = match (args.agent_config, args.agent_cache) {
            (Some(config), Some(cache)) => Storage { config, cache },
            _ => Storage::discover()?,
        };
        return if args.refresh_agent_service {
            difu::agents::client::refresh_running(&storage)
        } else {
            difu::agents::server::run(storage)
        };
    }
    if let Some(Commands::Shell { session, action }) = &args.command {
        let storage = Storage::discover()?;
        difu::agents::client::ensure_running(&storage)?;
        match difu::agents::client::request(
            &storage,
            difu::agents::Request::ShellTool {
                id: session.clone(),
                args: action.clone(),
            },
        )? {
            difu::agents::Reply::Shell(value) => {
                println!("{}", serde_json::to_string_pretty(&value)?)
            }
            other => anyhow::bail!("Unexpected shell reply: {other:?}"),
        }
        return Ok(());
    }
    if let Some(Commands::AttachPr { url, session }) = &args.command {
        let storage = Storage::discover()?;
        difu::agents::client::ensure_running(&storage)?;
        let reply = difu::agents::client::request(
            &storage,
            difu::agents::Request::AttachPr {
                id: session.clone(),
                url: url.url(),
            },
        )?;
        anyhow::ensure!(
            matches!(reply, difu::agents::Reply::Ok),
            "Unexpected PR attachment reply: {reply:?}"
        );
        println!("Attached {} to session {session}", url.url());
        return Ok(());
    }
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "difu needs an interactive terminal. Run `difu` directly in your terminal."
    );
    let local_diff = matches!(args.command, Some(Commands::Diff { .. }));
    let requested_pr = match args.command {
        Some(Commands::Pr { .. }) => Some(github::current_branch_pr(&Cancel::default())?.url()),
        _ => args.pr,
    };
    let pr = match requested_pr {
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
    difu::theme::select(difu::theme::index(config.theme.as_deref()));
    let mut app = if local_diff {
        Shell::local(storage, config, std::env::current_dir()?)
    } else {
        Shell::new(storage, config, pr)
    };
    let signals = authentication::Signals::register()?;
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
        authentication::enable()?;
        app.reviews.images.detect();
        let mut focused = true;
        let mut redraw = difu::repaint::Schedule::new(Instant::now());
        while !app.reviews.quit && !signals.quitting() {
            // Read queued input before maintenance. Poll wakes immediately on a
            // key; active animation deadlines never inherit the idle 50 ms wait.
            let mut input = false;
            let started = Instant::now();
            while event::poll(if input {
                Duration::ZERO
            } else {
                redraw.wait(Instant::now(), focused, app.redraw_interval())
            })? {
                input = true;
                redraw.invalidate();
                match event::read()? {
                    Event::Key(key) => {
                        focused = true;
                        app.key(key);
                    }
                    Event::FocusGained => focused = true,
                    Event::FocusLost => {
                        focused = false;
                        app.reviews.hover = Default::default();
                        app.agents.cancel_voice();
                    }
                    Event::Mouse(mouse) => app.mouse(mouse),
                    Event::Paste(text) => app.paste(text),
                    Event::Resize(..) => app.reviews.invalidate(),
                }
                // Bound input batches so continuous typing cannot starve draws.
                if started.elapsed() >= Duration::from_millis(2) {
                    break;
                }
            }
            if let Some(provider) = app.take_sign_in() {
                app.agents.cancel_voice();
                let result = authentication::run(
                    &mut terminal,
                    &mut authentication::command(provider),
                    &signals,
                    || app.notification_tick(false),
                );
                // A permanent shutdown must not start post-auth status workers.
                if signals.quitting() {
                    break;
                }
                match result {
                    Ok((success, cancelled)) => app.finish_sign_in(success, cancelled),
                    Err(_) => app.finish_sign_in(false, false),
                }
                // Native auth may have moved focus to a browser. Resume presence only
                // after the terminal reports focus again (or a deliberate input event).
                focused = false;
                redraw.invalidate();
                if !crossterm::terminal::is_raw_mode_enabled()? {
                    anyhow::bail!("Cannot restore terminal after sign-in");
                }
            }
            app.notification_tick(focused);
            let sent = app.agents.take_send_repaint();
            if !input && !sent {
                app.tick_focused(focused);
            }
            app.clipboard(&mut io::stdout().lock());
            if redraw.due(
                Instant::now(),
                difu::repaint::revision(),
                focused,
                app.redraw_interval(),
            ) {
                let frame = terminal.draw(|frame| {
                    app.draw(frame);
                    difu::theme::present(frame.buffer_mut());
                })?;
                app.reviews
                    .hover
                    .render(&mut io::stdout().lock(), frame.buffer)?;
            }
            // Flush the cleared composer and optimistic message before any work
            // that receives provider results or schedules cache refreshes.
            if input || sent {
                app.tick_focused(focused);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_requires_a_full_pr_url_and_explicit_session() -> Result<()> {
        let url = "https://github.com/other/repo/pull/42";
        let args = Args::try_parse_from(["difu", "attach-pr", url, "--session", "one"])?;
        assert!(
            matches!(args.command, Some(Commands::AttachPr { url, session })
            if url.id() == "other/repo#42" && session == "one")
        );
        assert!(Args::try_parse_from(["difu", "attach-pr", url]).is_err());
        assert!(Args::try_parse_from(["difu", "attach-pr", url, "--session", ""]).is_err());
        for value in [
            "42",
            "other/repo#42",
            "https://github.com/other/repo/issues/42",
        ] {
            assert!(
                Args::try_parse_from(["difu", "attach-pr", value, "--session", "one"]).is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn accepts_current_branch_command_and_preserves_existing_launch_forms() -> Result<()> {
        let args = Args::try_parse_from(["difu", "pr", "."])?;
        assert!(matches!(args.command, Some(Commands::Pr { target }) if target == "."));
        assert!(args.pr.is_none());
        let args = Args::try_parse_from(["difu", "diff", "."])?;
        assert!(matches!(args.command, Some(Commands::Diff { target }) if target == "."));
        assert!(args.pr.is_none());
        assert!(Args::try_parse_from(["difu", "123", "diff", "."]).is_err());
        assert!(Args::try_parse_from(["difu", "diff"]).is_err());
        assert!(Args::try_parse_from(["difu", "diff", "elsewhere"]).is_err());
        for value in ["123", "https://github.com/owner/repo/pull/123"] {
            let args = Args::try_parse_from(["difu", value])?;
            assert_eq!(args.pr.as_deref(), Some(value));
            assert!(args.command.is_none());
        }
        assert!(Args::try_parse_from(["difu"])?.command.is_none());
        assert!(Args::try_parse_from(["difu", "--agent-service"])?.agent_service);
        assert!(Args::try_parse_from(["difu", "pr"]).is_err());
        assert!(Args::try_parse_from(["difu", "pr", "elsewhere"]).is_err());
        Ok(())
    }
}
