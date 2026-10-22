use super::*;
use crate::agents::notifications::{Capabilities, Command};
impl Shell {
    pub(super) fn notification_action(&mut self, command: Command) {
        if let Err(error) = self.notification_client.action(command)
            && let Some(panel) = &mut self.settings
        {
            panel.error = Some(format!("{error:#}"));
        }
    }
    pub(super) fn test_notification(&mut self) {
        let Some(panel) = &self.settings else { return };
        let mut preferences = panel.draft.config().preferences.notifications.clone();
        use crate::{agents::notifications::Kind, settings::SettingId};
        let event = match panel.rows().get(panel.selected).map(|row| &row.id) {
            Some(SettingId::NotificationSound) => {
                preferences.delivery = crate::settings::NotificationDelivery::InApp;
                None
            }
            Some(SettingId::QuestionNotification) => Some(Kind::Question),
            Some(SettingId::ErrorNotification) => Some(Kind::Error),
            _ => Some(Kind::Completion),
        };
        self.notification_action(Command::Test { preferences, event });
    }
    /// Call every event-loop iteration, including sustained input and motion-off frames.
    pub fn notification_tick(&mut self, focused: bool) {
        let viewed = if self.agents_active && self.settings.is_none() && self.palette.is_none() {
            self.agents.notification_viewed_session()
        } else {
            None
        };
        for (action, reply) in self.notification_client.tick(focused, viewed) {
            match reply {
                Ok(snapshot) => {
                    if let Some(panel) = &mut self.settings {
                        panel.notification_status = Some(snapshot.capabilities.clone());
                        if action {
                            panel.error = snapshot.capabilities.error.clone();
                            panel.notice = snapshot.notice.clone();
                        }
                    }
                    for event in snapshot.events {
                        let message = format!("{} · {}", event.title, event.kind.label());
                        self.agents.notice = Some((
                            message.clone(),
                            event.kind == crate::agents::notifications::Kind::Error,
                        ));
                        self.reviews.notice =
                            if event.kind == crate::agents::notifications::Kind::Error {
                                crate::app::Notice::error(message)
                            } else {
                                crate::app::Notice::info(message)
                            };
                    }
                }
                Err(error) => {
                    if let Some(panel) = &mut self.settings {
                        panel.notification_status = Some(Capabilities {
                            desktop: "Unavailable".into(),
                            sound: "Unavailable".into(),
                            error: Some(format!("{error:#}")),
                        });
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests;
