use super::*;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use zbus::zvariant::OwnedValue;
#[derive(Default)]
struct Received {
    app: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    suppressed: bool,
    timeout: i32,
}
struct Service {
    received: Arc<Mutex<Vec<Received>>>,
    capabilities: Arc<AtomicUsize>,
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Service {
    fn get_capabilities(&self) -> Vec<String> {
        self.capabilities.fetch_add(1, Ordering::SeqCst);
        vec!["body".into()]
    }
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let suppressed = hints
            .get("suppress-sound")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        self.received
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("lock".into()))?
            .push(Received {
                app: app_name,
                summary,
                body,
                actions,
                suppressed,
                timeout: expire_timeout,
            });
        Ok(1)
    }
}
#[test]
fn private_bus_native_notification_payload_and_missing_bus() -> Result<()> {
    let mode = std::env::var("DIFU_PRIVATE_NOTIFICATION_TEST").unwrap_or_default();
    if mode.is_empty() {
        let name = "agents::notifications::native::linux_tests::private_bus_native_notification_payload_and_missing_bus";
        for mode in ["bus", "missing"] {
            let temp = tempfile::tempdir()?;
            let mut command = if mode == "bus" {
                let mut c = std::process::Command::new("dbus-run-session");
                c.arg("--").arg(std::env::current_exe()?);
                c
            } else {
                let mut c = std::process::Command::new(std::env::current_exe()?);
                c.env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    format!("unix:path={}", temp.path().join("missing-bus").display()),
                );
                c
            };
            command
                .args(["--exact", name, "--nocapture"])
                .env("DIFU_PRIVATE_NOTIFICATION_TEST", mode);
            let output = command
                .output()
                .context("Private notification test requires dbus-run-session")?;
            anyhow::ensure!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return Ok(());
    }
    let received = Arc::new(Mutex::new(Vec::new()));
    let capabilities = Arc::new(AtomicUsize::new(0));
    let connection = if mode == "bus" {
        Some(
            zbus::blocking::connection::Builder::session()?
                .name("org.freedesktop.Notifications")?
                .serve_at(
                    "/org/freedesktop/Notifications",
                    Service {
                        received: received.clone(),
                        capabilities: capabilities.clone(),
                    },
                )?
                .build()?,
        )
    } else {
        None
    };
    let mut native = Native;
    let status = native.status()?;
    let event = Event {
        identity: "turn".into(),
        session: "test".into(),
        title: "Example <&> title".into(),
        kind: super::super::Kind::Question,
    };
    if mode == "bus" {
        assert_eq!(status.desktop, "Available");
        assert_eq!(capabilities.load(Ordering::SeqCst), 1);
        native.desktop(&event)?;
        let rows = received.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        let row = rows.first().context("Notify was not called")?;
        assert_eq!(rows.len(), 1);
        assert_eq!(row.app, "Difu");
        assert_eq!(row.summary, "Example <&> title");
        assert_eq!(row.body, "Question or approval needed");
        assert!(row.actions.is_empty());
        assert!(row.suppressed);
        assert_eq!(row.timeout, 5000);
    } else {
        assert_eq!(status.desktop, "Unavailable");
        assert!(status.error.is_some());
        assert!(native.desktop(&event).is_err());
    }
    drop(connection);
    Ok(())
}
