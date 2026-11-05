//! `/restart`: reloading the terminal app in place, usually to load an upgrade.
//! The new process picks up where this one stopped: the session on show and
//! every unsent draft, saved here because a reload replaces the process.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    selected: Option<String>,
    /// Draft text by session.
    drafts: BTreeMap<String, String>,
}

fn path(storage: &Storage) -> anyhow::Result<std::path::PathBuf> {
    Ok(super::super::server::home(storage)?.join("restart.json"))
}

impl Ui {
    pub fn save_for_restart(&self) -> anyhow::Result<()> {
        let saved = Saved {
            selected: self.selected.clone(),
            drafts: self
                .positions
                .iter()
                .map(|(id, position)| (id.clone(), position.draft.text()))
                .filter(|(_, draft)| !draft.is_empty())
                .collect(),
        };
        crate::storage::atomic_json(&path(&self.storage)?, &saved)
    }

    /// Picks up where `/restart` left off. The saved state is used once.
    pub fn restore_after_restart(&mut self) {
        let Ok(path) = path(&self.storage) else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let Ok(saved) = serde_json::from_slice::<Saved>(&bytes) else {
            return;
        };
        for (id, draft) in saved.drafts {
            self.positions.entry(id).or_default().draft = Editor::from(draft.as_str());
        }
        if let Some(id) = saved.selected {
            self.select(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn ui(dir: &std::path::Path) -> Ui {
        let storage = Storage {
            config: dir.join("config.json"),
            cache: dir.join("cache"),
        };
        Ui::new(storage, &Config::default())
    }

    #[test]
    fn a_restart_keeps_the_session_on_show_and_unsent_drafts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut before = ui(dir.path());
        before.selected = Some("two".into());
        before.positions.entry("one".into()).or_default().draft = Editor::from("half a thought");
        before.positions.entry("two".into()).or_default();
        before.save_for_restart()?;

        let mut after = ui(dir.path());
        after.restore_after_restart();
        assert_eq!(after.selected.as_deref(), Some("two"));
        let draft = |ui: &Ui, id: &str| ui.positions.get(id).map(|p| p.draft.text());
        assert_eq!(draft(&after, "one").as_deref(), Some("half a thought"));
        assert_eq!(draft(&after, "two").as_deref(), Some(""));

        // It applies once: an ordinary start later begins fresh.
        let mut later = ui(dir.path());
        later.restore_after_restart();
        assert!(later.selected.is_none() && later.positions.is_empty());
        Ok(())
    }

    #[test]
    fn a_normal_start_has_nothing_to_restore() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = ui(dir.path());
        ui.restore_after_restart();
        assert!(ui.selected.is_none());
        Ok(())
    }

    #[test]
    fn the_restart_command_asks_the_shell_to_reload() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = ui(dir.path());
        ui.run_palette_command("/restart", "");
        assert!(ui.restart_requested);
        Ok(())
    }
}
