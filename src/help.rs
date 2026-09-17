use crate::editor::Editor;

#[derive(Default)]
pub struct State {
    pub query: Editor,
    pub scroll: usize,
    pub viewport: usize,
    pub rows: usize,
}

pub fn entries(query: &str) -> Vec<(&'static str, &'static str)> {
    let query = query.to_lowercase();
    [
        ("↑ / ↓", "Navigate repositories, PRs, files, or code"),
        ("Alt+↑ / ↓", "Previous / next guide chapter"),
        ("Tab / Shift+Tab", "Switch navigation / content focus"),
        ("Cmd+↑ / ↓", "Scroll content by ten lines"),
        (
            "Enter",
            "Open repository or PR / comment / toggle completion",
        ),
        (
            "c / Cmd+C",
            "Copy focused source line, selected lines, or file path",
        ),
        ("Click symbol", "JS/TS function definition; Esc closes"),
        ("/", "PR actions / worktree management / default models"),
        ("Page Up / Down", "Scroll a page; Space scrolls down"),
        ("Home / End", "Jump to start / end"),
        (
            "← / →",
            "Select old / new diff side; scroll the focused Files tree",
        ),
        (
            "Alt+← / →",
            "Scroll unwrapped code or the Files tree horizontally",
        ),
        ("Shift+↑ / ↓", "Select code lines within one file and side"),
        ("w", "Toggle diff wrapping (saved)"),
        ("1 / 2", "Home: My PRs / Repositories"),
        ("1 / 2 / 3", "Inside PR: Overview / Guide / Diff"),
        (
            "[ / ] / s",
            "Previous / next / next PR state: Open / Merged / Closed / All",
        ),
        ("f", "Focus sidebar filter; Enter/Esc returns to the list"),
        ("Ctrl+U", "Clear the focused filter or search input"),
        ("*", "Pin/unpin the selected repository locally"),
        ("?", "Search keyboard and mouse shortcuts"),
        ("m", "Choose model and reasoning"),
        ("r", "Refresh / load the new PR revision"),
        ("g", "Generate guide again / retry"),
        ("l", "Choose another local clone path"),
        (
            "x",
            "Cancel generation, snapshot loading, or conflict resolution",
        ),
        ("Ctrl+R", "Refresh mentions in the editor"),
        ("Ctrl+B", "Toggle side-by-side / unified diff"),
        ("Ctrl+O", "Open PR on GitHub"),
        ("Mouse", "Click items and links; wheel to scroll"),
        ("Esc", "Close dialog / back / quit"),
        ("Ctrl+C", "Quit and clean up running work"),
    ]
    .into_iter()
    .filter(|(key, description)| {
        format!("{key} {description}")
            .to_lowercase()
            .contains(&query)
    })
    .collect()
}
