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
mod repository;
mod tool;
mod worker;

pub use clock::Clock;
pub use event_store::{EventStore, EventStoreError};
pub use ids::IdSource;
pub use model::{ModelEvent, ModelProvider, ModelRequest, StopReason};
pub use repository::{ConversationRepository, ProjectRepository, RepositoryError, TaskRepository};
pub use tool::{ToolInvocation, ToolOutcome, ToolProvider};
pub use worker::{AgentProvider, WorkerHost, WorkerOutcome, WorkerRequest};
