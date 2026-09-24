use super::*;
pub(super) struct History {
    drafts: Vec<Editor>,
    index: usize,
}
impl Ui {
    pub(super) fn history_key(&mut self, key: KeyEvent) -> bool {
        if !key.modifiers.is_empty() || !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            return false;
        }
        let Some(id) = self.selected.as_ref() else {
            return false;
        };
        let Some(position) = self.positions.get_mut(id) else {
            return false;
        };
        if position.history.is_none() {
            if key.code != KeyCode::Up || !position.draft.chars.is_empty() {
                return false;
            }
            let mut drafts = self
                .sessions
                .get(id)
                .map(|s| {
                    s.entries
                        .iter()
                        .filter(|e| e.kind == "userMessage")
                        .map(|e| Editor::from(e.text.as_str()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if drafts.is_empty() {
                return true;
            }
            let index = drafts.len();
            drafts.push(position.draft.clone());
            position.history = Some(History { drafts, index });
        }
        let Some(history) = &mut position.history else {
            return false;
        };
        if let Some(draft) = history.drafts.get_mut(history.index) {
            *draft = position.draft.clone();
        }
        history.index = if key.code == KeyCode::Up {
            history.index.saturating_sub(1)
        } else {
            history
                .index
                .saturating_add(1)
                .min(history.drafts.len().saturating_sub(1))
        };
        if let Some(draft) = history.drafts.get(history.index) {
            position.draft = draft.clone();
        }
        if history.index == history.drafts.len().saturating_sub(1) {
            position.history = None;
        }
        true
    }
}
