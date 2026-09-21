use super::*;
use crate::storage::AgentDefaults;

pub struct Settings {
    pub fields: Vec<Editor>,
    pub field: usize,
    pub isolated: bool,
    pub error: Option<String>,
}
impl Settings {
    fn new(defaults: AgentDefaults) -> Self {
        Self {
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
            isolated: defaults.isolated,
            error: None,
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
                ))))
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
            isolated: form.isolated,
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
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Tab => form.field = (form.field + 1) % 5,
            KeyCode::BackTab => form.field = (form.field + 4) % 5,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) || form.field == 4 => {
                self.save_defaults()
            }
            KeyCode::Enter | KeyCode::Char(' ') if form.field == 3 => {
                form.isolated = !form.isolated
            }
            KeyCode::Enter => form.field = (form.field + 1) % 5,
            _ => {
                if let Some(editor) = form.fields.get_mut(form.field) {
                    editor.key(key);
                }
            }
        }
    }
    pub(super) fn draw_defaults(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::AgentDefaults(form)) = &self.modal else {
            return;
        };
        let mut buttons = Vec::new();
        let labels = [
            "Default local repository",
            "Default model · blank inherits Codex",
            "Default reasoning · blank inherits Codex",
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
        let text = if form.isolated {
            "[x] New isolated worktree"
        } else {
            "[ ] Use the repository directory"
        };
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(if form.field == 3 {
                ACCENT
            } else {
                TEXT
            })),
            toggle,
        );
        buttons.push((toggle, Action::DefaultToggle));
        let save = Rect::new(area.x, area.y + 15, area.width, 1).intersection(area);
        frame.render_widget(
            Paragraph::new("[ Save defaults · Ctrl+Enter ]")
                .style(Style::default().fg(if form.field == 4 { ACCENT } else { TEXT })),
            save,
        );
        buttons.push((save, Action::DefaultSave));
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
