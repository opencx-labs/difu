//! The ease-in-out curve every progress-based animation shares: split-flap
//! sweeps and reveals, panel resizes and collapses start slowly, speed up
//! through the middle and settle gently instead of moving at a constant rate.
//!
//! The curve is smoothstep, `3t² − 2t³`, close to CSS's `ease-in-out`. It is
//! symmetric, so an animation is half done halfway through, and its fastest
//! point is 1.5 times the average speed: a panel that moved one row per frame
//! still never skips more than a row at a time.
use std::time::Duration;

/// Eased progress for linear progress `t`, both from 0 to 1.
pub(super) fn ease(t: f64) -> f64 {
    let t = unit(t);
    t * t * (3.0 - 2.0 * t)
}

/// Eased progress `elapsed` into an animation lasting `duration`. An
/// animation without a duration is complete.
pub(super) fn progress(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    ease(elapsed.as_secs_f64() / duration.as_secs_f64())
}

/// The inverse of [`ease`]: the linear progress at which eased progress
/// reaches `eased`.
pub(super) fn inverse(eased: f64) -> f64 {
    let eased = unit(eased);
    if eased <= 0.0 || eased >= 1.0 {
        return eased;
    }
    unit(0.5 - ((1.0 - 2.0 * eased).asin() / 3.0).sin())
}

/// When, within `duration`, an eased animation reaches `fraction` of its way:
/// item `i` of `n` items spread over `duration` takes its turn at
/// `offset(duration, i / n)`, so items bunch up in the middle of the sweep.
pub(super) fn offset(duration: Duration, fraction: f64) -> Duration {
    let t = inverse(fraction);
    if t <= 0.0 {
        Duration::ZERO
    } else if t >= 1.0 {
        duration
    } else {
        duration.mul_f64(t)
    }
}

/// The steepest start [`ease_from`] allows; steeper curves would overshoot.
const STEEPEST: f64 = 3.0;

/// [`ease`] for an animation already moving when it starts: progress leaves 0
/// with `slope`, in progress per unit of `t`, and still comes to rest at 1.
/// A slope of 0 is [`ease`]; slopes stay within 0 to 3 so progress never
/// overshoots or turns back.
pub(super) fn ease_from(t: f64, slope: f64) -> f64 {
    let t = unit(t);
    ease(t) + steepness(slope) * t * (1.0 - t) * (1.0 - t)
}

/// How fast [`ease_from`] progresses at `t`, in progress per unit of `t`.
pub(super) fn slope_from(t: f64, slope: f64) -> f64 {
    let t = unit(t);
    (6.0 * t * (1.0 - t) + steepness(slope) * (1.0 - t) * (1.0 - 3.0 * t)).max(0.0)
}

/// The inverse of [`ease_from`]: the `t` at which progress reaches `eased`.
pub(super) fn inverse_from(eased: f64, slope: f64) -> f64 {
    let eased = unit(eased);
    if steepness(slope) == 0.0 || eased <= 0.0 || eased >= 1.0 {
        return inverse(eased);
    }
    // `ease_from` rises monotonically, so halving the interval converges.
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..48 {
        let middle = (low + high) / 2.0;
        if ease_from(middle, slope) < eased {
            low = middle;
        } else {
            high = middle;
        }
    }
    high
}

fn steepness(slope: f64) -> f64 {
    if slope.is_nan() {
        0.0
    } else {
        slope.clamp(0.0, STEEPEST)
    }
}

fn unit(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: usize = 1000;

    fn samples() -> impl Iterator<Item = f64> {
        (0..=SAMPLES).map(|step| step as f64 / SAMPLES as f64)
    }

    #[test]
    fn eases_from_start_to_end_through_the_midpoint() -> anyhow::Result<()> {
        anyhow::ensure!(ease(0.0) == 0.0);
        anyhow::ensure!(ease(1.0) == 1.0);
        anyhow::ensure!(ease(0.5) == 0.5);
        anyhow::ensure!(ease(-1.0) == 0.0 && ease(2.0) == 1.0 && ease(f64::NAN) == 0.0);
        anyhow::ensure!(progress(Duration::ZERO, Duration::ZERO) == 1.0);
        let halfway = progress(Duration::from_millis(50), Duration::from_millis(100));
        anyhow::ensure!((halfway - 0.5).abs() < 1e-12);
        anyhow::ensure!(progress(Duration::from_secs(1), Duration::from_millis(100)) == 1.0);
        Ok(())
    }

    #[test]
    fn eased_progress_never_turns_back_and_mirrors_around_the_middle() -> anyhow::Result<()> {
        let mut previous = 0.0;
        for t in samples() {
            let eased = ease(t);
            anyhow::ensure!(eased >= previous, "turned back at {t}");
            anyhow::ensure!(
                (eased + ease(1.0 - t) - 1.0).abs() < 1e-12,
                "not symmetric at {t}"
            );
            previous = eased;
        }
        Ok(())
    }

    #[test]
    fn starts_and_ends_slower_than_linear_and_peaks_at_one_and_a_half() -> anyhow::Result<()> {
        for t in samples().filter(|t| *t > 0.0 && *t < 0.5) {
            anyhow::ensure!(ease(t) < t, "not slower than linear at {t}");
            anyhow::ensure!(ease(1.0 - t) > 1.0 - t, "not settling at {}", 1.0 - t);
        }
        let step = 1.0 / SAMPLES as f64;
        let start = ease(step) / step;
        let end = (1.0 - ease(1.0 - step)) / step;
        anyhow::ensure!(start < 0.01 && end < 0.01, "{start} {end}");
        anyhow::ensure!((slope_from(0.5, 0.0) - 1.5).abs() < 1e-12);
        let fastest = samples().map(|t| slope_from(t, 0.0)).fold(0.0, f64::max);
        anyhow::ensure!(fastest <= 1.5, "{fastest}");
        Ok(())
    }

    #[test]
    fn the_inverse_finds_when_each_fraction_is_reached() -> anyhow::Result<()> {
        anyhow::ensure!(inverse(0.0) == 0.0 && inverse(1.0) == 1.0 && inverse(0.5) == 0.5);
        for fraction in samples() {
            let t = inverse(fraction);
            anyhow::ensure!((ease(t) - fraction).abs() < 1e-9, "{fraction} -> {t}");
        }
        let duration = Duration::from_millis(600);
        anyhow::ensure!(offset(duration, 0.0) == Duration::ZERO);
        anyhow::ensure!(offset(duration, 1.0) == duration);
        let halfway = offset(duration, 0.5).abs_diff(Duration::from_millis(300));
        anyhow::ensure!(halfway < Duration::from_micros(1));
        // Early items wait longer than an even spread, middle ones bunch up.
        anyhow::ensure!(offset(duration, 0.1) > Duration::from_millis(60));
        anyhow::ensure!(offset(duration, 0.9) < Duration::from_millis(540));
        Ok(())
    }

    #[test]
    fn a_moving_start_carries_its_speed_and_still_comes_to_rest() -> anyhow::Result<()> {
        for slope in [0.0, 0.5, 1.0, 2.0, 3.0, 10.0] {
            anyhow::ensure!(ease_from(0.0, slope) == 0.0 && ease_from(1.0, slope) == 1.0);
            anyhow::ensure!((slope_from(0.0, slope) - slope.min(3.0)).abs() < 1e-12);
            anyhow::ensure!(slope_from(1.0, slope) == 0.0);
            let mut previous = 0.0;
            for t in samples() {
                let eased = ease_from(t, slope);
                anyhow::ensure!(eased >= previous && eased <= 1.0, "{slope} at {t}");
                previous = eased;
                let back = inverse_from(eased, slope);
                anyhow::ensure!(
                    (ease_from(back, slope) - eased).abs() < 1e-9,
                    "{slope}: {eased} -> {back}"
                );
            }
        }
        anyhow::ensure!(ease_from(0.25, 0.0) == ease(0.25));
        Ok(())
    }
}
