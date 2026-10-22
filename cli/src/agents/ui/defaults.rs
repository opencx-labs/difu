use super::*;
use crate::{
    shortcuts::{Bindings, ids::*},
    storage::AgentDefaults,
};

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
}
impl Settings {
    fn new(defaults: AgentDefaults) -> Self {
        let provider = defaults.resolved_provider();
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
                Editor::from(defaults.model.unwrap_or_default()),
                Editor::from(defaults.effort.unwrap_or_default()),
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
        }
    }
    pub(super) fn changed(&mut self) {
        let provider = self
            .value(1)
            .map(|model| super::super::provider::Provider::for_model(Some(&model)))
            .unwrap_or(self.draft.config().agent_defaults.resolved_provider());
        if provider != self.provider {
            if let Some(effort) = self.fields.get_mut(2) {
                *effort = Editor::default();
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
            && self.value(1).is_none()
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
        } else if self.field < 3 {
            let model = self
                .value(1)
                .or_else(|| (self.field == 2).then(|| self.inherited.clone()).flatten())
                .unwrap_or_default();
            models.choices(&model, &self.value(2).unwrap_or_default(), self.field == 1)
        } else {
            Vec::new()
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
                self.modal = Some(Modal::AgentDefaults(Box::new(Settings::new(
                    config.agent_defaults,
                ))));
                self.model_completion.load();
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
            let model = form.value(1);
            let effort = form.value(2);
            if model != form.draft.config().agent_defaults.model {
                form.draft.select_default_model(model, effort.clone())?;
            } else if effort != form.draft.config().agent_defaults.effort {
                form.draft
                    .set(SettingId::DefaultEffort, SettingValue::Text(effort))?;
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
            Ok(self.storage.save_settings_draft(&mut form.draft)?)
        })();
        match result {
            Ok(config) => {
                self.defaults = config.agent_defaults;
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
                        3
                    })
                    % 4;
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
        if form.field == 3 {
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
            form.field = (form.field + 1) % 4;
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
            "Default model · blank inherits Codex",
            "Default reasoning · blank uses provider default",
        ];
        for (i, label) in labels.into_iter().enumerate() {
            let rect = Rect::new(area.x, area.y.saturating_add(i as u16 * 4), area.width, 3)
                .intersection(area);
            if let Some(value) = form.fields.get(i) {
                editor(frame, rect, label, value, form.field == i);
            }
            buttons.push((rect, Action::DefaultField(i)));
        }
        let toggle = Rect::new(area.x, area.y + 12, area.width, 1).intersection(area);
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
        frame.render_widget(
            Paragraph::new("New sessions always use an isolated worktree")
                .style(Style::default().fg(TEXT)),
            toggle,
        );
        let save = Rect::new(area.x, area.y + 15, area.width, 1).intersection(area);
        frame.render_widget(
            Paragraph::new(format!("[ Save defaults · {} ]", hint(DEFAULTS_SAVE)))
                .style(Style::default().fg(if form.field == 3 { ACCENT } else { TEXT })),
            save,
        );
        buttons.push((save, Action::DefaultSave));
        let options = form.options(&self.model_completion);
        let top = area.y.saturating_add(17);
        let available = area.bottom().saturating_sub(top).saturating_sub(1);
        let selected = form.selected.min(options.len().saturating_sub(1));
        let first = selected.saturating_sub(usize::from(available).saturating_sub(1));
        if form.field < 3 {
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
                        crate::model::clean(option)
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
                Rect::new(
                    area.x,
                    area.y + 17,
                    area.width,
                    area.height.saturating_sub(17),
                )
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
        assert_eq!(form.options(&models).len(), 2); // Empty model query still shows the whole catalog.
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
        for field in 0..3 {
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
        for field in 0..4 {
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
        for field in 0..4 {
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
        for field in 0..4 {
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
                if field < 3 {
                    crate::shortcuts::contexts::AGENT_DEFAULTS
                } else {
                    crate::shortcuts::contexts::AGENT_DEFAULTS_BUTTON
                },
                (field < 3).then_some("d"),
            )?;
        }
        Ok(())
    }
    #[test]
    fn defaults_activate_fills_or_advances_each_field_and_saves_only_on_button() -> Result<()> {
        for field in 0..4 {
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
                if field == 3 {
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
        for field in 0..4 {
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
