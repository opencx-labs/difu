//! Top-level navigation: agents own the landing page, reviews retain their UI.
use crate::{
    agents::ui::Ui,
    app::App,
    model::PrKey,
    storage::{Config, Storage},
    ui::{ACCENT, BG, DIM},
};
use crossterm::{
    clipboard::CopyToClipboard,
    event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};
use std::io::Write;
use unicode_width::UnicodeWidthStr;

const TAB_MARGIN: u16 = 1;
mod palette;
mod preferences;
pub(crate) mod settings_panel;
#[cfg(test)]
mod shortcut_tests;

pub struct Shell {
    pub reviews: App,
    pub agents: Ui,
    pub agents_active: bool,
    reviews_started: bool,
    palette: Option<palette::Palette>,
    pub(crate) settings: Option<settings_panel::Panel>,
    settings_action_open: bool,
    settings_refreshed: Option<std::time::Instant>,
    settings_fingerprint: Option<String>,
    settings_refresh_error: Option<String>,
    pub(crate) settings_pending_action: Option<crate::settings::SettingId>,
    pub(crate) settings_preview_requested: Option<crate::settings::Preview>,
    /// When the last tab switch started flipping the screen in.
    switched: Option<std::time::Instant>,
}
impl Shell {
    pub(crate) fn reviews_on_launch(config: &Config, explicit_review: bool) -> bool {
        explicit_review
            || match config.preferences.layout.startup_tab {
                crate::settings::StartupTab::LastUsed => !config.last_tab_agents,
                crate::settings::StartupTab::Agents => false,
                crate::settings::StartupTab::Reviews => true,
            }
    }
    pub fn new(storage: Storage, config: Config, pr: Option<PrKey>) -> Self {
        let mut reviews = App::new(storage.clone(), config.clone());
        let reviews_started = Self::reviews_on_launch(&config, pr.is_some());
        if reviews_started {
            reviews.start(pr);
        }
        Self {
            reviews,
            agents: Ui::new(storage, &config),
            agents_active: !reviews_started,
            reviews_started,
            palette: None,
            settings: None,
            settings_action_open: false,
            settings_refreshed: None,
            settings_fingerprint: serde_json::to_string(&config).ok(),
            settings_refresh_error: None,
            settings_pending_action: None,
            settings_preview_requested: None,
            switched: None,
        }
    }
    pub fn local(storage: Storage, config: Config, path: std::path::PathBuf) -> Self {
        let mut initial = config.clone();
        initial.last_tab_agents = true;
        initial.preferences.layout.startup_tab = crate::settings::StartupTab::Agents;
        let mut shell = Self::new(storage, initial, None);
        shell.apply_committed_settings(config);
        shell.agents_active = false;
        shell.reviews_started = true;
        shell.reviews.open_local(path, false);
        shell
    }
    pub(crate) fn open_settings(&mut self, category: Option<crate::settings::Category>) {
        self.agents.cancel_voice();
        self.palette = None;
        self.settings_action_open = false;
        self.settings_pending_action = None;
        self.settings_preview_requested = None;
        self.switched = None;
        match settings_panel::Panel::new(&self.reviews.storage, category) {
            Ok(mut panel) => {
                panel.refresh_preview(&self.reviews.config);
                self.settings = Some(panel);
            }
            Err(error) => {
                let message = format!("Could not open Settings: {error:#}");
                self.reviews.notice = crate::app::Notice::error(message.clone());
                self.agents.notice = Some((message, true));
            }
        }
    }
    fn take_settings_request(&mut self) {
        if let Some(category) = self.agents.settings_requested.take() {
            self.open_settings(Some(category));
        }
    }
    fn switch(&mut self, agents: bool) {
        self.agents.cancel_voice();
        if self.agents_active != agents {
            self.switched = self.screen_motion().then(std::time::Instant::now);
        }
        self.agents_active = agents;
        self.reviews.config.last_tab_agents = agents;
        let saved = self
            .reviews
            .storage
            .update_config(|config| config.last_tab_agents = agents);
        if let Err(error) = saved {
            self.agents.notice = Some((format!("Could not save the active tab: {error:#}"), true));
        }
        self.reviews.hover = Default::default();
        if !agents && !self.reviews_started {
            self.reviews.start(None);
            self.reviews_started = true;
        }
    }
    pub fn redraw_interval(&self) -> Option<std::time::Duration> {
        if self
            .settings
            .as_ref()
            .is_some_and(|panel| panel.animating())
            || (self.screen_motion() && self.switched.is_some())
        {
            return Some(crate::repaint::FRAME_INTERVAL);
        }
        if self.agents_active {
            self.agents.redraw_interval()
        } else {
            self.reviews.redraw_interval()
        }
    }
    fn reconcile_settings_action(&mut self) {
        self.settings_action_open &= self.settings.is_some() && self.agents.modal.is_some();
    }
    pub fn tick(&mut self) {
        self.tick_focused(true);
    }
    pub fn tick_focused(&mut self, focused: bool) {
        self.refresh_settings();
        let selected = self.palette_selection();
        self.agents
            .tick(focused && (self.agents_active || self.palette.is_some()));
        self.reconcile_settings_action();
        self.tick_palette();
        self.restore_palette_selection(selected);
        self.take_settings_request();
        self.reviews.config.agent_defaults = self.agents.defaults.clone();
        // Layout is a local visual overlay while Settings is open. Never promote
        // that overlay into the committed snapshot used for refresh/conflicts.
        if self.settings.is_none() {
            self.reviews.config.agent_list_visible = self.agents.list_visible;
            self.reviews.config.agent_changes_visible = self.agents.changes_visible;
            self.reviews.config.agent_panel_right = self.agents.panel_right();
        }
        self.reviews.config.pinned_sessions = self.agents.pinned_sessions.clone();
        self.reviews.config.session_order = self.agents.session_order.clone();
        self.reviews.config.voice_enabled = self.agents.voice_enabled();
        if self.reviews_started {
            self.reviews.tick_visible(focused && !self.agents_active);
        }
        if self.agents.review_requested {
            self.agents.review_requested = false;
            self.switch(false);
        }
    }
    fn tab_labels(&self) -> [String; 2] {
        let bindings = crate::shortcuts::Bindings::new(&self.reviews.config);
        [
            format!(
                " Agents ({}) ",
                bindings.hint(crate::shortcuts::ids::SHELL_AGENTS)
            ),
            format!(
                " Reviews ({}) ",
                bindings.hint(crate::shortcuts::ids::SHELL_REVIEWS)
            ),
        ]
    }
    pub fn key(&mut self, key: KeyEvent) {
        self.reconcile_settings_action();
        if key.kind == KeyEventKind::Release {
            if self.agents_active {
                self.agents.key(key);
            } else {
                self.reviews.key_event(key);
            }
            return;
        }
        if self.settings_action_open {
            self.agents.key(key);
            if self.agents.modal.is_none() {
                self.settings_action_open = false;
            }
            return;
        }
        if self.settings.is_some() {
            let outcome = self
                .settings
                .as_mut()
                .map(|panel| panel.key(key, &self.reviews.storage))
                .unwrap_or_default();
            if let Some(config) = outcome.saved {
                self.apply_committed_settings(config);
            }
            if let Some(preview) = outcome.preview_test {
                self.settings_preview_requested = Some(preview);
            }
            if let Some(action) = outcome.action {
                self.settings_action(action);
            }
            if let Some(panel) = &mut self.settings {
                panel.refresh_preview(&self.reviews.config);
            }
            if outcome.close {
                self.settings = None;
                self.agents.preview_visual_settings(&self.reviews.config);
            }
            return;
        }
        use crate::shortcuts::{Bindings, contexts, ids};
        let context = if self.palette.is_some() {
            contexts::PALETTE
        } else if self.agents_active && self.agents.browser_input() {
            contexts::AGENT_BROWSER
        } else if self.agents_active {
            contexts::AGENT_COMPOSER
        } else {
            contexts::REVIEW_CONTENT
        };
        let action = Bindings::new(&self.reviews.config).resolve(context, key);
        match action {
            Some(ids::SETTINGS_OPEN) => {
                self.open_settings(None);
                return;
            }
            Some(ids::PALETTE_OPEN) => {
                if self.palette.is_some() {
                    self.palette = None;
                } else {
                    self.agents.cancel_voice();
                    self.agents.prepare_palette();
                    self.palette = Some(palette::Palette::new(self.reviews.storage.clone()));
                }
                return;
            }
            _ => {}
        }
        if self.palette.is_some() {
            self.palette_key(key);
            return;
        }
        match action {
            Some(ids::SHELL_AGENTS) => {
                self.switch(true);
                return;
            }
            Some(ids::SHELL_REVIEWS) => {
                self.switch(false);
                return;
            }
            Some(ids::APP_QUIT) => {
                if self.agents_active && self.agents.copy_chat_selection() {
                    return;
                }
                self.reviews.quit = true;
                return;
            }
            _ => {}
        }
        // Keep Ctrl+C's copy precedence even when exit is moved elsewhere; do not
        // let a legacy view handler turn the removed global default into exit.
        if key.code == KeyCode::Char('c')
            && key.modifiers == KeyModifiers::CONTROL
            && context != contexts::AGENT_BROWSER
            && self
                .reviews
                .config
                .preferences
                .keyboard
                .bindings
                .contains_key(ids::APP_QUIT.0)
            && action.is_none()
        {
            if self.agents_active {
                self.agents.copy_chat_selection();
            }
            return;
        }
        if self.agents_active {
            self.agents.key(key);
            self.take_settings_request();
            self.reviews
                .config
                .session_order
                .clone_from(&self.agents.session_order);
        } else {
            self.reviews.key_event(key);
        }
    }
    pub fn mouse(&mut self, mouse: MouseEvent) {
        self.reconcile_settings_action();
        if self.settings_action_open {
            self.agents.mouse(mouse);
            if self.agents.modal.is_none() {
                self.settings_action_open = false;
            }
            return;
        }
        if self.settings.is_some() {
            return;
        }
        if self.palette.is_some() {
            self.palette_mouse(mouse);
            return;
        }
        if mouse.row == 0 && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let mut x = TAB_MARGIN;
            for (index, label) in self.tab_labels().iter().enumerate() {
                let end = x + label.width() as u16;
                if (x..end).contains(&mouse.column) {
                    self.switch(index == 0);
                    break;
                }
                x = end;
            }
            return;
        }
        if self.agents_active {
            self.agents.mouse(mouse);
        } else {
            self.reviews.mouse(mouse);
        }
    }
    pub fn paste(&mut self, text: String) {
        self.reconcile_settings_action();
        if self.settings_action_open {
            self.agents.paste(&text);
            self.reconcile_settings_action();
            return;
        }
        if let Some(panel) = &mut self.settings {
            panel.paste(&text);
            return;
        }
        if let Some(palette) = &mut self.palette {
            palette.paste(&text);
            return;
        }
        if self.agents_active {
            self.agents.paste(&text);
        } else {
            self.reviews.paste(text);
        }
    }
    pub fn draw(&mut self, frame: &mut Frame) {
        if let Some(panel) = &mut self.settings {
            panel.refresh_preview(&self.reviews.config);
        }
        let visual = self
            .settings
            .as_ref()
            .map(|panel| panel.preview_config())
            .unwrap_or(&self.reviews.config);
        if self.settings.is_some() {
            self.agents.preview_visual_settings(visual);
        }
        let diff_visual = self
            .settings
            .as_ref()
            .map(|panel| panel.preview_config().clone());
        self.reviews.preview_diff_settings(diff_visual.as_ref());
        self.agents.configure_pr_images(&self.reviews.images);
        if self.agents_active {
            self.agents.draw(frame);
        } else {
            crate::ui::draw(frame, &mut self.reviews);
        }
        let mut x = TAB_MARGIN;
        for (index, label) in self.tab_labels().iter().enumerate() {
            let width = label.width() as u16;
            let active = self.agents_active == (index == 0);
            frame.render_widget(
                Paragraph::new(label.as_str()).style(if active {
                    Style::default().bg(ACCENT).fg(crate::ui::INK)
                } else {
                    Style::default().fg(DIM).bg(BG)
                }),
                Rect::new(x, 0, width.min(frame.area().width.saturating_sub(x)), 1),
            );
            x += width;
        }
        self.draw_palette(frame);
        if let Some(panel) = &mut self.settings {
            crate::theme::select(crate::theme::index(panel.preview_config().theme.as_deref()));
            crate::theme::set_contrast(panel.preview_config().preferences.appearance.contrast);
            panel.draw(frame);
        }
        if self.settings_action_open {
            self.agents.draw_settings_action(frame);
        }
        // A tab switch flips the whole screen in.
        if self.settings.is_none()
            && let Some(started) = self.switched
            && !self.agents.flip_screen(frame.buffer_mut(), started)
        {
            self.switched = None;
        }
    }
    pub fn clipboard(&mut self, output: &mut impl Write) {
        self.reviews.flush_clipboard(output);
        if let Some(text) = self.agents.clipboard.take() {
            self.agents.notice = Some(
                match execute!(output, CopyToClipboard::to_clipboard_from(text)) {
                    Ok(()) => ("Sent to terminal clipboard".into(), false),
                    Err(error) => (format!("Could not send clipboard content: {error}"), true),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_tabs_persists_choice_without_overwriting_settings() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut shell = Shell::new(storage.clone(), Config::default(), None);
        assert!(shell.agents_active);
        // Treat review loading as already started to keep this test offline.
        shell.reviews_started = true;
        let config = Config {
            wrap_diff: true,
            ..Default::default()
        };
        storage.save_config(&config)?;
        shell.key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT));
        assert!(shell.switched.is_some() && shell.redraw_interval().is_some());
        let saved = storage.load_config()?;
        assert!(!saved.last_tab_agents);
        assert!(saved.wrap_diff);
        assert!(!shell.reviews.config.last_tab_agents);
        shell.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert!(storage.load_config()?.last_tab_agents);
        let reviews_start =
            TAB_MARGIN + shell.tab_labels().first().map_or(0, |label| label.width()) as u16;
        shell.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: reviews_start,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!shell.agents_active);
        shell.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!shell.agents_active);
        shell.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: TAB_MARGIN,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(shell.agents_active);
        Ok(())
    }
}

#[cfg(test)]
mod settings_integration_tests {
    use super::*;
    use crate::settings::{Category, KeyChord, SettingId, SettingValue};
    use anyhow::{Context, Result};
    fn fixture() -> Result<(tempfile::TempDir, Shell)> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut shell = Shell::new(storage, Config::default(), None);
        shell.reviews_started = true;
        Ok((dir, shell))
    }
    #[test]
    fn committed_refresh_preserves_dirty_settings_and_unsent_session_state() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.open_settings(None);
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(SettingId::Theme, SettingValue::Choice("amber".into()))?;
        shell.agents.selected = Some("session".into());
        let position = shell.agents.positions.entry("session".into()).or_default();
        position.draft = crate::editor::Editor::from("Keep this draft");
        position.conversation = 42;
        position.selection = Some(9);
        shell.reviews.storage.update_config(|config| {
            config.preferences.motion.level = crate::settings::Motion::Off;
            config.theme = Some("dracula".into());
            config.agent_list_visible = false;
        })?;
        shell.refresh_settings();
        assert_eq!(shell.reviews.config.theme.as_deref(), Some("dracula"));
        assert_eq!(
            shell.agents.preferences.motion.level,
            crate::settings::Motion::Off
        );
        assert!(!shell.agents.list_visible);
        assert_eq!(
            shell
                .settings
                .as_ref()
                .context("panel")?
                .draft
                .config()
                .theme
                .as_deref(),
            Some("amber")
        );
        let position = shell.agents.positions.get("session").context("position")?;
        assert_eq!(position.draft.text(), "Keep this draft");
        assert_eq!(position.conversation, 42);
        assert_eq!(position.selection, Some(9));
        std::fs::write(&shell.reviews.storage.config, "{ malformed")?;
        shell.settings_refreshed = None;
        shell.refresh_settings();
        assert_eq!(shell.reviews.config.theme.as_deref(), Some("dracula"));
        assert!(
            shell
                .agents
                .notice
                .as_ref()
                .is_some_and(|(message, error)| *error && message.contains("refresh"))
        );
        assert_eq!(
            std::fs::read_to_string(&shell.reviews.storage.config)?,
            "{ malformed"
        );
        Ok(())
    }

    #[test]
    fn two_clients_preview_only_edited_fields_and_keep_same_field_conflicts() -> Result<()> {
        let (_dir, mut first) = fixture()?;
        let mut second = Shell::new(first.reviews.storage.clone(), Config::default(), None);
        first.open_settings(None);
        second.open_settings(None);
        first
            .settings
            .as_mut()
            .context("first")?
            .draft
            .set(SettingId::Theme, SettingValue::Choice("amber".into()))?;
        let draft = &mut second.settings.as_mut().context("second")?.draft;
        draft.set(SettingId::Theme, SettingValue::Choice("dracula".into()))?;
        draft.set(SettingId::Motion, SettingValue::Choice("off".into()))?;
        draft.set(SettingId::Contrast, SettingValue::Choice("high".into()))?;
        second.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        first.refresh_settings();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))?;
        terminal.draw(|frame| first.draw(frame))?;
        assert_eq!(crate::theme::active().id, "amber");
        assert_eq!(
            first.agents.preferences.motion.level,
            crate::settings::Motion::Off,
            "unedited external motion stays current while theme is previewed"
        );
        assert_eq!(
            first.agents.preferences.appearance.contrast,
            crate::settings::Contrast::High
        );
        assert_eq!(
            first
                .settings
                .as_ref()
                .context("first")?
                .draft
                .config()
                .preferences
                .motion
                .level,
            crate::settings::Motion::Full,
            "the original draft baseline stays intact"
        );
        first.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            first
                .settings
                .as_ref()
                .context("first")?
                .error
                .as_deref()
                .is_some_and(|error| error.contains("changed"))
        );
        assert_eq!(
            first.reviews.storage.load_config()?.theme.as_deref(),
            Some("dracula")
        );
        first.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| first.draw(frame))?;
        assert_eq!(crate::theme::active().id, "dracula");
        assert_eq!(
            first.agents.preferences.motion.level,
            crate::settings::Motion::Off
        );
        Ok(())
    }

    #[test]
    fn legacy_zero_speed_never_schedules_a_tab_transition() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.reviews.config.tweaks.agent_reply_cps = 0;
        shell.agents.tweaks.agent_reply_cps = 0;
        shell.switch(false);
        assert_eq!(shell.redraw_interval(), None);
        assert!(shell.switched.is_none());
        Ok(())
    }

    #[test]
    fn all_theme_previews_stay_local_and_save_discard_apply_visual_settings() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35))?;
        shell.open_settings(None);
        for theme in crate::theme::themes() {
            let draft = &mut shell.settings.as_mut().context("panel")?.draft;
            draft.set(SettingId::Theme, SettingValue::Choice(theme.id.into()))?;
            draft.set(SettingId::Contrast, SettingValue::Choice("high".into()))?;
            draft.set(SettingId::Density, SettingValue::Choice("compact".into()))?;
            draft.set(SettingId::MessageSeparators, SettingValue::Bool(true))?;
            draft.set(SettingId::VoiceEnabled, SettingValue::Bool(true))?;
            draft.set(SettingId::Motion, SettingValue::Choice("off".into()))?;
            terminal.draw(|frame| {
                shell.draw(frame);
                crate::theme::present(frame.buffer_mut());
            })?;
            assert_eq!(crate::theme::active().id, theme.id);
            assert_eq!(
                crate::theme::resolve(crate::theme::slot(crate::theme::Role::Dim)),
                theme.color(crate::theme::Role::Text)
            );
            assert_eq!(
                shell.agents.preferences.appearance.density,
                crate::settings::Density::Compact
            );
            assert!(
                !shell.agents.voice_enabled(),
                "preview must not apply nonvisual settings"
            );
            assert!(
                !shell.reviews.storage.config.exists(),
                "preview must not persist"
            );
        }
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(crate::theme::active().id, "default");
        assert_eq!(
            shell.agents.preferences.appearance,
            crate::settings::Appearance::default()
        );
        assert_eq!(
            shell.agents.preferences.motion.level,
            crate::settings::Motion::Full
        );
        assert_eq!(
            crate::theme::resolve(crate::theme::slot(crate::theme::Role::Dim)),
            crate::theme::active().color(crate::theme::Role::Dim)
        );
        let draft = &mut shell.settings.as_mut().context("panel")?.draft;
        draft.set(SettingId::Theme, SettingValue::Choice("amber".into()))?;
        draft.set(SettingId::Motion, SettingValue::Choice("reduced".into()))?;
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(crate::theme::active().id, "amber");
        assert_eq!(
            shell.reviews.storage.load_config()?.theme.as_deref(),
            Some("amber")
        );
        assert_eq!(
            shell.agents.preferences.motion.level,
            crate::settings::Motion::Reduced
        );
        Ok(())
    }

    #[test]
    fn settings_preserves_agent_draft_selection_and_scroll_from_both_tabs() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.agents.selected = Some("unchanged".into());
        shell
            .agents
            .positions
            .entry("unchanged".into())
            .or_default()
            .draft = crate::editor::Editor::from("Unsent text");
        shell
            .agents
            .positions
            .get_mut("unchanged")
            .context("position")?
            .conversation = 42;
        shell
            .agents
            .positions
            .get_mut("unchanged")
            .context("position")?
            .selection = Some(9);
        for active in [true, false] {
            shell.agents_active = active;
            shell.key(KeyEvent::new(KeyCode::Char(','), KeyModifiers::CONTROL));
            let panel = shell.settings.as_mut().context("panel")?;
            panel
                .draft
                .set(SettingId::WrapDiff, SettingValue::Bool(true))?;
            shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
            assert!(shell.reviews.config.wrap_diff);
            assert_eq!(
                shell
                    .agents
                    .positions
                    .get("unchanged")
                    .context("position")?
                    .draft
                    .text(),
                "Unsent text"
            );
            assert_eq!(
                shell
                    .agents
                    .positions
                    .get("unchanged")
                    .context("position")?
                    .conversation,
                42
            );
            assert_eq!(
                shell
                    .agents
                    .positions
                    .get("unchanged")
                    .context("position")?
                    .selection,
                Some(9)
            );
            shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(shell.settings.is_none());
            assert_eq!(shell.agents_active, active);
            assert_eq!(shell.agents.selected.as_deref(), Some("unchanged"));
        }
        Ok(())
    }
    #[test]
    fn settings_shortcut_override_replaces_default_and_slash_aliases_request_sections() -> Result<()>
    {
        let (_dir, mut shell) = fixture()?;
        shell
            .reviews
            .config
            .preferences
            .keyboard
            .bindings
            .insert("settings.open".into(), KeyChord::character('x'));
        assert!(!settings_panel::is_open_shortcut(
            &shell.reviews.config,
            KeyEvent::new(KeyCode::Char(','), KeyModifiers::CONTROL)
        ));
        shell.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(shell.settings.is_some());
        shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        for (command, category) in [
            ("/settings", Category::Appearance),
            ("/theme", Category::Appearance),
            ("/tweaks", Category::Motion),
            ("/voice", Category::Input),
        ] {
            shell.agents.run_palette_command(command, command);
            shell.take_settings_request();
            assert_eq!(shell.settings.as_ref().context("panel")?.category, category);
            shell.settings = None;
        }
        Ok(())
    }
    #[test]
    fn separate_action_keeps_the_preferences_draft_until_return() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.open_settings(None);
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(SettingId::WrapDiff, SettingValue::Bool(true))?;
        shell.settings_action(SettingId::Rules);
        assert!(shell.settings.is_some());
        assert_eq!(
            shell
                .settings
                .as_ref()
                .context("preserved panel")?
                .draft
                .edited()
                .len(),
            1
        );
        Ok(())
    }
    #[test]
    fn opening_settings_with_malformed_config_shows_error_in_reviews() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.agents_active = false;
        std::fs::write(&shell.reviews.storage.config, "{malformed")?;
        shell.open_settings(None);
        assert!(shell.settings.is_none());
        assert_eq!(shell.reviews.notice.kind, crate::app::NoticeKind::Error);
        assert_eq!(
            std::fs::read_to_string(&shell.reviews.storage.config)?,
            "{malformed"
        );
        Ok(())
    }
    fn settings_after_async_action_close() -> Result<(tempfile::TempDir, Shell)> {
        let (dir, mut shell) = fixture()?;
        shell.agents.selected = Some("retained".into());
        shell.agents.drilled = true;
        shell.agents.focus = crate::agents::ui::Focus::Composer;
        let position = shell.agents.positions.entry("retained".into()).or_default();
        position.draft = crate::editor::Editor::from("Unsent draft");
        position.conversation = 17;
        position.selection = Some(4);
        shell.open_settings(None);
        shell.settings_action(SettingId::Rules);
        assert!(shell.settings_action_open && shell.agents.modal.is_some());
        // An asynchronous Ui completion clears its modal without a modal key event.
        shell.agents.modal = None;
        Ok((dir, shell))
    }
    #[test]
    fn async_action_close_returns_key_and_paste_to_settings_without_touching_composer() -> Result<()>
    {
        for paste_first in [false, true] {
            let (_dir, mut shell) = settings_after_async_action_close()?;
            if paste_first {
                shell.paste("pasted query".into());
            } else {
                shell.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            }
            let position = shell.agents.positions.get("retained").context("position")?;
            assert_eq!(position.draft.text(), "Unsent draft");
            assert_eq!(position.conversation, 17);
            assert_eq!(position.selection, Some(4));
            assert_eq!(shell.agents.selected.as_deref(), Some("retained"));
            assert!(!shell.settings_action_open);
            assert_eq!(
                shell.settings.as_ref().context("panel")?.query,
                if paste_first { "pasted query" } else { "x" }
            );
        }
        Ok(())
    }
    #[test]
    fn async_action_close_is_reconciled_after_tick_and_before_mouse_input() -> Result<()> {
        let (_dir, mut shell) = settings_after_async_action_close()?;
        shell.tick_focused(false);
        assert!(!shell.settings_action_open);
        let (_other, mut shell) = settings_after_async_action_close()?;
        shell.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!shell.settings_action_open);
        assert_eq!(shell.agents.selected.as_deref(), Some("retained"));
        assert_eq!(
            shell
                .agents
                .positions
                .get("retained")
                .context("position")?
                .draft
                .text(),
            "Unsent draft"
        );
        Ok(())
    }
    #[test]
    fn layout_preview_ticks_never_contaminate_reviews_and_discard_loads_latest() -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.reviews.storage.save_config(&Config::default())?;
        shell.open_settings(Some(Category::Layout));
        let draft = &mut shell.settings.as_mut().context("panel")?.draft;
        draft.set(SettingId::SidebarVisible, SettingValue::Bool(false))?;
        draft.set(SettingId::ChangesVisible, SettingValue::Bool(true))?;
        draft.set(
            SettingId::HelperPlacement,
            SettingValue::Choice("bottom".into()),
        )?;
        draft.set(SettingId::ComposerHeight, SettingValue::Number(3))?;
        draft.set(
            SettingId::RunningInput,
            SettingValue::Choice("queue".into()),
        )?;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
        terminal.draw(|frame| shell.draw(frame))?;
        assert!(!shell.agents.list_visible && shell.agents.changes_visible);
        assert_eq!(shell.agents.preferences.input.composer_max_height, 3);
        assert_eq!(
            shell.agents.preferences.input.running,
            crate::settings::RunningInput::Steer
        );
        shell.tick_focused(false);
        assert!(
            shell.reviews.config.agent_list_visible,
            "preview must not be mirrored into committed Reviews"
        );
        assert!(!shell.reviews.config.agent_changes_visible);
        assert!(shell.reviews.storage.load_config()?.agent_list_visible);
        shell.reviews.storage.update_config(|c| {
            c.preferences.layout.sidebar_width = 35;
            c.preferences.conversation.timestamps = true;
        })?;
        shell.settings_refreshed = None;
        shell.refresh_settings();
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(shell.agents.preferences.layout.sidebar_width, 35);
        assert!(shell.agents.preferences.conversation.timestamps);
        assert_eq!(
            shell
                .settings
                .as_ref()
                .context("panel")?
                .draft
                .config()
                .preferences
                .layout
                .sidebar_width,
            0,
            "baseline is not replaced by refresh"
        );
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert!(shell.agents.list_visible && shell.reviews.config.agent_list_visible);
        assert!(!shell.agents.changes_visible && !shell.reviews.config.agent_changes_visible);
        assert_eq!(shell.agents.preferences.layout.sidebar_width, 35);
        assert_eq!(shell.agents.preferences.input.composer_max_height, 10);
        Ok(())
    }

    #[test]
    fn startup_tab_respects_explicit_review_routes_and_last_used() -> Result<()> {
        use crate::settings::StartupTab;
        for (startup, last_agents, want_reviews) in [
            (StartupTab::LastUsed, true, false),
            (StartupTab::LastUsed, false, true),
            (StartupTab::Agents, false, false),
            (StartupTab::Reviews, true, true),
        ] {
            let mut config = Config {
                last_tab_agents: last_agents,
                ..Default::default()
            };
            config.preferences.layout.startup_tab = startup;
            assert_eq!(Shell::reviews_on_launch(&config, false), want_reviews);
            assert!(
                Shell::reviews_on_launch(&config, true),
                "explicit PR target overrides {startup:?}"
            );
        }
        let (dir, shell) = fixture()?;
        let storage = shell.reviews.storage.clone();
        let mut config = Config::default();
        config.preferences.layout.startup_tab = StartupTab::Agents;
        let ordinary = Shell::new(storage.clone(), config.clone(), None);
        assert!(ordinary.agents_active);
        for startup in [
            StartupTab::LastUsed,
            StartupTab::Agents,
            StartupTab::Reviews,
        ] {
            config.last_tab_agents = false;
            config.preferences.layout.startup_tab = startup;
            let local = Shell::local(storage.clone(), config.clone(), dir.path().into());
            assert_eq!(
                local.agents.preferences.layout.startup_tab, startup,
                "internal landing override must not replace committed preferences"
            );
            assert_eq!(local.reviews.config.preferences.layout.startup_tab, startup);
            assert!(
                !local.agents_active && local.reviews_started,
                "explicit local target overrides {startup:?}"
            );
            assert!(
                !local.reviews.inbox_loading,
                "local target must not start an unrelated PR inbox load for {startup:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn conversation_layout_input_save_reset_and_discard_apply_at_their_effect_boundary()
    -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.reviews.storage.save_config(&Config::default())?;
        shell.open_settings(Some(Category::Conversation));
        let panel = shell.settings.as_mut().context("panel")?;
        for (id, value) in [
            (
                SettingId::ToolDisplay,
                SettingValue::Choice("expanded".into()),
            ),
            (SettingId::ToolPreviewLines, SettingValue::Number(6)),
            (SettingId::ReasoningExpanded, SettingValue::Bool(true)),
            (SettingId::Timestamps, SettingValue::Bool(true)),
            (SettingId::AutoScroll, SettingValue::Choice("manual".into())),
            (SettingId::SidebarVisible, SettingValue::Bool(false)),
            (SettingId::ChangesVisible, SettingValue::Bool(true)),
            (SettingId::SidebarWidth, SettingValue::Number(40)),
            (
                SettingId::HelperPlacement,
                SettingValue::Choice("bottom".into()),
            ),
            (
                SettingId::SessionSort,
                SettingValue::Choice("recent".into()),
            ),
            (
                SettingId::StartupTab,
                SettingValue::Choice("reviews".into()),
            ),
            (SettingId::ComposerHeight, SettingValue::Number(3)),
            (
                SettingId::PasteThreshold,
                SettingValue::Choice("never".into()),
            ),
            (
                SettingId::RunningInput,
                SettingValue::Choice("queue".into()),
            ),
        ] {
            panel.draft.set(id, value)?;
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell.agents.preferences.conversation.tool_display,
            crate::settings::ToolDisplay::Expanded
        );
        assert_eq!(
            shell.agents.preferences.input.running,
            crate::settings::RunningInput::Steer
        );
        assert_eq!(
            shell.reviews.storage.load_config()?.preferences,
            crate::settings::Preferences::default()
        );
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            shell.agents.preferences.input.running,
            crate::settings::RunningInput::Queue
        );
        assert_eq!(
            shell.agents.preferences,
            shell.reviews.storage.load_config()?.preferences
        );
        assert!(!shell.agents.list_visible && shell.agents.changes_visible);
        assert!(
            !shell.reviews.config.agent_list_visible && shell.reviews.config.agent_changes_visible
        );
        assert!(Shell::reviews_on_launch(&shell.reviews.config, false));
        // Reset each section is a draft operation, including a live visual rollback.
        let saved = shell.reviews.storage.load_config()?;
        for category in [Category::Conversation, Category::Layout, Category::Input] {
            let panel = shell.settings.as_mut().context("panel")?;
            panel.category = category;
            panel.reset_section();
        }
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell.agents.preferences.conversation.tool_display,
            crate::settings::ToolDisplay::Compact
        );
        assert_eq!(shell.agents.preferences.conversation.tool_preview_lines, 4);
        assert!(!shell.agents.preferences.conversation.reasoning_expanded);
        assert!(!shell.agents.preferences.conversation.timestamps);
        assert_eq!(
            shell.agents.preferences.conversation.auto_scroll,
            crate::settings::AutoScroll::Manual,
            "behavioral reset waits for Save"
        );
        assert!(shell.agents.list_visible && !shell.agents.changes_visible);
        assert!(!shell.reviews.storage.load_config()?.agent_list_visible);
        assert_eq!(shell.agents.preferences.input.composer_max_height, 10);
        assert_eq!(
            shell.agents.preferences.input.running,
            crate::settings::RunningInput::Queue,
            "nonvisual reset waits for Save"
        );
        assert_eq!(
            shell.reviews.storage.load_config()?.preferences,
            saved.preferences
        );
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(shell.agents.preferences, saved.preferences);
        assert!(
            shell
                .settings
                .as_ref()
                .context("panel")?
                .draft
                .edited()
                .is_empty()
        );
        for category in [Category::Conversation, Category::Layout, Category::Input] {
            let panel = shell.settings.as_mut().context("panel")?;
            panel.category = category;
            panel.reset_section();
        }
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            shell.agents.preferences,
            crate::settings::Preferences::default()
        );
        assert_eq!(
            shell.reviews.storage.load_config()?.preferences,
            crate::settings::Preferences::default()
        );
        assert!(shell.agents.list_visible && !shell.agents.changes_visible);
        assert!(shell.reviews.storage.load_config()?.agent_list_visible);
        assert!(!shell.reviews.storage.load_config()?.agent_changes_visible);
        Ok(())
    }
    #[test]
    fn auto_scroll_preview_keeps_live_anchor_copy_and_draft_until_save() -> Result<()> {
        use crate::agents::{Job, Launch, Session, ui::Focus};
        use crate::settings::{AutoScroll, Motion};
        let (_dir, mut shell) = fixture()?;
        let mut config = Config {
            agent_list_visible: false,
            ..Default::default()
        };
        config.preferences.motion.level = Motion::Off;
        shell.reviews.storage.save_config(&config)?;
        shell.apply_committed_settings(config);
        let mut session = Session::new(
            "scroll-session".into(),
            Job::Coding(Launch {
                repository: "/synthetic/project".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "Reading".into(),
                model: None,
                effort: None,
            }),
        );
        for n in 0..30 {
            session.note("agentMessage", format!("reading row {n}"));
        }
        shell.agents.selected = Some(session.id.clone());
        shell.agents.sessions.insert(session.id.clone(), session);
        shell.agents.drilled = true;
        shell.agents.focus = Focus::Conversation;
        shell
            .agents
            .positions
            .entry("scroll-session".into())
            .or_default()
            .draft = crate::editor::Editor::from("Keep this draft");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
        terminal.draw(|frame| shell.draw(frame))?;
        let (x, y) = terminal
            .backend()
            .buffer()
            .content
            .chunks(120)
            .enumerate()
            .find_map(|(y, row)| {
                let line = row.iter().map(|c| c.symbol()).collect::<String>();
                line.find("reading row 0").map(|x| (x as u16, y as u16))
            })
            .context("visible reading row")?;
        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), x),
            (MouseEventKind::Drag(MouseButton::Left), x + 13),
            (MouseEventKind::Up(MouseButton::Left), x + 13),
        ] {
            shell.agents.mouse(MouseEvent {
                kind,
                column,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
        }
        assert!(shell.agents.copy_chat_selection());
        let selected = shell.agents.clipboard.take().context("copy")?;
        let anchor = shell
            .agents
            .positions
            .get("scroll-session")
            .context("position")?
            .scroll_anchor
            .clone();
        shell.open_settings(Some(Category::Conversation));
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(SettingId::AutoScroll, SettingValue::Choice("always".into()))?;
        terminal.draw(|frame| shell.draw(frame))?;
        shell
            .agents
            .sessions
            .get_mut("scroll-session")
            .context("session")?
            .note("agentMessage", "Newest streaming reply");
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell.agents.preferences.conversation.auto_scroll,
            AutoScroll::WhenAtBottom,
            "draft policy must affect only the synthetic preview"
        );
        let position = shell
            .agents
            .positions
            .get("scroll-session")
            .context("position")?;
        assert_eq!(position.conversation, 0);
        assert_eq!(position.scroll_anchor, anchor);
        assert_eq!(position.draft.text(), "Keep this draft");
        assert!(shell.agents.copy_chat_selection());
        assert_eq!(shell.agents.clipboard.as_ref(), Some(&selected));
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell
                .agents
                .positions
                .get("scroll-session")
                .context("position")?
                .scroll_anchor,
            anchor
        );
        assert!(shell.agents.copy_chat_selection());
        assert_eq!(shell.agents.clipboard.as_ref(), Some(&selected));
        assert_eq!(
            shell
                .reviews
                .storage
                .load_config()?
                .preferences
                .conversation
                .auto_scroll,
            AutoScroll::WhenAtBottom
        );
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(SettingId::AutoScroll, SettingValue::Choice("always".into()))?;
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell.agents.preferences.conversation.auto_scroll,
            AutoScroll::Always
        );
        assert!(
            shell
                .agents
                .positions
                .get("scroll-session")
                .context("position")?
                .conversation
                > 0,
            "saved Always now follows the live transcript"
        );
        assert_eq!(
            shell
                .agents
                .positions
                .get("scroll-session")
                .context("position")?
                .draft
                .text(),
            "Keep this draft"
        );
        Ok(())
    }
    fn check_reading_intent_across_detail_preview(kind: &str) -> Result<()> {
        use crate::agents::{Entry, Job, Launch, Session, Status, ui::Focus};
        use crate::settings::{AutoScroll, Motion, ToolDisplay};
        let (_dir, mut shell) = fixture()?;
        let mut config = Config {
            agent_list_visible: false,
            ..Default::default()
        };
        config.preferences.motion.level = Motion::Off;
        config.preferences.conversation.auto_scroll = AutoScroll::Manual;
        config.preferences.conversation.tool_display = ToolDisplay::Expanded;
        config.preferences.conversation.reasoning_expanded = true;
        shell.reviews.storage.save_config(&config)?;
        shell.apply_committed_settings(config);
        let mut session = Session::new(
            "reader".into(),
            Job::Coding(Launch {
                repository: "/synthetic/project".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "Read output".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.note("agentMessage", "Before the long entry");
        let text = (0..80)
            .map(|n| format!("reading item {n:02}"))
            .collect::<Vec<_>>()
            .join(if kind == "reasoning" {
                "

"
            } else {
                "
"
            });
        session.entries.push(Entry {
            id: "long-entry".into(),
            kind: kind.into(),
            text: text.into(),
            data: serde_json::json!({"command":"echo values"}).into(),
            ..Default::default()
        });
        for n in 0..6 {
            session.note("agentMessage", format!("Trailing message {n}"));
        }
        shell.agents.selected = Some(session.id.clone());
        shell.agents.sessions.insert(session.id.clone(), session);
        shell.agents.drilled = true;
        shell.agents.focus = Focus::Conversation;
        let position = shell.agents.positions.entry("reader".into()).or_default();
        position.scroll_anchor = Some(("long-entry".into(), 40));
        position.draft = crate::editor::Editor::from("Keep this draft");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
        terminal.draw(|frame| shell.draw(frame))?;
        let original = shell
            .agents
            .positions
            .get("reader")
            .context("position")?
            .scroll_anchor
            .clone();
        assert_eq!(original, Some(("long-entry".into(), 40)));
        let original_row = shell
            .agents
            .positions
            .get("reader")
            .context("position")?
            .conversation;
        let (x, y) = terminal
            .backend()
            .buffer()
            .content
            .chunks(120)
            .enumerate()
            .find_map(|(y, row)| {
                let line = row.iter().map(|c| c.symbol()).collect::<String>();
                line.find("reading item").and_then(|at| {
                    line.get(..at)
                        .map(|prefix| (prefix.chars().count() as u16, y as u16))
                })
            })
            .context("mid-entry reading row")?;
        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), x),
            (MouseEventKind::Drag(MouseButton::Left), x + 15),
            (MouseEventKind::Up(MouseButton::Left), x + 15),
        ] {
            shell.agents.mouse(MouseEvent {
                kind,
                column,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
        }
        assert!(shell.agents.copy_chat_selection());
        let copied = shell.agents.clipboard.take().context("selected source")?;
        assert!(copied.contains("reading item"));
        let (id, collapsed, expanded) = if kind == "reasoning" {
            (
                SettingId::ReasoningExpanded,
                SettingValue::Bool(false),
                SettingValue::Bool(true),
            )
        } else {
            (
                SettingId::ToolDisplay,
                SettingValue::Choice("collapsed".into()),
                SettingValue::Choice("expanded".into()),
            )
        };
        shell.open_settings(Some(Category::Conversation));
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(id.clone(), collapsed.clone())?;
        terminal.draw(|frame| shell.draw(frame))?;
        assert!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .conversation
                < original_row,
            "preview must actually clamp the live display"
        );
        let session = shell.agents.sessions.get_mut("reader").context("session")?;
        session
            .entries
            .iter_mut()
            .find(|entry| entry.id == "long-entry")
            .context("long entry")?
            .text
            .push_str("\n\nIncoming continuation after the original reading point");
        session.note("agentMessage", "Incoming while the preview is collapsed");
        terminal.draw(|frame| shell.draw(frame))?;
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        let position = shell.agents.positions.get("reader").context("position")?;
        assert_eq!(
            position.scroll_anchor, original,
            "Discard restores the original intra-entry offset for {kind}"
        );
        assert_eq!(position.conversation, original_row);
        assert_eq!(position.draft.text(), "Keep this draft");
        assert!(shell.agents.copy_chat_selection());
        assert_eq!(shell.agents.clipboard.as_ref(), Some(&copied));
        // Saving a collapse keeps the latent source intent; navigation replaces it.
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(id.clone(), collapsed.clone())?;
        terminal.draw(|frame| shell.draw(frame))?;
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            crate::settings::get(&shell.reviews.storage.load_config()?, &id),
            collapsed
        );
        assert_eq!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .scroll_anchor,
            original
        );
        assert!(shell.agents.copy_chat_selection());
        assert_eq!(shell.agents.clipboard.as_ref(), Some(&copied));
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(id.clone(), expanded.clone())?;
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .conversation,
            original_row,
            "a saved collapse keeps its latent reading intent for later expansion"
        );
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            crate::settings::get(&shell.reviews.storage.load_config()?, &id),
            collapsed
        );
        shell.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(shell.settings.is_none());
        shell.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        terminal.draw(|frame| shell.draw(frame))?;
        let navigated = shell
            .agents
            .positions
            .get("reader")
            .context("position")?
            .scroll_anchor
            .clone();
        assert_ne!(navigated, original);
        assert_eq!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .conversation,
            0
        );
        shell.open_settings(Some(Category::Conversation));
        shell
            .settings
            .as_mut()
            .context("panel")?
            .draft
            .set(id.clone(), expanded)?;
        terminal.draw(|frame| shell.draw(frame))?;
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        terminal.draw(|frame| shell.draw(frame))?;
        assert_eq!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .scroll_anchor,
            navigated,
            "later expansion must not resurrect a replaced reading intent"
        );
        assert_eq!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .conversation,
            0
        );
        assert_eq!(
            shell
                .agents
                .positions
                .get("reader")
                .context("position")?
                .draft
                .text(),
            "Keep this draft"
        );
        Ok(())
    }
    #[test]
    fn reading_intent_survives_tool_preview_discard_and_saved_navigation() -> Result<()> {
        check_reading_intent_across_detail_preview("commandExecution")
    }
    #[test]
    fn reading_intent_survives_reasoning_preview_discard_and_saved_navigation() -> Result<()> {
        check_reading_intent_across_detail_preview("reasoning")
    }
    #[test]
    fn diff_settings_save_reset_discard_and_external_refresh_keep_committed_state_isolated()
    -> Result<()> {
        let (_dir, mut shell) = fixture()?;
        shell.open_settings(Some(Category::Code));
        let edited = [
            (SettingId::UnifiedDiff, SettingValue::Bool(true)),
            (SettingId::WrapDiff, SettingValue::Bool(true)),
            (SettingId::LineNumbers, SettingValue::Bool(false)),
            (SettingId::DiffContext, SettingValue::Number(10)),
            (
                SettingId::Whitespace,
                SettingValue::Choice("selected_lines".into()),
            ),
        ];
        for (id, value) in &edited {
            shell
                .settings
                .as_mut()
                .context("panel")?
                .draft
                .set(id.clone(), value.clone())?;
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
        terminal.draw(|f| shell.draw(f))?;
        assert!(!shell.reviews.config.unified && !shell.reviews.config.wrap_diff);
        assert!(shell.reviews.diff_config().unified && shell.reviews.diff_config().wrap_diff);
        assert_eq!(
            shell.reviews.config.preferences.code,
            crate::settings::Code::default()
        );
        assert_eq!(shell.agents.preferences.code.context_lines, 10);
        assert!(!shell.agents.preferences.code.line_numbers);
        assert_eq!(
            shell.agents.preferences.code.whitespace,
            crate::settings::Whitespace::SelectedLines
        );
        assert_eq!(
            shell.reviews.storage.load_config()?.preferences.code,
            crate::settings::Code::default()
        );
        let external = shell
            .reviews
            .storage
            .update_config(|c| c.preferences.appearance.message_separators = true)?;
        shell.apply_committed_settings(external);
        terminal.draw(|f| shell.draw(f))?;
        assert!(
            shell
                .reviews
                .diff_config()
                .preferences
                .appearance
                .message_separators
        );
        assert_eq!(
            shell
                .settings
                .as_ref()
                .context("panel")?
                .draft
                .edited()
                .len(),
            5
        );
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let saved = shell.reviews.storage.load_config()?;
        for (id, value) in &edited {
            assert_eq!(crate::settings::get(&saved, id), *value);
        }
        assert_eq!(shell.agents.preferences.code, saved.preferences.code);
        shell.settings.as_mut().context("panel")?.reset_section();
        terminal.draw(|f| shell.draw(f))?;
        assert_eq!(
            shell.reviews.diff_config().preferences.code,
            crate::settings::Code::default()
        );
        assert_eq!(
            shell.reviews.storage.load_config()?.preferences.code,
            saved.preferences.code
        );
        shell.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        terminal.draw(|f| shell.draw(f))?;
        assert_eq!(
            shell.reviews.diff_config().preferences.code,
            saved.preferences.code
        );
        shell.settings.as_mut().context("panel")?.reset_section();
        shell.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            shell.reviews.config.preferences.code,
            crate::settings::Code::default()
        );
        assert!(!shell.reviews.config.unified && !shell.reviews.config.wrap_diff);
        assert!(
            shell
                .reviews
                .config
                .preferences
                .appearance
                .message_separators
        );
        Ok(())
    }
}
