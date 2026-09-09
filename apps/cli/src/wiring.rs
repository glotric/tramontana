//! Composition root for the CLI.
//!
//! Assembles storage, providers, and platform ports into a service. The desktop
//! shell assembles the identical set; if the two ever diverge, the CLI stops
//! being a faithful way to inspect what the application actually did.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tramontana_adapters_tools::{FilesystemTools, GitTools};
use tramontana_core::ports::EventStore;
use tramontana_core::ports::{ApprovalStore, Approver, ToolProvider};
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{
    FakeModelProvider, FakeWorker, InMemoryApprovalStore, assemble_tool_plane,
};

use crate::approval::TerminalApprover;
use tramontana_persistence::{
    Database, SqliteConfigurationRepository, SqliteConversationRepository, SqliteEventStore,
    SqliteProjectRepository, SqliteTaskRepository,
};
use tramontana_platform::{SystemClock, UuidIdSource};

/// Where a development profile keeps its database.
///
/// Under the repository's ignored local-data directory rather than a real
/// application-support path: M2 has no installed application yet, and writing
/// to a user's system directory before there is one to uninstall would leave
/// litter nobody asked for.
#[must_use]
pub(crate) fn default_profile_path() -> PathBuf {
    PathBuf::from(".tramontana-local").join("tramontana.sqlite")
}

/// Select the profile named explicitly by the caller, then the shared profile
/// environment variable, then the repository-local development default.
///
/// `TRAMONTANA_PROFILE` gives the CLI and desktop shell a deliberate way to
/// open the same durable record during M2 validation without baking a
/// machine-specific application-data path into either adapter.
#[must_use]
pub(crate) fn profile_path(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("TRAMONTANA_PROFILE").map(PathBuf::from))
        .unwrap_or_else(default_profile_path)
}

/// Open a profile and assemble the service over it.
///
/// # Errors
///
/// Returns a message describing what could not be opened.
pub(crate) async fn open(path: &Path, chunk_delay_ms: u64) -> Result<Arc<TaskService>, String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }

    let database = Database::open(path)
        .await
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;

    // Read-only tools, behind the permission boundary. The registry says a
    // tool exists; only the permission manager says it may run.
    let (registry, permissions) = assemble_tool_plane(
        vec![
            Arc::new(FilesystemTools::new()) as Arc<dyn ToolProvider>,
            Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
        ],
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        // Prompts on a terminal, and refuses when there is no terminal to
        // prompt on. Every tool registered above is a read inside an approved
        // project, which policy permits without asking, so this is exercised
        // only once modifying tools arrive in M7.
        Arc::new(TerminalApprover::new()) as Arc<dyn Approver>,
        Arc::new(UuidIdSource::new()),
    )
    .map_err(|error| format!("could not register tools: {error}"))?;

    let lease = tramontana_platform::ProfileLease::try_acquire(path)
        .map_err(|error| format!("could not acquire runtime profile lease: {error}"))?
        .map(|lease| Arc::new(lease) as Arc<dyn Send + Sync>);
    let events = Arc::new(SqliteEventStore::new(database.clone())) as Arc<dyn EventStore>;
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(SqliteProjectRepository::new(database.clone())),
            conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
            tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
            events,
            checkpoints: Arc::new(tramontana_persistence::SqliteCheckpointRepository::new(
                database.clone(),
            )),
            configuration: Arc::new(SqliteConfigurationRepository::new(database)),
        },
        Providers {
            // M4 onward replaces these with real adapters behind the same
            // ports; nothing in this function changes shape.
            model: Arc::new(FakeModelProvider::streaming_paced(
                ["Hello", ", ", "world"],
                chunk_delay_ms,
            )),
            tools: Arc::clone(&registry),
            permissions: Arc::clone(&permissions),
            // Points the reference worker at a genuine read-only tool, so the
            // skeleton exercises the real permission and containment path
            // rather than a fake that cannot be refused.
            worker: Arc::new(FakeWorker::calling(
                "builtin.fs.list",
                serde_json::json!({ "path": "." }),
            )),
        },
        Arc::new(SystemClock::new()),
        Arc::new(UuidIdSource::new()),
    );

    let service = service.with_execution_guard(lease);

    // A process that died between appending an event and updating a task's
    // cached row leaves the two disagreeing. The events are the record, so the
    // cache is repaired before anything reads it.
    let repaired = service
        .recover()
        .await
        .map_err(|error| format!("recovery failed: {}", error.message))?;
    if repaired > 0 {
        eprintln!("recovered {repaired} task projection(s) from the event stream");
    }

    Ok(Arc::new(service))
}
