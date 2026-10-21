//! Registry metadata and typed adapters; all writable fields are explicitly enumerated.
use super::types::*;
use crate::storage::Config;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SettingId {
    Theme,
    Density,
    Contrast,
    MessageSeparators,
    Motion,
    ReplyToolEffect,
    SentMessageEffect,
    SidebarEffect,
    ScreenTransitionEffect,
    ReplySpeed,
    SentDuration,
    InputClearDuration,
    MaxAnimationDelay,
    ToolDisplay,
    ToolPreviewLines,
    ReasoningExpanded,
    Timestamps,
    AutoScroll,
    SidebarVisible,
    ChangesVisible,
    SidebarWidth,
    HelperPlacement,
    SessionSort,
    StartupTab,
    UnifiedDiff,
    WrapDiff,
    LineNumbers,
    DiffContext,
    Whitespace,
    VoiceEnabled,
    SendKey,
    RunningInput,
    ComposerHeight,
    PasteThreshold,
    QuestionOverridesSelection,
    CompletionNotification,
    QuestionNotification,
    ErrorNotification,
    NotificationScope,
    NotificationDelivery,
    NotificationSound,
    NotificationQuiet,
    NotificationPreset,
    Provider,
    DefaultModel,
    DefaultEffort,
    DefaultRepository,
    StartingMode,
    Rules,
    ProviderConnections,
    McpConnections,
    SlackConnection,
    RemoteConnection,
    ShortcutCapture,
    ShortcutConflicts,
    ShortcutReset,
    ShortcutTest,
    Shortcut(String),
}
impl SettingId {
    pub fn key(&self) -> Cow<'_, str> {
        match self {
            Self::Theme => Cow::Borrowed("appearance.theme"),
            Self::Density => Cow::Borrowed("appearance.density"),
            Self::Contrast => Cow::Borrowed("appearance.contrast"),
            Self::MessageSeparators => Cow::Borrowed("appearance.message_separators"),
            Self::Motion => Cow::Borrowed("motion.level"),
            Self::ReplyToolEffect => Cow::Borrowed("motion.reply_tool"),
            Self::SentMessageEffect => Cow::Borrowed("motion.sent_message"),
            Self::SidebarEffect => Cow::Borrowed("motion.sidebar"),
            Self::ScreenTransitionEffect => Cow::Borrowed("motion.screen_transition"),
            Self::ReplySpeed => Cow::Borrowed("motion.reply_speed"),
            Self::SentDuration => Cow::Borrowed("motion.sent_duration"),
            Self::InputClearDuration => Cow::Borrowed("motion.input_clear_duration"),
            Self::MaxAnimationDelay => Cow::Borrowed("motion.max_delay"),
            Self::ToolDisplay => Cow::Borrowed("conversation.tools"),
            Self::ToolPreviewLines => Cow::Borrowed("conversation.preview_lines"),
            Self::ReasoningExpanded => Cow::Borrowed("conversation.reasoning_expanded"),
            Self::Timestamps => Cow::Borrowed("conversation.timestamps"),
            Self::AutoScroll => Cow::Borrowed("conversation.auto_scroll"),
            Self::SidebarVisible => Cow::Borrowed("layout.sidebar_visible"),
            Self::ChangesVisible => Cow::Borrowed("layout.changes_visible"),
            Self::SidebarWidth => Cow::Borrowed("layout.sidebar_width"),
            Self::HelperPlacement => Cow::Borrowed("layout.helper_placement"),
            Self::SessionSort => Cow::Borrowed("layout.session_sort"),
            Self::StartupTab => Cow::Borrowed("layout.startup_tab"),
            Self::UnifiedDiff => Cow::Borrowed("diff.unified"),
            Self::WrapDiff => Cow::Borrowed("diff.wrap"),
            Self::LineNumbers => Cow::Borrowed("diff.line_numbers"),
            Self::DiffContext => Cow::Borrowed("diff.context"),
            Self::Whitespace => Cow::Borrowed("diff.whitespace"),
            Self::VoiceEnabled => Cow::Borrowed("input.voice"),
            Self::SendKey => Cow::Borrowed("input.send_key"),
            Self::RunningInput => Cow::Borrowed("input.running"),
            Self::ComposerHeight => Cow::Borrowed("input.composer_height"),
            Self::PasteThreshold => Cow::Borrowed("input.paste_threshold"),
            Self::QuestionOverridesSelection => {
                Cow::Borrowed("keyboard.question_overrides_selection")
            }
            Self::CompletionNotification => Cow::Borrowed("notifications.completion"),
            Self::QuestionNotification => Cow::Borrowed("notifications.question"),
            Self::ErrorNotification => Cow::Borrowed("notifications.error"),
            Self::NotificationScope => Cow::Borrowed("notifications.scope"),
            Self::NotificationDelivery => Cow::Borrowed("notifications.delivery"),
            Self::NotificationSound => Cow::Borrowed("notifications.sound"),
            Self::NotificationQuiet => Cow::Borrowed("notifications.quiet"),
            Self::NotificationPreset => Cow::Borrowed("notifications.preset"),
            Self::Provider => Cow::Borrowed("agents.provider"),
            Self::DefaultModel => Cow::Borrowed("agents.model"),
            Self::DefaultEffort => Cow::Borrowed("agents.effort"),
            Self::DefaultRepository => Cow::Borrowed("agents.repository"),
            Self::StartingMode => Cow::Borrowed("agents.starting_mode"),
            Self::Rules => Cow::Borrowed("agents.rules"),
            Self::ProviderConnections => Cow::Borrowed("connections.providers"),
            Self::McpConnections => Cow::Borrowed("connections.mcp"),
            Self::SlackConnection => Cow::Borrowed("connections.slack"),
            Self::RemoteConnection => Cow::Borrowed("connections.remote"),
            Self::ShortcutCapture => Cow::Borrowed("keyboard.capture"),
            Self::ShortcutConflicts => Cow::Borrowed("keyboard.conflicts"),
            Self::ShortcutReset => Cow::Borrowed("keyboard.reset"),
            Self::ShortcutTest => Cow::Borrowed("keyboard.test"),
            Self::Shortcut(action) => Cow::Owned(format!("keyboard.binding.{action}")),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Number(u32),
    Choice(String),
    Text(Option<String>),
    Shortcut(Option<KeyChord>),
    Action,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Appearance,
    Motion,
    Conversation,
    Layout,
    Code,
    Input,
    Keyboard,
    Notifications,
    AgentDefaults,
    Connections,
}
impl Category {
    pub const ALL: [Self; 10] = [
        Self::Appearance,
        Self::Motion,
        Self::Conversation,
        Self::Layout,
        Self::Code,
        Self::Input,
        Self::Keyboard,
        Self::Notifications,
        Self::AgentDefaults,
        Self::Connections,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Motion => "Motion",
            Self::Conversation => "Conversation",
            Self::Layout => "Layout & sessions",
            Self::Code => "Code & diffs",
            Self::Input => "Input & voice",
            Self::Keyboard => "Keyboard shortcuts",
            Self::Notifications => "Notifications",
            Self::AgentDefaults => "Agent defaults",
            Self::Connections => "Connections",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    ImmediateOnSave,
    NewSessions,
    NextLaunch,
    SeparateAction,
}
impl Effect {
    pub fn label(self) -> &'static str {
        match self {
            Self::ImmediateOnSave => "Immediate on save",
            Self::NewSessions => "New sessions",
            Self::NextLaunch => "Next launch",
            Self::SeparateAction => "Applies separately",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    None,
    Appearance,
    Animation,
    Conversation,
    Layout,
    Diff,
    Input,
    Shortcut,
    Notification,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    Number { min: u32, max: u32 },
    Choice,
    DynamicChoice,
    OptionalText,
    Shortcut,
    Action,
    DraftAction,
}
#[derive(Clone, Debug)]
pub struct SettingSpec {
    pub id: SettingId,
    pub category: Category,
    pub label: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    pub kind: SettingKind,
    pub choices: Vec<&'static str>,
    pub default: SettingValue,
    pub effect: Effect,
    pub preview: Preview,
}
impl SettingSpec {
    /// True only for independently stored fields. Presets are draft bundles; actions have no saved value.
    pub fn is_stored_field(&self) -> bool {
        !matches!(self.kind, SettingKind::Action | SettingKind::DraftAction)
            && self.id != SettingId::NotificationPreset
    }
    /// Metadata for an action supplied by the keyboard action registry. Contexts remain its authority.
    pub fn shortcut(action: impl Into<String>, label: &'static str) -> Self {
        Self {
            id: SettingId::Shortcut(action.into()),
            category: Category::Keyboard,
            label,
            description: "Remap this action in its supported contexts; test without dispatching it.",
            aliases: &[],
            kind: SettingKind::Shortcut,
            choices: vec![],
            default: SettingValue::Shortcut(None),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Shortcut,
        }
    }
}

pub fn get(config: &Config, id: &SettingId) -> SettingValue {
    match id {
        SettingId::Theme => {
            SettingValue::Choice(config.theme.clone().unwrap_or_else(|| "default".into()))
        }
        SettingId::Density => {
            SettingValue::Choice(config.preferences.appearance.density.key().into())
        }
        SettingId::Contrast => {
            SettingValue::Choice(config.preferences.appearance.contrast.key().into())
        }
        SettingId::MessageSeparators => {
            SettingValue::Bool(config.preferences.appearance.message_separators)
        }
        SettingId::Motion => SettingValue::Choice(config.preferences.motion.level.key().into()),
        SettingId::ReplyToolEffect => SettingValue::Bool(config.preferences.motion.reply_tool),
        SettingId::SentMessageEffect => SettingValue::Bool(config.preferences.motion.sent_message),
        SettingId::SidebarEffect => SettingValue::Bool(config.preferences.motion.sidebar),
        SettingId::ScreenTransitionEffect => {
            SettingValue::Bool(config.preferences.motion.screen_transition)
        }
        SettingId::ReplySpeed => SettingValue::Number(config.tweaks.agent_reply_cps),
        SettingId::SentDuration => SettingValue::Number(config.tweaks.sent_message_ms),
        SettingId::InputClearDuration => SettingValue::Number(config.tweaks.input_clear_ms),
        SettingId::MaxAnimationDelay => SettingValue::Number(config.tweaks.max_animation_ms),
        SettingId::ToolDisplay => {
            SettingValue::Choice(config.preferences.conversation.tool_display.key().into())
        }
        SettingId::ToolPreviewLines => {
            SettingValue::Number(config.preferences.conversation.tool_preview_lines)
        }
        SettingId::ReasoningExpanded => {
            SettingValue::Bool(config.preferences.conversation.reasoning_expanded)
        }
        SettingId::Timestamps => SettingValue::Bool(config.preferences.conversation.timestamps),
        SettingId::AutoScroll => {
            SettingValue::Choice(config.preferences.conversation.auto_scroll.key().into())
        }
        SettingId::SidebarVisible => SettingValue::Bool(config.agent_list_visible),
        SettingId::ChangesVisible => SettingValue::Bool(config.agent_changes_visible),
        SettingId::SidebarWidth => SettingValue::Number(config.preferences.layout.sidebar_width),
        SettingId::HelperPlacement => SettingValue::Choice(
            config
                .preferences
                .layout
                .helper_placement
                .unwrap_or(if config.agent_panel_right {
                    HelperPlacement::Right
                } else {
                    HelperPlacement::Main
                })
                .key()
                .into(),
        ),
        SettingId::SessionSort => {
            SettingValue::Choice(config.preferences.layout.session_sort.key().into())
        }
        SettingId::StartupTab => {
            SettingValue::Choice(config.preferences.layout.startup_tab.key().into())
        }
        SettingId::UnifiedDiff => SettingValue::Bool(config.unified),
        SettingId::WrapDiff => SettingValue::Bool(config.wrap_diff),
        SettingId::LineNumbers => SettingValue::Bool(config.preferences.code.line_numbers),
        SettingId::DiffContext => SettingValue::Number(config.preferences.code.context_lines),
        SettingId::Whitespace => {
            SettingValue::Choice(config.preferences.code.whitespace.key().into())
        }
        SettingId::VoiceEnabled => SettingValue::Bool(config.voice_enabled),
        SettingId::SendKey => SettingValue::Choice(config.preferences.input.send_key.key().into()),
        SettingId::RunningInput => {
            SettingValue::Choice(config.preferences.input.running.key().into())
        }
        SettingId::ComposerHeight => {
            SettingValue::Number(config.preferences.input.composer_max_height)
        }
        SettingId::PasteThreshold => SettingValue::Choice(
            config
                .preferences
                .input
                .paste_threshold
                .map(|n| n.to_string())
                .unwrap_or_else(|| "never".into()),
        ),
        SettingId::QuestionOverridesSelection => {
            SettingValue::Bool(config.preferences.keyboard.question_overrides_selection)
        }
        SettingId::CompletionNotification => {
            SettingValue::Bool(config.preferences.notifications.completion)
        }
        SettingId::QuestionNotification => {
            SettingValue::Bool(config.preferences.notifications.question_approval)
        }
        SettingId::ErrorNotification => SettingValue::Bool(config.preferences.notifications.error),
        SettingId::NotificationScope => {
            SettingValue::Choice(config.preferences.notifications.scope.key().into())
        }
        SettingId::NotificationDelivery => {
            SettingValue::Choice(config.preferences.notifications.delivery.key().into())
        }
        SettingId::NotificationSound => SettingValue::Bool(config.preferences.notifications.sound),
        SettingId::NotificationQuiet => SettingValue::Bool(config.preferences.notifications.quiet),
        SettingId::NotificationPreset => SettingValue::Choice(notification_preset(config).into()),
        SettingId::Provider => {
            SettingValue::Choice(config.agent_defaults.resolved_provider().key().into())
        }
        SettingId::DefaultModel => SettingValue::Text(config.agent_defaults.model.clone()),
        SettingId::DefaultEffort => SettingValue::Text(config.agent_defaults.effort.clone()),
        SettingId::DefaultRepository => SettingValue::Text(
            config
                .agent_defaults
                .repository
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        ),
        SettingId::StartingMode => SettingValue::Text(config.agent_defaults.starting_mode.clone()),
        SettingId::Rules => SettingValue::Action,
        SettingId::ProviderConnections => SettingValue::Action,
        SettingId::McpConnections => SettingValue::Action,
        SettingId::SlackConnection => SettingValue::Action,
        SettingId::RemoteConnection => SettingValue::Action,
        SettingId::ShortcutCapture => SettingValue::Action,
        SettingId::ShortcutConflicts => SettingValue::Action,
        SettingId::ShortcutReset => SettingValue::Action,
        SettingId::ShortcutTest => SettingValue::Action,
        SettingId::Shortcut(action) => {
            SettingValue::Shortcut(config.preferences.keyboard.bindings.get(action).cloned())
        }
    }
}
fn notification_preset(config: &Config) -> &'static str {
    let n = &config.preferences.notifications;
    if n.quiet && !n.sound {
        "quiet"
    } else if n.completion
        && n.question_approval
        && n.error
        && !n.quiet
        && n.scope == NotificationScope::All
    {
        "every_update"
    } else if n == &Notifications::default() {
        "balanced"
    } else {
        "custom"
    }
}
/// A raw provider snapshot distinguishes explicit provider selection from legacy model inference.
pub(crate) fn snapshot(config: &Config, id: &SettingId) -> SettingValue {
    if *id == SettingId::Provider {
        SettingValue::Text(config.agent_defaults.provider.map(|p| p.key().into()))
    } else {
        get(config, id)
    }
}

pub fn registry() -> Vec<SettingSpec> {
    let defaults = Config::default();
    let mut entries = vec![
        SettingSpec {
            id: SettingId::Theme,
            category: Category::Appearance,
            label: "Theme",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: crate::theme::themes()
                .iter()
                .map(|theme| theme.id)
                .collect(),
            default: get(&defaults, &SettingId::Theme),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Appearance,
        },
        SettingSpec {
            id: SettingId::Density,
            category: Category::Appearance,
            label: "Density",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["comfortable", "compact"],
            default: get(&defaults, &SettingId::Density),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Appearance,
        },
        SettingSpec {
            id: SettingId::Contrast,
            category: Category::Appearance,
            label: "Contrast",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["standard", "high"],
            default: get(&defaults, &SettingId::Contrast),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Appearance,
        },
        SettingSpec {
            id: SettingId::MessageSeparators,
            category: Category::Appearance,
            label: "Message separators",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::MessageSeparators),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Appearance,
        },
        SettingSpec {
            id: SettingId::Motion,
            category: Category::Motion,
            label: "Animations",
            description: "Choose full, reduced or no motion. Zero timing values disable the respective effect.",
            aliases: &[
                "stop flashing",
                "animations",
                "reduce motion",
                "no animation",
            ],
            kind: SettingKind::Choice,
            choices: vec!["full", "reduced", "off"],
            default: get(&defaults, &SettingId::Motion),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::ReplyToolEffect,
            category: Category::Motion,
            label: "Reply and tool effects",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::ReplyToolEffect),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::SentMessageEffect,
            category: Category::Motion,
            label: "Sent-message effect",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::SentMessageEffect),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::SidebarEffect,
            category: Category::Motion,
            label: "Sidebar effects",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::SidebarEffect),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::ScreenTransitionEffect,
            category: Category::Motion,
            label: "Screen transitions",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::ScreenTransitionEffect),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::ReplySpeed,
            category: Category::Motion,
            label: "Reply speed",
            description: "Characters per second; 0 disables reply reveal.",
            aliases: &[],
            kind: SettingKind::Number {
                min: 0,
                max: u32::MAX,
            },
            choices: vec![],
            default: get(&defaults, &SettingId::ReplySpeed),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::SentDuration,
            category: Category::Motion,
            label: "Sent-message duration",
            description: "Milliseconds; 0 disables this effect.",
            aliases: &[],
            kind: SettingKind::Number {
                min: 0,
                max: u32::MAX,
            },
            choices: vec![],
            default: get(&defaults, &SettingId::SentDuration),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::InputClearDuration,
            category: Category::Motion,
            label: "Input clearing duration",
            description: "Milliseconds; 0 disables this effect.",
            aliases: &[],
            kind: SettingKind::Number {
                min: 0,
                max: u32::MAX,
            },
            choices: vec![],
            default: get(&defaults, &SettingId::InputClearDuration),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::MaxAnimationDelay,
            category: Category::Motion,
            label: "Maximum animation delay",
            description: "Milliseconds of maximum lag; 0 means no cap.",
            aliases: &[],
            kind: SettingKind::Number {
                min: 0,
                max: u32::MAX,
            },
            choices: vec![],
            default: get(&defaults, &SettingId::MaxAnimationDelay),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Animation,
        },
        SettingSpec {
            id: SettingId::ToolDisplay,
            category: Category::Conversation,
            label: "Tool display",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["collapsed", "compact", "expanded"],
            default: get(&defaults, &SettingId::ToolDisplay),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Conversation,
        },
        SettingSpec {
            id: SettingId::ToolPreviewLines,
            category: Category::Conversation,
            label: "Tool preview lines",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Number { min: 1, max: 20 },
            choices: vec![],
            default: get(&defaults, &SettingId::ToolPreviewLines),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Conversation,
        },
        SettingSpec {
            id: SettingId::ReasoningExpanded,
            category: Category::Conversation,
            label: "Expand reasoning",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::ReasoningExpanded),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Conversation,
        },
        SettingSpec {
            id: SettingId::Timestamps,
            category: Category::Conversation,
            label: "Timestamps",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::Timestamps),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Conversation,
        },
        SettingSpec {
            id: SettingId::AutoScroll,
            category: Category::Conversation,
            label: "Auto-scroll",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["when_at_bottom", "always", "manual"],
            default: get(&defaults, &SettingId::AutoScroll),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Conversation,
        },
        SettingSpec {
            id: SettingId::SidebarVisible,
            category: Category::Layout,
            label: "Show sidebar",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::SidebarVisible),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Layout,
        },
        SettingSpec {
            id: SettingId::ChangesVisible,
            category: Category::Layout,
            label: "Show Changes",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::ChangesVisible),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Layout,
        },
        SettingSpec {
            id: SettingId::SidebarWidth,
            category: Category::Layout,
            label: "Sidebar width",
            description: "0 means Auto; explicit widths are 20–40 columns.",
            aliases: &[],
            kind: SettingKind::Number { min: 0, max: 40 },
            choices: vec![],
            default: get(&defaults, &SettingId::SidebarWidth),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Layout,
        },
        SettingSpec {
            id: SettingId::HelperPlacement,
            category: Category::Layout,
            label: "Helper panel placement",
            description: "Right, Bottom or Main area; Main area replaces the conversation with the helper.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["right", "bottom", "main"],
            default: get(&defaults, &SettingId::HelperPlacement),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Layout,
        },
        SettingSpec {
            id: SettingId::SessionSort,
            category: Category::Layout,
            label: "Session sorting",
            description: "Sort sessions within their pinned groups; pinned groups stay intact.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["active_first", "recent", "manual"],
            default: get(&defaults, &SettingId::SessionSort),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Layout,
        },
        SettingSpec {
            id: SettingId::StartupTab,
            category: Category::Layout,
            label: "Startup tab",
            description: "Choose the initial tab for the next terminal window.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["last_used", "agents", "reviews"],
            default: get(&defaults, &SettingId::StartupTab),
            effect: Effect::NextLaunch,
            preview: Preview::Layout,
        },
        SettingSpec {
            id: SettingId::UnifiedDiff,
            category: Category::Code,
            label: "Unified diff",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::UnifiedDiff),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Diff,
        },
        SettingSpec {
            id: SettingId::WrapDiff,
            category: Category::Code,
            label: "Wrap code",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::WrapDiff),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Diff,
        },
        SettingSpec {
            id: SettingId::LineNumbers,
            category: Category::Code,
            label: "Line numbers",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::LineNumbers),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Diff,
        },
        SettingSpec {
            id: SettingId::DiffContext,
            category: Category::Code,
            label: "Diff context lines",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Number { min: 0, max: 10 },
            choices: vec![],
            default: get(&defaults, &SettingId::DiffContext),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Diff,
        },
        SettingSpec {
            id: SettingId::Whitespace,
            category: Category::Code,
            label: "Whitespace markers",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["off", "selected_lines", "all_lines"],
            default: get(&defaults, &SettingId::Whitespace),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Diff,
        },
        SettingSpec {
            id: SettingId::VoiceEnabled,
            category: Category::Input,
            label: "Enable voice",
            description: "Hold Space to speak. Ready Codex sessions support conversation; dictation inserts text on release and Enter sends. Repeated keys support terminals without key-release reporting.",
            aliases: &["microphone", "dictation", "hold space", "voice behavior"],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::VoiceEnabled),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Input,
        },
        SettingSpec {
            id: SettingId::SendKey,
            category: Category::Input,
            label: "Send message with",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["enter", "ctrl_enter"],
            default: get(&defaults, &SettingId::SendKey),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Input,
        },
        SettingSpec {
            id: SettingId::RunningInput,
            category: Category::Input,
            label: "Input while running",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["steer", "queue"],
            default: get(&defaults, &SettingId::RunningInput),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Input,
        },
        SettingSpec {
            id: SettingId::ComposerHeight,
            category: Category::Input,
            label: "Composer maximum height",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Number { min: 3, max: 20 },
            choices: vec![],
            default: get(&defaults, &SettingId::ComposerHeight),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Input,
        },
        SettingSpec {
            id: SettingId::PasteThreshold,
            category: Category::Input,
            label: "Collapse pasted text",
            description: "Collapse pasted text at 250, 500 or 1000 characters, or Never.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["250", "500", "1000", "never"],
            default: get(&defaults, &SettingId::PasteThreshold),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Input,
        },
        SettingSpec {
            id: SettingId::QuestionOverridesSelection,
            category: Category::Keyboard,
            label: "Question key overrides text selection",
            description: "Explicitly allow the question shortcut to replace composer text selection, including Shift+Left.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::QuestionOverridesSelection),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Shortcut,
        },
        SettingSpec {
            id: SettingId::CompletionNotification,
            category: Category::Notifications,
            label: "Completion notifications",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::CompletionNotification),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::QuestionNotification,
            category: Category::Notifications,
            label: "Question and approval notifications",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::QuestionNotification),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::ErrorNotification,
            category: Category::Notifications,
            label: "Error notifications",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::ErrorNotification),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::NotificationScope,
            category: Category::Notifications,
            label: "Notify for",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["background_sessions", "all", "app_unfocused"],
            default: get(&defaults, &SettingId::NotificationScope),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::NotificationDelivery,
            category: Category::Notifications,
            label: "Notification delivery",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["in_app", "desktop", "both"],
            default: get(&defaults, &SettingId::NotificationDelivery),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::NotificationSound,
            category: Category::Notifications,
            label: "Notification sound",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::NotificationSound),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::NotificationQuiet,
            category: Category::Notifications,
            label: "Quiet notifications",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::Bool,
            choices: vec![],
            default: get(&defaults, &SettingId::NotificationQuiet),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::NotificationPreset,
            category: Category::Notifications,
            label: "Notification preset",
            description: "Apply a Balanced, Quiet or Every update bundle to the draft; no named profile is saved.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["balanced", "quiet", "every_update"],
            default: get(&defaults, &SettingId::NotificationPreset),
            effect: Effect::ImmediateOnSave,
            preview: Preview::Notification,
        },
        SettingSpec {
            id: SettingId::Provider,
            category: Category::AgentDefaults,
            label: "Default provider",
            description: "Explicit provider for new sessions; old configurations infer it from the model.",
            aliases: &[],
            kind: SettingKind::Choice,
            choices: vec!["codex", "claude"],
            default: get(&defaults, &SettingId::Provider),
            effect: Effect::NewSessions,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::DefaultModel,
            category: Category::AgentDefaults,
            label: "Default model",
            description: "Provider-specific options are loaded dynamically; empty uses the provider default.",
            aliases: &[],
            kind: SettingKind::DynamicChoice,
            choices: vec![],
            default: get(&defaults, &SettingId::DefaultModel),
            effect: Effect::NewSessions,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::DefaultEffort,
            category: Category::AgentDefaults,
            label: "Default effort",
            description: "Provider-specific options are loaded dynamically; empty uses the provider default.",
            aliases: &[],
            kind: SettingKind::DynamicChoice,
            choices: vec![],
            default: get(&defaults, &SettingId::DefaultEffort),
            effect: Effect::NewSessions,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::DefaultRepository,
            category: Category::AgentDefaults,
            label: "Default repository",
            description: "Applies to this preference when saved.",
            aliases: &[],
            kind: SettingKind::OptionalText,
            choices: vec![],
            default: get(&defaults, &SettingId::DefaultRepository),
            effect: Effect::NewSessions,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::StartingMode,
            category: Category::AgentDefaults,
            label: "Starting mode",
            description: "Provider-specific options are loaded dynamically; empty uses the provider default.",
            aliases: &[],
            kind: SettingKind::DynamicChoice,
            choices: vec![],
            default: get(&defaults, &SettingId::StartingMode),
            effect: Effect::NewSessions,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::Rules,
            category: Category::AgentDefaults,
            label: "Manage rules",
            description: "Explicit action; applies separately from draft preferences.",
            aliases: &[],
            kind: SettingKind::Action,
            choices: vec![],
            default: get(&defaults, &SettingId::Rules),
            effect: Effect::SeparateAction,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::ProviderConnections,
            category: Category::Connections,
            label: "Provider status and sign-in",
            description: "Explicit action; applies separately from draft preferences.",
            aliases: &[],
            kind: SettingKind::Action,
            choices: vec![],
            default: get(&defaults, &SettingId::ProviderConnections),
            effect: Effect::SeparateAction,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::McpConnections,
            category: Category::Connections,
            label: "Manage MCP servers",
            description: "Explicit action; applies separately from draft preferences.",
            aliases: &[],
            kind: SettingKind::Action,
            choices: vec![],
            default: get(&defaults, &SettingId::McpConnections),
            effect: Effect::SeparateAction,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::SlackConnection,
            category: Category::Connections,
            label: "Manage Slack connection",
            description: "Explicit action; applies separately from draft preferences.",
            aliases: &[],
            kind: SettingKind::Action,
            choices: vec![],
            default: get(&defaults, &SettingId::SlackConnection),
            effect: Effect::SeparateAction,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::RemoteConnection,
            category: Category::Connections,
            label: "Manage remote connections",
            description: "Explicit action; applies separately from draft preferences.",
            aliases: &[],
            kind: SettingKind::Action,
            choices: vec![],
            default: get(&defaults, &SettingId::RemoteConnection),
            effect: Effect::SeparateAction,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::ShortcutCapture,
            category: Category::Keyboard,
            label: "Capture shortcut",
            description: "Edits or tests the current draft; Save is required to apply binding changes.",
            aliases: &[],
            kind: SettingKind::DraftAction,
            choices: vec![],
            default: get(&defaults, &SettingId::ShortcutCapture),
            effect: Effect::ImmediateOnSave,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::ShortcutConflicts,
            category: Category::Keyboard,
            label: "Shortcut context conflicts",
            description: "Edits or tests the current draft; Save is required to apply binding changes.",
            aliases: &[],
            kind: SettingKind::DraftAction,
            choices: vec![],
            default: get(&defaults, &SettingId::ShortcutConflicts),
            effect: Effect::ImmediateOnSave,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::ShortcutReset,
            category: Category::Keyboard,
            label: "Reset shortcuts in draft",
            description: "Reset binding overrides in the current draft; Save is still required.",
            aliases: &[],
            kind: SettingKind::DraftAction,
            choices: vec![],
            default: get(&defaults, &SettingId::ShortcutReset),
            effect: Effect::ImmediateOnSave,
            preview: Preview::None,
        },
        SettingSpec {
            id: SettingId::ShortcutTest,
            category: Category::Keyboard,
            label: "Test shortcut safely",
            description: "Try a binding safely without sending, approving, interrupting or activating voice.",
            aliases: &[],
            kind: SettingKind::DraftAction,
            choices: vec![],
            default: get(&defaults, &SettingId::ShortcutTest),
            effect: Effect::ImmediateOnSave,
            preview: Preview::None,
        },
    ];
    let mut question = SettingSpec::shortcut("question.open", "Open question composer");
    question.aliases = &["change question key", "question shortcut", "alt up"];
    question.description = "Default Alt+Up. A selection key requires explicitly enabling composer text-selection override.";
    entries.push(question);
    entries
}

pub fn spec(id: &SettingId) -> Option<SettingSpec> {
    if let SettingId::Shortcut(action) = id {
        return Some(SettingSpec::shortcut(action.clone(), "Action shortcut"));
    }
    registry().into_iter().find(|entry| entry.id == *id)
}
/// Search labels, descriptions and curated aliases. Exact label/alias wins over partial matches.
pub fn search(query: &str) -> Vec<SettingSpec> {
    let query = query.trim().to_lowercase();
    let mut ranked: Vec<_> = registry()
        .into_iter()
        .filter_map(|entry| {
            let label = entry.label.to_lowercase();
            let aliases: Vec<_> = entry
                .aliases
                .iter()
                .map(|alias| alias.to_lowercase())
                .collect();
            let rank = if query.is_empty() || label == query || aliases.contains(&query) {
                0
            } else if label.contains(&query) || aliases.iter().any(|alias| alias.contains(&query)) {
                1
            } else if entry.description.to_lowercase().contains(&query) {
                2
            } else {
                return None;
            };
            Some((rank, entry))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

pub fn validate(id: &SettingId, value: &SettingValue) -> Result<()> {
    let spec = spec(id).ok_or_else(|| anyhow::anyhow!("Unknown setting: {}", id.key()))?;
    match (&spec.kind, value) {
        (SettingKind::Bool, SettingValue::Bool(_)) => {}
        (SettingKind::Number { min, max }, SettingValue::Number(n)) => ensure!(
            (*min..=*max).contains(n),
            "{} must be {min}–{max}",
            spec.label
        ),
        (SettingKind::Choice, SettingValue::Choice(key)) => ensure!(
            spec.choices.contains(&key.as_str()),
            "Invalid choice for {}",
            spec.label
        ),
        (SettingKind::OptionalText | SettingKind::DynamicChoice, SettingValue::Text(text)) => {
            ensure!(
                text.as_ref()
                    .is_none_or(|text| text.len() <= 4096 && !text.chars().any(char::is_control)),
                "Invalid setting text"
            );
        }
        (SettingKind::Shortcut, SettingValue::Shortcut(chord)) => {
            if let Some(chord) = chord {
                chord.validate()?;
            }
            if let SettingId::Shortcut(action) = id {
                ensure!(
                    !action.is_empty()
                        && action.len() <= 128
                        && action
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                    "Invalid shortcut action ID"
                );
            }
        }
        _ => bail!("{} is not an editable value of this type", spec.label),
    }
    match (id, value) {
        (SettingId::SidebarWidth, SettingValue::Number(n)) => ensure!(
            *n == 0 || (20..=40).contains(n),
            "Sidebar width must be Auto (0) or 20–40"
        ),
        (SettingId::DiffContext, SettingValue::Number(n)) => {
            ensure!([0, 3, 5, 10].contains(n), "Context must be 0, 3, 5 or 10")
        }
        _ => {}
    }
    Ok(())
}

/// Validates and updates exactly one registered field (or a documented preset bundle).
pub fn set(config: &mut Config, id: &SettingId, value: SettingValue) -> Result<()> {
    validate(id, &value)?;
    match (id, value) {
        (SettingId::Theme, SettingValue::Choice(value)) => {
            config.theme = (value != "default").then_some(value)
        }
        (SettingId::Density, SettingValue::Choice(value)) => {
            config.preferences.appearance.density =
                Density::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::Contrast, SettingValue::Choice(value)) => {
            config.preferences.appearance.contrast =
                Contrast::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::MessageSeparators, SettingValue::Bool(value)) => {
            config.preferences.appearance.message_separators = value
        }
        (SettingId::Motion, SettingValue::Choice(value)) => {
            config.preferences.motion.level =
                Motion::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::ReplyToolEffect, SettingValue::Bool(value)) => {
            config.preferences.motion.reply_tool = value
        }
        (SettingId::SentMessageEffect, SettingValue::Bool(value)) => {
            config.preferences.motion.sent_message = value
        }
        (SettingId::SidebarEffect, SettingValue::Bool(value)) => {
            config.preferences.motion.sidebar = value
        }
        (SettingId::ScreenTransitionEffect, SettingValue::Bool(value)) => {
            config.preferences.motion.screen_transition = value
        }
        (SettingId::ReplySpeed, SettingValue::Number(value)) => {
            config.tweaks.agent_reply_cps = value
        }
        (SettingId::SentDuration, SettingValue::Number(value)) => {
            config.tweaks.sent_message_ms = value
        }
        (SettingId::InputClearDuration, SettingValue::Number(value)) => {
            config.tweaks.input_clear_ms = value
        }
        (SettingId::MaxAnimationDelay, SettingValue::Number(value)) => {
            config.tweaks.max_animation_ms = value
        }
        (SettingId::ToolDisplay, SettingValue::Choice(value)) => {
            config.preferences.conversation.tool_display =
                ToolDisplay::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::ToolPreviewLines, SettingValue::Number(value)) => {
            config.preferences.conversation.tool_preview_lines = value
        }
        (SettingId::ReasoningExpanded, SettingValue::Bool(value)) => {
            config.preferences.conversation.reasoning_expanded = value
        }
        (SettingId::Timestamps, SettingValue::Bool(value)) => {
            config.preferences.conversation.timestamps = value
        }
        (SettingId::AutoScroll, SettingValue::Choice(value)) => {
            config.preferences.conversation.auto_scroll =
                AutoScroll::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::SidebarVisible, SettingValue::Bool(value)) => config.agent_list_visible = value,
        (SettingId::ChangesVisible, SettingValue::Bool(value)) => {
            config.agent_changes_visible = value
        }
        (SettingId::SidebarWidth, SettingValue::Number(value)) => {
            config.preferences.layout.sidebar_width = value
        }
        (SettingId::HelperPlacement, SettingValue::Choice(value)) => {
            let placement = HelperPlacement::parse(&value)
                .ok_or_else(|| anyhow::anyhow!("Invalid helper placement"))?;
            config.preferences.layout.helper_placement =
                (placement == HelperPlacement::Bottom).then_some(placement);
            config.agent_panel_right = placement != HelperPlacement::Main;
        }
        (SettingId::SessionSort, SettingValue::Choice(value)) => {
            config.preferences.layout.session_sort =
                SessionSort::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::StartupTab, SettingValue::Choice(value)) => {
            config.preferences.layout.startup_tab =
                StartupTab::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::UnifiedDiff, SettingValue::Bool(value)) => config.unified = value,
        (SettingId::WrapDiff, SettingValue::Bool(value)) => config.wrap_diff = value,
        (SettingId::LineNumbers, SettingValue::Bool(value)) => {
            config.preferences.code.line_numbers = value
        }
        (SettingId::DiffContext, SettingValue::Number(value)) => {
            config.preferences.code.context_lines = value
        }
        (SettingId::Whitespace, SettingValue::Choice(value)) => {
            config.preferences.code.whitespace =
                Whitespace::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::VoiceEnabled, SettingValue::Bool(value)) => config.voice_enabled = value,
        (SettingId::SendKey, SettingValue::Choice(value)) => {
            config.preferences.input.send_key =
                SendKey::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::RunningInput, SettingValue::Choice(value)) => {
            config.preferences.input.running =
                RunningInput::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::ComposerHeight, SettingValue::Number(value)) => {
            config.preferences.input.composer_max_height = value
        }
        (SettingId::PasteThreshold, SettingValue::Choice(value)) => {
            config.preferences.input.paste_threshold = if value == "never" {
                None
            } else {
                Some(value.parse()?)
            }
        }
        (SettingId::QuestionOverridesSelection, SettingValue::Bool(value)) => {
            config.preferences.keyboard.question_overrides_selection = value
        }
        (SettingId::CompletionNotification, SettingValue::Bool(value)) => {
            config.preferences.notifications.completion = value
        }
        (SettingId::QuestionNotification, SettingValue::Bool(value)) => {
            config.preferences.notifications.question_approval = value
        }
        (SettingId::ErrorNotification, SettingValue::Bool(value)) => {
            config.preferences.notifications.error = value
        }
        (SettingId::NotificationScope, SettingValue::Choice(value)) => {
            config.preferences.notifications.scope =
                NotificationScope::parse(&value).ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::NotificationDelivery, SettingValue::Choice(value)) => {
            config.preferences.notifications.delivery = NotificationDelivery::parse(&value)
                .ok_or_else(|| anyhow::anyhow!("Invalid choice"))?
        }
        (SettingId::NotificationSound, SettingValue::Bool(value)) => {
            config.preferences.notifications.sound = value
        }
        (SettingId::NotificationQuiet, SettingValue::Bool(value)) => {
            config.preferences.notifications.quiet = value
        }
        (SettingId::NotificationPreset, SettingValue::Choice(value)) => {
            let n = &mut config.preferences.notifications;
            match value.as_str() {
                "balanced" => *n = Notifications::default(),
                "quiet" => {
                    n.quiet = true;
                    n.sound = false;
                }
                "every_update" => {
                    n.quiet = false;
                    n.completion = true;
                    n.question_approval = true;
                    n.error = true;
                    n.scope = NotificationScope::All;
                }
                _ => bail!("Unknown notification preset"),
            }
        }
        (SettingId::Provider, SettingValue::Choice(value)) => {
            config.agent_defaults.provider = Some(if value == "claude" {
                crate::agents::provider::Provider::Claude
            } else {
                crate::agents::provider::Provider::Codex
            })
        }
        (SettingId::DefaultModel, SettingValue::Text(value)) => {
            config.agent_defaults.model = value.filter(|s| !s.is_empty())
        }
        (SettingId::DefaultEffort, SettingValue::Text(value)) => {
            config.agent_defaults.effort = value.filter(|s| !s.is_empty())
        }
        (SettingId::DefaultRepository, SettingValue::Text(value)) => {
            config.agent_defaults.repository = value.filter(|s| !s.is_empty()).map(Into::into)
        }
        (SettingId::StartingMode, SettingValue::Text(value)) => {
            config.agent_defaults.starting_mode = value.filter(|s| !s.is_empty())
        }
        (SettingId::Shortcut(action), SettingValue::Shortcut(value)) => {
            if let Some(chord) = value {
                config
                    .preferences
                    .keyboard
                    .bindings
                    .insert(action.clone(), chord);
            } else {
                config.preferences.keyboard.bindings.remove(action);
            }
        }
        _ => bail!("Unsupported settings value"),
    }
    Ok(())
}
