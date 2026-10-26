//! Sidebar folders: cards in the main list that open into their own session list.
use super::*;
use crate::agents::folders::{self, Folder};

pub(super) enum Row {
    Folder(Card),
    Session(Summary),
}
pub(super) struct Card {
    pub id: String,
    pub name: String,
    /// Members matching the active/archived view.
    pub sessions: Vec<Summary>,
}
impl Row {
    /// Sidebar animation key; folder keys never collide with session ids.
    pub(super) fn key(&self) -> String {
        match self {
            Self::Folder(card) => key(&card.id),
            Self::Session(session) => session.id.clone(),
        }
    }
}
pub(super) fn key(id: &str) -> String {
    format!("folder:{id}")
}

impl Card {
    /// "2 sessions · opencx, widget".
    pub(super) fn detail(&self) -> String {
        if self.sessions.is_empty() {
            return "Empty".into();
        }
        let mut repositories: Vec<String> = Vec::new();
        for session in &self.sessions {
            let name = session
                .repository
                .as_ref()
                .unwrap_or(&session.workspace)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if !name.is_empty() && !repositories.contains(&name) {
                repositories.push(name);
            }
        }
        let count = self.sessions.len();
        let mut text = format!("{count} session{}", if count == 1 { "" } else { "s" });
        if let [first, rest @ ..] = repositories.as_slice() {
            text.push_str(" · ");
            text.push_str(first);
            if let [second, more @ ..] = rest {
                text.push_str(", ");
                text.push_str(second);
                if !more.is_empty() {
                    text.push_str(&format!(" +{}", more.len()));
                }
            }
        }
        text
    }
    /// The most urgent state first, and its colour.
    pub(super) fn status(&self) -> (String, ratatui::style::Color) {
        let count =
            |test: fn(Status) -> bool| self.sessions.iter().filter(|s| test(s.status)).count();
        let waiting = count(|s| s == Status::Waiting);
        let failed = count(|s| s == Status::Failed);
        let working = count(|s| matches!(s, Status::Starting | Status::Running));
        let rest = self
            .sessions
            .len()
            .saturating_sub(waiting + failed + working);
        let parts: Vec<String> = [
            (waiting, "need input"),
            (failed, "failed"),
            (working, "working"),
            (rest, "idle"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| {
            if n == 1 && label == "need input" {
                "1 needs input".into()
            } else {
                format!("{n} {label}")
            }
        })
        .collect();
        let color = if waiting > 0 {
            ACCENT
        } else if failed > 0 {
            RED
        } else if working > 0 {
            DIM
        } else {
            GREEN
        };
        if parts.is_empty() {
            ("Ask the orchestrator to file sessions here".into(), DIM)
        } else {
            (parts.join(" · "), color)
        }
    }
}

impl Ui {
    /// Session id → folder id for every filed coding session.
    pub(super) fn filed(&self) -> HashMap<&str, &str> {
        self.session_folders
            .iter()
            .flat_map(|folder| {
                folder
                    .sessions
                    .iter()
                    .map(move |id| (id.as_str(), folder.id.as_str()))
            })
            .filter(|(id, _)| *id != super::super::orchestrator::ID)
            .collect()
    }
    /// The sidebar rows: the orchestrator, then folders, then unfiled sessions.
    pub(super) fn rows(&self) -> Vec<Row> {
        let sessions = self.filtered();
        if self.orchestrator_list || self.folder_view.is_some() || self.session_folders.is_empty() {
            return sessions.into_iter().map(Row::Session).collect();
        }
        let mut rows = Vec::new();
        let mut rest = sessions.into_iter().peekable();
        if let Some(root) = rest.next_if(|s| s.id == super::super::orchestrator::ID) {
            rows.push(Row::Session(root));
        }
        for folder in &self.session_folders {
            let sessions: Vec<Summary> = folder
                .sessions
                .iter()
                .filter_map(|id| self.summaries.iter().find(|s| &s.id == id))
                .filter(|s| {
                    s.kind != "Guide" && s.kind != "Orchestrator" && s.archived == self.archived
                })
                .cloned()
                .collect();
            // Archived view lists only folders holding archived sessions.
            if self.archived && sessions.is_empty() {
                continue;
            }
            rows.push(Row::Folder(Card {
                id: folder.id.clone(),
                name: folder.name.clone(),
                sessions,
            }));
        }
        rows.extend(rest.map(Row::Session));
        rows
    }
    pub(super) fn selectable_rows(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| match row {
                Row::Session(session) => !self.deleting.contains(&session.id),
                Row::Folder(_) => true,
            })
            .collect()
    }
    /// The highlighted row's key: a folder card or the selected session.
    pub(super) fn cursor(&self) -> Option<String> {
        self.folder_cursor
            .as_deref()
            .map(key)
            .or_else(|| self.selected.clone())
    }
    pub(super) fn open_folder_name(&self) -> Option<&str> {
        let id = self.folder_view.as_ref()?;
        self.session_folders
            .iter()
            .find(|f| &f.id == id)
            .map(|f| f.name.as_str())
    }
    /// Highlights a folder card; no session is selected meanwhile.
    pub(super) fn focus_folder(&mut self, id: String) {
        if self.selected.is_some() && self.inline_question() {
            self.remember_answers();
            self.modal = None;
        }
        self.folder_cursor = Some(id);
        self.selected = None;
        self.drilled = false;
        self.conversation_sections.clear();
        self.change_rows = Default::default();
        self.text_selection.clear();
        self.changes_at = None;
    }
    pub(super) fn open_folder(&mut self, id: String) {
        if !self.session_folders.iter().any(|f| f.id == id) {
            return;
        }
        self.folder_view = Some(id);
        self.folder_cursor = None;
        self.selected = None;
        self.drilled = false;
        self.focus = Focus::List;
        self.list_scroll = 0;
        if let Some(Row::Session(first)) = self.selectable_rows().into_iter().next() {
            self.select(first.id);
        }
    }
    /// Returns to the main list with the folder we left highlighted.
    pub(super) fn close_folder(&mut self) {
        let Some(id) = self.folder_view.take() else {
            return;
        };
        self.focus = Focus::List;
        self.list_scroll = 0;
        self.focus_folder(id);
        self.ensure_selected();
    }
    /// Shows the list that holds `id`, as opening it from search does.
    pub(super) fn reveal_in_folder(&mut self, id: &str) {
        self.folder_cursor = None;
        self.folder_view = self.filed().get(id).map(|folder| (*folder).to_owned());
    }
    /// Picks up folder changes the orchestrator saved; runs with each list refresh.
    pub(super) fn reload_folders(&mut self) {
        if let Ok(config) = self.storage.load_config() {
            self.set_folders(config.session_folders);
        }
    }
    pub(super) fn set_folders(&mut self, folders: Vec<Folder>) {
        if folders == self.session_folders {
            return;
        }
        self.session_folders = folders;
        if let Some(view) = &self.folder_view
            && !self.session_folders.iter().any(|f| &f.id == view)
        {
            self.folder_view = None;
        }
        if let Some(cursor) = &self.folder_cursor
            && !self.session_folders.iter().any(|f| &f.id == cursor)
        {
            self.folder_cursor = None;
        }
        // Follow the selected session when it was filed elsewhere.
        if self.orchestrator_list {
            return;
        }
        let Some(selected) = self.selected.clone() else {
            return;
        };
        let home = self.filed().get(selected.as_str()).map(|f| (*f).to_owned());
        if home == self.folder_view {
            return;
        }
        if self.drilled {
            self.folder_view = home;
        } else if let (None, Some(home)) = (&self.folder_view, home) {
            self.focus_folder(home);
        }
    }
    /// Ctrl+↑/↓ on a folder card moves it among the folders (saved).
    pub(super) fn reorder_folder(&mut self, down: bool) {
        let Some(id) = self.folder_cursor.clone() else {
            return;
        };
        let result = self.storage.update_config(|config| {
            folders::shift(&mut config.session_folders, &id, down);
        });
        match result {
            Ok(config) => self.set_folders(config.session_folders),
            Err(error) => {
                self.notice = Some((format!("Could not save folder order: {error:#}"), true))
            }
        }
    }
    /// A session started inside a folder is filed there.
    pub(super) fn file_launched(&mut self, id: &str) {
        let Some(folder) = self.folder_view.clone() else {
            return;
        };
        let ids = [id.to_owned()];
        let result = self.storage.try_update_config(|config| {
            folders::file(&mut config.session_folders, &ids, Some(&folder))
        });
        match result {
            Ok(config) => self.set_folders(config.session_folders),
            Err(error) => {
                self.notice = Some((format!("Could not file the new session: {error:#}"), true))
            }
        }
    }
    /// The main pane while a folder card is highlighted.
    pub(super) fn draw_folder_overview(&mut self, frame: &mut Frame, rect: Rect) -> bool {
        let Some(id) = self.folder_cursor.clone() else {
            return false;
        };
        let Some(card) = self.rows().into_iter().find_map(|row| match row {
            Row::Folder(card) if card.id == id => Some(card),
            _ => None,
        }) else {
            return false;
        };
        let area = panel(
            frame,
            rect,
            &crate::model::clean(&format!("{} · Folder", card.name)),
            false,
        );
        self.hits.push((rect, Action::OpenFolder(card.id.clone())));
        let (status, color) = card.status();
        let mut lines = vec![
            Line::from(Span::styled(card.detail(), Style::default().fg(TEXT))),
            Line::from(Span::styled(status, Style::default().fg(color))),
            Line::default(),
        ];
        for session in &card.sessions {
            let color = match session.status {
                Status::Failed => RED,
                Status::Waiting => ACCENT,
                Status::Completed | Status::Idle => GREEN,
                _ => DIM,
            };
            let width = usize::from(area.width).saturating_sub(4);
            lines.push(Line::from(vec![
                Span::styled("• ", Style::default().fg(DIM)),
                Span::styled(
                    crate::ui::crop(&session.title, 0, width.saturating_sub(16)),
                    Style::default().fg(TEXT),
                ),
                Span::styled(
                    format!("  {}", session.status.label()),
                    Style::default().fg(color),
                ),
            ]));
        }
        if !card.sessions.is_empty() {
            lines.push(Line::default());
        }
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        let hint =
            |action| bindings.primary_hint_in(action, crate::shortcuts::contexts::AGENT_LIST);
        use crate::shortcuts::ids::{
            NAVIGATION_ACTIVATE, SESSION_REORDER_DOWN, SESSION_REORDER_UP,
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{} · Open folder    {} / {} · Move folder",
                hint(NAVIGATION_ACTIVATE),
                hint(SESSION_REORDER_UP),
                hint(SESSION_REORDER_DOWN)
            ),
            Style::default().fg(DIM),
        )));
        lines.push(Line::from(Span::styled(
            "Ask the orchestrator to create, rename or fill folders.",
            Style::default().fg(DIM),
        )));
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        true
    }
}

impl Ui {
    /// One sidebar card per folder, sized like a session card.
    pub(super) fn draw_folder_card(
        &mut self,
        frame: &mut Frame,
        card: &Card,
        row: Rect,
        settled: bool,
        now: Instant,
    ) {
        let selected = self.folder_cursor.as_deref() == Some(card.id.as_str());
        let (status, color) = card.status();
        let title_width = usize::from(row.width).saturating_sub(5);
        let mut lines = vec![
            Line::from(Span::styled(
                format!(
                    "{} 📁 {}",
                    if selected { "›" } else { " " },
                    crate::ui::crop(&card.name, 0, title_width)
                ),
                Style::default().fg(if selected { ACCENT } else { TEXT }),
            )),
            Line::from(Span::styled(
                format!("  {}", card.detail()),
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                format!("  {status}"),
                Style::default().fg(color),
            )),
        ];
        let key = key(&card.id);
        let board = self.sidebar_boards.entry(key.clone()).or_default();
        if settled {
            board.settle(&lines, now);
        } else {
            board.show(&mut lines, now, ACCENT, row.width);
        }
        frame.render_widget(
            Paragraph::new(lines).style(if selected {
                crate::ui::user_message_style()
            } else {
                Style::default().bg(BG)
            }),
            row,
        );
        if let Some(highlight) = self.sidebar_highlights.get(&key) {
            let background = crate::ui::user_message_style().bg.unwrap_or(PANEL);
            highlight.apply(frame.buffer_mut(), row, background, BG, now);
        }
        self.hits.push((row, Action::OpenFolder(card.id.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use ratatui::{Terminal, backend::TestBackend};

    fn session(id: &str, title: &str, status: Status) -> Session {
        let mut session = Session::new(
            id.into(),
            Job::Coding(Launch {
                repository: "/tmp/project".into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.title = title.into();
        session.status = status;
        session
    }
    /// Orchestrator, one unfiled session and a "Widget" folder holding two.
    fn fixture(dir: &std::path::Path) -> Result<Ui> {
        let storage = Storage {
            config: dir.join("config.json"),
            cache: dir.join("cache"),
        };
        storage.update_config(|config| {
            config.session_folders = vec![Folder {
                id: "widget".into(),
                name: "Widget".into(),
                sessions: vec!["two".into(), "three".into()],
            }];
        })?;
        let config = storage.load_config()?;
        let mut ui = Ui::new(storage, &config);
        let mut root = session(
            super::super::super::orchestrator::ID,
            "Difu orchestrator",
            Status::Idle,
        );
        root.orchestrator = true;
        for session in [
            root,
            session("one", "Loose task", Status::Idle),
            session("two", "Fix composer", Status::Running),
            session("three", "Polish menu", Status::Waiting),
        ] {
            ui.summaries.push(session.summary());
            ui.sessions.insert(session.id.clone(), session);
        }
        ui.select(super::super::super::orchestrator::ID.into());
        Ok(ui)
    }
    fn screen(ui: &mut Ui) -> Result<String> {
        let mut terminal = Terminal::new(TestBackend::new(120, 32))?;
        terminal.draw(|f| ui.draw(f))?;
        Ok(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect())
    }
    fn keys(ui: &Ui) -> Vec<String> {
        ui.rows().iter().map(Row::key).collect()
    }
    fn press(ui: &mut Ui, code: KeyCode) {
        ui.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn folders_are_cards_that_open_into_their_own_list() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path())?;
        assert_eq!(keys(&ui), ["orchestrator", "folder:widget", "one"]);
        // Filed sessions leave the main list.
        assert!(
            ui.filtered()
                .iter()
                .all(|s| s.id != "two" && s.id != "three")
        );

        press(&mut ui, KeyCode::Down);
        assert_eq!(ui.folder_cursor.as_deref(), Some("widget"));
        assert!(ui.selected.is_none());
        let shown = screen(&mut ui)?;
        // The test buffer pads the wide folder glyph with a blank cell.
        assert!(shown.contains("› 📁  Widget"));
        assert!(shown.contains("2 sessions · project"));
        assert!(shown.contains("1 needs input · 1 working"));
        assert!(shown.contains("Widget · Folder"));
        assert!(shown.contains("Fix composer"));

        press(&mut ui, KeyCode::Enter);
        assert_eq!(ui.folder_view.as_deref(), Some("widget"));
        assert!(!ui.drilled);
        let inside: Vec<_> = ui.filtered().into_iter().map(|s| s.id).collect();
        assert_eq!(inside.len(), 2);
        assert!(inside.contains(&"two".to_owned()) && inside.contains(&"three".to_owned()));
        assert!(ui.selected.as_ref().is_some_and(|id| inside.contains(id)));
        let shown = screen(&mut ui)?;
        assert!(shown.contains("← All sessions · Esc"));
        assert!(!shown.contains("Loose task"));

        // Esc interrupts working sessions, so step back from idle ones.
        for summary in &mut ui.summaries {
            summary.status = Status::Idle;
        }
        for session in ui.sessions.values_mut() {
            session.status = Status::Idle;
        }
        press(&mut ui, KeyCode::Enter);
        assert!(ui.drilled);
        press(&mut ui, KeyCode::Esc);
        assert!(!ui.drilled && ui.folder_view.is_some());
        press(&mut ui, KeyCode::Esc);
        assert!(ui.folder_view.is_none());
        assert_eq!(ui.folder_cursor.as_deref(), Some("widget"));

        press(&mut ui, KeyCode::Down);
        assert_eq!(ui.selected.as_deref(), Some("one"));
        assert!(ui.folder_cursor.is_none());
        ui.action(Action::OpenFolder("widget".into()));
        assert_eq!(ui.folder_view.as_deref(), Some("widget"));
        ui.action(Action::FolderBack);
        assert!(ui.folder_view.is_none());
        Ok(())
    }

    #[test]
    fn orchestrator_changes_appear_live_and_follow_the_open_session() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path())?;
        ui.select("one".into());
        ui.action(Action::Open);
        assert!(ui.drilled);
        ui.storage.try_update_config(|config| {
            let id = crate::agents::folders::create(&mut config.session_folders, "Loose")?;
            crate::agents::folders::file(
                &mut config.session_folders,
                &["one".to_owned()],
                Some(&id),
            )
        })?;
        ui.reload_folders();
        // The open chat stays selected and the list shows its new folder.
        assert_eq!(ui.folder_view.as_deref(), Some("loose"));
        assert_eq!(ui.selected.as_deref(), Some("one"));
        assert_eq!(ui.open_folder_name(), Some("Loose"));

        ui.storage.update_config(|config| {
            config.session_folders.retain(|f| f.id != "loose");
        })?;
        ui.reload_folders();
        assert!(ui.folder_view.is_none());
        assert_eq!(ui.selected.as_deref(), Some("one"));
        Ok(())
    }

    #[test]
    fn folders_reorder_and_new_sessions_join_the_open_folder() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path())?;
        ui.storage.try_update_config(|config| {
            crate::agents::folders::create(&mut config.session_folders, "Later").map(drop)
        })?;
        ui.reload_folders();
        assert_eq!(
            keys(&ui),
            ["orchestrator", "folder:widget", "folder:later", "one"]
        );
        ui.focus_folder("later".into());
        ui.reorder_session(false);
        assert_eq!(
            keys(&ui),
            ["orchestrator", "folder:later", "folder:widget", "one"]
        );
        let saved = ui.storage.load_config()?.session_folders;
        assert_eq!(saved.first().map(|f| f.id.as_str()), Some("later"));

        ui.open_folder("later".into());
        assert!(ui.selected.is_none());
        assert!(screen(&mut ui)?.contains("This folder is empty"));
        ui.file_launched("four");
        let saved = ui.storage.load_config()?.session_folders;
        assert_eq!(
            crate::agents::folders::home(&saved, "four").map(|f| f.id.as_str()),
            Some("later")
        );
        Ok(())
    }

    #[test]
    fn search_opens_a_filed_session_inside_its_folder() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path())?;
        ui.open_session("three".into());
        assert_eq!(ui.folder_view.as_deref(), Some("widget"));
        assert!(ui.drilled);
        ui.open_session("one".into());
        assert!(ui.folder_view.is_none());
        Ok(())
    }

    #[test]
    fn archived_view_hides_folders_without_archived_sessions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = fixture(dir.path())?;
        ui.archived = true;
        assert!(keys(&ui).iter().all(|key| !key.starts_with("folder:")));
        ui.summaries
            .iter_mut()
            .find(|s| s.id == "two")
            .context("two")?
            .archived = true;
        assert!(keys(&ui).contains(&"folder:widget".to_owned()));
        Ok(())
    }
}
