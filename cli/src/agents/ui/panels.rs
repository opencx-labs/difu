use super::*;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub(super) enum View {
    PullRequest {
        app: Box<crate::app::App>,
    },
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
    pub hidden: bool,
    pub right: bool,
    pub focused: bool,
    pub rect: Rect,
    pub scroll: usize,
    pub shells: HashMap<String, Vec<Value>>,
    pub loading: bool,
    pub installing: bool,
    pub checked: Option<Instant>,
    pub error: Option<String>,
    footer_rect: Rect,
    footer_scroll: u16,
    footer_width: u16,
    footer_focus: Option<Focus>,
}
impl Panels {
    pub fn new(right: bool) -> Self {
        Self {
            session: None,
            view: None,
            hidden: false,
            right,
            focused: false,
            rect: Rect::default(),
            scroll: 0,
            shells: HashMap::new(),
            loading: false,
            installing: false,
            checked: None,
            error: None,
            footer_rect: Rect::default(),
            footer_scroll: 0,
            footer_width: 0,
            footer_focus: None,
        }
    }
    pub fn visible(&self) -> bool {
        self.view.is_some() && !self.hidden
    }
}
impl Ui {
    pub(super) fn visible_resources(&self) -> Vec<(bool, usize)> {
        let Some(id) = &self.selected else {
            return Vec::new();
        };
        let shells = self.panels.shells.get(id).map_or(0, Vec::len);
        let artifacts = self.sessions.get(id).map_or(0, |s| s.artifacts.len());
        [(false, shells), (true, artifacts)]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .collect()
    }
    pub(super) fn session_footer_height(&self) -> u16 {
        u16::from(
            !self.visible_resources().is_empty()
                || self
                    .selected
                    .as_ref()
                    .is_some_and(|id| !self.prs.all(id).is_empty()),
        )
    }
    /// Controls share one ordered list for drawing and keyboard traversal.
    fn footer_items(&self) -> Vec<(String, Focus, Style)> {
        let resources = self.visible_resources();
        let mut items = Vec::new();
        for artifacts in [true, false] {
            if !artifacts && let Some(id) = &self.selected {
                for (index, link) in self.prs.all(id).iter().enumerate() {
                    let pr = &link.pr;
                    let focus = Focus::PullRequest(index);
                    items.push((
                        format!(
                            " {} · {}{} ",
                            link.badge(),
                            pr.label(),
                            if self.prs.stale(id) { " · stale" } else { "" },
                        ),
                        focus,
                        if self.focus == focus {
                            Style::default().bg(prs::color(pr)).fg(crate::ui::INK)
                        } else {
                            Style::default().fg(prs::color(pr)).bg(BG)
                        },
                    ));
                }
            }
            if let Some((_, count)) = resources.iter().find(|(kind, _)| *kind == artifacts) {
                let focus = Focus::Resources(artifacts);
                items.push((
                    format!(
                        " {} ({count}) ",
                        if artifacts { "Artifacts" } else { "Shells" }
                    ),
                    focus,
                    crate::ui::option_style(self.focus == focus),
                ));
            }
        }
        items
    }
    pub(super) fn draw_session_footer(&mut self, frame: &mut Frame, area: Rect) {
        self.draw_footer(frame, area, Line::default());
    }
    pub(super) fn draw_composer_footer(&mut self, frame: &mut Frame, area: Rect) {
        let status = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(permissions_panel::composer_status)
            .unwrap_or_default();
        self.draw_footer(frame, area, status);
    }
    fn draw_footer(&mut self, frame: &mut Frame, area: Rect, mut line: Line<'static>) {
        if area.is_empty() {
            return;
        }
        let mut controls = Vec::new();
        for (label, focus, style) in self.footer_items() {
            if line.width() > 0 {
                line.spans.push(Span::raw(" "));
            }
            let start = line.width().min(usize::from(u16::MAX)) as u16;
            line.spans.push(Span::styled(label, style));
            let end = line.width().min(usize::from(u16::MAX)) as u16;
            controls.push((start, end, focus));
        }
        let width = line.width().min(usize::from(u16::MAX)) as u16;
        let maximum = width.saturating_sub(area.width);
        let scroll = &mut self.panels.footer_scroll;
        if self.panels.footer_focus != Some(self.focus)
            || self.panels.footer_rect.width != area.width
        {
            if let Some((start, end, _)) =
                controls.iter().find(|(_, _, focus)| *focus == self.focus)
            {
                if *start < *scroll {
                    *scroll = *start;
                } else if *end > scroll.saturating_add(area.width) {
                    *scroll = end.saturating_sub(area.width).min(*start);
                }
            } else {
                *scroll = 0;
            }
        }
        *scroll = (*scroll).min(maximum);
        let scroll = *scroll;
        self.panels.footer_focus = Some(self.focus);
        self.panels.footer_rect = area;
        self.panels.footer_width = width;
        frame.render_widget(Paragraph::new(line).scroll((0, scroll)), area);
        for (start, end, focus) in controls {
            let left = start.max(scroll);
            let right = end.min(scroll.saturating_add(area.width));
            if left >= right {
                continue;
            }
            let action = match focus {
                Focus::Resources(artifacts) => Action::Resources(artifacts),
                Focus::PullRequest(index) => Action::PullRequest(index),
                _ => continue,
            };
            self.hits.push((
                Rect::new(area.x + (left - scroll), area.y, right - left, 1),
                action,
            ));
        }
    }
    pub(super) fn reset_footer(&mut self, session_changed: bool) {
        self.panels.footer_rect.height = 0;
        if session_changed {
            self.panels.footer_scroll = 0;
            self.panels.footer_focus = None;
        }
    }
    pub(super) fn footer_mouse(&mut self, event: MouseEvent) -> bool {
        if self.modal.is_some()
            || !self
                .panels
                .footer_rect
                .contains((event.column, event.row).into())
        {
            return false;
        }
        let delta = match event.kind {
            MouseEventKind::ScrollLeft => -4,
            MouseEventKind::ScrollRight => 4,
            _ => return false,
        };
        self.panels.footer_scroll = self.panels.footer_scroll.saturating_add_signed(delta).min(
            self.panels
                .footer_width
                .saturating_sub(self.panels.footer_rect.width),
        );
        true
    }
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
            self.panels.hidden = false;
            self.panels.focused = false;
            self.panels.session = self.selected.clone();
            self.panels.checked = None;
            self.panels.error = None;
        }
        if let Some(View::PullRequest { app }) = &mut self.panels.view {
            app.tick_visible(visible && !self.panels.hidden);
            if let Some(text) = app.clipboard.take() {
                self.clipboard = Some(text);
            }
        }
        if let Some(View::Artifact {
            browser: Some(browser),
            ..
        }) = &mut self.panels.view
        {
            browser.visible(visible && !self.panels.hidden && self.modal.is_none());
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
                    artifact
                        .workspace
                        .as_deref()
                        .or(session.workspace.as_deref())
                        .context("No workspace")?,
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
        self.panels.hidden = false;
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
        self.panels.hidden = false;
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
            self.panels.hidden = false;
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
    pub(super) fn move_helper(&mut self) {
        if !self.panels.visible() {
            return;
        }
        self.panels.right = !self.panels.right;
        self.preferences.layout.helper_placement = None;
        let right = self.panels.right;
        let result = self.storage.update_config(|c| {
            c.agent_panel_right = right;
            c.preferences.layout.helper_placement = None;
        });
        if let Err(error) = result {
            self.notice = Some((format!("Cannot save pane placement: {error:#}"), true));
        }
    }
    pub(super) fn panel_key(&mut self, key: KeyEvent) -> bool {
        if self.modal.is_some() {
            return false;
        }
        if self.panels.focused
            && self.panels.visible()
            && let Some(View::PullRequest { app }) = &mut self.panels.view
            && !crate::shortcuts::embedded_content(app.shortcut_context())
        {
            app.key_event(key);
            return true;
        }
        use crate::shortcuts::ids::*;
        let shortcut = self.shortcut(key).filter(|action| {
            if self.panels.focused
                && self.panels.visible()
                && let Some(View::PullRequest { app }) = &self.panels.view
            {
                crate::shortcuts::embedded_host_action(
                    *action,
                    app.shortcut_context(),
                    self.panels.right,
                )
            } else {
                true
            }
        });
        if shortcut == Some(HELPER_PLACEMENT) && self.panels.visible() {
            self.move_helper();
            return true;
        }
        if shortcut == Some(HELPER_VISIBILITY) {
            if self.panels.view.is_some() {
                self.panels.hidden = !self.panels.hidden;
                self.panels.focused = !self.panels.hidden;
                if self.panels.hidden {
                    self.focus = if self.changes_visible {
                        Focus::Changes
                    } else {
                        Focus::Composer
                    };
                }
            }
            return true;
        }
        if !self.panels.focused || !self.panels.visible() {
            return false;
        }
        if self.panels.right
            && matches!(
                shortcut,
                Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS)
            )
        {
            // Keep the PR's file/chapter pane reachable before leaving the helper.
            let backwards = shortcut == Some(NAVIGATION_PREVIOUS_FOCUS);
            if let Some(View::PullRequest { app }) = &mut self.panels.view
                && app.view != crate::app::View::Overview
                && ((!backwards && app.focus == crate::app::Focus::Navigation)
                    || (backwards && app.focus == crate::app::Focus::Content))
            {
                if let Some(action) = shortcut {
                    app.shortcut_action(action);
                }
                return true;
            }
            self.cycle_focus(shortcut == Some(NAVIGATION_PREVIOUS_FOCUS));
            return true;
        }
        if shortcut == Some(AGENTS_BACK) {
            self.panels.view = None;
            self.panels.focused = false;
            self.focus = if self.changes_visible {
                Focus::Changes
            } else {
                Focus::Composer
            };
            return true;
        }
        if matches!(shortcut, Some(AGENTS_SIDEBAR | AGENTS_CHANGES)) {
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
        if let Some(View::PullRequest { app }) = &mut self.panels.view {
            app.key_event(key);
            return true;
        }
        if matches!(
            shortcut,
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS)
        ) {
            self.cycle_focus(shortcut == Some(NAVIGATION_PREVIOUS_FOCUS));
        } else {
            self.panels.scroll = shortcut_scroll(
                shortcut,
                self.panels.scroll,
                usize::from(self.panels.rect.height),
                usize::MAX,
            );
        }
        true
    }
    pub(super) fn panel_mouse(&mut self, event: MouseEvent) -> bool {
        if self.modal.is_some() || !self.panels.visible() {
            return false;
        }
        let inside = self.panels.rect.contains((event.column, event.row).into());
        if let Some(View::PullRequest { app }) = &mut self.panels.view
            && (inside || app.modal.is_some())
        {
            self.panels.focused = true;
            app.mouse(event);
            return true;
        }
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
        use crate::shortcuts::ids::*;
        let shortcut = self.shortcut(key);
        if self.modal.is_none() && !self.panels.focused && self.drilled {
            let visible = self
                .footer_items()
                .into_iter()
                .map(|(_, focus, _)| focus)
                .collect::<Vec<_>>();
            if self.focus == Focus::Composer
                && shortcut == Some(COMPOSER_HISTORY_NEXT)
                && let Some(first) = visible.first()
            {
                self.focus = *first;
                return true;
            }
            if matches!(self.focus, Focus::Resources(_) | Focus::PullRequest(_)) {
                if visible.is_empty() {
                    self.focus = Focus::Composer;
                    return false;
                }
                match shortcut {
                    Some(NAVIGATION_UP | AGENTS_BACK) => self.focus = Focus::Composer,
                    Some(NAVIGATION_LEFT | NAVIGATION_RIGHT) => {
                        let current = visible.iter().position(|f| *f == self.focus).unwrap_or(0);
                        let next = if shortcut == Some(NAVIGATION_RIGHT) {
                            (current + 1) % visible.len()
                        } else {
                            (current + visible.len() - 1) % visible.len()
                        };
                        if let Some(next) = visible.get(next) {
                            self.focus = *next;
                        }
                    }
                    Some(NAVIGATION_ACTIVATE) => match self.focus {
                        Focus::PullRequest(index) => self.open_pr_panel(index),
                        Focus::Resources(a) => self.open_resources(a),
                        _ => {}
                    },
                    Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                        self.cycle_focus(shortcut == Some(NAVIGATION_PREVIOUS_FOCUS))
                    }
                    _ => return false,
                }
                return true;
            }
        }
        if let Some(Modal::InstallBrowser { selected, .. }) = &mut self.modal {
            match shortcut {
                _ if key.code == KeyCode::Esc => self.modal = None,
                Some(NAVIGATION_UP) => *selected = selected.saturating_sub(1),
                Some(NAVIGATION_DOWN) => {
                    *selected = (*selected + 1) % if cfg!(target_os = "macos") { 3 } else { 2 }
                }
                Some(NAVIGATION_ACTIVATE) => {
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
        match shortcut {
            _ if key.code == KeyCode::Esc => self.modal = None,
            Some(NAVIGATION_ACTIVATE) => self.select_resource(artifacts, selected),
            Some(NAVIGATION_UP | NAVIGATION_DOWN) => {
                self.modal = Some(Modal::Resources {
                    artifacts,
                    selected: if shortcut == Some(NAVIGATION_UP) {
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
            Some(View::PullRequest { .. }) => "Pull request",
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
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        let move_hint = bindings.primary_hint_in(
            crate::shortcuts::ids::HELPER_PLACEMENT,
            self.shortcut_context(),
        );
        let close_hint = if self.panels.focused {
            format!(
                "{} Close",
                bindings
                    .primary_hint_in(crate::shortcuts::ids::AGENTS_BACK, self.shortcut_context())
            )
        } else {
            "Click to focus".into()
        };
        frame.render_widget(
            Paragraph::new(format!("{move_hint} Move · {close_hint}"))
                .style(Style::default().fg(DIM)),
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
            Some(View::PullRequest { app }) => {
                app.render_area = Some(inner);
                crate::ui::draw(frame, app);
            }
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
