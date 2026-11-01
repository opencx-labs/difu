use super::*;
use anyhow::{Context, Result};

fn record(history: &mut History, start: Instant, second: u64, cpu_ms: u64) {
    history.record(
        start + Duration::from_secs(second),
        second as i64 * 1000,
        Ok(Duration::from_millis(cpu_ms)),
        [Cost::default(); 5],
        second * 60,
    );
}
#[test]
fn process_percent_uses_elapsed_time_and_can_exceed_one_core() -> Result<()> {
    let start = Instant::now();
    let mut history = History::new(1, 0);
    record(&mut history, start, 0, 100);
    assert_eq!(history.samples.back().context("baseline")?.percent, None);
    record(&mut history, start, 2, 3100);
    let sample = history.samples.back().context("sample")?;
    assert_eq!(sample.percent, Some(150.0));
    assert_eq!(sample.frames as f64 / sample.seconds, 60.0);
    Ok(())
}
#[test]
fn history_is_bounded_by_time_and_count_and_cursor_resets_on_restart() -> Result<()> {
    let start = Instant::now();
    let mut history = History::new(1, 0);
    for second in 0..=400 {
        record(&mut history, start, second, second * 100);
    }
    assert_eq!(history.samples.len(), 300);
    assert_eq!(history.samples.front().context("oldest")?.at_ms, 101_000);
    let cursor = history.snapshot(None).cursor;
    assert!(history.snapshot(cursor).samples.is_empty());
    record(&mut history, start, 401, 40100);
    assert_eq!(history.snapshot(cursor).samples.len(), 1);
    assert_eq!(
        history
            .snapshot(Some(Cursor {
                pid: 2,
                started_ms: 0,
                sequence: 400
            }))
            .samples
            .len(),
        300
    );
    record(&mut history, start, 800, 80100);
    assert_eq!(history.samples.len(), 1);
    Ok(())
}
#[test]
fn unavailable_clocks_and_recovery_do_not_report_false_zero_or_a_spike() -> Result<()> {
    let start = Instant::now();
    let mut history = History::new(1, 0);
    record(&mut history, start, 0, 0);
    history.record(
        start + INTERVAL,
        1000,
        Err("unsupported".into()),
        [Cost::default(); 5],
        0,
    );
    assert!(history.snapshot(None).error.is_some());
    assert_eq!(history.samples.back().context("failed")?.percent, None);
    record(&mut history, start, 2, 1000);
    assert_eq!(
        history.samples.back().context("new baseline")?.percent,
        None
    );
    record(&mut history, start, 3, 1100);
    assert_eq!(
        history.samples.back().context("recovered")?.percent,
        Some(10.0)
    );
    assert!(history.error.is_none());
    Ok(())
}
#[test]
fn warning_requires_strict_threshold_and_thirty_seconds_then_ten_minute_cooldown() {
    let start = Instant::now();
    let mut alert = Alert::default();
    for second in 0..=40 {
        assert!(!alert.update(start + Duration::from_secs(second), Some(25.0)));
    }
    for second in 41..71 {
        assert!(!alert.update(start + Duration::from_secs(second), Some(25.1)));
    }
    assert!(alert.update(start + Duration::from_secs(71), Some(25.1)));
    for second in 72..671 {
        assert!(!alert.update(start + Duration::from_secs(second), Some(100.0)));
    }
    assert!(alert.update(start + Duration::from_secs(671), Some(100.0)));
}
#[test]
fn missing_readings_low_usage_and_sleep_gaps_break_sustained_warning() {
    let start = Instant::now();
    for interruption in [None, Some(0.0), Some(f64::NAN)] {
        let mut alert = Alert::default();
        for second in 0..30 {
            assert!(!alert.update(start + Duration::from_secs(second), Some(30.0)));
        }
        assert!(!alert.update(start + Duration::from_secs(30), interruption));
        for second in 31..61 {
            assert!(!alert.update(start + Duration::from_secs(second), Some(30.0)));
        }
        assert!(alert.update(start + Duration::from_secs(61), Some(30.0)));
    }
    let mut alert = Alert::default();
    assert!(!alert.update(start, Some(90.0)));
    assert!(!alert.update(start + Duration::from_secs(3600), Some(90.0)));
}
#[test]
fn suspend_and_wall_clock_adjustments_break_a_streak_even_if_instant_pauses() {
    let start = Instant::now();
    for new_wall in [3_600_000, -3_600_000] {
        let mut alert = Alert::default();
        for second in 0..30 {
            assert!(!alert.sample(
                start + Duration::from_secs(second),
                second as i64 * 1000,
                Some(50.0),
            ));
        }
        for second in 30..60 {
            assert!(!alert.sample(
                start + Duration::from_secs(second),
                new_wall + (second as i64 - 30) * 1000,
                Some(50.0),
            ));
        }
        assert!(alert.sample(
            start + Duration::from_secs(60),
            new_wall + 30_000,
            Some(50.0)
        ));
    }
}
#[test]
fn service_restart_duplicate_samples_and_stale_data_are_handled() -> Result<()> {
    let start = Instant::now();
    let mut history = History::new(1, 0);
    record(&mut history, start, 0, 0);
    record(&mut history, start, 1, 100);
    let mut process = Process::default();
    process.receive(history.snapshot(None), 1000);
    process.receive(history.snapshot(None), 1000);
    assert_eq!(process.samples.len(), 2);
    assert!(process.current(1000).is_some());
    assert!(process.current(5000).is_none());
    process.error = Some("disconnected".into());
    assert!(process.current(1000).is_none());
    let mut restarted = History::new(2, 1000);
    record(&mut restarted, start, 2, 0);
    process.receive(restarted.snapshot(None), 2000);
    assert_eq!(process.samples.len(), 1);
    assert_eq!(process.cursor.context("cursor")?.pid, 2);
    Ok(())
}
#[test]
fn combined_cpu_requires_fresh_samples_from_both_processes() {
    let start = Instant::now();
    let mut history = History::new(1, 0);
    record(&mut history, start, 0, 0);
    record(&mut history, start, 1, 150);
    let snapshot = history.snapshot(None);
    let mut overview = Overview {
        at_ms: 1000,
        ..Default::default()
    };
    overview.ui.receive(snapshot.clone(), 1000);
    assert!(overview.combined().is_none());
    overview.service.receive(snapshot, 1000);
    assert_eq!(overview.combined(), Some(30.0));
    overview.service.error = Some("disconnected".into());
    assert!(overview.combined().is_none());
}
#[test]
fn native_cpu_clocks_and_nested_scopes_record_cpu_once() -> Result<()> {
    assert!(cpu_time(ClockId::CLOCK_PROCESS_CPUTIME_ID).is_ok());
    assert!(cpu_time(ClockId::CLOCK_THREAD_CPUTIME_ID).is_ok());
    let recorder = Recorder {
        counters: std::array::from_fn(|_| Counter::default()),
        frames: AtomicU64::new(0),
        history: Mutex::new(History::new(1, 0)),
    };
    {
        let _outer = guard(Some(&recorder), Area::Rendering);
        let _nested = guard(Some(&recorder), Area::Persistence);
        let mut value = 1_u64;
        for i in 1..50_000 {
            value = std::hint::black_box(value.wrapping_mul(i));
        }
        std::hint::black_box(value);
    }
    let measured = costs(&recorder);
    let rendering = measured.first().context("rendering")?;
    assert_eq!(rendering.calls, 1);
    assert!(rendering.cpu_ns > 0);
    assert_eq!(
        measured
            .get(Area::Persistence as usize)
            .context("persistence")?
            .calls,
        0
    );
    assert!(!MEASURING.with(Cell::get));
    Ok(())
}
