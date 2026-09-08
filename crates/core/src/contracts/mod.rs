//! Canonical serializable contract types.
//!
//! `docs/CORE_CONTRACTS.md` is normative for the meaning of everything in this
//! module; these types are the executable source of truth for its shape. The
//! two are changed together.
//!
//! Serialization rules that apply to every type here (`CORE_CONTRACTS.md` §2):
//!
//! - JSON field names and enum values are `camelCase`;
//! - every top-level command, response, event, checkpoint, and export object
//!   carries a `schemaVersion`;
//! - absent is absent: an optional field is omitted, not sent as `null`;
//! - unknown additive fields are ignored, and an unknown *event* kind is read
//!   as unsupported rather than failing the parse;
//! - secrets are never serialized — only an opaque [`SecretRef`].

mod configuration;
mod descriptor;
mod error;
mod event;
mod ids;
mod message;
mod primitives;
mod records;
mod task;
mod time;

pub(crate) use configuration::hex as configuration_hex;
pub use configuration::{
    ConfigurationDiagnostic, ConfigurationKind, ConfigurationSource, DiagnosticSeverity,
    EffectiveConfiguration,
};
pub use descriptor::{
    AgentDescriptor, CheckpointSupport, Health, IntegrationMode, ModelCapability, ModelControl,
    ModelDescriptor, ModelLocation, NetworkControl, ToolControl, ToolDescriptor,
};
pub use error::{ErrorCode, ErrorEnvelope};
pub use event::{
    CancellationOrigin, EventDraft, EventEnvelope, EventPayload, StreamFrame, TaskEventKind,
    TaskUpdate,
};
pub use ids::{
    AgentId, AgentProviderId, ApprovalId, CheckpointId, ConversationId, CorrelationId, EventId,
    InvocationId, MessageId, ModelId, ProjectId, ProviderId, TaskId, WorkerSessionId,
};
pub use message::{MessagePart, MessageRole, MessageStatus, NormalizedMessage};
pub use primitives::{
    DataClass, ExecutionLocation, RiskClass, RoutingMode, SecretRef, SecretStore, SideEffect,
    Visibility,
};
pub use records::{ConversationRecord, ProjectRecord, TaskRecord};
pub use task::{InvalidTransition, TaskSnapshot, TaskState};
pub use time::Timestamp;

/// Version of the schemas exchanged across the IPC and export boundaries.
///
/// Initial executable schemas use version `1`. Bumping this is a breaking
/// change: it requires a migration, regenerated artifacts, and a decision
/// record (`docs/CORE_CONTRACTS.md` §15).
pub const SCHEMA_VERSION: u32 = 1;
