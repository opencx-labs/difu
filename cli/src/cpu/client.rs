use super::*;
use crate::{
    agents::{Reply, Request},
    storage::Storage,
};
use std::sync::{Arc, mpsc};

#[derive(Clone, Default)]
pub(crate) struct Process {
    pub cursor: Option<Cursor>,
    pub samples: VecDeque<Sample>,
    pub error: Option<String>,
}
impl Process {
    pub fn receive(&mut self, snapshot: Snapshot, now_ms: i64) {
        if self.cursor.zip(snapshot.cursor).is_some_and(|(old, new)| {
            old.pid != new.pid || old.started_ms != new.started_ms || new.sequence < old.sequence
        }) {
            self.samples.clear();
        }
        self.cursor = snapshot.cursor;
        self.error = snapshot.error;
        for sample in snapshot.samples {
            if self
                .samples
                .back()
                .is_none_or(|last| sample.sequence > last.sequence)
            {
                self.samples.push_back(sample);
            }
        }
        prune(&mut self.samples, now_ms);
        while self.samples.len() > CAPACITY {
            self.samples.pop_front();
        }
    }
    pub fn current(&self, now_ms: i64) -> Option<&Sample> {
        self.samples.back().filter(|s| {
            self.error.is_none()
                && (0..=3_000).contains(&now_ms.saturating_sub(s.at_ms))
                && s.seconds > 0.0
                && s.seconds <= 3.0
                && s.percent.is_some()
        })
    }
}
#[derive(Clone, Default)]
pub(crate) struct Overview {
    pub ui: Process,
    pub service: Process,
    pub at_ms: i64,
    pub warning: Option<f64>,
}
impl Overview {
    pub fn combined(&self) -> Option<f64> {
        Some(self.ui.current(self.at_ms)?.percent? + self.service.current(self.at_ms)?.percent?)
    }
}
/// One bounded mailbox and one worker, independent of rendering. Reading the
/// existing service never launches or restarts it and never runs shell commands.
pub(crate) struct Client {
    latest: Arc<Mutex<Option<Overview>>>,
    stop: mpsc::Sender<()>,
}
impl Client {
    pub fn new(storage: Storage) -> std::io::Result<Self> {
        let latest: Arc<Mutex<Option<Overview>>> = Arc::default();
        let output = latest.clone();
        let (stop, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("difu-cpu-client".into())
            .spawn(move || {
                let mut overview = Overview::default();
                let mut alert = Alert::default();
                loop {
                    let started = Instant::now();
                    let local = snapshot(overview.ui.cursor);
                    let service = crate::agents::client::request(
                        &storage,
                        Request::Cpu {
                            after: overview.service.cursor,
                        },
                    );
                    overview.at_ms = chrono::Utc::now().timestamp_millis();
                    overview.ui.receive(local, overview.at_ms);
                    let previous_service = overview.service.cursor;
                    match service {
                        Ok(Reply::Cpu(snapshot)) => {
                            overview.service.receive(snapshot, overview.at_ms)
                        }
                        Ok(_) => {
                            overview.service.error = Some(
                                "CPU monitoring requires the matching difu service version".into(),
                            )
                        }
                        Err(error) => overview.service.error = Some(format!("{error:#}")),
                    }
                    prune(&mut overview.service.samples, overview.at_ms);
                    if previous_service
                        .zip(overview.service.cursor)
                        .is_some_and(|(a, b)| a.pid != b.pid || a.started_ms != b.started_ms)
                    {
                        alert.update(Instant::now(), None);
                    }
                    let percent = overview.combined();
                    overview.warning = alert
                        .sample(Instant::now(), overview.at_ms, percent)
                        .then_some(percent)
                        .flatten();
                    {
                        let mut pending = output.lock().unwrap_or_else(|e| e.into_inner());
                        let mut next = overview.clone();
                        // Preserve an unread warning if input/rendering temporarily stalls.
                        next.warning = next
                            .warning
                            .or_else(|| pending.as_ref().and_then(|p| p.warning));
                        *pending = Some(next);
                    }
                    match receiver.recv_timeout(INTERVAL.saturating_sub(started.elapsed())) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })?;
        Ok(Self { latest, stop })
    }
    pub fn take(&self) -> Option<Overview> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}
