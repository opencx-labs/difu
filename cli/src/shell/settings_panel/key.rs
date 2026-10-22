use super::*;

impl Panel {
    fn activate(&mut self, backwards: bool) -> Option<SettingId> {
        let spec = self.rows().get(self.selected).cloned()?;
        let current = self.draft.value(&spec.id);
        let value = match (spec.kind, current) {
            (SettingKind::Bool, SettingValue::Bool(value)) => SettingValue::Bool(!value),
            (SettingKind::Choice, SettingValue::Choice(value)) => {
                let choices = spec
                    .choices
                    .iter()
                    .filter(|v| **v != "custom")
                    .collect::<Vec<_>>();
                let index = choices.iter().position(|v| **v == value).unwrap_or(0);
                let next = if backwards {
                    index
                        .checked_sub(1)
                        .unwrap_or(choices.len().saturating_sub(1))
                } else {
                    (index + 1) % choices.len().max(1)
                };
                SettingValue::Choice(choices.get(next).map(|v| (**v).to_owned()).unwrap_or(value))
            }
            (SettingKind::DynamicChoice, SettingValue::Text(value))
                if self.available_choices(&spec.id).is_some() =>
            {
                let choices = self.available_choices(&spec.id)?;
                let current = value
                    .as_ref()
                    .and_then(|v| choices.iter().position(|c| c == v))
                    .unwrap_or(choices.len());
                let next = if backwards {
                    current.checked_sub(1).unwrap_or(choices.len())
                } else {
                    (current + 1) % (choices.len() + 1)
                };
                SettingValue::Text(choices.get(next).cloned())
            }
            (SettingKind::Number { .. }, SettingValue::Number(value)) => {
                self.edit = Some(value.to_string());
                return None;
            }
            (SettingKind::OptionalText | SettingKind::DynamicChoice, SettingValue::Text(value)) => {
                self.edit = Some(value.unwrap_or_default());
                return None;
            }
            (SettingKind::Shortcut | SettingKind::Action | SettingKind::DraftAction, _) => {
                return Some(spec.id);
            }
            _ => return None,
        };
        self.apply(spec.id, value);
        None
    }
    pub(super) fn apply(&mut self, id: SettingId, value: SettingValue) {
        match self.draft.set(id, value) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }
    pub fn key(&mut self, key: KeyEvent, storage: &Storage) -> Outcome {
        let mut outcome = Outcome::default();
        if self.shortcut_key(key) {
            return outcome;
        }
        use crate::shortcuts::{Bindings, contexts, ids};
        let context = if self.confirm_discard {
            contexts::SETTINGS_CONFIRM
        } else if self.edit.is_some() {
            contexts::SETTINGS_EDIT
        } else if self.search_focused {
            contexts::SETTINGS_SEARCH
        } else {
            contexts::SETTINGS
        };
        let action = Bindings::new(&self.committed).resolve(context, key);
        if self.confirm_discard {
            match action {
                Some(ids::SETTINGS_DISCARD_CLOSE) => {
                    outcome.saved = self.discard(storage);
                    outcome.close = self.error.is_none();
                }
                _ => self.confirm_discard = false,
            }
            return outcome;
        }
        if self.edit.is_some() {
            match key.code {
                KeyCode::Esc => self.edit = None,
                _ if action == Some(ids::SETTINGS_EDIT_ACCEPT) => {
                    if let Some(spec) = self.rows().get(self.selected).cloned() {
                        let text = self.edit.clone().unwrap_or_default();
                        let value = if matches!(spec.kind, SettingKind::Number { .. }) {
                            match text.parse::<u32>() {
                                Ok(value) => SettingValue::Number(value),
                                Err(_) => {
                                    self.error =
                                        Some("Enter a whole number, zero or greater".into());
                                    return outcome;
                                }
                            }
                        } else {
                            SettingValue::Text(
                                (!text.trim().is_empty()).then(|| text.trim().to_owned()),
                            )
                        };
                        self.apply(spec.id, value);
                        if self.error.is_none() {
                            self.edit = None;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(text) = &mut self.edit {
                        text.pop();
                    }
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
                {
                    if let Some(text) = &mut self.edit {
                        text.push(ch);
                    }
                }
                _ => {}
            }
            return outcome;
        }
        match action {
            Some(ids::SETTINGS_SAVE) => {
                let enable = self.draft.enables_native();
                outcome.saved = self.save(storage);
                outcome.native_enable = enable && outcome.saved.is_some();
            }
            Some(ids::SETTINGS_DISCARD) => outcome.saved = self.discard(storage),
            Some(ids::SETTINGS_RESET_SECTION) => self.reset_section(),
            Some(ids::SETTINGS_EXPORT) => {
                if let Err(error) = self.export(storage) {
                    self.error = Some(format!("{error:#}"));
                }
            }
            Some(ids::SETTINGS_MODIFIED) => {
                self.modified_only = !self.modified_only;
                self.selected = 0;
            }
            Some(ids::SETTINGS_REPLAY) => self.replay = Instant::now(),
            Some(ids::SETTINGS_TEST) => {
                let preview = self.rows().get(self.selected).map(|s| s.preview);
                if preview == Some(Preview::Shortcut) {
                    self.start_shortcut_test();
                } else {
                    outcome.preview_test = preview.filter(|p| *p == Preview::Notification);
                }
            }
            Some(ids::SETTINGS_SEARCH | ids::SETTINGS_SEARCH_SLASH) => self.search_focused = true,
            Some(ids::SETTINGS_DETAILS) => self.details_expanded = !self.details_expanded,
            Some(ids::SETTINGS_DETAILS_DOWN) => {
                self.details_scroll = self.details_scroll.saturating_add(1)
            }
            Some(ids::SETTINGS_DETAILS_UP) => {
                self.details_scroll = self.details_scroll.saturating_sub(1)
            }
            Some(ids::SETTINGS_FOCUS) => self.search_focused = !self.search_focused,
            Some(ids::SETTINGS_PREVIOUS_SECTION) => self.change_category(-1),
            Some(ids::SETTINGS_NEXT_SECTION) => self.change_category(1),
            Some(ids::SETTINGS_UP) => {
                self.search_focused = false;
                self.selected = self.selected.saturating_sub(1);
            }
            Some(ids::SETTINGS_DOWN) => {
                self.search_focused = false;
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1));
            }
            Some(ids::SETTINGS_ACTIVATE | ids::SETTINGS_PREVIOUS_VALUE) => {
                if let Some(id) = self.activate(action == Some(ids::SETTINGS_PREVIOUS_VALUE))
                    && !self.shortcut_action(&id)
                {
                    outcome.action = Some(id);
                }
            }
            Some(ids::SETTINGS_RESET_BINDING) => {
                if let Some(id @ SettingId::Shortcut(_)) =
                    self.rows().get(self.selected).map(|s| s.id.clone())
                {
                    self.apply(id, SettingValue::Shortcut(None));
                    self.notice = Some("Selected shortcut reset in draft".into());
                }
            }
            _ if key.code == KeyCode::Esc => {
                if self.draft.edited().is_empty() {
                    outcome.close = true;
                } else {
                    self.confirm_discard = true;
                }
            }
            _ if self.search_focused => match key.code {
                KeyCode::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                KeyCode::Char(ch)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.query.push(ch);
                    self.selected = 0;
                }
                _ => {}
            },
            _ => {}
        }
        outcome
    }
}
