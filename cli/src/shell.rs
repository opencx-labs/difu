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

const TAB_LABELS: [&str; 2] = [" Agents (⌥+1) ", " Reviews (⌥+2) "];
const TAB_MARGIN: u16 = 1;
mod palette;
pub(crate) mod settings_panel;

pub struct Shell {
    pub reviews: App,
    pub agents: Ui,
    pub agents_active: bool,
    reviews_started: bool,
    palette: Option<palette::Palette>,
    pub(crate) settings: Option<settings_panel::Panel>,
    settings_action_open: bool,
    pub(crate) settings_pending_action: Option<crate::settings::SettingId>,
    pub(crate) settings_preview_requested: Option<crate::settings::Preview>,
    /// When the last tab switch started flipping the screen in.
    switched: Option<std::time::Instant>,
}
impl Shell {
    pub fn new(storage: Storage, config: Config, pr: Option<PrKey>) -> Self {
        let mut reviews = App::new(storage.clone(), config.clone());
        let reviews_started = pr.is_some() || !config.last_tab_agents;
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
            settings_pending_action: None,
            settings_preview_requested: None,
            switched: None,
        }
    }
    pub fn local(storage: Storage, config: Config, path: std::path::PathBuf) -> Self {
        let mut initial = config.clone();
        initial.last_tab_agents = true;
        let mut shell = Self::new(storage, initial, None);
        shell.reviews.config = config;
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
            Ok(panel) => self.settings = Some(panel),
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
            self.switched = Some(std::time::Instant::now());
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
            || self.switched.is_some()
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
        let selected = self.palette_selection();
        self.agents
            .tick(focused && (self.agents_active || self.palette.is_some()));
        self.reconcile_settings_action();
        self.tick_palette();
        self.restore_palette_selection(selected);
        self.take_settings_request();
        self.reviews.config.agent_defaults = self.agents.defaults.clone();
        self.reviews.config.agent_list_visible = self.agents.list_visible;
        self.reviews.config.agent_changes_visible = self.agents.changes_visible;
        self.reviews.config.agent_panel_right = self.agents.panel_right();
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
                self.agents.apply_settings(&config);
                self.reviews.config = config;
            }
            if let Some(preview) = outcome.preview_test {
                self.settings_preview_requested = Some(preview);
            }
            if let Some(action) = outcome.action {
                self.settings_action(action);
            }
            if outcome.close {
                self.settings = None;
                crate::theme::select(crate::theme::index(self.reviews.config.theme.as_deref()));
            }
            return;
        }
        if settings_panel::is_open_shortcut(&self.reviews.config, key) {
            self.open_settings(None);
            return;
        }
        if key.code == KeyCode::Char('k') && key.modifiers == KeyModifiers::SUPER {
            if self.palette.is_some() {
                self.palette = None;
            } else {
                self.agents.cancel_voice();
                self.agents.prepare_palette();
                self.palette = Some(palette::Palette::new(self.reviews.storage.clone()));
            }
            return;
        }
        if self.palette.is_some() {
            self.palette_key(key);
            return;
        }
        if key.modifiers == KeyModifiers::ALT {
            match key.code {
                KeyCode::Char('1') => {
                    self.switch(true);
                    return;
                }
                KeyCode::Char('2') => {
                    self.switch(false);
                    return;
                }
                _ => {}
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.agents_active && self.agents.browser_input() {
                self.agents.key(key);
                return;
            }
            if self.agents_active && self.agents.copy_chat_selection() {
                return;
            }
            self.reviews.quit = true;
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
            for (index, label) in TAB_LABELS.iter().enumerate() {
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
        self.agents.configure_pr_images(&self.reviews.images);
        if self.agents_active {
            self.agents.draw(frame);
        } else {
            crate::ui::draw(frame, &mut self.reviews);
        }
        let mut x = TAB_MARGIN;
        for (index, label) in TAB_LABELS.iter().enumerate() {
            let width = label.width() as u16;
            let active = self.agents_active == (index == 0);
            frame.render_widget(
                Paragraph::new(*label).style(if active {
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
            crate::theme::select(crate::theme::index(panel.draft.config().theme.as_deref()));
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
            TAB_MARGIN + TAB_LABELS.first().copied().unwrap_or_default().width() as u16;
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
}
