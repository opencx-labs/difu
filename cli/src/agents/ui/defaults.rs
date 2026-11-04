use super::*;
use crate::{
    agents::permissions::{self, CodexPreset},
    shortcuts::{Bindings, ids::*},
    storage::AgentDefaults,
};
use provider::Provider;

/// The editable fields: repository, model, reasoning, starting mode and Codex
/// approvals; the Save button comes after them.
pub(super) const FIELDS: usize = 5;
pub(super) const SAVE: usize = FIELDS;
const MODE: usize = 3;
const APPROVALS: usize = 4;
/// Typed in the model field: Codex's configured model rather than the
/// automatic one.
const CODEX_MODEL: &str = "codex";

/// Whether `provider` offers the starting mode `mode`.
fn offers(provider: Provider, mode: &str) -> bool {
    permissions::mode_choices(provider)
        .iter()
        .any(|choice| choice.id == mode)
}

pub struct Settings {
    draft: crate::settings::SettingsDraft,
    pub fields: Vec<Editor>,
    pub field: usize,
    pub error: Option<String>,
    selected: usize,
    filled: bool,
    provider: super::super::provider::Provider,
    directory: Option<std::path::PathBuf>,
    directories: Vec<String>,
    directory_error: Option<String>,
    directory_rx: Option<mpsc::Receiver<Result<Vec<String>, String>>>,
    inherited: Option<String>,
    inherited_cwd: Option<std::path::PathBuf>,
    inherited_rx: Option<mpsc::Receiver<Result<String, String>>>,
    /// The provider of a blank (automatic) model here: Claude Code, unless
    /// this Mac has no Claude models.
    automatic: Provider,
    /// This Mac is a difu server (Auto starting modes).
    server: bool,
    /// This Mac's effective defaults, as last read from the service.
    effective: Option<super::super::effective::Defaults>,
    effective_rx: Option<mpsc::Receiver<Result<super::super::effective::Defaults, String>>>,
}
impl Settings {
    fn new(defaults: AgentDefaults) -> Self {
        let provider = defaults.resolved_provider();
        // Codex's configured model is spelled out; blank is automatic.
        let model = match (&defaults.model, defaults.provider) {
            (Some(model), _) => model.clone(),
            (None, Some(Provider::Codex)) => CODEX_MODEL.into(),
            (None, _) => String::new(),
        };
        Self {
            draft: crate::settings::SettingsDraft::new(Config {
                agent_defaults: defaults.clone(),
                ..Config::default()
            }),
            provider,
            fields: vec![
                Editor::from(
                    defaults
                        .repository
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                Editor::from(model.as_str()),
                Editor::from(defaults.effort.unwrap_or_default()),
                Editor::from(defaults.starting_mode.unwrap_or_default()),
                Editor::from(defaults.codex_preset.map(|p| p.id()).unwrap_or_default()),
            ],
            field: 0,
            error: None,
            selected: 0,
            filled: false,
            directory: None,
            directories: Vec::new(),
            directory_error: None,
            directory_rx: None,
            inherited: None,
            inherited_cwd: None,
            inherited_rx: None,
            automatic: Provider::Claude,
            server: false,
            effective: None,
            effective_rx: None,
        }
    }
    /// The model field's model; `None` for a blank (automatic) one and for
    /// Codex's configured model.
    fn model(&self) -> Option<String> {
        self.value(1)
            .filter(|model| !model.eq_ignore_ascii_case(CODEX_MODEL))
    }
    /// The provider the model field picks: the model's, Codex for `codex`,
    /// and the automatic model's when blank.
    fn model_provider(&self) -> Provider {
        match self.value(1) {
            Some(model) => Provider::for_model(Some(&model)),
            None => self.automatic,
        }
    }
    /// This Mac's effective defaults arrived.
    fn receive(&mut self, defaults: super::super::effective::Defaults) {
        let saved = &defaults.saved;
        // Without Claude models a blank model is Codex's configured one.
        if saved.model.is_none() && saved.provider != Some(Provider::Codex) {
            self.automatic = defaults.provider;
        }
        self.server = defaults.server;
        self.effective = Some(defaults);
        self.changed();
    }
    pub(super) fn changed(&mut self) {
        let provider = self.model_provider();
        if provider != self.provider {
            if let Some(effort) = self.fields.get_mut(2) {
                *effort = Editor::default();
            }
            // Starting modes belong to a provider too.
            if self
                .value(MODE)
                .is_some_and(|mode| !offers(provider, &mode))
                && let Some(mode) = self.fields.get_mut(MODE)
            {
                *mode = Editor::default();
            }
            self.provider = provider;
        }
        self.selected = 0;
        self.filled = false;
    }
    fn directory_query(&self) -> (std::path::PathBuf, String, String) {
        let text = self.fields.first().map(Editor::text).unwrap_or_default();
        let (prefix, query) = text
            .rsplit_once('/')
            .map(|(p, q)| (format!("{p}/"), q.to_owned()))
            .unwrap_or_else(|| (String::new(), text));
        let directory = if let Some(relative) = prefix.strip_prefix("~/") {
            dirs::home_dir().unwrap_or_default().join(relative)
        } else if prefix.is_empty() {
            std::path::PathBuf::from(".")
        } else {
            std::path::PathBuf::from(&prefix)
        };
        (directory, prefix, query)
    }
    fn poll(&mut self) {
        if self.field == 0 {
            let (directory, _, _) = self.directory_query();
            if self.directory.as_ref() != Some(&directory) {
                self.directory = Some(directory.clone());
                self.directories.clear();
                self.directory_error = None;
                let (tx, rx) = mpsc::channel();
                self.directory_rx = Some(rx);
                thread::spawn(move || {
                    let result = (|| -> std::io::Result<Vec<String>> {
                        let mut names = Vec::new();
                        for entry in std::fs::read_dir(directory)? {
                            let entry = entry?;
                            if entry.path().is_dir() {
                                names.push(entry.file_name().to_string_lossy().into_owned());
                            }
                        }
                        names.sort_unstable();
                        Ok(names)
                    })()
                    .map_err(|e| format!("Cannot list directories: {e}"));
                    let _ = tx.send(result);
                });
            }
        }
        if let Some(result) = self.directory_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            crate::repaint::request();
            self.directory_rx = None;
            match result {
                Ok(names) => self.directories = names,
                Err(error) => self.directory_error = Some(error),
            }
        }
        if self.field == 2
            && self.model().is_none()
            && self.provider == super::super::provider::Provider::Codex
        {
            let cwd = self
                .value(0)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            if self.inherited_cwd.as_ref() != Some(&cwd) {
                self.inherited_cwd = Some(cwd.clone());
                self.inherited = None;
                let (tx, rx) = mpsc::channel();
                self.inherited_rx = Some(rx);
                thread::spawn(move || {
                    let result = super::super::engine::defaults(&cwd)
                        .and_then(|reply| match reply {
                            Reply::Defaults { model, .. } => Ok(model),
                            _ => anyhow::bail!("Codex did not report its default model"),
                        })
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(result);
                });
            }
        }
        if let Some(result) = self.inherited_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            crate::repaint::request();
            self.inherited_rx = None;
            match result {
                Ok(model) => self.inherited = Some(model),
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(result) = self.effective_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            crate::repaint::request();
            self.effective_rx = None;
            if let Ok(defaults) = result {
                self.receive(defaults);
            }
        }
    }
    /// What new sessions here start with, from the service's answer.
    fn summary(&self) -> Option<String> {
        let defaults = self.effective.as_ref()?;
        let mut parts = vec![defaults.provider.label().to_owned()];
        parts.push(
            defaults
                .model
                .clone()
                .unwrap_or_else(|| "its default model".into()),
        );
        if let Some(effort) = &defaults.effort {
            parts.push(effort.clone());
        }
        if defaults.automatic_model {
            parts.push("automatic".into());
        }
        Some(format!("New sessions here: {}", parts.join(" · ")))
    }
    fn options(&self, models: &models::State) -> Vec<String> {
        if self.field == 0 {
            let (_, prefix, query) = self.directory_query();
            self.directories
                .iter()
                .filter(|name| {
                    name.starts_with(&query) && (!name.starts_with('.') || query.starts_with('.'))
                })
                .map(|name| format!("{prefix}{name}/"))
                .collect()
        } else if self.field < MODE {
            // A blank model's levels: Codex's inherited model, or the
            // automatic Opus.
            let automatic = self
                .effective
                .as_ref()
                .filter(|defaults| defaults.automatic_model && self.value(1).is_none())
                .and_then(|defaults| defaults.model.clone());
            let model = self
                .model()
                .or_else(|| {
                    (self.field == 2)
                        .then(|| self.inherited.clone().or(automatic))
                        .flatten()
                })
                .unwrap_or_default();
            let typed = self.value(1).unwrap_or_default();
            let mut choices = models.choices(
                if self.field == 1 { &typed } else { &model },
                &self.value(2).unwrap_or_default(),
                self.field == 1,
            );
            if self.field == 1 && CODEX_MODEL.contains(&typed.to_lowercase()) {
                choices.insert(0, CODEX_MODEL.into());
            }
            choices
        } else if self.field == MODE {
            let typed = self.value(MODE).unwrap_or_default().to_lowercase();
            permissions::mode_choices(self.model_provider())
                .into_iter()
                .map(|choice| choice.id)
                .filter(|id| id.to_lowercase().contains(&typed))
                .collect()
        } else if self.field == APPROVALS {
            let typed = self.value(APPROVALS).unwrap_or_default().to_lowercase();
            CodexPreset::ALL
                .into_iter()
                .filter(|preset| {
                    preset.id().to_lowercase().contains(&typed)
                        || preset.label().to_lowercase().contains(&typed)
                })
                .map(|preset| preset.id().to_owned())
                .collect()
        } else {
            Vec::new()
        }
    }
    /// How an option reads in the list: modes and presets with their labels.
    fn option_text(&self, option: &str) -> String {
        match self.field {
            1 if option == CODEX_MODEL => "codex · Codex's configured model".into(),
            MODE => permissions::mode_choices(self.model_provider())
                .into_iter()
                .find(|choice| choice.id == option)
                .map_or_else(
                    || option.to_owned(),
                    |choice| format!("{option} · {} · {}", choice.label, choice.description),
                ),
            APPROVALS => CodexPreset::parse(option).map_or_else(
                || option.to_owned(),
                |preset| format!("{option} · {}", preset.label()),
            ),
            _ => option.to_owned(),
        }
    }
    fn value(&self, index: usize) -> Option<String> {
        self.fields
            .get(index)
            .map(Editor::text)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}
impl Ui {
    pub(super) fn open_defaults(&mut self) {
        match self.storage.load_config() {
            Ok(config) => {
                let mut form = Settings::new(config.agent_defaults);
                form.server = config.server.enabled;
                if let Some(effective) = self.local_effective.clone() {
                    form.receive(effective);
                }
                // What a blank model becomes needs this Mac's model catalog,
                // which the service keeps.
                let (tx, rx) = mpsc::channel();
                let storage = self.storage.clone();
                thread::spawn(move || {
                    let result =
                        match client::request(&storage, Request::AgentDefaults { server: None }) {
                            Ok(Reply::AgentDefaults(defaults)) => Ok(*defaults),
                            Ok(_) => Err("Unexpected defaults reply".to_owned()),
                            Err(error) => Err(format!("{error:#}")),
                        };
                    let _ = tx.send(result);
                });
                form.effective_rx = Some(rx);
                self.modal = Some(Modal::AgentDefaults(Box::new(form)));
                // These are this Mac's defaults, so this Mac's models.
                self.model_completion.load(&self.storage, None);
            }
            Err(error) => {
                self.notice = Some((format!("Cannot read agent defaults: {error:#}"), true))
            }
        }
    }
    pub(super) fn save_defaults(&mut self) {
        let Some(Modal::AgentDefaults(form)) = &mut self.modal else {
            return;
        };
        let result = (|| -> anyhow::Result<Config> {
            use crate::settings::{SettingId, SettingValue};
            let saved = form.draft.config().agent_defaults.clone();
            let model = form.model();
            let effort = form.value(2);
            let preset = form
                .value(APPROVALS)
                .map(|text| {
                    anyhow::Context::with_context(CodexPreset::parse(&text), || {
                        format!(
                            "Unknown Codex approvals `{text}`; choose Ask, Auto, Full or ReadOnly"
                        )
                    })
                })
                .transpose()?;
            // A model picks its provider and `codex` picks Codex; a blank
            // model stays automatic, with Claude Code for a mode only it has.
            let provider = match (&model, form.value(1)) {
                (Some(model), _) => Some(Provider::for_model(Some(model))),
                (None, Some(_)) => Some(Provider::Codex),
                (None, None) => form
                    .value(MODE)
                    .filter(|mode| !offers(Provider::Codex, mode))
                    .map(|_| Provider::Claude),
            };
            let target = provider.unwrap_or_else(|| {
                AgentDefaults {
                    provider: None,
                    model: model.clone(),
                    ..AgentDefaults::default()
                }
                .resolved_provider()
            });
            // Another provider's starting mode goes with the switch.
            let mode = form
                .value(MODE)
                .filter(|mode| offers(target, mode) || target == saved.resolved_provider());
            if model != saved.model || provider != saved.provider {
                match &model {
                    Some(_) => form
                        .draft
                        .select_default_model(model.clone(), effort.clone())?,
                    None => {
                        if let Some(provider) = provider {
                            form.draft.set(
                                SettingId::Provider,
                                SettingValue::Choice(provider.key().into()),
                            )?;
                        }
                        form.draft
                            .set(SettingId::DefaultModel, SettingValue::Text(None))?;
                        form.draft
                            .set(SettingId::DefaultEffort, SettingValue::Text(effort.clone()))?;
                    }
                }
            } else if effort != saved.effort {
                form.draft
                    .set(SettingId::DefaultEffort, SettingValue::Text(effort))?;
            }
            if mode != form.draft.config().agent_defaults.starting_mode {
                form.draft
                    .set(SettingId::StartingMode, SettingValue::Text(mode.clone()))?;
            }
            let repository = form.value(0);
            if repository.as_deref()
                != form
                    .draft
                    .config()
                    .agent_defaults
                    .repository
                    .as_ref()
                    .and_then(|p| p.to_str())
            {
                form.draft
                    .set(SettingId::DefaultRepository, SettingValue::Text(repository))?;
            }
            // Checked first for a plain message; the provider may go back to
            // automatic, which the settings registry cannot express.
            let mut checked = form.draft.config().agent_defaults.clone();
            checked.provider = provider;
            checked.codex_preset = preset;
            checked.validate()?;
            let config = self.storage.save_settings_draft(&mut form.draft)?;
            if config.agent_defaults.provider == provider
                && config.agent_defaults.codex_preset == preset
            {
                return Ok(config);
            }
            self.storage.try_patch_config(|config| {
                config.agent_defaults.provider = provider;
                config.agent_defaults.codex_preset = preset;
                config.agent_defaults.validate()
            })
        })();
        match result {
            Ok(config) => {
                self.defaults = config.agent_defaults;
                self.local_effective = None;
                self.modal = None;
                self.notice = Some(("Agent defaults saved for new sessions".into(), false));
            }
            Err(error) => form.error = Some(format!("Cannot save defaults: {error:#}")),
        }
    }
    pub(super) fn defaults_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let action = self.shortcut(key);
        if action == Some(DEFAULTS_REPOSITORY_RULES) {
            self.default_rules();
            return;
        }
        let Some(Modal::AgentDefaults(form)) = &mut self.modal else {
            return;
        };
        let options = form.options(&self.model_completion);
        match action {
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                form.field = (form.field
                    + if action == Some(NAVIGATION_NEXT_FOCUS) {
                        1
                    } else {
                        SAVE
                    })
                    % (SAVE + 1);
                form.changed();
            }
            Some(NAVIGATION_UP | NAVIGATION_DOWN) => {
                form.selected = if action == Some(NAVIGATION_UP) {
                    form.selected.saturating_sub(1)
                } else {
                    (form.selected + 1).min(options.len().saturating_sub(1))
                };
                form.filled = false;
            }
            Some(DEFAULTS_SAVE) => self.save_defaults(),
            Some(NAVIGATION_ACTIVATE) => self.default_activate(),
            _ => {
                if let Some(editor) = form.fields.get_mut(form.field) {
                    editor.key(key);
                    form.changed();
                }
            }
        }
    }
    fn default_activate(&mut self) {
        let Some(Modal::AgentDefaults(form)) = &mut self.modal else {
            return;
        };
        if form.field == SAVE {
            self.save_defaults();
            return;
        }
        let options = form.options(&self.model_completion);
        if !form.filled
            && let Some(value) = options.get(form.selected)
        {
            if let Some(editor) = form.fields.get_mut(form.field) {
                *editor = Editor::from(value.as_str());
            }
            form.changed();
            form.filled = true;
        } else {
            form.field = (form.field + 1) % (SAVE + 1);
            form.changed();
        }
    }
    pub(super) fn tick_defaults(&mut self) {
        if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
            form.poll();
        }
    }
    pub(super) fn default_rules(&mut self) {
        let path = match &self.modal {
            Some(Modal::AgentDefaults(form)) => form.value(0).map(std::path::PathBuf::from),
            _ => None,
        };
        self.open_rules(path);
    }
    pub(super) fn default_option(&mut self, index: usize) {
        if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
            form.selected = index;
            form.filled = false;
        }
        self.default_activate();
    }
    pub(super) fn draw_defaults(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::AgentDefaults(form)) = &self.modal else {
            return;
        };
        let bindings = Bindings::new(&self.shortcut_config);
        let context = self.shortcut_context();
        let hint = |action| bindings.primary_hint_in(action, context);
        let mut buttons = Vec::new();
        let labels = [
            "Default local repository",
            "Default model · blank: newest Claude Opus · codex: Codex's configured model",
            "Default reasoning · blank: xhigh for the automatic Opus, else the model's",
            if form.server {
                "Starting mode · blank: Auto for Claude Code on this server"
            } else {
                "Starting mode · blank: the provider's own mode"
            },
            if form.server {
                "Codex approvals · blank: Approve for me on this server"
            } else {
                "Codex approvals · blank: Codex's configured policy"
            },
        ];
        for (i, label) in labels.into_iter().enumerate() {
            let rect = Rect::new(area.x, area.y.saturating_add(i as u16 * 4), area.width, 3)
                .intersection(area);
            if let Some(value) = form.fields.get(i) {
                editor(frame, rect, label, value, form.field == i);
            }
            buttons.push((rect, Action::DefaultField(i)));
        }
        let below = area.y + FIELDS as u16 * 4;
        let toggle = Rect::new(area.x, below, area.width, 1).intersection(area);
        let rules = Rect::new(area.x, area.y + 3, area.width, 1).intersection(area);
        frame.render_widget(
            Paragraph::new(format!(
                "[ Edit repository rules · {} ]",
                hint(DEFAULTS_REPOSITORY_RULES)
            ))
            .style(crate::ui::option_style(false)),
            rules,
        );
        buttons.push((rules, Action::DefaultRules));
        let note = match form.summary() {
            Some(summary) => format!("{summary} · always in their own worktree"),
            None => "New sessions always use an isolated worktree".into(),
        };
        frame.render_widget(
            Paragraph::new(crate::model::clean(&note)).style(Style::default().fg(TEXT)),
            toggle,
        );
        let save = Rect::new(area.x, below + 1, area.width, 1).intersection(area);
        frame.render_widget(
            Paragraph::new(format!("[ Save defaults · {} ]", hint(DEFAULTS_SAVE)))
                .style(Style::default().fg(if form.field == SAVE { ACCENT } else { TEXT })),
            save,
        );
        buttons.push((save, Action::DefaultSave));
        let options = form.options(&self.model_completion);
        let top = below.saturating_add(3);
        let available = area.bottom().saturating_sub(top).saturating_sub(1);
        let selected = form.selected.min(options.len().saturating_sub(1));
        let first = selected.saturating_sub(usize::from(available).saturating_sub(1));
        if form.field < SAVE {
            for (index, option) in options
                .iter()
                .enumerate()
                .skip(first)
                .take(usize::from(available))
            {
                let rect = Rect::new(
                    area.x,
                    top.saturating_add((index - first) as u16),
                    area.width,
                    1,
                )
                .intersection(area);
                frame.render_widget(
                    Paragraph::new(format!(
                        "{} {}",
                        if index == selected { "›" } else { " " },
                        crate::model::clean(&form.option_text(option))
                    ))
                    .style(crate::ui::option_style(index == selected)),
                    rect,
                );
                buttons.push((rect, Action::DefaultOption(index)));
            }
            if options.is_empty() {
                let label = if form.field == 0 {
                    if form.directory_rx.is_some() {
                        "Loading directories…"
                    } else {
                        form.directory_error
                            .as_deref()
                            .unwrap_or("No matching directories")
                    }
                } else if form.inherited_rx.is_some() {
                    "Loading inherited model…"
                } else {
                    self.model_completion.empty_label()
                };
                frame.render_widget(
                    Paragraph::new(label).style(Style::default().fg(DIM)),
                    Rect::new(area.x, top, area.width, available).intersection(area),
                );
            }
            frame.render_widget(
                Paragraph::new(format!(
                    "{}/{} select · {} fills · {} fields · {} saves",
                    hint(NAVIGATION_UP),
                    hint(NAVIGATION_DOWN),
                    hint(NAVIGATION_ACTIVATE),
                    hint(NAVIGATION_NEXT_FOCUS),
                    hint(DEFAULTS_SAVE)
                ))
                .style(Style::default().fg(DIM)),
                Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1)
                    .intersection(area),
            );
        }
        if let Some(error) = &form.error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(RED))
                    .wrap(Wrap { trim: false }),
                Rect::new(area.x, top, area.width, area.bottom().saturating_sub(top))
                    .intersection(area),
            );
        }
        self.hits.extend(buttons);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};

    #[test]
    fn default_fields_complete_directories_models_and_supported_reasoning() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("project one"))?;
        std::fs::create_dir(temp.path().join("project two"))?;
        std::fs::write(temp.path().join("project file"), "not a directory")?;
        let storage = Storage {
            config: temp.path().join("config.json"),
            cache: temp.path().join("cache"),
        };
        let mut ui = Ui::new(storage, &Config::default());
        ui.model_completion.options = vec![
            crate::model::ModelInfo {
                id: "fixture-astra".into(),
                name: "Astra".into(),
                efforts: vec!["medium".into(), "high".into()],
            },
            crate::model::ModelInfo {
                id: "fixture-luna".into(),
                name: "Luna".into(),
                efforts: vec!["low".into()],
            },
        ];
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("defaults");
        };
        *form.fields.first_mut().context("repository field")? =
            Editor::from(format!("{}/project", temp.path().display()).as_str());
        let start = Instant::now();
        loop {
            ui.tick_defaults();
            if matches!(&ui.modal, Some(Modal::AgentDefaults(form)) if form.directory_rx.is_none())
            {
                break;
            }
            anyhow::ensure!(
                start.elapsed() < Duration::from_secs(5),
                "Directory lookup did not complete"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("defaults");
        };
        assert_eq!(form.options(&ui.model_completion).len(), 2);
        assert!(
            form.options(&ui.model_completion)
                .iter()
                .all(|v| !v.ends_with("project file/"))
        );
        ui.defaults_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.defaults_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("defaults");
        };
        assert!(
            form.fields
                .first()
                .context("repository field")?
                .text()
                .ends_with("project two/")
        );
        assert_eq!(form.field, 0); // Filling a suggestion does not save or change fields.
        ui.defaults_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        ui.paste("astra");
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("defaults");
        };
        assert_eq!(form.options(&ui.model_completion), ["fixture-astra"]);
        ui.default_option(0); // Mouse completion uses the same fill behavior.
        ui.defaults_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("defaults");
        };
        assert_eq!(form.options(&ui.model_completion), ["medium", "high"]);
        ui.paste("hi");
        ui.defaults_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("defaults");
        };
        assert_eq!(
            form.fields.get(2).context("reasoning field")?.text(),
            "high"
        );
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))?;
        terminal.draw(|frame| ui.draw_defaults(frame, frame.area()))?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Enter fills"));
        ui.defaults_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        let saved = ui.storage.load_config()?;
        assert_eq!(saved.agent_defaults.model.as_deref(), Some("fixture-astra"));
        assert_eq!(saved.agent_defaults.effort.as_deref(), Some("high"));
        assert!(
            saved
                .agent_defaults
                .repository
                .context("repository")?
                .ends_with("project two")
        );
        Ok(())
    }

    #[test]
    fn blank_default_model_uses_inherited_model_reasoning_without_filling_model() -> Result<()> {
        let mut form = Settings::new(AgentDefaults::default());
        form.field = 2;
        form.inherited_cwd = Some(std::env::current_dir()?);
        let (tx, rx) = mpsc::channel();
        form.inherited_rx = Some(rx);
        tx.send(Ok("inherited-model".into()))?;
        form.poll();
        let mut models = models::State::default();
        models.options = vec![crate::model::ModelInfo {
            id: "inherited-model".into(),
            name: "Inherited".into(),
            efforts: vec!["medium".into()],
        }];
        assert_eq!(form.options(&models), ["medium"]);
        assert!(form.fields.get(1).context("model field")?.text().is_empty());
        form.field = 1;
        models.options.push(crate::model::ModelInfo {
            id: "other-model".into(),
            name: "Other".into(),
            efforts: vec!["low".into()],
        });
        // An empty model query shows Codex's configured model and the whole catalog.
        assert_eq!(
            form.options(&models),
            ["codex", "inherited-model", "other-model"]
        );
        Ok(())
    }
}

#[cfg(test)]
mod shortcut_regressions {
    use super::*;
    use anyhow::{Context, Result};
    fn setup(ui: &mut Ui, field: usize) {
        ui.model_completion.options = vec![
            crate::model::ModelInfo {
                id: "fixture-one".into(),
                name: "One".into(),
                efforts: vec!["low".into(), "high".into()],
            },
            crate::model::ModelInfo {
                id: "fixture-two".into(),
                name: "Two".into(),
                efforts: vec!["low".into(), "high".into()],
            },
        ];
        // Match production opening: capture saved defaults before editing fields.
        let mut form = Settings::new(AgentDefaults::default());
        for (editor, value) in
            form.fields
                .iter_mut()
                .zip(["/temporary/repository", "fixture-one", "high"])
        {
            *editor = Editor::from(value);
        }
        form.field = field;
        ui.modal = Some(Modal::AgentDefaults(Box::new(form)));
    }
    fn form(ui: &Ui) -> Result<&Settings> {
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("defaults closed")
        };
        Ok(form)
    }
    #[test]
    fn defaults_focus_and_completion_keys_have_real_remap_and_reset_effects() -> Result<()> {
        for field in 0..=SAVE {
            for (action, code, mods, want) in [
                (
                    NAVIGATION_NEXT_FOCUS,
                    KeyCode::Tab,
                    KeyModifiers::NONE,
                    (field + 1) % (SAVE + 1),
                ),
                (
                    NAVIGATION_PREVIOUS_FOCUS,
                    KeyCode::BackTab,
                    KeyModifiers::SHIFT,
                    (field + SAVE) % (SAVE + 1),
                ),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    setup(&mut ui, field);
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
        for field in 0..FIELDS {
            for (action, code, start, want) in [
                (NAVIGATION_UP, KeyCode::Up, 1, 0),
                (NAVIGATION_DOWN, KeyCode::Down, 0, 1),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    setup(&mut ui, field);
                    let Some(Modal::AgentDefaults(f)) = &mut ui.modal else {
                        anyhow::bail!("form")
                    };
                    f.directories = vec!["one".into(), "two".into()];
                    *f.fields.get_mut(field).context("field")? = Editor::default();
                    if field == 2 {
                        *f.fields.get_mut(1).context("model")? = Editor::from("fixture-one");
                    }
                    f.selected = start;
                    if custom {
                        ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                        assert_eq!(
                            form(&ui)?.selected,
                            0,
                            "removed navigation goes to native editor"
                        );
                        if let Some(Modal::AgentDefaults(f)) = &mut ui.modal {
                            f.selected = start;
                        }
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        KeyModifiers::NONE,
                    ));
                    assert_eq!(form(&ui)?.selected, want, "{field} {} {phase}", action.0);
                }
            }
        }
        Ok(())
    }
    #[test]
    fn defaults_save_uses_control_predicate_and_separate_repository_rules() -> Result<()> {
        for field in 0..=SAVE {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                for extra in 0..32 {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(DEFAULTS_SAVE, phase)?;
                    setup(&mut ui, field);
                    let mut modifiers = KeyModifiers::CONTROL;
                    for (bit, modifier) in [
                        (1, KeyModifiers::SHIFT),
                        (2, KeyModifiers::ALT),
                        (4, KeyModifiers::SUPER),
                        (8, KeyModifiers::HYPER),
                        (16, KeyModifiers::META),
                    ] {
                        if extra & bit != 0 {
                            modifiers |= modifier;
                        }
                    }
                    let old = KeyEvent::new(KeyCode::Enter, modifiers);
                    if custom {
                        ui.key(old);
                        assert!(ui.modal.is_some());
                        assert!(ui.storage.load_config()?.agent_defaults.model.is_none());
                        setup(&mut ui, field);
                    }
                    ui.key(if custom {
                        KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE)
                    } else {
                        old
                    });
                    assert!(ui.modal.is_none(), "{field} {phase} {modifiers:?}");
                    let saved = ui.storage.load_config()?.agent_defaults;
                    assert_eq!(saved.model.as_deref(), Some("fixture-one"));
                    assert_eq!(saved.effort.as_deref(), Some("high"));
                    assert_eq!(
                        saved.repository,
                        Some(std::path::PathBuf::from("/temporary/repository"))
                    );
                }
            }
        }
        Ok(())
    }
    #[test]
    fn defaults_rules_action_preserves_draft_and_failed_save_retains_values() -> Result<()> {
        // This is the existing CtrlR repository-rules action, never a defaults reset.
        let action = crate::shortcuts::find("defaults.repository_rules")
            .context("truthful repository rules action")?;
        for field in 0..=SAVE {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                setup(&mut ui, field);
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
                    assert!(matches!(ui.modal, Some(Modal::AgentDefaults(_))));
                    setup(&mut ui, field);
                }
                ui.key(KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Char('r')
                    },
                    if custom {
                        KeyModifiers::NONE
                    } else {
                        KeyModifiers::CONTROL
                    },
                ));
                assert!(matches!(ui.modal, Some(Modal::RepositoryRules(_))));
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert_eq!(form(&ui)?.value(1).as_deref(), Some("fixture-one"));
                assert_eq!(form(&ui)?.field, field);
            }
        }
        let (mut ui, _dir, _) = shortcut_tests::fixture(Some(DEFAULTS_SAVE))?;
        setup(&mut ui, 1);
        std::fs::write(&ui.storage.config, "{malformed")?;
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|s| s.contains("Cannot save"))
        );
        assert_eq!(form(&ui)?.value(1).as_deref(), Some("fixture-one"));
        assert_eq!(std::fs::read_to_string(&ui.storage.config)?, "{malformed");
        Ok(())
    }
    #[test]
    fn visible_default_option_click_fills_after_activation_remap() -> Result<()> {
        for width in [40, 80, 160] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            setup(&mut ui, 1);
            let Some(Modal::AgentDefaults(f)) = &mut ui.modal else {
                anyhow::bail!("form")
            };
            *f.fields.get_mut(1).context("model")? = Editor::from("fixture");
            let click = shortcut_tests::native_target(&mut ui, width, "fixture-two")?;
            ui.mouse(click);
            assert_eq!(form(&ui)?.value(1).as_deref(), Some("fixture-two"));
            assert!(ui.storage.load_config()?.agent_defaults.model.is_none());
        }
        Ok(())
    }
    #[test]
    fn defaults_field_and_button_contexts_preserve_copy_globals_and_editing() -> Result<()> {
        for field in 0..=SAVE {
            shortcut_tests::owned_modal_context(
                |ui| {
                    setup(ui, field);
                    if let Some(Modal::AgentDefaults(f)) = &mut ui.modal
                        && let Some(editor) = f.fields.get_mut(field)
                    {
                        *editor = Editor::from("focused");
                        editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                    }
                },
                if field < FIELDS {
                    crate::shortcuts::contexts::AGENT_DEFAULTS
                } else {
                    crate::shortcuts::contexts::AGENT_DEFAULTS_BUTTON
                },
                (field < FIELDS).then_some("d"),
            )?;
        }
        Ok(())
    }
    #[test]
    fn defaults_activate_fills_or_advances_each_field_and_saves_only_on_button() -> Result<()> {
        for field in 0..=SAVE {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) =
                    shortcut_tests::binding_fixture(NAVIGATION_ACTIVATE, phase)?;
                setup(&mut ui, field);
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    assert_eq!(form(&ui)?.field, field);
                    assert!(!form(&ui)?.filled);
                    setup(&mut ui, field);
                }
                ui.key(KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Enter
                    },
                    KeyModifiers::NONE,
                ));
                if field == SAVE {
                    assert!(ui.modal.is_none());
                    assert_eq!(
                        ui.storage.load_config()?.agent_defaults.model.as_deref(),
                        Some("fixture-one")
                    );
                } else if field == 0 {
                    assert_eq!(form(&ui)?.field, 1);
                } else {
                    assert!(form(&ui)?.filled);
                    assert_eq!(form(&ui)?.field, field);
                    ui.key(KeyEvent::new(
                        if custom {
                            KeyCode::F(12)
                        } else {
                            KeyCode::Enter
                        },
                        KeyModifiers::NONE,
                    ));
                    assert_eq!(form(&ui)?.field, field + 1);
                }
            }
        }
        Ok(())
    }
    #[test]
    fn defaults_native_hints_follow_each_remapped_field_action() -> Result<()> {
        for width in [80, 160] {
            for action in [
                DEFAULTS_REPOSITORY_RULES,
                DEFAULTS_SAVE,
                NAVIGATION_UP,
                NAVIGATION_DOWN,
                NAVIGATION_ACTIVATE,
                NAVIGATION_NEXT_FOCUS,
            ] {
                let (mut ui, _dir, _) = shortcut_tests::fixture(Some(action))?;
                setup(&mut ui, 1);
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
                if action == DEFAULTS_REPOSITORY_RULES {
                    assert!(
                        !text.contains("Ctrl+R"),
                        "unregistered hardcoded rules hint"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn defaults_visible_buttons_and_blank_option_area_are_keyboard_independent() -> Result<()> {
        for width in [40, 80, 160] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(DEFAULTS_SAVE))?;
            setup(&mut ui, 1);
            let click = shortcut_tests::native_target(&mut ui, width, "Save defaults · F12")?;
            ui.mouse(click);
            assert!(ui.modal.is_none());
            assert_eq!(
                ui.storage.load_config()?.agent_defaults.model.as_deref(),
                Some("fixture-one")
            );
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(DEFAULTS_REPOSITORY_RULES))?;
            setup(&mut ui, 1);
            let click = shortcut_tests::native_target(&mut ui, width, "Edit repository rules")?;
            ui.mouse(click);
            assert!(matches!(ui.modal, Some(Modal::RepositoryRules(_))));
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            setup(&mut ui, 1);
            if let Some(Modal::AgentDefaults(f)) = &mut ui.modal {
                *f.fields.get_mut(1).context("model")? = Editor::from("fixture");
            }
            let blank = shortcut_tests::native_blank_below(&mut ui, width, "fixture-two")?;
            ui.mouse(blank);
            assert_eq!(form(&ui)?.value(1).as_deref(), Some("fixture"));
            assert!(!form(&ui)?.filled);
        }
        Ok(())
    }
    #[test]
    fn repository_rules_control_predicate_keeps_all_modifiers_and_override_removal() -> Result<()> {
        for field in 0..=SAVE {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                for extra in 0..32 {
                    let (mut ui, _dir, _) =
                        shortcut_tests::binding_fixture(DEFAULTS_REPOSITORY_RULES, phase)?;
                    setup(&mut ui, field);
                    let mut mods = KeyModifiers::CONTROL;
                    for (bit, m) in [
                        (1, KeyModifiers::SHIFT),
                        (2, KeyModifiers::ALT),
                        (4, KeyModifiers::SUPER),
                        (8, KeyModifiers::HYPER),
                        (16, KeyModifiers::META),
                    ] {
                        if extra & bit != 0 {
                            mods |= m;
                        }
                    }
                    if custom {
                        ui.key(KeyEvent::new(KeyCode::Char('r'), mods));
                        assert!(matches!(ui.modal, Some(Modal::AgentDefaults(_))));
                        setup(&mut ui, field);
                    }
                    ui.key(KeyEvent::new(
                        if custom {
                            KeyCode::F(12)
                        } else {
                            KeyCode::Char('r')
                        },
                        if custom { KeyModifiers::NONE } else { mods },
                    ));
                    assert!(
                        matches!(ui.modal, Some(Modal::RepositoryRules(_))),
                        "{field} {phase} {mods:?}"
                    );
                    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    assert_eq!(form(&ui)?.value(1).as_deref(), Some("fixture-one"));
                }
            }
        }
        Ok(())
    }
    #[test]
    fn defaults_directory_suggestion_fills_with_remapped_activation_without_saving() -> Result<()> {
        for (phase, custom) in shortcut_tests::BINDING_PHASES {
            let (mut ui, dir, _) = shortcut_tests::binding_fixture(NAVIGATION_ACTIVATE, phase)?;
            setup(&mut ui, 0);
            let path = dir.path().join("chosen repository");
            std::fs::create_dir(&path)?;
            if let Some(Modal::AgentDefaults(f)) = &mut ui.modal {
                f.directories = vec!["chosen repository".into()];
                *f.fields.first_mut().context("repository")? =
                    Editor::from(format!("{}/cho", dir.path().display()));
            }
            if custom {
                ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                assert_eq!(form(&ui)?.field, 0);
                assert!(!form(&ui)?.filled);
                if let Some(Modal::AgentDefaults(f)) = &mut ui.modal {
                    *f.fields.first_mut().context("repository")? =
                        Editor::from(format!("{}/cho", dir.path().display()));
                }
            }
            ui.key(KeyEvent::new(
                if custom {
                    KeyCode::F(12)
                } else {
                    KeyCode::Enter
                },
                KeyModifiers::NONE,
            ));
            assert_eq!(form(&ui)?.value(0), Some(format!("{}/", path.display())));
            assert_eq!(form(&ui)?.field, 0);
            assert!(form(&ui)?.filled);
            assert!(
                ui.storage
                    .load_config()?
                    .agent_defaults
                    .repository
                    .is_none()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod dependent_defaults_tests {
    use super::*;
    use anyhow::{Context, Result};
    fn ui(dir: &tempfile::TempDir) -> Result<Ui> {
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut ui = Ui::new(storage.clone(), &storage.load_config()?);
        ui.model_completion.options = vec![crate::model::ModelInfo {
            id: "gpt-5.6".into(),
            name: "GPT".into(),
            efforts: vec!["high".into()],
        }];
        Ok(ui)
    }
    fn fill(ui: &mut Ui, values: [&str; 5]) -> Result<()> {
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("form")
        };
        for (editor, value) in form.fields.iter_mut().zip(values) {
            *editor = Editor::from(value);
        }
        form.changed();
        Ok(())
    }

    #[test]
    fn starting_mode_and_codex_approvals_are_saved_with_an_automatic_model() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = ui(&dir)?;
        ui.open_defaults();
        // A Claude-only mode with a blank model keeps the model automatic
        // (newest Opus) and saves Claude Code as the provider.
        fill(&mut ui, ["", "", "", "acceptEdits", "approve for me"])?;
        ui.save_defaults();
        assert!(ui.modal.is_none(), "saved");
        let saved = ui.storage.load_config()?.agent_defaults;
        assert_eq!(saved.provider, Some(Provider::Claude));
        assert_eq!(saved.model, None);
        assert_eq!(saved.starting_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(saved.codex_preset, Some(CodexPreset::Auto));
        // Reopened, the fields show what was saved.
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("form")
        };
        assert_eq!(form.value(MODE).as_deref(), Some("acceptEdits"));
        assert_eq!(form.value(APPROVALS).as_deref(), Some("Auto"));
        // `codex` saves Codex's configured model; its modes only.
        fill(&mut ui, ["", "codex", "", "plan", ""])?;
        ui.save_defaults();
        let saved = ui.storage.load_config()?.agent_defaults;
        assert_eq!((saved.provider, saved.model), (Some(Provider::Codex), None));
        assert_eq!(saved.starting_mode.as_deref(), Some("plan"));
        assert_eq!(saved.codex_preset, None);
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &ui.modal else {
            anyhow::bail!("form")
        };
        assert_eq!(form.value(1).as_deref(), Some("codex"));
        // Blank again returns the provider to automatic.
        fill(&mut ui, ["", "", "", "", ""])?;
        ui.save_defaults();
        let saved = ui.storage.load_config()?.agent_defaults;
        assert_eq!(saved, AgentDefaults::default());
        Ok(())
    }

    #[test]
    fn mode_and_approval_lists_follow_the_model_and_refuse_unknown_values() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = ui(&dir)?;
        ui.open_defaults();
        fill(&mut ui, ["", "", "", "acceptEdits", ""])?;
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("form")
        };
        // Choosing a Codex model drops the Claude-only mode.
        *form.fields.get_mut(1).context("model")? = Editor::from("gpt-5.6");
        form.changed();
        assert_eq!(form.value(MODE), None);
        form.field = MODE;
        assert_eq!(form.options(&ui.model_completion), ["default", "plan"]);
        assert!(
            form.option_text("plan").starts_with("plan · Plan"),
            "{}",
            form.option_text("plan")
        );
        form.field = APPROVALS;
        assert_eq!(
            form.options(&ui.model_completion),
            ["Ask", "Auto", "Full", "ReadOnly"]
        );
        *form.fields.get_mut(APPROVALS).context("approvals")? = Editor::from("read");
        assert_eq!(form.options(&ui.model_completion), ["ReadOnly"]);
        *form.fields.get_mut(APPROVALS).context("approvals")? = Editor::from("yolo");
        ui.save_defaults();
        assert!(
            matches!(&ui.modal, Some(Modal::AgentDefaults(form))
                if form.error.as_deref().is_some_and(|e| e.contains("Unknown Codex approvals"))),
            "an unknown preset is refused"
        );
        assert_eq!(
            ui.storage.load_config()?.agent_defaults,
            AgentDefaults::default(),
            "nothing saved"
        );
        Ok(())
    }

    #[test]
    fn the_service_says_what_a_blank_model_becomes() -> Result<()> {
        let mut form = Settings::new(AgentDefaults::default());
        assert_eq!(form.summary(), None);
        form.receive(super::super::super::effective::Defaults::new(
            AgentDefaults::default(),
            || Some(super::super::super::effective::tests::catalog()),
            true,
        ));
        assert_eq!(
            form.summary().as_deref(),
            Some("New sessions here: Claude Code · claude/claude-opus-4-7 · xhigh · automatic")
        );
        assert!(form.server);
        form.field = MODE;
        let models = models::State::default();
        assert!(form.options(&models).contains(&"auto".to_owned()));
        // Without Claude models a blank model is Codex's configured one.
        let mut codex = Settings::new(AgentDefaults::default());
        codex.receive(super::super::super::effective::Defaults::new(
            AgentDefaults::default(),
            || None,
            false,
        ));
        assert_eq!(codex.model_provider(), Provider::Codex);
        Ok(())
    }

    #[test]
    fn legacy_defaults_patch_fields_and_clear_mode_only_on_provider_change() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        storage.update_config(|c| {
            c.agent_defaults.provider = Some(super::super::super::provider::Provider::Claude);
            c.agent_defaults.starting_mode = Some("acceptEdits".into());
            c.agent_defaults.model = Some("claude/sonnet".into());
        })?;
        let mut ui = Ui::new(storage.clone(), &storage.load_config()?);
        ui.model_completion.options = vec![crate::model::ModelInfo {
            id: "fixture-model".into(),
            name: "Fixture".into(),
            efforts: vec![],
        }];
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("form")
        };
        *form.fields.get_mut(1).context("model")? = Editor::from("fixture-model");
        storage.update_config(|c| c.agent_defaults.repository = Some("/concurrent-repo".into()))?;
        ui.save_defaults();
        let saved = storage.load_config()?;
        assert_eq!(
            saved.agent_defaults.provider,
            Some(super::super::super::provider::Provider::Codex)
        );
        assert!(saved.agent_defaults.starting_mode.is_none());
        assert_eq!(
            saved.agent_defaults.repository,
            Some("/concurrent-repo".into())
        );
        assert!(ui.modal.is_none());
        ui.open_defaults();
        let Some(Modal::AgentDefaults(form)) = &mut ui.modal else {
            anyhow::bail!("form")
        };
        *form.fields.get_mut(1).context("model")? = Editor::from("another-model");
        storage.update_config(|c| {
            c.agent_defaults.provider = Some(super::super::super::provider::Provider::Claude)
        })?;
        let bytes = std::fs::read(&storage.config)?;
        ui.save_defaults();
        assert!(matches!(&ui.modal, Some(Modal::AgentDefaults(form)) if form.error.is_some()));
        assert_eq!(std::fs::read(&storage.config)?, bytes);
        Ok(())
    }
}
