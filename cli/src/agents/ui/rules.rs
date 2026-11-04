use super::*;
use crate::{
    cloud::environments,
    shortcuts::{Bindings, contexts::*, ids::*},
};
use std::path::PathBuf;

type Loaded = std::result::Result<(PathBuf, String), String>;

/// What the modal edits: a repository's rules, or a server repository's
/// environment profile (as `environments::to_text`'s sectioned text).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Rules,
    /// Opened from `/cloud`; closing returns there.
    Environment,
}

/// Reads or replaces a server repository's environment profile with
/// `Request::Environment`; the agent service forwards a saved server's to it.
fn environment_request(
    storage: &Storage,
    server: Option<String>,
    repository: PathBuf,
    text: Option<String>,
) -> Loaded {
    let profile = match text {
        Some(text) => Some(environments::from_text(&text).map_err(|error| format!("{error:#}"))?),
        None => None,
    };
    let request = Request::Environment {
        server,
        repository: repository.to_string_lossy().into_owned(),
        profile,
        by: Some("the difu terminal app".into()),
    };
    match client::request(storage, request) {
        Ok(Reply::Environment(view)) => {
            Ok((PathBuf::from(&view.repository), environment_text(&view)))
        }
        Ok(_) => Err("Unexpected environment reply".into()),
        Err(error) => Err(format!("{error:#}")),
    }
}

/// The profile as editable text, after comments with what the repository's
/// files declare and the latest change.
pub(super) fn environment_text(view: &environments::View) -> String {
    let mut text = String::new();
    if !view.detected.is_empty() {
        text.push_str(&format!(
            "# {} declares on its main branch: {}.\n# The repository's files win over [tools]; the profile fills gaps.\n",
            view.repository,
            environments::describe(&view.detected)
        ));
    }
    if let Some(change) = view.profile.history.last() {
        let at = chrono::DateTime::from_timestamp_millis(change.at)
            .map(|at| {
                at.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        text.push_str(&format!(
            "# Last changed {at} by {}: {}\n",
            change.by,
            environments::short(&change.summary, 160)
        ));
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&environments::to_text(&view.profile));
    text
}

/// Reads or replaces rules with `Request::RepositoryRules`; the agent service
/// answers for this computer and forwards a saved server's to that server.
fn request(storage: &Storage, request: Request) -> Loaded {
    match client::request(storage, request) {
        Ok(Reply::RepositoryRules { root, rules }) => Ok((root, rules)),
        Ok(_) => Err("Unexpected repository rules reply".into()),
        Err(error) => Err(format!("{error:#}")),
    }
}

pub struct Settings {
    pub repository: Editor,
    pub text: Editor,
    pub field: usize,
    root: Option<PathBuf>,
    saved: String,
    candidates: Vec<PathBuf>,
    selected: usize,
    loading: Option<mpsc::Receiver<Loaded>>,
    error: Option<String>,
    return_to: Option<Box<defaults::Settings>>,
    /// The saved cloud server whose rules these are; `None` is this computer.
    server: Option<String>,
    saving: Option<mpsc::Receiver<Loaded>>,
    kind: Kind,
}

impl Settings {
    fn options(&self) -> Vec<PathBuf> {
        let query = self.repository.text().to_lowercase();
        self.candidates
            .iter()
            .filter(|path| path.to_string_lossy().to_lowercase().contains(&query))
            .cloned()
            .collect()
    }
    fn load(&mut self, storage: Storage, path: PathBuf) {
        if self.text.text() != self.saved {
            if let Some(root) = &self.root {
                self.repository = Editor::from(root.to_string_lossy().as_ref());
            }
            self.error =
                Some("Save or cancel these changes before choosing another repository".into());
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx);
        self.error = None;
        self.root = None;
        let server = self.server.clone();
        let kind = self.kind;
        std::thread::spawn(move || {
            let result = match kind {
                Kind::Rules => request(
                    &storage,
                    Request::RepositoryRules {
                        server,
                        repository: path,
                        rules: None,
                    },
                ),
                Kind::Environment => environment_request(&storage, server, path, None),
            };
            let _ = tx.send(result);
            crate::repaint::request();
        });
    }
    fn busy(&self) -> bool {
        self.loading.is_some() || self.saving.is_some()
    }
    /// Whether this edits an environment profile rather than rules.
    pub(super) fn environment(&self) -> bool {
        self.kind == Kind::Environment
    }
}

impl Ui {
    pub(super) fn open_rules(&mut self, path: Option<PathBuf>) {
        let config = match self.storage.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some((format!("Cannot load repository rules: {error:#}"), true));
                return;
            }
        };
        // Without a path, a server session's rules are its server's.
        let server = path
            .is_none()
            .then(|| self.selected.as_deref().and_then(servers::server_name))
            .flatten()
            .map(str::to_owned);
        let path = path
            .or_else(|| {
                self.selected
                    .as_ref()
                    .and_then(|id| self.sessions.get(id))
                    .map(|s| s.job.root().clone())
            })
            .or_else(|| {
                server
                    .is_none()
                    .then(|| config.agent_defaults.repository.clone())
                    .flatten()
            })
            .or_else(|| {
                server
                    .is_none()
                    .then(|| std::env::current_dir().ok())
                    .flatten()
            });
        let sessions = self
            .summaries
            .iter()
            .filter(|s| s.server == server)
            .filter_map(|s| s.repository.clone());
        let mut candidates: Vec<_> = if server.is_some() {
            sessions.collect()
        } else {
            config
                .repository_rules
                .keys()
                .cloned()
                .chain(config.repositories.values().cloned())
                .chain(config.agent_defaults.repository)
                .chain(sessions)
                .collect()
        };
        candidates.sort();
        candidates.dedup();
        let return_to = match self.modal.take() {
            Some(Modal::AgentDefaults(form)) => Some(form),
            _ => None,
        };
        let mut form = Settings {
            repository: Editor::from(
                path.as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_str(),
            ),
            text: Editor::default(),
            field: 0,
            root: None,
            saved: String::new(),
            candidates,
            selected: 0,
            loading: None,
            error: None,
            return_to,
            server,
            saving: None,
            kind: Kind::Rules,
        };
        if let Some(path) = path {
            form.load(self.storage.clone(), path);
        }
        self.modal = Some(Modal::RepositoryRules(Box::new(form)));
    }
    pub(super) fn tick_rules(&mut self) {
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        if let Some(result) = form.saving.as_ref().and_then(|rx| rx.try_recv().ok()) {
            crate::repaint::request();
            form.saving = None;
            self.rules_saved(result);
            return;
        }
        let Some(result) = form.loading.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        crate::repaint::request();
        form.loading = None;
        match result {
            Ok((root, text)) => {
                form.repository = Editor::from(root.to_string_lossy().as_ref());
                form.root = Some(root);
                form.text = Editor::from(text.as_str());
                form.saved = text;
                form.field = 1;
            }
            Err(error) => form.error = Some(error),
        }
    }
    pub(super) fn close_rules(&mut self) {
        if let Some(Modal::RepositoryRules(form)) = self.modal.take() {
            self.modal = match form.kind {
                Kind::Rules => form.return_to.map(Modal::AgentDefaults),
                Kind::Environment => Some(Modal::Cloud),
            };
        }
    }
    /// Edits the environment profiles of `server`'s repositories (from
    /// `/cloud`), starting with the first one.
    pub(super) fn open_environment(&mut self, server: String, repositories: Vec<String>) {
        let mut candidates: Vec<PathBuf> = repositories.into_iter().map(PathBuf::from).collect();
        candidates.sort();
        candidates.dedup();
        let first = candidates.first().cloned();
        let mut form = Settings {
            repository: Editor::from(
                first
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_str(),
            ),
            text: Editor::default(),
            field: 0,
            root: None,
            saved: String::new(),
            candidates,
            selected: 0,
            loading: None,
            error: None,
            return_to: None,
            server: Some(server),
            saving: None,
            kind: Kind::Environment,
        };
        match first {
            Some(path) => form.load(self.storage.clone(), path),
            None => {
                form.error = Some(
                    "This server has no repositories yet; add one with `difu repos add` on it"
                        .into(),
                )
            }
        }
        self.modal = Some(Modal::RepositoryRules(Box::new(form)));
    }
    pub(super) fn save_rules(&mut self) {
        let select_hint = Bindings::new(&self.shortcut_config)
            .primary_hint_in(NAVIGATION_ACTIVATE, AGENT_RULES_REPOSITORY);
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        let Some(root) = &form.root else { return };
        if form.repository.text() != root.to_string_lossy() {
            form.error = Some(format!(
                "Press {select_hint} to select the repository first"
            ));
            return;
        }
        if form.busy() {
            return;
        }
        let (server, repository, text, kind) = (
            form.server.clone(),
            root.clone(),
            form.text.text(),
            form.kind,
        );
        if kind == Kind::Environment
            && let Err(error) = environments::from_text(&text)
        {
            form.error = Some(format!("{error:#}"));
            return;
        }
        let (tx, rx) = mpsc::channel();
        form.saving = Some(rx);
        form.error = None;
        let storage = self.storage.clone();
        std::thread::spawn(move || {
            let result = match kind {
                Kind::Rules => request(
                    &storage,
                    Request::RepositoryRules {
                        server,
                        repository,
                        rules: Some(text),
                    },
                ),
                Kind::Environment => environment_request(&storage, server, repository, Some(text)),
            };
            let _ = tx.send(result);
            crate::repaint::request();
        });
    }
    fn rules_saved(&mut self, result: Loaded) {
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        match result {
            Ok(_) => {
                let notice = match (&form.server, form.kind) {
                    (Some(server), Kind::Environment) => format!(
                        "Environment saved on {server}; new sessions use it and running ones were told"
                    ),
                    (None, Kind::Environment) => "Environment saved".into(),
                    (Some(server), Kind::Rules) => {
                        format!("Repository rules saved on {server} for new sessions")
                    }
                    (None, Kind::Rules) => "Repository rules saved for new sessions".into(),
                };
                self.close_rules();
                self.notice = Some((notice, false));
            }
            Err(error) => {
                form.error = Some(match form.kind {
                    Kind::Rules => format!("Cannot save rules: {error}"),
                    Kind::Environment => format!("Cannot save the environment: {error}"),
                })
            }
        }
    }
    pub(super) fn rules_option(&mut self, index: usize) {
        if let Some(Modal::RepositoryRules(form)) = &mut self.modal
            && let Some(path) = form.options().get(index).cloned()
        {
            form.load(self.storage.clone(), path);
        }
    }
    pub(super) fn rules_paste(&mut self, text: &str) {
        if let Some(Modal::RepositoryRules(form)) = &mut self.modal
            && !form.busy()
        {
            if form.field == 0 {
                form.repository.insert(text);
                form.selected = 0;
            } else if form.field == 1 && form.root.is_some() {
                form.text.insert(text);
            }
        }
    }
    pub(super) fn rules_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.close_rules();
            return;
        }
        let action = self.shortcut(key);
        if action == Some(RULES_SAVE) {
            self.save_rules();
            return;
        }
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        if form.busy() {
            return;
        }
        match action {
            Some(NAVIGATION_ACTIVATE) => match form.field {
                0 => {
                    let path = form
                        .options()
                        .get(form.selected)
                        .cloned()
                        .unwrap_or_else(|| PathBuf::from(form.repository.text()));
                    form.load(self.storage.clone(), path);
                }
                2 => self.save_rules(),
                3 => self.close_rules(),
                _ => {}
            },
            Some(RULES_REPOSITORY_PREVIOUS | RULES_REPOSITORY_NEXT) => {
                form.selected = if action == Some(RULES_REPOSITORY_PREVIOUS) {
                    form.selected.saturating_sub(1)
                } else {
                    (form.selected + 1).min(form.options().len().saturating_sub(1))
                };
            }
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                form.field = (form.field
                    + if action == Some(NAVIGATION_NEXT_FOCUS) {
                        1
                    } else {
                        3
                    })
                    % 4;
            }
            _ if form.field == 0 => {
                form.repository.key(key);
                form.selected = 0;
            }
            _ if form.field == 1 && form.root.is_some() => form.text.key(key),
            _ => {}
        }
    }
    pub(super) fn draw_rules(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::RepositoryRules(form)) = &self.modal else {
            return;
        };
        let bindings = Bindings::new(&self.shortcut_config);
        let repository_label = format!(
            "{} · {}/{} select · {} opens",
            form.server
                .as_ref()
                .map(|server| format!("Repository on {server}"))
                .unwrap_or_else(|| "Repository".into()),
            bindings.primary_hint_in(RULES_REPOSITORY_PREVIOUS, AGENT_RULES_REPOSITORY),
            bindings.primary_hint_in(RULES_REPOSITORY_NEXT, AGENT_RULES_REPOSITORY),
            bindings.primary_hint_in(NAVIGATION_ACTIVATE, AGENT_RULES_REPOSITORY)
        );
        let save_label = format!(
            "Save · {}",
            bindings.primary_hint_in(RULES_SAVE, self.shortcut_context())
        );
        let repository = Rect::new(area.x, area.y, area.width, 3).intersection(area);
        editor(
            frame,
            repository,
            &repository_label,
            &form.repository,
            form.field == 0,
        );
        self.hits.push((repository, Action::RulesField(0)));
        let options = form.options();
        let count = if form.field == 0 {
            options.len().min(5).min(usize::from(area.height / 3))
        } else {
            0
        };
        let start = form.selected.saturating_sub(count.saturating_sub(1));
        for (index, path) in options.iter().enumerate().skip(start).take(count) {
            let rect = Rect::new(area.x, area.y + 3 + (index - start) as u16, area.width, 1)
                .intersection(area);
            frame.render_widget(
                Paragraph::new(path.to_string_lossy().into_owned())
                    .style(crate::ui::option_style(index == form.selected)),
                rect,
            );
            self.hits.push((rect, Action::RulesOption(index)));
        }
        let top = area.y + 3 + count as u16;
        let text = Rect::new(
            area.x,
            top,
            area.width,
            area.bottom().saturating_sub(top + 3),
        )
        .intersection(area);
        editor(
            frame,
            text,
            match form.kind {
                Kind::Rules => "Rules",
                Kind::Environment => "Environment · [tools] [setup] [env] [notes]",
            },
            &form.text,
            form.field == 1 && form.root.is_some(),
        );
        self.hits.push((text, Action::RulesField(1)));
        let footer = area.bottom().saturating_sub(3);
        frame.render_widget(
            Paragraph::new(match (&form.server, form.kind) {
                (Some(server), Kind::Environment) => format!(
                    "Installed and run in new sessions' sandboxes on {server}; agents read and extend it with difu_environment. Never secrets."
                ),
                (None, Kind::Environment) => {
                    "Installed and run in new sessions' sandboxes; agents read and extend it with difu_environment. Never secrets.".into()
                }
                (Some(server), Kind::Rules) => format!(
                    "Included in the initial instructions for new sessions in this repository on {server}."
                ),
                (None, Kind::Rules) => {
                    "Included in the initial instructions for new sessions in this repository."
                        .into()
                }
            })
            .style(Style::default().fg(DIM)),
            Rect::new(area.x, footer, area.width, 1),
        );
        for (field, label) in [(2, save_label.as_str()), (3, "Cancel · Esc")] {
            let rect = Rect::new(
                area.x + (field - 2) as u16 * (area.width / 2),
                footer + 1,
                area.width / 2,
                1,
            );
            frame.render_widget(
                Paragraph::new(label).style(crate::ui::option_style(form.field == field)),
                rect,
            );
            self.hits.push((
                rect,
                if field == 2 {
                    Action::RulesSave
                } else {
                    Action::RulesCancel
                },
            ));
        }
        if let Some(error) = &form.error {
            frame.render_widget(
                Paragraph::new(error.as_str()).style(Style::default().fg(RED)),
                Rect::new(area.x, footer + 2, area.width, 1),
            );
        } else if form.busy() {
            frame.render_widget(
                Paragraph::new(match (form.saving.is_some(), form.kind) {
                    (true, Kind::Rules) => "Saving repository rules…",
                    (false, Kind::Rules) => "Loading repository rules…",
                    (true, Kind::Environment) => "Saving the environment…",
                    (false, Kind::Environment) => "Loading the environment…",
                })
                .style(Style::default().fg(DIM)),
                Rect::new(area.x, footer + 2, area.width, 1),
            );
        }
    }
}

/// A Git repository to keep rules for: saving resolves the repository root
/// exactly like the service does.
/// A stand-in agent service for the rules modal's requests.
#[cfg(test)]
pub(super) mod service {
    use super::*;
    use anyhow::Result;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    /// A Git repository whose rules the real handler can resolve.
    pub(in super::super) fn repository() -> Result<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("repository");
        std::fs::create_dir(&root)?;
        crate::process::checked(
            crate::repo::git(&root).args(["init", "-q", "-b", "main"]),
            &crate::process::Cancel::default(),
        )?;
        let root = root.canonicalize()?;
        Ok((dir, root))
    }

    pub(in super::super) struct Service {
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<Vec<Request>>>,
    }

    impl Service {
        /// Answers `Request::RepositoryRules` on the UI's socket: `real` uses
        /// the agent service's handler, otherwise the rules are echoed back.
        pub(in super::super) fn start(storage: &Storage, real: bool) -> Result<Self> {
            let listener = UnixListener::bind(super::super::super::server::socket(storage)?)?;
            listener.set_nonblocking(true)?;
            let storage = storage.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                while !stopping.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Ok(request) = answer(&storage, real, stream) {
                                requests.push(request);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
                requests
            });
            Ok(Self {
                stop,
                handle: Some(handle),
            })
        }
        pub(in super::super) fn requests(mut self) -> Result<Vec<Request>> {
            self.stop.store(true, Ordering::Relaxed);
            self.handle
                .take()
                .map(|handle| handle.join())
                .transpose()
                .map_err(|_| anyhow::anyhow!("rules service panicked"))
                .map(Option::unwrap_or_default)
        }
    }

    impl Drop for Service {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn answer(storage: &Storage, real: bool, mut stream: UnixStream) -> Result<Request> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: Request = serde_json::from_str(&line)?;
        let reply = match &request {
            Request::RepositoryRules {
                repository, rules, ..
            } if real => super::super::super::rules::request(storage, repository, rules.clone())
                .unwrap_or_else(|error| Reply::Error(format!("{error:#}"))),
            Request::RepositoryRules {
                repository, rules, ..
            } => Reply::RepositoryRules {
                root: repository.clone(),
                rules: rules.clone().unwrap_or_else(|| "server rule".into()),
            },
            Request::Environment {
                repository,
                profile,
                ..
            } => Reply::Environment(crate::cloud::environments::View {
                repository: repository.clone(),
                profile: profile.clone().unwrap_or_else(|| {
                    let mut profile = crate::cloud::environments::Profile::default();
                    profile.tools.insert("python".into(), "3.12".into());
                    profile
                }),
                detected: vec![crate::cloud::environments::Detected {
                    tool: "node".into(),
                    version: "24".into(),
                    source: ".nvmrc".into(),
                    corepack: false,
                }],
            }),
            _ => Reply::Error("Unexpected request".into()),
        };
        serde_json::to_writer(&mut stream, &reply)?;
        stream.write_all(b"\n")?;
        Ok(request)
    }

    /// Waits for the modal's pending load or save.
    pub(in super::super) fn settle(ui: &mut Ui) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(Modal::RepositoryRules(form)) = &ui.modal
            && form.busy()
        {
            ui.tick_rules();
            anyhow::ensure!(Instant::now() < deadline, "rules request timed out");
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::service::{Service, repository, settle};
    use super::*;
    use anyhow::Result;

    #[test]
    fn multiline_rules_save_cancel_and_clear_without_changing_other_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (_repository, root) = repository()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let mut config = Config::default();
        config.pinned_sessions.insert("pinned".into());
        storage.save_config(&config)?;
        let _service = Service::start(&storage, true)?;
        let mut ui = Ui::new(storage.clone(), &config);
        let form = |text: &str| {
            Modal::RepositoryRules(Box::new(Settings {
                repository: Editor::from(root.to_string_lossy().as_ref()),
                text: Editor::from(text),
                field: 1,
                root: Some(root.clone()),
                saved: text.into(),
                candidates: vec![root.clone()],
                selected: 0,
                loading: None,
                error: None,
                return_to: None,
                server: None,
                saving: None,
                kind: Kind::Rules,
            }))
        };
        ui.modal = Some(form(""));
        ui.rules_paste("First rule");
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.rules_paste("Second rule");
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER));
        settle(&mut ui)?;
        assert!(ui.modal.is_none());
        let config = storage.load_config()?;
        assert_eq!(
            config.repository_rules.get(&root).map(String::as_str),
            Some("First rule\nSecond rule")
        );
        assert!(config.pinned_sessions.contains("pinned"));
        ui.modal = Some(form("First rule\nSecond rule"));
        ui.rules_paste("Unsaved");
        ui.rules_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            storage.load_config()?.repository_rules,
            config.repository_rules
        );
        ui.modal = Some(form(""));
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        settle(&mut ui)?;
        assert!(storage.load_config()?.repository_rules.is_empty());
        Ok(())
    }

    #[test]
    fn a_server_sessions_rules_are_read_and_saved_on_its_server() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let session = crate::cloud::federation::tests::session("hetzner:1");
        let mut summary = session.summary();
        summary.server = Some("hetzner".into());
        ui.summaries.push(summary);
        ui.sessions.insert(session.id.clone(), session);
        ui.selected = Some("hetzner:1".into());
        let service = Service::start(&ui.storage, false)?;
        ui.open_rules(None);
        settle(&mut ui)?;
        let Some(Modal::RepositoryRules(form)) = &mut ui.modal else {
            anyhow::bail!("rules");
        };
        assert_eq!(form.server.as_deref(), Some("hetzner"));
        assert_eq!(form.text.text(), "server rule");
        assert_eq!(
            form.candidates,
            vec![PathBuf::from("/srv/difu/repos/acme/api")],
            "only that server's repositories"
        );
        form.text = Editor::from("Run only the affected tests");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 44))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Repository on hetzner"), "{text}");
        ui.save_rules();
        settle(&mut ui)?;
        assert!(ui.modal.is_none());
        assert_eq!(
            ui.notice.as_ref().map(|(n, _)| n.as_str()),
            Some("Repository rules saved on hetzner for new sessions")
        );
        let requests = service.requests()?;
        let [load, save] = requests.as_slice() else {
            anyhow::bail!("two requests: {requests:?}");
        };
        assert!(matches!(
            load,
            Request::RepositoryRules { server: Some(s), rules: None, repository }
                if s == "hetzner" && repository == std::path::Path::new("/srv/difu/repos/acme/api")
        ));
        assert!(matches!(
            save,
            Request::RepositoryRules { server: Some(s), rules: Some(r), .. }
                if s == "hetzner" && r == "Run only the affected tests"
        ));
        assert!(ui.storage.load_config()?.repository_rules.is_empty());
        Ok(())
    }

    #[test]
    fn local_rules_are_read_and_saved_with_the_service_request() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let (_repository, root) = repository()?;
        ui.storage.update_config(|config| {
            config
                .repository_rules
                .insert(root.clone(), "local rule".into());
        })?;
        let service = Service::start(&ui.storage, true)?;
        ui.open_rules(Some(root.clone()));
        settle(&mut ui)?;
        let Some(Modal::RepositoryRules(form)) = &mut ui.modal else {
            anyhow::bail!("rules");
        };
        assert_eq!(form.server, None);
        assert_eq!(form.text.text(), "local rule");
        form.text = Editor::default();
        ui.save_rules();
        settle(&mut ui)?;
        assert!(ui.modal.is_none());
        assert!(
            !ui.storage
                .load_config()?
                .repository_rules
                .contains_key(&root),
            "empty rules remove the entry"
        );
        let requests = service.requests()?;
        assert!(
            requests
                .iter()
                .all(|r| matches!(r, Request::RepositoryRules { server: None, .. })),
            "{requests:?}"
        );
        assert_eq!(requests.len(), 2);
        Ok(())
    }
}

#[cfg(test)]
mod shortcut_regressions {
    use super::service::{Service, repository, settle};
    use super::*;
    use anyhow::{Context, Result};
    /// The one save request a key press sent.
    fn saved(service: Service, root: &std::path::Path) -> Result<()> {
        let requests = service.requests()?;
        let [
            Request::RepositoryRules {
                server: None,
                repository,
                rules: Some(rules),
            },
        ] = requests.as_slice()
        else {
            anyhow::bail!("expected one save: {requests:?}");
        };
        anyhow::ensure!(repository == root && rules == "saved rule", "{requests:?}");
        Ok(())
    }
    fn setup(ui: &mut Ui, root: &std::path::Path, field: usize) {
        ui.modal = Some(Modal::RepositoryRules(Box::new(Settings {
            repository: Editor::from(root.to_string_lossy().as_ref()),
            text: Editor::from("saved rule"),
            field,
            root: Some(root.into()),
            saved: "saved rule".into(),
            candidates: vec![root.join("one"), root.join("two")],
            selected: 0,
            loading: None,
            error: None,
            return_to: None,
            server: None,
            saving: None,
            kind: Kind::Rules,
        })));
    }
    fn form(ui: &Ui) -> Result<&Settings> {
        let Some(Modal::RepositoryRules(f)) = &ui.modal else {
            anyhow::bail!("rules closed")
        };
        Ok(f)
    }
    #[test]
    fn server_environments_load_as_text_save_parsed_and_return_to_cloud() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let config = Config::default();
        storage.save_config(&config)?;
        let service = Service::start(&storage, false)?;
        let mut ui = Ui::new(storage.clone(), &config);
        ui.open_environment("hetzner".into(), vec!["acme/web".into(), "acme/api".into()]);
        settle(&mut ui)?;
        let loaded = form(&ui)?;
        assert!(loaded.environment());
        assert_eq!(
            loaded.root.as_deref(),
            Some(std::path::Path::new("acme/api"))
        );
        let text = loaded.text.text();
        assert!(
            text.contains("# acme/api declares on its main branch: node 24 (.nvmrc)."),
            "{text}"
        );
        assert!(text.contains("[tools]\npython = 3.12\n"), "{text}");
        // An invalid profile is explained before anything is sent.
        if let Some(Modal::RepositoryRules(form)) = &mut ui.modal {
            form.text = Editor::from("[env]\nNPM_TOKEN = abc\n");
        }
        ui.save_rules();
        let refused = form(&ui)?;
        assert!(refused.saving.is_none());
        assert!(
            refused
                .error
                .as_deref()
                .is_some_and(|error| error.contains("looks like a secret")),
            "{:?}",
            refused.error
        );
        if let Some(Modal::RepositoryRules(form)) = &mut ui.modal {
            form.text = Editor::from("[tools]\nnode = 24\n\n[setup]\npnpm install\n");
        }
        ui.save_rules();
        settle(&mut ui)?;
        assert!(matches!(ui.modal, Some(Modal::Cloud)));
        assert!(
            ui.notice.as_ref().is_some_and(
                |(text, error)| !error && text.contains("Environment saved on hetzner")
            ),
            "{:?}",
            ui.notice
        );
        // No repositories: explained, and Esc returns to /cloud.
        ui.open_environment("hetzner".into(), Vec::new());
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|error| error.contains("no repositories"))
        );
        ui.rules_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(ui.modal, Some(Modal::Cloud)));
        let requests = service.requests()?;
        anyhow::ensure!(
            requests.iter().any(|request| matches!(
                request,
                Request::Environment {
                    server: Some(server),
                    repository,
                    profile: Some(profile),
                    ..
                } if server == "hetzner"
                    && repository == "acme/api"
                    && profile.setup == ["pnpm install"]
                    && profile.tools.get("node").map(String::as_str) == Some("24")
            )),
            "{requests:?}"
        );
        Ok(())
    }

    #[test]
    fn rules_focus_navigation_and_repository_selection_are_contextual() -> Result<()> {
        for field in 0..4 {
            for (action, code, mods, want) in [
                (
                    NAVIGATION_NEXT_FOCUS,
                    KeyCode::Tab,
                    KeyModifiers::NONE,
                    (field + 1) % 4,
                ),
                (
                    NAVIGATION_PREVIOUS_FOCUS,
                    KeyCode::BackTab,
                    KeyModifiers::SHIFT,
                    (field + 3) % 4,
                ),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    setup(&mut ui, dir.path(), field);
                    if custom {
                        ui.key(KeyEvent::new(code, mods));
                        assert_eq!(form(&ui)?.field, field);
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        if custom { KeyModifiers::NONE } else { mods },
                    ));
                    assert_eq!(form(&ui)?.field, want);
                }
            }
        }
        for (action, code, start, want) in [
            (RULES_REPOSITORY_PREVIOUS, KeyCode::Up, 1, 0),
            (RULES_REPOSITORY_NEXT, KeyCode::Down, 0, 1),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                setup(&mut ui, dir.path(), 0);
                let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
                    anyhow::bail!("rules")
                };
                f.repository = Editor::default();
                f.selected = start;
                if custom {
                    ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                    assert_eq!(form(&ui)?.selected, 0, "removed navigation remains native");
                    if let Some(Modal::RepositoryRules(f)) = &mut ui.modal {
                        f.selected = start;
                    }
                }
                ui.key(KeyEvent::new(
                    if custom { KeyCode::F(12) } else { code },
                    KeyModifiers::NONE,
                ));
                assert_eq!(form(&ui)?.selected, want);
                let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
                    anyhow::bail!("rules")
                };
                f.field = 1;
                assert_eq!(ui.shortcut_context(), AGENT_RULES);
                assert!(
                    !crate::shortcuts::Bindings::new(&ui.shortcut_config).matches(
                        action,
                        ui.shortcut_context(),
                        KeyEvent::new(
                            if custom { KeyCode::F(12) } else { code },
                            KeyModifiers::NONE
                        )
                    )
                );
            }
        }
        Ok(())
    }
    #[test]
    fn rules_save_predicate_replaces_all_control_or_super_combinations() -> Result<()> {
        for field in 0..4 {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                for bits in 1u8..64 {
                    if bits & 9 == 0 {
                        continue;
                    }
                    let (mut ui, dir, _) = shortcut_tests::binding_fixture(RULES_SAVE, phase)?;
                    let root = dir.path();
                    let service = Service::start(&ui.storage, false)?;
                    setup(&mut ui, root, field);
                    let mut mods = KeyModifiers::NONE;
                    for (bit, modifier) in [
                        (1, KeyModifiers::CONTROL),
                        (2, KeyModifiers::ALT),
                        (4, KeyModifiers::SHIFT),
                        (8, KeyModifiers::SUPER),
                        (16, KeyModifiers::HYPER),
                        (32, KeyModifiers::META),
                    ] {
                        if bits & bit != 0 {
                            mods |= modifier;
                        }
                    }
                    if custom {
                        ui.key(KeyEvent::new(KeyCode::Enter, mods));
                        assert!(ui.modal.is_some());
                        assert!(form(&ui)?.saving.is_none(), "old binding saved");
                        setup(&mut ui, root, field);
                    }
                    ui.key(KeyEvent::new(
                        if custom {
                            KeyCode::F(12)
                        } else {
                            KeyCode::Enter
                        },
                        if custom { KeyModifiers::NONE } else { mods },
                    ));
                    settle(&mut ui)?;
                    assert!(ui.modal.is_none(), "{field} {phase} {mods:?}");
                    saved(service, root)?;
                }
            }
        }
        Ok(())
    }
    #[test]
    fn rules_activation_keeps_multiline_input_and_repository_and_dirty_guards() -> Result<()> {
        for field in [0, 1, 2, 3] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, dir, _) = shortcut_tests::binding_fixture(NAVIGATION_ACTIVATE, phase)?;
                let service = Service::start(&ui.storage, false)?;
                setup(&mut ui, dir.path(), field);
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    assert!(ui.modal.is_some());
                    assert!(form(&ui)?.loading.is_none());
                    setup(&mut ui, dir.path(), field);
                }
                ui.key(KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Enter
                    },
                    KeyModifiers::NONE,
                ));
                match field {
                    0 => assert!(form(&ui)?.loading.is_some()),
                    1 => {
                        assert_eq!(
                            form(&ui)?.text.text(),
                            if custom { "saved rule" } else { "saved rule\n" }
                        );
                        assert!(ui.storage.load_config()?.repository_rules.is_empty());
                    }
                    2 => {
                        settle(&mut ui)?;
                        assert!(ui.modal.is_none());
                        saved(service, dir.path())?;
                    }
                    3 => {
                        assert!(ui.modal.is_none());
                        assert!(ui.storage.load_config()?.repository_rules.is_empty());
                    }
                    _ => anyhow::bail!("field"),
                }
            }
        }
        let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
        setup(&mut ui, dir.path(), 0);
        let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
            anyhow::bail!("rules")
        };
        f.text.insert("unsaved");
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(form(&ui)?.loading.is_none());
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Save or cancel"))
        );
        assert_eq!(form(&ui)?.text.text(), "saved ruleunsaved");
        let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
            anyhow::bail!("rules")
        };
        f.repository.insert("changed");
        ui.save_rules();
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("select the repository"))
        );
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("F12")),
            "recovery uses the actual activation binding"
        );
        Ok(())
    }
    #[test]
    fn rules_save_failure_loading_and_escape_keep_native_safety() -> Result<()> {
        let (mut ui, dir, _) = shortcut_tests::fixture(Some(RULES_SAVE))?;
        let (_repository, root) = repository()?;
        let _service = Service::start(&ui.storage, true)?;
        setup(&mut ui, &root, 1);
        std::fs::write(&ui.storage.config, "{malformed")?;
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        settle(&mut ui)?;
        assert_eq!(form(&ui)?.text.text(), "saved rule");
        assert!(form(&ui)?.error.is_some());
        assert_eq!(std::fs::read_to_string(&ui.storage.config)?, "{malformed");
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        setup(&mut ui, dir.path(), 1);
        let (_tx, rx) = mpsc::channel();
        let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
            anyhow::bail!("rules")
        };
        f.loading = Some(rx);
        f.root = None;
        ui.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(form(&ui)?.text.text(), "saved rule");
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(ui.modal.is_some());
        Ok(())
    }
    #[test]
    fn rules_fields_and_buttons_preserve_copy_early_globals_and_editing() -> Result<()> {
        for field in 0..4 {
            shortcut_tests::owned_modal_context(
                |ui| {
                    setup(ui, std::path::Path::new("/temporary/repository"), field);
                    if let Some(Modal::RepositoryRules(f)) = &mut ui.modal {
                        let editor = if field == 0 {
                            Some(&mut f.repository)
                        } else if field == 1 {
                            Some(&mut f.text)
                        } else {
                            None
                        };
                        if let Some(editor) = editor {
                            *editor = Editor::from("focused");
                            editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                        }
                    }
                },
                match field {
                    0 => AGENT_RULES_REPOSITORY,
                    1 => AGENT_RULES,
                    _ => AGENT_RULES_BUTTON,
                },
                (field < 2).then_some("d"),
            )?;
        }
        Ok(())
    }
    #[test]
    fn rules_native_hints_use_repository_navigation_focus_and_save_bindings() -> Result<()> {
        for width in [80, 160] {
            for action in [
                RULES_SAVE,
                RULES_REPOSITORY_PREVIOUS,
                RULES_REPOSITORY_NEXT,
                NAVIGATION_NEXT_FOCUS,
                NAVIGATION_ACTIVATE,
            ] {
                let (mut ui, dir, _) = shortcut_tests::fixture(Some(action))?;
                setup(&mut ui, dir.path(), 0);
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 44))?;
                terminal.draw(|frame| ui.draw(frame))?;
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("F12"), "{} {width}: {text}", action.0);
            }
        }
        Ok(())
    }

    #[test]
    fn rules_visible_save_cancel_and_blank_targets_survive_activation_remap() -> Result<()> {
        for width in [40, 80, 160] {
            let (_repository, root) = repository()?;
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(RULES_SAVE))?;
            let _service = Service::start(&ui.storage, true)?;
            setup(&mut ui, &root, 1);
            let click = shortcut_tests::native_target(&mut ui, width, "Save · F12")?;
            ui.mouse(click);
            settle(&mut ui)?;
            assert!(ui.modal.is_none());
            assert_eq!(
                ui.storage
                    .load_config()?
                    .repository_rules
                    .get(&root)
                    .map(String::as_str),
                Some("saved rule")
            );
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            setup(&mut ui, dir.path(), 1);
            let click = shortcut_tests::native_target(&mut ui, width, "Cancel · Esc")?;
            ui.mouse(click);
            assert!(ui.modal.is_none());
            assert!(ui.storage.load_config()?.repository_rules.is_empty());
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            setup(&mut ui, dir.path(), 1);
            let blank = shortcut_tests::native_blank_below(&mut ui, width, "Save · Ctrl")?;
            ui.mouse(blank);
            assert!(ui.modal.is_some());
            assert!(ui.storage.load_config()?.repository_rules.is_empty());
        }
        Ok(())
    }
    #[test]
    fn rules_visible_repository_option_loads_the_selected_temporary_repository() -> Result<()> {
        for width in [40, 80, 160] {
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            let root = dir.path().join("repository");
            std::fs::create_dir(&root)?;
            crate::process::checked(
                crate::repo::git(&root).args(["init", "-b", "main"]),
                &crate::process::Cancel::default(),
            )?;
            let root = root.canonicalize()?;
            ui.storage.update_config(|config| {
                config
                    .repository_rules
                    .insert(root.clone(), "selected repository rules".into());
            })?;
            let _service = Service::start(&ui.storage, true)?;
            setup(&mut ui, &root, 0);
            let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
                anyhow::bail!("rules")
            };
            f.candidates = vec![root.clone()];
            f.repository = Editor::default();
            // Long temporary paths are clipped on narrow screens; use the visible
            // prefix to locate the actual rendered option, not an invented rectangle.
            let label = root.to_string_lossy();
            let prefix = label.get(..16).context("path prefix")?;
            let click = shortcut_tests::native_target(&mut ui, width, prefix)?;
            ui.mouse(click);
            let deadline = Instant::now() + Duration::from_secs(3);
            while form(&ui)?.loading.is_some() {
                ui.tick_rules();
                anyhow::ensure!(Instant::now() < deadline, "rules load timeout");
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(form(&ui)?.root.as_ref(), Some(&root));
            assert_eq!(form(&ui)?.text.text(), "selected repository rules");
            assert_eq!(form(&ui)?.field, 1);
        }
        Ok(())
    }
}
