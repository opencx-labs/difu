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
