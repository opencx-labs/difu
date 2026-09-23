//! terminal-browser's documented embedded socket protocol. No HTTP server.
use crate::process::ChildGroup;
use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Color};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

static NEXT_IMAGE: AtomicU32 = AtomicU32::new(0x700000);
// Resolve the installed package without executing its first-run CLI setup.
pub(super) fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ])
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}
fn package_root(binary: &Path) -> Result<PathBuf> {
    let binary = binary.canonicalize()?;
    let root = binary
        .parent()
        .and_then(Path::parent)
        .context("Unknown terminal-browser package layout")?;
    ensure!(
        root.join("browser/dist/main.js").is_file(),
        "Unsupported terminal-browser package layout; use Open in browser"
    );
    Ok(root.to_path_buf())
}
struct Engine {
    runtime: PathBuf,
    request: Vec<u8>,
    response: Vec<u8>,
    control: Option<UnixStream>,
    accepted: bool,
    started: Instant,
}
impl Engine {
    fn poll(&mut self) -> Result<()> {
        if self.accepted {
            return Ok(());
        }
        ensure!(
            self.started.elapsed() < Duration::from_secs(60),
            "Embedded browser startup timed out; use Open in browser"
        );
        if self.control.is_none() {
            if !self.runtime.exists() {
                return Ok(());
            }
            for entry in std::fs::read_dir(&self.runtime)? {
                let path = entry?.path().join("daemon.sock");
                if path.exists() {
                    match UnixStream::connect(path) {
                        Ok(stream) => {
                            stream.set_nonblocking(true)?;
                            self.control = Some(stream);
                            break;
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::ConnectionRefused
                                    | std::io::ErrorKind::NotFound
                            ) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
        let Some(stream) = &mut self.control else {
            return Ok(());
        };
        while !self.request.is_empty() {
            match stream.write(&self.request) {
                Ok(0) => anyhow::bail!("Browser engine closed during startup"),
                Ok(n) => {
                    self.request.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        let mut bytes = [0; 4096];
        loop {
            match stream.read(&mut bytes) {
                Ok(0) => anyhow::bail!("Browser engine disconnected during startup"),
                Ok(n) => self
                    .response
                    .extend_from_slice(bytes.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
            ensure!(
                self.response.len() < 65_536,
                "Browser engine response too large"
            );
            if let Some(end) = self.response.iter().position(|b| *b == b'\n') {
                let value: Value =
                    serde_json::from_slice(self.response.get(..end).unwrap_or_default())?;
                ensure!(
                    value.get("ok").and_then(Value::as_bool) == Some(true),
                    "Browser could not open artifact: {}",
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown engine error")
                );
                self.accepted = true;
                return Ok(());
            }
        }
    }
}
pub struct Browser {
    child: ChildGroup,
    engine: Option<Engine>,
    _directory: tempfile::TempDir,
    listener: UnixListener,
    stream: Option<UnixStream>,
    input: Vec<u8>,
    output: Vec<u8>,
    area: Rect,
    cell: [u16; 2],
    image: u32,
    grid: Option<(u16, u16)>,
    pub error: Option<String>,
    focused: bool,
    visible: bool,
}
impl Browser {
    pub fn open(path: &Path) -> Result<Self> {
        let binary = executable("terminal-browser").context("terminal-browser is not installed")?;
        let root = package_root(&binary)?;
        let size =
            crossterm::terminal::window_size().context("Terminal pixel dimensions unavailable")?;
        ensure!(
            size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0,
            "Embedded browser needs terminal pixel dimensions; use Open in browser"
        );
        let cell = [size.width / size.columns, size.height / size.rows];
        ensure!(cell[0] > 0 && cell[1] > 0, "Invalid terminal cell size");
        let directory = tempfile::Builder::new()
            .prefix("difu-browser-")
            .tempdir_in("/tmp")?;
        let socket = directory.path().join("embed.sock");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let tty = Command::new("tty").stdin(Stdio::inherit()).output()?;
        ensure!(
            tty.status.success(),
            "No terminal available for embedded browser"
        );
        let url =
            url::Url::from_file_path(path).map_err(|_| anyhow::anyhow!("Invalid artifact path"))?;
        let tty = String::from_utf8_lossy(&tty.stdout).trim().to_owned();
        let mut environment = serde_json::Map::new();
        for key in ["TERM", "TERM_PROGRAM", "COLORTERM", "LANG"] {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.into(), json!(value));
            }
        }
        for (key, value) in [
            ("PIXEL_EMBED", socket.clone()),
            ("XDG_RUNTIME_DIR", directory.path().join("run")),
            ("XDG_STATE_HOME", directory.path().join("state")),
            ("XDG_CACHE_HOME", directory.path().join("cache")),
            ("XDG_DATA_HOME", directory.path().join("data")),
            ("TERMINAL_BROWSER_DIST_ROOT", root.clone()),
        ] {
            environment.insert(key.into(), json!(value));
        }
        environment.insert("PIXEL_TTY".into(), json!(tty));
        let executable = root.join(if cfg!(target_os = "macos") {
            "electron/terminal-browser.app/Contents/MacOS/terminal-browser"
        } else {
            "electron/pixel"
        });
        ensure!(
            executable.is_file(),
            "Installed browser engine was not found; use Open in browser"
        );
        let mut command = Command::new(executable);
        command
            .arg(root.join("browser/dist/main.js"))
            .current_dir(root.join("browser"))
            .env_remove("ELECTRON_RUN_AS_NODE")
            .env_remove("PIXEL_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if cfg!(target_os = "linux")
            && std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none()
        {
            command.args(["--ozone-platform=headless", "--screen-info={8192x8192}"]);
        }
        for (key, value) in &environment {
            if let Some(value) = value.as_str() {
                command.env(key, value);
            }
        }
        let child = ChildGroup::spawn(&mut command)
            .context("Could not start the installed browser engine")?;
        let request = json!({"cmd":"open", "tty":tty, "argv":[url.as_str()], "env":environment, "cwd":path.parent()});
        let engine = Some(Engine {
            runtime: directory.path().join("run"),
            request: format!("{request}\n").into_bytes(),
            response: Vec::new(),
            control: None,
            accepted: false,
            started: Instant::now(),
        });
        Ok(Self {
            child,
            engine,
            _directory: directory,
            listener,
            stream: None,
            input: Vec::new(),
            output: Vec::new(),
            area: Rect::default(),
            cell,
            image: NEXT_IMAGE.fetch_add(1, Ordering::Relaxed),
            grid: None,
            error: None,
            focused: false,
            visible: true,
        })
    }
    fn send(&mut self, value: Value) {
        self.output.extend(value.to_string().as_bytes());
        self.output.push(b'\n');
    }
    fn size(&self, kind: &str) -> Value {
        json!({"type":kind,"cols":self.area.width,"rows":self.area.height,
            "width":u32::from(self.area.width)*u32::from(self.cell[0]),"height":u32::from(self.area.height)*u32::from(self.cell[1]),"cell":self.cell})
    }
    pub fn visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            self.send(json!({"type":"visible","value":visible}));
        }
    }
    pub fn paste(&mut self, text: &str) {
        self.send(json!({"type":"paste","text":text}));
    }
    pub fn focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.send(json!({"type":"focus","focused":focused}));
        }
    }
    pub fn tick(&mut self) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.poll() {
            self.error = Some(format!("{error:#}"));
        }
    }
    fn poll(&mut self) -> Result<()> {
        if let Some(status) = self.child.child.try_wait()? {
            anyhow::bail!("Embedded browser exited ({status}); use Open in browser");
        }
        if let Some(engine) = &mut self.engine {
            engine.poll()?;
        }
        if self.stream.is_none() {
            if let Some(engine) = &self.engine {
                ensure!(
                    engine.started.elapsed() < Duration::from_secs(60),
                    "Embedded browser did not connect; use Open in browser"
                );
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true)?;
                    self.stream = Some(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(stream) = &mut self.stream {
            let mut data = [0; 8192];
            loop {
                match stream.read(&mut data) {
                    Ok(0) => anyhow::bail!("Embedded browser disconnected"),
                    Ok(n) => self.input.extend(data.get(..n).unwrap_or_default()),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
                ensure!(
                    self.input.len() < 1_048_576,
                    "Embedded browser message too large"
                );
            }
        }
        while let Some(end) = self.input.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.input.drain(..=end).collect();
            let message: Value = serde_json::from_slice(&line)?;
            match message.get("type").and_then(Value::as_str) {
                Some("join") => {
                    let mut init = self.size("init");
                    if let Some(init) = init.as_object_mut() {
                        init.insert("imageId".into(), json!(self.image));
                        init.insert("transport".into(), json!("file"));
                        init.insert("focused".into(), json!(self.focused));
                    }
                    self.send(init);
                }
                Some("placed") => {
                    self.grid = Some((
                        message
                            .get("cols")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            .min(65535) as u16,
                        message
                            .get("rows")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            .min(65535) as u16,
                    ));
                }
                _ => {}
            }
        }
        if let Some(stream) = &mut self.stream {
            while !self.output.is_empty() {
                match stream.write(&self.output) {
                    Ok(0) => anyhow::bail!("Embedded browser socket closed"),
                    Ok(n) => {
                        self.output.drain(..n);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }
    pub fn draw(&mut self, frame: &mut Frame, mut area: Rect) {
        area.width = area.width.min(DIACRITICS.len() as u16);
        area.height = area.height.min(DIACRITICS.len() as u16);
        if let Ok(size) = crossterm::terminal::window_size()
            && size.columns > 0
            && size.rows > 0
            && size.width > 0
            && size.height > 0
        {
            let cell = [size.width / size.columns, size.height / size.rows];
            if cell != self.cell {
                self.cell = cell;
                self.area = Rect::default();
            }
        }
        if area != self.area {
            self.area = area;
            self.grid = None;
            self.send(self.size("size"));
        }
        if let Some((cols, rows)) = self.grid {
            let color = Color::Rgb(
                (self.image >> 16) as u8,
                (self.image >> 8) as u8,
                self.image as u8,
            );
            for row in 0..rows.min(area.height) {
                for col in 0..cols.min(area.width) {
                    let Some(r) = DIACRITICS
                        .get(row as usize)
                        .and_then(|x| char::from_u32(*x))
                    else {
                        continue;
                    };
                    let Some(c) = DIACRITICS
                        .get(col as usize)
                        .and_then(|x| char::from_u32(*x))
                    else {
                        continue;
                    };
                    frame.buffer_mut()[(area.x + col, area.y + row)]
                        .set_symbol(&format!("\u{10eeee}{r}{c}"))
                        .set_fg(color);
                }
            }
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        let name = match key.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "enter".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::Esc => "escape".into(),
            KeyCode::Left => "left".into(),
            KeyCode::Right => "right".into(),
            KeyCode::Up => "up".into(),
            KeyCode::Down => "down".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pageup".into(),
            KeyCode::PageDown => "pagedown".into(),
            KeyCode::Delete => "delete".into(),
            _ => return,
        };
        let mut msg = json!({"type":"key","key":name,"kind":"press","mods":mods(key.modifiers)});
        if let KeyCode::Char(c) = key.code
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            && let Some(msg) = msg.as_object_mut()
        {
            msg.insert("text".into(), json!(c.to_string()));
        }
        self.send(msg);
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        if !self.area.contains((event.column, event.row).into()) {
            return;
        }
        let (kind, button) = match event.kind {
            MouseEventKind::Down(b) => ("down", Some(b)),
            MouseEventKind::Up(b) => ("up", Some(b)),
            MouseEventKind::ScrollDown => ("scrolldown", None),
            MouseEventKind::ScrollUp => ("scrollup", None),
            _ => ("move", None),
        };
        let button = match button {
            Some(MouseButton::Left) => "left",
            Some(MouseButton::Right) => "right",
            Some(MouseButton::Middle) => "middle",
            None => "none",
        };
        self.send(json!({"type":"mouse","kind":kind,"button":button,"mods":mods(event.modifiers),
            "x":u32::from(event.column-self.area.x)*u32::from(self.cell[0])+u32::from(self.cell[0]/2),
            "y":u32::from(event.row-self.area.y)*u32::from(self.cell[1])+u32::from(self.cell[1]/2)}));
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.stop();
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = write!(tty, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", self.image);
        }
    }
}
fn mods(m: KeyModifiers) -> Value {
    json!({"shift":m.contains(KeyModifiers::SHIFT),"alt":m.contains(KeyModifiers::ALT),"ctrl":m.contains(KeyModifiers::CONTROL),"super":m.contains(KeyModifiers::SUPER)})
}

// Unicode combining marks specified by the Kitty graphics placeholder protocol.
const DIACRITICS: &[u32] = &[
    0x0305, 0x030d, 0x030e, 0x0310, 0x0312, 0x033d, 0x033e, 0x033f, 0x0346, 0x034a, 0x034b, 0x034c,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035b, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036a, 0x036b, 0x036c, 0x036d, 0x036e, 0x036f, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059c, 0x059d, 0x059e, 0x059f, 0x05a0, 0x05a1,
    0x05a8, 0x05a9, 0x05ab, 0x05ac, 0x05af, 0x05c4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065a, 0x065b, 0x065d, 0x065e, 0x06d6, 0x06d7, 0x06d8,
    0x06d9, 0x06da, 0x06db, 0x06dc, 0x06df, 0x06e0, 0x06e1, 0x06e2, 0x06e4, 0x06e7, 0x06e8, 0x06eb,
    0x06ec, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073a, 0x073d, 0x073f, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074a, 0x07eb, 0x07ec, 0x07ed, 0x07ee, 0x07ef, 0x07f0, 0x07f1, 0x07f3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081b, 0x081c, 0x081d, 0x081e, 0x081f, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082a, 0x082b, 0x082c, 0x082d, 0x0951, 0x0953, 0x0954,
    0x0f82, 0x0f83, 0x0f86, 0x0f87, 0x135d, 0x135e, 0x135f, 0x17dd, 0x193a, 0x1a17, 0x1a75, 0x1a76,
    0x1a77, 0x1a78, 0x1a79, 0x1a7a, 0x1a7b, 0x1a7c, 0x1b6b, 0x1b6d, 0x1b6e, 0x1b6f, 0x1b70, 0x1b71,
    0x1b72, 0x1b73, 0x1cd0, 0x1cd1, 0x1cd2, 0x1cda, 0x1cdb, 0x1ce0, 0x1dc0, 0x1dc1, 0x1dc3, 0x1dc4,
    0x1dc5, 0x1dc6, 0x1dc7, 0x1dc8, 0x1dc9, 0x1dcb, 0x1dcc, 0x1dd1, 0x1dd2, 0x1dd3, 0x1dd4, 0x1dd5,
    0x1dd6, 0x1dd7, 0x1dd8, 0x1dd9, 0x1dda, 0x1ddb, 0x1ddc, 0x1ddd, 0x1dde, 0x1ddf, 0x1de0, 0x1de1,
    0x1de2, 0x1de3, 0x1de4, 0x1de5, 0x1de6, 0x1dfe, 0x20d0, 0x20d1, 0x20d4, 0x20d5, 0x20d6, 0x20d7,
    0x20db, 0x20dc, 0x20e1, 0x20e7, 0x20e9, 0x20f0, 0x2cef, 0x2cf0, 0x2cf1, 0x2de0, 0x2de1, 0x2de2,
    0x2de3, 0x2de4, 0x2de5, 0x2de6, 0x2de7, 0x2de8, 0x2de9, 0x2dea, 0x2deb, 0x2dec, 0x2ded, 0x2dee,
    0x2def, 0x2df0, 0x2df1, 0x2df2, 0x2df3, 0x2df4, 0x2df5, 0x2df6, 0x2df7, 0x2df8, 0x2df9, 0x2dfa,
    0x2dfb, 0x2dfc, 0x2dfd, 0x2dfe, 0x2dff, 0xa66f, 0xa67c, 0xa67d, 0xa6f0, 0xa6f1, 0xa8e0, 0xa8e1,
    0xa8e2, 0xa8e3, 0xa8e4, 0xa8e5, 0xa8e6, 0xa8e7, 0xa8e8, 0xa8e9, 0xa8ea, 0xa8eb, 0xa8ec, 0xa8ed,
    0xa8ee, 0xa8ef, 0xa8f0, 0xa8f1, 0xaab0, 0xaab2, 0xaab3, 0xaab7, 0xaab8, 0xaabe, 0xaabf, 0xaac1,
    0xfe20, 0xfe21, 0xfe22, 0xfe23, 0xfe24, 0xfe25, 0xfe26, 0x10a0f, 0x10a38, 0x1d185, 0x1d186,
    0x1d187, 0x1d188, 0x1d189, 0x1d1aa, 0x1d1ab, 0x1d1ac, 0x1d1ad, 0x1d242, 0x1d243, 0x1d244,
];

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::io::{BufRead, BufReader};

    #[test]
    fn package_resolution_follows_symlinks_without_running_cli() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("terminal-browser");
        std::fs::create_dir_all(root.join("bin"))?;
        std::fs::create_dir_all(root.join("browser/dist"))?;
        std::fs::write(root.join("bin/terminal-browser"), "DO NOT EXECUTE")?;
        std::fs::write(root.join("browser/dist/main.js"), "")?;
        let link = dir.path().join("linked-browser");
        std::os::unix::fs::symlink(root.join("bin/terminal-browser"), &link)?;
        assert_eq!(package_root(&link)?, root.canonicalize()?);
        std::fs::remove_file(root.join("browser/dist/main.js"))?;
        assert!(package_root(&link).is_err());
        Ok(())
    }
    #[test]
    fn engine_handshake_keeps_control_connection_and_reports_failure() -> Result<()> {
        let dir = tempfile::Builder::new()
            .prefix("difu-engine-test-")
            .tempdir_in("/tmp")?;
        let runtime = dir.path().join("engine");
        std::fs::create_dir(&runtime)?;
        let listener = UnixListener::bind(runtime.join("daemon.sock"))?;
        let mut engine = Engine {
            runtime: dir.path().into(),
            request: b"{\"cmd\":\"open\"}\n".to_vec(),
            response: Vec::new(),
            control: None,
            accepted: false,
            started: Instant::now(),
        };
        engine.poll()?;
        let (mut peer, _) = listener.accept()?;
        peer.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut line = String::new();
        BufReader::new(peer.try_clone()?).read_line(&mut line)?;
        assert_eq!(line, "{\"cmd\":\"open\"}\n");
        peer.write_all(b"{\"ok\":true}\n")?;
        engine.poll()?;
        assert!(engine.accepted);
        assert!(engine.control.is_some());
        engine.accepted = false;
        engine.response.clear();
        peer.write_all(b"{\"ok\":false,\"error\":\"fixture error\"}\n")?;
        assert!(
            engine
                .poll()
                .err()
                .context("Expected engine error")?
                .to_string()
                .contains("fixture error")
        );
        Ok(())
    }
    #[test]
    #[ignore = "Requires installed terminal-browser and a pixel-sized interactive terminal"]
    fn local_browser_smoke() -> Result<()> {
        let path = PathBuf::from(std::env::var("DIFU_BROWSER_SMOKE_ARTIFACT")?);
        let mut browser = Browser::open(&path)?;
        browser.area = Rect::new(1, 2, 70, 24);
        let start = Instant::now();
        let mut resized = false;
        while start.elapsed() < Duration::from_secs(30) {
            browser.poll()?;
            if browser.grid == Some((70, 24)) && !resized {
                browser.area = Rect::new(1, 2, 50, 20);
                browser.grid = None;
                browser.send(browser.size("size"));
                resized = true;
            } else if resized && browser.grid == Some((50, 20)) {
                // Give the renderer time to paint the page after placement.
                for _ in 0..30 {
                    browser.poll()?;
                    std::thread::sleep(Duration::from_millis(100));
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("Browser did not render and resize")
    }
    #[test]
    fn embedded_handshake_clips_graphics_and_forwards_local_coordinates() -> Result<()> {
        let directory = tempfile::Builder::new()
            .prefix("difu-embed-test-")
            .tempdir_in("/tmp")?;
        let socket = directory.path().join("socket");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let mut peer = UnixStream::connect(&socket)?;
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        let child = ChildGroup::spawn(Command::new("sleep").arg("30"))?;
        let mut browser = Browser {
            child,
            engine: None,
            _directory: directory,
            listener,
            stream: None,
            input: Vec::new(),
            output: Vec::new(),
            area: Rect::default(),
            cell: [10, 20],
            image: 0x700001,
            grid: None,
            error: None,
            focused: false,
            visible: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 30))?;
        let area = Rect::new(10, 4, 20, 12);
        terminal.draw(|frame| browser.draw(frame, area))?;
        peer.write_all(b"{\"type\":\"join\"}\n")?;
        browser.poll()?;
        let mut reader = BufReader::new(peer.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        assert_eq!(
            serde_json::from_str::<Value>(&line)?.get("type"),
            Some(&json!("size"))
        );
        line.clear();
        reader.read_line(&mut line)?;
        let init: Value = serde_json::from_str(&line)?;
        assert_eq!(init.get("type"), Some(&json!("init")));
        assert_eq!(init.get("width"), Some(&json!(200)));
        assert_eq!(init.get("height"), Some(&json!(240)));
        peer.write_all(b"{\"type\":\"placed\",\"cols\":90,\"rows\":80}\n")?;
        browser.poll()?;
        terminal.draw(|frame| browser.draw(frame, area))?;
        let buffer = terminal.backend().buffer();
        assert!(buffer[(10, 4)].symbol().starts_with('\u{10eeee}'));
        assert!(!buffer[(9, 4)].symbol().contains('\u{10eeee}'));
        assert!(!buffer[(30, 4)].symbol().contains('\u{10eeee}'));
        assert!(!buffer[(10, 16)].symbol().contains('\u{10eeee}'));
        browser.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        });
        browser.poll()?;
        line.clear();
        reader.read_line(&mut line)?;
        let click: Value = serde_json::from_str(&line)?;
        assert_eq!(click.get("x"), Some(&json!(25)));
        assert_eq!(click.get("y"), Some(&json!(70)));
        browser.paste("line one\nline two");
        browser.poll()?;
        line.clear();
        reader.read_line(&mut line)?;
        let paste: Value = serde_json::from_str(&line)?;
        assert_eq!(paste.get("type"), Some(&json!("paste")));
        assert_eq!(paste.get("text"), Some(&json!("line one\nline two")));
        terminal.draw(|frame| browser.draw(frame, Rect::new(1, 1, 50, 20)))?;
        assert!(browser.grid.is_none());
        browser.poll()?;
        line.clear();
        reader.read_line(&mut line)?;
        let resize: Value = serde_json::from_str(&line)?;
        assert_eq!(resize.get("cols"), Some(&json!(50)));
        assert_eq!(resize.get("rows"), Some(&json!(20)));
        Ok(())
    }
}
