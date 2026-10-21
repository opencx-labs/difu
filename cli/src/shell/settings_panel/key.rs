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
                if self.dynamic_choices.contains_key(&spec.id) =>
            {
                let choices = self.dynamic_choices.get(&spec.id)?;
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
        if self.confirm_discard {
            match key.code {
                KeyCode::Char('d') | KeyCode::Enter => {
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
                KeyCode::Enter => {
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
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => outcome.saved = self.save(storage),
                KeyCode::Char('d') => outcome.saved = self.discard(storage),
                KeyCode::Char('r') => self.reset_section(),
                KeyCode::Char('e') => {
                    if let Err(error) = self.export(storage) {
                        self.error = Some(format!("{error:#}"));
                    }
                }
                KeyCode::Char('m') => {
                    self.modified_only = !self.modified_only;
                    self.selected = 0;
                }
                KeyCode::Char('p') => self.replay = Instant::now(),
                KeyCode::Char('t') => {
                    outcome.preview_test = self
                        .rows()
                        .get(self.selected)
                        .map(|spec| spec.preview)
                        .filter(|preview| {
                            matches!(preview, Preview::Notification | Preview::Shortcut)
                        })
                }
                KeyCode::Char('f') => self.search_focused = true,
                KeyCode::Char('i') => self.details_expanded = !self.details_expanded,
                KeyCode::Down => self.details_scroll = self.details_scroll.saturating_add(1),
                KeyCode::Up => self.details_scroll = self.details_scroll.saturating_sub(1),
                _ => {}
            }
            return outcome;
        }
        match key.code {
            KeyCode::Esc => {
                if self.draft.edited().is_empty() {
                    outcome.close = true;
                } else {
                    self.confirm_discard = true;
                }
            }
            KeyCode::Tab | KeyCode::BackTab => self.search_focused = !self.search_focused,
            KeyCode::PageUp => self.change_category(-1),
            KeyCode::PageDown => self.change_category(1),
            KeyCode::Up => {
                self.search_focused = false;
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down => {
                self.search_focused = false;
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1));
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ')
                if !self.search_focused =>
            {
                outcome.action = self.activate(key.code == KeyCode::Left)
            }
            KeyCode::Backspace if self.search_focused => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(ch) if self.search_focused => {
                self.query.push(ch);
                self.selected = 0;
            }
            KeyCode::Char('/') => self.search_focused = true,
            _ => {}
        }
        outcome
    }
}
