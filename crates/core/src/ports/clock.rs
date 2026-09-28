use crate::contracts::Timestamp;

/// The source of "now".
///
/// Time is a dependency rather than an ambient call so tests can be
/// deterministic. `TESTING_STRATEGY.md` §11 forbids sleeping in tests; a
/// controllable clock is what makes that possible.
pub trait Clock: Send + Sync {
    /// The current instant, as a UTC RFC 3339 string.
    fn now(&self) -> Timestamp;
}

/// The computer's local wall-clock time, to the minute (ADR 0063).
///
/// What a person means by "at 8:00": schedules are kept and compared in it,
/// so no time-zone rules are needed.
pub trait LocalClock: Send + Sync {
    /// The current local date and time.
    fn local_now(&self) -> crate::contracts::LocalDateTime;
}
