//! An isolated real composer. Only opening/closing a sample question is dispatched;
//! provider, approval, send, interrupt and microphone actions never cross this facade.
use super::*;
use crate::shortcuts::{Bindings, contexts, ids};
use ratatui::{Terminal, backend::TestBackend};

pub(crate) struct QuestionPreview {
    ui: Ui,
}
impl QuestionPreview {
    pub(crate) fn new(config: &Config) -> Self {
        let mut config = config.clone();
        config.voice_enabled = false;
        config.preferences.motion.level = crate::settings::Motion::Off;
        let mut ui = Ui::new(
            Storage {
                config: "/synthetic/difu-shortcut-test/config.json".into(),
                cache: "/synthetic/difu-shortcut-test/cache".into(),
            },
            &config,
        );
        let mut session = Session::new(
            "sample-question".into(),
            Job::Coding(Launch {
                repository: "/synthetic/difu-shortcut-test".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "Sample".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.pending.push(Pending {
            id: serde_json::json!("sample-request"),
            method: "item/tool/requestUserInput".into(), responded: false,
            params: serde_json::json!({"questions":[{"id":"scope","question":"Which sample scope should be used?","options":[{"label":"Small sample"},{"label":"Larger sample"}]}]}),
        });
        ui.selected = Some(session.id.clone());
        ui.positions.entry(session.id.clone()).or_default().draft =
            Editor::from("An unsent sample draft");
        ui.sessions.insert(session.id.clone(), session);
        ui.drilled = true;
        ui.focus = Focus::Composer;
        Self { ui }
    }
    pub(crate) fn key(&mut self, event: KeyEvent) {
        if event.kind != crossterm::event::KeyEventKind::Press {
            return;
        }
        if (self.ui.modal.is_none()
            && Bindings::new(&self.ui.shortcut_config).matches(
                ids::QUESTION_OPEN,
                contexts::AGENT_COMPOSER,
                event,
            ))
            || (self.ui.modal.is_some() && event.code == KeyCode::Esc)
        {
            self.ui.key(event);
        }
    }
    pub(crate) fn render(&mut self, width: u16) -> Vec<Line<'static>> {
        let mut terminal = match Terminal::new(TestBackend::new(width.max(1), 10)) {
            Ok(terminal) => terminal,
            Err(error) => match error {},
        };
        if terminal.draw(|frame| {
            if self.ui.inline_question() {
                self.ui.draw_inline_question(frame, frame.area());
            } else {
                frame.render_widget(Paragraph::new("Safe question sample: press its draft shortcut.\nAn unsent sample draft\nNo send, approve, interrupt or microphone action runs."), frame.area());
            }
        }).is_err() { return Vec::new(); }
        terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width.max(1)))
            .map(|cells| {
                Line::from(
                    cells
                        .iter()
                        .map(|cell| Span::styled(cell.symbol().to_owned(), cell.style()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }
}
