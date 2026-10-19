use super::*;

pub struct Settings {
    fields: Vec<Editor>,
    status: String,
    pub(super) field: usize,
}
#[derive(Default)]
pub(super) struct State {
    saving: Option<mpsc::Receiver<Result<super::super::slack::Settings, String>>>,
}
impl Ui {
    pub(super) fn open_slack(&mut self) {
        if !cfg!(target_os = "macos") {
            self.notice = Some(("Slack is available on macOS in this release".into(), true));
            return;
        }
        let config = match self.storage.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some((format!("{error:#}"), true));
                return;
            }
        };
        let settings = config.slack;
        let status = super::super::server::home(&self.storage)
            .ok()
            .and_then(|home| std::fs::read(home.join("slack/status.json")).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value.get("text").and_then(Value::as_str).map(String::from))
            .unwrap_or_else(|| "Slack is not connected".into());
        self.modal = Some(Modal::Slack(Settings {
            status,
            fields: vec![
                Editor::default(),
                Editor::default(),
                Editor::from(settings.as_ref().map_or("", |s| s.user.as_str())),
                Editor::from(settings.as_ref().map_or("", |s| s.channel.as_str())),
            ],
            field: 0,
        }));
    }
    pub(super) fn save_slack(&mut self) {
        if self.slack.saving.is_some() {
            return;
        }
        let Some(Modal::Slack(form)) = &self.modal else {
            return;
        };
        let values: Vec<_> = form
            .fields
            .iter()
            .map(|e| e.text().trim().to_owned())
            .collect();
        let [bot, app, user, channel] = values.as_slice() else {
            return;
        };
        let (bot, app, user, channel) = (bot.clone(), app.clone(), user.clone(), channel.clone());
        let storage = self.storage.clone();
        let (tx, rx) = mpsc::channel();
        self.slack.saving = Some(rx);
        // Drop the editors (including undo history) as soon as Save is pressed.
        self.modal = None;
        thread::spawn(move || {
            let result = super::super::slack::configure(&storage, bot, app, user, channel)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            crate::repaint::request();
        });
        self.notice = Some((
            "Validating Slack tokens and saving them in Keychain…".into(),
            false,
        ));
    }
    pub(super) fn tick_slack(&mut self) {
        let Some(result) = self.slack.saving.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.slack.saving = None;
        let result = result.and_then(|settings| {
            self.storage
                .update_config(|config| config.slack = Some(settings))
                .map_err(|e| format!("{e:#}"))
        });
        self.notice = Some(match result {
            Ok(_) => (
                "Slack enabled · message difu directly, or mention it in your configured channel"
                    .into(),
                false,
            ),
            Err(error) => (error, true),
        });
    }
    pub(super) fn disable_slack(&mut self) {
        if self.slack.saving.is_some() {
            return;
        }
        let result = self.storage.update_config(|config| {
            if let Some(settings) = &mut config.slack {
                settings.enabled = false;
            }
        });
        self.notice = Some(match result {
            Ok(_) => ("Slack disabled".into(), false),
            Err(error) => (format!("{error:#}"), true),
        });
        self.modal = None;
    }
    pub(super) fn slack_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let Some(Modal::Slack(form)) = &mut self.modal else {
            return;
        };
        match key.code {
            KeyCode::Tab | KeyCode::Down => form.field = (form.field + 1) % 6,
            KeyCode::BackTab | KeyCode::Up => form.field = (form.field + 5) % 6,
            KeyCode::Enter if form.field == 4 => self.save_slack(),
            KeyCode::Enter if form.field == 5 => self.disable_slack(),
            KeyCode::Enter => form.field = (form.field + 1) % 6,
            _ => {
                if let Some(editor) = form.fields.get_mut(form.field) {
                    editor.key(key);
                }
            }
        }
    }
    pub(super) fn slack_paste(&mut self, text: &str) {
        if let Some(Modal::Slack(form)) = &mut self.modal
            && let Some(editor) = form.fields.get_mut(form.field)
        {
            editor.insert(text.trim());
        }
    }
    pub(super) fn draw_slack(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::Slack(form)) = &self.modal else {
            return;
        };
        let field = form.field;
        let fields = form.fields.clone();
        frame.render_widget(Paragraph::new(format!("{}\nTokens are masked and stored in macOS Keychain.\nOnly the member below can control difu. Channel mentions are optional.", form.status)).wrap(Wrap { trim: false }), Rect::new(area.x, area.y, area.width, 4.min(area.height)));
        for (index, (input, label)) in fields
            .iter()
            .zip([
                "Bot token · xoxb-",
                "App token · xapp-",
                "Allowed member ID · U…",
                "Optional channel ID · C… (blank for DMs only)",
            ])
            .enumerate()
        {
            let mut masked = Editor::from("•".repeat(input.chars.len()));
            masked.cursor = input.cursor;
            let rect = Rect::new(
                area.x,
                area.y.saturating_add(3 + index as u16 * 3),
                area.width,
                3,
            );
            if rect.bottom() <= area.bottom() {
                editor(
                    frame,
                    rect,
                    label,
                    if index < 2 { &masked } else { input },
                    field == index,
                );
                self.hits.push((rect, Action::SlackField(index)));
            }
        }
        for (index, label, action) in [
            (4, "Save and connect", Action::SlackSave),
            (5, "Disable Slack", Action::SlackDisable),
        ] {
            let rect = Rect::new(
                area.x,
                area.y.saturating_add(16 + (index - 4) as u16),
                area.width,
                1,
            );
            if rect.bottom() <= area.bottom() {
                self.button(frame, rect, label, action, field == index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn setup_masks_tokens_and_does_not_expose_them_to_copy_or_the_composer() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: temp.path().join("config.json"),
                cache: temp.path().join("cache"),
            },
            &Config::default(),
        );
        assert!(
            ui.palette_commands()
                .iter()
                .any(|(name, _, _)| name == "/slack")
        );
        ui.modal = Some(Modal::Slack(Settings {
            fields: vec![
                Editor::from("xoxb-private-fixture"),
                Editor::from("xapp-private-fixture"),
                Editor::from("UALI"),
                Editor::from("CPRIVATE"),
            ],
            field: 0,
            status: "Not connected".into(),
        }));
        assert!(ui.focused_editor().is_none());
        let mut terminal = Terminal::new(TestBackend::new(100, 24))?;
        terminal.draw(|frame| ui.draw_modal(frame))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!screen.contains("private-fixture"));
        assert!(screen.contains('•'));
        assert!(screen.contains("UALI") && screen.contains("CPRIVATE"));
        assert!(screen.contains("Save and connect"));
        ui.slack_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        assert!(ui.positions.values().all(|p| p.draft.text().is_empty()));
        assert!(!ui.storage.config.exists());
        Ok(())
    }
}
