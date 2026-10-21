//! Typed preference storage. Existing Config fields remain canonical for legacy settings.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

macro_rules! choice {
    ($name:ident, $default:ident, {$($variant:ident => $key:literal),+ $(,)?}) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $key)] $variant),+ }
        impl Default for $name { fn default() -> Self { Self::$default } }
        impl $name {
            pub fn key(self) -> &'static str { match self { $(Self::$variant => $key),+ } }
            pub fn parse(key: &str) -> Option<Self> { match key { $($key => Some(Self::$variant)),+, _ => None } }
        }
    }
}
choice!(Density, Comfortable, {Comfortable => "comfortable", Compact => "compact"});
choice!(Contrast, Standard, {Standard => "standard", High => "high"});
choice!(Motion, Full, {Full => "full", Reduced => "reduced", Off => "off"});
choice!(ToolDisplay, Compact, {Collapsed => "collapsed", Compact => "compact", Expanded => "expanded"});
choice!(AutoScroll, WhenAtBottom, {WhenAtBottom => "when_at_bottom", Always => "always", Manual => "manual"});
choice!(HelperPlacement, Right, {Right => "right", Bottom => "bottom", Main => "main"});
choice!(SessionSort, Manual, {ActiveFirst => "active_first", Recent => "recent", Manual => "manual"});
choice!(StartupTab, LastUsed, {LastUsed => "last_used", Agents => "agents", Reviews => "reviews"});
choice!(Whitespace, Off, {Off => "off", SelectedLines => "selected_lines", AllLines => "all_lines"});
choice!(SendKey, Enter, {Enter => "enter", CtrlEnter => "ctrl_enter"});
choice!(RunningInput, Steer, {Steer => "steer", Queue => "queue"});
choice!(NotificationScope, BackgroundSessions, {BackgroundSessions => "background_sessions", All => "all", AppUnfocused => "app_unfocused"});
choice!(NotificationDelivery, InApp, {InApp => "in_app", Desktop => "desktop", Both => "both"});

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub appearance: Appearance,
    pub motion: MotionPreferences,
    pub conversation: Conversation,
    pub layout: Layout,
    pub code: Code,
    pub input: Input,
    pub keyboard: Keyboard,
    pub notifications: Notifications,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub density: Density,
    pub contrast: Contrast,
    pub message_separators: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MotionPreferences {
    pub level: Motion,
    pub reply_tool: bool,
    pub sent_message: bool,
    pub sidebar: bool,
    pub screen_transition: bool,
}
impl Default for MotionPreferences {
    fn default() -> Self {
        Self {
            level: Motion::Full,
            reply_tool: true,
            sent_message: true,
            sidebar: true,
            screen_transition: true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Conversation {
    pub tool_display: ToolDisplay,
    pub tool_preview_lines: u32,
    pub reasoning_expanded: bool,
    pub timestamps: bool,
    pub auto_scroll: AutoScroll,
}
impl Default for Conversation {
    fn default() -> Self {
        Self {
            tool_display: ToolDisplay::Compact,
            tool_preview_lines: 4,
            reasoning_expanded: false,
            timestamps: false,
            auto_scroll: AutoScroll::WhenAtBottom,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// Zero means automatic; explicit widths are 20–40 columns.
    pub sidebar_width: u32,
    /// None derives Right/Main from legacy Config.agent_panel_right.
    pub helper_placement: Option<HelperPlacement>,
    pub session_sort: SessionSort,
    pub startup_tab: StartupTab,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Code {
    pub line_numbers: bool,
    pub context_lines: u32,
    pub whitespace: Whitespace,
}
impl Default for Code {
    fn default() -> Self {
        Self {
            line_numbers: true,
            context_lines: 3,
            whitespace: Whitespace::Off,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    pub send_key: SendKey,
    pub running: RunningInput,
    pub composer_max_height: u32,
    /// None means never collapse pasted text.
    pub paste_threshold: Option<u32>,
}
impl Default for Input {
    fn default() -> Self {
        Self {
            send_key: SendKey::Enter,
            running: RunningInput::Steer,
            composer_max_height: 10,
            paste_threshold: Some(500),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keyboard {
    /// Action IDs and contexts are defined by the action registry, never by this file.
    pub bindings: BTreeMap<String, KeyChord>,
    pub question_overrides_selection: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingKey {
    Character(char),
    Enter,
    Escape,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyModifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChord {
    pub key: BindingKey,
    #[serde(default)]
    pub modifiers: KeyModifiers,
}
impl KeyChord {
    pub fn character(character: char) -> Self {
        Self {
            key: BindingKey::Character(character),
            modifiers: KeyModifiers::default(),
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !matches!(self.key, BindingKey::Character(c) if c.is_control()),
            "Use a named key for control characters"
        );
        anyhow::ensure!(
            !matches!(self.key, BindingKey::Function(n) if !(1..=24).contains(&n)),
            "Function key must be F1–F24"
        );
        // Escape remains a safe modal escape; action/context conflicts are checked by the action registry.
        anyhow::ensure!(
            self.key != BindingKey::Escape,
            "Escape is reserved for closing dialogs"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub completion: bool,
    pub question_approval: bool,
    pub error: bool,
    pub scope: NotificationScope,
    pub delivery: NotificationDelivery,
    pub sound: bool,
    pub quiet: bool,
}
impl Default for Notifications {
    fn default() -> Self {
        Self {
            completion: true,
            question_approval: true,
            error: true,
            scope: NotificationScope::BackgroundSessions,
            delivery: NotificationDelivery::InApp,
            sound: false,
            quiet: false,
        }
    }
}
