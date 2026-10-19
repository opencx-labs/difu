//! Colour themes. difu draws with logical colours — `Color::Indexed` slots
//! reserved for its roles — and the active theme resolves them as each frame is
//! presented. Switching themes therefore recolours everything on screen,
//! including cached rows, and the Default theme reproduces difu's original colours.
use ratatui::{buffer::Buffer, style::Color};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    Background,
    Panel,
    Text,
    Dim,
    Border,
    Accent,
    /// Text drawn on an accent background.
    OnAccent,
    Green,
    Purple,
    Yellow,
    Red,
    AddBg,
    RemoveBg,
    UserMessage,
    UserMessageText,
    Keyword,
    String,
    Number,
    Comment,
    FocusedEntry,
    SelectedCode,
    FocusedCode,
}
const ROLES: usize = Role::FocusedCode as usize + 1;
/// 256-colour indexes reserved for roles. Widgets never draw these indexes
/// directly; themes resolve them to any colour, including other indexes.
const BASE: u8 = 100;

pub const fn slot(role: Role) -> Color {
    Color::Indexed(BASE + role as u8)
}

pub fn is_slot(color: Color) -> bool {
    matches!(color, Color::Indexed(index) if (BASE..BASE + ROLES as u8).contains(&index))
}

fn palette(entries: [(Role, Color); ROLES]) -> [Color; ROLES] {
    let mut colors = [Color::Reset; ROLES];
    for (role, color) in entries {
        if let Some(slot) = colors.get_mut(role as usize) {
            *slot = color;
        }
    }
    colors
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn mix(from: u32, to: u32, amount: f32) -> Color {
    let channel = |shift: u32| {
        let (a, b) = (
            ((from >> shift) & 0xff) as f32,
            ((to >> shift) & 0xff) as f32,
        );
        (a + (b - a) * amount).round() as u8
    };
    Color::Rgb(channel(16), channel(8), channel(0))
}

pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    colors: [Color; ROLES],
}
impl Theme {
    pub fn color(&self, role: Role) -> Color {
        self.colors
            .get(role as usize)
            .copied()
            .unwrap_or(Color::Reset)
    }
    /// Resolve a drawn colour. Themes with their own background also replace
    /// the terminal's default colours.
    fn present(&self, color: Color, foreground: bool) -> Color {
        match color {
            Color::Indexed(index) if is_slot(color) => self
                .colors
                .get(usize::from(index - BASE))
                .copied()
                .unwrap_or(color),
            Color::Reset if self.color(Role::Background) != Color::Reset => {
                self.color(if foreground {
                    Role::Text
                } else {
                    Role::Background
                })
            }
            color => color,
        }
    }
}

/// The colours a theme chooses; tints and gradients derive from them.
struct Base {
    /// None keeps the terminal's own background.
    background: Option<u32>,
    text: u32,
    dim: u32,
    border: u32,
    accent: u32,
    on_accent: u32,
    green: u32,
    purple: u32,
    yellow: u32,
    red: u32,
    keyword: u32,
    string: u32,
    number: u32,
    comment: u32,
}
impl Base {
    fn theme(self, id: &'static str, name: &'static str, description: &'static str) -> Theme {
        // Tints over the terminal's own background assume a dark terminal.
        let surface = self.background.unwrap_or(0x0c0e12);
        let background = self.background.map_or(Color::Reset, rgb);
        let colors = palette([
            (Role::Background, background),
            (Role::Panel, background),
            (Role::Text, rgb(self.text)),
            (Role::Dim, rgb(self.dim)),
            (Role::Border, rgb(self.border)),
            (Role::Accent, rgb(self.accent)),
            (Role::OnAccent, rgb(self.on_accent)),
            (Role::Green, rgb(self.green)),
            (Role::Purple, rgb(self.purple)),
            (Role::Yellow, rgb(self.yellow)),
            (Role::Red, rgb(self.red)),
            (Role::AddBg, mix(surface, self.green, 0.16)),
            (Role::RemoveBg, mix(surface, self.red, 0.16)),
            // Selected sessions share this tint, so dim text must stay readable on it.
            (Role::UserMessage, mix(surface, self.accent, 0.16)),
            (Role::UserMessageText, rgb(self.text)),
            (Role::Keyword, rgb(self.keyword)),
            (Role::String, rgb(self.string)),
            (Role::Number, rgb(self.number)),
            (Role::Comment, rgb(self.comment)),
            (Role::FocusedEntry, mix(surface, self.accent, 0.15)),
            (Role::SelectedCode, mix(surface, self.accent, 0.22)),
            (Role::FocusedCode, mix(surface, self.text, 0.06)),
        ]);
        Theme {
            id,
            name,
            description,
            colors,
        }
    }
}

/// difu's original colours, exactly.
fn default() -> Theme {
    let colors = palette([
        (Role::Background, Color::Reset),
        (Role::Panel, Color::Reset),
        (Role::Text, rgb(0xdce1e8)),
        (Role::Dim, rgb(0x828c9c)),
        (Role::Border, rgb(0x606a7a)),
        (Role::Accent, rgb(0x00ff41)),
        (Role::OnAccent, rgb(0x0c0e12)),
        (Role::Green, rgb(0x72d8a3)),
        (Role::Purple, rgb(0xab7df8)),
        (Role::Yellow, rgb(0xe5c064)),
        (Role::Red, rgb(0xf78991)),
        (Role::AddBg, rgb(0x122b20)),
        (Role::RemoveBg, rgb(0x31191f)),
        (Role::UserMessage, rgb(0x003f10)),
        (Role::UserMessageText, Color::White),
        (Role::Keyword, rgb(0x00ff41)),
        (Role::String, rgb(0xddc28b)),
        (Role::Number, rgb(0xe0ab81)),
        (Role::Comment, rgb(0x828c9c)),
        (Role::FocusedEntry, rgb(0x102719)),
        (Role::SelectedCode, rgb(0x124022)),
        (Role::FocusedCode, rgb(0x12151b)),
    ]);
    Theme {
        id: "default",
        name: "Default",
        description: "difu's original colours on your terminal's background",
        colors,
    }
}

/// Default's palette with another accent, on the terminal's own background.
fn accent(accent: u32) -> Base {
    Base {
        background: None,
        text: 0xdce1e8,
        dim: 0x828c9c,
        border: 0x606a7a,
        accent,
        on_accent: 0x0c0e12,
        green: 0x72d8a3,
        purple: 0xab7df8,
        yellow: 0xe5c064,
        red: 0xf78991,
        keyword: accent,
        string: 0xddc28b,
        number: 0xe0ab81,
        comment: 0x828c9c,
    }
}

/// The terminal's own 16 colours, so difu follows its colour scheme. Tints use
/// fixed 256-colour shades chosen for dark or light terminals.
fn ansi(light: bool) -> Theme {
    let tint = |dark: u8, bright: u8| Color::Indexed(if light { bright } else { dark });
    let (id, name, description) = if light {
        (
            "ansi-light",
            "ANSI Light",
            "Your terminal's colours, for light terminals",
        )
    } else {
        (
            "ansi",
            "ANSI",
            "Your terminal's colours, for dark terminals",
        )
    };
    Theme {
        id,
        name,
        description,
        colors: palette([
            (Role::Background, Color::Reset),
            (Role::Panel, Color::Reset),
            (Role::Text, Color::Reset),
            (Role::Dim, Color::DarkGray),
            (Role::Border, Color::DarkGray),
            (Role::Accent, Color::Green),
            (
                Role::OnAccent,
                if light { Color::White } else { Color::Black },
            ),
            (Role::Green, Color::Green),
            (Role::Purple, Color::Magenta),
            (Role::Yellow, Color::Yellow),
            (Role::Red, Color::Red),
            (Role::AddBg, tint(22, 194)),
            (Role::RemoveBg, tint(52, 224)),
            (Role::UserMessage, tint(22, 194)),
            (Role::UserMessageText, Color::Reset),
            (Role::Keyword, Color::Magenta),
            (Role::String, Color::Yellow),
            (Role::Number, Color::Cyan),
            (Role::Comment, Color::DarkGray),
            (Role::FocusedEntry, tint(235, 254)),
            (Role::SelectedCode, tint(22, 194)),
            (Role::FocusedCode, tint(234, 255)),
        ]),
    }
}

static DEFAULT: LazyLock<Theme> = LazyLock::new(default);
static THEMES: LazyLock<Vec<Theme>> = LazyLock::new(|| {
    vec![
        default(),
        accent(0xffb000).theme(
            "amber",
            "Amber",
            "Amber accent on your terminal's background",
        ),
        accent(0x38bdf8).theme(
            "ocean",
            "Ocean",
            "Sky-blue accent on your terminal's background",
        ),
        ansi(false),
        ansi(true),
        Base {
            background: Some(0x282a36),
            text: 0xf8f8f2,
            dim: 0x8692b7,
            border: 0x6a7390,
            accent: 0xbd93f9,
            on_accent: 0x282a36,
            green: 0x50fa7b,
            purple: 0xff79c6,
            yellow: 0xf1fa8c,
            red: 0xff5555,
            keyword: 0xff79c6,
            string: 0xf1fa8c,
            number: 0xbd93f9,
            comment: 0x8591b8,
        }
        .theme(
            "dracula",
            "Dracula",
            "Purple and pink on a dark slate background",
        ),
        Base {
            background: Some(0x2e3440),
            text: 0xd8dee9,
            dim: 0x919db2,
            border: 0x737e92,
            accent: 0x88c0d0,
            on_accent: 0x2e3440,
            green: 0xa3be8c,
            purple: 0xb690af,
            yellow: 0xebcb8b,
            red: 0xd08a91,
            keyword: 0x81a1c1,
            string: 0xa3be8c,
            number: 0xb690af,
            comment: 0x939db2,
        }
        .theme("nord", "Nord", "Cool arctic blues"),
        Base {
            background: Some(0x282828),
            text: 0xebdbb2,
            dim: 0xa89984,
            border: 0x7a6f63,
            accent: 0xfabd2f,
            on_accent: 0x282828,
            green: 0xb8bb26,
            purple: 0xd3869b,
            yellow: 0xfabd2f,
            red: 0xfb5440,
            keyword: 0xfb5440,
            string: 0xb8bb26,
            number: 0xd3869b,
            comment: 0x9b8d7f,
        }
        .theme("gruvbox-dark", "Gruvbox Dark", "Warm retro contrast"),
        Base {
            background: Some(0x1e1e2e),
            text: 0xcdd6f4,
            dim: 0x9399b2,
            border: 0x66697f,
            accent: 0xcba6f7,
            on_accent: 0x1e1e2e,
            green: 0xa6e3a1,
            purple: 0xf5c2e7,
            yellow: 0xf9e2af,
            red: 0xf38ba8,
            keyword: 0xcba6f7,
            string: 0xa6e3a1,
            number: 0xfab387,
            comment: 0x82869e,
        }
        .theme(
            "catppuccin-mocha",
            "Catppuccin Mocha",
            "Soft pastels on a deep background",
        ),
        Base {
            background: Some(0x1a1b26),
            text: 0xc0caf5,
            dim: 0x7b82aa,
            border: 0x5e6588,
            accent: 0x7aa2f7,
            on_accent: 0x1a1b26,
            green: 0x9ece6a,
            purple: 0xbb9af7,
            yellow: 0xe0af68,
            red: 0xf7768e,
            keyword: 0xbb9af7,
            string: 0x9ece6a,
            number: 0xff9e64,
            comment: 0x7b82a3,
        }
        .theme(
            "tokyo-night",
            "Tokyo Night",
            "Neon blues and purples at night",
        ),
        Base {
            background: Some(0x282c34),
            text: 0xb7bcc8,
            dim: 0x8e939c,
            border: 0x6f7581,
            accent: 0x61afef,
            on_accent: 0x282c34,
            green: 0x98c379,
            purple: 0xc678dd,
            yellow: 0xe5c07b,
            red: 0xe1727b,
            keyword: 0xc678dd,
            string: 0x98c379,
            number: 0xd19a66,
            comment: 0x8e939c,
        }
        .theme("one-dark", "One Dark", "Atom's balanced dark palette"),
        Base {
            background: Some(0x272822),
            text: 0xf8f8f2,
            dim: 0x90908a,
            border: 0x75715e,
            accent: 0xa6e22e,
            on_accent: 0x272822,
            green: 0xa6e22e,
            purple: 0xae81ff,
            yellow: 0xe6db74,
            red: 0xfa4989,
            keyword: 0xfa4989,
            string: 0xe6db74,
            number: 0xae81ff,
            comment: 0x939081,
        }
        .theme("monokai", "Monokai", "Vivid classic on warm charcoal"),
        Base {
            background: Some(0x002b36),
            text: 0xa9b4b4,
            dim: 0x7e9097,
            border: 0x5d737a,
            accent: 0x3794d6,
            on_accent: 0x002b36,
            green: 0x859900,
            purple: 0x8488cd,
            yellow: 0xb58900,
            red: 0xe56765,
            keyword: 0x859900,
            string: 0x2aa198,
            number: 0xdd629e,
            comment: 0x809196,
        }
        .theme(
            "solarized-dark",
            "Solarized Dark",
            "Solarized's palette on deep teal",
        ),
        Base {
            background: Some(0x191724),
            text: 0xe0def4,
            dim: 0x908caa,
            border: 0x66637c,
            accent: 0xebbcba,
            on_accent: 0x191724,
            green: 0x9ccfd8,
            purple: 0xc4a7e7,
            yellow: 0xf6c177,
            red: 0xeb6f92,
            keyword: 0x528aa1,
            string: 0xf6c177,
            number: 0xebbcba,
            comment: 0x827f97,
        }
        .theme("rose-pine", "Rosé Pine", "Muted rose and pine at dusk"),
        Base {
            background: Some(0x1f1f28),
            text: 0xdcd7ba,
            dim: 0x898881,
            border: 0x696975,
            accent: 0x7e9cd8,
            on_accent: 0x1f1f28,
            green: 0x98bb6c,
            purple: 0x957fb8,
            yellow: 0xe6c384,
            red: 0xe46876,
            keyword: 0x957fb8,
            string: 0x98bb6c,
            number: 0xd27e99,
            comment: 0x898881,
        }
        .theme("kanagawa", "Kanagawa", "Ink and wave colours from Hokusai"),
        Base {
            background: Some(0x2d353b),
            text: 0xd7cbb1,
            dim: 0x949f97,
            border: 0x74807a,
            accent: 0xa7c080,
            on_accent: 0x2d353b,
            green: 0xa7c080,
            purple: 0xd699b6,
            yellow: 0xdbbc7f,
            red: 0xe67e80,
            keyword: 0xe67e80,
            string: 0xdbbc7f,
            number: 0xd699b6,
            comment: 0x949f97,
        }
        .theme(
            "everforest-dark",
            "Everforest Dark",
            "Soft greens of a forest",
        ),
        Base {
            background: Some(0xfdf6e3),
            text: 0x45565b,
            dim: 0x647477,
            border: 0x889089,
            accent: 0x2075b0,
            on_accent: 0xfdf6e3,
            green: 0x687700,
            purple: 0x666ab8,
            yellow: 0x8d6b00,
            red: 0xd3302d,
            keyword: 0x687700,
            string: 0x217e77,
            number: 0xc6337a,
            comment: 0x677171,
        }
        .theme(
            "solarized-light",
            "Solarized Light",
            "Solarized's warm light palette on cream",
        ),
        Base {
            background: Some(0xffffff),
            text: 0x1f2328,
            dim: 0x656d76,
            border: 0x8e959e,
            accent: 0x0969da,
            on_accent: 0xffffff,
            green: 0x1a7f37,
            purple: 0x8250df,
            yellow: 0x9a6700,
            red: 0xcf222e,
            keyword: 0xcf222e,
            string: 0x0a3069,
            number: 0x0550ae,
            comment: 0x6e7781,
        }
        .theme("github-light", "GitHub Light", "Crisp light theme"),
        Base {
            background: Some(0xeff1f5),
            text: 0x4c4f69,
            dim: 0x6a6d82,
            border: 0x868a9c,
            accent: 0x8839ef,
            on_accent: 0xeff1f5,
            green: 0x327d22,
            purple: 0x9f508a,
            yellow: 0x986114,
            red: 0xd20f39,
            keyword: 0x8839ef,
            string: 0x327d22,
            number: 0xbc4a08,
            comment: 0x6a6d7a,
        }
        .theme(
            "catppuccin-latte",
            "Catppuccin Latte",
            "Catppuccin's light flavour",
        ),
        Base {
            background: Some(0xfbf1c7),
            text: 0x3c3836,
            dim: 0x776b60,
            border: 0x978978,
            accent: 0x076678,
            on_accent: 0xfbf1c7,
            green: 0x746f0d,
            purple: 0x8f3f71,
            yellow: 0x946110,
            red: 0x9d0006,
            keyword: 0x9d0006,
            string: 0x746f0d,
            number: 0x8f3f71,
            comment: 0x786b5f,
        }
        .theme(
            "gruvbox-light",
            "Gruvbox Light",
            "Gruvbox's warm retro light",
        ),
    ]
});

pub fn themes() -> &'static [Theme] {
    &THEMES
}

/// Index of the theme with `id`; unknown or missing ids use Default.
pub fn index(id: Option<&str>) -> usize {
    id.and_then(|id| themes().iter().position(|theme| theme.id == id))
        .unwrap_or(0)
}

// The drawing thread owns the theme. Parallel tests each keep their own.
#[cfg(not(test))]
static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
thread_local! {
    static ACTIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn active_index() -> usize {
    #[cfg(not(test))]
    let index = ACTIVE.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    let index = ACTIVE.with(std::cell::Cell::get);
    index.min(themes().len().saturating_sub(1))
}

pub fn select(index: usize) {
    let index = index.min(themes().len().saturating_sub(1));
    #[cfg(not(test))]
    ACTIVE.store(index, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    ACTIVE.with(|active| active.set(index));
}

pub fn active() -> &'static Theme {
    themes().get(active_index()).unwrap_or(&DEFAULT)
}

/// The active theme's colour for a role slot; other colours pass through.
pub fn resolve(color: Color) -> Color {
    active().present(color, true)
}

/// Resolve every drawn colour before the frame reaches the terminal.
pub fn present(buffer: &mut Buffer) {
    let theme = active();
    for cell in &mut buffer.content {
        cell.fg = theme.present(cell.fg, true);
        cell.bg = theme.present(cell.bg, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn slots() -> impl Iterator<Item = Color> {
        (0..ROLES as u8).map(|offset| Color::Indexed(BASE + offset))
    }

    #[test]
    fn every_theme_resolves_every_role_and_ids_are_unique() {
        let mut ids = std::collections::BTreeSet::new();
        for theme in themes() {
            assert!(ids.insert(theme.id), "duplicate id {}", theme.id);
            for (role, color) in slots().map(|slot| (slot, theme.present(slot, true))) {
                assert!(!is_slot(color), "{} leaves {role:?} unresolved", theme.id);
            }
            for role in [Role::Accent, Role::Keyword, Role::UserMessage] {
                assert_ne!(theme.color(role), Color::Reset, "{} {role:?}", theme.id);
            }
            // Text follows the terminal's colour only with its background.
            if theme.color(Role::Text) == Color::Reset {
                assert_eq!(theme.color(Role::Background), Color::Reset, "{}", theme.id);
            }
        }
        assert_eq!(themes().first().map(|t| t.id), Some("default"));
        assert_eq!(
            themes().get(index(Some("nord"))).map(|t| t.id),
            Some("nord")
        );
        assert_eq!(index(Some("removed-theme")), 0);
        assert_eq!(index(None), 0);
    }

    /// None for terminal palette colours, which the terminal defines.
    fn contrast(a: Color, b: Color) -> Option<f64> {
        let luminance = |color: Color| {
            let (r, g, b) = match color {
                Color::Rgb(r, g, b) => (r, g, b),
                Color::White => (255, 255, 255),
                _ => return None,
            };
            let linear = |channel: u8| {
                let c = f64::from(channel) / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            Some(0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b))
        };
        let (a, b) = (luminance(a)?, luminance(b)?);
        Some((a.max(b) + 0.05) / (a.min(b) + 0.05))
    }

    #[test]
    fn every_theme_keeps_text_readable() {
        for theme in themes() {
            // Terminal-background themes are designed for dark terminals.
            let background = match theme.color(Role::Background) {
                Color::Reset => rgb(0x0c0e12),
                color => color,
            };
            for (role, minimum) in [
                (Role::Text, 7.0),
                (Role::Dim, 4.5),
                (Role::Accent, 4.5),
                (Role::Green, 4.5),
                (Role::Purple, 4.5),
                (Role::Yellow, 4.5),
                (Role::Red, 4.5),
                (Role::Keyword, 4.5),
                (Role::String, 4.5),
                (Role::Number, 4.5),
                (Role::Comment, 4.5),
                // Inactive pane titles use the border colour.
                (Role::Border, 3.0),
            ] {
                if let Some(ratio) = contrast(theme.color(role), background) {
                    assert!(ratio >= minimum, "{} {role:?} is {ratio:.2}:1", theme.id);
                }
            }
            for (text, surface, minimum) in [
                (Role::UserMessageText, Role::UserMessage, 4.5),
                (Role::OnAccent, Role::Accent, 4.5),
                // A selected session's details sit on the user message tint.
                (Role::Dim, Role::UserMessage, 3.0),
            ] {
                if let Some(ratio) = contrast(theme.color(text), theme.color(surface)) {
                    assert!(ratio >= minimum, "{} {text:?} is {ratio:.2}:1", theme.id);
                }
            }
        }
    }

    #[test]
    fn presenting_resolves_slots_and_keeps_other_colours() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        let cells = [
            (slot(Role::Accent), slot(Role::UserMessage)),
            (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6)),
            (Color::Reset, Color::Reset),
            (Color::Indexed(12), Color::Indexed(BASE - 1)),
        ];
        let paint = |buffer: &mut Buffer| {
            for (cell, (fg, bg)) in buffer.content.iter_mut().zip(cells) {
                cell.fg = fg;
                cell.bg = bg;
            }
        };
        let colours = |buffer: &Buffer| -> Vec<(Color, Color)> {
            buffer
                .content
                .iter()
                .map(|cell| (cell.fg, cell.bg))
                .collect()
        };

        select(index(Some("default")));
        paint(&mut buffer);
        present(&mut buffer);
        assert_eq!(
            colours(&buffer),
            [
                (Color::Rgb(0, 255, 65), Color::Rgb(0, 63, 16)),
                (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6)),
                (Color::Reset, Color::Reset),
                (Color::Indexed(12), Color::Indexed(BASE - 1)),
            ]
        );

        // A theme with its own background also replaces the terminal defaults.
        select(index(Some("github-light")));
        paint(&mut buffer);
        present(&mut buffer);
        let light = active();
        assert_eq!(
            colours(&buffer).get(2).copied(),
            Some((light.color(Role::Text), light.color(Role::Background)))
        );
        assert_eq!(
            colours(&buffer).first().copied(),
            Some((light.color(Role::Accent), light.color(Role::UserMessage)))
        );
        assert_eq!(resolve(slot(Role::Dim)), light.color(Role::Dim));
        select(0);
    }
}
