use super::*;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub(super) enum View {
    Shell {
        id: String,
        command: String,
    },
    Artifact {
        path: PathBuf,
        title: String,
        browser: Option<Box<browser::Browser>>,
        error: Option<String>,
    },
}
pub(super) struct Panels {
    pub session: Option<String>,
    pub view: Option<View>,
    pub right: bool,
    pub focused: bool,
    pub rect: Rect,
    pub scroll: usize,
    pub shells: HashMap<String, Vec<Value>>,
    pub loading: bool,
    pub installing: bool,
    pub checked: Option<Instant>,
    pub error: Option<String>,
}
impl Panels {
    pub fn new(right: bool) -> Self {
        Self {
            session: None,
            view: None,
            right,
            focused: false,
            rect: Rect::default(),
            scroll: 0,
            shells: HashMap::new(),
            loading: false,
            installing: false,
            checked: None,
            error: None,
        }
    }
}
impl Ui {
    pub fn panel_right(&self) -> bool {
        self.panels.right
    }
    pub fn browser_input(&self) -> bool {
        self.modal.is_none()
            && self.panels.focused
            && matches!(
                self.panels.view,
                Some(View::Artifact {
                    browser: Some(_),
                    ..
                })
            )
    }
    pub(super) fn tick_panels(&mut self, visible: bool) {
        if self.panels.session != self.selected {
            self.panels.view = None;
            self.panels.focused = false;
            self.panels.session = self.selected.clone();
            self.panels.checked = None;
            self.panels.error = None;
        }
        if let Some(View::Artifact {
            browser: Some(browser),
            ..
        }) = &mut self.panels.view
        {
            browser.visible(visible && self.modal.is_none());
            browser.focus(visible && self.panels.focused && self.modal.is_none());
            browser.tick();
        }
        if visible
            && self.panels.error.is_none()
            && !self.panels.loading
            && self
                .panels
                .checked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(3))
            && let Some(id) = self.selected.clone()
            && self
                .sessions
                .get(&id)
                .is_some_and(|s| matches!(s.job, Job::Coding(_)) && s.thread_id.is_some())
        {
            self.panels.loading = true;
            self.task(Task::Shells(id.clone()), Request::Shells { id }, false);
        }
    }
    pub(super) fn open_resources(&mut self, artifacts: bool) {
        self.modal = Some(Modal::Resources {
            artifacts,
            selected: 0,
        });
        if !artifacts {
            self.panels.error = None;
            self.panels.checked = None;
        }
    }
    fn resources(&self, artifacts: bool) -> Vec<String> {
        let id = self.selected.as_deref().unwrap_or_default();
        if artifacts {
            self.sessions
                .get(id)
                .map(|s| {
                    s.artifacts
                        .iter()
                        .map(|a| format!("{} · {}", a.title, a.path.display()))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.panels
                .shells
                .get(id)
                .into_iter()
                .flatten()
                .map(|v| {
                    v.get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("Shell")
                        .to_owned()
                })
                .collect()
        }
    }
    pub(super) fn select_resource(&mut self, artifacts: bool, index: usize) {
        let id = self.selected.as_deref().unwrap_or_default();
        if artifacts {
            let Some(session) = self.sessions.get(id) else {
                return;
            };
            let Some(artifact) = session.artifacts.get(index) else {
                return;
            };
            let result = (|| -> Result<PathBuf> {
                super::super::artifacts::resolve(
                    session.workspace.as_deref().context("No workspace")?,
                    &artifact.path,
                )
            })();
            match result {
                Ok(path) => {
                    let title = artifact.title.clone();
                    if browser::executable("terminal-browser").is_none() && !self.panels.installing
                    {
                        self.modal = Some(Modal::InstallBrowser {
                            path,
                            title,
                            selected: 0,
                        });
                        return;
                    }
                    self.open_artifact(path, title);
                }
                Err(e) => {
                    self.notice = Some((format!("{e:#}"), true));
                    return;
                }
            }
        } else {
            let Some(shell) = self.panels.shells.get(id).and_then(|s| s.get(index)) else {
                return;
            };
            self.panels.view = Some(View::Shell {
                id: shell
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                command: shell
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("Shell")
                    .into(),
            });
        }
        self.panels.session = self.selected.clone();
        self.panels.focused = true;
        self.panels.scroll = 0;
        self.drilled = true;
        self.modal = None;
    }
    pub(super) fn open_artifact(&mut self, path: PathBuf, title: String) {
        let (browser, error) = if self.panels.installing {
            (
                None,
                Some("Installing terminal-browser with Homebrew… You can keep using difu.".into()),
            )
        } else {
            match browser::Browser::open(&path) {
                Ok(browser) => (Some(Box::new(browser)), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            }
        };
        self.panels.view = Some(View::Artifact {
            path,
            title,
            browser,
            error,
        });
        self.panels.session = self.selected.clone();
        self.panels.focused = true;
        self.drilled = true;
    }
    pub(super) fn browser_choice(&mut self, index: usize) {
        let Some(Modal::InstallBrowser { path, title, .. }) = self.modal.take() else {
            return;
        };
        let install = cfg!(target_os = "macos") && index == 0;
        let external = index == usize::from(cfg!(target_os = "macos"));
        if !install && !external {
            return;
        }
        if external {
            self.panels.view = Some(View::Artifact {
                path,
                title,
                browser: None,
                error: Some(
                    "Opened externally. Install terminal-browser for embedded previews.".into(),
                ),
            });
            self.panels.session = self.selected.clone();
            self.external_artifact();
            return;
        }
        if self.panels.installing {
            return;
        }
        let Some(brew) = browser::executable("brew") else {
            self.notice = Some(("Homebrew is not installed. Install it from brew.sh, or open the artifact in your browser.".into(), true));
            self.modal = Some(Modal::InstallBrowser {
                path,
                title,
                selected: 1,
            });
            return;
        };
        self.panels.installing = true;
        self.open_artifact(path.clone(), title.clone());
        let sender = self.sender.clone();
        let id = self.selected.clone().unwrap_or_default();
        thread::spawn(move || {
            let result = (|| -> Result<Reply> {
                let output = crate::process::run(
                    std::process::Command::new(brew)
                        .args(["install", "--cask", "terminal-browser"])
                        .env("HOMEBREW_NO_AUTO_UPDATE", "1"),
                    None,
                    &crate::process::Cancel::default(),
                )?;
                anyhow::ensure!(
                    output.code == 0,
                    "Homebrew installation failed: {}",
                    crate::model::clean(String::from_utf8_lossy(&output.stderr).trim())
                );
                anyhow::ensure!(
                    browser::executable("terminal-browser").is_some(),
                    "Homebrew finished, but terminal-browser was not found on PATH"
                );
                Ok(Reply::Ok)
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = sender.send(ResultMessage {
                kind: Task::InstallBrowser(id, path, title),
                result,
            });
        });
    }
    pub(super) fn draw_browser_install(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::InstallBrowser { selected, .. }) = self.modal.as_ref() else {
            return;
        };
        let selected = *selected;
        let text = if cfg!(target_os = "macos") {
            "Embed this artifact using terminal-browser?\n\nOptional local package: about 140 MB download / 339 MB installed. Homebrew installs it only if you choose Install. difu uses its engine directly, without installing agent skills or changing terminal settings."
        } else {
            "Embedded previews need the optional terminal-browser package. Install it using the Linux instructions at https://terminal-browser.sh, then reopen this artifact. You can open it in your browser now."
        };
        let lines = wrapped(text, area.width);
        let offset = (lines.len() as u16).saturating_add(2);
        frame.render_widget(
            Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()),
            area,
        );
        let options: &[&str] = if cfg!(target_os = "macos") {
            &[
                "Install terminal-browser with Homebrew",
                "Open in external browser",
                "Cancel",
            ]
        } else {
            &["Open in external browser", "Cancel"]
        };
        for (index, label) in options.iter().enumerate() {
            let row = Rect::new(
                area.x,
                area.y
                    .saturating_add(offset)
                    .saturating_add(index as u16 * 2),
                area.width,
                1,
            )
            .intersection(area);
            self.button(
                frame,
                row,
                label,
                Action::BrowserChoice(index),
                selected == index,
            );
        }
    }
    pub(super) fn panel_key(&mut self, key: KeyEvent) -> bool {
        if self.modal.is_some() {
            return false;
        }
        if key.code == KeyCode::Char('p')
            && key.modifiers == KeyModifiers::ALT
            && self.panels.view.is_some()
        {
            self.panels.right = !self.panels.right;
            let result = self.storage.load_config().and_then(|mut c| {
                c.agent_panel_right = self.panels.right;
                self.storage.save_config(&c)
            });
            if let Err(e) = result {
                self.notice = Some((format!("Cannot save pane placement: {e:#}"), true));
            }
            return true;
        }
        if !self.panels.focused || self.panels.view.is_none() {
            return false;
        }
        if key.code == KeyCode::Esc {
            self.panels.view = None;
            self.panels.focused = false;
            self.focus = Focus::Composer;
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('b' | 'd'))
        {
            self.panels.focused = false;
            return false;
        }
        if let Some(View::Artifact {
            browser: Some(browser),
            ..
        }) = &mut self.panels.view
        {
            browser.key(key);
            return true;
        }
        match key.code {
            KeyCode::Up => self.panels.scroll = self.panels.scroll.saturating_sub(1),
            KeyCode::Down => self.panels.scroll = self.panels.scroll.saturating_add(1),
            KeyCode::PageUp => {
                self.panels.scroll = self
                    .panels
                    .scroll
                    .saturating_sub(self.panels.rect.height as usize)
            }
            KeyCode::PageDown => {
                self.panels.scroll = self
                    .panels
                    .scroll
                    .saturating_add(self.panels.rect.height as usize)
            }
            KeyCode::Home => self.panels.scroll = 0,
            KeyCode::End => self.panels.scroll = usize::MAX,
            KeyCode::Tab => {
                self.panels.focused = false;
                self.focus = Focus::Composer;
            }
            _ => {}
        }
        true
    }
    pub(super) fn panel_mouse(&mut self, event: MouseEvent) -> bool {
        if self.modal.is_some() || self.panels.view.is_none() {
            return false;
        }
        let inside = self.panels.rect.contains((event.column, event.row).into());
        if matches!(event.kind, MouseEventKind::Down(_)) {
            self.panels.focused = inside;
        }
        if !inside {
            return false;
        }
        if let Some(View::Artifact {
            browser: Some(browser),
            ..
        }) = &mut self.panels.view
        {
            browser.mouse(event);
        } else {
            match event.kind {
                MouseEventKind::ScrollDown => {
                    self.panels.scroll = self.panels.scroll.saturating_add(3)
                }
                MouseEventKind::ScrollUp => {
                    self.panels.scroll = self.panels.scroll.saturating_sub(3)
                }
                _ => {}
            }
        }
        true
    }
    pub(super) fn external_artifact(&mut self) {
        let Some(View::Artifact { path, .. }) = &self.panels.view else {
            return;
        };
        let path = path.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = (|| -> Result<Reply> {
                let status = std::process::Command::new(if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                })
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .context("Could not start the external browser")?;
                anyhow::ensure!(
                    status.success(),
                    "External browser could not open the artifact ({status})"
                );
                Ok(Reply::Ok)
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = sender.send(ResultMessage {
                kind: Task::OpenArtifact,
                result,
            });
        });
    }

    pub(super) fn refresh_artifact(&mut self) {
        self.panels.focused = true;
        if let Some(View::Artifact {
            browser: Some(browser),
            ..
        }) = &mut self.panels.view
        {
            browser.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        }
    }
    pub(super) fn draw_resources(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        artifacts: bool,
        selected: usize,
    ) {
        let items = self.resources(artifacts);
        if items.is_empty() {
            let empty = if artifacts {
                "No registered HTML artifacts. New coding sessions can present artifacts after creating them."
            } else if self.panels.loading {
                "Loading open shells…"
            } else {
                self.panels
                    .error
                    .as_deref()
                    .unwrap_or("No open background shells")
            };
            frame.render_widget(Paragraph::new(empty).wrap(Wrap { trim: false }), area);
            return;
        }
        let offset = selected.saturating_sub(area.height.saturating_sub(1) as usize);
        for (index, item) in items
            .iter()
            .enumerate()
            .skip(offset)
            .take(area.height as usize)
        {
            self.button(
                frame,
                Rect::new(area.x, area.y + (index - offset) as u16, area.width, 1),
                &crate::model::clean(item),
                Action::Resource(artifacts, index),
                index == selected,
            );
        }
    }
    pub(super) fn resource_key(&mut self, key: KeyEvent) -> bool {
        if let Some(Modal::InstallBrowser { selected, .. }) = &mut self.modal {
            match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    *selected = (*selected + 1) % if cfg!(target_os = "macos") { 3 } else { 2 }
                }
                KeyCode::Enter => {
                    let index = *selected;
                    self.browser_choice(index);
                }
                _ => {}
            }
            return true;
        }
        let Some(Modal::Resources {
            artifacts,
            selected,
        }) = self.modal.as_ref()
        else {
            return false;
        };
        let artifacts = *artifacts;
        let selected = *selected;
        let count = self.resources(artifacts).len();
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Enter => self.select_resource(artifacts, selected),
            KeyCode::Up | KeyCode::Down => {
                self.modal = Some(Modal::Resources {
                    artifacts,
                    selected: if key.code == KeyCode::Up {
                        selected.saturating_sub(1)
                    } else {
                        selected.saturating_add(1).min(count.saturating_sub(1))
                    },
                })
            }
            _ => {}
        }
        true
    }
    pub(super) fn draw_panel(&mut self, frame: &mut Frame, area: Rect) {
        let title = match &self.panels.view {
            Some(View::Shell { .. }) => "Shell output",
            Some(View::Artifact { title, .. }) => title,
            _ => return,
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(crate::model::clean(title))
            .border_style(Style::default().fg(if self.panels.focused { ACCENT } else { BORDER }));
        let mut inner = block.inner(area);
        frame.render_widget(block, area);
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        inner.height = inner.height.saturating_sub(2);
        self.panels.rect = inner;
        frame.render_widget(
            Paragraph::new("Alt+P Move · Esc Close").style(Style::default().fg(DIM)),
            footer,
        );
        if matches!(self.panels.view, Some(View::Artifact { .. })) {
            self.button(
                frame,
                Rect::new(inner.x, inner.y, 12.min(inner.width), 1),
                " Refresh ",
                Action::RefreshArtifact,
                false,
            );
            if inner.width > 13 {
                self.button(
                    frame,
                    Rect::new(inner.x + 13, inner.y, inner.width - 13, 1),
                    " Open in browser ",
                    Action::ExternalArtifact,
                    false,
                );
            }
            inner.y = inner.y.saturating_add(2);
            inner.height = inner.height.saturating_sub(2);
            self.panels.rect = inner;
        }
        match &mut self.panels.view {
            Some(View::Artifact { browser, error, .. }) => {
                if let Some(browser) = browser {
                    if let Some(error) = &browser.error {
                        frame.render_widget(
                            Paragraph::new(error.as_str()).wrap(Wrap { trim: false }),
                            inner,
                        );
                    } else if self.modal.is_none() {
                        browser.draw(frame, inner);
                    }
                } else {
                    frame.render_widget(
                        Paragraph::new(error.as_deref().unwrap_or("Browser unavailable"))
                            .wrap(Wrap { trim: false }),
                        inner,
                    );
                }
            }
            Some(View::Shell { id, command }) => {
                let session = self.selected.as_ref().and_then(|s| self.sessions.get(s));
                let output = session
                    .and_then(|s| s.entries.iter().find(|e| e.id == *id))
                    .map(|e| {
                        e.text
                            .split_once('\n')
                            .map_or(e.text.as_str(), |(_, output)| output)
                    })
                    .unwrap_or("Waiting for shell output…");
                let active = self
                    .selected
                    .as_ref()
                    .and_then(|s| self.panels.shells.get(s))
                    .is_some_and(|s| {
                        s.iter()
                            .any(|v| v.get("itemId").and_then(Value::as_str) == Some(id))
                    });
                let lines = wrapped(
                    &format!(
                        "{} · {}\n\n{}",
                        if active { "Running" } else { "Finished" },
                        command,
                        output
                    ),
                    inner.width,
                );
                self.panels.scroll = self
                    .panels
                    .scroll
                    .min(lines.len().saturating_sub(inner.height as usize));
                frame.render_widget(
                    Paragraph::new(
                        lines
                            .into_iter()
                            .skip(self.panels.scroll)
                            .take(inner.height as usize)
                            .map(Line::from)
                            .collect::<Vec<_>>(),
                    ),
                    inner,
                );
            }
            _ => {}
        }
    }
}
