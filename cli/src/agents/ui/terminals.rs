//! Shared PTYs use the same helper pane as artifacts; only the picker terminates them.
use super::super::shells;
use super::*;
use anyhow::Result;
use base64::Engine;
use ratatui::style::{Color, Modifier};
use std::{
    num::NonZeroU16,
    sync::atomic::{AtomicU64, Ordering},
};
static CLIENT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(super) fn poll_interval(focused: bool) -> Duration {
    if focused {
        crate::repaint::FRAME_INTERVAL
    } else {
        Duration::from_millis(50)
    }
}
enum Human {
    Focus(bool),
    Input(String),
}

#[derive(Clone)]
pub(super) enum Operation {
    Open,
    Run(String),
    Poll(String),
    Input,
    Close(String),
}
pub(super) struct Terminal {
    pub id: String,
    parser: vt100::Parser,
    input: Option<mpsc::Sender<Human>>,
    focused: bool,
    renewed: Option<Instant>,
    checked: Option<Instant>,
    loading: bool,
    closed: bool,
    error: Option<String>,
}
impl Terminal {
    pub fn new(id: String) -> Self {
        Self {
            id,
            parser: vt100::Parser::new(NonZeroU16::MIN, NonZeroU16::MIN, 0),
            input: None,
            focused: false,
            renewed: None,
            checked: None,
            loading: false,
            closed: false,
            error: None,
        }
    }
    fn connect(&mut self, id: &str, storage: &Storage, results: &mpsc::Sender<ResultMessage>) {
        if self.input.is_some() {
            return;
        }
        let (send, receive) = mpsc::channel::<Human>();
        let shell = self.id.clone();
        let id = id.to_owned();
        let storage = storage.clone();
        let results = results.clone();
        let client = format!(
            "{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis(),
            CLIENT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        thread::spawn(move || {
            for (sequence, message) in receive.into_iter().enumerate() {
                let (focused, text, report) = match message {
                    Human::Focus(focused) => (focused, String::new(), false),
                    Human::Input(text) => (true, text, true),
                };
                let result = client::request(
                    &storage,
                    Request::Shell {
                        id: id.clone(),
                        action: shells::Action::Human {
                            shell: shell.clone(),
                            client: client.clone(),
                            sequence: sequence as u64 + 1,
                            focused,
                            text,
                        },
                    },
                )
                .map_err(|error| format!("{error:#}"));
                if report || result.is_err() {
                    let _ = results.send(ResultMessage {
                        kind: Task::Terminal(id.clone(), Operation::Input),
                        result,
                    });
                }
            }
        });
        self.input = Some(send);
    }
    fn focus(&mut self, focused: bool) {
        if self.focused != focused
            || (focused
                && self
                    .renewed
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(250)))
        {
            if let Some(input) = &self.input {
                let _ = input.send(Human::Focus(focused));
            }
            self.focused = focused;
            self.renewed = Some(Instant::now());
        }
    }
    fn receive(&mut self, value: &Value) -> Result<()> {
        let rows = dimension(
            value
                .get("rows")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .min(200) as u16,
        );
        let cols = dimension(
            value
                .get("cols")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .min(500) as u16,
        );
        let bytes = base64::engine::general_purpose::STANDARD.decode(
            value
                .get("screen")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?;
        self.parser = vt100::Parser::new(rows, cols, 0);
        self.parser.process(&bytes);
        self.closed = value.get("closed").and_then(Value::as_bool) == Some(true);
        self.error = value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(())
    }
    pub fn draw(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        for row in 0..area.height.min(rows.get()) {
            for col in 0..area.width.min(cols.get()) {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let Some(target) = frame.buffer_mut().cell_mut((area.x + col, area.y + row)) else {
                    continue;
                };
                let mut style = Style::default()
                    .fg(color(cell.fgcolor(), TEXT))
                    .bg(color(cell.bgcolor(), BG));
                for (active, modifier) in [
                    (cell.bold(), Modifier::BOLD),
                    (cell.dim(), Modifier::DIM),
                    (cell.italic(), Modifier::ITALIC),
                    (cell.underline(), Modifier::UNDERLINED),
                    (cell.inverse(), Modifier::REVERSED),
                ] {
                    if active {
                        style = style.add_modifier(modifier);
                    }
                }
                target.set_style(style);
                target.set_symbol(if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                });
            }
        }
        let (row, col) = screen.cursor_position();
        if focused && !self.closed && !screen.hide_cursor() && row < area.height && col < area.width
        {
            frame.set_cursor_position((area.x + col, area.y + row));
        }
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str()).style(Style::default().fg(RED)),
                Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1)
                    .intersection(area),
            );
        } else if self.closed {
            frame.render_widget(
                Paragraph::new("Shell exited · x in Shells removes it")
                    .style(Style::default().fg(DIM)),
                Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1)
                    .intersection(area),
            );
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(input) = &self.input {
            let _ = input.send(Human::Focus(false));
        }
    }
}

fn dimension(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}
fn color(value: vt100::Color, default: Color) -> Color {
    match value {
        vt100::Color::Default => default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
fn key_text(key: KeyEvent, application_cursor: bool) -> Option<String> {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return None;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let text = match key.code {
        KeyCode::Char(c) if control => match c.to_ascii_lowercase() {
            'a'..='z' => char::from(c.to_ascii_lowercase() as u8 - b'a' + 1).to_string(),
            ' ' | '@' | '2' => "\0".into(),
            '[' => "\x1b".into(),
            '\\' => "\x1c".into(),
            ']' => "\x1d".into(),
            '^' | '6' => "\x1e".into(),
            '_' | '-' => "\x1f".into(),
            _ => return None,
        },
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "\r".into(),
        KeyCode::Backspace => "\x7f".into(),
        KeyCode::Tab => "\t".into(),
        KeyCode::BackTab => "\x1b[Z".into(),
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Right
        | KeyCode::Left
        | KeyCode::Home
        | KeyCode::End => {
            let code = match key.code {
                KeyCode::Up => 'A',
                KeyCode::Down => 'B',
                KeyCode::Right => 'C',
                KeyCode::Left => 'D',
                KeyCode::Home => 'H',
                _ => 'F',
            };
            return Some(if modifier > 1 {
                format!("\x1b[1;{modifier}{code}")
            } else {
                format!("\x1b{}{code}", if application_cursor { 'O' } else { '[' })
            });
        }
        KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown => {
            let code = match key.code {
                KeyCode::Insert => 2,
                KeyCode::Delete => 3,
                KeyCode::PageUp => 5,
                _ => 6,
            };
            return Some(if modifier > 1 {
                format!("\x1b[{code};{modifier}~")
            } else {
                format!("\x1b[{code}~")
            });
        }
        KeyCode::F(n) => {
            return match n {
                1..=4 => Some(format!("\x1bO{}", char::from(b'P' + n - 1))),
                5..=12 => [15, 17, 18, 19, 20, 21, 23, 24]
                    .get(usize::from(n - 5))
                    .map(|code| format!("\x1b[{code}~")),
                _ => None,
            };
        }
        _ => return None,
    };
    Some(if alt { format!("\x1b{text}") } else { text })
}
impl Ui {
    pub(crate) fn release_terminal_focus(&mut self) {
        if let Some(panels::View::Terminal(terminal)) = &mut self.panels.view {
            terminal.focus(false);
            self.panels.focused = false;
        }
    }
    pub(super) fn command_mode(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.positions.get(id))
            .is_some_and(|position| position.draft.text().starts_with('!'))
    }
    pub(super) fn terminal_input(&self) -> bool {
        self.modal.is_none()
            && self.panels.focused
            && self.panels.visible()
            && matches!(self.panels.view, Some(panels::View::Terminal(_)))
    }
    pub(super) fn open_terminal(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.modal = None;
        self.task(
            Task::Terminal(id.clone(), Operation::Open),
            Request::Shell {
                id,
                action: shells::Action::Open,
            },
            false,
        );
    }
    pub(super) fn run_shell_command(&mut self) -> bool {
        if !self.command_mode() {
            return false;
        }
        let Some(id) = self.selected.clone() else {
            return true;
        };
        let position = self.positions.entry(id.clone()).or_default();
        let text = position.draft.text();
        let command = text.strip_prefix('!').unwrap_or_default().to_owned();
        if command.trim().is_empty() {
            return true;
        }
        position.draft = Editor::default();
        position.history = None;
        position.follow = true;
        position.keep_transcript_position = false;
        position.transcript_viewport = None;
        self.task(
            Task::Terminal(id.clone(), Operation::Run(text)),
            Request::Shell {
                id,
                action: shells::Action::Run { command },
            },
            false,
        );
        true
    }
    pub(super) fn terminal_key(&mut self, key: KeyEvent) {
        let application = match &self.panels.view {
            Some(panels::View::Terminal(terminal)) => terminal.parser.screen().application_cursor(),
            _ => return,
        };
        if let Some(text) = key_text(key, application) {
            self.terminal_text(&text);
        }
    }
    pub(super) fn terminal_text(&mut self, text: &str) {
        if text.len() > 64 * 1024 {
            self.notice = Some((
                "Shell input exceeds 64 KiB; paste a smaller block".into(),
                true,
            ));
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(panels::View::Terminal(terminal)) = &mut self.panels.view else {
            return;
        };
        terminal.connect(&id, &self.storage, &self.sender);
        if let Some(send) = &terminal.input
            && send.send(Human::Input(text.to_owned())).is_err()
        {
            self.notice = Some(("Shell input disconnected".into(), true));
        }
        terminal.focused = true;
        terminal.renewed = Some(Instant::now());
        terminal.checked = None;
        self.panels.scroll = 0;
    }
    pub(super) fn terminal_paste(&mut self, text: &str) {
        let bracketed = matches!(&self.panels.view, Some(panels::View::Terminal(terminal)) if terminal.parser.screen().bracketed_paste());
        self.terminal_text(&if bracketed {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text.replace('\n', "\r")
        });
    }
    pub(super) fn tick_terminal(&mut self, visible: bool) {
        let shown = visible && self.panels.visible();
        let focused = shown && self.panels.focused && self.modal.is_none();
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(panels::View::Terminal(terminal)) = &mut self.panels.view else {
            return;
        };
        if focused {
            terminal.connect(&id, &self.storage, &self.sender);
        }
        terminal.focus(focused);
        if !shown {
            return;
        }
        if terminal.loading
            || terminal
                .checked
                .is_some_and(|time| time.elapsed() < poll_interval(focused))
        {
            return;
        }
        terminal.loading = true;
        // Measure from dispatch, not receipt: slow replies must not add a
        // second cooldown, nor erase an input-triggered refresh request.
        terminal.checked = Some(Instant::now());
        let shell = terminal.id.clone();
        let action = shells::Action::Exchange {
            shell: shell.clone(),
            rows: self.panels.rect.height.max(1),
            cols: self.panels.rect.width.max(1),
            scrollback: self.panels.scroll,
        };
        self.task(
            Task::Terminal(id.clone(), Operation::Poll(shell)),
            Request::Shell { id, action },
            false,
        );
    }
    pub(super) fn close_shell(&mut self, index: usize) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(shell) = self
            .panels
            .shells
            .get(&id)
            .and_then(|shells| shells.get(index))
        else {
            return;
        };
        let shell_id = shell
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let request = if shell.get("local").and_then(Value::as_bool) == Some(true) {
            Request::Shell {
                id: id.clone(),
                action: shells::Action::Close {
                    shell: shell_id.clone(),
                },
            }
        } else {
            Request::Control {
                id: id.clone(),
                control: Control::CloseShell {
                    process: shell
                        .get("processId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                },
            }
        };
        self.task(
            Task::Terminal(id, Operation::Close(shell_id)),
            request,
            false,
        );
    }
    pub(super) fn finish_terminal(
        &mut self,
        id: &str,
        operation: Operation,
        result: Result<Reply, String>,
    ) {
        if matches!(operation, Operation::Input) {
            if let Err(error) = result {
                self.notice = Some((format!("Shell input: {error}"), true));
            }
            return;
        }
        if let Operation::Poll(shell) = &operation {
            if self.selected.as_deref() == Some(id)
                && let Some(panels::View::Terminal(terminal)) = &mut self.panels.view
                && terminal.id == *shell
            {
                terminal.loading = false;
                match result {
                    Ok(Reply::Shell(value)) => {
                        if let Err(error) = terminal.receive(&value) {
                            terminal.error = Some(error.to_string());
                        }
                    }
                    Err(error) => terminal.error = Some(error),
                    _ => terminal.error = Some("Unexpected shell snapshot".into()),
                }
            }
            return;
        }
        match result {
            Ok(Reply::Shell(value)) => {
                if let Some(shell) = value.get("shell").and_then(Value::as_str) {
                    if matches!(operation, Operation::Open) && self.selected.as_deref() == Some(id)
                    {
                        self.panels.view = Some(panels::View::Terminal(Box::new(Terminal::new(
                            shell.into(),
                        ))));
                        self.panels.session = self.selected.clone();
                        self.panels.hidden = false;
                        self.panels.focused = true;
                        self.panels.scroll = 0;
                        self.drilled = true;
                    } else if matches!(operation, Operation::Run(_)) {
                        self.positions
                            .entry(id.to_owned())
                            .or_default()
                            .expanded
                            .insert(shell.into());
                    }
                }
            }
            Ok(Reply::Ok) => {}
            other => {
                let error = match other {
                    Err(error) => error,
                    _ => "Unexpected shell response".into(),
                };
                self.notice = Some((format!("Shell: {error}"), true));
                if let Operation::Run(text) = &operation {
                    let position = self.positions.entry(id.to_owned()).or_default();
                    if position.draft.text().is_empty() {
                        position.draft = Editor::from(text.as_str());
                    } else {
                        self.notice = Some((format!("Shell command failed: {text}"), true));
                    }
                }
                return;
            }
        }
        if let Operation::Close(shell) = operation {
            if let Some(shells) = self.panels.shells.get_mut(id) {
                shells.retain(|value| value.get("itemId").and_then(Value::as_str) != Some(&shell));
            }
            if self.selected.as_deref() == Some(id)
                && matches!(&self.panels.view, Some(panels::View::Terminal(terminal)) if terminal.id == shell)
            {
                self.panels.view = None;
                self.panels.focused = false;
            }
            if let Some(Modal::Resources {
                artifacts: false,
                selected,
            }) = &mut self.modal
            {
                *selected = (*selected).min(
                    self.panels
                        .shells
                        .get(id)
                        .map_or(0, Vec::len)
                        .saturating_sub(1),
                );
            }
        }
        self.panels.checked = None;
        self.refreshed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use ratatui::{Terminal as TestTerminal, backend::TestBackend};
    use serde_json::json;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };

    fn fixture() -> Result<(tempfile::TempDir, Ui)> {
        let temp = tempfile::tempdir()?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = Ui::new(storage, &Config::default());
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: temp.path().into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.workspace = Some(temp.path().into());
        session.workspace_ready = true;
        session.status = Status::Running;
        session.turn_id = Some("working".into());
        session.thread_id = Some("thread".into());
        ui.summaries.push(session.summary());
        ui.sessions.insert("one".into(), session);
        ui.selected = Some("one".into());
        ui.panels.session = ui.selected.clone();
        ui.drilled = true;
        ui.focus = Focus::Composer;
        Ok((temp, ui))
    }
    fn server(
        ui: &Ui,
        count: usize,
    ) -> Result<(mpsc::Receiver<Request>, thread::JoinHandle<Result<()>>)> {
        let listener = UnixListener::bind(super::super::super::server::socket(&ui.storage)?)?;
        let (send, receive) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..count {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request = serde_json::from_str(&line)?;
                serde_json::to_writer(&mut stream, &Reply::Shell(json!({"shell":"shared"})))?;
                stream.write_all(b"\n")?;
                send.send(request)?;
            }
            Ok(())
        });
        Ok((receive, worker))
    }
    fn finish(ui: &mut Ui) -> Result<()> {
        let message = ui.receiver.recv_timeout(Duration::from_secs(5))?;
        let Task::Terminal(id, operation) = message.kind else {
            anyhow::bail!("Expected terminal task")
        };
        ui.finish_terminal(&id, operation, message.result);
        Ok(())
    }
    #[test]
    fn a_completed_snapshot_does_not_delay_input_or_restart_its_cooldown() -> Result<()> {
        for checked in [None, Some(Instant::now() - Duration::from_secs(1))] {
            let (_temp, mut ui) = fixture()?;
            let (requests, worker) = server(&ui, 1)?;
            let (input, _receive) = mpsc::channel();
            let mut terminal = Terminal::new("shared".into());
            terminal.input = Some(input);
            terminal.focused = true;
            terminal.renewed = Some(Instant::now());
            terminal.loading = true;
            terminal.checked = checked;
            ui.panels.view = Some(panels::View::Terminal(Box::new(terminal)));
            ui.panels.focused = true;
            ui.preferences.motion.level = crate::settings::Motion::Off;
            assert_eq!(ui.redraw_interval(), Some(crate::repaint::FRAME_INTERVAL));
            ui.finish_terminal(
                "one",
                Operation::Poll("shared".into()),
                Ok(Reply::Shell(json!({"rows":1,"cols":1,"screen":""}))),
            );
            ui.tick_terminal(true);
            assert!(matches!(
                requests.recv_timeout(Duration::from_secs(2))?,
                Request::Shell {
                    action: shells::Action::Exchange { .. },
                    ..
                }
            ));
            finish(&mut ui)?;
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Fixture server failed"))??;
        }
        Ok(())
    }
    #[test]
    fn bang_bypasses_agent_queue_and_shows_the_working_directory() -> Result<()> {
        let (temp, mut ui) = fixture()?;
        let (requests, worker) = server(&ui, 1)?;
        ui.preferences.input.running = crate::settings::RunningInput::Queue;
        ui.positions.entry("one".into()).or_default().draft = Editor::from("!printf immediate");
        assert!(ui.command_mode());
        let mut terminal = TestTerminal::new(TestBackend::new(160, 40))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("pwd:"));
        assert!(text.contains(&temp.path().display().to_string()));
        ui.send_primary();
        assert!(
            matches!(requests.recv_timeout(Duration::from_secs(5))?, Request::Shell { id, action: shells::Action::Run { command } } if id == "one" && command == "printf immediate")
        );
        assert!(ui.send_queues.is_empty());
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .outgoing
                .is_empty()
        );
        assert!(
            ui.positions
                .get("one")
                .context("position")?
                .draft
                .text()
                .is_empty()
        );
        finish(&mut ui)?;
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Fixture server failed"))??;
        Ok(())
    }
    #[test]
    fn palette_shell_opens_the_helper_escape_hides_and_picker_x_terminates() -> Result<()> {
        let (_temp, mut ui) = fixture()?;
        let (requests, worker) = server(&ui, 5)?;
        assert!(
            ui.palette_commands()
                .iter()
                .any(|(name, _, _)| name == "/shell")
        );
        ui.run_palette_command("/shell", "/shell");
        assert!(matches!(
            requests.recv_timeout(Duration::from_secs(5))?,
            Request::Shell {
                action: shells::Action::Open,
                ..
            }
        ));
        finish(&mut ui)?;
        assert!(ui.terminal_input());
        let right = ui.panels.right;
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        assert_ne!(ui.panels.right, right);
        ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for expected in ["\x03", "\t"] {
            assert!(
                matches!(requests.recv_timeout(Duration::from_secs(5))?, Request::Shell { action: shells::Action::Human { text, focused: true, .. }, .. } if text == expected)
            );
            finish(&mut ui)?;
        }
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.panels.hidden);
        assert!(ui.panels.view.is_some());
        assert!(
            matches!(requests.recv_timeout(Duration::from_secs(5))?, Request::Shell { action: shells::Action::Human { focused: false, text, .. }, .. } if text.is_empty()),
            "Hiding a pane releases input ownership without terminating it"
        );
        ui.panels.shells.insert(
            "one".into(),
            vec![json!({"itemId":"shared","local":true,"command":"Interactive shell"})],
        );
        ui.open_resources(false);
        ui.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(
            matches!(requests.recv_timeout(Duration::from_secs(5))?, Request::Shell { action: shells::Action::Close { shell }, .. } if shell == "shared")
        );
        finish(&mut ui)?;
        assert!(ui.panels.view.is_none());
        assert!(ui.panels.shells.get("one").is_some_and(Vec::is_empty));
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Fixture server failed"))??;
        Ok(())
    }
    #[test]
    fn snapshots_render_colors_and_preserve_terminal_cursor_and_paste_modes() -> Result<()> {
        let mut source = vt100::Parser::new(dimension(4), dimension(20), 0);
        source.process(b"\x1b[31mred\x1b[0m\r\nnext\x1b[?2004h");
        let mut terminal = Terminal::new("one".into());
        terminal.receive(&json!({"rows":4,"cols":20,"screen":base64::engine::general_purpose::STANDARD.encode(source.screen().state_formatted())}))?;
        assert_eq!(terminal.parser.screen().cursor_position(), (1, 4));
        assert!(terminal.parser.screen().bracketed_paste());
        let mut screen = TestTerminal::new(TestBackend::new(20, 4))?;
        screen.draw(|frame| terminal.draw(frame, frame.area(), true))?;
        assert_eq!(
            screen
                .backend()
                .buffer()
                .cell((0, 0))
                .context("first cell")?
                .symbol(),
            "r"
        );
        assert_eq!(
            screen
                .backend()
                .buffer()
                .cell((0, 0))
                .context("first cell")?
                .fg,
            Color::Indexed(1)
        );
        assert_eq!(
            key_text(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true).as_deref(),
            Some("\x1bOA")
        );
        assert_eq!(
            key_text(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL), false).as_deref(),
            Some("\x1b[1;5D")
        );
        Ok(())
    }
}
