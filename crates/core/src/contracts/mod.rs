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

mod checkpoint;
mod configuration;
mod context;
mod descriptor;
mod error;
mod event;
mod export;
mod ids;
mod message;
mod model;
mod permission;
mod primitives;
mod records;
mod release;
mod routing;
mod task;
mod time;

pub use checkpoint::{
    CheckpointWorker, PendingAction, PendingActionKind, ReplaySafety, ResumeEligibility,
    ResumeMode, TaskCheckpoint,
};
pub(crate) use configuration::hex as configuration_hex;
pub use configuration::{
    ConfigurationDiagnostic, ConfigurationKind, ConfigurationSource, DiagnosticSeverity,
    EffectiveConfiguration,
};
pub use context::{
    ContextCategory, ContextManifest, ContextSource, DestinationClass, content_hash,
};
pub use descriptor::{
    AgentDescriptor, CheckpointSupport, Health, IntegrationMode, ModelCapability, ModelControl,
    ModelDescriptor, ModelLocation, NetworkControl, ToolControl, ToolDescriptor,
};
pub use error::{ErrorCode, ErrorEnvelope};
pub use event::{
    CancellationOrigin, EventDraft, EventEnvelope, EventPayload, PermissionOrigin, StreamFrame,
    TaskEventKind, TaskUpdate,
};
pub use export::{
    BUNDLE_MAJOR_VERSION, BUNDLE_MINOR_VERSION, BundleEntry, ExportBundle, ExportManifest,
    ExportOptions, ExportedConfiguration, HandoffBundle, HandoffEntry, ImportedRecords, Omission,
    OmissionCategory,
};
pub use ids::{
    AgentId, AgentProviderId, ApprovalId, CheckpointId, ConversationId, CorrelationId, EventId,
    InvocationId, MessageId, ModelId, ProjectId, ProviderId, TaskId, WorkerSessionId,
};
pub use message::{MessagePart, MessageRole, MessageStatus, NormalizedMessage};
pub use model::{
    AuthorizedModelRequest, ModelEvent, ModelLimits, ModelRequest, NormalizedToolDefinition,
    ResponseFormat, StopReason, Usage,
};
pub use permission::{
    ApprovalDecision, ApprovalDecisionKind, ApprovalDetail, ApprovalRequest, AuthorizedToolRequest,
    Capability, CapabilityScope, GrantKind, ProjectGrant,
};
pub(crate) use permission::{AuthorizedTarget, GrantedAuthority};
pub use primitives::{
    DataClass, ExecutionLocation, RiskClass, RoutingMode, SecretRef, SecretStore, SideEffect,
    Visibility,
};
pub use records::{ConversationRecord, ProjectRecord, TaskRecord};
pub use release::{
    ContextReleasePlan, ReleaseAuthorization, ReleaseExclusion, ReleasedCategory,
    reason as release_reason,
};
pub use routing::{RoutedModel, RoutingDecision, RoutingFallback, reason as routing_reason};
pub use task::{InvalidTransition, TaskSnapshot, TaskState};
pub use time::Timestamp;

/// Version of the schemas exchanged across the IPC and export boundaries.
///
/// Initial executable schemas use version `1`. Bumping this is a breaking
/// change: it requires a migration, regenerated artifacts, and a decision
/// record (`docs/CORE_CONTRACTS.md` §15).
pub const SCHEMA_VERSION: u32 = 1;
