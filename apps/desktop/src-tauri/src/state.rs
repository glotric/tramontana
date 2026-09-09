//! Composition root for the application services.

use std::path::Path;
use std::sync::Arc;

use tauri::AppHandle;
use tramontana_adapters_tools::{FilesystemTools, GitTools};
use tramontana_core::ports::EventStore;
use tramontana_core::ports::{ApprovalStore, Approver, ToolProvider};
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{
    FakeModelProvider, FakeWorker, InMemoryApprovalStore, assemble_tool_plane,
};

use crate::approval::DesktopApprover;
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
    /// Where approval questions are raised and answered.
    pub(crate) approver: Arc<DesktopApprover>,
    /// Where grants live, so they can be listed and revoked.
    pub(crate) approvals: Arc<InMemoryApprovalStore>,
}

impl AppState {
    /// Assemble the desktop service over the durable M2 profile.
    ///
    /// Providers remain deterministic fakes until their later milestones, but
    /// task, project, conversation, event, and configuration records use the
    /// same SQLite repositories as the CLI.
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
        // Read-only tools, behind the permission boundary. The registry says
        // a tool exists; only the permission manager says it may run. The CLI
        // assembles the identical set, so the two adapters cannot drift into
        // offering different tools under different policy.
        let approvals = Arc::new(InMemoryApprovalStore::new());
        let (registry, permissions) = assemble_tool_plane(
            vec![
                Arc::new(FilesystemTools::new()) as Arc<dyn ToolProvider>,
                Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
            ],
            Arc::clone(&approvals) as Arc<dyn ApprovalStore>,
            Arc::clone(&approver) as Arc<dyn Approver>,
            Arc::new(UuidIdSource::new()),
        )
        .map_err(|error| format!("could not register tools: {error}"))?;

        let lease = tramontana_platform::ProfileLease::try_acquire(path)
            .map_err(|error| format!("could not acquire runtime profile lease: {error}"))?
            .map(|lease| Arc::new(lease) as Arc<dyn Send + Sync>);
        let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::new(database.clone()));
        let service = TaskService::new(
            Repositories {
                projects: Arc::new(SqliteProjectRepository::new(database.clone())),
                conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
                tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
                events: store,
                checkpoints: Arc::new(tramontana_persistence::SqliteCheckpointRepository::new(
                    database.clone(),
                )),
                configuration: Arc::new(SqliteConfigurationRepository::new(database)),
            },
            Providers {
                model: Arc::new(FakeModelProvider::streaming_paced(
                    ["Hello", ", ", "world", " from ", "Tramontana"],
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
        service
            .recover()
            .await
            .map_err(|error| format!("recovery failed: {}", error.message))?;
        Ok(Self {
            service: Arc::new(service),
            approver,
            approvals,
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
