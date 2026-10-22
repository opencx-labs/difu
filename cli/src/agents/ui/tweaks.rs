//! `/tweaks`: chat animation timing, saved in the difu config.
use super::*;
use crate::storage::Tweaks;

const LIMIT: u32 = 10_000;
const LABELS: [&str; 4] = [
    "Agent reply speed · characters per second",
    "Sent message flips in over · milliseconds",
    "Input clears over · milliseconds",
    "Longest animation · milliseconds (0 = no cap)",
];

impl Ui {
    pub(super) fn open_tweaks(&mut self) {
        let Tweaks {
            agent_reply_cps,
            sent_message_ms,
            input_clear_ms,
            max_animation_ms,
        } = self.tweaks;
        self.modal = Some(Modal::Tweaks {
            fields: Box::new(
                [
                    agent_reply_cps,
                    sent_message_ms,
                    input_clear_ms,
                    max_animation_ms,
                ]
                .map(|value| Editor::from(value.to_string().as_str())),
            ),
            field: 0,
        });
    }
    pub(super) fn tweaks_key(&mut self, key: KeyEvent) {
        use crate::shortcuts::ids::*;
        let shortcut = self.shortcut(key);
        let Some(Modal::Tweaks { fields, field }) = &mut self.modal else {
            return;
        };
        // Four numbers, then Save.
        match shortcut {
            _ if key.code == KeyCode::Esc => self.modal = None,
            Some(NAVIGATION_NEXT_FOCUS) => *field = (*field + 1) % 5,
            Some(NAVIGATION_PREVIOUS_FOCUS) => *field = (*field + 4) % 5,
            Some(NAVIGATION_ACTIVATE) => self.save_tweaks(),
            _ => {
                if let Some(editor) = fields.get_mut(*field) {
                    editor.key(key);
                }
            }
        }
    }
    pub(super) fn save_tweaks(&mut self) {
        let Some(Modal::Tweaks { fields, .. }) = &self.modal else {
            return;
        };
        let values = fields
            .iter()
            .map(|editor| {
                editor
                    .text()
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|v| *v <= LIMIT)
            })
            .collect::<Option<Vec<_>>>();
        let Some(
            &[
                agent_reply_cps,
                sent_message_ms,
                input_clear_ms,
                max_animation_ms,
            ],
        ) = values.as_deref()
        else {
            self.notice = Some((
                format!("Enter whole numbers from 0 to {LIMIT}; 0 turns an animation off"),
                true,
            ));
            return;
        };
        let tweaks = Tweaks {
            agent_reply_cps,
            sent_message_ms,
            input_clear_ms,
            max_animation_ms,
        };
        let saved = self.storage.update_config(|config| config.tweaks = tweaks);
        match saved {
            Ok(_) => {
                self.tweaks = tweaks;
                self.modal = None;
                self.notice = Some(("Tweaks saved".into(), false));
            }
            Err(error) => {
                self.notice = Some((format!("Could not save tweaks: {error:#}"), true));
            }
        }
    }
    pub(super) fn draw_tweaks(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::Tweaks { fields, field }) = &self.modal else {
            return;
        };
        let (fields, field) = (fields.clone(), *field);
        let defaults = Tweaks::default();
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        let hint = |id| bindings.primary_hint_in(id, self.shortcut_context());
        frame.render_widget(
            Paragraph::new(format!(
                "{}/{} fields · {} saves · Esc cancels\nChat text flips in split-flap style. Replies speed up when behind the stream; sent messages and input clearing take a fixed time. 0 turns an animation off.\nDefaults: {} characters per second · {} ms · {} ms · {} ms.",
                hint(crate::shortcuts::ids::NAVIGATION_NEXT_FOCUS),
                hint(crate::shortcuts::ids::NAVIGATION_PREVIOUS_FOCUS),
                hint(crate::shortcuts::ids::NAVIGATION_ACTIVATE),
                defaults.agent_reply_cps, defaults.sent_message_ms, defaults.input_clear_ms, defaults.max_animation_ms
            ))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(DIM)),
            Rect::new(area.x, area.y, area.width, 5.min(area.height)),
        );
        for (index, (editor_value, label)) in fields.iter().zip(LABELS).enumerate() {
            let input = Rect::new(
                area.x,
                area.y.saturating_add(6 + 3 * index as u16),
                area.width,
                3,
            );
            editor(frame, input, label, editor_value, field == index);
            self.hits.push((input, Action::TweaksField(index)));
        }
        self.button(
            frame,
            Rect::new(area.x, area.y.saturating_add(19), area.width, 1),
            "Save tweaks",
            Action::TweaksSave,
            field == 4,
        );
    }
}
