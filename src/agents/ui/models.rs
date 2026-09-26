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
            self.model_completion.receiver = None;
            self.model_completion.cancel = None;
            match output {
                Ok(models) => self.model_completion.options = models,
                Err(error) => self.model_completion.error = Some(error),
            }
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
                .style(Style::default().fg(if selected { ACCENT } else { TEXT })),
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
