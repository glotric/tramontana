//! Application services.
//!
//! This is the layer the CLI and the desktop shell both call. Neither adapter
//! may own routing, permission, provider, or task behavior of its own
//! (`AGENTS.md`, "Architectural boundaries"), so anything that looks like a
//! decision belongs here.
//!
//! M1 provides the command set from `CORE_CONTRACTS.md` §13 that the walking
//! skeleton needs, driving one task from creation to exactly one terminal
//! state.

mod checkpoint;
mod export;
mod handoff;
mod journal;
mod permission_manager;
mod preparation;
mod projection;
mod routing;
mod runtime;
mod task_export;
mod task_recovery;
mod task_service;
mod tool_registry;

pub use checkpoint::{
    DerivedState, ResumePlan, derive_checkpoint, derive_state, reason as checkpoint_reason,
};
pub use export::{
    ExportRequest, ImportError, ImportLimits, REDACTED_OUTPUT, REDACTED_PATH, build_export,
    imported_project_id, is_safe_relative_path, validate_import,
};
pub use handoff::build_handoff;
pub use journal::{JournalPorts, TaskJournal};
pub use permission_manager::PermissionManager;
pub use preparation::assemble_context;
pub use projection::TaskProjectionWriter;
pub use routing::{
    AvailabilitySnapshot, NoEligibleRoute, RoutingInput, route, route_after_failure,
};
pub use task_export::ImportSummary;
pub use task_service::{Providers, Repositories, StartTaskRequest, TaskService};
pub use tool_registry::{RegisteredTool, RegistryError, ToolRegistry};
