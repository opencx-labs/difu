use super::*;
use crate::{
    agents::notifications::{Delivery, Dispatcher, Event, Snapshot, client::Rpc},
    settings::{Category, SettingId, SettingValue},
};
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl Delivery for Arc<Recorder> {
    fn status(&mut self) -> Result<Capabilities> {
        Ok(Capabilities {
            desktop: "Denied".into(),
            sound: "Available".into(),
            error: None,
        })
    }
    fn authorize(&mut self) -> Result<Capabilities> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .push("authorize".into());
        self.status()
    }
    fn desktop(&mut self, _: &Event) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .push("desktop".into());
        anyhow::bail!("Permission denied")
    }
    fn sound(&mut self) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .push("sound".into());
        Ok(())
    }
}
impl Rpc for Dispatcher {
    fn request(&self, command: Command) -> Result<Snapshot> {
        Dispatcher::request(self, command)
    }
}
fn setup() -> Result<(tempfile::TempDir, Shell, Arc<Recorder>)> {
    let temp = tempfile::tempdir()?;
    let storage = Storage {
        config: temp.path().join("config.json"),
        cache: temp.path().join("cache"),
    };
    let config = storage.load_config()?;
    let recorder = Arc::new(Recorder::default());
    let dispatcher = Dispatcher::start(storage.clone(), recorder.clone())?;
    let mut shell = Shell::new(storage, config, None);
    shell.notification_client =
        crate::agents::notifications::client::Client::with_rpc(Arc::new(dispatcher));
    shell.open_settings(Some(Category::Notifications));
    Ok((temp, shell, recorder))
}
fn settle(shell: &mut Shell) -> Result<()> {
    for _ in 0..50 {
        shell.notification_tick(true);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    Ok(())
}
fn key(shell: &mut Shell, id: crate::shortcuts::ActionId) -> Result<()> {
    let chord = crate::shortcuts::Bindings::new(&shell.reviews.config)
        .effective(id, crate::shortcuts::contexts::SETTINGS)
        .into_iter()
        .next()
        .context("binding")?;
    let code = match chord.key {
        crate::settings::BindingKey::Character(c) => KeyCode::Char(c),
        crate::settings::BindingKey::Function(n) => KeyCode::F(n),
        _ => anyhow::bail!("Unexpected test binding"),
    };
    let mut modifiers = KeyModifiers::NONE;
    if chord.modifiers.control {
        modifiers |= KeyModifiers::CONTROL;
    }
    if chord.modifiers.alt {
        modifiers |= KeyModifiers::ALT;
    }
    if chord.modifiers.shift {
        modifiers |= KeyModifiers::SHIFT;
    }
    if chord.modifiers.super_key {
        modifiers |= KeyModifiers::SUPER;
    }
    shell.key(KeyEvent::new(code, modifiers));
    Ok(())
}
#[test]
fn explicit_successful_save_authorizes_but_draft_discard_refresh_and_noop_do_not() -> Result<()> {
    let (_temp, mut shell, recorder) = setup()?;
    settle(&mut shell)?;
    shell.settings.as_mut().context("panel")?.draft.set(
        SettingId::NotificationDelivery,
        SettingValue::Choice("both".into()),
    )?;
    settle(&mut shell)?;
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty()
    );
    key(&mut shell, crate::shortcuts::ids::SETTINGS_SAVE)?;
    settle(&mut shell)?;
    assert_eq!(
        *recorder.0.lock().map_err(|_| anyhow::anyhow!("lock"))?,
        vec!["authorize"]
    );
    key(&mut shell, crate::shortcuts::ids::SETTINGS_SAVE)?;
    key(&mut shell, crate::shortcuts::ids::SETTINGS_DISCARD)?;
    settle(&mut shell)?;
    assert_eq!(
        *recorder.0.lock().map_err(|_| anyhow::anyhow!("lock"))?,
        vec!["authorize"]
    );
    assert_eq!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .notification_status
            .as_ref()
            .context("status")?
            .desktop,
        "Denied"
    );
    Ok(())
}
#[test]
fn discard_external_enable_failed_save_and_startup_never_prompt() -> Result<()> {
    let (_temp, mut shell, recorder) = setup()?;
    settle(&mut shell)?;
    shell.settings.as_mut().context("panel")?.draft.set(
        SettingId::NotificationDelivery,
        SettingValue::Choice("both".into()),
    )?;
    shell.reviews.storage.update_config(|c| {
        c.preferences.notifications.delivery = crate::settings::NotificationDelivery::Desktop
    })?;
    key(&mut shell, crate::shortcuts::ids::SETTINGS_SAVE)?;
    settle(&mut shell)?;
    assert!(shell.settings.as_ref().context("panel")?.error.is_some());
    key(&mut shell, crate::shortcuts::ids::SETTINGS_DISCARD)?;
    settle(&mut shell)?;
    shell.apply_committed_settings(shell.reviews.storage.load_config()?);
    shell.open_settings(Some(Category::Notifications));
    settle(&mut shell)?;
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty()
    );
    Ok(())
}
#[test]
fn actual_test_action_uses_draft_sound_and_shows_native_denial_without_changing_choices()
-> Result<()> {
    let (_temp, mut shell, recorder) = setup()?;
    let session = crate::agents::Session::new(
        "untouched".into(),
        crate::agents::Job::Coding(crate::agents::Launch {
            repository: shell.reviews.storage.cache.clone(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Private session must remain unchanged".into(),
            model: None,
            effort: None,
        }),
    );
    shell.agents.sessions.insert(session.id.clone(), session);
    shell.agents.selected = Some("untouched".into());
    let sessions_before = serde_json::to_value(&shell.agents.sessions)?;
    settle(&mut shell)?;
    {
        let panel = shell.settings.as_mut().context("panel")?;
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| r.id == SettingId::NotificationSound)
            .context("sound")?;
    }
    shell
        .settings
        .as_mut()
        .context("panel")?
        .draft
        .set(SettingId::NotificationSound, SettingValue::Bool(true))?;
    key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
    settle(&mut shell)?;
    assert_eq!(
        *recorder.0.lock().map_err(|_| anyhow::anyhow!("lock"))?,
        vec!["sound"]
    );
    assert!(
        !shell
            .reviews
            .storage
            .load_config()?
            .preferences
            .notifications
            .sound
    );
    {
        let panel = shell.settings.as_mut().context("panel")?;
        panel.draft.set(
            SettingId::NotificationDelivery,
            SettingValue::Choice("desktop".into()),
        )?;
        panel
            .draft
            .set(SettingId::NotificationSound, SettingValue::Bool(false))?;
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| r.id == SettingId::NotificationDelivery)
            .context("delivery")?;
    }
    key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
    settle(&mut shell)?;
    assert_eq!(
        *recorder.0.lock().map_err(|_| anyhow::anyhow!("lock"))?,
        vec!["sound", "authorize", "desktop"]
    );
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .error
            .as_ref()
            .is_some_and(|e| e.contains("denied"))
    );
    assert_eq!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .draft
            .value(&SettingId::NotificationDelivery),
        SettingValue::Choice("desktop".into())
    );
    assert_eq!(
        serde_json::to_value(&shell.agents.sessions)?,
        sessions_before
    );
    assert_eq!(shell.agents.selected.as_deref(), Some("untouched"));
    Ok(())
}
#[test]
fn preview_respects_soundoff_quiet_and_disabled_selected_event() -> Result<()> {
    let (_temp, mut shell, recorder) = setup()?;
    settle(&mut shell)?;
    {
        let panel = shell.settings.as_mut().context("panel")?;
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| r.id == SettingId::NotificationSound)
            .context("sound")?;
    }
    key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
    settle(&mut shell)?;
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty()
    );
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .notice
            .as_ref()
            .is_some_and(|s| s.contains("off"))
    );
    {
        let panel = shell.settings.as_mut().context("panel")?;
        panel
            .draft
            .set(SettingId::NotificationQuiet, SettingValue::Bool(true))?;
        panel.draft.set(
            SettingId::NotificationDelivery,
            SettingValue::Choice("desktop".into()),
        )?;
        panel.selected = 0;
    }
    key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
    settle(&mut shell)?;
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty()
    );
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .notice
            .as_ref()
            .is_some_and(|s| s.contains("Quiet"))
    );
    {
        let panel = shell.settings.as_mut().context("panel")?;
        panel
            .draft
            .set(SettingId::NotificationQuiet, SettingValue::Bool(false))?;
        panel
            .draft
            .set(SettingId::CompletionNotification, SettingValue::Bool(false))?;
    }
    key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
    settle(&mut shell)?;
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty()
    );
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .notice
            .as_ref()
            .is_some_and(|s| s.contains("disabled"))
    );
    Ok(())
}
#[test]
fn presence_renews_during_continuous_input_and_tracks_review_and_overlay_visibility() -> Result<()>
{
    #[derive(Default)]
    struct PresenceLog(Mutex<Vec<(bool, Option<String>)>>);
    impl Rpc for PresenceLog {
        fn request(&self, command: Command) -> Result<Snapshot> {
            if let Command::Poll {
                focused, viewed, ..
            } = command
            {
                self.0
                    .lock()
                    .map_err(|_| anyhow::anyhow!("lock"))?
                    .push((focused, viewed));
            }
            Ok(Snapshot::default())
        }
    }
    let (_temp, mut shell, _) = setup()?;
    shell.settings = None;
    let observed = Arc::new(PresenceLog::default());
    shell.notification_client =
        crate::agents::notifications::client::Client::with_rpc(observed.clone());
    shell.agents.selected = Some("visible".into());
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2150);
    while std::time::Instant::now() < deadline {
        shell.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        shell.notification_tick(true);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        observed
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .iter()
            .filter(|p| p.0 && p.1.as_deref() == Some("visible"))
            .count()
            >= 2
    );
    shell.agents_active = false;
    settle(&mut shell)?;
    assert_eq!(
        observed
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .last(),
        Some(&(true, None))
    );
    shell.agents_active = true;
    shell.open_settings(Some(Category::Notifications));
    settle(&mut shell)?;
    assert_eq!(
        observed
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .last(),
        Some(&(true, None))
    );
    shell.notification_tick(false);
    std::thread::sleep(std::time::Duration::from_millis(10));
    shell.notification_tick(false);
    assert_eq!(
        observed
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .last(),
        Some(&(false, None))
    );
    Ok(())
}

#[test]
fn saved_changes_visibility_suppresses_only_the_conversation_actually_drawn() -> Result<()> {
    use crate::agents::notifications::{Kind, client::Client};
    let temp = tempfile::tempdir()?;
    let storage = Storage {
        config: temp.path().join("config.json"),
        cache: temp.path().join("cache"),
    };
    storage.update_config(|config| {
        config.agent_changes_visible = true;
        config.preferences.notifications.delivery = crate::settings::NotificationDelivery::Both;
        config.preferences.notifications.sound = true;
    })?;
    let config = storage.load_config()?;
    let recorder = Arc::new(Recorder::default());
    let dispatcher = Arc::new(Dispatcher::start(storage.clone(), recorder.clone())?);
    let mut shell = Shell::new(storage, config, None);
    shell.notification_client = Client::with_rpc(dispatcher.clone());
    let mut session = crate::agents::Session::new(
        "visible".into(),
        crate::agents::Job::Coding(crate::agents::Launch {
            repository: temp.path().into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: String::new(),
            model: None,
            effort: None,
        }),
    );
    session.note("agentMessage", "VISIBLE CONVERSATION SENTINEL");
    shell.agents.summaries.push(session.summary());
    shell.agents.sessions.insert(session.id.clone(), session);
    shell.agents.selected = Some("visible".into());
    assert!(shell.agents.changes_visible);
    assert!(!shell.agents.drilled);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 35))?;
    terminal.draw(|frame| shell.draw(frame))?;
    let content = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(content.contains("VISIBLE CONVERSATION SENTINEL"));
    settle(&mut shell)?;
    let emit = |identity: &str| {
        dispatcher.emit(vec![Event {
            identity: identity.into(),
            session: "visible".into(),
            title: "Example".into(),
            kind: Kind::Completion,
        }])
    };
    emit("visible-conversation");
    dispatcher.request(Command::Status)?;
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty(),
        "Visible focused conversation must suppress desktop and audio"
    );
    shell.agents.drilled = true;
    terminal.draw(|frame| shell.draw(frame))?;
    let content = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(!content.contains("VISIBLE CONVERSATION SENTINEL"));
    settle(&mut shell)?;
    emit("changes-replaces-conversation");
    dispatcher.request(Command::Status)?;
    assert_eq!(
        *recorder.0.lock().map_err(|_| anyhow::anyhow!("lock"))?,
        vec!["desktop", "sound"]
    );
    Ok(())
}

#[test]
fn explicit_inapp_test_renders_real_toast_without_mutating_existing_notice_or_session() -> Result<()>
{
    let (_temp, mut shell, recorder) = setup()?;
    shell.agents.notice = Some(("Existing notice".into(), true));
    let before_sessions = serde_json::to_value(&shell.agents.sessions)?;
    for width in [40, 140] {
        for (id, message, error) in [
            (
                SettingId::CompletionNotification,
                "Example · Turn completed",
                false,
            ),
            (
                SettingId::ErrorNotification,
                "Example · Session failed",
                true,
            ),
        ] {
            let panel = shell.settings.as_mut().context("panel")?;
            panel.selected = panel
                .rows()
                .iter()
                .position(|row| row.id == id)
                .context("row")?;
            key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
            settle(&mut shell)?;
            let mut actual = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 35))?;
            actual.draw(|frame| shell.draw(frame))?;
            let mut production =
                crate::agents::ui::Ui::new(shell.reviews.storage.clone(), &shell.reviews.config);
            production.notice = Some((message.into(), error));
            let mut expected =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 35))?;
            expected.draw(|frame| production.draw(frame))?;
            let toast_width = (width - 2).min(64);
            for row in 31..34 {
                for column in width - toast_width - 1..width - 1 {
                    assert_eq!(
                        actual.backend().buffer().cell((column, row)),
                        expected.backend().buffer().cell((column, row)),
                        "production toast at {width}x35 ({column},{row})"
                    );
                }
            }
            assert_eq!(shell.agents.notice, Some(("Existing notice".into(), true)));
            assert_eq!(
                serde_json::to_value(&shell.agents.sessions)?,
                before_sessions
            );
        }
    }
    assert!(
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .is_empty()
    );
    assert!(!shell.reviews.storage.config.exists());
    Ok(())
}

#[test]
fn notification_test_expiry_repaints_idle_motion_off_without_an_animation_timer() -> Result<()> {
    use std::time::{Duration, Instant};
    let (_temp, mut shell, _) = setup()?;
    shell.reviews.config.preferences.motion.level = crate::settings::Motion::Off;
    shell.agents.preview_visual_settings(&shell.reviews.config);
    shell
        .settings
        .as_mut()
        .context("panel")?
        .draft
        .set(SettingId::Motion, SettingValue::Choice("off".into()))?;
    key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
    settle(&mut shell)?;
    let now = Instant::now();
    let panel = shell.settings.as_mut().context("panel")?;
    let started = panel.notification_test.as_ref().context("test toast")?.2;
    panel.expire_notification_test(started + Duration::from_millis(4999));
    assert!(panel.notification_test.is_some());
    assert_eq!(shell.redraw_interval(), None);
    let mut schedule = crate::repaint::Schedule::new(now);
    let revision = crate::repaint::revision();
    assert!(schedule.due(now, revision, true, shell.redraw_interval()));
    assert!(!schedule.due(
        now + Duration::from_secs(5),
        revision,
        true,
        shell.redraw_interval()
    ));
    assert_eq!(
        schedule.wait(now, true, shell.redraw_interval()),
        Duration::from_millis(50)
    );
    shell
        .settings
        .as_mut()
        .context("panel")?
        .notification_test
        .as_mut()
        .context("toast")?
        .2 = now - Duration::from_secs(5);
    shell.notification_tick(true);
    assert!(
        shell
            .settings
            .as_ref()
            .context("panel")?
            .notification_test
            .is_none()
    );
    assert!(schedule.due(
        Instant::now(),
        crate::repaint::revision(),
        true,
        shell.redraw_interval()
    ));
    Ok(())
}

#[test]
fn notification_test_delivery_modes_keep_toasts_local_and_preserve_draft_session_and_feed()
-> Result<()> {
    let (_temp, mut shell, recorder) = setup()?;
    settle(&mut shell)?;
    let dispatcher = Arc::new(Dispatcher::start(
        shell.reviews.storage.clone(),
        recorder.clone(),
    )?);
    shell.notification_client =
        crate::agents::notifications::client::Client::with_rpc(dispatcher.clone());
    let poll = |cursor| {
        dispatcher.request(Command::Poll {
            client: "observer".into(),
            focused: false,
            viewed: None,
            cursor,
        })
    };
    let baseline = poll(None)?;
    let session = crate::agents::Session::new(
        "untouched".into(),
        crate::agents::Job::Coding(crate::agents::Launch {
            repository: shell.reviews.storage.cache.clone(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Existing private session".into(),
            model: None,
            effort: None,
        }),
    );
    shell.agents.sessions.insert(session.id.clone(), session);
    shell.agents.selected = Some("untouched".into());
    shell.agents.notice = Some(("Existing notice".into(), true));
    let sessions = serde_json::to_value(&shell.agents.sessions)?;
    for (delivery, quiet, enabled, sound_row, visible, calls) in [
        ("in_app", false, true, false, true, vec!["sound"]),
        (
            "both",
            false,
            true,
            false,
            true,
            vec!["authorize", "desktop", "sound"],
        ),
        ("desktop", true, true, false, true, vec![]),
        (
            "desktop",
            false,
            true,
            false,
            false,
            vec!["authorize", "desktop", "sound"],
        ),
        ("both", false, false, false, false, vec![]),
        ("both", false, true, true, false, vec!["sound"]),
        ("both", true, true, true, false, vec![]),
    ] {
        recorder
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .clear();
        let panel = shell.settings.as_mut().context("panel")?;
        panel.draft.set(
            SettingId::NotificationDelivery,
            SettingValue::Choice(delivery.into()),
        )?;
        panel
            .draft
            .set(SettingId::NotificationQuiet, SettingValue::Bool(quiet))?;
        panel
            .draft
            .set(SettingId::NotificationSound, SettingValue::Bool(true))?;
        panel.draft.set(
            SettingId::CompletionNotification,
            SettingValue::Bool(enabled),
        )?;
        let id = if sound_row {
            SettingId::NotificationSound
        } else {
            SettingId::CompletionNotification
        };
        panel.selected = panel
            .rows()
            .iter()
            .position(|row| row.id == id)
            .context("row")?;
        let draft = serde_json::to_value(panel.draft.config())?;
        key(&mut shell, crate::shortcuts::ids::SETTINGS_TEST)?;
        settle(&mut shell)?;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 35))?;
        terminal.draw(|frame| shell.draw(frame))?;
        let corner = terminal
            .backend()
            .buffer()
            .cell((15, 31))
            .context("toast corner")?;
        assert_eq!(
            corner.symbol() == "┌" && corner.fg == crate::ui::GREEN,
            visible,
            "{delivery},quiet={quiet},enabled={enabled},sound={sound_row}"
        );
        let panel = shell.settings.as_ref().context("panel")?;
        assert_eq!(panel.notification_test.is_some(), visible);
        assert_eq!(serde_json::to_value(panel.draft.config())?, draft);
        assert_eq!(
            *recorder.0.lock().map_err(|_| anyhow::anyhow!("lock"))?,
            calls
        );
        assert_eq!(shell.agents.notice, Some(("Existing notice".into(), true)));
        assert_eq!(serde_json::to_value(&shell.agents.sessions)?, sessions);
        assert_eq!(shell.agents.selected.as_deref(), Some("untouched"));
        let after = poll(baseline.cursor.clone())?;
        assert_eq!(after.cursor, baseline.cursor);
        assert!(after.events.is_empty());
    }
    assert!(!shell.reviews.storage.config.exists());
    Ok(())
}
