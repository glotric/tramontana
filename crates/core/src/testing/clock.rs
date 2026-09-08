use std::sync::atomic::{AtomicU64, Ordering};

use crate::contracts::Timestamp;
use crate::ports::Clock;

/// A clock that advances only when told to.
///
/// Every call to [`Clock::now`] returns the current instant and then steps
/// forward by a fixed interval, so a run produces a stable, readable sequence
/// of timestamps without any test ever sleeping.
#[derive(Debug)]
pub struct FakeClock {
    /// Milliseconds since the fixed epoch below.
    elapsed_ms: AtomicU64,
    /// How far each observation advances the clock.
    step_ms: u64,
}

/// The instant a fake run starts. Arbitrary, fixed, and obviously synthetic.
const EPOCH: &str = "2026-01-01T00:00:00";

impl FakeClock {
    /// A clock starting at the fixed epoch, advancing one second per reading.
    #[must_use]
    pub fn new() -> Self {
        Self::with_step_ms(1_000)
    }

    /// A clock advancing `step_ms` per reading.
    #[must_use]
    pub fn with_step_ms(step_ms: u64) -> Self {
        Self {
            elapsed_ms: AtomicU64::new(0),
            step_ms,
        }
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        let elapsed = self.elapsed_ms.fetch_add(self.step_ms, Ordering::SeqCst);
        // Formatted by hand rather than with a date library: the domain never
        // does arithmetic on timestamps, and this keeps the fake's output
        // obviously synthetic.
        let seconds = elapsed / 1_000;
        let millis = elapsed % 1_000;
        let (hours, minutes, secs) = (seconds / 3_600, (seconds / 60) % 60, seconds % 60);
        Timestamp::new(format!("{EPOCH}Z").replace(
            "00:00:00",
            &format!("{hours:02}:{minutes:02}:{secs:02}.{millis:03}"),
        ))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_starts_at_the_epoch_and_advances_by_its_step() {
        let clock = FakeClock::new();
        assert_eq!(clock.now().as_str(), "2026-01-01T00:00:00.000Z");
        assert_eq!(clock.now().as_str(), "2026-01-01T00:00:01.000Z");
        assert_eq!(clock.now().as_str(), "2026-01-01T00:00:02.000Z");
    }

    #[test]
    fn timestamps_stay_ordered_across_minutes_and_hours() {
        let clock = FakeClock::with_step_ms(3_600_000);
        let first = clock.now();
        let second = clock.now();
        assert_eq!(first.as_str(), "2026-01-01T00:00:00.000Z");
        assert_eq!(second.as_str(), "2026-01-01T01:00:00.000Z");
        assert!(
            first < second,
            "textual order must match chronological order"
        );
    }
}
