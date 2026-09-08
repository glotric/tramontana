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

mod journal;
mod runtime;
mod task_service;

pub use journal::{JournalPorts, TaskJournal};
pub use task_service::{ConversationRecord, ProjectRecord, StartTaskRequest, TaskService};
