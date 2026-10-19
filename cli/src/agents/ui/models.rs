use super::*;
use crate::{model::ModelInfo, process::Cancel};

#[derive(Default)]
pub(super) struct State {
    pub options: Vec<ModelInfo>,
    receiver: Option<mpsc::Receiver<Result<Vec<ModelInfo>, String>>>,
    cancel: Option<Cancel>,
    error: Option<String>,
    selected: usize,
    filled: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}
impl State {
    pub(super) fn load(&mut self) {
        self.selected = 0;
        self.filled = false;
        if !self.options.is_empty() || self.receiver.is_some() {
            return;
        }
        self.error = None;
        let cancel = Cancel::default();
        self.cancel = Some(cancel.clone());
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let (codex, claude) = thread::scope(|scope| {
                let codex = scope.spawn(|| crate::codex::models(&cancel));
                let claude = super::super::claude::models(&cancel);
                (
                    codex
                        .join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Codex model discovery stopped"))),
                    claude,
                )
            });
            let mut models = Vec::new();
            let mut errors = Vec::new();
            for result in [codex, claude] {
                match result {
                    Ok(options) => models.extend(options),
                    Err(error) => errors.push(format!("{error:#}")),
                }
            }
            let _ = tx.send(if models.is_empty() {
                Err(errors.join("; "))
            } else {
                Ok(models)
            });
        });
    }
    pub(super) fn choices(&self, model: &str, effort: &str, choosing_model: bool) -> Vec<String> {
        let query = if choosing_model { model } else { effort }.to_lowercase();
        if choosing_model {
            self.options
                .iter()
                .filter(|m| {
                    m.id.to_lowercase().contains(&query) || m.name.to_lowercase().contains(&query)
                })
                .map(|m| m.id.clone())
                .collect()
        } else {
            self.options
                .iter()
                .find(|m| m.id == model)
                .map(|m| {
                    m.efforts
                        .iter()
                        .filter(|e| e.to_lowercase().contains(&query))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        }
    }
    pub(super) fn empty_label(&self) -> &str {
        if self.receiver.is_some() {
            "Loading model options…"
        } else {
            self.error.as_deref().unwrap_or("No matching options")
        }
    }
    fn changed(&mut self) {
        self.selected = 0;
        self.filled = false;
    }
}
impl Ui {
    pub(super) fn open_model(&mut self, field: usize) {
        let session = self.selected.as_ref().and_then(|id| self.sessions.get(id));
        self.modal = Some(Modal::Model {
            model: Editor::from(session.and_then(|s| s.model.as_deref()).unwrap_or_default()),
            effort: Editor::from(
                session
                    .and_then(|s| s.effort.as_deref())
                    .unwrap_or_default(),
            ),
            field,
        });
        self.model_completion.load();
    }
    pub(super) fn tick_models(&mut self) {
        let output = self
            .model_completion
            .receiver
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(output) => Some(output),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Model discovery stopped".into()))
                }
            });
        if let Some(output) = output {
            crate::repaint::request();
            self.model_completion.receiver = None;
            self.model_completion.cancel = None;
            match output {
                Ok(models) => self.model_completion.options = models,
                Err(error) => self.model_completion.error = Some(error),
            }
            self.place_picker();
        }
    }
    fn model_options(&self) -> Vec<String> {
        let Some(Modal::Model {
            model,
            effort,
            field,
        }) = &self.modal
        else {
            return Vec::new();
        };
        self.model_completion
            .choices(&model.text(), &effort.text(), *field == 0)
    }
    pub(super) fn model_paste(&mut self, text: &str) {
        if let Some(Modal::Model {
            model,
            effort,
            field,
        }) = &mut self.modal
        {
            if *field == 0 {
                let before = super::super::provider::Provider::for_model(Some(&model.text()));
                model.insert(text);
                if before != super::super::provider::Provider::for_model(Some(&model.text())) {
                    *effort = Editor::default();
                }
            } else {
                effort.insert(text);
            }
            self.model_completion.changed();
        }
    }
    pub(super) fn model_key(&mut self, key: KeyEvent) {
        let options = self.model_options();
        let Some(Modal::Model {
            model,
            effort,
            field,
        }) = &mut self.modal
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Tab | KeyCode::BackTab => {
                *field = 1usize.saturating_sub(*field);
                self.model_completion.changed();
            }
            KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                self.model_completion.selected = if key.code == KeyCode::Up {
                    self.model_completion.selected.saturating_sub(1)
                } else {
                    self.model_completion
                        .selected
                        .saturating_add(1)
                        .min(options.len().saturating_sub(1))
                };
                self.model_completion.filled = false;
            }
            KeyCode::Enter => {
                if !self.model_completion.filled
                    && let Some(value) = options.get(self.model_completion.selected)
                {
                    if *field == 0
                        && super::super::provider::Provider::for_model(Some(&model.text()))
                            != super::super::provider::Provider::for_model(Some(value))
                    {
                        *effort = Editor::default();
                    }
                    let editor = if *field == 0 { model } else { effort };
                    *editor = Editor::from(value.as_str());
                    self.model_completion.selected = 0;
                    self.model_completion.filled = true;
                } else {
                    let model = model.text();
                    let effort = effort.text();
                    self.control(Control::Model {
                        model: (!model.is_empty()).then_some(model),
                        effort: (!effort.is_empty()).then_some(effort),
                    });
                }
            }
            _ => {
                if *field == 0 {
                    let before = super::super::provider::Provider::for_model(Some(&model.text()));
                    model.key(key);
                    if before != super::super::provider::Provider::for_model(Some(&model.text())) {
                        *effort = Editor::default();
                    }
                } else {
                    effort.key(key);
                }
                self.model_completion.changed();
            }
        }
    }
    pub(super) fn draw_model(&mut self, frame: &mut Frame, area: Rect) {
        let options = self.model_options();
        let Some(Modal::Model {
            model,
            effort,
            field,
        }) = &self.modal
        else {
            return;
        };
        for (index, value, label) in [(0, model, "Model"), (1, effort, "Reasoning effort")] {
            let rect = Rect::new(
                area.x,
                area.y.saturating_add(index as u16 * 4),
                area.width,
                3,
            )
            .intersection(area);
            editor(frame, rect, label, value, *field == index);
            self.hits.push((rect, Action::ModelField(index)));
        }
        let top = area.y.saturating_add(8);
        let height = area.bottom().saturating_sub(top).saturating_sub(2);
        let state = &mut self.model_completion;
        state.selected = state.selected.min(options.len().saturating_sub(1));
        let first = state
            .selected
            .saturating_sub(usize::from(height).saturating_sub(1));
        for (index, value) in options
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(height))
        {
            let rect = Rect::new(
                area.x,
                top.saturating_add((index - first) as u16),
                area.width,
                1,
            )
            .intersection(area);
            let selected = index == state.selected;
            frame.render_widget(
                Paragraph::new(format!(
                    "{} {}",
                    if selected { "›" } else { " " },
                    crate::model::clean(value)
                ))
                .style(crate::ui::option_style(selected)),
                rect,
            );
            self.hits.push((rect, Action::ModelOption(index)));
        }
        if options.is_empty() && height > 0 {
            let label = if state.receiver.is_some() {
                "Loading model options…"
            } else {
                state.error.as_deref().unwrap_or("No matching options")
            };
            frame.render_widget(
                Paragraph::new(crate::model::clean(label))
                    .style(Style::default().fg(DIM))
                    .wrap(Wrap { trim: false }),
                Rect::new(area.x, top, area.width, height).intersection(area),
            );
        }
        let hint = if state.filled {
            "Enter applies · Tab switches fields · Esc cancels"
        } else {
            "Type to filter · ↑/↓ select · Enter fills · Tab switches fields"
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(DIM)),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1).intersection(area),
        );
    }
    pub(super) fn model_field(&mut self, index: usize) {
        if let Some(Modal::Model { field, .. }) = &mut self.modal {
            *field = index;
            self.model_completion.changed();
        }
    }
    pub(super) fn model_option(&mut self, index: usize) {
        self.model_completion.selected = index;
        self.model_completion.filled = false;
        self.model_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }
}

/// Claude Code-style model picker shared by Codex and Claude Code sessions.
/// Enter applies the pick to the current session and saves it as the default
/// for new sessions; `s` changes the current session only.
pub struct Picker {
    selected: usize,
    effort: Option<String>,
    /// The model the selection snaps to once the catalog arrives.
    current: Option<String>,
    placed: bool,
}
impl Picker {
    fn effort_options(model: &ModelInfo) -> Vec<Option<String>> {
        std::iter::once(None)
            .chain(model.efforts.iter().cloned().map(Some))
            .collect()
    }
    fn place(&mut self, options: &[ModelInfo]) {
        if self.placed || options.is_empty() {
            return;
        }
        self.placed = true;
        if let Some(index) = self
            .current
            .as_ref()
            .and_then(|current| options.iter().position(|m| &m.id == current))
        {
            self.selected = index;
        } else {
            self.selected = 0;
        }
        self.select(self.selected, options);
    }
    fn select(&mut self, index: usize, options: &[ModelInfo]) {
        let Some(model) = options.get(index) else {
            return;
        };
        self.selected = index;
        if !model
            .efforts
            .iter()
            .any(|e| Some(e) == self.effort.as_ref())
        {
            self.effort = None;
        }
    }
    fn adjust_effort(&mut self, forward: bool, options: &[ModelInfo]) {
        let Some(model) = options.get(self.selected) else {
            return;
        };
        let efforts = Self::effort_options(model);
        let index = efforts
            .iter()
            .position(|e| *e == self.effort)
            .unwrap_or_default();
        let index = if forward {
            (index + 1) % efforts.len()
        } else {
            (index + efforts.len() - 1) % efforts.len()
        };
        self.effort = efforts.get(index).cloned().flatten();
    }
}
fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
impl Ui {
    pub(super) fn open_model_picker(&mut self) {
        let session = self.selected.as_ref().and_then(|id| self.sessions.get(id));
        let (current, effort) = match session.and_then(|s| s.model.clone()) {
            Some(model) => (Some(model), session.and_then(|s| s.effort.clone())),
            None => (self.defaults.model.clone(), self.defaults.effort.clone()),
        };
        self.modal = Some(Modal::ModelPicker(Box::new(Picker {
            selected: 0,
            effort,
            current,
            placed: false,
        })));
        self.model_completion.load();
        self.place_picker();
    }
    pub(super) fn place_picker(&mut self) {
        if let Some(Modal::ModelPicker(picker)) = &mut self.modal {
            picker.place(&self.model_completion.options);
        }
    }
    pub(super) fn picker_option(&mut self, index: usize) {
        if let Some(Modal::ModelPicker(picker)) = &mut self.modal {
            picker.select(index, &self.model_completion.options);
        }
    }
    pub(super) fn picker_key(&mut self, key: KeyEvent) {
        let Some(Modal::ModelPicker(picker)) = &mut self.modal else {
            return;
        };
        let options = &self.model_completion.options;
        let last = options.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Up => picker.select(picker.selected.saturating_sub(1), options),
            KeyCode::Down => picker.select(picker.selected.saturating_add(1).min(last), options),
            KeyCode::PageUp => picker.select(picker.selected.saturating_sub(10), options),
            KeyCode::PageDown => {
                picker.select(picker.selected.saturating_add(10).min(last), options)
            }
            KeyCode::Home => picker.select(0, options),
            KeyCode::End => picker.select(last, options),
            KeyCode::Left | KeyCode::Right => {
                picker.adjust_effort(key.code == KeyCode::Right, options)
            }
            KeyCode::Char(digit @ '1'..='9') if key.modifiers.is_empty() => {
                let index = usize::from(digit as u8 - b'1');
                if index < options.len() {
                    picker.select(index, options);
                }
            }
            KeyCode::Enter => self.picker_apply(true),
            KeyCode::Char('s') if key.modifiers.is_empty() => self.picker_apply(false),
            KeyCode::Char('t') if key.modifiers.is_empty() => self.open_model(0),
            _ => {}
        }
    }
    fn picker_apply(&mut self, default: bool) {
        let Some(Modal::ModelPicker(picker)) = &self.modal else {
            return;
        };
        let Some(model) = self.model_completion.options.get(picker.selected) else {
            return;
        };
        let (id, name, effort) = (model.id.clone(), model.name.clone(), picker.effort.clone());
        let applies = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .is_some_and(|s| matches!(s.job, Job::Coding(_)));
        if !default && !applies {
            self.notice = Some(("Select a coding session to change its model".into(), true));
            return;
        }
        self.modal = None;
        if default {
            let result = self.storage.load_config().and_then(|mut config| {
                config.agent_defaults.model = Some(id.clone());
                config.agent_defaults.effort = effort.clone();
                self.storage.save_config(&config)
            });
            match result {
                Ok(()) => {
                    self.defaults.model = Some(id.clone());
                    self.defaults.effort = effort.clone();
                    self.notice = Some((
                        format!(
                            "{} is now the default model for new sessions",
                            crate::model::clean(&name)
                        ),
                        false,
                    ));
                }
                Err(error) => {
                    self.notice = Some((format!("Cannot save default model: {error:#}"), true));
                    return;
                }
            }
        }
        if applies {
            self.control(Control::Model {
                model: Some(id),
                effort,
            });
        }
    }
    pub(super) fn draw_picker(&mut self, frame: &mut Frame, area: Rect) {
        self.place_picker();
        let Some(Modal::ModelPicker(picker)) = &mut self.modal else {
            return;
        };
        let options = &self.model_completion.options;
        let current = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .and_then(|s| s.model.clone());
        let intro = Rect::new(area.x, area.y, area.width, 2.min(area.height));
        frame.render_widget(
            Paragraph::new(
                "Switch between Codex and Claude Code models. Enter also makes your pick the default for new sessions.",
            )
            .style(Style::default().fg(DIM))
            .wrap(Wrap { trim: false }),
            intro,
        );
        let top = area.y.saturating_add(3);
        let height = usize::from(area.bottom().saturating_sub(top).saturating_sub(6));
        if options.is_empty() {
            frame.render_widget(
                Paragraph::new(crate::model::clean(self.model_completion.empty_label()))
                    .style(Style::default().fg(DIM))
                    .wrap(Wrap { trim: false }),
                Rect::new(area.x, top, area.width, height as u16).intersection(area),
            );
        }
        picker.selected = picker.selected.min(options.len().saturating_sub(1));
        let first = picker
            .selected
            .saturating_sub(height.saturating_sub(1))
            .min(options.len().saturating_sub(height));
        let name_width = options
            .iter()
            .map(|m| m.name.chars().count())
            .max()
            .unwrap_or_default()
            .min(usize::from(area.width) / 2);
        for (index, model) in options.iter().enumerate().skip(first).take(height) {
            let rect =
                Rect::new(area.x, top + (index - first) as u16, area.width, 1).intersection(area);
            let selected = index == picker.selected;
            let marker = if selected {
                "›"
            } else if index == first && first > 0 {
                "↑"
            } else if index + 1 == first + height && index + 1 < options.len() {
                "↓"
            } else {
                " "
            };
            let name = crate::model::clean(&model.name);
            let name = if name.is_empty() {
                model.id.clone()
            } else {
                name
            };
            let padding = name_width.saturating_sub(name.chars().count());
            let base = crate::ui::option_style(selected);
            let dim = if selected {
                base
            } else {
                Style::default().fg(DIM).bg(PANEL)
            };
            let mut spans = vec![
                Span::styled(format!("{marker}{:>3}. ", index + 1), dim),
                Span::styled(name, base),
                Span::styled(
                    if Some(&model.id) == current.as_ref() {
                        " ✔"
                    } else {
                        "  "
                    },
                    if selected {
                        base
                    } else {
                        Style::default().fg(GREEN).bg(PANEL)
                    },
                ),
                Span::styled(
                    format!("{} {}", " ".repeat(padding), crate::model::clean(&model.id)),
                    dim,
                ),
            ];
            if Some(&model.id) == self.defaults.model.as_ref() {
                spans.push(Span::styled(" · default", dim));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)).style(base), rect);
            self.hits.push((rect, Action::PickerOption(index)));
        }
        let effort = picker
            .effort
            .as_deref()
            .map(capitalize)
            .unwrap_or_else(|| "Default".into());
        let supports = options
            .get(picker.selected)
            .is_some_and(|m| !m.efforts.is_empty());
        let effort_row =
            Rect::new(area.x, area.bottom().saturating_sub(4), area.width, 1).intersection(area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("● ", Style::default().fg(ACCENT)),
                Span::raw(format!("{effort} effort")),
                Span::styled(
                    if supports {
                        "  ←/→ to adjust"
                    } else {
                        "  (this model has no reasoning levels)"
                    },
                    Style::default().fg(DIM),
                ),
            ])),
            effort_row,
        );
        frame.render_widget(
            Paragraph::new(
                "Enter · set as default and use now  ·  s · this session only  ·  1-9 · jump  ·  t · type a custom model  ·  Esc · cancel",
            )
            .style(Style::default().fg(DIM))
            .wrap(Wrap { trim: false }),
            Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 2).intersection(area),
        );
    }
}
