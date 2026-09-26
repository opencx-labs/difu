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

pub struct Shell {
    pub reviews: App,
    pub agents: Ui,
    pub agents_active: bool,
    reviews_started: bool,
    palette: Option<palette::Palette>,
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
    fn switch(&mut self, agents: bool) {
        self.agents.cancel_voice();
        self.agents_active = agents;
        self.reviews.config.last_tab_agents = agents;
        let saved = self.reviews.storage.load_config().and_then(|mut config| {
            config.last_tab_agents = agents;
            self.reviews.storage.save_config(&config)
        });
        if let Err(error) = saved {
            self.agents.notice = Some((format!("Could not save the active tab: {error:#}"), true));
        }
        self.reviews.hover = Default::default();
        if !agents && !self.reviews_started {
            self.reviews.start(None);
            self.reviews_started = true;
        }
    }
    pub fn tick(&mut self) {
        let selected = self.palette_selection();
        self.agents
            .tick(self.agents_active || self.palette.is_some());
        self.tick_palette();
        self.restore_palette_selection(selected);
        self.reviews.config.agent_defaults = self.agents.defaults.clone();
        self.reviews.config.agent_list_visible = self.agents.list_visible;
        self.reviews.config.agent_changes_visible = self.agents.changes_visible;
        self.reviews.config.agent_panel_right = self.agents.panel_right();
        self.reviews.config.pinned_sessions = self.agents.pinned_sessions.clone();
        self.reviews.config.voice_enabled = self.agents.voice_enabled();
        if self.reviews_started {
            self.reviews.tick_visible(!self.agents_active);
        }
        if self.agents.review_requested {
            self.agents.review_requested = false;
            self.switch(false);
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            if self.agents_active {
                self.agents.key(key);
            } else {
                self.reviews.key_event(key);
            }
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
        } else {
            self.reviews.key_event(key);
        }
    }
    pub fn mouse(&mut self, mouse: MouseEvent) {
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
