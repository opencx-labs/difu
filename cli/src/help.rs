use crate::editor::Editor;

#[derive(Default)]
pub struct State {
    pub query: Editor,
    pub scroll: usize,
    pub viewport: usize,
    pub rows: usize,
}

pub fn entries(query: &str, config: &crate::storage::Config) -> Vec<(String, String)> {
    use crate::shortcuts::{Bindings, all};
    let bindings = Bindings::new(config);
    let mut entries = all()
        .iter()
        .filter(|action| {
            action
                .contexts
                .iter()
                .any(|context| context.0.starts_with("Review") || context.0.starts_with("Workflow"))
        })
        .map(|action| (bindings.hint(action.id), action.label.to_owned()))
        .collect::<Vec<_>>();
    entries.extend(
        [
            ("Cmd/Ctrl+click", "JS/TS symbol definition; Esc closes"),
            ("Ctrl+U", "Clear focused filter/search"),
            ("Cmd+Z", "Undo text edit"),
            ("Alt+Backspace", "Delete previous input word"),
            ("Cmd+Backspace / Ctrl+U", "Delete input line"),
            ("Cmd+Left / Right · Ctrl+A/E", "Input line start/end"),
            ("Shift+Alt+Left / Right", "Extend input selection by a word"),
            ("Mouse", "Click items and links; wheel scrolls"),
            ("Esc", "Close or cancel active dialog"),
        ]
        .into_iter()
        .map(|(key, description)| (key.into(), description.into())),
    );
    let query = query.to_lowercase();
    entries
        .into_iter()
        .filter(|(key, description)| {
            format!("{key} {description}")
                .to_lowercase()
                .contains(&query)
        })
        .collect()
}
