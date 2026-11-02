//! The new-session form. Every new coding session starts here: where it runs
//! (this Mac or a saved cloud server) first, then its repository, model and
//! reasoning, each prefilled from that location's saved defaults. Enter starts
//! at once, so the defaults stay one keystroke away; choosing another location
//! reloads the other lists from it.
use super::*;
use crate::{
    cloud::federation::ServerStatus,
    model::ModelInfo,
    shortcuts::{Bindings, contexts::AGENT_MODEL, ids::*},
    storage::AgentDefaults,
};
use provider::Provider;
use ratatui::style::Modifier;
use std::path::{Path, PathBuf};

/// The form asks the service for server state this often while open.
const REFRESH: Duration = Duration::from_secs(2);
pub(super) const LOCATION: usize = 0;
pub(super) const REPOSITORY: usize = 1;
pub(super) const MODEL: usize = 2;
pub(super) const EFFORT: usize = 3;
const LABELS: [&str; 4] = ["Location", "Repository", "Model", "Reasoning"];
/// Local repositories offered at most.
const CANDIDATES: usize = 40;

type Fetched = std::result::Result<Vec<ServerStatus>, String>;
type Inherited = std::result::Result<String, String>;

/// A row of the focused field's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    pub label: String,
    pub detail: String,
    /// What choosing the row sets. `None` is the location's default: This Mac,
    /// this directory's repository, or the provider's model or reasoning.
    pub value: Option<String>,
    pub enabled: bool,
}
impl Choice {
    fn new(label: impl Into<String>, detail: impl Into<String>, value: Option<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            value,
            enabled: true,
        }
    }
    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.label.to_lowercase().contains(query)
            || self
                .value
                .as_deref()
                .is_some_and(|value| value.to_lowercase().contains(query))
    }
}

/// The model a blank model inherits where the session would run, which
/// decides the reasoning levels offered for it.
struct Inheritance {
    location: Option<String>,
    directory: PathBuf,
    model: Option<Inherited>,
    receiver: Option<mpsc::Receiver<Inherited>>,
}

pub struct Form {
    /// Saved servers with their latest state, repositories and defaults.
    servers: Vec<ServerStatus>,
    /// Where the session runs; `None` is this Mac.
    pub(super) location: Option<String>,
    /// This Mac's saved defaults.
    local: AgentDefaults,
    /// Repositories on this Mac difu already knows.
    candidates: Vec<PathBuf>,
    cwd: Option<PathBuf>,
    pub field: usize,
    /// Filters the focused field's list.
    pub query: Editor,
    pub(super) repository: Option<String>,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    /// Fields edited since the location was chosen keep their value when the
    /// location's defaults arrive.
    touched: [bool; 4],
    loading: Option<mpsc::Receiver<Fetched>>,
    fetched: Option<Instant>,
    inherited: Option<Inheritance>,
    pub(super) error: Option<String>,
    pub(super) starting: bool,
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
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

impl Form {
    pub(super) fn new(
        local: AgentDefaults,
        candidates: Vec<PathBuf>,
        cwd: Option<PathBuf>,
        servers: Vec<ServerStatus>,
    ) -> Self {
        let mut form = Self {
            servers,
            location: None,
            local,
            candidates,
            cwd,
            field: LOCATION,
            query: Editor::default(),
            repository: None,
            model: None,
            effort: None,
            touched: [false; 4],
            loading: None,
            fetched: None,
            inherited: None,
            error: None,
            starting: false,
        };
        form.prefill();
        form
    }
    /// The chosen server's state; `None` on this Mac.
    pub(super) fn status(&self) -> Option<&ServerStatus> {
        let server = self.location.as_ref()?;
        self.servers.iter().find(|status| &status.name == server)
    }
    /// The defaults saved where the session would run.
    fn defaults(&self) -> AgentDefaults {
        match &self.location {
            None => self.local.clone(),
            Some(_) => self
                .status()
                .and_then(|status| status.defaults.clone())
                .unwrap_or_default(),
        }
    }
    /// The provider a model runs on here; a blank model keeps the location's.
    fn provider(&self, model: Option<&str>) -> Provider {
        match model {
            Some(model) => Provider::for_model(Some(model)),
            None => self.defaults().provider.unwrap_or_default(),
        }
    }
    /// The location's default repository; a server without a saved one offers
    /// its first ready repository.
    fn default_repository(&self) -> Option<String> {
        if self.location.is_none() {
            return self.local.repository.as_deref().map(path_text);
        }
        let status = self.status()?;
        let saved = status.defaults.as_ref().and_then(|d| d.repository.as_ref());
        status
            .repositories
            .iter()
            .find(|repository| Some(&repository.path) == saved)
            .or_else(|| status.repositories.iter().find(|r| r.cloned))
            .or_else(|| status.repositories.first())
            .map(|repository| path_text(&repository.path))
    }
    /// Fills the fields not edited since the location was chosen from its
    /// defaults.
    fn prefill(&mut self) {
        let defaults = self.defaults();
        if !self.touched[REPOSITORY] {
            self.repository = self.default_repository();
        }
        if !self.touched[MODEL] {
            self.model = defaults.model.clone();
        }
        if !self.touched[EFFORT] {
            self.effort = defaults.effort;
        }
    }
    fn relocate(&mut self, location: Option<String>) {
        if self.location == location {
            return;
        }
        self.location = location;
        self.touched = [false; 4];
        self.inherited = None;
        self.prefill();
    }
    fn receive(&mut self, result: Fetched) {
        match result {
            Ok(servers) => {
                self.servers = servers;
                if self.location.is_some() && self.status().is_none() {
                    self.relocate(None);
                } else {
                    self.prefill();
                }
            }
            Err(error) => {
                for status in &mut self.servers {
                    status.problem = Some(error.clone());
                }
            }
        }
    }
    fn value(&self, field: usize) -> Option<&String> {
        match field {
            LOCATION => self.location.as_ref(),
            REPOSITORY => self.repository.as_ref(),
            MODEL => self.model.as_ref(),
            _ => self.effort.as_ref(),
        }
    }
    fn choose(&mut self, field: usize, value: Option<String>) {
        self.error = None;
        match field {
            LOCATION => self.relocate(value),
            REPOSITORY => {
                self.repository = value;
                self.touched[REPOSITORY] = true;
            }
            MODEL => {
                // Reasoning levels belong to a provider.
                if self.provider(value.as_deref()) != self.provider(self.model.as_deref()) {
                    self.effort = None;
                    self.touched[EFFORT] = true;
                }
                self.model = value;
                self.touched[MODEL] = true;
            }
            _ => {
                self.effort = value;
                self.touched[EFFORT] = true;
            }
        }
    }
    fn focus(&mut self, field: usize) {
        self.field = field;
        self.query = Editor::default();
    }
    /// Custom values are typed paths on this Mac, and models or levels the
    /// catalog does not list; server repositories come only from the server.
    fn accepts_custom(&self, field: usize) -> bool {
        field != LOCATION && (field != REPOSITORY || self.location.is_none())
    }
    /// The focused field's rows matching the filter.
    pub(super) fn choices(&self, field: usize, catalog: Option<&[ModelInfo]>) -> Vec<Choice> {
        let mut choices = match field {
            LOCATION => self.location_choices(),
            REPOSITORY => self.repository_choices(),
            MODEL => self.model_choices(catalog),
            _ => self.effort_choices(catalog),
        };
        let typed = self.query.text().trim().to_owned();
        let query = typed.to_lowercase();
        if !self.accepts_custom(field) {
            choices.retain(|choice| choice.matches(&query));
            return choices;
        }
        // A custom path or model chosen earlier stays listed.
        if let Some(value) = self.value(field)
            && !choices.iter().any(|c| c.value.as_ref() == Some(value))
            && *value != typed
        {
            choices.push(Choice::new(
                value.clone(),
                "your choice",
                Some(value.clone()),
            ));
        }
        choices.retain(|choice| choice.matches(&query));
        if !typed.is_empty()
            && !choices
                .iter()
                .any(|choice| choice.value.as_deref() == Some(typed.as_str()))
        {
            let detail = match field {
                REPOSITORY => "use this path",
                MODEL => "custom model",
                _ => "custom level",
            };
            choices.push(Choice::new(typed.clone(), detail, Some(typed)));
        }
        choices
    }
    fn location_choices(&self) -> Vec<Choice> {
        let mut choices = vec![Choice::new(
            "This Mac",
            match &self.local.repository {
                Some(path) => format!("default repository {}", path.display()),
                None => "this directory's repository".into(),
            },
            None,
        )];
        choices.extend(self.servers.iter().map(|status| Choice {
            label: status.name.clone(),
            detail: server_detail(status),
            value: Some(status.name.clone()),
            enabled: status.problem.is_none(),
        }));
        choices
    }
    fn repository_choices(&self) -> Vec<Choice> {
        if self.location.is_some() {
            let Some(status) = self.status() else {
                return Vec::new();
            };
            let saved = status.defaults.as_ref().and_then(|d| d.repository.as_ref());
            return status
                .repositories
                .iter()
                .map(|repository| Choice {
                    label: repository.name.clone(),
                    detail: if !repository.cloned {
                        "cloning…".into()
                    } else if Some(&repository.path) == saved {
                        "default".into()
                    } else {
                        String::new()
                    },
                    value: Some(path_text(&repository.path)),
                    enabled: repository.cloned,
                })
                .collect();
        }
        let mut choices = Vec::new();
        if self.local.repository.is_none() {
            choices.push(Choice::new(
                "This directory",
                self.cwd.as_deref().map(path_text).unwrap_or_default(),
                None,
            ));
        }
        choices.extend(self.candidates.iter().map(|path| {
            Choice::new(
                path_text(path),
                if self.local.repository.as_ref() == Some(path) {
                    "default"
                } else {
                    ""
                },
                Some(path_text(path)),
            )
        }));
        choices
    }
    fn model_choices(&self, catalog: Option<&[ModelInfo]>) -> Vec<Choice> {
        let defaults = self.defaults();
        let mut choices = vec![Choice::new(
            "Default",
            match self.provider(None) {
                Provider::Codex => "Codex's configured model",
                Provider::Claude => "Claude Code's default model",
            },
            None,
        )];
        for model in catalog.unwrap_or_default() {
            let name = crate::model::clean(&model.name);
            let mut detail = crate::model::clean(&model.id);
            if defaults.model.as_ref() == Some(&model.id) {
                detail.push_str(" · default");
            }
            choices.push(Choice::new(
                if name.is_empty() {
                    model.id.clone()
                } else {
                    name
                },
                detail,
                Some(model.id.clone()),
            ));
        }
        if let Some(model) = &defaults.model
            && !choices.iter().any(|c| c.value.as_ref() == Some(model))
        {
            choices.push(Choice::new(model.clone(), "default", Some(model.clone())));
        }
        choices
    }
    fn effort_choices(&self, catalog: Option<&[ModelInfo]>) -> Vec<Choice> {
        let defaults = self.defaults();
        let model = self.model.clone().or_else(|| self.inherited_model());
        let efforts = catalog
            .zip(model)
            .and_then(|(catalog, model)| catalog.iter().find(|m| m.id == model))
            .map(|model| model.efforts.clone())
            .unwrap_or_default();
        let mut choices = vec![Choice::new("Default", "the model's default", None)];
        for effort in efforts {
            let detail = if defaults.effort.as_ref() == Some(&effort) {
                "default"
            } else {
                ""
            };
            choices.push(Choice::new(effort.clone(), detail, Some(effort)));
        }
        // The saved level belongs to the saved model.
        if self.model == defaults.model
            && let Some(effort) = &defaults.effort
            && !choices.iter().any(|c| c.value.as_ref() == Some(effort))
        {
            choices.push(Choice::new(effort.clone(), "default", Some(effort.clone())));
        }
        choices
    }
    fn inherited_model(&self) -> Option<String> {
        self.inherited
            .as_ref()
            .filter(|inherited| inherited.location == self.location)
            .and_then(|inherited| inherited.model.clone())
            .and_then(Result::ok)
    }
    fn selected(&self, choices: &[Choice]) -> Option<usize> {
        let value = self.value(self.field);
        choices
            .iter()
            .position(|choice| choice.value.as_ref() == value)
    }
    fn step(&mut self, down: bool, catalog: Option<&[ModelInfo]>) {
        let choices = self.choices(self.field, catalog);
        let last = choices.len().saturating_sub(1);
        let index = match self.selected(&choices) {
            Some(index) if down => (index + 1).min(last),
            Some(index) => index.saturating_sub(1),
            None => 0,
        };
        if let Some(choice) = choices.get(index) {
            self.choose(self.field, choice.value.clone());
        }
    }
    /// Typing searches: the first matching row is chosen.
    fn filtered(&mut self, catalog: Option<&[ModelInfo]>) {
        if let Some(choice) = self.choices(self.field, catalog).first() {
            let value = choice.value.clone();
            self.choose(self.field, value);
        }
    }
    /// Asks the location which model a blank model inherits there, so its
    /// reasoning levels can be offered.
    fn inherit(&mut self, storage: &Storage) {
        if let Some(inherited) = &mut self.inherited
            && let Some(result) = inherited
                .receiver
                .as_ref()
                .and_then(|rx| rx.try_recv().ok())
        {
            inherited.receiver = None;
            inherited.model = Some(result);
            crate::repaint::request();
        }
        if self.field != EFFORT || self.model.is_some() || self.provider(None) != Provider::Codex {
            return;
        }
        let directory = match (&self.location, &self.repository, &self.cwd) {
            (_, Some(path), _) => PathBuf::from(path),
            (None, None, Some(cwd)) => cwd.clone(),
            _ => return,
        };
        if self
            .inherited
            .as_ref()
            .is_some_and(|i| i.location == self.location && i.directory == directory)
        {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let storage = storage.clone();
        let request = Request::Defaults {
            cwd: directory.clone(),
            server: self.location.clone(),
        };
        thread::spawn(move || {
            let result = match client::request(&storage, request) {
                Ok(Reply::Defaults { model, .. }) => Ok(model),
                Ok(_) => Err("Unexpected defaults reply".into()),
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = tx.send(result);
        });
        self.inherited = Some(Inheritance {
            location: self.location.clone(),
            directory,
            model: None,
            receiver: Some(rx),
        });
    }
    /// What a field shows when its list is closed.
    fn display(&self, field: usize, catalog: Option<&[ModelInfo]>) -> String {
        match field {
            LOCATION => match (&self.location, self.status()) {
                (None, _) => "This Mac".into(),
                (
                    Some(name),
                    Some(ServerStatus {
                        problem: Some(problem),
                        ..
                    }),
                ) => format!("{name} · {problem}"),
                (Some(name), _) => name.clone(),
            },
            REPOSITORY => match (&self.location, &self.repository) {
                (None, None) => format!(
                    "This directory · {}",
                    self.cwd.as_deref().map(path_text).unwrap_or_default()
                ),
                (Some(_), Some(path)) => self
                    .status()
                    .and_then(|status| {
                        status
                            .repositories
                            .iter()
                            .find(|r| path_text(&r.path) == *path)
                    })
                    .map(|repository| repository.name.clone())
                    .unwrap_or_else(|| path.clone()),
                (Some(_), None) => "No repository yet".into(),
                (None, Some(path)) => path.clone(),
            },
            MODEL => match &self.model {
                None => "Default".into(),
                Some(id) => catalog
                    .and_then(|catalog| catalog.iter().find(|m| &m.id == id))
                    .filter(|model| !model.name.is_empty())
                    .map(|model| format!("{} · {id}", model.name))
                    .unwrap_or_else(|| id.clone()),
            },
            _ => self.effort.clone().unwrap_or_else(|| "Default".into()),
        }
    }
    /// The launch request, or why the session cannot start yet.
    fn request(&self) -> std::result::Result<Request, String> {
        let mut defaults = self.defaults();
        let provider = defaults.resolved_provider();
        defaults.model = self.model.clone();
        defaults.effort = self.effort.clone();
        if let Some(model) = &self.model {
            let chosen = Provider::for_model(Some(model));
            if chosen != provider {
                defaults.starting_mode = None;
            }
            defaults.provider = Some(chosen);
        }
        let Some(server) = &self.location else {
            defaults.repository = self.repository.as_deref().map(PathBuf::from);
            let cwd = std::env::current_dir()
                .map_err(|error| format!("Cannot read current directory: {error}"))?;
            return Ok(Request::NewAgent {
                defaults,
                cwd,
                remember_repository: false,
                server: None,
            });
        };
        let status = self
            .status()
            .ok_or_else(|| format!("{server} is no longer saved"))?;
        if let Some(problem) = &status.problem {
            return Err(format!("{server}: {problem}"));
        }
        let Some(path) = &self.repository else {
            return Err(if status.repositories.is_empty() {
                format!(
                    "No repositories on {server} yet. Add one there with `difu repos add owner/name`."
                )
            } else {
                format!("Choose a repository on {server}")
            });
        };
        if let Some(repository) = status
            .repositories
            .iter()
            .find(|repository| path_text(&repository.path) == *path)
            && !repository.cloned
        {
            return Err(format!(
                "{} is still being cloned on {server}",
                repository.name
            ));
        }
        defaults.repository = Some(PathBuf::from(path));
        Ok(Request::NewAgent {
            defaults,
            cwd: PathBuf::new(),
            remember_repository: false,
            server: Some(server.clone()),
        })
    }
    pub(super) fn failed(&mut self, error: &str) {
        self.starting = false;
        self.error = Some(error.to_owned());
    }
}

impl Ui {
    /// Opens the new-session form on this Mac with its saved defaults.
    pub(super) fn open_new_session(&mut self) {
        let config = self.storage.load_config().ok();
        let local = config
            .as_ref()
            .map(|config| config.agent_defaults.clone())
            .unwrap_or_else(|| self.defaults.clone());
        let candidates = self.local_repositories(config.as_ref(), &local);
        let servers = crate::cloud::servers::load(&self.storage)
            .unwrap_or_default()
            .into_iter()
            .map(|server| ServerStatus {
                defaults: self.location_defaults.get(&server.name).cloned(),
                name: server.name,
                hosts: server.hosts,
                problem: Some("Connecting…".into()),
                ..ServerStatus::default()
            })
            .collect();
        let form = Form::new(local, candidates, std::env::current_dir().ok(), servers);
        self.modal = Some(Modal::NewSession(Box::new(form)));
        self.model_completion.retry();
        self.tick_new_session();
    }
    /// Repositories on this Mac difu knows: the default, recent sessions',
    /// review clones and local diff repositories.
    fn local_repositories(&self, config: Option<&Config>, local: &AgentDefaults) -> Vec<PathBuf> {
        let mut recent: Vec<_> = self
            .summaries
            .iter()
            .filter(|summary| summary.server.is_none())
            .filter_map(|summary| summary.repository.clone().map(|r| (summary.updated, r)))
            .collect();
        recent.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
        let mut seen = HashSet::new();
        local
            .repository
            .iter()
            .cloned()
            .chain(recent.into_iter().map(|(_, path)| path))
            .chain(config.into_iter().flat_map(|config| {
                config
                    .repositories
                    .values()
                    .cloned()
                    .chain(config.local_diff_repositories.iter().cloned())
            }))
            .filter(|path| seen.insert(path.clone()))
            .filter(|path| path.is_dir())
            .take(CANDIDATES)
            .collect()
    }
    /// Polls the service for server state while the form is open and loads
    /// the chosen location's models once a model field is focused.
    pub(super) fn tick_new_session(&mut self) {
        let Some(Modal::NewSession(form)) = &mut self.modal else {
            return;
        };
        if let Some(result) = form.loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
            form.loading = None;
            crate::repaint::request();
            form.receive(result);
            for status in &form.servers {
                if let Some(defaults) = &status.defaults {
                    self.location_defaults
                        .insert(status.name.clone(), defaults.clone());
                }
            }
        }
        if !form.servers.is_empty()
            && form.loading.is_none()
            && form.fetched.is_none_or(|at| at.elapsed() >= REFRESH)
        {
            form.fetched = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            form.loading = Some(rx);
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
        // Discovery starts real provider processes, so only once asked for.
        if matches!(form.field, MODEL | EFFORT) {
            self.model_completion
                .ensure(&self.storage, form.location.as_deref());
        }
        form.inherit(&self.storage);
    }
    pub(super) fn new_session_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let shortcut = self.shortcut(key);
        if shortcut == Some(NAVIGATION_ACTIVATE) {
            self.start_new_session();
            return;
        }
        let Some(Modal::NewSession(form)) = &mut self.modal else {
            return;
        };
        let catalog = self.model_completion.catalog(form.location.as_deref());
        match shortcut {
            Some(NAVIGATION_NEXT_FOCUS) => form.focus((form.field + 1) % LABELS.len()),
            Some(NAVIGATION_PREVIOUS_FOCUS) => {
                form.focus((form.field + LABELS.len() - 1) % LABELS.len())
            }
            Some(NAVIGATION_UP | NAVIGATION_DOWN) => {
                form.step(shortcut == Some(NAVIGATION_DOWN), catalog)
            }
            _ => {
                let before = form.query.text();
                form.query.key(key);
                if form.query.text() != before {
                    form.filtered(catalog);
                }
            }
        }
        self.tick_new_session();
    }
    pub(super) fn new_session_paste(&mut self, text: &str) {
        if let Some(Modal::NewSession(form)) = &mut self.modal {
            form.query.insert(text);
            let catalog = self.model_completion.catalog(form.location.as_deref());
            form.filtered(catalog);
        }
    }
    pub(super) fn new_session_field(&mut self, field: usize) {
        if let Some(Modal::NewSession(form)) = &mut self.modal {
            form.focus(field.min(LABELS.len() - 1));
        }
        self.tick_new_session();
    }
    pub(super) fn new_session_option(&mut self, index: usize) {
        if let Some(Modal::NewSession(form)) = &mut self.modal {
            let catalog = self.model_completion.catalog(form.location.as_deref());
            if let Some(choice) = form.choices(form.field, catalog).get(index) {
                let value = choice.value.clone();
                form.choose(form.field, value);
            }
        }
        self.tick_new_session();
    }
    /// Starts the session with the form's choices, keeping the form open
    /// until the service answers.
    pub(super) fn start_new_session(&mut self) {
        if self.busy {
            return;
        }
        let Some(Modal::NewSession(form)) = &mut self.modal else {
            return;
        };
        let request = match form.request() {
            Ok(request) => request,
            Err(error) => {
                form.error = Some(error);
                return;
            }
        };
        form.error = None;
        form.starting = true;
        let local = match &request {
            Request::NewAgent {
                defaults,
                server: None,
                ..
            } => Some(defaults.clone()),
            _ => None,
        };
        let connect = local.is_some();
        self.launch_defaults = local;
        self.busy = true;
        self.task(Task::Launch, request, connect);
    }
    pub(super) fn draw_new_session(&mut self, frame: &mut Frame, area: Rect) {
        let bindings = Bindings::new(&self.shortcut_config);
        let hint = |action| bindings.primary_hint_in(action, AGENT_MODEL);
        let Some(Modal::NewSession(form)) = &self.modal else {
            return;
        };
        let catalog = self.model_completion.catalog(form.location.as_deref());
        let choices = form.choices(form.field, catalog);
        let mut hits = Vec::new();
        frame.render_widget(
            Paragraph::new(format!(
                "Where should the new session run? {} starts with these choices; {} changes a field.",
                hint(NAVIGATION_ACTIVATE),
                hint(NAVIGATION_NEXT_FOCUS)
            ))
            .style(Style::default().fg(DIM))
            .wrap(Wrap { trim: false }),
            Rect::new(area.x, area.y, area.width, 2).intersection(area),
        );
        for (index, label) in LABELS.iter().enumerate() {
            let rect = Rect::new(
                area.x,
                area.y.saturating_add(2 + index as u16),
                area.width,
                1,
            )
            .intersection(area);
            let focused = form.field == index;
            let style = if focused {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{} {label:<11}", if focused { "›" } else { " " }),
                        style,
                    ),
                    Span::styled(
                        crate::model::clean(&form.display(index, catalog)),
                        Style::default().fg(TEXT),
                    ),
                ])),
                rect,
            );
            hits.push((rect, Action::NewSessionField(index)));
        }
        let start = Rect::new(area.x, area.y.saturating_add(7), area.width, 1).intersection(area);
        let place = form.location.as_deref().unwrap_or("This Mac");
        frame.render_widget(
            Paragraph::new(if form.starting {
                format!("Starting on {place}…")
            } else {
                format!("[ {} · Start on {place} ]", hint(NAVIGATION_ACTIVATE))
            })
            .style(Style::default().fg(if form.starting { DIM } else { ACCENT })),
            start,
        );
        hits.push((start, Action::NewSessionStart));
        let filter = Rect::new(area.x, area.y.saturating_add(9), area.width, 3).intersection(area);
        let label = LABELS
            .get(form.field)
            .copied()
            .unwrap_or_default()
            .to_lowercase();
        editor(
            frame,
            filter,
            &format!(
                "Filter {label} · {}/{} choose",
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN)
            ),
            &form.query,
            true,
        );
        let top = area.y.saturating_add(12);
        let footer = area.bottom().saturating_sub(1);
        let status_row = footer.saturating_sub(1);
        let visible = usize::from(status_row.saturating_sub(top));
        let selected = form.selected(&choices);
        let first = selected
            .unwrap_or_default()
            .saturating_sub(visible.saturating_sub(1));
        let label_width = choices
            .iter()
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(8, 40);
        for (offset, (index, choice)) in choices
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .enumerate()
        {
            let rect = Rect::new(area.x, top + offset as u16, area.width, 1).intersection(area);
            let active = Some(index) == selected;
            let style = crate::ui::option_style(active);
            let detail_style = if active {
                style
            } else if choice.enabled {
                Style::default().fg(DIM)
            } else {
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!(
                            "{} {:label_width$}  ",
                            if active { "›" } else { " " },
                            crate::model::clean(&choice.label)
                        ),
                        style,
                    ),
                    Span::styled(crate::model::clean(&choice.detail), detail_style),
                ]))
                .style(style),
                rect,
            );
            hits.push((rect, Action::NewSessionOption(index)));
        }
        if choices.is_empty() {
            let message = match (&form.location, form.status()) {
                (Some(server), Some(status))
                    if form.field == REPOSITORY && status.repositories.is_empty() =>
                {
                    format!(
                        "No repositories on {server} yet. Add one there with `difu repos add owner/name`."
                    )
                }
                _ => "No matching options".into(),
            };
            frame.render_widget(
                Paragraph::new(message)
                    .style(Style::default().fg(DIM))
                    .wrap(Wrap { trim: false }),
                Rect::new(area.x, top, area.width, 2).intersection(area),
            );
        }
        let (status, error) = if let Some(error) = &form.error {
            (crate::model::clean(error), true)
        } else if matches!(form.field, MODEL | EFFORT) && catalog.is_none() {
            let label = self.model_completion.empty_label();
            (
                match &form.location {
                    Some(server) if self.model_completion.loading() => {
                        format!("Loading models on {server}…")
                    }
                    _ => crate::model::clean(label),
                },
                !self.model_completion.loading(),
            )
        } else {
            (String::new(), false)
        };
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(if error { RED } else { DIM })),
            Rect::new(area.x, status_row, area.width, 1).intersection(area),
        );
        frame.render_widget(
            Paragraph::new(format!(
                "Type to filter · {}/{} choose · {}/{} fields · {} starts · Esc cancels",
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN),
                hint(NAVIGATION_NEXT_FOCUS),
                hint(NAVIGATION_PREVIOUS_FOCUS),
                hint(NAVIGATION_ACTIVATE)
            ))
            .style(Style::default().fg(DIM)),
            Rect::new(area.x, footer, area.width, 1).intersection(area),
        );
        self.hits.extend(hits);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::cloud::{
        federation::{
            Federation, Repository, Route,
            tests::{Fake, connected, server_defaults, server_models, session},
        },
        servers::{self, tests::server},
    };
    use anyhow::Result;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    /// The UI's agent service: `Servers` and requests for saved servers go
    /// through a real federation to fake servers; anything for this Mac is
    /// refused, so a test notices it.
    pub(in super::super) struct Service {
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<Vec<Request>>>,
    }

    impl Service {
        pub(in super::super) fn start(storage: &Storage, federation: Federation) -> Result<Self> {
            let listener = UnixListener::bind(super::super::super::server::socket(storage)?)?;
            listener.set_nonblocking(true)?;
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                while !stopping.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Ok(request) = answer(&federation, stream) {
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
                .map_err(|_| anyhow::anyhow!("service panicked"))
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

    fn answer(federation: &Federation, mut stream: UnixStream) -> Result<Request> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: Request = serde_json::from_str(&line)?;
        let reply = match &request {
            Request::Servers => Reply::Servers(federation.servers()),
            request => match federation.route(request.clone()) {
                Route::Handled(reply) => {
                    reply.unwrap_or_else(|error| Reply::Error(format!("{error:#}")))
                }
                Route::Local(_) => Reply::Error("This Mac was asked".into()),
            },
        };
        serde_json::to_writer(&mut stream, &reply)?;
        stream.write_all(b"\n")?;
        Ok(request)
    }

    /// Ticks the UI until `done`, failing after a few seconds.
    pub(in super::super) fn until(
        ui: &mut Ui,
        what: &str,
        done: impl Fn(&Ui) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(ui) {
            anyhow::ensure!(
                Instant::now() < deadline,
                "the UI never got to {what}: notice {:?}",
                ui.notice
            );
            ui.tick(false);
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

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
            defaults: Some(server_defaults()),
            ..ServerStatus::default()
        }
    }

    /// The form with these servers, fresh enough that it never polls.
    fn open(ui: &mut Ui, servers: Vec<ServerStatus>) {
        let local = ui.defaults.clone();
        let candidates = ui.local_repositories(None, &local);
        let mut form = Form::new(local, candidates, Some("/Users/me/code".into()), servers);
        form.fetched = Some(Instant::now());
        ui.modal = Some(Modal::NewSession(Box::new(form)));
    }

    fn form(ui: &Ui) -> Result<&Form> {
        let Some(Modal::NewSession(form)) = &ui.modal else {
            anyhow::bail!("the new-session form is closed");
        };
        Ok(form)
    }

    fn labels(ui: &Ui) -> Result<Vec<String>> {
        let form = form(ui)?;
        let catalog = ui.model_completion.catalog(form.location.as_deref());
        Ok(form
            .choices(form.field, catalog)
            .into_iter()
            .map(|choice| choice.label)
            .collect())
    }

    fn screen(ui: &mut Ui) -> Result<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 44))?;
        terminal.draw(|frame| ui.draw(frame))?;
        Ok(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect())
    }

    fn press(ui: &mut Ui, code: KeyCode) {
        ui.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn local_defaults(ui: &mut Ui, repository: &Path) -> Result<()> {
        let repository = repository.to_owned();
        ui.storage.update_config(|config| {
            config.agent_defaults.repository = Some(repository.clone());
            config.agent_defaults.model = Some("local-model".into());
            config.agent_defaults.effort = Some("low".into());
        })?;
        ui.defaults = ui.storage.load_config()?.agent_defaults;
        Ok(())
    }

    #[test]
    fn without_servers_the_form_offers_this_mac_and_enter_starts_with_its_defaults() -> Result<()> {
        let (mut ui, dir, _) = shortcut_tests::fixture(None)?;
        local_defaults(&mut ui, dir.path())?;
        ui.focus = Focus::List;
        let request = shortcut_tests::request_at_connected_boundary_with(&mut ui, |ui, _| {
            ui.launch();
            // Nothing reaches the service before the user starts.
            if let Ok(text) = screen(ui) {
                assert!(text.contains("New session"), "{text}");
                assert!(text.contains("This Mac"), "{text}");
                assert!(text.contains("local-model"), "{text}");
            }
            assert_eq!(
                labels(ui).ok(),
                Some(vec!["This Mac".to_owned()]),
                "only This Mac without saved servers"
            );
            press(ui, KeyCode::Down);
            press(ui, KeyCode::Enter);
        })?;
        let Request::NewAgent {
            defaults,
            server,
            remember_repository,
            ..
        } = request
        else {
            anyhow::bail!("a new agent request");
        };
        assert_eq!(server, None);
        assert!(!remember_repository);
        assert_eq!(defaults.repository.as_deref(), Some(dir.path()));
        assert_eq!(
            (defaults.model.as_deref(), defaults.effort.as_deref()),
            (Some("local-model"), Some("low"))
        );
        assert!(form(&ui)?.starting, "the form waits for the service");
        Ok(())
    }

    #[test]
    fn choosing_a_server_reloads_every_choice_from_that_server() -> Result<()> {
        let (mut ui, dir, _) = shortcut_tests::fixture(None)?;
        local_defaults(&mut ui, dir.path())?;
        open(
            &mut ui,
            vec![
                status("hetzner", None),
                status(
                    "old",
                    Some("old runs difu 0.0.1; run `difu server upgrade old`"),
                ),
            ],
        );
        assert_eq!(labels(&ui)?, ["This Mac", "hetzner", "old"]);
        let text = screen(&mut ui)?;
        assert!(text.contains("1 repository"), "{text}");
        assert!(text.contains("difu server upgrade old"), "{text}");
        press(&mut ui, KeyCode::Down);
        let chosen = form(&ui)?;
        assert_eq!(chosen.location.as_deref(), Some("hetzner"));
        assert_eq!(
            (
                chosen.repository.as_deref(),
                chosen.model.as_deref(),
                chosen.effort.as_deref()
            ),
            (
                Some("/srv/difu/repos/acme/api"),
                Some("server-model"),
                Some("high")
            )
        );
        let text = screen(&mut ui)?;
        assert!(text.contains("Start on hetzner"), "{text}");
        assert!(text.contains("acme/api"), "{text}");
        // Its repositories are the server's, with clones in progress marked.
        press(&mut ui, KeyCode::Tab);
        assert_eq!(labels(&ui)?, ["acme/api", "acme/web"]);
        assert!(screen(&mut ui)?.contains("cloning…"));
        // Back on This Mac, its own defaults return.
        press(&mut ui, KeyCode::BackTab);
        press(&mut ui, KeyCode::Up);
        let back = form(&ui)?;
        assert_eq!(back.location, None);
        assert_eq!(back.model.as_deref(), Some("local-model"));
        assert_eq!(
            back.repository.as_deref(),
            Some(dir.path().display().to_string().as_str())
        );
        Ok(())
    }

    #[test]
    fn enter_on_a_server_starts_there_with_its_defaults() -> Result<()> {
        let (mut ui, dir, _) = shortcut_tests::fixture(None)?;
        local_defaults(&mut ui, dir.path())?;
        open(&mut ui, vec![status("hetzner", None)]);
        press(&mut ui, KeyCode::Down);
        let request =
            shortcut_tests::request_at_boundary_with(&mut ui, |ui| press(ui, KeyCode::Enter))?;
        let Request::NewAgent {
            defaults,
            server,
            cwd,
            ..
        } = request
        else {
            anyhow::bail!("a new agent request");
        };
        assert_eq!(server.as_deref(), Some("hetzner"));
        assert_eq!(cwd, PathBuf::new());
        assert_eq!(
            defaults.repository,
            Some(PathBuf::from("/srv/difu/repos/acme/api"))
        );
        assert_eq!(
            (defaults.model.as_deref(), defaults.effort.as_deref()),
            (Some("server-model"), Some("high"))
        );
        // The visible start button does the same.
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        open(&mut ui, vec![status("hetzner", None)]);
        press(&mut ui, KeyCode::Down);
        let click = shortcut_tests::native_target(&mut ui, 110, "[ Enter · Start on hetzner ]")?;
        let request = shortcut_tests::request_at_boundary_with(&mut ui, |ui| ui.mouse(click))?;
        assert!(matches!(request, Request::NewAgent { server: Some(s), .. } if s == "hetzner"));
        Ok(())
    }

    #[test]
    fn unusable_servers_and_unready_repositories_explain_instead_of_starting() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let mut empty = status("empty", None);
        empty.repositories.clear();
        empty.defaults = None;
        open(
            &mut ui,
            vec![status("old", Some("old runs difu 0.0.1")), empty],
        );
        press(&mut ui, KeyCode::Down);
        press(&mut ui, KeyCode::Enter);
        assert!(!ui.busy);
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("0.0.1"))
        );
        press(&mut ui, KeyCode::Down);
        press(&mut ui, KeyCode::Enter);
        assert!(!ui.busy);
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("difu repos add"))
        );
        press(&mut ui, KeyCode::Tab);
        assert!(screen(&mut ui)?.contains("No repositories on empty yet"));
        // A repository still cloning cannot start yet.
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        open(&mut ui, vec![status("hetzner", None)]);
        press(&mut ui, KeyCode::Down);
        press(&mut ui, KeyCode::Tab);
        ui.paste("web");
        assert_eq!(
            form(&ui)?.repository.as_deref(),
            Some("/srv/difu/repos/acme/web")
        );
        press(&mut ui, KeyCode::Enter);
        assert!(!ui.busy);
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("still being cloned on hetzner"))
        );
        press(&mut ui, KeyCode::Esc);
        assert!(ui.modal.is_none());
        Ok(())
    }

    #[test]
    fn late_server_defaults_fill_only_the_fields_not_yet_edited() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let mut unread = status("hetzner", None);
        unread.defaults = None;
        open(&mut ui, vec![unread]);
        press(&mut ui, KeyCode::Down);
        assert_eq!(form(&ui)?.model, None);
        press(&mut ui, KeyCode::Tab);
        press(&mut ui, KeyCode::Tab);
        ui.paste("my-model");
        let Some(Modal::NewSession(form)) = &mut ui.modal else {
            anyhow::bail!("form");
        };
        form.receive(Ok(vec![status("hetzner", None)]));
        assert_eq!(form.model.as_deref(), Some("my-model"));
        assert_eq!(form.effort.as_deref(), Some("high"));
        Ok(())
    }

    #[test]
    fn a_typed_path_on_this_mac_becomes_the_repository() -> Result<()> {
        let (mut ui, dir, _) = shortcut_tests::fixture(None)?;
        open(&mut ui, Vec::new());
        // Without a saved default, this directory's repository comes first.
        press(&mut ui, KeyCode::Tab);
        let listed = labels(&ui)?;
        assert_eq!(listed.first().map(String::as_str), Some("This directory"));
        assert!(
            listed.contains(&dir.path().display().to_string()),
            "{listed:?}"
        );
        assert_eq!(form(&ui)?.repository, None);
        ui.paste("/tmp/elsewhere");
        assert_eq!(form(&ui)?.repository.as_deref(), Some("/tmp/elsewhere"));
        assert!(screen(&mut ui)?.contains("use this path"));
        let request = shortcut_tests::request_at_connected_boundary_with(&mut ui, |ui, _| {
            press(ui, KeyCode::Enter)
        })?;
        assert!(matches!(
            request,
            Request::NewAgent { defaults, server: None, .. }
                if defaults.repository == Some(PathBuf::from("/tmp/elsewhere"))
        ));
        Ok(())
    }

    #[test]
    fn model_and_reasoning_lists_come_from_the_chosen_server() -> Result<()> {
        let fake = Fake::new(Vec::new());
        let (_servers, federation) = connected(vec![("hetzner", fake.clone())])?;
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        ui.model_completion.set(
            None,
            vec![ModelInfo {
                id: "local-only".into(),
                name: "Local Only".into(),
                efforts: vec!["low".into()],
            }],
        );
        let service = Service::start(&ui.storage, federation)?;
        open(&mut ui, vec![status("hetzner", None)]);
        press(&mut ui, KeyCode::Down);
        press(&mut ui, KeyCode::Tab);
        press(&mut ui, KeyCode::Tab);
        until(&mut ui, "the server catalog", |ui| {
            ui.model_completion.catalog(Some("hetzner")).is_some()
        })?;
        assert_eq!(labels(&ui)?, ["Default", "Server Model", "Opus"]);
        assert!(screen(&mut ui)?.contains("server-model · default"));
        // Choosing a model on another provider drops the reasoning level.
        ui.paste("opus");
        assert_eq!(form(&ui)?.model.as_deref(), Some("claude/opus"));
        assert_eq!(form(&ui)?.effort, None);
        press(&mut ui, KeyCode::Tab);
        assert_eq!(labels(&ui)?, ["Default", "max"]);
        press(&mut ui, KeyCode::Down);
        assert_eq!(form(&ui)?.effort.as_deref(), Some("max"));
        // This Mac's catalog comes back from memory, without discovery.
        press(&mut ui, KeyCode::Tab);
        press(&mut ui, KeyCode::Up);
        press(&mut ui, KeyCode::Tab);
        press(&mut ui, KeyCode::Tab);
        assert_eq!(form(&ui)?.location, None);
        assert_eq!(labels(&ui)?, ["Default", "Local Only"]);
        let requests = service.requests()?;
        assert!(
            requests
                .iter()
                .any(|r| matches!(r, Request::Models { server: Some(s) } if s == "hetzner")),
            "{requests:?}"
        );
        assert!(
            !requests
                .iter()
                .any(|r| matches!(r, Request::Models { server: None })),
            "{requests:?}"
        );
        assert!(
            fake.requests()
                .iter()
                .any(|r| matches!(r, Request::Models { server: None }))
        );
        Ok(())
    }

    #[test]
    fn a_blank_model_offers_the_levels_of_the_model_it_inherits_there() -> Result<()> {
        let fake = Fake::new(Vec::new());
        let (_servers, federation) = connected(vec![("hetzner", fake.clone())])?;
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let mut inherited = server_models();
        inherited.push(ModelInfo {
            id: "codex-at-/srv/difu/repos/acme/api".into(),
            name: "Inherited".into(),
            efforts: vec!["minimal".into()],
        });
        ui.model_completion.set(Some("hetzner"), inherited);
        let service = Service::start(&ui.storage, federation)?;
        let mut blank = status("hetzner", None);
        blank.defaults = Some(AgentDefaults {
            model: None,
            effort: None,
            ..server_defaults()
        });
        open(&mut ui, vec![blank]);
        press(&mut ui, KeyCode::Down);
        press(&mut ui, KeyCode::BackTab);
        until(&mut ui, "the inherited levels", |ui| {
            labels(ui).is_ok_and(|labels| labels.contains(&"minimal".to_owned()))
        })?;
        let requests = service.requests()?;
        assert!(requests.iter().any(|r| matches!(
            r,
            Request::Defaults { cwd, server: Some(s) }
                if s == "hetzner" && cwd == Path::new("/srv/difu/repos/acme/api")
        )));
        Ok(())
    }

    #[test]
    fn opening_the_form_with_saved_servers_reads_their_state_from_the_service() -> Result<()> {
        let fake = Fake::new(Vec::new());
        let (_servers, federation) = connected(vec![("hetzner", fake)])?;
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        servers::save(&ui.storage, server("hetzner", "a"))?;
        let service = Service::start(&ui.storage, federation)?;
        ui.launch();
        assert_eq!(labels(&ui)?, ["This Mac", "hetzner"]);
        assert!(!ui.busy, "nothing starts before a choice");
        until(&mut ui, "a connected server", |ui| {
            form(ui).is_ok_and(|form| form.servers.first().is_some_and(|s| s.problem.is_none()))
        })?;
        assert_eq!(
            ui.location_defaults.get("hetzner"),
            Some(&server_defaults())
        );
        press(&mut ui, KeyCode::Down);
        assert_eq!(form(&ui)?.model.as_deref(), Some("server-model"));
        let requests = service.requests()?;
        assert!(requests.iter().all(|r| matches!(r, Request::Servers)));
        Ok(())
    }

    #[test]
    fn a_repository_asked_for_later_keeps_the_form_choices() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        ui.launch_defaults = Some(AgentDefaults {
            model: Some("chosen-model".into()),
            ..AgentDefaults::default()
        });
        let request = shortcut_tests::request_at_connected_boundary_with(&mut ui, |ui, _| {
            ui.launch_in("/tmp/project".into())
        })?;
        assert!(matches!(
            request,
            Request::NewAgent { defaults, remember_repository: true, server: None, .. }
                if defaults.model.as_deref() == Some("chosen-model")
                    && defaults.repository == Some(PathBuf::from("/tmp/project"))
        ));
        Ok(())
    }

    #[test]
    fn server_session_codex_defaults_are_read_on_that_server() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let fresh = session("hetzner:2");
        let request = shortcut_tests::request_at_connected_boundary_with(&mut ui, |ui, _| {
            let _ = ui.sender.send(ResultMessage {
                kind: Task::Read("hetzner:2".into()),
                result: Ok(Reply::Session(Box::new(fresh))),
            });
            ui.tick(false);
        })?;
        let Request::Defaults { cwd, server } = request else {
            anyhow::bail!("Codex defaults request: {request:?}");
        };
        assert_eq!(server.as_deref(), Some("hetzner"));
        assert_eq!(cwd, PathBuf::from("/srv/difu/repos/acme/api"));
        Ok(())
    }
}
