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
    // Coding transcripts forward ordinary typing to the composer. Their modified
    // and navigation defaults remain available, while list-only character aliases
    // cannot bypass that input ownership (the command slash is composer-owned).
    for entry in &mut entries {
        if entry.contexts.contains(&contexts::AGENT_CONVERSATION)
            && ![
                ids::SESSION_NEW,
                ids::SESSION_REFRESH,
                ids::AGENTS_HELP,
                ids::CONVERSATION_COMPOSE,
            ]
            .contains(&entry.id)
        {
            entry.contexts.push(contexts::AGENT_CONVERSATION_INPUT);
            for binding in &mut entry.defaults {
                let modifiers = &binding.chord.modifiers;
                let typing = matches!(binding.chord.key, BindingKey::Character(_))
                    && !modifiers.control
                    && !modifiers.alt
                    && !modifiers.super_key;
                if binding.contexts.contains(&contexts::AGENT_CONVERSATION)
                    && (!typing || entry.id == ids::AGENTS_COMMANDS)
                {
                    binding.contexts.push(contexts::AGENT_CONVERSATION_INPUT);
                }
            }
        }
    }
    // These main actions precede generic modal dispatch, using the actual modal
    // context so invalid overrides still obey that editor's reservations.
    for entry in &mut entries {
        if [
            ids::AGENTS_SIDEBAR,
            ids::AGENTS_CHANGES,
            ids::AGENTS_MODELS,
            ids::QUESTION_OPEN,
        ]
        .contains(&entry.id)
        {
            for context in contexts::AGENT_GENERIC_MODALS {
                if !entry.contexts.contains(context) {
                    entry.contexts.push(*context);
                }
                for binding in &mut entry.defaults {
                    if !binding.contexts.contains(context) {
                        binding.contexts.push(*context);
                    }
                }
            }
        }
    }
    // Queue buttons have no Editor; globals must use that exact context rather
    // than suppressing valid list/button bindings as though text were focused.
    for entry in &mut entries {
        if entry.contexts.contains(&contexts::AGENT_QUEUE_EDITOR) {
            if !entry.contexts.contains(&contexts::AGENT_QUEUE_BUTTON) {
                entry.contexts.push(contexts::AGENT_QUEUE_BUTTON);
            }
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&contexts::AGENT_QUEUE_EDITOR)
                    && !binding.contexts.contains(&contexts::AGENT_QUEUE_BUTTON)
                {
                    binding.contexts.push(contexts::AGENT_QUEUE_BUTTON);
                }
            }
        }
    }
    // A permission form has native text only in its value field. Selector and
    // button contexts preserve early globals and field actions without pretending
    // those controls are editors. Delete confirmation retains native refresh.
    for entry in &mut entries {
        if entry.contexts.contains(&contexts::AGENT_PERMISSION_FORM) {
            for context in [
                contexts::AGENT_PERMISSION_CHOICE,
                contexts::AGENT_PERMISSION_BUTTON,
            ] {
                if entry.id == ids::PERMISSIONS_ACTIVATE
                    && context == contexts::AGENT_PERMISSION_CHOICE
                {
                    continue;
                }
                if !entry.contexts.contains(&context) {
                    entry.contexts.push(context);
                }
                for binding in &mut entry.defaults {
                    if binding.contexts.contains(&contexts::AGENT_PERMISSION_FORM)
                        && !binding.contexts.contains(&context)
                    {
                        binding.contexts.push(context);
                    }
                }
            }
        }
        if entry.id == ids::PERMISSIONS_REFRESH_FORM {
            entry.contexts.push(contexts::AGENT_PERMISSION_DELETE);
            for binding in &mut entry.defaults {
                binding.contexts.push(contexts::AGENT_PERMISSION_DELETE);
            }
        }
    }
    for entry in &mut entries {
        if entry.contexts.contains(&contexts::AGENT_PERMISSION_FORM) {
            if !entry
                .contexts
                .contains(&contexts::AGENT_PERMISSION_MULTILINE)
            {
                entry.contexts.push(contexts::AGENT_PERMISSION_MULTILINE);
            }
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&contexts::AGENT_PERMISSION_FORM)
                    && !binding
                        .contexts
                        .contains(&contexts::AGENT_PERMISSION_MULTILINE)
                {
                    binding.contexts.push(contexts::AGENT_PERMISSION_MULTILINE);
                }
            }
        }
        if entry.id == ids::PERMISSIONS_ACTIVATE {
            entry.defaults.push(DefaultBinding {
                chord: key(BindingKey::Enter, 4),
                contexts: vec![
                    contexts::AGENT_PERMISSION_FORM,
                    contexts::AGENT_PERMISSION_BUTTON,
                ],
            });
        }
    }
    // MCP list, actions, tools, and source detail are different real pages.
    // Numeric actions belong only to the server action list; source has no select.
    for entry in &mut entries {
        if entry.contexts.contains(&contexts::AGENT_MCP) {
            for context in [
                contexts::AGENT_MCP_SERVER,
                contexts::AGENT_MCP_TOOLS,
                contexts::AGENT_MCP_TOOL,
            ] {
                if context == contexts::AGENT_MCP_TOOL
                    && [ids::MCP_SELECT, ids::NAVIGATION_ACTIVATE].contains(&entry.id)
                {
                    continue;
                }
                if !entry.contexts.contains(&context) {
                    entry.contexts.push(context);
                }
                for binding in &mut entry.defaults {
                    if binding.contexts.contains(&contexts::AGENT_MCP)
                        && !binding.contexts.contains(&context)
                    {
                        binding.contexts.push(context);
                    }
                }
            }
        }
    }
    // Refine mixed forms to the field that actually owns input. Defaults Save
    // has no Editor; rules Enter selects a repository or activates a button,
    // while the Rules text field retains its native multiline Enter.
    for entry in &mut entries {
        for (base, added) in [
            (contexts::AGENT_DEFAULTS, contexts::AGENT_DEFAULTS_BUTTON),
            (contexts::AGENT_RULES, contexts::AGENT_RULES_REPOSITORY),
            (contexts::AGENT_RULES, contexts::AGENT_RULES_BUTTON),
        ] {
            if !entry.contexts.contains(&base)
                || (base == contexts::AGENT_DEFAULTS
                    && [ids::NAVIGATION_UP, ids::NAVIGATION_DOWN].contains(&entry.id))
            {
                continue;
            }
            if !entry.contexts.contains(&added) {
                entry.contexts.push(added);
            }
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&base) && !binding.contexts.contains(&added) {
                    binding.contexts.push(added);
                }
            }
        }
        if entry.id == ids::NAVIGATION_ACTIVATE {
            entry.contexts.retain(|c| *c != contexts::AGENT_RULES);
            for binding in &mut entry.defaults {
                binding.contexts.retain(|c| *c != contexts::AGENT_RULES);
            }
        }
        // These legacy predicates explicitly accept extra modifiers; enumerate
        // all capturable combinations so capture/conflict validation agrees.
        if [
            ids::DEFAULTS_SAVE,
            ids::DEFAULTS_REPOSITORY_RULES,
            ids::RULES_SAVE,
        ]
        .contains(&entry.id)
        {
            let modifiers: &[u8] = if entry.id == ids::RULES_SAVE {
                &[1, 8, 3, 5, 9, 10, 12, 7, 11, 13, 14, 15]
            } else {
                &[1, 3, 5, 7, 9, 11, 13, 15]
            };
            entry.defaults = modifiers
                .iter()
                .map(|m| DefaultBinding {
                    chord: key(
                        if entry.id == ids::DEFAULTS_REPOSITORY_RULES {
                            BindingKey::Character('r')
                        } else {
                            BindingKey::Enter
                        },
                        *m,
                    ),
                    contexts: entry.contexts.clone(),
                })
                .collect();
        }
    }
    // The two form buttons do not own an Editor; all form actions/globals
    // retain their availability while invalid typing bindings stay field-safe.
    for entry in &mut entries {
        for (base, button) in [
            (contexts::AGENT_SLACK, contexts::AGENT_SLACK_BUTTON),
            (contexts::AGENT_TWEAKS, contexts::AGENT_TWEAKS_BUTTON),
        ] {
            if entry.contexts.contains(&base) {
                if !entry.contexts.contains(&button) {
                    entry.contexts.push(button);
                }
                for binding in &mut entry.defaults {
                    if binding.contexts.contains(&base) && !binding.contexts.contains(&button) {
                        binding.contexts.push(button);
                    }
                }
            }
        }
    }
    // Legacy explicit predicates accepted all Control combinations, and plan
    // feedback accepted every non-Shift Enter. Keep these meaningful aliases.
    for entry in &mut entries {
        if [
            ids::APPROVAL_SUBMIT,
            ids::QUEUE_SAVE,
            ids::COMMANDS_REFRESH,
            ids::PLAN_FEEDBACK,
        ]
        .contains(&entry.id)
        {
            let modifiers: &[u8] = if entry.id == ids::PLAN_FEEDBACK {
                &[0, 1, 2, 3, 8, 9, 10, 11]
            } else {
                &[1, 3, 5, 7, 9, 11, 13, 15]
            };
            if entry.id != ids::APPROVAL_SUBMIT {
                entry.defaults.clear();
            }
            for modifiers in modifiers {
                let chord = key(
                    if entry.id == ids::COMMANDS_REFRESH {
                        BindingKey::Character('r')
                    } else {
                        BindingKey::Enter
                    },
                    *modifiers,
                );
                if !entry.defaults.iter().any(|binding| binding.chord == chord) {
                    entry.defaults.push(DefaultBinding {
                        chord,
                        contexts: entry.contexts.clone(),
                    });
                }
            }
        }
        if [
            ids::HELPER_PLACEMENT,
            ids::HELPER_VISIBILITY,
            ids::AGENTS_BACK,
        ]
        .contains(&entry.id)
        {
            entry.contexts.push(contexts::AGENT_BROWSER);
            for binding in &mut entry.defaults {
                binding.contexts.push(contexts::AGENT_BROWSER);
            }
        }
        // These modal scroll handlers explicitly used Super for ten-row motion.
        if [ids::NAVIGATION_FAST_UP, ids::NAVIGATION_FAST_DOWN].contains(&entry.id) {
            for context in [
                contexts::AGENT_STATUS,
                contexts::AGENT_TRANSCRIPT,
                contexts::AGENT_USAGE,
                contexts::AGENT_HELP,
            ] {
                for modifiers in [8, 9, 10, 11, 12, 13, 14, 15] {
                    if context == contexts::AGENT_HELP && modifiers & 4 != 0 {
                        continue;
                    }
                    let chord = key(
                        if entry.id == ids::NAVIGATION_FAST_UP {
                            BindingKey::Up
                        } else {
                            BindingKey::Down
                        },
                        modifiers,
                    );
                    if !entry.defaults.iter().any(|binding| {
                        binding.chord == chord && binding.contexts.contains(&context)
                    }) {
                        entry.defaults.push(DefaultBinding {
                            chord,
                            contexts: vec![context],
                        });
                    }
                }
            }
        }
    }
    // Masked Slack token fields remain native editors but never expose a copy
    // action; only the unmasked member/channel editors own selected clipboard text.
    for entry in &mut entries {
        if entry.id != ids::SELECTION_COPY && entry.contexts.contains(&contexts::AGENT_SLACK) {
            if !entry.contexts.contains(&contexts::AGENT_SLACK_SECRET) {
                entry.contexts.push(contexts::AGENT_SLACK_SECRET);
            }
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&contexts::AGENT_SLACK)
                    && !binding.contexts.contains(&contexts::AGENT_SLACK_SECRET)
                {
                    binding.contexts.push(contexts::AGENT_SLACK_SECRET);
                }
            }
        }
    }
    // Right and Bottom helpers share host focus cycling. Main placement keeps
    // its original one-way Tab, and Browser receives those raw main-area events.
    for entry in &mut entries {
        if [ids::NAVIGATION_NEXT_FOCUS, ids::NAVIGATION_PREVIOUS_FOCUS].contains(&entry.id) {
            entry.contexts.push(contexts::AGENT_BROWSER);
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&contexts::AGENT_HELPER) {
                    binding.contexts.push(contexts::AGENT_BROWSER);
                }
            }
        }
        for (base, main) in [
            (contexts::AGENT_HELPER, contexts::AGENT_HELPER_MAIN),
            (contexts::AGENT_BROWSER, contexts::AGENT_BROWSER_MAIN),
        ] {
            if !entry.contexts.contains(&base)
                || (main == contexts::AGENT_HELPER_MAIN
                    && entry.id == ids::NAVIGATION_PREVIOUS_FOCUS)
                || (main == contexts::AGENT_BROWSER_MAIN
                    && [ids::NAVIGATION_NEXT_FOCUS, ids::NAVIGATION_PREVIOUS_FOCUS]
                        .contains(&entry.id))
                || (main == contexts::AGENT_HELPER_MAIN
                    && [ids::AGENTS_MODELS, ids::QUESTION_OPEN].contains(&entry.id))
            {
                continue;
            }
            entry.contexts.push(main);
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&base) {
                    binding.contexts.push(main);
                }
            }
        }
    }
    // Only real selection owners belong in copy metadata. Non-editor modal
    // bodies have never supported pointer selection; hidden composer text must
    // not become their clipboard source. Reviews are owned by their consumer.
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.id == ids::SELECTION_COPY)
    {
        let copyable = |context: &Context| {
            !context.0.starts_with("Agent")
                || [
                    contexts::AGENT_LIST,
                    contexts::AGENT_COMPOSER,
                    contexts::AGENT_COMPOSER_PANES,
                    contexts::AGENT_CONVERSATION,
                    contexts::AGENT_CONVERSATION_INPUT,
                    contexts::AGENT_CHANGES,
                    contexts::AGENT_CHANGE_TREE,
                    contexts::AGENT_RESOURCES,
                    contexts::AGENT_HELPER,
                    contexts::AGENT_HELPER_MAIN,
                    contexts::AGENT_QUESTION,
                    contexts::AGENT_QUESTION_NOTE,
                    contexts::AGENT_QUEUE_EDITOR,
                    contexts::AGENT_PLAN_FEEDBACK,
                    contexts::AGENT_COMMANDS,
                    contexts::AGENT_MENU,
                    contexts::AGENT_HELP,
                    contexts::AGENT_MODEL,
                    contexts::AGENT_DEFAULTS,
                    contexts::AGENT_PERMISSION_SEARCH,
                    contexts::AGENT_PERMISSION_FORM,
                    contexts::AGENT_PERMISSION_MULTILINE,
                    contexts::AGENT_RULES,
                    contexts::AGENT_RULES_REPOSITORY,
                    contexts::AGENT_SLACK,
                    contexts::AGENT_TWEAKS,
                    contexts::AGENT_TRANSCRIPT,
                    contexts::AGENT_REPOSITORY,
                    contexts::AGENT_CHANGE_REPOSITORY,
                    contexts::AGENT_RENAME,
                ]
                .contains(context)
        };
        entry.contexts.retain(copyable);
        for binding in &mut entry.defaults {
            binding.contexts.retain(copyable);
        }
    }
    // Pending is a request chooser, not a scrollable transcript. Its original
    // handler consumes all input after Up/Down/Enter; do not advertise no-ops.
    for entry in &mut entries {
        if [
            ids::NAVIGATION_PAGE_UP,
            ids::NAVIGATION_PAGE_DOWN,
            ids::NAVIGATION_FIRST,
            ids::NAVIGATION_LAST,
        ]
        .contains(&entry.id)
        {
            entry
                .contexts
                .retain(|context| *context != contexts::AGENT_PENDING);
            for binding in &mut entry.defaults {
                binding
                    .contexts
                    .retain(|context| *context != contexts::AGENT_PENDING);
            }
        }
    }
    // Focused helpers historically consume models/question keys; only sidebar
    // and Changes pass back to the host. Do not invent new helper actions.
    for entry in &mut entries {
        if [ids::AGENTS_MODELS, ids::QUESTION_OPEN].contains(&entry.id) {
            entry
                .contexts
                .retain(|context| *context != contexts::AGENT_HELPER);
            for binding in &mut entry.defaults {
                binding
                    .contexts
                    .retain(|context| *context != contexts::AGENT_HELPER);
            }
        }
    }
    // Reviews copy only belongs to a real selection owner. Help Home/End
    // remain native query editing, unlike the read-only definition viewer.
    for entry in &mut entries {
        let supported = |context: &Context| {
            if entry.id == ids::SELECTION_COPY
                && (context.0.starts_with("Review") || context.0.starts_with("Workflow"))
            {
                return [
                    contexts::REVIEW_CONTENT,
                    contexts::REVIEW_FILTER,
                    contexts::REVIEW_HELP,
                    contexts::REVIEW_CLONE,
                    contexts::REVIEW_MODELS,
                    contexts::WORKFLOW_CONTROLS,
                    contexts::WORKFLOW_COMPOSE,
                    contexts::WORKFLOW_MENTIONS,
                ]
                .contains(context);
            }
            !([ids::NAVIGATION_FIRST, ids::NAVIGATION_LAST].contains(&entry.id)
                && *context == contexts::REVIEW_HELP)
        };
        entry.contexts.retain(supported);
        for binding in &mut entry.defaults {
            binding.contexts.retain(supported);
        }
    }
    for entry in &mut entries {
        if [
            ids::REVIEW_LAYOUT,
            ids::REVIEW_OPEN_LINK,
            ids::LOCAL_SCAN,
            ids::WORKFLOW_NEXT,
        ]
        .contains(&entry.id)
        {
            let key_code = match entry.id {
                ids::REVIEW_LAYOUT => BindingKey::Character('b'),
                ids::REVIEW_OPEN_LINK => BindingKey::Character('o'),
                _ => BindingKey::Enter,
            };
            for flags in [3, 5, 7, 9, 11, 13, 15] {
                entry.defaults.push(DefaultBinding {
                    chord: key(key_code.clone(), flags),
                    contexts: entry.contexts.clone(),
                });
            }
        }
    }
    // These predicates were explicit in the native Review handlers. Preserve
    // Shift source selection and Alt side ownership, not incidental modified
    // variants of ordinary KeyCode-only navigation.
    for entry in &mut entries {
        let modifier = if [ids::REVIEW_SELECT_UP, ids::REVIEW_SELECT_DOWN].contains(&entry.id) {
            4
        } else if [ids::REVIEW_SIDE_LEFT, ids::REVIEW_SIDE_RIGHT].contains(&entry.id) {
            2
        } else {
            continue;
        };
        let key_code = match entry.id {
            ids::REVIEW_SELECT_UP => BindingKey::Up,
            ids::REVIEW_SELECT_DOWN => BindingKey::Down,
            ids::REVIEW_SIDE_LEFT => BindingKey::Left,
            _ => BindingKey::Right,
        };
        for flags in 0..16 {
            if flags & modifier != 0 && flags != modifier {
                entry.defaults.push(DefaultBinding {
                    chord: key(key_code.clone(), flags),
                    contexts: entry.contexts.clone(),
                });
            }
        }
    }
    // Local list consumes only its own arrows/Enter/configuration; pane focus
    // and fast scrolling still fall through to the main Review handler.
    for entry in &mut entries {
        if [
            ids::NAVIGATION_NEXT_FOCUS,
            ids::NAVIGATION_PREVIOUS_FOCUS,
            ids::NAVIGATION_FAST_UP,
            ids::NAVIGATION_FAST_DOWN,
        ]
        .contains(&entry.id)
        {
            entry.contexts.push(contexts::REVIEW_LOCAL_LIST);
            for binding in &mut entry.defaults {
                if binding.contexts.contains(&contexts::REVIEW_HOME) {
                    binding.contexts.push(contexts::REVIEW_LOCAL_LIST);
                }
            }
        }
        let chapter = [ids::REVIEW_CHAPTER_PREVIOUS, ids::REVIEW_CHAPTER_NEXT].contains(&entry.id);
        let fast = [ids::NAVIGATION_FAST_UP, ids::NAVIGATION_FAST_DOWN].contains(&entry.id);
        let mention = [
            ids::WORKFLOW_MENTION_PREVIOUS,
            ids::WORKFLOW_MENTION_NEXT,
            ids::WORKFLOW_MENTION_ACCEPT,
        ]
        .contains(&entry.id);
        if !chapter && !fast && !mention {
            continue;
        }
        let code = match entry.id {
            ids::REVIEW_CHAPTER_PREVIOUS
            | ids::NAVIGATION_FAST_UP
            | ids::WORKFLOW_MENTION_PREVIOUS => BindingKey::Up,
            ids::WORKFLOW_MENTION_ACCEPT => BindingKey::Tab,
            _ => BindingKey::Down,
        };
        for context in entry.contexts.clone() {
            if fast && !context.0.starts_with("Review") {
                continue;
            }
            for flags in 0..16 {
                let valid = if chapter {
                    flags & 2 != 0 && !(context == contexts::REVIEW_CONTENT && flags & 4 != 0)
                } else if mention {
                    flags & 4 == 0
                } else {
                    flags & 8 != 0
                        && match context {
                            contexts::REVIEW_DEFINITION => true,
                            contexts::REVIEW_HELP => flags & 4 == 0,
                            contexts::REVIEW_CONTENT => flags & 6 == 0,
                            _ => flags & 2 == 0,
                        }
                };
                let chord = key(code.clone(), flags);
                if valid
                    && !entry
                        .defaults
                        .iter()
                        .any(|b| b.chord == chord && b.contexts.contains(&context))
                {
                    entry.defaults.push(DefaultBinding {
                        chord,
                        contexts: vec![context],
                    });
                }
            }
        }
    }
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.id == ids::SELECTION_COPY)
    {
        let editors = vec![
            contexts::REVIEW_FILTER,
            contexts::REVIEW_HELP,
            contexts::REVIEW_CLONE,
            contexts::REVIEW_MODELS,
            contexts::WORKFLOW_CONTROLS,
            contexts::WORKFLOW_COMPOSE,
            contexts::WORKFLOW_MENTIONS,
        ];
        for flags in 9..16 {
            entry.defaults.push(DefaultBinding {
                chord: key(BindingKey::Character('c'), flags),
                contexts: editors.clone(),
            });
        }
    }
    for entry in &mut entries {
        if [ids::NAVIGATION_UP, ids::NAVIGATION_DOWN].contains(&entry.id) {
            // Models and Controls accepted every non-Shift arrow; Help also
            // excluded Super because that was its fast-scroll predicate.
            for flags in [1, 2, 3, 8, 9, 10, 11] {
                let mut contexts = vec![contexts::REVIEW_MODELS, contexts::WORKFLOW_CONTROLS];
                if flags & 8 == 0 {
                    contexts.push(contexts::REVIEW_HELP);
                }
                entry.defaults.push(DefaultBinding {
                    chord: key(
                        if entry.id == ids::NAVIGATION_UP {
                            BindingKey::Up
                        } else {
                            BindingKey::Down
                        },
                        flags,
                    ),
                    contexts,
                });
            }
        }
        if entry.id == ids::APP_QUIT {
            // App's Control predicate followed its Super editor-copy guard.
            // Local setup consumed these modified events before App dispatch;
            // the exact global Ctrl+C remains unchanged in those contexts.
            let editors = [
                contexts::REVIEW_FILTER,
                contexts::REVIEW_HELP,
                contexts::REVIEW_CLONE,
                contexts::REVIEW_MODELS,
                contexts::WORKFLOW_CONTROLS,
                contexts::WORKFLOW_COMPOSE,
                contexts::WORKFLOW_MENTIONS,
            ];
            for flags in [3, 5, 7, 9, 11, 13, 15] {
                let contexts = entry
                    .contexts
                    .iter()
                    .copied()
                    .filter(|context| {
                        (context.0.starts_with("Review") || context.0.starts_with("Workflow"))
                            && ![contexts::REVIEW_LOCAL_ROOTS, contexts::REVIEW_LOCAL_SELECT]
                                .contains(context)
                            && !(flags & 8 != 0 && editors.contains(context))
                    })
                    .collect();
                entry.defaults.push(DefaultBinding {
                    chord: key(BindingKey::Character('c'), flags),
                    contexts,
                });
            }
        }
    }
    entries
}
