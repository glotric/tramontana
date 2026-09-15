//! Composition root for the application services.

use std::path::Path;
use std::sync::Arc;

use tauri::AppHandle;
use tramontana_adapters_tools::{FilesystemTools, GitTools, RecipeTools};
use tramontana_core::ports::EventStore;
use tramontana_core::ports::{
    ApprovalStore, Approver, ModelProvider, ProviderSettingsRepository, SecretResolver,
    ToolProvider,
};
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{FakeModelProvider, InMemoryApprovalStore, assemble_tool_plane};

use crate::approval::DesktopApprover;
use tramontana_composition::{ModelPlane, Supervision, assemble_models_recording};
use tramontana_persistence::{
    Database, SqliteConfigurationRepository, SqliteConversationRepository, SqliteEventStore,
    SqliteProjectRepository, SqliteProviderSettingsRepository, SqliteTaskRepository,
};
use tramontana_platform::{PlatformSecretVault, SystemClock, UuidIdSource};

/// What the Tauri layer manages.
///
/// One field today, but the shape matters: adapters are assembled here and
/// handed to the service, so replacing a fake with a real provider in M4 is a
/// change to this file alone.
pub(crate) struct AppState {
    /// The application service every command delegates to.
    pub(crate) service: Arc<TaskService>,
    /// Where approval questions are raised and answered.
    pub(crate) approver: Arc<DesktopApprover>,
    /// Where grants live, so they can be listed and revoked.
    pub(crate) approvals: Arc<InMemoryApprovalStore>,
    /// Runtimes this process started, held for the life of the window.
    ///
    /// Dropping it stops them, which is why the desktop keeps it rather than
    /// letting assembly go out of scope: a model runtime that outlived the
    /// application would hold gigabytes with nothing to stop it.
    pub(crate) models: Arc<tokio::sync::Mutex<ModelPlane>>,
}

impl AppState {
    /// Assemble the desktop service over the durable profile.
    ///
    /// Model providers come from the endpoints configured in this profile,
    /// through the same composition the CLI uses, so the two cannot drift
    /// into reaching different models under different policy. A profile with
    /// nothing configured falls back to the deterministic fake, and the
    /// interface says so rather than letting a stub pass for a model.
    pub(crate) async fn open(path: &Path, app: AppHandle) -> Result<Self, String> {
        Self::open_with_chunk_delay(path, 220, Arc::new(DesktopApprover::new(app))).await
    }

    async fn open_with_chunk_delay(
        path: &Path,
        chunk_delay_ms: u64,
        approver: Arc<DesktopApprover>,
    ) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        let database = Database::open(path)
            .await
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        // Filesystem and Git tools, behind the permission boundary. The
        // registry says a tool exists; only the permission manager says it may
        // run. The CLI assembles the identical set, including the same
        // recovery store beside the profile, so the two adapters cannot drift
        // into offering different tools under different policy.
        let approvals = Arc::new(InMemoryApprovalStore::new());
        let (registry, permissions, releases) = assemble_tool_plane(
            vec![
                Arc::new(
                    FilesystemTools::new()
                        .with_recovery_store(tramontana_composition::recovery_store(path)),
                ) as Arc<dyn ToolProvider>,
                Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
                // Registered so both adapters hold the same set; offered to a
                // model only in a task whose person chose to allow recipes.
                Arc::new(RecipeTools::new()) as Arc<dyn ToolProvider>,
            ],
            Arc::clone(&approvals) as Arc<dyn ApprovalStore>,
            Arc::clone(&approver) as Arc<dyn Approver>,
            Arc::new(UuidIdSource::new()),
        )
        .map_err(|error| format!("could not register tools: {error}"))?;

        let lease = tramontana_platform::ProfileLease::try_acquire(path)
            .map_err(|error| format!("could not acquire runtime profile lease: {error}"))?
            .map(|lease| Arc::new(lease) as Arc<dyn Send + Sync>);
        // Holding the lease proves any runtime recorded for this profile
        // outlived the process that started it: stop those before starting
        // more. Without the lease they may be in use (ADR 0030).
        if lease.is_some() {
            for note in tramontana_composition::reap_runtimes(path).await {
                tracing::warn!("{note}");
            }
        }
        let records = lease
            .is_some()
            .then(|| tramontana_composition::runtime_records(path));
        let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::new(database.clone()));
        // The endpoints the user configured, through the same composition the
        // CLI uses. Supervised runtimes are started here because the desktop
        // is long-lived; the plane below owns them and stops them on close.
        let settings = SqliteProviderSettingsRepository::new(database.clone());
        let configured = settings
            .list()
            .await
            .map_err(|error| format!("could not read provider settings: {error}"))?;
        let resolver = Arc::new(PlatformSecretVault::new()) as Arc<dyn SecretResolver>;
        let plane = assemble_models_recording(
            &configured,
            &resolver,
            Supervision::Start,
            records.as_deref(),
        )
        .await;
        for failure in &plane.failures {
            tracing::warn!(
                endpoint = failure.endpoint_id,
                "a configured model endpoint is unavailable: {}",
                failure.error.message
            );
        }
        let has_configured = configured.iter().any(|endpoint| endpoint.enabled);
        let models: Vec<Arc<dyn ModelProvider>> = if !has_configured {
            // Labelled explicitly in the interface, not silently substituted.
            vec![Arc::new(FakeModelProvider::streaming_paced(
                ["Hello", ", ", "world", " from ", "Tramontana"],
                chunk_delay_ms,
            ))]
        } else {
            plane.providers.clone()
        };

        let service = TaskService::new(
            Repositories {
                projects: Arc::new(SqliteProjectRepository::new(database.clone())),
                conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
                tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
                events: store,
                checkpoints: Arc::new(tramontana_persistence::SqliteCheckpointRepository::new(
                    database.clone(),
                )),
                configuration: Arc::new(SqliteConfigurationRepository::new(database.clone())),
            },
            Providers {
                models,
                tools: Arc::clone(&registry),
                permissions: Arc::clone(&permissions),
                releases: Arc::clone(&releases),
                // Same strategy and governed tool plane as the CLI. The
                // service's own worker is read-only. A task gets a worker that
                // may ask to write or run recipes only when the person chose
                // that in the composer, and each one is still approved as it
                // happens (ADRs 0021 and 0031).
                worker: tramontana_composition::assemble_worker(
                    has_configured,
                    tramontana_core::ports::ToolAdmission::READ_ONLY,
                ),
            },
            Arc::new(SystemClock::new()),
            Arc::new(UuidIdSource::new()),
        );
        let service = service
            .with_worker_choice(tramontana_composition::worker_choice(has_configured))
            .with_execution_guard(lease);
        service
            .recover()
            .await
            .map_err(|error| format!("recovery failed: {}", error.message))?;
        Ok(Self {
            service: Arc::new(service),
            approver,
            approvals,
            models: Arc::new(tokio::sync::Mutex::new(plane)),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
)]
mod tests {
    use std::time::Duration;

    use tramontana_core::config::ConfigurationSelection;
    use tramontana_core::contracts::TaskEventKind;
    use tramontana_core::services::StartTaskRequest;
    use tramontana_platform::{canonicalize_root, discover};

    use super::*;

    #[tokio::test]
    async fn a_task_can_ask_for_its_own_admission_and_mode() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::open_with_chunk_delay(
            &directory.path().join("desktop.sqlite"),
            0,
            Arc::new(DesktopApprover::headless()),
        )
        .await
        .unwrap();

        // Refused as `capabilityUnsupported` by a service that cannot build a
        // worker per task, which is what the desktop was before M8.
        let task_id = state
            .service
            .start_task(StartTaskRequest {
                goal: "Say hello".to_owned(),
                admission: Some(tramontana_core::ports::ToolAdmission::READ_WRITE),
                routing_mode: Some(tramontana_core::contracts::RoutingMode::LocalOnly),
                ..Default::default()
            })
            .await
            .unwrap();

        let events = state.service.replay_task_events(&task_id, 0).await.unwrap();
        assert!(matches!(
            events[0].payload,
            tramontana_core::contracts::EventPayload::TaskCreated {
                routing_mode: Some(tramontana_core::contracts::RoutingMode::LocalOnly),
                ..
            }
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !state
                .service
                .get_task(&task_id)
                .await
                .unwrap()
                .state
                .is_terminal()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn reopening_the_desktop_profile_restores_the_same_durable_work() {
        let directory = tempfile::tempdir().unwrap();
        let project_directory = directory.path().join("project with ünicode");
        std::fs::create_dir_all(&project_directory).unwrap();
        std::fs::write(
            project_directory.join("AGENTS.md"),
            "---\nenabled: true\n---\nKeep the durable record portable.\n",
        )
        .unwrap();
        let root = canonicalize_root(&project_directory).unwrap();
        let profile = directory.path().join("desktop.sqlite");

        let first =
            AppState::open_with_chunk_delay(&profile, 0, Arc::new(DesktopApprover::headless()))
                .await
                .unwrap();
        let project = first
            .service
            .open_project("Restart fixture", vec![root.as_str().to_owned()])
            .await
            .unwrap();
        let project_id = project.project_id.clone();
        let configuration = first
            .service
            .resolve_configuration(
                &project_id,
                discover(&root),
                &ConfigurationSelection::default(),
            )
            .await
            .unwrap();
        assert_eq!(configuration.sources.len(), 1);
        assert!(configuration.sources[0].active);
        let conversation = first
            .service
            .create_conversation(Some(project_id.clone()), "Durable conversation")
            .await
            .unwrap();
        let conversation_id = conversation.conversation_id.clone();
        let task_id = first
            .service
            .start_task(StartTaskRequest {
                conversation_id: Some(conversation_id),
                goal: "Survive a desktop restart".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = first.service.get_task(&task_id).await.unwrap();
                let events = first.service.replay_task_events(&task_id, 0).await.unwrap();
                let terminal_is_fully_projected = events.last().is_some_and(|event| {
                    event.kind == TaskEventKind::TaskCompleted
                        && event.sequence == snapshot.last_sequence
                });
                if snapshot.state.is_terminal() && terminal_is_fully_projected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let task_before = first.service.get_task_record(&task_id).await.unwrap();
        let events_before = first.service.replay_task_events(&task_id, 0).await.unwrap();
        drop(first);

        let reopened =
            AppState::open_with_chunk_delay(&profile, 0, Arc::new(DesktopApprover::headless()))
                .await
                .unwrap();
        assert_eq!(
            reopened.service.list_projects().await.unwrap(),
            vec![project]
        );
        assert_eq!(
            reopened
                .service
                .list_conversations(Some(&project_id))
                .await
                .unwrap(),
            vec![conversation]
        );
        assert_eq!(
            reopened.service.get_task_record(&task_id).await.unwrap(),
            task_before
        );
        assert_eq!(
            reopened
                .service
                .replay_task_events(&task_id, 0)
                .await
                .unwrap(),
            events_before
        );
        assert_eq!(
            reopened
                .service
                .get_effective_configuration(&project_id)
                .await
                .unwrap(),
            Some(configuration)
        );
    }
}
