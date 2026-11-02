//! Cloud servers in the session UI: which sessions run on a saved server, and
//! the notice for features that need a session's files on this computer.
use super::*;
use crate::cloud::federation;

pub(super) const UNAVAILABLE: &str = "Not available for server sessions";

/// Server sessions have `<server>:<session>` ids; local ids never contain `:`.
pub(super) fn is_server_session(id: &str) -> bool {
    federation::split(id).is_some()
}

/// The saved server running a session, from its id.
pub(super) fn server_name(id: &str) -> Option<&str> {
    federation::split(id).map(|(server, _)| server)
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
    /// The saved server running the selected session; `None` is this Mac.
    pub(super) fn selected_server(&self) -> Option<String> {
        self.selected
            .as_deref()
            .and_then(server_name)
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};

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
