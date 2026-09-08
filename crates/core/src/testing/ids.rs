use std::sync::atomic::{AtomicU64, Ordering};

use crate::ports::IdSource;

/// An identifier source that counts.
///
/// Produces `task-1`, `event-1`, `event-2`, and so on, per prefix, so a failing
/// test names the exact event rather than a random string that differs on every
/// run.
#[derive(Debug, Default)]
pub struct SequentialIdSource {
    next: AtomicU64,
}

impl SequentialIdSource {
    /// A source starting from 1.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdSource for SequentialIdSource {
    fn next(&self, prefix: &str) -> String {
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{prefix}-{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_sequential_and_readable() {
        let ids = SequentialIdSource::new();
        assert_eq!(ids.next("task"), "task-1");
        assert_eq!(ids.next("event"), "event-2");
        assert_eq!(ids.next("event"), "event-3");
    }

    #[test]
    fn the_counter_is_shared_so_every_identifier_is_unique() {
        // A per-prefix counter would let `task-1` and `event-1` coexist, which
        // reads fine but hides ordering when events are compared across kinds.
        let ids = SequentialIdSource::new();
        let all = [ids.next("a"), ids.next("b"), ids.next("a")];
        assert_eq!(all, ["a-1", "b-2", "a-3"]);
    }
}
