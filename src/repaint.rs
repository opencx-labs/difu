//! Redraw only after input, visible state changes, or a displayed timer advances.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static REVISION: AtomicU64 = AtomicU64::new(0);
pub(crate) fn request() {
    REVISION.fetch_add(1, Ordering::Relaxed);
}
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub struct Schedule {
    dirty: bool,
    revision: u64,
    drawn: Instant,
}
impl Schedule {
    pub fn new(now: Instant) -> Self {
        Self {
            dirty: true,
            revision: 0,
            drawn: now,
        }
    }
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }
    pub fn due(
        &mut self,
        now: Instant,
        revision: u64,
        focused: bool,
        timer: Option<Duration>,
    ) -> bool {
        if !focused {
            return false;
        }
        if !self.dirty
            && revision == self.revision
            && !timer.is_some_and(|interval| now.duration_since(self.drawn) >= interval)
        {
            return false;
        }
        self.dirty = false;
        self.revision = revision;
        self.drawn = now;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_idle_views_sleep_but_input_results_and_focus_return_redraw() {
        let now = Instant::now();
        let mut schedule = Schedule::new(now);
        assert!(schedule.due(now, 0, true, None));
        assert!(!schedule.due(now + Duration::from_secs(60), 0, true, None));
        schedule.invalidate();
        assert!(schedule.due(now, 0, true, None));
        assert!(schedule.due(now, 1, true, None));
        assert!(!schedule.due(now, 2, false, None));
        assert!(schedule.due(now, 2, true, None));
        assert!(!schedule.due(now, 2, true, Some(Duration::from_secs(1))));
        assert!(schedule.due(
            now + Duration::from_secs(1),
            2,
            true,
            Some(Duration::from_secs(1))
        ));
    }
}
