//! `difu serve`: the agent service in the foreground with server mode and remote
//! control on, plus `difu remote` administration of the running service.
use super::{cli::RemoteCommand, password};
use crate::{
    agents::{
        Reply, Request, client,
        remote::{self, Command, Status},
    },
    storage::Storage,
};
use anyhow::{Context, Result, ensure};
use std::io::{self, BufRead, IsTerminal, Read, Write};

const NOT_RUNNING: &str =
    "The difu service is not running. Start it with `difu serve` or `systemctl start difu-agent`.";

/// systemd already made this process a session leader, so unlike the
/// background service started by the terminal app it never calls `setsid`.
pub fn run(storage: Storage, port: Option<u16>) -> Result<()> {
    if let Some(parent) = storage.config.parent() {
        std::fs::create_dir_all(parent)?;
    }
    storage.try_patch_config(|config| {
        config.server.enabled = true;
        Ok(())
    })?;
    let port = remote::prepare_server(&storage, port)?;
    eprintln!(
        "difu {} serving agents; remote control on port {port}",
        env!("CARGO_PKG_VERSION")
    );
    // Before any session starts, so no Claude Code is writing the file yet.
    if let Some(home) = dirs::home_dir() {
        match super::claude_config::share(&home) {
            Ok(Some(note)) => eprintln!("{note}"),
            Ok(None) => {}
            Err(error) => eprintln!("Claude Code config: {error:#}"),
        }
    }
    crate::agents::server::run(storage)
}

pub fn remote(storage: &Storage, command: RemoteCommand) -> Result<()> {
    match command {
        RemoteCommand::Pair { json } => pair(storage, json),
        RemoteCommand::Devices => {
            devices(&send(storage, Command::Status)?);
            Ok(())
        }
        RemoteCommand::Forget { device } => forget(storage, &device),
        RemoteCommand::Password { generate, disable } => set_password(storage, generate, disable),
        RemoteCommand::Config {
            hosts,
            clear_hosts,
            port,
            name,
        } => {
            let command = if hosts.is_empty() && !clear_hosts && port.is_none() && name.is_none() {
                Command::Status
            } else {
                Command::Configure {
                    hosts,
                    clear_hosts,
                    port,
                    name,
                }
            };
            show_config(&send(storage, command)?);
            Ok(())
        }
    }
}

fn send(storage: &Storage, command: Command) -> Result<Status> {
    let reply = client::request(storage, Request::Remote { command }).map_err(|error| {
        let absent = error.chain().any(|cause| {
            cause.downcast_ref::<io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                )
            })
        });
        if absent {
            anyhow::anyhow!(NOT_RUNNING)
        } else {
            error
        }
    })?;
    match reply {
        Reply::Remote(status) => Ok(status),
        other => anyhow::bail!("Unexpected remote control reply: {other:?}"),
    }
}

fn pair(storage: &Storage, json: bool) -> Result<()> {
    let status = send(storage, Command::Open)?;
    let (Some(link), Some(expires)) = (status.pairing, status.pairing_expires) else {
        anyhow::bail!(
            "{}",
            status
                .error
                .unwrap_or_else(|| "Remote control is not listening".into())
        );
    };
    if json {
        println!("{}", serde_json::json!({"link": link, "expires": expires}));
        return Ok(());
    }
    print!("{}", qr_text(&link, io::stdout().is_terminal())?);
    println!();
    println!("Scan this code in the difu app, or paste the link:");
    println!("{link}");
    println!(
        "It works once and expires at {}.",
        local_time(expires, "%H:%M")
    );
    Ok(())
}

fn devices(status: &Status) {
    if status.devices.is_empty() {
        println!("No paired devices. Pair one with `difu remote pair`.");
        return;
    }
    let width = status
        .devices
        .iter()
        .map(|d| d.name.chars().count())
        .max()
        .unwrap_or(0);
    for device in &status.devices {
        let seen = if device.connected {
            "connected now".to_owned()
        } else {
            device.seen.map_or_else(
                || "never connected".to_owned(),
                |seen| format!("last seen {}", local_time(seen, "%Y-%m-%d %H:%M")),
            )
        };
        let access = serde_json::to_value(device.access)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        println!(
            "{:width$}  {access:8}  {seen:28}  {}",
            device.name, device.id
        );
    }
}

fn forget(storage: &Storage, device: &str) -> Result<()> {
    let status = send(storage, Command::Status)?;
    let by_id: Vec<_> = status.devices.iter().filter(|d| d.id == device).collect();
    let by_name: Vec<_> = status
        .devices
        .iter()
        .filter(|d| d.name.eq_ignore_ascii_case(device))
        .collect();
    let target = match (by_id.as_slice(), by_name.as_slice()) {
        ([target], _) | ([], [target]) => *target,
        ([], []) => anyhow::bail!(
            "No paired device is named or has the id `{device}`. See `difu remote devices`."
        ),
        _ => anyhow::bail!("Several devices are named `{device}`; forget one by its id."),
    };
    send(
        storage,
        Command::Forget {
            device: target.id.clone(),
        },
    )?;
    println!("Forgot {}.", target.name);
    Ok(())
}

fn set_password(storage: &Storage, generate: bool, disable: bool) -> Result<()> {
    // Fail before asking for a password the service cannot take.
    send(storage, Command::Status)?;
    if disable {
        send(storage, Command::Password { verifier: None })?;
        println!("Password pairing is off. QR pairing keeps working.");
        return Ok(());
    }
    let secret = if generate {
        password::generate()?
    } else {
        read_password()?
    };
    let verifier = password::Verifier::create(&secret)?;
    send(
        storage,
        Command::Password {
            verifier: Some(verifier),
        },
    )?;
    if generate {
        // Only the password goes to stdout, so scripts can capture it.
        println!("{secret}");
        eprintln!("Server password set. New clients pair with this server's address and it.");
    } else {
        eprintln!("Server password set.");
    }
    Ok(())
}

fn read_password() -> Result<String> {
    if !io::stdin().is_terminal() {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    let first = prompt_secret("New server password: ")?;
    ensure!(
        first.chars().count() >= password::MINIMUM_LENGTH,
        "The server password needs at least {} characters",
        password::MINIMUM_LENGTH
    );
    let second = prompt_secret("Repeat it: ")?;
    ensure!(first == second, "The passwords do not match");
    Ok(first)
}

/// Read a line from the terminal without echoing it.
fn prompt_secret(prompt: &str) -> Result<String> {
    struct Raw;
    impl Drop for Raw {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode().context("Cannot read the password")?;
    let raw = Raw;
    let mut bytes = Vec::new();
    let mut stdin = io::stdin().lock();
    let cancelled = loop {
        let mut byte = [0u8; 1];
        if stdin.read(&mut byte)? == 0 {
            break false;
        }
        match byte {
            [b'\r' | b'\n'] => break false,
            // Ctrl-C, or Ctrl-D on an empty line.
            [3] => break true,
            [4] if bytes.is_empty() => break true,
            [127 | 8] => {
                // Remove one whole UTF-8 character.
                while let Some(byte) = bytes.pop() {
                    if byte & 0xC0 != 0x80 {
                        break;
                    }
                }
            }
            [byte] => bytes.push(byte),
        }
    };
    drop(raw);
    eprintln!();
    ensure!(!cancelled, "Cancelled");
    String::from_utf8(bytes).context("The password is not valid UTF-8")
}

fn show_config(status: &Status) {
    println!("Name: {}", status.name);
    println!(
        "Port: {}{}",
        status.port,
        if status.listening {
            ""
        } else {
            " (not listening)"
        }
    );
    println!(
        "Advertised hosts: {}",
        if status.configured_hosts.is_empty() {
            "none (discovered addresses only)".to_owned()
        } else {
            status.configured_hosts.join(", ")
        }
    );
    if status.listening {
        println!("Pairing links use: {}", status.hosts.join(", "));
    }
    println!(
        "Password pairing: {}",
        if status.password { "on" } else { "off" }
    );
    if let Some(error) = &status.error {
        eprintln!("{error}");
    }
}

fn local_time(milliseconds: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp_millis(milliseconds)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format(format)
                .to_string()
        })
        .unwrap_or_default()
}

/// Two QR modules per line with half blocks. Like `qrencode -t UTF8`, the light
/// modules are the drawn ones, so the code scans on the usual dark terminal;
/// `color` pins white on black so light themes scan too.
fn qr_text(uri: &str, color: bool) -> Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(uri, qrcode::EcLevel::L)?;
    let width = code.width();
    let colors = code.to_colors();
    const QUIET: usize = 2;
    let size = width + QUIET * 2;
    let light = |x: usize, y: usize| {
        let dark = x >= QUIET
            && y >= QUIET
            && x < width + QUIET
            && y < width + QUIET
            && colors.get((y - QUIET) * width + (x - QUIET)) == Some(&qrcode::Color::Dark);
        y < size && !dark
    };
    let mut text = String::new();
    for row in 0..size.div_ceil(2) {
        if color {
            text.push_str("\x1b[97;40m");
        }
        for x in 0..size {
            text.push(match (light(x, row * 2), light(x, row * 2 + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        if color {
            text.push_str("\x1b[0m");
        }
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_qr_code_is_square_and_framed_by_its_quiet_zone() -> Result<()> {
        let text = qr_text("difu://pair?v=1&n=box&h=203.0.113.4&p=3438&f=x&c=y", false)?;
        let lines: Vec<&str> = text.lines().collect();
        let widths: Vec<usize> = lines.iter().map(|l| l.chars().count()).collect();
        let Some(&width) = widths.first() else {
            anyhow::bail!("empty QR code");
        };
        assert!(widths.iter().all(|w| *w == width));
        assert_eq!(lines.len(), width.div_ceil(2));
        assert!(lines.first().is_some_and(|l| l.chars().all(|c| c == '█')));
        let colored = qr_text("difu://pair?v=1", true)?;
        assert!(colored.starts_with("\x1b[97;40m") && colored.ends_with("\x1b[0m\n"));
        Ok(())
    }

    #[test]
    fn commands_explain_how_to_start_a_missing_service() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let Err(error) = send(&storage, Command::Status) else {
            anyhow::bail!("No service should be running");
        };
        assert!(format!("{error:#}").contains("difu serve"));
        Ok(())
    }

    #[test]
    fn serve_settings_prefer_the_flag_then_the_saved_port() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        assert_eq!(
            remote::prepare_server(&storage, None)?,
            super::super::DEFAULT_PORT
        );
        assert_eq!(remote::prepare_server(&storage, Some(4000))?, 4000);
        assert_eq!(remote::prepare_server(&storage, None)?, 4000);
        Ok(())
    }
}
