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

pub struct Shell {
    pub reviews: App,
    pub agents: Ui,
    pub agents_active: bool,
    reviews_started: bool,
}
impl Shell {
    pub fn new(storage: Storage, config: Config, pr: Option<PrKey>) -> Self {
        let mut reviews = App::new(storage.clone(), config.clone());
        let reviews_started = pr.is_some();
        if reviews_started {
            reviews.start(pr);
        }
        Self {
            reviews,
            agents: Ui::new(storage, &config),
            agents_active: !reviews_started,
            reviews_started,
        }
    }
    fn switch(&mut self, agents: bool) {
        self.agents.cancel_voice();
        self.agents_active = agents;
        self.reviews.hover = Default::default();
        if !agents && !self.reviews_started {
            self.reviews.start(None);
            self.reviews_started = true;
        }
    }
    pub fn tick(&mut self) {
        self.agents.tick(self.agents_active);
        self.reviews.config.agent_defaults = self.agents.defaults.clone();
        self.reviews.config.agent_list_visible = self.agents.list_visible;
        self.reviews.config.agent_changes_visible = self.agents.changes_visible;
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
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('1') => {
                    self.switch(true);
                    return;
                }
                KeyCode::Char('2') => {
                    self.switch(false);
                    return;
                }
                KeyCode::Char('c') => {
                    if self.agents_active && self.agents.copy_chat_selection() {
                        return;
                    }
                    self.reviews.quit = true;
                    return;
                }
                _ => {}
            }
        }
        if self.agents_active {
            self.agents.key(key);
        } else {
            self.reviews.key_event(key);
        }
    }
    pub fn mouse(&mut self, mouse: MouseEvent) {
        if mouse.row == 0 && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if mouse.column < 21 {
                self.switch(true);
            } else if mouse.column < 45 {
                self.switch(false);
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
        for (x, width, label, active) in [
            (0, 21, " 1 Agents · Ctrl+1 ", self.agents_active),
            (21, 24, " 2 Reviews · Ctrl+2 ", !self.agents_active),
        ] {
            frame.render_widget(
                Paragraph::new(label).style(if active {
                    Style::default().bg(ACCENT).fg(crate::ui::INK)
                } else {
                    Style::default().fg(DIM).bg(BG)
                }),
                Rect::new(x, 0, width.min(frame.area().width.saturating_sub(x)), 1),
            );
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
