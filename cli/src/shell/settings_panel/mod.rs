//! Native draft editor with cancellable discovery and separately dispatched explicit actions.
use crate::{
    settings::*,
    storage::{Config, Storage},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{path::PathBuf, time::Instant};
mod connections;
mod draw;
mod footer;
mod integration;
mod key;
mod shortcuts;
pub(crate) use integration::command_category;
#[cfg(test)]
pub(crate) use integration::is_open_shortcut;
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct Outcome {
    pub close: bool,
    pub saved: Option<Config>,
    pub native_enable: bool,
    /// Explicit user action; Shell or later integrations decide how to apply it.
    pub action: Option<SettingId>,
    pub preview_test: Option<Preview>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Search,
    Sections,
    Settings,
}

pub(crate) struct Panel {
    connections: connections::State,
    pub draft: SettingsDraft,
    committed: Config,
    capture: Option<crate::shortcuts::ActionId>,
    testing: bool,
    question_test: Option<crate::agents::ui::question_preview::QuestionPreview>,
    last_shortcut: Option<crate::shortcuts::ActionId>,
    pub category: Category,
    pub query: String,
    pub modified_only: bool,
    pub selected: usize,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub notification_status: Option<crate::agents::notifications::Capabilities>,
    pub(super) notification_test: Option<(String, bool, Instant)>,
    pub confirm_discard: bool,
    pub export_path: Option<PathBuf>,
    pub replay: Instant,
    preview: crate::agents::ui::settings_preview::SettingsPreview,
    visual_config: Option<Config>,
    preview_selection: Option<SettingId>,
    preview_motion: Option<(MotionPreferences, crate::storage::Tweaks)>,
    pub(super) details_expanded: bool,
    pub(super) details_scroll: u16,
    focus: Focus,
    edit: Option<String>,
    /// Full action registry metadata can be supplied by the keyboard integration.
    extra_specs: Vec<SettingSpec>,
    /// Provider capability integration can supply validated dynamic values here.
    dynamic_choices: std::collections::BTreeMap<SettingId, Vec<String>>,
}
impl Panel {
    pub(super) fn expire_notification_test(&mut self, now: Instant) -> bool {
        if self
            .notification_test
            .as_ref()
            .is_some_and(|(_, _, started)| {
                now.saturating_duration_since(*started) >= crate::agents::ui::toast::LIFETIME
            })
        {
            self.notification_test = None;
            return true;
        }
        false
    }
    pub fn new(storage: &Storage, category: Option<Category>) -> anyhow::Result<Self> {
        let mut panel = Self {
            connections: Default::default(),
            draft: storage.settings_draft()?,
            committed: storage.load_config()?,
            capture: None,
            testing: false,
            question_test: None,
            last_shortcut: None,
            category: category.unwrap_or(Category::Appearance),
            query: String::new(),
            modified_only: false,
            selected: 0,
            error: None,
            notice: None,
            notification_status: None,
            notification_test: None,
            confirm_discard: false,
            details_expanded: false,
            details_scroll: 0,
            export_path: None,
            replay: Instant::now(),
            focus: Focus::Search,
            preview: crate::agents::ui::settings_preview::SettingsPreview::new(Instant::now()),
            visual_config: None,
            preview_selection: None,
            preview_motion: None,
            edit: None,
            extra_specs: Vec::new(),
            dynamic_choices: Default::default(),
        };
        panel.set_shortcuts(crate::shortcuts::settings());
        Ok(panel)
    }
    pub fn rows(&self) -> Vec<SettingSpec> {
        let mut rows = registry();
        for spec in &self.extra_specs {
            rows.retain(|row| row.id != spec.id);
            rows.push(spec.clone());
        }
        let query = self.query.trim().to_lowercase();
        let rank = |spec: &SettingSpec| -> Option<u8> {
            let label = spec.label.to_lowercase();
            if query.is_empty()
                || label == query
                || spec.aliases.iter().any(|a| a.to_lowercase() == query)
            {
                Some(0)
            } else if label.contains(&query)
                || spec
                    .aliases
                    .iter()
                    .any(|a| a.to_lowercase().contains(&query))
            {
                Some(1)
            } else if spec.description.to_lowercase().contains(&query) {
                Some(2)
            } else {
                None
            }
        };
        rows.retain(|spec| rank(spec).is_some());
        rows.sort_by_key(|spec| rank(spec));
        rows.retain(|spec| {
            (self.modified_only || !self.query.is_empty() || spec.category == self.category)
                && (!self.modified_only || self.draft.edited().contains(&spec.id))
        });
        rows
    }
    /// Integration hook: authoritative shortcuts, preserving current selection and draft.
    pub fn set_shortcuts(&mut self, specs: Vec<SettingSpec>) {
        self.extra_specs = specs;
    }
    /// Integration hook: choices obtained without starting any login or connection flow.
    pub fn set_dynamic_choices(&mut self, id: SettingId, choices: Vec<String>) {
        self.dynamic_choices.insert(id, choices);
    }
    pub fn save(&mut self, storage: &Storage) -> Option<Config> {
        if self.draft.edited().iter().any(|id| {
            matches!(
                id,
                SettingId::Provider
                    | SettingId::DefaultModel
                    | SettingId::DefaultEffort
                    | SettingId::StartingMode
                    | SettingId::DefaultRepository
            )
        }) && let Some(error) = self.invalid_defaults()
        {
            self.error = Some(error);
            return None;
        }
        match storage.save_settings_draft(&mut self.draft) {
            Ok(config) => {
                self.committed = config.clone();
                self.error = None;
                self.notice = Some("Preferences saved".into());
                Some(config)
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                None
            }
        }
    }
    pub fn discard(&mut self, storage: &Storage) -> Option<Config> {
        match storage.refresh_settings() {
            Ok(config) => {
                self.committed = config.clone();
                self.draft.discard(config.clone());
                self.error = None;
                self.notice = Some("Draft discarded; latest saved preferences loaded".into());
                self.confirm_discard = false;
                Some(config)
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                None
            }
        }
    }
    pub fn reset_section(&mut self) {
        if self.category == Category::Keyboard
            && let Err(error) = self.draft.reset_shortcuts()
        {
            self.error = Some(format!("{error:#}"));
        }
        for spec in registry()
            .into_iter()
            .chain(self.extra_specs.clone())
            .filter(|s| s.category == self.category && s.is_stored_field())
        {
            if let Err(error) = self.draft.reset(&spec.id) {
                self.error = Some(format!("{error:#}"));
            }
        }
        self.notice = Some(format!("{} reset in draft", self.category.label()));
    }
    pub fn export(&mut self, storage: &Storage) -> anyhow::Result<()> {
        use std::io::Write;
        let dir = storage.cache.join("preference-exports");
        std::fs::create_dir_all(&dir)?;
        let mut file = tempfile::Builder::new()
            .prefix("difu-preferences-")
            .suffix(".json")
            .tempfile_in(&dir)?;
        file.write_all(export_preferences(self.draft.config())?.as_bytes())?;
        let (_, path) = file.keep()?;
        self.notice = Some(format!("Exported draft preferences to {}", path.display()));
        self.export_path = Some(path);
        Ok(())
    }
    pub fn paste(&mut self, text: &str) {
        if self.capture.is_some() || self.testing {
            return;
        }
        let text = text.replace(['\n', '\r', '\t'], " ");
        if let Some(edit) = &mut self.edit {
            edit.push_str(&text);
        } else if self.focus == Focus::Search {
            self.query.push_str(&text);
            self.selected = 0;
        }
    }
    fn focus_next(&mut self, backwards: bool) {
        self.focus = match (self.focus, backwards) {
            (Focus::Search, false) | (Focus::Settings, true) => Focus::Sections,
            (Focus::Sections, false) | (Focus::Search, true) => Focus::Settings,
            (Focus::Settings, false) | (Focus::Sections, true) => Focus::Search,
        };
        self.details_expanded = false;
    }
    fn change_category(&mut self, delta: isize) {
        let index = Category::ALL
            .iter()
            .position(|c| *c == self.category)
            .unwrap_or(0);
        self.category = Category::ALL
            .get((index as isize + delta).rem_euclid(10) as usize)
            .copied()
            .unwrap_or(Category::Appearance);
        self.selected = 0;
        self.details_scroll = 0;
        self.query.clear();
        if self.focus == Focus::Search {
            self.focus = Focus::Settings;
        }
        self.edit = None;
    }
}

impl Panel {
    /// Overlay only locally edited visual fields; keep the save/conflict baseline untouched.
    pub fn refresh_preview(&mut self, committed: &Config) {
        self.committed = committed.clone();
        let mut visual = committed.clone();
        for id in self.draft.edited() {
            if spec(id).is_some_and(|spec| {
                matches!(
                    spec.preview,
                    Preview::Appearance
                        | Preview::Animation
                        | Preview::Conversation
                        | Preview::Layout
                        | Preview::Diff
                        | Preview::Input
                )
            }) && *id != SettingId::SendKey
                && let Err(error) = set(&mut visual, id, self.draft.value(id))
            {
                self.error = Some(format!("Could not preview Settings: {error:#}"));
            }
        }
        self.visual_config = Some(visual);
    }
    pub fn preview_config(&self) -> &Config {
        self.visual_config.as_ref().unwrap_or(self.draft.config())
    }
    pub fn animating(&self) -> bool {
        let rows = self.rows();
        let Some(spec) = rows.get(self.selected) else {
            return false;
        };
        let config = self.preview_config();
        let motion = &config.preferences.motion;
        let enabled = match spec.id {
            SettingId::SidebarEffect => motion.sidebar() && config.tweaks.agent_reply_cps > 0,
            SettingId::ScreenTransitionEffect => {
                motion.screen() && config.tweaks.agent_reply_cps > 0
            }
            SettingId::InputClearDuration => motion.sent() && config.tweaks.input_clear_ms > 0,
            _ => {
                (motion.reply() && config.tweaks.agent_reply_cps > 0)
                    || (motion.sent() && config.tweaks.sent_message_ms > 0)
            }
        };
        spec.preview == Preview::Animation && enabled && self.preview.animating()
    }
}
