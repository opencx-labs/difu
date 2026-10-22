use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Clone, Default)]
struct Transport {
    desktop: Arc<AtomicUsize>,
    sound: Arc<AtomicUsize>,
    authorization: Arc<AtomicUsize>,
}
impl Delivery for Transport {
    fn status(&mut self) -> Result<Capabilities> {
        Ok(Capabilities {
            desktop: "Ready".into(),
            sound: "Ready".into(),
            error: None,
        })
    }
    fn authorize(&mut self) -> Result<Capabilities> {
        self.authorization.fetch_add(1, Ordering::SeqCst);
        self.status()
    }
    fn desktop(&mut self, event: &Event) -> Result<()> {
        assert_eq!(event.title, "Example");
        self.desktop.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn sound(&mut self) -> Result<()> {
        self.sound.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn storage(temp: &tempfile::TempDir) -> Storage {
    Storage {
        config: temp.path().join("config.json"),
        cache: temp.path().join("cache"),
    }
}
fn event(n: u32) -> Event {
    Event {
        identity: format!("one-{n}"),
        session: "one".into(),
        title: "Example".into(),
        kind: Kind::Completion,
    }
}
fn poll(
    dispatcher: &Dispatcher,
    client: &str,
    focused: bool,
    viewed: Option<&str>,
    cursor: Option<Cursor>,
) -> Result<Snapshot> {
    dispatcher.request(Command::Poll {
        client: client.into(),
        focused,
        viewed: viewed.map(String::from),
        cursor,
    })
}
#[test]
fn quiet_keeps_in_app_and_consumes_event_without_native_or_sound() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    storage.update_config(|c| {
        c.preferences.notifications.quiet = true;
        c.preferences.notifications.sound = true;
        c.preferences.notifications.delivery = NotificationDelivery::Both;
    })?;
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage.clone(), transport.clone())?;
    let first = poll(&dispatcher, "a", false, None, None)?;
    dispatcher.emit(vec![event(1)]);
    dispatcher.request(Command::Status)?;
    let feed = poll(&dispatcher, "a", false, None, first.cursor.clone())?;
    assert_eq!(feed.events.len(), 1);
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    assert_eq!(transport.sound.load(Ordering::SeqCst), 0);
    storage.update_config(|c| c.preferences.notifications.quiet = false)?;
    dispatcher.emit(vec![event(1)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    Ok(())
}
#[test]
fn sound_only_test_never_creates_a_desktop_toast_or_requests_permission() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage(&temp), transport.clone())?;
    dispatcher.request(Command::Test {
        preferences: Notifications {
            sound: true,
            ..Default::default()
        },
        event: None,
    })?;
    assert_eq!(transport.sound.load(Ordering::SeqCst), 1);
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    assert_eq!(transport.authorization.load(Ordering::SeqCst), 0);
    Ok(())
}
#[test]
fn background_and_unfocused_scopes_respect_multiple_clients_expiry_and_close() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    storage.update_config(|c| c.preferences.notifications.delivery = NotificationDelivery::Both)?;
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage.clone(), transport.clone())?;
    poll(&dispatcher, "one", true, Some("one"), None)?;
    poll(&dispatcher, "two", true, Some("two"), None)?;
    dispatcher.emit(vec![event(1)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    dispatcher.request(Command::Close {
        client: "one".into(),
    })?;
    dispatcher.emit(vec![event(2)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 1);
    storage
        .update_config(|c| c.preferences.notifications.scope = NotificationScope::AppUnfocused)?;
    dispatcher.emit(vec![event(3)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 1);
    {
        let mut state = dispatcher
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?;
        for presence in state.clients.values_mut() {
            presence.updated = Instant::now() - LEASE;
        }
    }
    dispatcher.emit(vec![event(4)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 2);
    poll(&dispatcher, "one", true, None, None)?;
    dispatcher.request(Command::Close {
        client: "one".into(),
    })?;
    dispatcher.emit(vec![event(5)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 3);
    Ok(())
}
#[test]
fn no_historical_feed_or_delivery_replay_after_reconnect_restart_or_disabled_toggle() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    storage.update_config(|c| c.preferences.notifications.completion = false)?;
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage.clone(), transport.clone())?;
    let initial = poll(&dispatcher, "one", false, None, None)?;
    dispatcher.emit(vec![event(1)]);
    dispatcher.request(Command::Status)?;
    storage.update_config(|c| {
        c.preferences.notifications.completion = true;
        c.preferences.notifications.delivery = NotificationDelivery::Both;
    })?;
    dispatcher.emit(vec![event(1), event(2), event(2)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 1);
    assert_eq!(
        poll(&dispatcher, "one", false, None, initial.cursor.clone())?
            .events
            .len(),
        1
    );
    assert!(
        poll(&dispatcher, "new-client", false, None, None)?
            .events
            .is_empty()
    );
    drop(dispatcher);
    let restarted = Dispatcher::start(storage, transport.clone())?;
    restarted.emit(vec![event(1), event(2)]);
    restarted.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 1);
    assert!(
        poll(&restarted, "one", false, None, initial.cursor)?
            .events
            .is_empty()
    );
    Ok(())
}
#[test]
fn real_store_reducer_path_is_async_deduplicated_and_privacy_limited() -> Result<()> {
    use crate::agents::{Job, Launch, Session, engine, server::Store};
    let temp = tempfile::tempdir()?;
    let transport = Transport::default();
    let mut session = Session::new(
        "one".into(),
        Job::Coding(Launch {
            repository: temp.path().into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Example".into(),
            model: None,
            effort: None,
        }),
    );
    session.note("userMessage", "PRIVATE TRANSCRIPT");
    let mut store = Store::fixture(temp.path().into(), vec![session]);
    store.notifications = Some(Dispatcher::start(store.storage.clone(), transport.clone())?);
    let dispatcher = store.notifications.as_ref().context("dispatcher")?;
    let cursor = poll(dispatcher, "one", false, None, None)?.cursor;
    for (id, outcome) in [
        ("a", "completed"),
        ("b", "failed"),
        ("c", "interrupted"),
        ("d", "completed"),
    ] {
        store.update("one", |s| {
            engine::apply_event(
                s,
                &serde_json::json!({"method":"turn/started","params":{"turn":{"id":id}}}),
            )
        })?;
        for _ in 0..2 {
            store.update("one",|s|engine::apply_event(s,&serde_json::json!({"method":"turn/completed","params":{"turn":{"id":id,"status":outcome,"error":"PRIVATE ERROR"}}})))?;
        }
    }
    dispatcher.request(Command::Status)?;
    let snapshot = poll(dispatcher, "one", false, None, cursor)?;
    assert_eq!(
        snapshot.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![Kind::Completion, Kind::Error, Kind::Completion]
    );
    assert!(!serde_json::to_string(&snapshot)?.contains("PRIVATE"));
    assert_eq!(transport.authorization.load(Ordering::SeqCst), 0);
    Ok(())
}
#[test]
fn event_toggles_all_scope_soundoff_and_inapp_sound_use_saved_settings() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage.clone(), transport.clone())?;
    poll(&dispatcher, "one", true, Some("one"), None)?;
    storage.update_config(|c| {
        let n = &mut c.preferences.notifications;
        n.scope = NotificationScope::All;
        n.sound = true;
    })?;
    dispatcher.emit(vec![event(1)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.sound.load(Ordering::SeqCst), 1);
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    storage.update_config(|c| {
        let n = &mut c.preferences.notifications;
        n.sound = false;
        n.delivery = NotificationDelivery::Desktop;
        n.question_approval = false;
        n.error = false;
    })?;
    let mut question = event(2);
    question.kind = Kind::Question;
    let mut error = event(3);
    error.kind = Kind::Error;
    dispatcher.emit(vec![question, error, event(4)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.sound.load(Ordering::SeqCst), 1);
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 1);
    Ok(())
}
#[test]
fn question_request_ids_can_be_reused_in_subsequent_turns() -> Result<()> {
    use crate::agents::{Job, Launch, Pending, Session, server::Store};
    let temp = tempfile::tempdir()?;
    let session = Session::new(
        "one".into(),
        Job::Coding(Launch {
            repository: temp.path().into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Example".into(),
            model: None,
            effort: None,
        }),
    );
    let mut store = Store::fixture(temp.path().into(), vec![session]);
    store.notifications = Some(Dispatcher::start(
        store.storage.clone(),
        Transport::default(),
    )?);
    let dispatcher = store.notifications.as_ref().context("dispatcher")?;
    let cursor = poll(dispatcher, "one", false, None, None)?.cursor;
    for turn in ["one", "two"] {
        store.update("one", |s| {
            s.pending.clear();
            s.turn_id = Some(turn.into());
        })?;
        store.update("one", |s| {
            s.pending.push(Pending {
                id: serde_json::json!(1),
                method: "item/tool/requestUserInput".into(),
                params: serde_json::json!({}),
                responded: false,
            })
        })?;
    }
    dispatcher.request(Command::Status)?;
    assert_eq!(
        poll(dispatcher, "one", false, None, cursor)?.events.len(),
        2
    );
    Ok(())
}
#[test]
fn blocked_native_delivery_never_holds_session_or_presence_locks() -> Result<()> {
    use crate::agents::{Job, Launch, Session, Status, engine, server::Store};
    struct Blocking {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl Delivery for Blocking {
        fn status(&mut self) -> Result<Capabilities> {
            Ok(Capabilities::default())
        }
        fn authorize(&mut self) -> Result<Capabilities> {
            self.status()
        }
        fn desktop(&mut self, _: &Event) -> Result<()> {
            self.started.send(())?;
            self.release.recv_timeout(Duration::from_secs(3))?;
            Ok(())
        }
        fn sound(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let temp = tempfile::tempdir()?;
    let mut session = Session::new(
        "one".into(),
        Job::Coding(Launch {
            repository: temp.path().into(),
            isolated: false,
            base: "HEAD".into(),
            prompt: "Example".into(),
            model: None,
            effort: None,
        }),
    );
    session.status = Status::Running;
    let mut store = Store::fixture(temp.path().into(), vec![session]);
    store
        .storage
        .update_config(|c| c.preferences.notifications.delivery = NotificationDelivery::Both)?;
    let (started, ready) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    store.notifications = Some(Dispatcher::start(
        store.storage.clone(),
        Blocking {
            started,
            release: resume,
        },
    )?);
    let store = Arc::new(store);
    store.update("one",|s|engine::apply_event(s,&serde_json::json!({"method":"turn/completed","params":{"turn":{"id":"finished","status":"completed"}}})))?;
    ready.recv_timeout(Duration::from_secs(2))?;
    let updating = store.clone();
    let (done, finished) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            updating.update("one", |s| s.title = "New title".into())?;
            updating.save("one")?;
            poll(
                updating.notifications.as_ref().context("dispatcher")?,
                "live",
                true,
                Some("one"),
                None,
            )?;
            Ok(())
        })();
        let _ = done.send(result);
    });
    let result = finished.recv_timeout(Duration::from_millis(500));
    let _ = release.send(());
    worker.join().map_err(|_| anyhow::anyhow!("worker"))?;
    result.context("Native IO blocked session/presence progress")??;
    Ok(())
}
#[test]
fn corrupt_dispatch_markers_disable_delivery_and_remain_visible_on_status() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    std::fs::create_dir_all(temp.path().join("agents/notifications"))?;
    std::fs::write(
        temp.path().join("agents/notifications/dispatched.json"),
        b"bad JSON",
    )?;
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage, transport.clone())?;
    dispatcher.emit(vec![event(1)]);
    let status = dispatcher.request(Command::Status)?;
    assert!(
        status
            .capabilities
            .error
            .is_some_and(|e| e.contains("deduplication"))
    );
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    Ok(())
}
#[test]
fn guide_and_conflict_completion_edges_have_durable_distinct_identities() -> Result<()> {
    use crate::agents::{Job, Session, Status, server::Store};
    let temp = tempfile::tempdir()?;
    let session = Session::new(
        "one".into(),
        Job::Conflict {
            root: temp.path().into(),
            key: crate::model::PrKey {
                owner: "owner".into(),
                repo: "repo".into(),
                number: 1,
            },
            head: "head".into(),
            model: Default::default(),
        },
    );
    let mut store = Store::fixture(temp.path().into(), vec![session]);
    store.notifications = Some(Dispatcher::start(
        store.storage.clone(),
        Transport::default(),
    )?);
    let dispatcher = store.notifications.as_ref().context("dispatcher")?;
    let cursor = poll(dispatcher, "a", false, None, None)?.cursor;
    store.update("one", |s| s.status = Status::Completed)?;
    store.update("one", |s| s.title = "New title".into())?;
    store.save("one")?;
    let restored: Session = serde_json::from_slice(&std::fs::read(temp.path().join("one.json"))?)?;
    assert!(restored.version > 0);
    store.update("one", |s| s.status = Status::Running)?;
    store.update("one", |s| s.status = Status::Completed)?;
    dispatcher.request(Command::Status)?;
    let feed = poll(dispatcher, "a", false, None, cursor)?;
    assert_eq!(feed.events.len(), 2);
    assert_ne!(
        feed.events.first().map(|e| &e.identity),
        feed.events.last().map(|e| &e.identity)
    );
    Ok(())
}
#[test]
fn opening_status_preserves_the_last_native_delivery_failure() -> Result<()> {
    struct Fails;
    impl Delivery for Fails {
        fn status(&mut self) -> Result<Capabilities> {
            Ok(Capabilities {
                desktop: "Available".into(),
                sound: "Unavailable".into(),
                error: None,
            })
        }
        fn authorize(&mut self) -> Result<Capabilities> {
            self.status()
        }
        fn desktop(&mut self, _: &Event) -> Result<()> {
            anyhow::bail!("Notification server refused delivery")
        }
        fn sound(&mut self) -> Result<()> {
            anyhow::bail!("Sound unavailable")
        }
    }
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    storage.update_config(|c| c.preferences.notifications.delivery = NotificationDelivery::Both)?;
    let dispatcher = Dispatcher::start(storage, Fails)?;
    dispatcher.emit(vec![event(1)]);
    let snapshot = dispatcher.request(Command::Status)?;
    assert!(
        snapshot
            .capabilities
            .error
            .is_some_and(|e| e.contains("refused delivery"))
    );
    Ok(())
}
#[test]
fn saved_presets_and_reset_or_discard_drive_real_event_policy() -> Result<()> {
    use crate::settings::{SettingId, SettingValue};
    let temp = tempfile::tempdir()?;
    let storage = storage(&temp);
    let transport = Transport::default();
    let dispatcher = Dispatcher::start(storage.clone(), transport.clone())?;
    let cursor = poll(&dispatcher, "visible", true, Some("one"), None)?.cursor;
    let mut draft = storage.settings_draft()?;
    draft.set(
        SettingId::NotificationPreset,
        SettingValue::Choice("every_update".into()),
    )?;
    dispatcher.emit(vec![event(1)]);
    dispatcher.request(Command::Status)?;
    assert!(
        poll(&dispatcher, "visible", true, Some("one"), cursor.clone())?
            .events
            .is_empty()
    );
    storage.save_settings_draft(&mut draft)?;
    dispatcher.emit(vec![event(2)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(
        poll(&dispatcher, "visible", true, Some("one"), cursor.clone())?
            .events
            .len(),
        1
    );
    draft.set(
        SettingId::NotificationPreset,
        SettingValue::Choice("quiet".into()),
    )?;
    draft.set(
        SettingId::NotificationDelivery,
        SettingValue::Choice("desktop".into()),
    )?;
    storage.save_settings_draft(&mut draft)?;
    dispatcher.emit(vec![event(3)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(
        poll(&dispatcher, "visible", true, Some("one"), cursor.clone())?
            .events
            .len(),
        2
    );
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    draft.reset(&SettingId::NotificationPreset)?;
    draft.discard(storage.load_config()?);
    dispatcher.emit(vec![event(4)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(transport.desktop.load(Ordering::SeqCst), 0);
    draft.reset(&SettingId::NotificationPreset)?;
    storage.save_settings_draft(&mut draft)?;
    dispatcher.emit(vec![event(5)]);
    dispatcher.request(Command::Status)?;
    assert_eq!(
        poll(&dispatcher, "visible", true, Some("one"), cursor)?
            .events
            .len(),
        3
    );
    assert_eq!(transport.authorization.load(Ordering::SeqCst), 0);
    Ok(())
}
