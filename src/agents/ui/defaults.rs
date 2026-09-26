use super::*;
use crate::storage::AgentDefaults;

pub struct Settings {
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
        let provider = super::super::provider::Provider::for_model(defaults.model.as_deref());
        Self {
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
        let provider = super::super::provider::Provider::for_model(self.value(1).as_deref());
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
            self.directory_rx = None;
            match result {
                Ok(names) => self.directories = names,
                Err(error) => self.directory_error = Some(error),
            }
        }
        if self.field == 2 && self.value(1).is_none() {
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
        let defaults = AgentDefaults {
            repository: form.value(0).map(std::path::PathBuf::from),
            model: form.value(1),
            effort: form.value(2),
            isolated: true,
        };
        let result = self.storage.load_config().and_then(|mut config| {
            config.agent_defaults = defaults.clone();
            self.storage.save_config(&config)
        });
        match result {
            Ok(()) => {
                self.defaults = defaults;
                self.modal = None;
                self.notice = Some(("Agent defaults saved for new sessions".into(), false));
            }
            Err(error) => form.error = Some(format!("Cannot save defaults: {error:#}")),
        }
    }
    pub(super) fn defaults_key(&mut self, key: KeyEvent) {
        let Some(Modal::AgentDefaults(form)) = &mut self.modal else {
            return;
        };
        let options = form.options(&self.model_completion);
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Tab | KeyCode::BackTab => {
                form.field = (form.field + if key.code == KeyCode::Tab { 1 } else { 3 }) % 4;
                form.changed();
            }
            KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() && form.field < 3 => {
                form.selected = if key.code == KeyCode::Up {
                    form.selected.saturating_sub(1)
                } else {
                    (form.selected + 1).min(options.len().saturating_sub(1))
                };
                form.filled = false;
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) || form.field == 3 => {
                self.save_defaults()
            }
            KeyCode::Enter => {
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
            _ => {
                if let Some(editor) = form.fields.get_mut(form.field) {
                    editor.key(key);
                    form.changed();
                }
            }
        }
    }
    pub(super) fn tick_defaults(&mut self) {
        if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
            form.poll();
        }
    }
    pub(super) fn default_option(&mut self, index: usize) {
        if let Some(Modal::AgentDefaults(form)) = &mut self.modal {
            form.selected = index;
            form.filled = false;
        }
        self.defaults_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }
    pub(super) fn draw_defaults(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::AgentDefaults(form)) = &self.modal else {
            return;
        };
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
        frame.render_widget(
            Paragraph::new("New sessions always use an isolated worktree")
                .style(Style::default().fg(TEXT)),
            toggle,
        );
        let save = Rect::new(area.x, area.y + 15, area.width, 1).intersection(area);
        frame.render_widget(
            Paragraph::new("[ Save defaults · Ctrl+Enter ]")
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
                    .style(Style::default().fg(if index == selected {
                        ACCENT
                    } else {
                        TEXT
                    })),
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
                Paragraph::new("↑/↓ select · Enter fills · Tab fields · Ctrl+Enter saves")
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
