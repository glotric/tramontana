//! Timestamps.
//!
//! `CORE_CONTRACTS.md` §2 requires UTC RFC 3339 strings with subsecond
//! precision where available. The value is carried as an opaque string rather
//! than a parsed instant: core never does arithmetic on it, every producer is
//! a [`Clock`](crate::ports::Clock), and keeping it textual avoids pulling a
//! date library into the domain to no purpose.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An instant, serialized as a UTC RFC 3339 string.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Timestamp {
    /// Wrap an RFC 3339 string produced by a [`Clock`](crate::ports::Clock).
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the RFC 3339 representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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
    fn timestamps_serialize_as_bare_strings() {
        let timestamp = Timestamp::new("2026-09-07T12:00:00.000Z");
        assert_eq!(
            serde_json::to_string(&timestamp).unwrap(),
            "\"2026-09-07T12:00:00.000Z\""
        );
    }

    #[test]
    fn timestamps_order_lexicographically_which_matches_chronologically() {
        // RFC 3339 in UTC with a fixed shape sorts correctly as text, which is
        // what makes an event stream orderable without parsing.
        let earlier = Timestamp::new("2026-09-07T12:00:00.000Z");
        let later = Timestamp::new("2026-09-07T12:00:01.000Z");
        assert!(earlier < later);
    }
}
