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
