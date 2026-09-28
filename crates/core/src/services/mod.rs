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
mod earlier_turns;
mod export;
mod handoff;
mod instructions;
mod journal;
mod model_catalogue;
mod model_context;
mod model_history;
mod model_input;
mod model_turn;
mod permission_manager;
mod person_read;
mod preparation;
mod projection;
mod recipe;
mod redaction;
mod reference_worker;
mod release;
mod routing;
mod runtime;
mod suggestion;
mod task_export;
mod task_recovery;
mod task_service;
mod tool_registry;
mod tool_schema;

pub use checkpoint::{
    DerivedState, ResumePlan, derive_checkpoint, derive_state, reason as checkpoint_reason,
};
pub use earlier_turns::{
    EARLIER_TURN_BYTES, EARLIER_TURNS, EarlierTurn, EarlierTurns, answer_source_id, goal_source_id,
};
pub use export::{
    ExportRequest, ImportError, ImportLimits, REDACTED_OUTPUT, REDACTED_PATH, build_export,
    imported_project_id, is_safe_relative_path, validate_import,
};
pub use handoff::build_handoff;
pub use instructions::{
    ComposedInstructions, InstructionSource, NO_INSTRUCTION_TEXT, OVER_INSTRUCTION_BUDGET,
    PROJECT_INSTRUCTION_BYTES, PROJECT_INSTRUCTIONS_SOURCE_ID, ProjectInstructions,
};
pub use journal::{JournalPorts, TaskJournal};
pub use model_catalogue::ModelToolCatalogue;
pub use model_context::{
    ModelContentProvenance, ModelContextInput, PreparedModelContext, prepare_model_context,
    prepare_model_context_on,
};
pub use model_history::{
    CHAT_INSTRUCTIONS, CanonicalModelHistory, HISTORY_RESULT_BYTES, RESULT_LEFT_OUT,
    WORKER_INSTRUCTIONS,
};
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
/// The reason a task moved on to another worker, from which that worker's
/// budget is counted (ADR 0048).
pub const HANDED_OVER: &str = "user.handedOver";

pub use reference_worker::{PinnedModel, REFERENCE_AGENT_ID, ReferenceWorker};
pub use release::{RELEASE_CAPABILITY, ReleaseContext, ReleaseError, ReleaseManager};
pub use routing::{
    AvailabilitySnapshot, NoEligibleRoute, RoutingInput, route, route_after_failure,
};
pub use runtime::{beyond_the_model, stopped_on};
pub use suggestion::suggested_skills;
pub use task_export::ImportSummary;
pub use task_service::{
    HandoffOffer, PinnedWorkerChoice, Providers, Repositories, StartTaskRequest, TaskService,
    WorkerChoice, WorkerOption,
};
pub use tool_registry::{
    APP_NAMESPACE, BROWSER_PROVIDER, RegisteredTool, RegistryError, ToolRegistry, is_app_tool,
    is_browser_tool,
};
pub use tool_schema::{TOOL_INPUT_BYTES, ToolInputSchema};
