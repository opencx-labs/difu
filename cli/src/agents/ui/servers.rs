//! Cloud servers in the session UI: which sessions run on a saved server.
use super::*;
use crate::cloud::federation;

/// Marks server sessions in the sessions list, after the worktree tree. The
/// emoji presentation selector makes it two cells wide, like the tree.
pub(super) const CLOUD: &str = "\u{2601}\u{fe0f}";

/// Server sessions have `<server>:<session>` ids; local ids never contain `:`.
pub(super) fn is_server_session(id: &str) -> bool {
    federation::split(id).is_some()
}

/// The saved server running a session, from its id.
pub(super) fn server_name(id: &str) -> Option<&str> {
    federation::split(id).map(|(server, _)| server)
}

impl Ui {
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
        let (mut ui, dir, _) = shortcut_tests::fixture(None)?;
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
        // Pasted media is a private copy on this computer until Send uploads
        // it, and the draft keeps it like a local session's.
        let shot = dir.path().join("shot.png");
        image::RgbaImage::new(2, 2).save(&shot)?;
        ui.attach(Some(vec![shot]));
        let deadline = Instant::now() + Duration::from_secs(5);
        while ui.media_pending.is_some() {
            anyhow::ensure!(Instant::now() < deadline, "attachment never read");
            thread::sleep(Duration::from_millis(5));
            ui.tick_media();
        }
        assert!(ui.notice.is_none(), "{:?}", ui.notice);
        let position = ui.positions.get("hetzner:1").context("position")?;
        assert_eq!(position.draft.text(), "[image 1] ");
        let copy = position.attachments.first().context("copy")?.clone();
        let local = crate::agents::media::directory(&ui.storage, "hetzner:1")?;
        assert_eq!(copy.path.parent(), Some(local.as_path()));
        assert!(local.ends_with("hetzner@1"));
        crate::agents::media::validate(&ui.storage, "hetzner:1", &copy)?;
        ui.cache_writer.flush()?;
        ui.positions.remove("hetzner:1");
        ui.restore_media("hetzner:1");
        assert!(ui.notice.is_none(), "no attachment restore error");
        let restored = ui.positions.get("hetzner:1").context("restored")?;
        assert_eq!(restored.attachments, vec![copy]);
        assert_eq!(restored.draft.text(), "[image 1]");
        Ok(())
    }

    #[test]
    fn server_sessions_mark_a_server_short_of_disk_space() -> Result<()> {
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
        assert!(!text.contains("disk low"), "{text}");
        let low = federation::ServerStatus {
            name: "hetzner".into(),
            disk: Some(crate::cloud::disk::Report {
                status: crate::cloud::disk::Status::Low,
                ..Default::default()
            }),
            ..Default::default()
        };
        ui.cloud.remember(&[low]);
        let text = screen(&mut ui)?;
        assert!(text.contains("on hetzner (disk low)"), "{text}");
        // A narrow list keeps the name, colored, without the words.
        assert!(text.contains("hetzner · api"), "{text}");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let colored = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "h" && cell.fg == crate::ui::YELLOW);
        assert!(colored, "the server name takes the low-disk color");
        Ok(())
    }

    #[test]
    fn server_session_artifacts_open_from_a_downloaded_copy() -> Result<()> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let mut session = crate::cloud::federation::tests::session("hetzner:1");
        let artifact = super::super::super::artifacts::Artifact {
            path: "out/report.html".into(),
            title: "Report".into(),
            workspace: Some("/srv/difu/agents/worktrees/1".into()),
        };
        session.artifacts.push(artifact.clone());
        let mut summary = session.summary();
        summary.server = Some("hetzner".into());
        ui.summaries.push(summary);
        ui.sessions.insert(session.id.clone(), session);
        ui.selected = Some("hetzner:1".into());
        let listener = UnixListener::bind(crate::agents::server::socket(&ui.storage)?)?;
        let replies = vec![
            Reply::ArtifactFile(STANDARD.encode("<h1>First</h1>")),
            Reply::ArtifactFile(STANDARD.encode("<h1>Second</h1>")),
            Reply::Error(
                "out/report.html is 40.0 MB; artifacts open from a server up to 32 MB".into(),
            ),
        ];
        let (requests, received) = mpsc::channel();
        let service = thread::spawn(move || -> Result<()> {
            for reply in replies {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                requests.send(serde_json::from_str::<Request>(&line)?)?;
                serde_json::to_writer(&mut stream, &reply)?;
                stream.write_all(b"\n")?;
            }
            Ok(())
        });
        let finish = |ui: &mut Ui| -> Result<Request> {
            let request = received.recv_timeout(Duration::from_secs(5))?;
            let message = ui.receiver.recv_timeout(Duration::from_secs(5))?;
            ui.sender.send(message)?;
            ui.tick(false);
            Ok(request)
        };
        ui.select_resource(true, 0);
        assert_eq!(
            ui.notice.as_ref().map(|(n, error)| (n.as_str(), *error)),
            Some(("Downloading Report from hetzner…", false))
        );
        let request = finish(&mut ui)?;
        assert!(
            matches!(&request, Request::ReadArtifact { id, path, workspace }
                if id == "hetzner:1" && path == &artifact.path && workspace == &artifact.workspace),
            "{request:?}"
        );
        let copy = super::super::super::artifacts::local_copy(&ui.storage, "hetzner:1", &artifact)?;
        assert!(copy.starts_with(ui.storage.cache.join("server-artifacts/hetzner@1")));
        assert_eq!(
            copy.file_name().and_then(|n| n.to_str()),
            Some("report.html")
        );
        assert_eq!(std::fs::read_to_string(&copy)?, "<h1>First</h1>");
        // The copy opens exactly like a local artifact: in the embedded
        // browser, or after offering to install it.
        let opened = match (&ui.modal, &ui.panels.view) {
            (Some(Modal::InstallBrowser { path, .. }), _) => path.clone(),
            (_, Some(panels::View::Artifact { path, .. })) => path.clone(),
            _ => anyhow::bail!("artifact not opened"),
        };
        assert_eq!(opened, copy);
        assert!(ui.notice.is_none(), "{:?}", ui.notice);
        // Refresh downloads the server's current copy again.
        ui.modal = None;
        ui.panels.view = Some(panels::View::Artifact {
            path: copy.clone(),
            title: "Report".into(),
            browser: None,
            error: None,
        });
        ui.panels.session = Some("hetzner:1".into());
        ui.refresh_artifact();
        assert!(matches!(finish(&mut ui)?, Request::ReadArtifact { .. }));
        assert_eq!(std::fs::read_to_string(&copy)?, "<h1>Second</h1>");
        // Too large on the server: a clear error, and the last copy stays.
        ui.refresh_artifact();
        finish(&mut ui)?;
        assert!(
            ui.notice
                .as_ref()
                .is_some_and(|(n, error)| *error && n.contains("up to 32 MB")),
            "{:?}",
            ui.notice
        );
        assert_eq!(std::fs::read_to_string(&copy)?, "<h1>Second</h1>");
        service
            .join()
            .map_err(|_| anyhow::anyhow!("service panicked"))??;
        Ok(())
    }
}
