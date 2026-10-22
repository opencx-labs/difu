use super::*;
use crate::{
    model::ModelInfo,
    process::Cancel,
    shortcuts::{Bindings, contexts::*, ids::*},
};

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
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let action = self.shortcut(key);
        let options = self.model_options();
        let Some(Modal::Model {
            model,
            effort,
            field,
        }) = &mut self.modal
        else {
            return;
        };
        match action {
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                *field = 1usize.saturating_sub(*field);
                self.model_completion.changed();
            }
            Some(NAVIGATION_UP | NAVIGATION_DOWN) => {
                self.model_completion.selected = if action == Some(NAVIGATION_UP) {
                    self.model_completion.selected.saturating_sub(1)
                } else {
                    self.model_completion
                        .selected
                        .saturating_add(1)
                        .min(options.len().saturating_sub(1))
                };
                self.model_completion.filled = false;
            }
            Some(NAVIGATION_ACTIVATE) => self.model_activate(),
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
    fn model_activate(&mut self) {
        let options = self.model_options();
        let Some(Modal::Model {
            model,
            effort,
            field,
        }) = &mut self.modal
        else {
            return;
        };
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
        let bindings = Bindings::new(&self.shortcut_config);
        let hint = |action| bindings.primary_hint_in(action, AGENT_MODEL);
        let hint = if state.filled {
            format!(
                "{} applies · {}/{} fields · Esc cancels",
                hint(NAVIGATION_ACTIVATE),
                hint(NAVIGATION_NEXT_FOCUS),
                hint(NAVIGATION_PREVIOUS_FOCUS)
            )
        } else {
            format!(
                "Type to filter · {}/{} select · {} fills · {}/{} fields",
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN),
                hint(NAVIGATION_ACTIVATE),
                hint(NAVIGATION_NEXT_FOCUS),
                hint(NAVIGATION_PREVIOUS_FOCUS)
            )
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
        self.model_activate();
    }
}

const MODEL_CHOICES: [crate::shortcuts::ActionId; 9] = [
    MODELS_CHOOSE_1,
    MODELS_CHOOSE_2,
    MODELS_CHOOSE_3,
    MODELS_CHOOSE_4,
    MODELS_CHOOSE_5,
    MODELS_CHOOSE_6,
    MODELS_CHOOSE_7,
    MODELS_CHOOSE_8,
    MODELS_CHOOSE_9,
];

/// Claude Code-style model picker shared by Codex and Claude Code sessions.
/// Enter applies the pick to the current session and saves it as the default
/// for new sessions; `s` changes the current session only.
pub struct Picker {
    defaults: crate::settings::SettingsDraft,
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
        let defaults = match self.storage.settings_draft() {
            Ok(defaults) => defaults,
            Err(error) => {
                self.notice = Some((format!("Cannot read defaults: {error:#}"), true));
                return;
            }
        };
        self.modal = Some(Modal::ModelPicker(Box::new(Picker {
            defaults,
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
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let action = self.shortcut(key);
        let Some(Modal::ModelPicker(picker)) = &mut self.modal else {
            return;
        };
        let options = &self.model_completion.options;
        let last = options.len().saturating_sub(1);
        match action {
            Some(NAVIGATION_UP) => picker.select(picker.selected.saturating_sub(1), options),
            Some(NAVIGATION_DOWN) => {
                picker.select(picker.selected.saturating_add(1).min(last), options)
            }
            Some(NAVIGATION_PAGE_UP) => picker.select(picker.selected.saturating_sub(10), options),
            Some(NAVIGATION_PAGE_DOWN) => {
                picker.select(picker.selected.saturating_add(10).min(last), options)
            }
            Some(NAVIGATION_FIRST) => picker.select(0, options),
            Some(NAVIGATION_LAST) => picker.select(last, options),
            Some(MODELS_PREVIOUS_EFFORT | MODELS_NEXT_EFFORT) => {
                picker.adjust_effort(action == Some(MODELS_NEXT_EFFORT), options)
            }
            Some(NAVIGATION_ACTIVATE) => self.picker_apply(true),
            Some(MODELS_SAVE) => self.picker_apply(false),
            Some(MODELS_CUSTOM) => self.open_model(0),
            Some(action) => {
                if let Some(index) = MODEL_CHOICES.iter().position(|id| *id == action) {
                    picker.select(index, options);
                }
            }
            None => {}
        }
    }
    fn picker_apply(&mut self, default: bool) {
        let Some(Modal::ModelPicker(picker)) = &mut self.modal else {
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
        if default {
            let result = picker
                .defaults
                .select_default_model(Some(id.clone()), effort.clone())
                .and_then(|()| {
                    self.storage
                        .save_settings_draft(&mut picker.defaults)
                        .map_err(Into::into)
                });
            match result {
                Ok(config) => {
                    self.defaults = config.agent_defaults;
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
        self.modal = None;
        if applies {
            self.control(Control::Model {
                model: Some(id),
                effort,
            });
        }
    }
    pub(super) fn draw_picker(&mut self, frame: &mut Frame, area: Rect) {
        self.place_picker();
        let bindings = Bindings::new(&self.shortcut_config);
        let hint = |action| bindings.primary_hint_in(action, AGENT_MODEL_PICKER);
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
                format!("Switch between Codex and Claude Code models. {} also makes your pick the default for new sessions.", hint(NAVIGATION_ACTIVATE)),
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
                Span::styled(
                    format!(
                        "{marker}{:>3}. ",
                        MODEL_CHOICES
                            .get(index)
                            .map(|id| hint(*id))
                            .unwrap_or_else(|| (index + 1).to_string())
                    ),
                    dim,
                ),
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
                        format!(
                            "  {}/{} to adjust",
                            hint(MODELS_PREVIOUS_EFFORT),
                            hint(MODELS_NEXT_EFFORT)
                        )
                    } else {
                        "  (this model has no reasoning levels)".into()
                    },
                    Style::default().fg(DIM),
                ),
            ])),
            effort_row,
        );
        frame.render_widget(
            Paragraph::new(
                format!("{} · set as default and use now  ·  {} · this session only  ·  option keys · jump  ·  {} · type a custom model  ·  Esc · cancel", hint(NAVIGATION_ACTIVATE), hint(MODELS_SAVE), hint(MODELS_CUSTOM)),
            )
            .style(Style::default().fg(DIM))
            .wrap(Wrap { trim: false }),
            Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 2).intersection(area),
        );
    }
}

#[cfg(test)]
mod shortcut_regressions {
    use super::*;
    use anyhow::Result;

    fn catalog(ui: &mut Ui) {
        ui.model_completion.options = (1..=12)
            .map(|i| ModelInfo {
                id: format!("fixture-{i}"),
                name: format!("Fixture {i}"),
                efforts: vec!["low".into(), "high".into()],
            })
            .collect();
    }
    fn picker(ui: &mut Ui, selected: usize) {
        ui.modal = Some(Modal::ModelPicker(Box::new(Picker {
            defaults: crate::settings::SettingsDraft::new(Config::default()),
            selected,
            effort: None,
            current: None,
            placed: true,
        })));
    }
    fn selected(ui: &Ui) -> Result<(usize, Option<&str>)> {
        let Some(Modal::ModelPicker(p)) = &ui.modal else {
            anyhow::bail!("picker closed")
        };
        Ok((p.selected, p.effort.as_deref()))
    }
    fn event(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn picker_literal_numbers_navigation_and_effort_replace_old_keys() -> Result<()> {
        for (action, code, start, want, effort) in [
            (MODELS_CHOOSE_1, KeyCode::Char('1'), 10, 0, None),
            (MODELS_CHOOSE_2, KeyCode::Char('2'), 10, 1, None),
            (MODELS_CHOOSE_3, KeyCode::Char('3'), 10, 2, None),
            (MODELS_CHOOSE_4, KeyCode::Char('4'), 10, 3, None),
            (MODELS_CHOOSE_5, KeyCode::Char('5'), 10, 4, None),
            (MODELS_CHOOSE_6, KeyCode::Char('6'), 10, 5, None),
            (MODELS_CHOOSE_7, KeyCode::Char('7'), 10, 6, None),
            (MODELS_CHOOSE_8, KeyCode::Char('8'), 10, 7, None),
            (MODELS_CHOOSE_9, KeyCode::Char('9'), 10, 8, None),
            (NAVIGATION_UP, KeyCode::Up, 5, 4, None),
            (NAVIGATION_DOWN, KeyCode::Down, 5, 6, None),
            (NAVIGATION_PAGE_UP, KeyCode::PageUp, 11, 1, None),
            (NAVIGATION_PAGE_DOWN, KeyCode::PageDown, 0, 10, None),
            (NAVIGATION_FIRST, KeyCode::Home, 5, 0, None),
            (NAVIGATION_LAST, KeyCode::End, 5, 11, None),
            (MODELS_PREVIOUS_EFFORT, KeyCode::Left, 5, 5, Some("high")),
            (MODELS_NEXT_EFFORT, KeyCode::Right, 5, 5, Some("low")),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                catalog(&mut ui);
                picker(&mut ui, start);
                if custom {
                    ui.key(event(code));
                    assert_eq!(selected(&ui)?, (start, None), "removed {}", action.0);
                }
                ui.key(event(if custom { KeyCode::F(12) } else { code }));
                assert_eq!(selected(&ui)?, (want, effort), "{} {phase}", action.0);
            }
        }
        Ok(())
    }

    #[test]
    fn picker_apply_and_session_only_send_exact_model_without_provider() -> Result<()> {
        for (action, code, default) in [
            (MODELS_SAVE, KeyCode::Char('s'), false),
            (NAVIGATION_ACTIVATE, KeyCode::Enter, true),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                catalog(&mut ui);
                picker(&mut ui, 3);
                if let Some(Modal::ModelPicker(p)) = &mut ui.modal {
                    p.effort = Some("high".into());
                }
                if custom {
                    ui.key(event(code));
                    assert!(matches!(ui.modal, Some(Modal::ModelPicker(_))));
                    assert!(!ui.busy);
                }
                let request = shortcut_tests::request_at_boundary(
                    &mut ui,
                    event(if custom { KeyCode::F(12) } else { code }),
                )?;
                anyhow::ensure!(
                    matches!(request, Request::Control { id, control: Control::Model { model, effort } } if id == "one" && model.as_deref() == Some("fixture-4") && effort.as_deref() == Some("high")),
                    "model payload"
                );
                assert!(ui.modal.is_none());
                assert_eq!(
                    ui.storage.load_config()?.agent_defaults.model.as_deref(),
                    default.then_some("fixture-4")
                );
            }
        }
        Ok(())
    }

    #[test]
    fn custom_model_fields_use_committed_focus_activation_and_raw_editing() -> Result<()> {
        for (phase, custom) in shortcut_tests::BINDING_PHASES {
            let (mut ui, _dir, _) = shortcut_tests::binding_fixture(MODELS_CUSTOM, phase)?;
            catalog(&mut ui);
            picker(&mut ui, 2);
            if custom {
                ui.key(event(KeyCode::Char('t')));
                assert!(matches!(ui.modal, Some(Modal::ModelPicker(_))));
            }
            ui.key(event(if custom {
                KeyCode::F(12)
            } else {
                KeyCode::Char('t')
            }));
            assert!(matches!(ui.modal, Some(Modal::Model { field: 0, .. })));
            ui.key(event(KeyCode::Esc));
            assert!(ui.modal.is_none());
        }
        for field in [0, 1] {
            for (action, code, mods) in [
                (NAVIGATION_NEXT_FOCUS, KeyCode::Tab, KeyModifiers::NONE),
                (
                    NAVIGATION_PREVIOUS_FOCUS,
                    KeyCode::BackTab,
                    KeyModifiers::SHIFT,
                ),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    catalog(&mut ui);
                    ui.modal = Some(Modal::Model {
                        model: Editor::from("fixture"),
                        effort: Editor::default(),
                        field,
                    });
                    if custom {
                        ui.key(KeyEvent::new(code, mods));
                        assert!(
                            matches!(ui.modal, Some(Modal::Model { field: actual, .. }) if actual == field)
                        );
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        if custom { KeyModifiers::NONE } else { mods },
                    ));
                    assert!(
                        matches!(ui.modal, Some(Modal::Model { field: actual, .. }) if actual == 1-field)
                    );
                }
            }
        }
        for (phase, custom) in shortcut_tests::BINDING_PHASES {
            let (mut ui, _dir, _) = shortcut_tests::binding_fixture(NAVIGATION_ACTIVATE, phase)?;
            catalog(&mut ui);
            ui.modal = Some(Modal::Model {
                model: Editor::from("fixture-4"),
                effort: Editor::from("high"),
                field: 0,
            });
            if custom {
                ui.key(event(KeyCode::Enter));
                assert!(!ui.model_completion.filled);
                assert!(
                    matches!(&ui.modal, Some(Modal::Model { model, .. }) if model.text() == "fixture-4\n")
                );
                if let Some(Modal::Model { model, .. }) = &mut ui.modal {
                    *model = Editor::from("fixture-4");
                }
            }
            let key = event(if custom {
                KeyCode::F(12)
            } else {
                KeyCode::Enter
            });
            ui.key(key);
            assert!(ui.model_completion.filled);
            let request = shortcut_tests::request_at_boundary(&mut ui, key)?;
            anyhow::ensure!(
                matches!(request, Request::Control { id, control: Control::Model { model, effort } } if id == "one" && model.as_deref() == Some("fixture-4") && effort.as_deref() == Some("high")),
                "custom model payload"
            );
        }
        Ok(())
    }

    #[test]
    fn model_completion_navigation_and_visible_mouse_ignore_rebound_keys() -> Result<()> {
        for (action, code, start, want) in [
            (NAVIGATION_UP, KeyCode::Up, 2, 1),
            (NAVIGATION_DOWN, KeyCode::Down, 0, 1),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                catalog(&mut ui);
                ui.modal = Some(Modal::Model {
                    model: Editor::from("fixture"),
                    effort: Editor::default(),
                    field: 0,
                });
                ui.model_completion.selected = start;
                if custom {
                    ui.key(event(code));
                    assert_eq!(
                        ui.model_completion.selected, 0,
                        "removed key stays native editor"
                    );
                    ui.model_completion.selected = start;
                }
                ui.key(event(if custom { KeyCode::F(12) } else { code }));
                assert_eq!(ui.model_completion.selected, want);
            }
        }
        for width in [40, 80, 160] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            catalog(&mut ui);
            ui.modal = Some(Modal::Model {
                model: Editor::from("fixture"),
                effort: Editor::default(),
                field: 0,
            });
            let click = shortcut_tests::native_target(&mut ui, width, "fixture-2")?;
            ui.mouse(click);
            assert!(
                matches!(&ui.modal, Some(Modal::Model { model, .. }) if model.text() == "fixture-2")
            );
            assert!(!ui.busy);
        }
        Ok(())
    }
    #[test]
    fn model_field_and_picker_contexts_preserve_early_actions_copy_and_typing() -> Result<()> {
        for field in [0, 1] {
            shortcut_tests::owned_modal_context(
                |ui| {
                    let mut value = Editor::from("focused");
                    value.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                    ui.modal = Some(Modal::Model {
                        model: if field == 0 {
                            value.clone()
                        } else {
                            Editor::default()
                        },
                        effort: if field == 1 { value } else { Editor::default() },
                        field,
                    });
                },
                AGENT_MODEL,
                Some("d"),
            )?;
        }
        shortcut_tests::owned_modal_context(|ui| picker(ui, 0), AGENT_MODEL_PICKER, None)?;
        Ok(())
    }
    #[test]
    fn picker_guards_do_not_send_without_a_valid_model_or_coding_session() -> Result<()> {
        for action in [NAVIGATION_ACTIVATE, MODELS_SAVE] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(action))?;
            picker(&mut ui, 0);
            ui.key(event(KeyCode::F(12)));
            assert!(!ui.busy);
            assert!(ui.modal.is_some());
            catalog(&mut ui);
            ui.selected = None;
            if action == MODELS_SAVE {
                ui.key(event(KeyCode::F(12)));
                assert!(!ui.busy);
                assert!(ui.modal.is_some());
                assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
            } else {
                std::fs::write(&ui.storage.config, "{malformed")?;
                ui.key(event(KeyCode::F(12)));
                assert!(!ui.busy);
                assert_eq!(std::fs::read_to_string(&ui.storage.config)?, "{malformed");
                assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
            }
        }
        Ok(())
    }
    #[test]
    fn model_and_picker_native_hints_use_effective_bindings() -> Result<()> {
        for width in [80, 160] {
            for action in [
                NAVIGATION_UP,
                NAVIGATION_DOWN,
                NAVIGATION_ACTIVATE,
                NAVIGATION_NEXT_FOCUS,
                MODELS_PREVIOUS_EFFORT,
                MODELS_NEXT_EFFORT,
                MODELS_SAVE,
                MODELS_CUSTOM,
                MODELS_CHOOSE_1,
                MODELS_CHOOSE_2,
                MODELS_CHOOSE_3,
                MODELS_CHOOSE_4,
                MODELS_CHOOSE_5,
                MODELS_CHOOSE_6,
                MODELS_CHOOSE_7,
                MODELS_CHOOSE_8,
                MODELS_CHOOSE_9,
            ] {
                let (mut ui, _dir, _) = shortcut_tests::fixture(Some(action))?;
                catalog(&mut ui);
                if [NAVIGATION_UP, NAVIGATION_DOWN, NAVIGATION_NEXT_FOCUS].contains(&action) {
                    ui.modal = Some(Modal::Model {
                        model: Editor::from("fixture"),
                        effort: Editor::default(),
                        field: 0,
                    });
                } else {
                    picker(&mut ui, 0);
                }
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
                assert!(text.contains("F12"), "{} width{width}: {text}", action.0);
            }
        }
        Ok(())
    }

    #[test]
    fn model_blank_option_area_and_picker_clicks_keep_semantic_mouse_behavior() -> Result<()> {
        for width in [40, 80, 160] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            catalog(&mut ui);
            ui.modal = Some(Modal::Model {
                model: Editor::from("fixture"),
                effort: Editor::default(),
                field: 0,
            });
            let blank = shortcut_tests::native_blank_below(&mut ui, width, "fixture-12")?;
            ui.mouse(blank);
            assert!(!ui.model_completion.filled);
            assert!(!ui.busy);
            assert!(matches!(&ui.modal,Some(Modal::Model{model,..})if model.text()=="fixture"));
            picker(&mut ui, 0);
            let click = shortcut_tests::native_target(&mut ui, width, "Fixture 3")?;
            ui.mouse(click);
            assert_eq!(selected(&ui)?, (2, None));
            assert!(!ui.busy);
        }
        Ok(())
    }
    #[test]
    fn effort_editor_navigation_fill_apply_and_native_click_use_the_real_field() -> Result<()> {
        for (phase, custom) in shortcut_tests::BINDING_PHASES {
            for (action, code) in [
                (NAVIGATION_UP, KeyCode::Up),
                (NAVIGATION_DOWN, KeyCode::Down),
                (NAVIGATION_ACTIVATE, KeyCode::Enter),
            ] {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                catalog(&mut ui);
                ui.modal = Some(Modal::Model {
                    model: Editor::from("fixture-4"),
                    effort: Editor::default(),
                    field: 1,
                });
                ui.model_completion.selected = usize::from(action == NAVIGATION_UP);
                if custom {
                    ui.key(event(code));
                    assert_eq!(ui.model_completion.selected, 0);
                    assert!(!ui.model_completion.filled);
                    ui.modal = Some(Modal::Model {
                        model: Editor::from("fixture-4"),
                        effort: Editor::default(),
                        field: 1,
                    });
                    ui.model_completion.selected = usize::from(action == NAVIGATION_UP);
                }
                let key = event(if custom { KeyCode::F(12) } else { code });
                ui.key(key);
                if action == NAVIGATION_ACTIVATE {
                    assert!(
                        matches!(&ui.modal,Some(Modal::Model{model,effort,field:1})if model.text()=="fixture-4"&&effort.text()=="low")
                    );
                    assert!(ui.model_completion.filled);
                    let request = shortcut_tests::request_at_boundary(&mut ui, key)?;
                    assert!(
                        matches!(request,Request::Control{id,control:Control::Model{model,effort}}if id=="one"&&model.as_deref()==Some("fixture-4")&&effort.as_deref()==Some("low"))
                    );
                } else {
                    assert_eq!(
                        ui.model_completion.selected,
                        usize::from(action == NAVIGATION_DOWN)
                    );
                }
            }
        }
        for width in [40, 80, 160] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            catalog(&mut ui);
            ui.modal = Some(Modal::Model {
                model: Editor::from("fixture-4"),
                effort: Editor::default(),
                field: 1,
            });
            let click = shortcut_tests::native_target(&mut ui, width, "high")?;
            ui.mouse(click);
            assert!(
                matches!(&ui.modal,Some(Modal::Model{model,effort,field:1})if model.text()=="fixture-4"&&effort.text()=="high")
            );
            assert!(!ui.busy);
        }
        Ok(())
    }
}

#[cfg(test)]
mod dependent_picker_tests {
    use super::*;
    use anyhow::Result;
    #[test]
    fn legacy_picker_saves_compatible_provider_and_rejects_concurrent_bundle_change() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        storage.update_config(|c| {
            c.agent_defaults.provider = Some(super::super::super::provider::Provider::Claude);
            c.agent_defaults.starting_mode = Some("acceptEdits".into());
        })?;
        let mut ui = Ui::new(storage.clone(), &storage.load_config()?);
        ui.model_completion.options = vec![ModelInfo {
            id: "fixture-model".into(),
            name: "Fixture".into(),
            efforts: vec![],
        }];
        ui.open_model_picker();
        ui.picker_apply(true);
        let saved = storage.load_config()?;
        assert_eq!(
            saved.agent_defaults.provider,
            Some(super::super::super::provider::Provider::Codex)
        );
        assert!(saved.agent_defaults.starting_mode.is_none());
        ui.open_model_picker();
        storage.update_config(|c| c.agent_defaults.starting_mode = Some("plan".into()))?;
        let bytes = std::fs::read(&storage.config)?;
        ui.picker_apply(true);
        assert!(matches!(ui.modal, Some(Modal::ModelPicker(_))));
        assert!(ui.notice.as_ref().is_some_and(|(_, error)| *error));
        assert_eq!(std::fs::read(&storage.config)?, bytes);
        Ok(())
    }
}
