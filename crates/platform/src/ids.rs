use tramontana_core::ports::IdSource;

/// Identifiers backed by random UUIDs.
///
/// `CORE_CONTRACTS.md` §2 requires identifiers to be *globally* unique. The
/// deterministic source used in tests counts from one in every process, which
/// is exactly what makes tests readable — and exactly why it cannot be used
/// against a durable profile: a second process would reissue `task-1` and
/// collide with the row the first one wrote.
///
/// The prefix is a human-facing hint only. Nothing derives meaning or authority
/// from an identifier's shape.
#[derive(Debug, Default)]
pub struct UuidIdSource;

impl UuidIdSource {
    /// A source of random identifiers.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl IdSource for UuidIdSource {
    fn next(&self, prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_unique_across_calls() {
        let source = UuidIdSource::new();
        let first = source.next("task");
        let second = source.next("task");
        assert_ne!(first, second);
    }

    #[test]
    fn identifiers_carry_their_prefix_for_readability() {
        assert!(UuidIdSource::new().next("task").starts_with("task-"));
    }

    #[test]
    fn two_sources_do_not_collide() {
        // Two processes each construct their own source. A counter-based
        // implementation would hand both the same first identifier.
        let a = UuidIdSource::new();
        let b = UuidIdSource::new();
        assert_ne!(a.next("task"), b.next("task"));
    }
}
