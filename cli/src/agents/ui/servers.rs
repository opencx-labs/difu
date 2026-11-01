//! Cloud servers in the session UI: the new-session picker (This Mac or a saved
//! server, then that server's repositories) and the notice for features that
//! need a session's files on this computer.
use super::*;
use crate::cloud::{
    federation::{self, Repository, ServerStatus},
    servers::SavedServer,
};
use crate::shortcuts::ids::*;
use ratatui::style::Modifier;
use std::path::PathBuf;

pub(super) const UNAVAILABLE: &str = "Not available for server sessions";
/// The picker asks the service for server state this often while open.
const REFRESH: Duration = Duration::from_secs(2);

/// Server sessions have `<server>:<session>` ids; local ids never contain `:`.
pub(super) fn is_server_session(id: &str) -> bool {
    federation::split(id).is_some()
}

/// The saved server running a session, from its id.
pub(super) fn server_name(id: &str) -> Option<&str> {
    federation::split(id).map(|(server, _)| server)
}

type Fetched = std::result::Result<Vec<ServerStatus>, String>;

pub struct Picker {
    servers: Vec<ServerStatus>,
    /// The server whose repositories are listed; `None` lists the machines.
    server: Option<String>,
    pub query: Editor,
    selected: usize,
    loading: Option<mpsc::Receiver<Fetched>>,
    fetched: Option<Instant>,
    error: Option<String>,
}

impl Picker {
    fn new(saved: Vec<SavedServer>) -> Self {
        Self {
            servers: saved
                .into_iter()
                .map(|server| ServerStatus {
                    name: server.name,
                    hosts: server.hosts,
                    problem: Some("Connecting…".into()),
                    ..ServerStatus::default()
                })
                .collect(),
            server: None,
            query: Editor::default(),
            selected: 0,
            loading: None,
            fetched: None,
            error: None,
        }
    }
    pub(super) fn choosing_repository(&self) -> bool {
        self.server.is_some()
    }
    fn status(&self) -> Option<&ServerStatus> {
        let server = self.server.as_ref()?;
        self.servers.iter().find(|status| &status.name == server)
    }
    fn repositories(&self) -> Vec<&Repository> {
        let query = self.query.text().to_lowercase();
        self.status()
            .map(|status| {
                status
                    .repositories
                    .iter()
                    .filter(|repository| repository.name.to_lowercase().contains(&query))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn count(&self) -> usize {
        if self.server.is_some() {
            self.repositories().len()
        } else {
            self.servers.len() + 1
        }
    }
}

fn server_detail(status: &ServerStatus) -> String {
    if let Some(problem) = &status.problem {
        return problem.clone();
    }
    let cloned = status
        .repositories
        .iter()
        .filter(|repository| repository.cloned)
        .count();
    format!(
        "{} · {cloned} {}",
        status.hosts.first().map(String::as_str).unwrap_or("server"),
        if cloned == 1 {
            "repository"
        } else {
            "repositories"
        }
    )
}

impl Ui {
    /// Refuses a feature that reads or opens the session's files on this
    /// computer; true when the session is on a server.
    pub(super) fn unavailable_for_server(&mut self, id: &str) -> bool {
        let server = is_server_session(id);
        if server {
            self.notice = Some((UNAVAILABLE.into(), true));
        }
        server
    }
    pub(super) fn open_new_session(&mut self, saved: Vec<SavedServer>) {
        self.modal = Some(Modal::NewSession(Box::new(Picker::new(saved))));
        self.tick_new_session();
    }
    /// Polls the service for server state and repositories while the picker
    /// is open.
    pub(super) fn tick_new_session(&mut self) {
        let Some(Modal::NewSession(picker)) = &mut self.modal else {
            return;
        };
        if let Some(result) = picker.loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
            picker.loading = None;
            crate::repaint::request();
            match result {
                Ok(servers) => picker.servers = servers,
                Err(error) => {
                    for status in &mut picker.servers {
                        status.problem = Some(error.clone());
                    }
                }
            }
            picker.selected = picker.selected.min(picker.count().saturating_sub(1));
        }
        if picker.loading.is_none() && picker.fetched.is_none_or(|at| at.elapsed() >= REFRESH) {
            picker.fetched = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            picker.loading = Some(rx);
            let storage = self.storage.clone();
            thread::spawn(move || {
                let result = match client::request(&storage, Request::Servers) {
                    Ok(Reply::Servers(servers)) => Ok(servers),
                    Ok(_) => Err("Unexpected server list reply".into()),
                    Err(error) => Err(format!("{error:#}")),
                };
                let _ = tx.send(result);
            });
        }
    }
    pub(super) fn new_session_key(&mut self, key: KeyEvent) {
        let shortcut = self.shortcut(key);
        let Some(Modal::NewSession(picker)) = &mut self.modal else {
            return;
        };
        let count = picker.count();
        match shortcut {
            _ if key.code == KeyCode::Esc => {
                if picker.server.take().is_some() {
                    picker.query = Editor::default();
                    picker.selected = 0;
                    picker.error = None;
                } else {
                    self.modal = None;
                }
            }
            Some(NAVIGATION_UP) => picker.selected = picker.selected.saturating_sub(1),
            Some(NAVIGATION_DOWN) => {
                picker.selected = (picker.selected + 1).min(count.saturating_sub(1))
            }
            Some(NAVIGATION_ACTIVATE) => {
                let index = picker.selected;
                self.new_session_choice(index);
            }
            _ if picker.server.is_some() => {
                if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    picker.query = Editor::default();
                } else {
                    picker.query.key(key);
                }
                picker.selected = 0;
            }
            _ => {}
        }
    }
    pub(super) fn new_session_paste(&mut self, text: &str) {
        if let Some(Modal::NewSession(picker)) = &mut self.modal
            && picker.server.is_some()
        {
            picker.query.insert(text);
            picker.selected = 0;
        }
    }
    pub(super) fn new_session_choice(&mut self, index: usize) {
        let Some(Modal::NewSession(picker)) = &mut self.modal else {
            return;
        };
        picker.selected = index.min(picker.count().saturating_sub(1));
        let Some(server) = picker.server.clone() else {
            let Some(machine) = index.checked_sub(1) else {
                self.modal = None;
                self.launch_in(None);
                return;
            };
            let Some(status) = picker.servers.get(machine) else {
                return;
            };
            if let Some(problem) = &status.problem {
                picker.error = Some(format!("{}: {problem}", status.name));
                return;
            }
            picker.server = Some(status.name.clone());
            picker.query = Editor::default();
            picker.selected = 0;
            picker.error = None;
            return;
        };
        let Some(repository) = picker.repositories().get(index).map(|r| (*r).clone()) else {
            return;
        };
        if !repository.cloned {
            picker.error = Some(format!(
                "{} is still being cloned on {server}",
                repository.name
            ));
            return;
        }
        self.launch_on(server, repository.path);
    }
    /// A new session on a saved server, in one of that server's repositories.
    fn launch_on(&mut self, server: String, repository: PathBuf) {
        if self.busy {
            return;
        }
        let mut defaults = self.defaults.clone();
        defaults.repository = Some(repository);
        self.busy = true;
        self.task(
            Task::Launch,
            Request::NewAgent {
                defaults,
                cwd: std::env::current_dir().unwrap_or_default(),
                remember_repository: false,
                server: Some(server),
            },
            false,
        );
    }
    pub(super) fn draw_new_session(&mut self, frame: &mut Frame, area: Rect) {
        let activate = crate::shortcuts::Bindings::new(&self.shortcut_config)
            .primary_hint_in(NAVIGATION_ACTIVATE, self.shortcut_context());
        let local = self
            .defaults
            .repository
            .as_ref()
            .map(|path| format!("default repository {}", path.display()))
            .unwrap_or_else(|| "this directory's repository".into());
        let Some(Modal::NewSession(picker)) = &self.modal else {
            return;
        };
        let mut rows: Vec<(String, String, bool)> = Vec::new();
        let top = if let Some(server) = &picker.server {
            editor(
                frame,
                Rect::new(area.x, area.y, area.width, 3.min(area.height)),
                &format!("Filter {server} repositories · {activate} starts · Esc back"),
                &picker.query,
                true,
            );
            for repository in picker.repositories() {
                rows.push((
                    repository.name.clone(),
                    if repository.cloned {
                        String::new()
                    } else {
                        "cloning…".into()
                    },
                    repository.cloned,
                ));
            }
            area.y.saturating_add(3)
        } else {
            frame.render_widget(
                Paragraph::new(format!(
                    "Where should the new session run? · {activate} chooses"
                ))
                .style(Style::default().fg(DIM)),
                Rect::new(area.x, area.y, area.width, 1.min(area.height)),
            );
            rows.push(("This Mac".into(), local, true));
            for status in &picker.servers {
                rows.push((
                    status.name.clone(),
                    server_detail(status),
                    status.problem.is_none(),
                ));
            }
            area.y.saturating_add(2)
        };
        let label_width = rows
            .iter()
            .map(|(label, ..)| label.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(8, 40);
        let footer = area.bottom().saturating_sub(1);
        let visible = usize::from(footer.saturating_sub(top));
        let selected = picker.selected.min(rows.len().saturating_sub(1));
        let first = selected.saturating_sub(visible.saturating_sub(1));
        let mut hits = Vec::new();
        for (offset, (index, (label, detail, enabled))) in rows
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .enumerate()
        {
            let rect = Rect::new(area.x, top + offset as u16, area.width, 1).intersection(area);
            let active = index == selected;
            let style = crate::ui::option_style(active);
            let detail_style = if active {
                style
            } else if *enabled {
                Style::default().fg(DIM)
            } else {
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{label:label_width$}  "), style),
                    Span::styled(detail.clone(), detail_style),
                ]))
                .style(style),
                rect,
            );
            hits.push((rect, Action::ServerChoice(index)));
        }
        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new(if picker.status().is_some_and(|s| s.repositories.is_empty()) {
                    "No repositories on this server yet. Add one there with `difu repos add owner/name`."
                } else {
                    "No matching repositories"
                })
                .style(Style::default().fg(DIM))
                .wrap(Wrap { trim: false }),
                Rect::new(area.x, top, area.width, 2).intersection(area),
            );
        }
        if let Some(error) = &picker.error {
            frame.render_widget(
                Paragraph::new(crate::model::clean(error)).style(Style::default().fg(RED)),
                Rect::new(area.x, footer, area.width, 1).intersection(area),
            );
        }
        self.hits.extend(hits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::servers::{self, tests::server};
    use anyhow::{Context, Result};

    fn status(name: &str, problem: Option<&str>) -> ServerStatus {
        ServerStatus {
            name: name.into(),
            hosts: vec!["203.0.113.7".into()],
            connected: problem.is_none(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            problem: problem.map(Into::into),
            repositories: vec![
                Repository {
                    name: "acme/api".into(),
                    path: "/srv/difu/repos/acme/api".into(),
                    cloned: true,
                },
                Repository {
                    name: "acme/web".into(),
                    path: "/srv/difu/repos/acme/web".into(),
                    cloned: false,
                },
            ],
        }
    }

    fn picker(ui: &mut Ui, servers: Vec<ServerStatus>) {
        let mut picker = Picker::new(Vec::new());
        picker.servers = servers;
        // Fresh enough that the test never contacts a service.
        picker.fetched = Some(Instant::now());
        ui.modal = Some(Modal::NewSession(Box::new(picker)));
    }

    fn screen(ui: &mut Ui) -> Result<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40))?;
        terminal.draw(|frame| ui.draw(frame))?;
        Ok(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect())
    }

    #[test]
    fn server_ids_are_namespaced_and_local_ids_are_not() -> Result<()> {
        assert!(is_server_session("box:1790000000000-ab12cd34"));
        assert_eq!(server_name("box:1"), Some("box"));
        assert!(!is_server_session("1790000000000-ab12cd34"));
        assert!(!is_server_session("orchestrator"));
        assert_eq!(server_name("1"), None);
        Ok(())
    }

    #[test]
    fn new_session_offers_servers_only_when_some_are_saved() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        ui.focus = Focus::List;
        let request =
            shortcut_tests::request_at_connected_boundary_with(&mut ui, |ui, _| ui.launch())?;
        assert!(matches!(request, Request::NewAgent { server: None, .. }));
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        servers::save(&ui.storage, server("hetzner", "a"))?;
        ui.launch();
        let Some(Modal::NewSession(picker)) = &ui.modal else {
            anyhow::bail!("server picker");
        };
        assert_eq!(
            picker
                .servers
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["hetzner"]
        );
        assert!(!ui.busy, "nothing launches before a choice");
        Ok(())
    }

    #[test]
    fn picker_lists_this_mac_and_servers_then_server_repositories() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        picker(
            &mut ui,
            vec![
                status("hetzner", None),
                status(
                    "old",
                    Some("old runs difu 0.0.1; run `difu server upgrade old`"),
                ),
            ],
        );
        let text = screen(&mut ui)?;
        assert!(text.contains("New session"), "{text}");
        assert!(text.contains("This Mac"), "{text}");
        assert!(text.contains("hetzner"), "{text}");
        assert!(text.contains("1 repository"), "{text}");
        assert!(text.contains("difu server upgrade old"), "{text}");
        // A server with a problem explains it instead of opening.
        ui.new_session_choice(2);
        let Some(Modal::NewSession(p)) = &ui.modal else {
            anyhow::bail!("picker");
        };
        assert!(!p.choosing_repository());
        assert!(p.error.as_deref().is_some_and(|e| e.contains("upgrade")));
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        ui.key(down);
        ui.key(enter);
        let Some(Modal::NewSession(p)) = &ui.modal else {
            anyhow::bail!("picker");
        };
        assert_eq!(p.server.as_deref(), Some("hetzner"));
        let text = screen(&mut ui)?;
        assert!(text.contains("acme/api"), "{text}");
        assert!(text.contains("cloning…"), "{text}");
        ui.paste("web");
        ui.key(enter);
        let Some(Modal::NewSession(p)) = &ui.modal else {
            anyhow::bail!("picker");
        };
        assert!(p.error.as_deref().is_some_and(|e| e.contains("cloned")));
        assert!(!ui.busy);
        // Esc returns to the machines, then closes.
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(&ui.modal, Some(Modal::NewSession(p)) if !p.choosing_repository()));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        Ok(())
    }

    #[test]
    fn choosing_a_server_repository_launches_there() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        picker(&mut ui, vec![status("hetzner", None)]);
        ui.new_session_choice(1);
        let request = shortcut_tests::request_at_boundary_with(&mut ui, |ui| {
            ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        })?;
        let Request::NewAgent {
            server, defaults, ..
        } = request
        else {
            anyhow::bail!("new agent request");
        };
        assert_eq!(server.as_deref(), Some("hetzner"));
        assert_eq!(
            defaults.repository,
            Some(PathBuf::from("/srv/difu/repos/acme/api"))
        );
        // "This Mac" keeps the local flow.
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        picker(&mut ui, vec![status("hetzner", None)]);
        let click = shortcut_tests::native_target(&mut ui, 100, "This Mac")?;
        let request =
            shortcut_tests::request_at_connected_boundary_with(&mut ui, |ui, _| ui.mouse(click))?;
        assert!(matches!(request, Request::NewAgent { server: None, .. }));
        Ok(())
    }

    #[test]
    fn server_sessions_show_a_badge_and_refuse_local_file_features() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let mut session = crate::cloud::federation::tests::session("hetzner:1");
        session.title = "Server task".into();
        let mut summary = session.summary();
        summary.server = Some("hetzner".into());
        ui.summaries.push(summary);
        ui.sessions.insert(session.id.clone(), session);
        ui.selected = Some("hetzner:1".into());
        ui.drilled = false;
        ui.focus = Focus::List;
        let text = screen(&mut ui)?;
        assert!(text.contains("hetzner · api"), "{text}");
        ui.attach(None);
        assert_eq!(
            ui.notice.as_ref().map(|(n, error)| (n.as_str(), *error)),
            Some((UNAVAILABLE, true))
        );
        assert!(ui.media_pending.is_none());
        ui.notice = None;
        ui.restore_media("hetzner:1");
        assert!(ui.notice.is_none(), "no attachment restore error");
        ui.sessions
            .get_mut("hetzner:1")
            .context("session")?
            .artifacts
            .push(super::super::super::artifacts::Artifact {
                path: "report.html".into(),
                title: "Report".into(),
                workspace: None,
            });
        ui.select_resource(true, 0);
        assert_eq!(
            ui.notice.as_ref().map(|(n, _)| n.as_str()),
            Some(UNAVAILABLE)
        );
        assert!(!matches!(ui.modal, Some(Modal::InstallBrowser { .. })));
        Ok(())
    }
}
