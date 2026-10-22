//! Capture/test are isolated state machines: they never dispatch a matched action.
use super::*;
use crate::shortcuts::{self, ActionId, Bindings, ids};
use crossterm::event::KeyEventKind;
impl Panel {
    pub(super) fn shortcut_action(&mut self, id: &SettingId) -> bool {
        match id {
            SettingId::Shortcut(name) => {
                let Some(action) = shortcuts::find(name) else {
                    self.error = Some(
                        "Unknown action from a newer version; its stored override is preserved"
                            .into(),
                    );
                    return true;
                };
                self.capture = Some(action);
                self.last_shortcut = Some(action);
                self.testing = false;
                self.question_test = None;
                self.error = None;
                self.notice = Some(format!(
                    "Press a new key for {}. Esc cancels. Ctrl+Enter requires a terminal that reports it distinctly (CSI-u). Shift+Left requires the text-selection override.",
                    shortcuts::spec(action).map_or(name.as_str(), |s| s.label)
                ));
            }
            SettingId::ShortcutCapture => {
                let action = self.last_shortcut.unwrap_or(ids::QUESTION_OPEN);
                return self.shortcut_action(&SettingId::Shortcut(action.0.into()));
            }
            SettingId::ShortcutTest => self.start_shortcut_test(),
            SettingId::ShortcutConflicts => match Bindings::new(self.draft.config()).validate() {
                Ok(()) => {
                    self.error = None;
                    self.notice =
                        Some("No overlapping-context shortcut conflicts in this draft".into());
                }
                Err(error) => self.error = Some(format!("{error:#}")),
            },
            SettingId::ShortcutReset => {
                if let Err(error) = self.draft.reset_shortcuts() {
                    self.error = Some(format!("{error:#}"));
                }
                self.notice=Some("Shortcuts and send preset reset to defaults in draft; future action overrides preserved".into());
            }
            _ => return false,
        }
        true
    }
    pub(super) fn start_shortcut_test(&mut self) {
        self.capture = None;
        self.testing = true;
        self.question_test = Some(crate::agents::ui::question_preview::QuestionPreview::new(
            self.draft.config(),
        ));
        self.error = None;
        self.notice=Some("Safe draft binding test: press any key. Matches are reported only; no action runs. Esc leaves the test.".into());
    }
    pub(super) fn shortcut_key(&mut self, event: KeyEvent) -> bool {
        if self.capture.is_none() && !self.testing {
            return false;
        }
        if event.kind != KeyEventKind::Press {
            return true;
        }
        if let Some(preview) = &mut self.question_test {
            preview.key(event);
        }
        if event.code == KeyCode::Esc {
            self.capture = None;
            self.testing = false;
            self.error = None;
            self.notice = Some("Returned to shortcut settings; draft unchanged".into());
            return true;
        }
        let Some(chord) = shortcuts::capture(event) else {
            self.error = Some("This terminal key or modifier cannot be captured".into());
            return true;
        };
        if let Some(action) = self.capture {
            if let Err(error) = self.capture_binding(action, chord) {
                self.error = Some(format!("{error:#}"));
            } else {
                self.capture = None;
                self.error = None;
                self.notice = Some("Shortcut captured in draft. Save to activate it.".into());
            }
        } else {
            let bindings = Bindings::new(self.draft.config());
            let matches = shortcuts::all()
                .iter()
                .filter(|s| s.contexts.iter().any(|c| bindings.matches(s.id, *c, event)))
                .map(|s| {
                    format!(
                        "{} ({})",
                        s.label,
                        s.contexts
                            .iter()
                            .filter(|c| bindings.matches(s.id, **c, event))
                            .map(|c| c.0)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>();
            self.notice = Some(if matches.is_empty() {
                format!(
                    "{}: no action in this draft. No action ran.",
                    shortcuts::label(&chord)
                )
            } else {
                format!(
                    "{}: {}. No action ran.",
                    shortcuts::label(&chord),
                    matches.join("; ")
                )
            });
        }
        true
    }
    fn capture_binding(&mut self, action: ActionId, chord: KeyChord) -> anyhow::Result<()> {
        let id = SettingId::Shortcut(action.0.into());
        let value = SettingValue::Shortcut(Some(chord));
        let mut candidate = self.draft.config().clone();
        set(&mut candidate, &id, value.clone())?;
        Bindings::new(&candidate).validate()?;
        self.draft.set(id, value)
    }
    pub(super) fn binding_hint(&self, action: ActionId) -> String {
        Bindings::new(&self.committed).hint(action)
    }
}
