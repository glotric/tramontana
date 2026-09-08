//! Composition root for the application services.

use std::path::Path;
use std::sync::Arc;

use tramontana_core::ports::EventStore;
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{FakeModelProvider, FakeToolProvider, FakeWorker};
use tramontana_persistence::{
    Database, SqliteConfigurationRepository, SqliteConversationRepository, SqliteEventStore,
    SqliteProjectRepository, SqliteTaskRepository,
};
use tramontana_platform::{SystemClock, UuidIdSource};

/// What the Tauri layer manages.
///
/// One field today, but the shape matters: adapters are assembled here and
/// handed to the service, so replacing a fake with a real provider in M4 is a
/// change to this file alone.
pub(crate) struct AppState {
    /// The application service every command delegates to.
    pub(crate) service: Arc<TaskService>,
}

impl AppState {
    /// Assemble the desktop service over the durable M2 profile.
    ///
    /// Providers remain deterministic fakes until their later milestones, but
    /// task, project, conversation, event, and configuration records use the
    /// same SQLite repositories as the CLI.
    pub(crate) async fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        let database = Database::open(path)
            .await
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::new(database.clone()));
        let service = TaskService::new(
            Repositories {
                projects: Arc::new(SqliteProjectRepository::new(database.clone())),
                conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
                tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
                events: store,
                configuration: Arc::new(SqliteConfigurationRepository::new(database)),
            },
            Providers {
                model: Arc::new(FakeModelProvider::streaming_paced(
                    ["Hello", ", ", "world", " from ", "Tramontana"],
                    220,
                )),
                tools: Arc::new(FakeToolProvider::new()),
                worker: Arc::new(FakeWorker::new()),
            },
            Arc::new(SystemClock::new()),
            Arc::new(UuidIdSource::new()),
        );
        service
            .recover()
            .await
            .map_err(|error| format!("recovery failed: {}", error.message))?;
        Ok(Self {
            service: Arc::new(service),
        })
    }
}
