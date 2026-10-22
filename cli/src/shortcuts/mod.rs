//! Semantic keyboard actions. Chords are resolved here; consumers dispatch actions, never keys.
use crate::{
    settings::{BindingKey, KeyChord, KeyModifiers, SendKey, SettingSpec},
    storage::Config,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers as M};

mod catalog;
mod chord;
pub use chord::{capture, label, normalize};
pub mod contexts;
pub mod ids;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionId(pub &'static str);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Context(pub &'static str);
#[derive(Clone, Debug)]
pub struct DefaultBinding {
    pub chord: KeyChord,
    /// A default alias may apply in only some of an action's contexts (e.g. j/k in a list).
    pub contexts: Vec<Context>,
}
#[derive(Clone, Debug)]
pub struct ActionSpec {
    pub id: ActionId,
    pub label: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    pub contexts: Vec<Context>,
    pub defaults: Vec<DefaultBinding>,
}
impl ActionSpec {
    pub fn setting(&self) -> SettingSpec {
        let mut spec = SettingSpec::shortcut(self.id.0, self.label);
        spec.description = self.description;
        spec.aliases = self.aliases;
        spec
    }
}
pub fn all() -> &'static [ActionSpec] {
    static ALL: std::sync::OnceLock<Vec<ActionSpec>> = std::sync::OnceLock::new();
    ALL.get_or_init(catalog::build)
}
pub fn spec(id: ActionId) -> Option<&'static ActionSpec> {
    all().iter().find(|s| s.id == id)
}
pub fn find(id: &str) -> Option<ActionId> {
    all().iter().find(|s| s.id.0 == id).map(|s| s.id)
}
pub fn settings() -> Vec<SettingSpec> {
    all().iter().map(ActionSpec::setting).collect()
}

pub struct Bindings<'a> {
    config: &'a Config,
}
impl<'a> Bindings<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }
    pub fn effective(&self, action: ActionId, context: Context) -> Vec<KeyChord> {
        let Some(spec) = spec(action).filter(|s| s.contexts.contains(&context)) else {
            return Vec::new();
        };
        if let Some(chord) = self.config.preferences.keyboard.bindings.get(action.0) {
            return vec![normalize(chord.clone())];
        }
        if let Some(chord) = send_companions(self.config.preferences.input.send_key)
            .into_iter()
            .find_map(|(id, chord)| (id == action).then_some(chord))
        {
            return vec![chord];
        }
        spec.defaults
            .iter()
            .filter(|b| b.contexts.contains(&context))
            .map(|b| normalize(b.chord.clone()))
            .collect()
    }
    pub fn matches(&self, action: ActionId, context: Context, event: KeyEvent) -> bool {
        let Some(chord) = capture(event) else {
            return false;
        };
        // Release is owned only by the voice hold lifecycle, never by action dispatch.
        event.kind != KeyEventKind::Release && self.effective(action, context).contains(&chord)
    }
    pub fn resolve(&self, context: Context, event: KeyEvent) -> Option<ActionId> {
        all()
            .iter()
            .find(|s| self.matches(s.id, context, event))
            .map(|s| s.id)
    }
    pub fn hint(&self, action: ActionId) -> String {
        let mut chords = Vec::new();
        if let Some(spec) = spec(action) {
            for context in &spec.contexts {
                for chord in self.effective(action, *context) {
                    if !chords.contains(&chord) {
                        chords.push(chord);
                    }
                }
            }
        }
        chords.iter().map(label).collect::<Vec<_>>().join(" / ")
    }
    pub fn conflicts(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (index, left) in all().iter().enumerate() {
            for right in all().iter().skip(index + 1) {
                for context in left.contexts.iter().filter(|c| right.contexts.contains(c)) {
                    let right_keys = self.effective(right.id, *context);
                    if let Some(chord) = self
                        .effective(left.id, *context)
                        .iter()
                        .find(|c| right_keys.contains(c))
                    {
                        errors.push(format!(
                            "{} conflicts with {} on {} in {}",
                            left.label,
                            right.label,
                            label(chord),
                            context.0
                        ));
                        break;
                    }
                }
            }
        }
        errors
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        for action in all() {
            if let Some(chord) = self.config.preferences.keyboard.bindings.get(action.id.0) {
                chord.validate()?;
                let chord = normalize(chord.clone());
                anyhow::ensure!(
                    chord.key != BindingKey::Escape,
                    "Escape is reserved for closing dialogs"
                );
                if chord.key == BindingKey::Left
                    && chord.modifiers.shift
                    && action.contexts.iter().any(|c| {
                        [contexts::AGENT_COMPOSER, contexts::AGENT_COMPOSER_PANES].contains(c)
                    })
                {
                    anyhow::ensure!(
                        action.id == ids::QUESTION_OPEN,
                        "Shift+Left is reserved for composer text selection; only the question action has an explicit override"
                    );
                    anyhow::ensure!(
                        self.config
                            .preferences
                            .keyboard
                            .question_overrides_selection,
                        "Shift+Left selects composer text. Enable the explicit question text-selection override before assigning it."
                    );
                }
            }
        }
        let conflicts = self.conflicts();
        anyhow::ensure!(conflicts.is_empty(), "{}", conflicts.join("; "));
        Ok(())
    }
}
/// Presets and individual remaps share these same action IDs and effective bindings.
pub fn send_companions(preset: SendKey) -> [(ActionId, KeyChord); 3] {
    let (primary, newline, queue) = match preset {
        SendKey::Enter => (0, 4, 1),
        SendKey::CtrlEnter => (1, 0, 2),
    };
    [
        (ids::SEND, key(BindingKey::Enter, primary)),
        (ids::NEWLINE, key(BindingKey::Enter, newline)),
        (ids::QUEUE, key(BindingKey::Enter, queue)),
    ]
}
pub fn send_preset(config: &Config) -> Option<SendKey> {
    let bindings = Bindings::new(config);
    [SendKey::Enter, SendKey::CtrlEnter]
        .into_iter()
        .find(|preset| {
            send_companions(*preset).iter().all(|(id, chord)| {
                bindings.effective(*id, contexts::AGENT_COMPOSER) == vec![chord.clone()]
            })
        })
}
pub const fn key(key: BindingKey, flags: u8) -> KeyChord {
    KeyChord {
        key,
        modifiers: KeyModifiers {
            control: flags & 1 != 0,
            alt: flags & 2 != 0,
            shift: flags & 4 != 0,
            super_key: flags & 8 != 0,
        },
    }
}
