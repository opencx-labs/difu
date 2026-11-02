//! `/cloud`: every saved cloud server with its connection, version, sessions,
//! logins, repositories and disk space, and a server's disk breakdown with
//! cleanups. Everything shown comes from the server through this computer's
//! agent service (`Request::Servers`, `Request::Disk`, `Request::DiskClean`).
use super::*;
use crate::{
    cloud::{
        disk::{self, Action, Report, human},
        federation::ServerStatus,
    },
    shortcuts::{Bindings, ids::*},
};
use ratatui::style::Modifier;

/// The panel asks the local service for server state this often while open.
const REFRESH: Duration = Duration::from_secs(2);
/// While a server measures its disk, the panel reads its status this often.
const MEASURING: Duration = Duration::from_secs(3);
/// Without the panel, server disk status for the sessions list is read this
/// often while server sessions are listed.
const INDICATOR: Duration = Duration::from_secs(30);
const BAR: usize = 24;

type Fetched = std::result::Result<Vec<ServerStatus>, String>;

enum Outcome {
    Disk(String, std::result::Result<Report, String>),
    Cleaned(String, std::result::Result<disk::Cleaned, String>),
}

#[derive(Default)]
pub(super) struct State {
    servers: Vec<ServerStatus>,
    loading: Option<mpsc::Receiver<Fetched>>,
    fetched: Option<Instant>,
    error: Option<String>,
    selected: usize,
    /// The selected server's breakdown and cleanups.
    detail: bool,
    /// The highlighted detail row: Refresh, then each of `Action::ALL`.
    action: usize,
    /// A cleanup waiting for confirmation.
    confirming: Option<Action>,
    /// A refresh or cleanup the user started, with what it is doing.
    running: Option<(String, mpsc::Receiver<Outcome>)>,
    /// A background read of a server that is measuring.
    polling: Option<mpsc::Receiver<Outcome>>,
    polled: Option<Instant>,
    /// The last action's outcome; `true` for an error.
    message: Option<(String, bool)>,
    /// Saved servers whose disk is low or critical, for the sessions list and
    /// the new-session form.
    disk: HashMap<String, disk::Status>,
    indicator: Option<mpsc::Receiver<Fetched>>,
    indicated: Option<Instant>,
}

fn fetch(storage: &Storage) -> mpsc::Receiver<Fetched> {
    let (tx, rx) = mpsc::channel();
    let storage = storage.clone();
    thread::spawn(move || {
        let result = match client::request(&storage, Request::Servers) {
            Ok(Reply::Servers(servers)) => Ok(servers),
            Ok(_) => Err("Unexpected server list reply".into()),
            Err(error) => Err(format!("{error:#}")),
        };
        let _ = tx.send(result);
        crate::repaint::request();
    });
    rx
}

fn tool(name: &str) -> &str {
    match name {
        "github" => "GitHub",
        "codex" => "Codex",
        "claude" => "Claude Code",
        other => other,
    }
}

pub(super) fn status_color(status: disk::Status) -> ratatui::style::Color {
    match status {
        disk::Status::Ok => GREEN,
        disk::Status::Low => crate::ui::YELLOW,
        disk::Status::Critical => RED,
    }
}

/// `disk low` / `disk critical` for statuses worth a mark.
pub(super) fn indicator(status: disk::Status) -> Option<&'static str> {
    match status {
        disk::Status::Ok => None,
        disk::Status::Low => Some("disk low"),
        disk::Status::Critical => Some("disk critical"),
    }
}

/// A used/total bar with the numbers after it.
fn disk_line(report: &Report) -> Line<'static> {
    let fraction = if report.total == 0 {
        1.0
    } else {
        report.used as f64 / report.total as f64
    };
    let filled = ((fraction.clamp(0.0, 1.0) * BAR as f64).round() as usize).min(BAR);
    let color = status_color(report.status);
    Line::from(vec![
        Span::styled("Disk ", Style::default().fg(DIM)),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(BAR - filled), Style::default().fg(DIM)),
        Span::styled(
            format!(
                " {} of {} used · {} free ({}%) · ",
                human(report.used),
                human(report.total),
                human(report.free),
                report.space().free_percent()
            ),
            Style::default().fg(TEXT),
        ),
        Span::styled(
            report.status.label().to_owned(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn ago(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86400),
    }
}

fn row(label: String, bytes: u64, detail: String, width: usize) -> Line<'static> {
    let label: String = label
        .chars()
        .take(width.saturating_sub(12).max(8))
        .collect();
    let pad = 34usize.min(width.saturating_sub(12));
    Line::from(vec![
        Span::styled(format!("{label:<pad$}"), Style::default().fg(TEXT)),
        Span::styled(format!("{:>9}", human(bytes)), Style::default().fg(TEXT)),
        Span::styled(
            if detail.is_empty() {
                String::new()
            } else {
                format!("  {detail}")
            },
            Style::default().fg(DIM),
        ),
    ])
}

impl State {
    fn selected(&self) -> Option<&ServerStatus> {
        self.servers.get(self.selected)
    }
    /// Remembers low and critical servers from any server list.
    pub(super) fn remember(&mut self, servers: &[ServerStatus]) {
        self.disk = servers
            .iter()
            .filter_map(|status| {
                status
                    .disk_status()
                    .filter(|status| *status != disk::Status::Ok)
                    .map(|disk| (status.name.clone(), disk))
            })
            .collect();
    }
    /// The disk mark for a saved server, if any.
    pub(super) fn mark(&self, server: &str) -> Option<disk::Status> {
        self.disk.get(server).copied()
    }
}

impl Ui {
    /// Opens `/cloud`, on a server's details when `server` names one.
    pub(super) fn open_cloud(&mut self, server: Option<&str>) {
        let previous = std::mem::take(&mut self.cloud.servers);
        self.cloud.servers = crate::cloud::servers::load(&self.storage)
            .unwrap_or_default()
            .into_iter()
            .map(|saved| {
                previous
                    .iter()
                    .find(|status| status.name == saved.name)
                    .cloned()
                    .unwrap_or_else(|| ServerStatus {
                        name: saved.name,
                        hosts: saved.hosts,
                        port: saved.port,
                        problem: Some("Connecting…".into()),
                        ..ServerStatus::default()
                    })
            })
            .collect();
        let named = server
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .and_then(|name| self.cloud.servers.iter().position(|s| s.name == name));
        self.cloud.selected = named.unwrap_or(0);
        self.cloud.detail = named.is_some();
        self.cloud.action = 0;
        self.cloud.confirming = None;
        self.cloud.message = server
            .filter(|name| !name.trim().is_empty() && named.is_none())
            .map(|name| (format!("No saved server named {}", name.trim()), true));
        self.cloud.error = None;
        self.cloud.fetched = None;
        self.modal = Some(Modal::Cloud);
        self.tick_cloud(false);
    }

    /// `visible`: the terminal shows difu, so the sessions list may read
    /// server disk status for its marks.
    pub(super) fn tick_cloud(&mut self, visible: bool) {
        if let Some(result) = self
            .cloud
            .loading
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.cloud.loading = None;
            match result {
                Ok(servers) => {
                    self.cloud.remember(&servers);
                    self.cloud.servers = servers;
                    self.cloud.error = None;
                }
                Err(error) => self.cloud.error = Some(error),
            }
            self.cloud.selected = self
                .cloud
                .selected
                .min(self.cloud.servers.len().saturating_sub(1));
            if self.cloud.servers.is_empty() {
                self.cloud.detail = false;
            }
        }
        if let Some(result) = self
            .cloud
            .indicator
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.cloud.indicator = None;
            if let Ok(servers) = result {
                self.cloud.remember(&servers);
            }
        }
        let outcome = self
            .cloud
            .running
            .as_ref()
            .and_then(|(_, rx)| rx.try_recv().ok());
        if let Some(outcome) = outcome {
            self.cloud.running = None;
            self.cloud_outcome(outcome, true);
        }
        if let Some(outcome) = self
            .cloud
            .polling
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.cloud.polling = None;
            self.cloud_outcome(outcome, false);
        }
        if !matches!(self.modal, Some(Modal::Cloud)) {
            if visible {
                self.tick_disk_indicator();
            }
            return;
        }
        if !self.cloud.servers.is_empty()
            && self.cloud.loading.is_none()
            && self.cloud.fetched.is_none_or(|at| at.elapsed() >= REFRESH)
        {
            self.cloud.fetched = Some(Instant::now());
            self.cloud.loading = Some(fetch(&self.storage));
        }
        // A measurement in progress finishes on the server; read it promptly.
        let measuring = self
            .cloud
            .selected()
            .filter(|_| self.cloud.detail)
            .filter(|status| status.disk.as_ref().is_some_and(|disk| disk.measuring))
            .map(|status| status.name.clone());
        if let Some(name) = measuring
            && self.cloud.polling.is_none()
            && self.cloud.running.is_none()
            && self.cloud.polled.is_none_or(|at| at.elapsed() >= MEASURING)
        {
            self.cloud.polled = Some(Instant::now());
            self.cloud.polling = Some(self.cloud_request(
                name.clone(),
                Request::Disk {
                    server: Some(name),
                    refresh: false,
                },
            ));
        }
    }

    /// Reads server disk status now and then while server sessions are
    /// listed, so low servers are marked without opening `/cloud`.
    fn tick_disk_indicator(&mut self) {
        if self.cloud.indicator.is_some()
            || self
                .cloud
                .indicated
                .is_some_and(|at| at.elapsed() < INDICATOR)
            || !self
                .summaries
                .iter()
                .any(|summary| summary.server.is_some())
        {
            return;
        }
        self.cloud.indicated = Some(Instant::now());
        self.cloud.indicator = Some(fetch(&self.storage));
    }

    fn cloud_outcome(&mut self, outcome: Outcome, reported: bool) {
        crate::repaint::request();
        match outcome {
            Outcome::Disk(name, Ok(report)) => {
                if let Some(status) = self.cloud.servers.iter_mut().find(|s| s.name == name) {
                    status.disk = Some(report);
                    status.disk_problem = None;
                }
                let servers = self.cloud.servers.clone();
                self.cloud.remember(&servers);
            }
            Outcome::Disk(_, Err(error)) => {
                if reported {
                    self.cloud.message = Some((error, true));
                }
            }
            Outcome::Cleaned(name, Ok(cleaned)) => {
                self.cloud.message = Some((
                    format!(
                        "{name}: freed {}. {}",
                        human(cleaned.freed),
                        cleaned.summary
                    ),
                    false,
                ));
                self.cloud.fetched = None;
            }
            Outcome::Cleaned(name, Err(error)) => {
                self.cloud.message = Some((format!("{name}: {error}"), true));
                self.cloud.fetched = None;
            }
        }
    }

    fn cloud_request(&self, name: String, request: Request) -> mpsc::Receiver<Outcome> {
        let (tx, rx) = mpsc::channel();
        let storage = self.storage.clone();
        thread::spawn(move || {
            let outcome = match request {
                Request::DiskClean { .. } => Outcome::Cleaned(
                    name,
                    match client::request(&storage, request) {
                        Ok(Reply::DiskCleaned(cleaned)) => Ok(cleaned),
                        Ok(_) => Err("Unexpected cleanup reply".into()),
                        Err(error) => Err(format!("{error:#}")),
                    },
                ),
                request => Outcome::Disk(
                    name,
                    match client::request(&storage, request) {
                        Ok(Reply::Disk(report)) => Ok(report),
                        Ok(_) => Err("Unexpected disk status reply".into()),
                        Err(error) => Err(format!("{error:#}")),
                    },
                ),
            };
            let _ = tx.send(outcome);
            crate::repaint::request();
        });
        rx
    }

    /// Measures the selected server's disk again and rereads its logins and
    /// repositories.
    fn cloud_refresh(&mut self) {
        self.cloud.fetched = None;
        let Some(name) = self.cloud.selected().map(|status| status.name.clone()) else {
            return;
        };
        if self.cloud.running.is_some() {
            return;
        }
        self.cloud.message = None;
        let receiver = self.cloud_request(
            name.clone(),
            Request::Disk {
                server: Some(name.clone()),
                refresh: true,
            },
        );
        self.cloud.running = Some((format!("Refreshing {name}…"), receiver));
    }

    /// Opens the confirmation for a cleanup on the selected server.
    fn cloud_confirm(&mut self, action: Action) {
        let Some(status) = self.cloud.selected() else {
            return;
        };
        let problem = if self.cloud.running.is_some() {
            Some("Wait for the current action to finish".to_owned())
        } else if status.disk.is_none() {
            Some(
                status
                    .disk_problem
                    .clone()
                    .or_else(|| status.problem.clone())
                    .unwrap_or_else(|| "Disk status is not known yet".into()),
            )
        } else {
            None
        };
        self.cloud.detail = true;
        self.cloud.action = Action::ALL
            .iter()
            .position(|candidate| *candidate == action)
            .map_or(0, |index| index + 1);
        match problem {
            Some(problem) => self.cloud.message = Some((problem, true)),
            None => {
                self.cloud.message = None;
                self.cloud.confirming = Some(action);
            }
        }
    }

    fn cloud_clean(&mut self, action: Action) {
        let Some(name) = self.cloud.selected().map(|status| status.name.clone()) else {
            return;
        };
        let receiver = self.cloud_request(
            name.clone(),
            Request::DiskClean {
                server: Some(name.clone()),
                action,
            },
        );
        self.cloud.running = Some((format!("{} on {name}…", action.label()), receiver));
    }

    pub(super) fn cloud_key(&mut self, key: KeyEvent) {
        let shortcut = self.shortcut(key);
        if key.code == KeyCode::Esc {
            if self.cloud.confirming.take().is_none() {
                if self.cloud.detail {
                    self.cloud.detail = false;
                } else {
                    self.modal = None;
                }
            }
            return;
        }
        if let Some(action) = self.cloud.confirming {
            if shortcut == Some(NAVIGATION_ACTIVATE) {
                self.cloud.confirming = None;
                self.cloud_clean(action);
            }
            return;
        }
        let servers = self.cloud.servers.len();
        match shortcut {
            Some(NAVIGATION_UP) if self.cloud.detail => {
                self.cloud.action = self.cloud.action.saturating_sub(1)
            }
            Some(NAVIGATION_DOWN) if self.cloud.detail => {
                self.cloud.action = (self.cloud.action + 1).min(Action::ALL.len())
            }
            Some(NAVIGATION_UP) => self.cloud.selected = self.cloud.selected.saturating_sub(1),
            Some(NAVIGATION_DOWN) => {
                self.cloud.selected = (self.cloud.selected + 1).min(servers.saturating_sub(1))
            }
            Some(NAVIGATION_ACTIVATE) if self.cloud.detail => {
                match self
                    .cloud
                    .action
                    .checked_sub(1)
                    .and_then(|index| Action::ALL.get(index))
                {
                    Some(action) => self.cloud_confirm(*action),
                    None => self.cloud_refresh(),
                }
            }
            Some(NAVIGATION_ACTIVATE) if servers > 0 => {
                self.cloud.detail = true;
                self.cloud.action = 0;
                self.cloud.message = None;
            }
            Some(CLOUD_BACK) => self.cloud.detail = false,
            Some(CLOUD_REFRESH) => self.cloud_refresh(),
            Some(CLOUD_CLEAN_ARCHIVED) => self.cloud_confirm(Action::ArchivedWorktrees),
            Some(CLOUD_CLEAR_BUILDS) => self.cloud_confirm(Action::BuildOutputs),
            Some(CLOUD_PRUNE_SANDBOX) => self.cloud_confirm(Action::Sandbox),
            _ => {}
        }
    }

    fn cloud_hint(&self, id: crate::shortcuts::ActionId) -> String {
        Bindings::new(&self.shortcut_config).primary_hint_in(id, self.shortcut_context())
    }

    pub(super) fn draw_cloud(&mut self, frame: &mut Frame, area: Rect) {
        let footer = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
        let width = usize::from(body.width);
        let height = usize::from(body.height);
        let mut lines = if self.cloud.servers.is_empty() {
            self.cloud_empty()
        } else if self.cloud.detail {
            self.cloud_detail(width, height)
        } else {
            self.cloud_list(width, height)
        };
        if let Some(error) = &self.cloud.error {
            lines.push(Line::default());
            lines.push(Line::styled(error.clone(), Style::default().fg(RED)));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
        let hint = |id| self.cloud_hint(id);
        let text = if self.cloud.confirming.is_some() {
            format!("{} confirms · Esc cancels", hint(NAVIGATION_ACTIVATE))
        } else if self.cloud.servers.is_empty() {
            "Esc close".to_owned()
        } else if self.cloud.detail {
            format!(
                "{}/{} select · {} run · {} refresh · {}/{}/{} clean · {} back · Esc close",
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN),
                hint(NAVIGATION_ACTIVATE),
                hint(CLOUD_REFRESH),
                hint(CLOUD_CLEAN_ARCHIVED),
                hint(CLOUD_CLEAR_BUILDS),
                hint(CLOUD_PRUNE_SANDBOX),
                hint(CLOUD_BACK),
            )
        } else {
            format!(
                "{}/{} select · {} details · {} refresh · {}/{}/{} clean · Esc close",
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN),
                hint(NAVIGATION_ACTIVATE),
                hint(CLOUD_REFRESH),
                hint(CLOUD_CLEAN_ARCHIVED),
                hint(CLOUD_CLEAR_BUILDS),
                hint(CLOUD_PRUNE_SANDBOX),
            )
        };
        frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)), footer);
    }

    fn cloud_empty(&self) -> Vec<Line<'static>> {
        let bold = Style::default().fg(TEXT).add_modifier(Modifier::BOLD);
        let text = Style::default().fg(TEXT);
        let dim = Style::default().fg(DIM);
        let mut lines = vec![
            Line::styled("No cloud servers yet", bold),
            Line::default(),
            Line::styled(
                "Run difu on a Linux server you rent, so agents keep working while this computer sleeps. In a terminal:",
                text,
            ),
            Line::default(),
            Line::from(vec![
                Span::styled("  difu server add <ip>      ", Style::default().fg(ACCENT)),
                Span::styled(
                    "set up a new server with its root password or your SSH key",
                    dim,
                ),
            ]),
            Line::from(vec![
                Span::styled("  difu server join <host>   ", Style::default().fg(ACCENT)),
                Span::styled(
                    "pair with a server that is already set up, using its server password",
                    dim,
                ),
            ]),
        ];
        if disk::server_mode(&self.storage) {
            lines.push(Line::default());
            lines.push(Line::styled(
                "This computer is a difu server: `difu disk` shows its own disk space and cleanups.",
                dim,
            ));
        }
        lines
    }

    /// The header line of a server: name, connection, version and address.
    fn cloud_heading(&self, status: &ServerStatus, selected: bool) -> Line<'static> {
        let ours = env!("CARGO_PKG_VERSION");
        let address = format!(
            "{}{}",
            status
                .hosts
                .first()
                .cloned()
                .unwrap_or_else(|| "no address".into()),
            if status.port == 0 {
                String::new()
            } else {
                format!(":{}", status.port)
            }
        );
        let (dot, color) = if status.connected {
            ("● ", GREEN)
        } else {
            ("○ ", DIM)
        };
        let mut spans = vec![
            Span::styled(
                if selected { "› " } else { "  " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                status.name.clone(),
                Style::default()
                    .fg(if selected { ACCENT } else { TEXT })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {dot}"), Style::default().fg(color)),
            Span::styled(
                if status.connected {
                    "connected"
                } else {
                    "not connected"
                },
                Style::default().fg(color),
            ),
        ];
        if let Some(version) = &status.version {
            spans.push(Span::styled(
                format!(" · difu {version}"),
                Style::default().fg(if version == ours {
                    DIM
                } else {
                    crate::ui::YELLOW
                }),
            ));
        }
        spans.push(Span::styled(
            format!(" · {address}"),
            Style::default().fg(DIM),
        ));
        if let Some(mark) = status.disk_status().and_then(indicator) {
            spans.push(Span::styled(
                format!(" · {mark}"),
                Style::default().fg(status.disk_status().map_or(DIM, status_color)),
            ));
        }
        Line::from(spans)
    }

    /// Sessions, repositories and logins, or why they are unknown.
    fn cloud_summary(status: &ServerStatus) -> Line<'static> {
        let dim = Style::default().fg(DIM);
        let mut spans = vec![Span::raw("    ")];
        match status.sessions {
            Some(counts) => spans.push(Span::styled(
                format!(
                    "{} active / {} sessions ({} archived)",
                    counts.active, counts.total, counts.archived
                ),
                Style::default().fg(TEXT),
            )),
            None => spans.push(Span::styled("sessions unknown", dim)),
        }
        let cloned = status.repositories.iter().filter(|r| r.cloned).count();
        spans.push(Span::styled(
            format!(
                " · {} {}{}",
                status.repositories.len(),
                if status.repositories.len() == 1 {
                    "repository"
                } else {
                    "repositories"
                },
                if cloned < status.repositories.len() {
                    format!(" ({cloned} cloned)")
                } else {
                    String::new()
                }
            ),
            Style::default().fg(TEXT),
        ));
        if status.logins.is_empty() {
            spans.push(Span::styled(" · logins unknown", dim));
        }
        for login in &status.logins {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(
                format!(
                    "{} {}{}",
                    tool(&login.tool),
                    if login.logged_in { "✓" } else { "✗" },
                    login
                        .account
                        .as_ref()
                        .map(|account| format!(" {account}"))
                        .unwrap_or_default()
                ),
                Style::default().fg(if login.logged_in { GREEN } else { RED }),
            ));
        }
        Line::from(spans)
    }

    fn cloud_disk(status: &ServerStatus) -> Line<'static> {
        match (&status.disk, &status.disk_problem) {
            (Some(report), _) => {
                let mut line = disk_line(report);
                line.spans.insert(0, Span::raw("    "));
                line
            }
            (None, Some(problem)) => Line::styled(
                format!("    Disk: {problem}"),
                Style::default().fg(crate::ui::YELLOW),
            ),
            (None, None) => Line::styled(
                if status.connected {
                    "    Disk: reading…"
                } else {
                    "    Disk: unknown while not connected"
                },
                Style::default().fg(DIM),
            ),
        }
    }

    fn cloud_list(&self, _width: usize, height: usize) -> Vec<Line<'static>> {
        const ROWS: usize = 5;
        let visible = (height.saturating_sub(2) / ROWS).max(1);
        let start = self.cloud.selected.saturating_sub(visible - 1);
        let mut lines = Vec::new();
        for (index, status) in self
            .cloud
            .servers
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
        {
            lines.push(self.cloud_heading(status, index == self.cloud.selected));
            if let Some(problem) = &status.problem {
                lines.push(Line::styled(
                    format!("    {problem}"),
                    Style::default().fg(if status.connected {
                        crate::ui::YELLOW
                    } else {
                        RED
                    }),
                ));
            }
            lines.push(Self::cloud_summary(status));
            lines.push(Self::cloud_disk(status));
            lines.push(Line::default());
        }
        self.cloud_status_lines(&mut lines);
        lines
    }

    fn cloud_status_lines(&self, lines: &mut Vec<Line<'static>>) {
        if let Some((text, _)) = &self.cloud.running {
            lines.push(Line::styled(text.clone(), Style::default().fg(ACCENT)));
        } else if let Some((text, error)) = &self.cloud.message {
            lines.push(Line::styled(
                text.clone(),
                Style::default().fg(if *error { RED } else { GREEN }),
            ));
        }
    }

    fn cloud_detail(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let Some(status) = self.cloud.selected() else {
            return Vec::new();
        };
        let dim = Style::default().fg(DIM);
        let bold = Style::default().fg(TEXT).add_modifier(Modifier::BOLD);
        let mut lines = vec![self.cloud_heading(status, true)];
        if let Some(problem) = &status.problem {
            lines.push(Line::styled(
                format!("    {problem}"),
                Style::default().fg(if status.connected {
                    crate::ui::YELLOW
                } else {
                    RED
                }),
            ));
        }
        lines.push(Self::cloud_summary(status));
        lines.push(Self::cloud_disk(status));
        let Some(report) = &status.disk else {
            lines.push(Line::default());
            self.cloud_status_lines(&mut lines);
            return lines;
        };
        if report.status == disk::Status::Critical {
            lines.push(Line::styled(
                "    New sessions, moves and clones are paused until space is freed; running sessions continue.",
                Style::default().fg(RED),
            ));
        }
        lines.push(Line::styled(
            match (report.measured_at, report.measuring) {
                (None, _) => "    Measuring what uses the space…".to_owned(),
                (Some(at), measuring) => format!(
                    "    Measured {}{}",
                    ago(chrono::Utc::now().timestamp_millis() - at),
                    if measuring {
                        " · measuring again…"
                    } else {
                        ""
                    }
                ),
            },
            dim,
        ));
        lines.push(Line::default());
        if let Some(action) = self.cloud.confirming {
            lines.extend(Self::cloud_confirmation(status, report, action, width));
            return lines;
        }
        let breakdown = &report.breakdown;
        // Actions and the fixed rows below the sessions.
        let reserved = lines.len() + 8 + 2 + Action::ALL.len() + 3;
        let shown = height
            .saturating_sub(reserved)
            .min(breakdown.sessions.len());
        if report.measured_at.is_some() {
            let sessions: u64 = breakdown.sessions.iter().map(|s| s.bytes).sum();
            lines.push(row(
                format!("Session worktrees ({})", breakdown.sessions.len()),
                sessions,
                String::new(),
                width,
            ));
            for session in breakdown.sessions.iter().take(shown) {
                let mut label = format!("  {}", session.title);
                if session.active {
                    label.push_str(" · active");
                } else if session.archived {
                    label.push_str(" · archived");
                }
                lines.push(row(
                    label,
                    session.bytes,
                    if session.build_outputs > 0 {
                        format!("build outputs {}", human(session.build_outputs))
                    } else {
                        String::new()
                    },
                    width,
                ));
            }
            if breakdown.sessions.len() > shown {
                lines.push(Line::styled(
                    format!("  … and {} more", breakdown.sessions.len() - shown),
                    dim,
                ));
            }
            lines.push(row(
                "Repositories".into(),
                breakdown.repositories,
                String::new(),
                width,
            ));
            lines.push(row(
                "Attachments".into(),
                breakdown.attachments,
                String::new(),
                width,
            ));
            lines.push(row("Caches".into(), breakdown.caches, String::new(), width));
            let sandbox = &breakdown.sandbox;
            lines.push(row(
                "Sandbox storage".into(),
                sandbox.total(),
                if sandbox.available {
                    format!(
                        "images {} · containers {} · volumes {}",
                        human(sandbox.images),
                        human(sandbox.containers),
                        human(sandbox.volumes)
                    )
                } else {
                    "no Podman".into()
                },
                width,
            ));
            lines.push(row("Other".into(), breakdown.other, String::new(), width));
            lines.push(Line::default());
        }
        lines.push(Line::styled("Actions", bold));
        let selected = |index: usize| index == self.cloud.action;
        lines.push(Self::action_line(
            "Refresh",
            "measure again and reread logins and repositories".into(),
            selected(0),
        ));
        for (index, action) in Action::ALL.into_iter().enumerate() {
            lines.push(Self::action_line(
                action.label(),
                Self::expectation(report, action),
                selected(index + 1),
            ));
        }
        lines.push(Line::default());
        self.cloud_status_lines(&mut lines);
        lines
    }

    fn action_line(label: &str, detail: String, selected: bool) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                format!("{label:<28}"),
                Style::default().fg(if selected { ACCENT } else { TEXT }),
            ),
            Span::styled(detail, Style::default().fg(DIM)),
        ])
    }

    /// What an action is expected to free, from the last measurement.
    fn expectation(report: &Report, action: Action) -> String {
        if report.measured_at.is_none() {
            return "waiting for the first measurement".into();
        }
        let expected = report.expected(action);
        match action {
            Action::ArchivedWorktrees => {
                let count = report.archived().count();
                if count == 0 {
                    "no archived worktrees".into()
                } else {
                    format!(
                        "about {} in {count} archived {}",
                        human(expected),
                        if count == 1 { "session" } else { "sessions" }
                    )
                }
            }
            Action::BuildOutputs => {
                let count = report.build_outputs().count();
                if count == 0 {
                    "no build outputs in idle sessions".into()
                } else {
                    format!(
                        "about {} in {count} idle {}",
                        human(expected),
                        if count == 1 { "session" } else { "sessions" }
                    )
                }
            }
            Action::Sandbox if !report.breakdown.sandbox.available => "no Podman".into(),
            Action::Sandbox => format!("up to {} reclaimable", human(expected)),
        }
    }

    fn cloud_confirmation(
        status: &ServerStatus,
        report: &Report,
        action: Action,
        width: usize,
    ) -> Vec<Line<'static>> {
        let bold = Style::default().fg(TEXT).add_modifier(Modifier::BOLD);
        let text = Style::default().fg(TEXT);
        let mut lines = vec![
            Line::styled(format!("{} on {}?", action.label(), status.name), bold),
            Line::default(),
            Line::styled(action.describe(), text),
            Line::default(),
        ];
        let sessions: Vec<(String, u64)> = match action {
            Action::ArchivedWorktrees => report
                .archived()
                .map(|s| (s.title.clone(), s.bytes))
                .collect(),
            Action::BuildOutputs => report
                .build_outputs()
                .map(|s| (s.title.clone(), s.build_outputs))
                .collect(),
            Action::Sandbox => Vec::new(),
        };
        const SHOWN: usize = 8;
        for (title, bytes) in sessions.iter().take(SHOWN) {
            lines.push(row(format!("  {title}"), *bytes, String::new(), width));
        }
        if sessions.len() > SHOWN {
            lines.push(Line::styled(
                format!("  … and {} more", sessions.len() - SHOWN),
                Style::default().fg(DIM),
            ));
        }
        if !sessions.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            format!("Expected: {}", Self::expectation(report, action)),
            Style::default().fg(ACCENT),
        ));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::super::shortcut_tests::{
        BINDING_PHASES, binding_fixture, fixture, request_at_boundary_with,
    };
    use super::*;
    use crate::cloud::federation::{Login, Repository, SessionCounts};
    use anyhow::{Context, Result};

    fn report(status: disk::Status) -> Report {
        Report {
            path: "/srv/difu/agents".into(),
            total: 80_000_000_000,
            used: 52_000_000_000,
            free: 28_000_000_000,
            status,
            measured_at: Some(chrono::Utc::now().timestamp_millis()),
            measuring: false,
            breakdown: disk::Breakdown {
                sessions: vec![
                    disk::SessionUsage {
                        id: "1".into(),
                        title: "Fix login".into(),
                        archived: true,
                        active: false,
                        bytes: 4_100_000_000,
                        build_outputs: 3_200_000_000,
                    },
                    disk::SessionUsage {
                        id: "2".into(),
                        title: "Widget work".into(),
                        archived: false,
                        active: true,
                        bytes: 1_000_000_000,
                        build_outputs: 500_000_000,
                    },
                ],
                repositories: 9_000_000_000,
                attachments: 12_000_000,
                caches: 340_000_000,
                sandbox: disk::SandboxUsage {
                    available: true,
                    images: 3_000_000_000,
                    containers: 120_000_000,
                    volumes: 0,
                    reclaimable: 900_000_000,
                },
                other: 30_000_000_000,
            },
        }
    }

    fn servers() -> Vec<ServerStatus> {
        vec![
            ServerStatus {
                name: "hetzner".into(),
                hosts: vec!["203.0.113.7".into()],
                port: 3438,
                connected: true,
                version: Some(env!("CARGO_PKG_VERSION").into()),
                repositories: vec![Repository {
                    name: "acme/api".into(),
                    path: "/srv/difu/repos/acme/api".into(),
                    cloned: true,
                }],
                sessions: Some(SessionCounts {
                    active: 2,
                    archived: 5,
                    total: 14,
                }),
                logins: vec![
                    Login {
                        tool: "github".into(),
                        logged_in: true,
                        account: Some("octocat".into()),
                        detail: String::new(),
                    },
                    Login {
                        tool: "codex".into(),
                        logged_in: false,
                        account: None,
                        detail: String::new(),
                    },
                ],
                disk: Some(report(disk::Status::Low)),
                ..ServerStatus::default()
            },
            ServerStatus {
                name: "old".into(),
                hosts: vec!["198.51.100.2".into()],
                port: 3438,
                connected: true,
                version: Some("0.1.0".into()),
                problem: Some("old runs difu 0.1.0 and this computer runs difu 0.37.0. Run `difu server upgrade old` to use it from here.".into()),
                disk_problem: Some("old runs an older difu without disk status; run `difu server upgrade old`".into()),
                ..ServerStatus::default()
            },
            ServerStatus {
                name: "home".into(),
                hosts: vec!["10.0.0.2".into()],
                problem: Some("Not connected: connection refused".into()),
                ..ServerStatus::default()
            },
        ]
    }

    fn ready(ui: &mut Ui) {
        ui.modal = Some(Modal::Cloud);
        ui.cloud.servers = servers();
        // Fresh, so the panel does not poll during the test.
        ui.cloud.fetched = Some(Instant::now());
    }

    fn screen(ui: &mut Ui, width: u16, height: u16) -> Result<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| ui.draw(frame))?;
        Ok(terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn settle(ui: &mut Ui) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while ui.cloud.running.is_some() {
            anyhow::ensure!(Instant::now() < deadline, "the request never finished");
            ui.tick_cloud(false);
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    #[test]
    fn the_palette_and_help_offer_cloud() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        assert!(
            ui.palette_commands()
                .iter()
                .any(|(name, _, _)| name == "/cloud")
        );
        ui.modal = Some(Modal::Help(crate::help::State {
            query: Editor::from("cloud"),
            ..Default::default()
        }));
        let text = screen(&mut ui, 140, 40)?;
        assert!(
            text.contains("/cloud") && text.contains("disk space and cleanups"),
            "{text}"
        );
        Ok(())
    }

    #[test]
    fn without_servers_the_panel_explains_how_to_add_one() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        ui.open_cloud(None);
        assert!(matches!(ui.modal, Some(Modal::Cloud)));
        assert!(ui.cloud.loading.is_none(), "nothing to ask for");
        let text = screen(&mut ui, 120, 40)?;
        assert!(text.contains("No cloud servers yet"), "{text}");
        assert!(text.contains("difu server add <ip>"), "{text}");
        assert!(text.contains("difu server join <host>"), "{text}");
        Ok(())
    }

    #[test]
    fn servers_show_connection_version_sessions_logins_repositories_and_disk() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        ready(&mut ui);
        let text = screen(&mut ui, 140, 44)?;
        assert!(text.contains("› hetzner  ● connected"), "{text}");
        assert!(text.contains("203.0.113.7:3438 · disk low"), "{text}");
        assert!(
            text.contains("2 active / 14 sessions (5 archived) · 1 repository"),
            "{text}"
        );
        assert!(
            text.contains("GitHub ✓ octocat") && text.contains("Codex ✗"),
            "{text}"
        );
        assert!(
            text.contains("52.0 GB of 80.0 GB used · 28.0 GB free (35%) · low"),
            "{text}"
        );
        assert!(text.contains("difu 0.1.0"), "{text}");
        assert!(text.contains("Run `difu server upgrade old`"), "{text}");
        assert!(text.contains("○ not connected"), "{text}");
        assert!(text.contains("Not connected: connection refused"), "{text}");
        assert!(
            text.contains("details") && text.contains("Esc close"),
            "{text}"
        );
        Ok(())
    }

    #[test]
    fn navigation_opens_details_with_the_biggest_sessions_first() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        ready(&mut ui);
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.cloud.selected, 1);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ui.cloud.detail);
        let text = screen(&mut ui, 140, 44)?;
        let first = text.find("Fix login · archived").context("first session")?;
        let second = text
            .find("Widget work · active")
            .context("second session")?;
        assert!(first < second, "{text}");
        assert!(text.contains("build outputs 3.2 GB"), "{text}");
        assert!(
            text.contains("Repositories") && text.contains("9.0 GB"),
            "{text}"
        );
        assert!(text.contains("images 3.0 GB · containers 120 MB"), "{text}");
        assert!(
            text.contains("about 4.1 GB in 1 archived session"),
            "{text}"
        );
        assert!(text.contains("about 3.2 GB in 1 idle session"), "{text}");
        assert!(text.contains("up to 900 MB reclaimable"), "{text}");
        ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(!ui.cloud.detail);
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        Ok(())
    }

    #[test]
    fn cleanups_confirm_what_they_remove_before_asking_the_server() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        ready(&mut ui);
        ui.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(ui.cloud.confirming, Some(Action::ArchivedWorktrees));
        assert!(ui.cloud.running.is_none(), "nothing runs before confirming");
        let text = screen(&mut ui, 140, 44)?;
        assert!(
            text.contains("Clean archived worktrees on hetzner?"),
            "{text}"
        );
        assert!(
            text.contains("Their chats, branches and commits stay"),
            "{text}"
        );
        assert!(
            text.contains("Fix login") && !text.contains("Widget work"),
            "{text}"
        );
        assert!(text.contains("Expected: about 4.1 GB"), "{text}");
        assert!(text.contains("confirms · Esc cancels"), "{text}");
        // Esc cancels without a request.
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.cloud.confirming.is_none() && ui.cloud.running.is_none());
        assert!(matches!(ui.modal, Some(Modal::Cloud)));
        ui.key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        let request = request_at_boundary_with(&mut ui, |ui| {
            ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        })?;
        assert!(matches!(
            request,
            Request::DiskClean {
                server: Some(ref server),
                action: Action::BuildOutputs
            } if server == "hetzner"
        ));
        // The fixture answers `Ok`, which is not a cleanup result.
        settle(&mut ui)?;
        assert!(
            ui.cloud
                .message
                .as_ref()
                .is_some_and(|(text, error)| *error && text.contains("Unexpected cleanup reply")),
            "{:?}",
            ui.cloud.message
        );
        // A server without disk status explains why instead of confirming.
        ui.cloud.selected = 1;
        ui.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(ui.cloud.confirming.is_none());
        assert!(
            ui.cloud
                .message
                .as_ref()
                .is_some_and(|(text, _)| text.contains("difu server upgrade old"))
        );
        Ok(())
    }

    #[test]
    fn results_and_refreshes_update_the_panel() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        ready(&mut ui);
        ui.cloud_outcome(
            Outcome::Cleaned(
                "hetzner".into(),
                Ok(disk::Cleaned {
                    freed: 4_000_000_000,
                    summary: "Removed 1 archived worktree.".into(),
                }),
            ),
            true,
        );
        let text = screen(&mut ui, 140, 44)?;
        assert!(
            text.contains("hetzner: freed 4.0 GB. Removed 1 archived worktree."),
            "{text}"
        );
        assert!(ui.cloud.fetched.is_none(), "the list is read again");
        let mut critical = report(disk::Status::Critical);
        critical.free = 2_000_000_000;
        ui.cloud_outcome(Outcome::Disk("hetzner".into(), Ok(critical)), true);
        assert_eq!(ui.cloud.mark("hetzner"), Some(disk::Status::Critical));
        ui.cloud.detail = true;
        let text = screen(&mut ui, 140, 44)?;
        assert!(
            text.contains("New sessions, moves and clones are paused"),
            "{text}"
        );
        let request = request_at_boundary_with(&mut ui, |ui| {
            ui.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
        })?;
        assert!(matches!(
            request,
            Request::Disk {
                server: Some(ref server),
                refresh: true
            } if server == "hetzner"
        ));
        Ok(())
    }

    #[test]
    fn cloud_hints_follow_remapped_shortcuts() -> Result<()> {
        for action in [
            CLOUD_REFRESH,
            CLOUD_CLEAN_ARCHIVED,
            CLOUD_CLEAR_BUILDS,
            CLOUD_PRUNE_SANDBOX,
            CLOUD_BACK,
        ] {
            let (mut ui, _dir, _) = fixture(Some(action))?;
            ready(&mut ui);
            ui.cloud.detail = true;
            let text = screen(&mut ui, 160, 44)?;
            assert!(text.contains("F12"), "{action:?}: {text}");
        }
        Ok(())
    }

    #[test]
    fn cloud_actions_remap_and_leave_old_keys_inert() -> Result<()> {
        for (phase, custom) in BINDING_PHASES {
            for (action, old) in [
                (CLOUD_CLEAN_ARCHIVED, KeyCode::Char('a')),
                (CLOUD_CLEAR_BUILDS, KeyCode::Char('b')),
                (CLOUD_PRUNE_SANDBOX, KeyCode::Char('p')),
                (CLOUD_BACK, KeyCode::Left),
                (CLOUD_BACK, KeyCode::Backspace),
                (CLOUD_REFRESH, KeyCode::Char('r')),
            ] {
                let (mut ui, _dir, _) = binding_fixture(action, phase)?;
                ready(&mut ui);
                ui.cloud.detail = true;
                if custom {
                    ui.key(KeyEvent::new(old, KeyModifiers::NONE));
                    assert!(ui.cloud.confirming.is_none(), "{phase} {action:?}");
                    assert!(ui.cloud.running.is_none(), "{phase} {action:?}");
                    assert!(ui.cloud.detail, "{phase} {action:?}");
                }
                let key = KeyEvent::new(
                    if custom { KeyCode::F(12) } else { old },
                    KeyModifiers::NONE,
                );
                if action == CLOUD_REFRESH {
                    let request = request_at_boundary_with(&mut ui, |ui| ui.key(key))?;
                    assert!(
                        matches!(request, Request::Disk { refresh: true, .. }),
                        "{phase}"
                    );
                    settle(&mut ui)?;
                    continue;
                }
                ui.key(key);
                match action {
                    CLOUD_BACK => assert!(!ui.cloud.detail, "{phase}"),
                    CLOUD_CLEAN_ARCHIVED => {
                        assert_eq!(ui.cloud.confirming, Some(Action::ArchivedWorktrees))
                    }
                    CLOUD_CLEAR_BUILDS => {
                        assert_eq!(ui.cloud.confirming, Some(Action::BuildOutputs))
                    }
                    _ => assert_eq!(ui.cloud.confirming, Some(Action::Sandbox)),
                }
            }
        }
        Ok(())
    }

    #[test]
    fn opening_with_a_name_shows_that_server() -> Result<()> {
        let (mut ui, _dir, _) = fixture(None)?;
        for name in ["hetzner", "home"] {
            crate::cloud::servers::save(
                &ui.storage,
                crate::cloud::servers::tests::server(name, &format!("id-{name}")),
            )?;
        }
        ui.open_cloud(Some("home"));
        assert!(ui.cloud.detail);
        assert_eq!(
            ui.cloud.selected().map(|status| status.name.as_str()),
            Some("home")
        );
        ui.cloud.loading = None;
        ui.open_cloud(Some("nowhere"));
        assert!(!ui.cloud.detail);
        assert!(
            ui.cloud
                .message
                .as_ref()
                .is_some_and(|(text, _)| text.contains("nowhere"))
        );
        ui.cloud.loading = None;
        Ok(())
    }
}
