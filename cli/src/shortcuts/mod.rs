//! Semantic keyboard actions. Chords are resolved here; consumers dispatch actions, never keys.
use crate::{
    settings::{BindingKey, KeyChord, KeyModifiers, SendKey, SettingSpec},
    storage::Config,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers as M};

mod catalog;
mod chord;
mod editing;
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
            let chord = normalize(chord.clone());
            return editing::allows(spec, context, &chord, self.config)
                .then_some(chord)
                .into_iter()
                .collect();
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
        // Release is owned only by the voice hold lifecycle, never by action dispatch.
        if event.kind == KeyEventKind::Release {
            return false;
        }
        // The original question handler accepted every non-Shift Enter. Preserve
        // opaque terminal modifiers only for this default semantic action; they
        // remain unsupported by shortcut capture and persisted custom chords.
        if action == ids::QUESTION_SUBMIT
            && [contexts::AGENT_QUESTION, contexts::AGENT_QUESTION_NOTE].contains(&context)
            && !self
                .config
                .preferences
                .keyboard
                .bindings
                .contains_key(action.0)
            && event.code == KeyCode::Enter
            && !event.modifiers.contains(M::SHIFT)
            && event.modifiers.intersects(M::HYPER | M::META)
        {
            return true;
        }
        // Native permission form controls explicitly accepted CONTROL with extra
        // modifiers. Capturable combinations live in the catalog; only opaque
        // Hyper/Meta variants need this narrow, default-only compatibility path.
        if [ids::PERMISSIONS_SUBMIT, ids::PERMISSIONS_REFRESH_FORM].contains(&action)
            && spec(action).is_some_and(|spec| spec.contexts.contains(&context))
            && !self
                .config
                .preferences
                .keyboard
                .bindings
                .contains_key(action.0)
            && event.modifiers.contains(M::CONTROL)
            && event.modifiers.intersects(M::HYPER | M::META)
            && ((action == ids::PERMISSIONS_SUBMIT && event.code == KeyCode::Enter)
                || (action == ids::PERMISSIONS_REFRESH_FORM && event.code == KeyCode::Char('r')))
        {
            return true;
        }
        // These forms used explicit Control / Super predicates, including opaque
        // terminal modifiers. A committed override removes the whole predicate.
        if [
            ids::DEFAULTS_SAVE,
            ids::DEFAULTS_REPOSITORY_RULES,
            ids::RULES_SAVE,
        ]
        .contains(&action)
            && spec(action).is_some_and(|spec| spec.contexts.contains(&context))
            && !self
                .config
                .preferences
                .keyboard
                .bindings
                .contains_key(action.0)
            && event.modifiers.intersects(M::HYPER | M::META)
            && if action == ids::RULES_SAVE {
                event.code == KeyCode::Enter && event.modifiers.intersects(M::CONTROL | M::SUPER)
            } else {
                event.modifiers.contains(M::CONTROL)
                    && event.code
                        == if action == ids::DEFAULTS_REPOSITORY_RULES {
                            KeyCode::Char('r')
                        } else {
                            KeyCode::Enter
                        }
            }
        {
            return true;
        }
        let Some(chord) = capture(event) else {
            return false;
        };
        self.effective(action, context).contains(&chord)
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
    /// Compact UI hints show the primary alias; binding details retain every alias.
    pub fn primary_hint_in(&self, action: ActionId, context: Context) -> String {
        self.effective(action, context)
            .first()
            .map(label)
            .unwrap_or_default()
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
                for context in &action.contexts {
                    anyhow::ensure!(
                        editing::allows(action, *context, &chord, self.config)
                            // Consent authorizes this assignment only in composers.
                            // Effective bindings still suppress it in other editors.
                            || editing::question_selection_override(action, &chord, self.config),
                        "{} is reserved for text editing in {}.{}",
                        label(&chord),
                        context.0,
                        if action.id == ids::QUESTION_OPEN && chord == key(BindingKey::Left, 4) {
                            " Enable the explicit question text-selection override before assigning Shift+Left."
                        } else {
                            " Choose another shortcut."
                        }
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
