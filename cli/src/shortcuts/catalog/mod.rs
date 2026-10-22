//! Authoritative metadata; view handlers dispatch these semantic IDs.
use super::*;
mod agents;
mod navigation;
mod reviews;
mod shell;
fn entry(id: ActionId, label: &'static str, contexts: &[Context], keys: &[KeyChord]) -> ActionSpec {
    ActionSpec {
        id,
        label,
        description: "Remap this action in the listed input contexts.",
        aliases: &[],
        contexts: contexts.to_vec(),
        defaults: keys
            .iter()
            .map(|chord| DefaultBinding {
                chord: chord.clone(),
                contexts: contexts.to_vec(),
            })
            .collect(),
    }
}
pub(super) fn build() -> Vec<ActionSpec> {
    let mut entries = shell::entries();
    entries.extend(navigation::entries());
    entries.extend(agents::entries());
    entries.extend(reviews::entries());
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::APP_QUIT) {
        entry.description = "Copy a selected transcript first; otherwise exit the terminal app. Terminal input remains passthrough.";
        entry.aliases = &[];
    }
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::QUESTION_OPEN) {
        entry.description =
            "A selection key requires explicitly enabling composer text-selection override.";
        entry.aliases = &["change question key", "question shortcut", "alt up"];
    }
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::COMPOSER_SEND) {
        entry.description = "Send or steer/queue according to Input while running.";
        entry.aliases = &[];
    }
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::VOICE_HOLD) {
        entry.description = "Hold/release speaks using the selected provider; a short Space tap stays ordinary text.";
        entry.aliases = &["voice shortcut", "dictation key"];
    }
    if let Some(entry) = entries
        .iter_mut()
        .find(|s| s.id == ids::CONVERSATION_INTERRUPT)
    {
        entry.description = "An active session is interrupted; otherwise return to sessions. Modal Escape always closes safely.";
        entry.aliases = &[];
    }
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::SELECTION_COPY) {
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Character('c'), 0),
            contexts: vec![
                contexts::REVIEW_HOME,
                contexts::REVIEW_REPOSITORY,
                contexts::REVIEW_NAVIGATION,
                contexts::REVIEW_CONTENT,
                contexts::REVIEW_OVERVIEW,
                contexts::REVIEW_LOCAL_LIST,
                contexts::AGENT_CHANGES,
            ],
        });
    }
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::NAVIGATION_UP) {
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Character('k'), 0),
            contexts: vec![contexts::AGENT_PERMISSIONS, contexts::AGENT_MCP],
        });
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Tab, 4),
            contexts: vec![contexts::AGENT_THEMES, contexts::AGENT_PLAN],
        });
    }
    if let Some(entry) = entries.iter_mut().find(|s| s.id == ids::NAVIGATION_DOWN) {
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Character('j'), 0),
            contexts: vec![contexts::AGENT_PERMISSIONS, contexts::AGENT_MCP],
        });
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Tab, 0),
            contexts: vec![
                contexts::AGENT_THEMES,
                contexts::AGENT_PLAN,
                contexts::AGENT_RESOURCE_PICKER,
            ],
        });
    }
    if let Some(entry) = entries
        .iter_mut()
        .find(|s| s.id == ids::NAVIGATION_PAGE_DOWN)
    {
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Character(' '), 0),
            contexts: vec![
                contexts::REVIEW_HOME,
                contexts::REVIEW_REPOSITORY,
                contexts::REVIEW_NAVIGATION,
                contexts::REVIEW_CONTENT,
                contexts::REVIEW_OVERVIEW,
                contexts::REVIEW_DEFINITION,
            ],
        });
    }
    if let Some(entry) = entries
        .iter_mut()
        .find(|s| s.id == ids::NAVIGATION_NEXT_FOCUS)
    {
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Down, 0),
            contexts: vec![contexts::AGENT_SLACK, contexts::AGENT_TWEAKS],
        });
    }
    if let Some(entry) = entries
        .iter_mut()
        .find(|s| s.id == ids::NAVIGATION_PREVIOUS_FOCUS)
    {
        entry.defaults.push(DefaultBinding {
            chord: key(BindingKey::Up, 0),
            contexts: vec![contexts::AGENT_SLACK, contexts::AGENT_TWEAKS],
        });
    }
    entries
}
