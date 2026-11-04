//! "Move to server…": moves the selected session from this computer to a saved
//! cloud server (`Request::Move`) and opens the new server session. On a server
//! session the entry is "Move to this computer" (`Request::MoveHere`).
use super::*;

impl Ui {
    pub(super) fn open_move_to_server(&mut self) {
        self.modal = None;
        let Some(id) = self.selected.clone() else {
            return;
        };
        if servers::is_server_session(&id) {
            self.busy = true;
            self.notice = Some(("Moving this session to this computer…".into(), false));
            // Like a launch, success opens the new local session.
            self.task(Task::Launch, Request::MoveHere { id }, true);
            return;
        }
        match crate::cloud::servers::load(&self.storage) {
            Ok(saved) if saved.is_empty() => {
                self.notice = Some((
                    "No saved servers. Add one with `difu server add`.".into(),
                    true,
                ))
            }
            Ok(saved) => {
                self.modal = Some(Modal::MoveToServer {
                    servers: saved.into_iter().map(|server| server.name).collect(),
                    selected: 0,
                })
            }
            Err(error) => {
                self.notice = Some((format!("Cannot read saved servers: {error:#}"), true))
            }
        }
    }
    pub(super) fn choose_move_server(&mut self, index: usize) {
        let Some(Modal::MoveToServer { servers, .. }) = &self.modal else {
            return;
        };
        let (Some(server), Some(id)) = (servers.get(index).cloned(), self.selected.clone()) else {
            return;
        };
        self.modal = None;
        self.busy = true;
        self.notice = Some((format!("Moving this session to {server}…"), false));
        // Like a launch, success opens the new server session.
        self.task(Task::Launch, Request::Move { id, server }, true);
    }
    pub(super) fn draw_move_to_server(
        &self,
        frame: &mut Frame,
        area: Rect,
        servers: &[String],
        selected: usize,
        activate_hint: &str,
        deferred: &mut Vec<(Rect, String, Action, bool)>,
    ) {
        frame.render_widget(
            Paragraph::new(format!(
                "The server continues this conversation with its branch, commits and uncommitted files as a new session. This copy is archived and keeps its worktree.\n{activate_hint} moves · Esc cancels"
            ))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(DIM)),
            Rect::new(area.x, area.y, area.width, 4.min(area.height)),
        );
        let list = Rect::new(
            area.x,
            area.y.saturating_add(5),
            area.width,
            area.height.saturating_sub(5),
        );
        for (offset, server) in servers.iter().enumerate().take(list.height as usize) {
            deferred.push((
                Rect::new(list.x, list.y + offset as u16, list.width, 1),
                format!(
                    "{} {}",
                    if offset == selected { ">" } else { " " },
                    crate::model::clean(server)
                ),
                Action::MoveChoice(offset),
                offset == selected,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use anyhow::{Context, Result};
    use ratatui::{Terminal, backend::TestBackend};

    fn fixture(dir: &std::path::Path) -> Result<Ui> {
        let storage = Storage {
            config: dir.join("config.json"),
            cache: dir.join("cache"),
        };
        let config = storage.load_config()?;
        let mut ui = Ui::new(storage, &config);
        for id in ["one", "box:7"] {
            let mut session = Session::new(
                id.into(),
                Job::Coding(Launch {
                    repository: "/tmp/repo".into(),
                    isolated: true,
                    base: "origin/main".into(),
                    prompt: String::new(),
                    model: None,
                    effort: None,
                }),
            );
            session.status = Status::Idle;
            ui.summaries.push(session.summary());
            ui.sessions.insert(id.into(), session);
        }
        ui.select("one".into());
        Ok(ui)
    }

    fn entry(ui: &Ui) -> Result<usize> {
        ui.menu_entries()
            .iter()
            .position(|label| label == "Move to server…" || label == "Move to this computer")
            .context("move entry")
    }

    #[test]
    fn the_menu_moves_a_local_session_to_a_chosen_saved_server() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path())?;
        ui.menu_action(entry(&ui)?);
        assert!(ui.modal.is_none());
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| *error && text.contains("difu server add"))
        );

        for name in ["box", "home"] {
            crate::cloud::servers::save(
                &ui.storage,
                crate::cloud::servers::tests::server(name, &format!("id-{name}")),
            )?;
        }
        ui.notice = None;
        ui.menu_action(entry(&ui)?);
        assert!(
            matches!(&ui.modal, Some(Modal::MoveToServer { servers, selected: 0 })
            if servers == &["box".to_owned(), "home".to_owned()])
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 32))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            shown.contains("Move to server") && shown.contains("> box"),
            "{shown}"
        );
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(
            &ui.modal,
            Some(Modal::MoveToServer { selected: 1, .. })
        ));
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ui.modal.is_none() && ui.busy);
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(text, error)| !*error && text == "Moving this session to home…")
        );

        // Server sessions move to this computer instead.
        ui.busy = false;
        ui.select("box:7".into());
        assert!(
            ui.menu_entries()
                .iter()
                .any(|label| label == "Move to this computer")
        );
        ui.menu_action(entry(&ui)?);
        assert!(ui.modal.is_none() && ui.busy);
        assert!(ui.notice.as_ref().is_some_and(
            |(text, error)| !*error && text == "Moving this session to this computer…"
        ));
        Ok(())
    }
}
