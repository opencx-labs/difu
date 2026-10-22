//! Editing reservations are shared by capture/save validation and live dispatch.
//! Legacy invalid overrides stay stored, but cannot intercept input in these contexts.
use super::{ActionSpec, Context, contexts::*, ids, key, normalize, send_companions};
use crate::{
    settings::{BindingKey, KeyChord, SendKey},
    storage::Config,
};

pub(super) fn allows(
    action: &ActionSpec,
    context: Context,
    chord: &KeyChord,
    config: &Config,
) -> bool {
    if !reserved(context, chord) {
        return true;
    }
    // Existing action-owned aliases retain their original context, including Enter,
    // list movement, copy, and voice's tap/hold Space handling.
    if action.defaults.iter().any(|binding| {
        binding.contexts.contains(&context) && normalize(binding.chord.clone()) == *chord
    }) {
        return true;
    }
    // Both advertised send presets are atomic bundles in this same registry.
    if [SendKey::Enter, SendKey::CtrlEnter]
        .into_iter()
        .any(|preset| {
            send_companions(preset)
                .iter()
                .any(|(id, binding)| *id == action.id && binding == chord)
        })
    {
        return true;
    }
    [AGENT_COMPOSER, AGENT_COMPOSER_PANES].contains(&context)
        && question_selection_override(action, chord, config)
}

pub(super) fn question_selection_override(
    action: &ActionSpec,
    chord: &KeyChord,
    config: &Config,
) -> bool {
    action.id == ids::QUESTION_OPEN
        && *chord == key(BindingKey::Left, 4)
        && config.preferences.keyboard.question_overrides_selection
}

fn reserved(context: Context, chord: &KeyChord) -> bool {
    let modifiers = &chord.modifiers;
    let printable =
        matches!(chord.key, BindingKey::Character(_)) && !modifiers.control && !modifiers.super_key;
    // Coding transcripts forward only unmodified/Shift printable typing to the
    // composer; navigation still belongs to the transcript, not an Editor.
    if context == AGENT_CONVERSATION_INPUT {
        return printable && !modifiers.alt;
    }
    // AGENT_QUEUE_BUTTON, AGENT_PERMISSION_CHOICE and AGENT_PERMISSION_BUTTON
    // deliberately have no Editor: their text fields are unfocused.
    // Settings uses simple strings, not Editor: reserve exactly its insertion and
    // Backspace paths. Its field editor also accepts Alt characters.
    if context == SETTINGS_SEARCH || context == SETTINGS_EDIT {
        return chord.key == BindingKey::Backspace
            || (printable && (context == SETTINGS_EDIT || !modifiers.alt));
    }
    // These contexts contain an Editor. Mixed forms keep their action-owned defaults;
    // consumers can refine contexts when a field and a list have different behavior.
    if ![
        AGENT_COMPOSER,
        AGENT_COMPOSER_PANES,
        AGENT_QUESTION,
        AGENT_QUESTION_NOTE,
        AGENT_APPROVAL_TEXT,
        AGENT_QUEUE_EDITOR,
        AGENT_PLAN_FEEDBACK,
        AGENT_COMMANDS,
        AGENT_MENU,
        AGENT_HELP,
        AGENT_MODEL,
        AGENT_DEFAULTS,
        AGENT_PERMISSION_SEARCH,
        AGENT_PERMISSION_FORM,
        AGENT_PERMISSION_MULTILINE,
        AGENT_RULES,
        AGENT_RULES_REPOSITORY,
        AGENT_SLACK,
        AGENT_TWEAKS,
        AGENT_REPOSITORY,
        AGENT_CHANGE_REPOSITORY,
        AGENT_RENAME,
        REVIEW_FILTER,
        REVIEW_HELP,
        REVIEW_CLONE,
        REVIEW_MODELS,
        REVIEW_LOCAL_ROOTS,
        WORKFLOW_CONTROLS,
        WORKFLOW_REVIEWERS,
        WORKFLOW_COMPOSE,
        WORKFLOW_MENTIONS,
        PALETTE,
    ]
    .contains(&context)
    {
        return false;
    }
    if printable && !modifiers.alt {
        return true;
    }
    if matches!(
        chord.key,
        BindingKey::Left
            | BindingKey::Right
            | BindingKey::Up
            | BindingKey::Down
            | BindingKey::Home
            | BindingKey::End
            | BindingKey::Enter
            | BindingKey::Backspace
            | BindingKey::Delete
    ) {
        return true;
    }
    // Exact aliases from Editor::key/key_raw, plus the surrounding copy/paste handlers.
    matches!(chord.key, BindingKey::Character('a')) && modifiers.super_key
        || *chord == key(BindingKey::Character('z'), 8)
        || ['a', 'e', 'u', 'c', 'v']
            .into_iter()
            .any(|ch| *chord == key(BindingKey::Character(ch), 1))
        || ['b', 'f']
            .into_iter()
            .any(|ch| *chord == key(BindingKey::Character(ch), 2))
        || ['c', 'v']
            .into_iter()
            .any(|ch| *chord == key(BindingKey::Character(ch), 8))
}
