//! Composition root for the CLI.
//!
//! Assembles storage, providers, and platform ports into a service. The desktop
//! shell assembles the identical set; if the two ever diverge, the CLI stops
//! being a faithful way to inspect what the application actually did.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tramontana_core::ports::EventStore;
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{FakeModelProvider, FakeToolProvider, FakeWorker};
use tramontana_persistence::{
    Database, SqliteConversationRepository, SqliteEventStore, SqliteProjectRepository,
    SqliteTaskRepository,
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

    let events = Arc::new(SqliteEventStore::new(database.clone())) as Arc<dyn EventStore>;
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(SqliteProjectRepository::new(database.clone())),
            conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
            tasks: Arc::new(SqliteTaskRepository::new(database)),
            events,
        },
        Providers {
            // M4 onward replaces these with real adapters behind the same
            // ports; nothing in this function changes shape.
            model: Arc::new(FakeModelProvider::streaming_paced(
                ["Hello", ", ", "world"],
                chunk_delay_ms,
            )),
            tools: Arc::new(FakeToolProvider::new()),
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(SystemClock::new()),
        Arc::new(UuidIdSource::new()),
    );

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
