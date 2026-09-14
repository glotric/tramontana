//! Domain identifiers.
//!
//! Each identifier is a distinct newtype so the compiler refuses to pass a
//! conversation where a task is expected. `CORE_CONTRACTS.md` §2 requires that
//! identifiers be opaque and that **authorization is never derived from an ID
//! prefix** — an ID says which record, never what may be done to it.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Declare an opaque identifier newtype over `String`.
macro_rules! declare_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an already-generated identifier.
            ///
            /// Identifiers come from an [`IdSource`](crate::ports::IdSource) so
            /// that tests can be deterministic; this constructor exists for the
            /// source and for deserialization, not for ad-hoc invention.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying opaque string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

declare_id!(
    /// A project: one or more user-approved roots and the work done against them.
    ProjectId
);
declare_id!(
    /// A conversation within a project, or a projectless conversation.
    ConversationId
);
declare_id!(
    /// A durable message in a conversation.
    MessageId
);
declare_id!(
    /// A unit of work with its own event stream and exactly one terminal state.
    TaskId
);
declare_id!(
    /// A single durable event.
    EventId
);
declare_id!(
    /// A saved checkpoint of a task.
    CheckpointId
);
declare_id!(
    /// One tool invocation.
    InvocationId
);
declare_id!(
    /// One approval request and its decision.
    ApprovalId
);
declare_id!(
    /// A model or tool provider.
    ProviderId
);
declare_id!(
    /// A model offered by a provider.
    ModelId
);
declare_id!(
    /// A provider of specialist workers.
    AgentProviderId
);
declare_id!(
    /// A specialist worker offered by an agent provider.
    AgentId
);
declare_id!(
    /// One supervised worker session.
    WorkerSessionId
);
declare_id!(
    /// A correlation identifier shared by everything caused by one request.
    CorrelationId
);
declare_id!(
    /// Something a task produced and recorded, such as its changed-file summary.
    ArtifactId
);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_serialize_as_bare_strings() {
        // `serde(transparent)` keeps the wire format an opaque string, so the
        // newtype is a compile-time distinction only and costs nothing on the
        // wire or in a schema.
        let task = TaskId::new("task-1");
        assert_eq!(serde_json::to_string(&task).unwrap(), "\"task-1\"");
        assert_eq!(serde_json::from_str::<TaskId>("\"task-1\"").unwrap(), task);
    }

    #[test]
    fn identifiers_display_as_their_value() {
        assert_eq!(ProjectId::new("p1").to_string(), "p1");
        assert_eq!(ProjectId::new("p1").as_str(), "p1");
    }
}
