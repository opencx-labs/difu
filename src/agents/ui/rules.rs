use super::*;
use std::path::PathBuf;

type Loaded = std::result::Result<(PathBuf, String), String>;

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
        std::thread::spawn(move || {
            let result = super::super::rules::load(&storage, &path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
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
        let path = path
            .or_else(|| {
                self.selected
                    .as_ref()
                    .and_then(|id| self.sessions.get(id))
                    .map(|s| s.job.root().clone())
            })
            .or_else(|| config.agent_defaults.repository.clone())
            .or_else(|| std::env::current_dir().ok());
        let mut candidates: Vec<_> = config
            .repository_rules
            .keys()
            .cloned()
            .chain(config.repositories.values().cloned())
            .chain(config.agent_defaults.repository)
            .chain(self.summaries.iter().filter_map(|s| s.repository.clone()))
            .collect();
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
        let Some(result) = form.loading.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
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
            self.modal = form.return_to.map(Modal::AgentDefaults);
        }
    }
    pub(super) fn save_rules(&mut self) {
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        let Some(root) = &form.root else { return };
        if form.repository.text() != root.to_string_lossy() {
            form.error = Some("Press Enter to select the repository first".into());
            return;
        }
        let text = form.text.text();
        let result = self.storage.load_config().and_then(|mut config| {
            if text.trim().is_empty() {
                config.repository_rules.remove(root);
            } else {
                config.repository_rules.insert(root.clone(), text);
            }
            self.storage.save_config(&config)
        });
        match result {
            Ok(()) => {
                self.close_rules();
                self.notice = Some(("Repository rules saved for new sessions".into(), false));
            }
            Err(error) => form.error = Some(format!("Cannot save rules: {error:#}")),
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
            && form.loading.is_none()
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
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.close_rules(),
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL) =>
            {
                self.save_rules()
            }
            _ if form.loading.is_some() => {}
            KeyCode::Enter if form.field == 0 => {
                let path = form
                    .options()
                    .get(form.selected)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(form.repository.text()));
                form.load(self.storage.clone(), path);
            }
            KeyCode::Up | KeyCode::Down if form.field == 0 => {
                form.selected = if key.code == KeyCode::Up {
                    form.selected.saturating_sub(1)
                } else {
                    (form.selected + 1).min(form.options().len().saturating_sub(1))
                };
            }
            KeyCode::Tab | KeyCode::BackTab => {
                form.field = (form.field + if key.code == KeyCode::Tab { 1 } else { 3 }) % 4;
            }
            KeyCode::Enter if form.field == 2 => self.save_rules(),
            KeyCode::Enter if form.field == 3 => self.close_rules(),
            _ if form.field == 0 => {
                form.repository.key(key);
                form.selected = 0;
            }
            _ if form.field == 1 && form.root.is_some() => {
                form.text.key(key);
            }
            _ => {}
        }
    }
    pub(super) fn draw_rules(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::RepositoryRules(form)) = &self.modal else {
            return;
        };
        let repository = Rect::new(area.x, area.y, area.width, 3).intersection(area);
        editor(
            frame,
            repository,
            "Repository · Enter selects",
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
            "Rules",
            &form.text,
            form.field == 1 && form.root.is_some(),
        );
        self.hits.push((text, Action::RulesField(1)));
        let footer = area.bottom().saturating_sub(3);
        frame.render_widget(
            Paragraph::new(
                "Included in the initial instructions for new sessions in this repository.",
            )
            .style(Style::default().fg(DIM)),
            Rect::new(area.x, footer, area.width, 1),
        );
        for (field, label) in [(2, "Save · ⌘Enter / Ctrl+Enter"), (3, "Cancel · Esc")] {
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
        } else if form.loading.is_some() {
            frame.render_widget(
                Paragraph::new("Loading repository rules…").style(Style::default().fg(DIM)),
                Rect::new(area.x, footer + 2, area.width, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn multiline_rules_save_cancel_and_clear_without_changing_other_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().to_owned();
        let storage = Storage {
            config: root.join("config.json"),
            cache: root.join("cache"),
        };
        let mut config = Config::default();
        config.pinned_sessions.insert("pinned".into());
        storage.save_config(&config)?;
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
            }))
        };
        ui.modal = Some(form(""));
        ui.rules_paste("First rule");
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.rules_paste("Second rule");
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER));
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
        assert!(storage.load_config()?.repository_rules.is_empty());
        Ok(())
    }
}
