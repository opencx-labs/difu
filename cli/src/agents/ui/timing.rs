//! The timing every progress-based animation shares: split-flap sweeps and
//! reveals, panel resizes and collapses all move at a constant rate.
use std::time::Duration;

/// How long every panel, card and question takes per row to reveal or hide, so
/// opening and closing match wherever they happen.
const REVEAL_ROW_MS: u32 = 10;

/// How long `rows` take to reveal or hide, within the animation cap.
pub(super) fn reveal(rows: usize, tweaks: crate::storage::Tweaks) -> Duration {
    let ms = u32::try_from(rows)
        .unwrap_or(u32::MAX)
        .saturating_mul(REVEAL_ROW_MS);
    Duration::from_millis(u64::from(tweaks.capped(ms)))
}

/// Progress `elapsed` into an animation lasting `duration`, from 0 to 1. An
/// animation without a duration is complete.
pub(super) fn progress(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f64() / duration.as_secs_f64()).min(1.0)
}

/// When, within `duration`, an animation reaches `fraction` of its way: item
/// `i` of `n` items spread over `duration` takes its turn at
/// `offset(duration, i / n)`.
pub(super) fn offset(duration: Duration, fraction: f64) -> Duration {
    if fraction >= 1.0 {
        duration
    } else if fraction > 0.0 {
        duration.mul_f64(fraction)
    } else {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Tweaks;

    #[test]
    fn progress_moves_at_a_constant_rate() -> anyhow::Result<()> {
        let duration = Duration::from_millis(100);
        anyhow::ensure!(progress(Duration::ZERO, Duration::ZERO) == 1.0);
        anyhow::ensure!(progress(Duration::ZERO, duration) == 0.0);
        for ms in [10, 25, 50, 75, 90] {
            let reached = progress(Duration::from_millis(ms), duration);
            anyhow::ensure!((reached - ms as f64 / 100.0).abs() < 1e-12, "{ms}");
        }
        anyhow::ensure!(progress(Duration::from_secs(1), duration) == 1.0);
        Ok(())
    }

    #[test]
    fn items_take_their_turns_evenly() -> anyhow::Result<()> {
        let duration = Duration::from_millis(600);
        anyhow::ensure!(offset(duration, 0.0) == Duration::ZERO);
        anyhow::ensure!(offset(duration, f64::NAN) == Duration::ZERO);
        anyhow::ensure!(offset(duration, -1.0) == Duration::ZERO);
        anyhow::ensure!(offset(duration, 1.0) == duration);
        anyhow::ensure!(offset(duration, 2.0) == duration);
        let quarter = offset(duration, 0.25).abs_diff(Duration::from_millis(150));
        anyhow::ensure!(quarter < Duration::from_micros(1));
        Ok(())
    }

    #[test]
    fn every_reveal_takes_the_same_time_per_row_within_the_cap() -> anyhow::Result<()> {
        let tweaks = Tweaks::default();
        anyhow::ensure!(reveal(0, tweaks) == Duration::ZERO);
        anyhow::ensure!(reveal(20, tweaks) == Duration::from_millis(200));
        anyhow::ensure!(reveal(40, tweaks) == reveal(20, tweaks) * 2);
        let cap = Duration::from_millis(u64::from(tweaks.max_animation_ms));
        anyhow::ensure!(reveal(10_000, tweaks) == cap);
        let uncapped = Tweaks {
            max_animation_ms: 0,
            ..tweaks
        };
        anyhow::ensure!(reveal(100, uncapped) == Duration::from_millis(1000));
        Ok(())
    }
}
