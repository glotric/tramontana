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
mod model_catalogue;
mod model_context;
mod model_history;
mod model_input;
mod model_turn;
mod permission_manager;
mod preparation;
mod projection;
mod recipe;
mod redaction;
mod reference_worker;
mod release;
mod routing;
mod runtime;
mod task_export;
mod task_recovery;
mod task_service;
mod tool_registry;
mod tool_schema;

pub use checkpoint::{
    DerivedState, ResumePlan, derive_checkpoint, derive_state, reason as checkpoint_reason,
};
pub use export::{
    ExportRequest, ImportError, ImportLimits, REDACTED_OUTPUT, REDACTED_PATH, build_export,
    imported_project_id, is_safe_relative_path, validate_import,
};
pub use handoff::build_handoff;
pub use journal::{JournalPorts, TaskJournal};
pub use model_catalogue::ModelToolCatalogue;
pub use model_context::{
    ModelContentProvenance, ModelContextInput, PreparedModelContext, prepare_model_context,
};
pub use model_history::CanonicalModelHistory;
pub use model_input::{
    MODEL_INPUT_BYTES, model_input_content, validate_model_capabilities, validate_model_input,
};
pub use model_turn::{ModelToolCall, ModelTurn, ModelTurnAccumulator, ModelTurnLimits};
pub use permission_manager::PermissionManager;
pub use preparation::assemble_context;
pub use projection::TaskProjectionWriter;
pub use recipe::{COMMAND_ENVIRONMENT, RECIPE_TOOL_ID};
pub use redaction::{
    PLACEHOLDER, Redacted, carries_credential, reason as redaction_reason, redact, redacted_hash,
};
pub use reference_worker::{REFERENCE_AGENT_ID, ReferenceWorker};
pub use release::{RELEASE_CAPABILITY, ReleaseContext, ReleaseError, ReleaseManager};
pub use routing::{
    AvailabilitySnapshot, NoEligibleRoute, RoutingInput, route, route_after_failure,
};
pub use task_export::ImportSummary;
pub use task_service::{Providers, Repositories, StartTaskRequest, TaskService, WorkerChoice};
pub use tool_registry::{RegisteredTool, RegistryError, ToolRegistry};
pub use tool_schema::{TOOL_INPUT_BYTES, ToolInputSchema};
