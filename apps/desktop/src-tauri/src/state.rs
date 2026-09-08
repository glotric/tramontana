//! Composition root for the application services.

use std::sync::Arc;

use tramontana_core::ports::EventStore;
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{
    FakeClock, FakeModelProvider, FakeToolProvider, FakeWorker, InMemoryConfigurationRepository,
    InMemoryConversationRepository, InMemoryEventStore, InMemoryProjectRepository,
    InMemoryTaskRepository, SequentialIdSource,
};

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
    /// Assemble the M1 skeleton over in-memory fakes.
    ///
    /// The model is paced so streaming and cancellation are visible in the
    /// interface; an instant fake would finish before a person could see it
    /// happen, let alone press stop.
    #[must_use]
    pub(crate) fn with_fakes() -> Self {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
        let service = TaskService::new(
            Repositories {
                projects: Arc::new(InMemoryProjectRepository::new()),
                conversations: Arc::new(InMemoryConversationRepository::new()),
                tasks: Arc::new(InMemoryTaskRepository::with_events(Arc::clone(&store))),
                events: store,
                configuration: Arc::new(InMemoryConfigurationRepository::new()),
            },
            Providers {
                model: Arc::new(FakeModelProvider::streaming_paced(
                    ["Hello", ", ", "world", " from ", "Tramontana"],
                    220,
                )),
                tools: Arc::new(FakeToolProvider::new()),
                worker: Arc::new(FakeWorker::new()),
            },
            Arc::new(FakeClock::new()),
            Arc::new(SequentialIdSource::new()),
        );
        Self {
            service: Arc::new(service),
        }
    }
}
