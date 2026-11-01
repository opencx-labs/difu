//! Local, bounded CPU diagnostics. Native process clocks exclude child processes.
mod client;
#[cfg(test)]
mod tests;
pub(crate) use client::{Client, Overview, Process};

use nix::time::{ClockId, clock_gettime};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::VecDeque,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const INTERVAL: Duration = Duration::from_secs(1);
const RETENTION_MS: i64 = 300_000;
const CAPACITY: usize = 300;
pub(crate) const THRESHOLD: f64 = 25.0;
const SUSTAINED: Duration = Duration::from_secs(30);
const COOLDOWN: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Area {
    Rendering,
    SessionUpdates,
    Persistence,
    TerminalParsing,
    Transport,
}
impl Area {
    pub const ALL: [Self; 5] = [
        Self::Rendering,
        Self::SessionUpdates,
        Self::Persistence,
        Self::TerminalParsing,
        Self::Transport,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Rendering => "Rendering",
            Self::SessionUpdates => "Session updates",
            Self::Persistence => "Persistence",
            Self::TerminalParsing => "Terminal parsing",
            Self::Transport => "Session transport",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Cost {
    pub cpu_ns: u64,
    pub calls: u64,
}
#[derive(Default)]
struct Counter {
    ns: AtomicU64,
    calls: AtomicU64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub sequence: u64,
    pub at_ms: i64,
    pub seconds: f64,
    /// 100% is one core; values may exceed 100%. None means unavailable, never zero.
    pub percent: Option<f64>,
    pub areas: [Cost; 5],
    pub frames: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub pid: u32,
    pub started_ms: i64,
    pub sequence: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub cursor: Option<Cursor>,
    pub samples: Vec<Sample>,
    pub error: Option<String>,
}
struct History {
    pid: u32,
    started_ms: i64,
    sequence: u64,
    previous: Option<(Instant, Duration, [Cost; 5], u64)>,
    samples: VecDeque<Sample>,
    error: Option<String>,
}
impl History {
    fn new(pid: u32, started_ms: i64) -> Self {
        Self {
            pid,
            started_ms,
            sequence: 0,
            previous: None,
            samples: VecDeque::new(),
            error: None,
        }
    }
    fn record(
        &mut self,
        now: Instant,
        at_ms: i64,
        cpu: Result<Duration, String>,
        costs: [Cost; 5],
        frames: u64,
    ) {
        prune(&mut self.samples, at_ms);
        self.sequence = self.sequence.saturating_add(1);
        let mut sample = Sample {
            sequence: self.sequence,
            at_ms,
            seconds: 0.0,
            percent: None,
            areas: [Cost::default(); 5],
            frames: 0,
        };
        match cpu {
            Ok(cpu) => {
                if let Some((at, previous, before, previous_frames)) = self.previous.take() {
                    sample.seconds = now.saturating_duration_since(at).as_secs_f64();
                    if sample.seconds > 0.0 && cpu >= previous {
                        sample.percent =
                            Some((cpu - previous).as_secs_f64() / sample.seconds * 100.0);
                        for ((out, current), previous) in
                            sample.areas.iter_mut().zip(costs).zip(before)
                        {
                            out.cpu_ns = current.cpu_ns.saturating_sub(previous.cpu_ns);
                            out.calls = current.calls.saturating_sub(previous.calls);
                        }
                        sample.frames = frames.saturating_sub(previous_frames);
                    }
                }
                self.error = None;
                self.previous = Some((now, cpu, costs, frames));
            }
            Err(error) => {
                self.error = Some(error);
                self.previous = None;
            }
        }
        self.samples.push_back(sample);
        while self.samples.len() > CAPACITY {
            self.samples.pop_front();
        }
    }
    fn snapshot(&self, after: Option<Cursor>) -> Snapshot {
        let same = after.filter(|c| {
            c.pid == self.pid && c.started_ms == self.started_ms && c.sequence <= self.sequence
        });
        Snapshot {
            cursor: Some(Cursor {
                pid: self.pid,
                started_ms: self.started_ms,
                sequence: self.sequence,
            }),
            samples: self
                .samples
                .iter()
                .filter(|s| same.is_none_or(|c| s.sequence > c.sequence))
                .cloned()
                .collect(),
            error: self.error.clone(),
        }
    }
}
pub(crate) fn prune(samples: &mut VecDeque<Sample>, now_ms: i64) {
    while samples
        .front()
        .is_some_and(|s| now_ms.saturating_sub(s.at_ms) >= RETENTION_MS)
    {
        samples.pop_front();
    }
}
struct Recorder {
    counters: [Counter; 5],
    frames: AtomicU64,
    history: Mutex<History>,
}
static RECORDER: OnceLock<Recorder> = OnceLock::new();

fn cpu_time(clock: ClockId) -> Result<Duration, String> {
    let time = clock_gettime(clock).map_err(|error| format!("CPU clock unavailable: {error}"))?;
    Ok(Duration::new(
        time.tv_sec().max(0) as u64,
        time.tv_nsec().clamp(0, 999_999_999) as u32,
    ))
}
fn costs(recorder: &Recorder) -> [Cost; 5] {
    std::array::from_fn(|index| {
        recorder
            .counters
            .get(index)
            .map(|c| Cost {
                cpu_ns: c.ns.load(Ordering::Relaxed),
                calls: c.calls.load(Ordering::Relaxed),
            })
            .unwrap_or_default()
    })
}
/// Called once by each production process, never by fixture constructors.
pub fn start() {
    if RECORDER.get().is_some() {
        return;
    }
    let recorder = Recorder {
        counters: std::array::from_fn(|_| Counter::default()),
        frames: AtomicU64::new(0),
        history: Mutex::new(History::new(
            std::process::id(),
            chrono::Utc::now().timestamp_millis(),
        )),
    };
    if RECORDER.set(recorder).is_err() {
        return;
    }
    let worker = std::thread::Builder::new()
        .name("difu-cpu".into())
        .spawn(|| {
            loop {
                if let Some(recorder) = RECORDER.get() {
                    let now = Instant::now();
                    let value = cpu_time(ClockId::CLOCK_PROCESS_CPUTIME_ID);
                    recorder
                        .history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .record(
                            now,
                            chrono::Utc::now().timestamp_millis(),
                            value,
                            costs(recorder),
                            recorder.frames.load(Ordering::Relaxed),
                        );
                }
                std::thread::sleep(INTERVAL);
            }
        });
    if let Err(error) = worker
        && let Some(recorder) = RECORDER.get()
    {
        recorder
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error = Some(format!("CPU monitoring could not start: {error}"));
    }
}
pub fn snapshot(after: Option<Cursor>) -> Snapshot {
    RECORDER
        .get()
        .map(|r| {
            r.history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot(after)
        })
        .unwrap_or_else(|| Snapshot {
            error: Some("CPU monitoring is unavailable".into()),
            ..Default::default()
        })
}
pub fn frame() {
    if let Some(recorder) = RECORDER.get() {
        recorder.frames.fetch_add(1, Ordering::Relaxed);
    }
}
thread_local! { static MEASURING: Cell<bool> = const { Cell::new(false) }; }
/// Scoped thread CPU time, excluding blocked I/O and nested scopes. A guard cannot
/// move to another thread. Nested work belongs to its outermost instrumented area.
pub struct Guard<'a> {
    recorder: Option<&'a Recorder>,
    area: Area,
    start: Option<Duration>,
    _thread: PhantomData<Rc<()>>,
}
pub fn measure(area: Area) -> Guard<'static> {
    guard(RECORDER.get(), area)
}
fn guard(recorder: Option<&Recorder>, area: Area) -> Guard<'_> {
    let start = if recorder.is_some() && !MEASURING.with(Cell::get) {
        let start = cpu_time(ClockId::CLOCK_THREAD_CPUTIME_ID).ok();
        if start.is_some() {
            MEASURING.with(|value| value.set(true));
        }
        start
    } else {
        None
    };
    Guard {
        recorder,
        area,
        start,
        _thread: PhantomData,
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        let elapsed = cpu_time(ClockId::CLOCK_THREAD_CPUTIME_ID)
            .ok()
            .and_then(|end| end.checked_sub(start));
        MEASURING.with(|value| value.set(false));
        if let Some(elapsed) = elapsed
            && let Some(counter) = self
                .recorder
                .and_then(|r| r.counters.get(self.area as usize))
        {
            counter.ns.fetch_add(
                elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
            counter.calls.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
pub(crate) struct Alert {
    high: Duration,
    previous: Option<Instant>,
    last_warning: Option<Instant>,
}
impl Alert {
    /// Count only consecutive measured intervals; missing samples and sleep gaps
    /// must not turn a stale high reading into a sustained-CPU warning.
    pub fn update(&mut self, now: Instant, percent: Option<f64>) -> bool {
        if !percent.is_some_and(|p| p.is_finite() && p > THRESHOLD) {
            self.high = Duration::ZERO;
            self.previous = None;
            return false;
        }
        let gap = self
            .previous
            .replace(now)
            .map(|at| now.saturating_duration_since(at));
        self.high = match gap {
            Some(gap) if gap <= Duration::from_secs(3) => self.high.saturating_add(gap),
            _ => Duration::ZERO,
        };
        if self.high < SUSTAINED
            || self
                .last_warning
                .is_some_and(|at| now.saturating_duration_since(at) < COOLDOWN)
        {
            return false;
        }
        self.last_warning = Some(now);
        self.high = Duration::ZERO;
        true
    }
}
