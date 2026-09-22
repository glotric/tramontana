//! Stable interfaces that adapters implement.
//!
//! `docs/ARCHITECTURE.md` §2: dependency direction points inward. Everything
//! here is defined in terms of core contract types, so a concrete model
//! runtime, worker, database, or platform API can be replaced without the
//! interface or the durable record changing.
//!
//! M1 defines the ports the walking skeleton exercises. `ComputeProvider` and
//! `ContextProvider` arrive with the milestones that need them.

mod clock;
mod event_store;
mod ids;
mod model;
mod permission;
mod read_only;
mod repository;
mod secret;
mod tool;
mod worker;

pub use clock::Clock;
pub use event_store::{EventStore, EventStoreError};
pub use ids::IdSource;
pub use model::ModelProvider;
pub use permission::{
    ApprovalStore, Approver, PermissionError, PermissionOutcome, PermissionPolicy, PermissionQuery,
    SiteAccess, reason,
};
pub use read_only::{ReadOnlyLimits, ReadOnlyTurn, TOOL_RUN_LIMIT, ToolAdmission};
pub use repository::{
    AppSettingsRepository, CheckpointRepository, ConfigurationRepository, ConversationRepository,
    ProjectRepository, ProviderSettingsRepository, RepositoryError, TaskRepository,
};
pub use secret::{SecretResolver, SecretVault};
pub use tool::{ToolInvocation, ToolOutcome, ToolProvider, ToolTarget};
pub use worker::{AgentProvider, WorkerHost, WorkerOutcome, WorkerRequest};
