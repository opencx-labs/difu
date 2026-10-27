//! Read-only child transcripts in the same movable pane as artifacts and shells.
use super::*;
use crate::agents::subagents::State;

pub(super) struct View {
    pub id: String,
    position: Position,
    window: transcript_window::Window,
    sections: Vec<transcript::Section>,
    total: usize,
    loading: bool,
    checked: Option<Instant>,
    error: Option<String>,
}

impl Ui {
    pub(super) fn open_subagents(&mut self) {
        self.modal = Some(Modal::Subagents { selected: 0 });
    }
    pub(super) fn select_subagent(&mut self, index: usize) {
        let Some(agent) = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .and_then(|s| s.subagents.get(index))
        else {
            return;
        };
        let id = agent.id.clone();
        self.release_terminal_focus();
        self.panels.view = Some(panels::View::Subagent(Box::new(View {
            id,
            position: Position {
                follow: true,
                ..Position::default()
            },
            window: transcript_window::Window::default(),
            sections: Vec::new(),
            total: 0,
            loading: false,
            checked: None,
            error: None,
        })));
        self.panels.session = self.selected.clone();
        self.panels.hidden = false;
        self.panels.focused = true;
        self.modal = None;
    }
    pub(super) fn draw_subagents(&mut self, frame: &mut Frame, area: Rect, selected: usize) {
        let items: Vec<_> = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .into_iter()
            .flat_map(|s| &s.subagents)
            .map(|a| {
                format!(
                    "{} · {} · {}",
                    a.state.label(),
                    a.provider.label(),
                    a.title()
                )
            })
            .collect();
        if items.is_empty() {
            frame.render_widget(Paragraph::new("No sub-agents in this session."), area);
            return;
        }
        let offset = selected.saturating_sub(usize::from(area.height.saturating_sub(1)));
        for (index, item) in items
            .iter()
            .enumerate()
            .skip(offset)
            .take(usize::from(area.height))
        {
            self.button(
                frame,
                Rect::new(area.x, area.y + (index - offset) as u16, area.width, 1),
                &crate::model::clean(item),
                Action::Subagent(index),
                index == selected,
            );
        }
    }
    pub(super) fn subagent_picker_key(&mut self, key: KeyEvent) -> bool {
        use crate::shortcuts::ids::*;
        let Some(Modal::Subagents { selected }) = self.modal else {
            return false;
        };
        let count = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map_or(0, |s| s.subagents.len());
        match self.shortcut(key) {
            _ if key.code == KeyCode::Esc => self.modal = None,
            Some(NAVIGATION_ACTIVATE) => self.select_subagent(selected),
            Some(NAVIGATION_UP) => {
                self.modal = Some(Modal::Subagents {
                    selected: selected.saturating_sub(1),
                })
            }
            Some(NAVIGATION_DOWN) => {
                self.modal = Some(Modal::Subagents {
                    selected: selected.saturating_add(1).min(count.saturating_sub(1)),
                })
            }
            _ => {}
        }
        true
    }
    pub(super) fn tick_subagent(&mut self, visible: bool) {
        if !visible || self.panels.hidden || self.modal.is_some() {
            return;
        }
        let Some(session) = self.selected.as_ref().and_then(|id| self.sessions.get(id)) else {
            return;
        };
        let Some(panels::View::Subagent(view)) = &mut self.panels.view else {
            return;
        };
        let Some(agent) = session.subagents.iter().find(|a| a.id == view.id) else {
            return;
        };
        if agent.provider != provider::Provider::Codex
            || view.loading
            || (view.checked.is_some() && !agent.state.active() && view.error.is_none())
            || view
                .checked
                .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        view.loading = true;
        let (id, agent) = (session.id.clone(), view.id.clone());
        self.task(
            Task::Subagent(id.clone(), agent.clone()),
            Request::Subagent { id, agent },
            false,
        );
    }
    pub(super) fn subagent_loaded(&mut self, id: &str, agent: &str, error: Option<String>) {
        if self.selected.as_deref() == Some(id)
            && let Some(panels::View::Subagent(view)) = &mut self.panels.view
            && view.id == agent
        {
            view.loading = false;
            view.checked = Some(Instant::now());
            view.error = error;
        }
    }
    pub(super) fn draw_subagent(&mut self, frame: &mut Frame, area: Rect) {
        let Some(session) = self.selected.as_ref().and_then(|id| self.sessions.get(id)) else {
            return;
        };
        let Some(panels::View::Subagent(view)) = &mut self.panels.view else {
            return;
        };
        let Some(agent) = session.subagents.iter().find(|a| a.id == view.id) else {
            return;
        };
        let heading = format!("{} · {}", agent.state.label(), agent.title());
        frame.render_widget(
            Paragraph::new(crate::model::clean(&heading)).style(Style::default().fg(ACCENT)),
            Rect::new(area.x, area.y, area.width, area.height.min(1)),
        );
        let mut body = Rect::new(
            area.x,
            area.y.saturating_add(2),
            area.width,
            area.height.saturating_sub(2),
        );
        if let Some(error) = &view.error {
            frame.render_widget(
                Paragraph::new(format!("Could not refresh: {}", crate::model::clean(error)))
                    .style(Style::default().fg(RED)),
                Rect::new(body.x, body.y, body.width, body.height.min(1)),
            );
            body.y = body.y.saturating_add(1);
            body.height = body.height.saturating_sub(1);
        }
        // A lightweight display session lets the normal chat renderer retain its
        // markdown, tool expansion and viewport cache without parent state leaking in.
        let mut display = Session::new(agent.id.clone(), session.job.clone());
        display.provider = agent.provider;
        display.entries = agent.entries.clone();
        display.status = match agent.state {
            State::Starting | State::Running => Status::Running,
            State::Failed => Status::Failed,
            State::Interrupted => Status::Interrupted,
            _ => Status::Completed,
        };
        display.updated = session.updated;
        view.window.appearance = self.preferences.appearance.clone();
        view.window.conversation = self.preferences.conversation.clone();
        let rendered = view.window.render(
            &display,
            &mut view.position,
            body.width,
            body.height,
            self.panels.focused,
        );
        view.total = rendered.total;
        view.sections = rendered.sections;
        if display.entries.is_empty() {
            frame.render_widget(
                Paragraph::new(if view.loading {
                    "Loading sub-agent conversation…"
                } else if agent.state.active() {
                    "Waiting for sub-agent messages…"
                } else {
                    "No messages were recorded for this sub-agent."
                })
                .style(Style::default().fg(DIM)),
                body,
            );
        } else {
            frame.render_widget(
                Paragraph::new(
                    rendered
                        .lines
                        .into_iter()
                        .skip(view.position.conversation.saturating_sub(rendered.start))
                        .take(usize::from(body.height))
                        .collect::<Vec<_>>(),
                ),
                body,
            );
            self.hits.extend(transcript::link_hits(
                &rendered.links,
                body,
                view.position.conversation,
            ));
            for section in &view.sections {
                if let Some(row) = section
                    .row
                    .checked_sub(view.position.conversation)
                    .filter(|row| *row < usize::from(body.height))
                {
                    self.hits.push((
                        Rect::new(body.x, body.y + row as u16, body.width, 1),
                        Action::SubagentEntry(section.id.clone()),
                    ));
                }
            }
        }
    }
    pub(super) fn toggle_subagent_entry(&mut self, id: String) {
        if let Some(panels::View::Subagent(view)) = &mut self.panels.view {
            if !view.position.expanded.remove(&id) {
                view.position.expanded.insert(id.clone());
            }
            view.position.focused_entry = Some(id);
        }
    }
    pub(super) fn subagent_key(&mut self, key: KeyEvent) -> bool {
        use crate::shortcuts::ids::*;
        let shortcut = self.shortcut(key);
        let Some(panels::View::Subagent(view)) = &mut self.panels.view else {
            return false;
        };
        if matches!(
            shortcut,
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS)
        ) {
            return false;
        }
        if shortcut == Some(NAVIGATION_ACTIVATE) {
            let id = view.position.focused_entry.clone().or_else(|| {
                view.sections
                    .iter()
                    .find(|s| s.tool && s.row >= view.position.conversation)
                    .map(|s| s.id.clone())
            });
            if let Some(id) = id {
                self.toggle_subagent_entry(id);
            }
        } else {
            let max = view
                .total
                .saturating_sub(usize::from(self.panels.rect.height.saturating_sub(2)));
            view.position.conversation = shortcut_scroll(
                shortcut,
                view.position.conversation,
                usize::from(self.panels.rect.height),
                max,
            );
            view.position.follow = view.position.conversation >= max;
            view.position.scroll_anchor = None;
        }
        true
    }
    pub(super) fn subagent_mouse(&mut self, event: MouseEvent) -> bool {
        let Some(panels::View::Subagent(view)) = &mut self.panels.view else {
            return false;
        };
        match event.kind {
            MouseEventKind::ScrollUp => {
                view.position.conversation = view.position.conversation.saturating_sub(3);
                view.position.follow = false;
            }
            MouseEventKind::ScrollDown => {
                let max = view
                    .total
                    .saturating_sub(usize::from(self.panels.rect.height.saturating_sub(2)));
                view.position.conversation = view.position.conversation.saturating_add(3).min(max);
                view.position.follow = view.position.conversation >= max;
            }
            _ => return false,
        }
        view.position.scroll_anchor = None;
        true
    }
}
