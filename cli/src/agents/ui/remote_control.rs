//! `/remote-control`: pair the difu iOS app by QR code and manage paired phones.
use super::*;
use crate::agents::remote::{Command, Status};
use ratatui::style::Color;

type Outcome = std::result::Result<Status, String>;

#[derive(Default)]
pub(super) struct State {
    status: Option<Status>,
    error: Option<String>,
    receiver: Option<mpsc::Receiver<Outcome>>,
    checked: Option<Instant>,
    selected: usize,
}

impl Ui {
    pub(super) fn open_remote(&mut self) {
        self.modal = Some(Modal::Remote);
        self.remote.error = None;
        self.remote.selected = 0;
        // Opening the command is the explicit request to listen and pair.
        self.remote_command(Command::Open, true);
    }

    fn remote_command(&mut self, command: Command, connect: bool) {
        let storage = self.storage.clone();
        let (tx, rx) = mpsc::channel();
        self.remote.receiver = Some(rx);
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<Status> {
                if connect {
                    client::ensure_running(&storage)?;
                }
                match client::request(&storage, Request::Remote { command })? {
                    Reply::Remote(status) => Ok(status),
                    _ => anyhow::bail!("Unexpected remote control reply"),
                }
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            crate::repaint::request();
        });
    }

    pub(super) fn tick_remote(&mut self) {
        if let Some(result) = self
            .remote
            .receiver
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.remote.receiver = None;
            self.remote.checked = Some(Instant::now());
            match result {
                Ok(status) => {
                    if let Some(previous) = &self.remote.status {
                        for device in &status.devices {
                            if !previous.devices.iter().any(|d| d.id == device.id) {
                                self.notice = Some((format!("Paired {}", device.name), false));
                            }
                        }
                    }
                    self.remote.selected = self
                        .remote
                        .selected
                        .min(status.devices.len().saturating_sub(1));
                    self.remote.status = Some(status);
                    self.remote.error = None;
                }
                Err(error) => self.remote.error = Some(error),
            }
        }
        if !matches!(self.modal, Some(Modal::Remote)) || self.remote.receiver.is_some() {
            return;
        }
        if self
            .remote
            .checked
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(1))
        {
            // A used or expired code is replaced while the modal stays open.
            let command = match &self.remote.status {
                Some(status) if status.enabled && status.listening && status.pairing.is_none() => {
                    Command::Open
                }
                _ => Command::Status,
            };
            self.remote_command(command, false);
        }
    }

    pub(super) fn remote_key(&mut self, key: KeyEvent) {
        use crate::shortcuts::ids::*;
        let shortcut = self.shortcut(key);
        let enabled = self.remote.status.as_ref().is_some_and(|s| s.enabled);
        let devices = self
            .remote
            .status
            .as_ref()
            .map(|s| s.devices.clone())
            .unwrap_or_default();
        match shortcut {
            _ if key.code == KeyCode::Esc => self.modal = None,
            Some(NAVIGATION_UP) => self.remote.selected = self.remote.selected.saturating_sub(1),
            Some(NAVIGATION_DOWN) => {
                self.remote.selected =
                    (self.remote.selected + 1).min(devices.len().saturating_sub(1))
            }
            Some(REMOTE_COPY) => {
                if let Some(link) = self.remote.status.as_ref().and_then(|s| s.pairing.clone()) {
                    // Universal Clipboard carries it to the iPhone's Paste Pairing Link.
                    self.clipboard = Some(link);
                    self.notice = Some(("Pairing link copied".into(), false));
                }
            }
            Some(REMOTE_DISABLE) if enabled => self.remote_command(Command::Disable, false),
            Some(REMOTE_ENABLE) => self.remote_command(Command::Open, false),
            Some(REMOTE_REMOVE) => {
                if let Some(device) = devices.get(self.remote.selected) {
                    self.notice = Some((format!("Forgot {}", device.name), false));
                    self.remote_command(
                        Command::Forget {
                            device: device.id.clone(),
                        },
                        false,
                    );
                }
            }
            _ => {}
        }
    }

    pub(super) fn draw_remote(&mut self, frame: &mut Frame, area: Rect) {
        let footer = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
        let Some(status) = self.remote.status.clone() else {
            let text = self
                .remote
                .error
                .clone()
                .unwrap_or_else(|| "Starting remote control…".into());
            let style = Style::default().fg(if self.remote.error.is_some() {
                RED
            } else {
                DIM
            });
            frame.render_widget(
                Paragraph::new(text).style(style).wrap(Wrap { trim: false }),
                body,
            );
            return;
        };
        let qr = status.pairing.as_deref().and_then(|uri| qr_lines(uri).ok());
        let qr_width = qr
            .as_ref()
            .and_then(|lines| lines.first())
            .map_or(0, |line| line.width() as u16);
        let qr_height = qr.as_ref().map_or(0, |lines| lines.len() as u16);
        let fits = qr_width > 0 && qr_width + 30 <= body.width && qr_height <= body.height;
        let text_area = if let (Some(lines), true) = (&qr, fits) {
            frame.render_widget(
                Paragraph::new(lines.clone()),
                Rect::new(body.x, body.y, qr_width, qr_height),
            );
            Rect::new(
                body.x + qr_width + 3,
                body.y,
                body.width.saturating_sub(qr_width + 3),
                body.height,
            )
        } else {
            body
        };
        let mut lines = self.remote_lines(&status, qr.is_some() && !fits);
        if let Some(error) = &self.remote.error {
            lines.push(Line::default());
            lines.push(Line::styled(error.clone(), Style::default().fg(RED)));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text_area);
        use crate::shortcuts::{Bindings, ids::*};
        let bindings = Bindings::new(&self.shortcut_config);
        let hint = |id| bindings.primary_hint_in(id, self.shortcut_context());
        let hint = if status.enabled {
            format!(
                "{} copy link · {}/{} select · {} forget · {} new code · {} off · Esc close",
                hint(REMOTE_COPY),
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN),
                hint(REMOTE_REMOVE),
                hint(REMOTE_ENABLE),
                hint(REMOTE_DISABLE)
            )
        } else {
            format!("{} turn on · Esc close", hint(REMOTE_ENABLE))
        };
        frame.render_widget(Paragraph::new(hint).style(Style::default().fg(DIM)), footer);
    }

    fn remote_lines(&self, status: &Status, cramped: bool) -> Vec<Line<'static>> {
        let bold = Style::default()
            .fg(TEXT)
            .add_modifier(ratatui::style::Modifier::BOLD);
        let dim = Style::default().fg(DIM);
        let mut lines = vec![
            Line::styled("Control difu from your iPhone", bold),
            Line::default(),
        ];
        if !status.enabled {
            lines.push(Line::styled(
                "Remote control is off. Press e to listen on this Wi-Fi network and show a pairing code.",
                Style::default().fg(TEXT),
            ));
        } else if !status.listening {
            lines.push(Line::styled(
                status
                    .error
                    .clone()
                    .unwrap_or_else(|| "Remote control is not listening.".into()),
                Style::default().fg(RED),
            ));
        } else {
            let address = status
                .hosts
                .iter()
                .find(|h| !h.ends_with(".local"))
                .or(status.hosts.first())
                .cloned()
                .unwrap_or_else(|| "no network".into());
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(GREEN)),
                Span::styled(
                    format!("Listening on {address}:{}", status.port),
                    Style::default().fg(TEXT),
                ),
            ]));
            if let Some(name) = status.hosts.iter().find(|h| h.ends_with(".local")) {
                lines.push(Line::styled(format!("  {name}"), dim));
            }
            lines.push(Line::default());
            if status.pairing.is_some() {
                lines.push(Line::styled(
                    "Open the difu app on your iPhone and scan this code.",
                    Style::default().fg(TEXT),
                ));
                let remaining = status
                    .pairing_expires
                    .map(|at| (at - chrono::Utc::now().timestamp_millis()).max(0) / 1000)
                    .unwrap_or(0);
                lines.push(Line::styled(
                    format!(
                        "The code works once and expires in {}:{:02}.",
                        remaining / 60,
                        remaining % 60
                    ),
                    dim,
                ));
                if cramped {
                    lines.push(Line::styled(
                        "Enlarge the terminal to show the QR code.",
                        Style::default().fg(ACCENT),
                    ));
                }
            } else {
                lines.push(Line::styled("Preparing a pairing code…", dim));
            }
            lines.push(Line::styled(
                "Your phone must be on the same Wi-Fi. macOS may ask to allow incoming connections for difu.",
                dim,
            ));
        }
        lines.push(Line::default());
        lines.push(Line::styled("Paired phones", bold));
        if status.devices.is_empty() {
            lines.push(Line::styled("None yet", dim));
        }
        let now = chrono::Utc::now().timestamp_millis();
        for (index, device) in status.devices.iter().enumerate() {
            let state = if device.connected {
                "connected".to_owned()
            } else {
                device
                    .seen
                    .map(|seen| format!("seen {}", ago(now - seen)))
                    .unwrap_or_else(|| "never connected".into())
            };
            let selected = index == self.remote.selected;
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    device.name.clone(),
                    Style::default().fg(if selected { ACCENT } else { TEXT }),
                ),
                Span::styled(
                    format!(" · {state}"),
                    Style::default().fg(if device.connected { GREEN } else { DIM }),
                ),
            ]));
        }
        lines
    }
}

fn ago(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

/// Two QR modules per terminal cell with half blocks, in explicit black and
/// white so every theme and terminal background scans.
fn qr_lines(uri: &str) -> anyhow::Result<Vec<Line<'static>>> {
    let code = qrcode::QrCode::with_error_correction_level(uri, qrcode::EcLevel::L)?;
    let width = code.width();
    let colors = code.to_colors();
    const QUIET: usize = 2;
    let size = width + QUIET * 2;
    let dark = |x: usize, y: usize| {
        x >= QUIET
            && y >= QUIET
            && x < width + QUIET
            && y < width + QUIET
            && colors.get((y - QUIET) * width + (x - QUIET)) == Some(&qrcode::Color::Dark)
    };
    let black = Color::Rgb(0, 0, 0);
    let white = Color::Rgb(255, 255, 255);
    Ok((0..size.div_ceil(2))
        .map(|row| {
            let text: String = (0..size)
                .map(|x| match (dark(x, row * 2), dark(x, row * 2 + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect();
            Line::from(Span::styled(text, Style::default().fg(black).bg(white)))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_code_is_square_with_a_quiet_zone() -> anyhow::Result<()> {
        let lines = qr_lines(
            "difu://pair?v=1&n=Mac&h=mac.local,192.168.1.2&p=50000&f=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&c=BBBBBBBBBBBBBBBBBBBBBB",
        )?;
        let width = lines.first().map_or(0, |l| l.width());
        assert!(width >= 25);
        assert_eq!(lines.len(), width.div_ceil(2));
        let first: String = lines
            .first()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .unwrap_or_default();
        assert!(first.chars().all(|c| c == ' ' || c == '▄'));
        Ok(())
    }

    #[test]
    fn modal_draws_status_and_devices() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = Ui::new(storage, &Config::default());
        assert!(
            ui.palette_commands()
                .iter()
                .any(|(name, _, _)| name == "/remote-control")
        );
        ui.modal = Some(Modal::Remote);
        ui.remote.status = Some(Status {
            enabled: true,
            listening: true,
            port: 50123,
            name: "Mac".into(),
            hosts: vec!["mac.local".into(), "192.168.1.2".into()],
            pairing: Some("difu://pair?v=1&c=code".into()),
            pairing_expires: Some(chrono::Utc::now().timestamp_millis() + 90_000),
            devices: vec![crate::agents::remote::DeviceStatus {
                id: "phone-0001".into(),
                name: "Ali's iPhone".into(),
                paired: 0,
                seen: None,
                connected: true,
            }],
            error: None,
        });
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
        terminal.draw(|frame| ui.draw_modal(frame))?;
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Listening on 192.168.1.2:50123"));
        assert!(screen.contains("Ali's iPhone · connected"));
        assert!(screen.contains('█'));
        ui.remote_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(ui.clipboard.as_deref(), Some("difu://pair?v=1&c=code"));
        Ok(())
    }
}

#[cfg(test)]
mod shortcut_effect_tests {
    use super::super::shortcut_tests::{BINDING_PHASES, binding_fixture, request_at_boundary_with};
    use super::*;
    use crate::shortcuts::ids::*;
    use anyhow::Result;

    fn ready(ui: &mut Ui) {
        ui.modal = Some(Modal::Remote);
        ui.remote.status = Some(Status {
            enabled: true,
            pairing: Some("difu://pair?v=1&c=fake".into()),
            devices: vec![
                crate::agents::remote::DeviceStatus {
                    id: "first".into(),
                    name: "First".into(),
                    paired: 0,
                    seen: None,
                    connected: false,
                },
                crate::agents::remote::DeviceStatus {
                    id: "second".into(),
                    name: "Second".into(),
                    paired: 0,
                    seen: None,
                    connected: false,
                },
            ],
            ..Default::default()
        });
    }
    #[test]
    fn remote_native_hints_show_each_effective_action_in_compact_footer() -> Result<()> {
        for width in [80, 160] {
            for action in [REMOTE_COPY, REMOTE_DISABLE, REMOTE_ENABLE, REMOTE_REMOVE] {
                let (mut ui, _dir, _) = super::super::shortcut_tests::fixture(Some(action))?;
                ready(&mut ui);
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 44))?;
                terminal.draw(|frame| ui.draw(frame))?;
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(text.contains("F12"), "{width} {action:?}: {text}");
            }
        }
        Ok(())
    }

    #[test]
    fn remote_actions_remap_all_legacy_aliases_without_starting_a_listener() -> Result<()> {
        for (phase, custom) in BINDING_PHASES {
            for (action, old) in [
                (REMOTE_COPY, KeyCode::Char('c')),
                (REMOTE_DISABLE, KeyCode::Char('d')),
                (REMOTE_ENABLE, KeyCode::Char('e')),
                (REMOTE_ENABLE, KeyCode::Char('n')),
                (REMOTE_REMOVE, KeyCode::Char('x')),
                (REMOTE_REMOVE, KeyCode::Delete),
                (REMOTE_REMOVE, KeyCode::Backspace),
            ] {
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                ready(&mut ui);
                if custom {
                    ui.key(KeyEvent::new(old, KeyModifiers::NONE));
                    assert!(ui.remote.receiver.is_none());
                    assert!(ui.clipboard.is_none());
                }
                let key = KeyEvent::new(
                    if custom { KeyCode::F(12) } else { old },
                    KeyModifiers::NONE,
                );
                if action == REMOTE_COPY {
                    ui.key(key);
                    assert_eq!(
                        ui.clipboard.take().as_deref(),
                        Some("difu://pair?v=1&c=fake")
                    );
                } else {
                    let request = request_at_boundary_with(&mut ui, |ui| ui.key(key))?;
                    match request {
                        Request::Remote {
                            command: Command::Disable,
                        } if action == REMOTE_DISABLE => {}
                        Request::Remote {
                            command: Command::Open,
                        } if action == REMOTE_ENABLE => {}
                        Request::Remote {
                            command: Command::Forget { device },
                        } if action == REMOTE_REMOVE => assert_eq!(device, "first"),
                        _ => anyhow::bail!("wrong remote intent"),
                    }
                    // Fixture returns Ok rather than Remote status: real error handling keeps the modal.
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while ui.remote.receiver.is_some() && Instant::now() < deadline {
                        ui.tick_remote();
                        thread::sleep(Duration::from_millis(2));
                    }
                    assert!(ui.remote.receiver.is_none());
                    assert!(
                        ui.remote
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains("Unexpected remote control reply"))
                    );
                    assert!(matches!(ui.modal, Some(Modal::Remote)));
                }
            }
        }
        Ok(())
    }
    #[test]
    fn remote_navigation_and_action_guards_are_contextual() -> Result<()> {
        for (phase, custom) in BINDING_PHASES {
            for (action, old, initial, want) in [
                (NAVIGATION_UP, KeyCode::Up, 1, 0),
                (NAVIGATION_DOWN, KeyCode::Down, 0, 1),
            ] {
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                ready(&mut ui);
                ui.remote.selected = initial;
                if custom {
                    ui.key(KeyEvent::new(old, KeyModifiers::NONE));
                    assert_eq!(ui.remote.selected, initial);
                }
                ui.key(KeyEvent::new(
                    if custom { KeyCode::F(12) } else { old },
                    KeyModifiers::NONE,
                ));
                assert_eq!(ui.remote.selected, want);
            }
            for action in [REMOTE_COPY, REMOTE_DISABLE, REMOTE_REMOVE] {
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                ui.modal = Some(Modal::Remote);
                let code = if custom {
                    KeyCode::F(12)
                } else {
                    KeyCode::Char(if action == REMOTE_COPY {
                        'c'
                    } else if action == REMOTE_DISABLE {
                        'd'
                    } else {
                        'x'
                    })
                };
                ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                assert!(ui.remote.receiver.is_none());
                assert!(ui.clipboard.is_none());
            }
        }
        Ok(())
    }
}
