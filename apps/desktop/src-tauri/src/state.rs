//! Composition root for the application services.

use std::path::Path;
use std::sync::Arc;

use tauri::AppHandle;
use tramontana_core::ports::{Approver, SecretResolver};
use tramontana_core::services::TaskService;
use tramontana_core::testing::InMemoryApprovalStore;

use crate::approval::DesktopApprover;
use tramontana_composition::ModelPlane;
use tramontana_persistence::{SqliteAppSettingsRepository, SqliteProviderSettingsRepository};
use tramontana_platform::PlatformSecretVault;

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
    /// Runtimes this process started, held for the life of the window and
    /// stopped by [`AppState::shut_down`] as it exits.
    ///
    /// Tauri ends the process without dropping managed state, so the
    /// runtimes' own drop is only a backstop for a panic: a model runtime
    /// that outlived the application would hold gigabytes until something
    /// next opened the profile.
    pub(crate) models: Arc<tokio::sync::Mutex<ModelPlane>>,
    /// The Apps this process started, stopped with it (ADR 0036).
    pub(crate) apps: tramontana_composition::AppPlane,
    /// The built-in browser, stopped with the application (ADR 0047).
    pub(crate) browser: Arc<tramontana_composition::BrowserProvider>,
    /// The profile's App settings, which the Apps screen changes.
    pub(crate) app_settings: Arc<SqliteAppSettingsRepository>,
    /// The bundle folder the person last inspected, waiting for them to
    /// confirm the import.
    pub(crate) staged_bundle: Arc<crate::bundles::StagedSlot>,
    /// Files picked for a message and not yet sent (ADR 0058).
    pub(crate) staged_attachments: Arc<crate::attachments::StagedAttachments>,
    /// The task streams windows are watching, so one can be stopped.
    pub(crate) watches: Arc<crate::commands::Watches>,
    /// The profile's configured endpoints, which the settings view changes.
    pub(crate) settings: Arc<SqliteProviderSettingsRepository>,
    /// The operating system's credential store.
    pub(crate) vault: Arc<PlatformSecretVault>,
    /// Where the person's own skills are kept (ADR 0070).
    pub(crate) skills_folder: std::path::PathBuf,
    /// Empties the profile's records, for "Delete all my data" (ADR 0068).
    pub(crate) eraser: Arc<tramontana_persistence::SqliteProfileEraser>,
    /// Where the profile's database is; its folders sit beside it.
    pub(crate) profile: std::path::PathBuf,
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

    pub(crate) async fn open_with_chunk_delay(
        path: &Path,
        chunk_delay_ms: u64,
        approver: Arc<DesktopApprover>,
    ) -> Result<Self, String> {
        // The assembly a host shares (SHARED_PROJECTS_PLAN.md SP1). What is
        // the desktop's own: a card for approvals, and the OS credential
        // store.
        let vault = Arc::new(PlatformSecretVault::new());
        let plane = tramontana_composition::open_service_plane(
            path,
            Arc::clone(&approver) as Arc<dyn Approver>,
            Arc::clone(&vault) as Arc<dyn SecretResolver>,
            chunk_delay_ms,
        )
        .await?;
        Ok(Self {
            service: Arc::new(plane.service),
            approver,
            approvals: plane.approvals,
            models: Arc::new(tokio::sync::Mutex::new(plane.models)),
            apps: plane.apps,
            browser: plane.browser,
            app_settings: plane.app_settings,
            staged_bundle: Arc::new(std::sync::Mutex::new(None)),
            staged_attachments: Arc::default(),
            watches: Arc::default(),
            settings: plane.settings,
            vault,
            eraser: plane.eraser,
            profile: path.to_path_buf(),
            skills_folder: plane.skills_folder,
        })
    }

    /// Delete everything this profile holds (ADR 0068): its records, the
    /// credentials they name, and the folders beside its database. Project
    /// folders are never touched.
    ///
    /// Refused, with nothing removed, while a task runs. Once it succeeds the
    /// model runtimes, Apps and browser have been stopped, so the window must
    /// restart the application before anything else is done.
    ///
    /// # Errors
    ///
    /// Whatever the service reports, or [`ErrorCode::Internal`] naming a
    /// folder that could not be removed.
    pub(crate) async fn erase_everything(
        &self,
    ) -> Result<tramontana_core::services::ErasureReport, tramontana_core::contracts::ErrorEnvelope>
    {
        let report = self
            .service
            .erase_everything(&tramontana_core::services::ProfileErasure {
                records: &*self.eraser,
                endpoints: &*self.settings,
                apps: &*self.app_settings,
                vault: &*self.vault,
            })
            .await?;
        // They write in the folders about to go.
        self.shut_down().await;
        tramontana_composition::erase_profile_folders(&self.profile).map_err(|error| {
            tramontana_core::contracts::ErrorEnvelope::new(
                tramontana_core::contracts::ErrorCode::Internal,
                format!(
                    "the records were deleted, but a folder beside them could not be: {}",
                    error.kind()
                ),
                tramontana_core::contracts::CorrelationId::new("erase"),
            )
        })?;
        Ok(report)
    }

    /// Stop the model runtimes this process started.
    ///
    /// Called as the application exits, whether by quitting or by "Restart
    /// now". Waits for each runtime to stop, up to its grace period.
    pub(crate) async fn shut_down(&self) {
        self.models.lock().await.shutdown().await;
        self.apps.shutdown().await;
        self.browser.shutdown().await;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
)]
mod tests {
    /// How long a test waits for a fixture's task: generous, because a
    /// loaded CI runner (Windows especially) can take seconds to run one.
    const TASK_DEADLINE: Duration = Duration::from_secs(20);
    /// How often a test looks again while it waits, rather than spinning on
    /// the database and competing with the task it is waiting for.
    const POLL: Duration = Duration::from_millis(10);

    use std::time::Duration;

    use tramontana_composition::acquire_lease;
    use tramontana_core::config::ConfigurationSelection;
    use tramontana_core::contracts::TaskEventKind;
    use tramontana_core::services::StartTaskRequest;
    use tramontana_platform::{canonicalize_root, discover};

    use super::*;

    #[tokio::test]
    async fn a_restarted_window_waits_for_the_one_before_it_to_let_go() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("desktop.sqlite");
        std::fs::write(&profile, b"").unwrap();
        let held = tramontana_platform::ProfileLease::try_acquire(&profile)
            .unwrap()
            .unwrap();
        // A restart: the old process lets go a moment after the new one
        // starts. Without waiting, the new window could not run tasks.
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(held);
        });

        let lease = acquire_lease(&profile, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(lease.is_some(), "gave up before the previous owner let go");
        release.await.unwrap();
    }

    #[tokio::test]
    async fn a_window_beside_another_stops_waiting_for_the_lease() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("desktop.sqlite");
        std::fs::write(&profile, b"").unwrap();
        let _held = tramontana_platform::ProfileLease::try_acquire(&profile)
            .unwrap()
            .unwrap();

        let started = std::time::Instant::now();
        let lease = tokio::time::timeout(
            Duration::from_secs(5),
            acquire_lease(&profile, Duration::from_millis(300)),
        )
        .await
        .expect("still waiting after its patience ran out")
        .unwrap();
        assert!(lease.is_none());
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[tokio::test]
    async fn a_window_that_reattaches_gets_each_later_event_once_in_order() {
        let directory = tempfile::tempdir().unwrap();
        // Paced, so the task is still running when the window comes back.
        let state = AppState::open_with_chunk_delay(
            &directory.path().join("desktop.sqlite"),
            100,
            Arc::new(DesktopApprover::headless()),
        )
        .await
        .unwrap();
        let task_id = state
            .service
            .start_task(StartTaskRequest {
                goal: "Say hello".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();

        // Wait until the record holds more than the window saw, so the
        // reattached window gets some events from the record and the rest
        // live; a window that came back at once would only test the second.
        tokio::time::timeout(TASK_DEADLINE, async {
            while state
                .service
                .replay_task_events(&task_id, 0)
                .await
                .unwrap()
                .len()
                < 4
            {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap();
        assert!(
            !state
                .service
                .get_task(&task_id)
                .await
                .unwrap()
                .state
                .is_terminal(),
            "the task ended before the window came back, so nothing was live"
        );

        // A window that had seen the first two events, then reloaded.
        let (replayed, updates) = state
            .service
            .subscribe_task_events(&task_id, 2)
            .await
            .unwrap();
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        crate::commands::forward_updates(
            Arc::clone(&state.service),
            task_id.clone(),
            2,
            replayed,
            updates,
            None,
            move |update| sender.send(update).is_ok(),
        );

        let mut sequences = Vec::new();
        tokio::time::timeout(TASK_DEADLINE, async {
            while let Some(update) = received.recv().await {
                if let tramontana_core::contracts::TaskUpdate::Event { event } = update {
                    sequences.push(event.sequence);
                    if event.is_terminal_outcome() {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();

        let recorded: Vec<u64> = state
            .service
            .replay_task_events(&task_id, 2)
            .await
            .unwrap()
            .iter()
            .map(|event| event.sequence)
            .collect();
        assert_eq!(sequences.first(), Some(&3));
        assert_eq!(
            sequences, recorded,
            "a gap, a repeat, or an event out of order"
        );
    }

    #[tokio::test]
    async fn deleting_all_data_leaves_a_profile_that_opens_as_new() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("tramontana.sqlite");
        let project_folder = directory.path().join("my-project");
        std::fs::create_dir_all(&project_folder).unwrap();
        std::fs::write(project_folder.join("notes.md"), "mine").unwrap();
        let state =
            AppState::open_with_chunk_delay(&profile, 0, Arc::new(DesktopApprover::headless()))
                .await
                .unwrap();
        let root = canonicalize_root(&project_folder).unwrap();
        let project = state
            .service
            .open_project("Mine", vec![root.as_str().to_owned()])
            .await
            .unwrap();
        state
            .service
            .create_conversation(Some(project.project_id), "Plans")
            .await
            .unwrap();
        for folder in [
            tramontana_composition::recovery_store(&profile),
            tramontana_composition::browser_profiles(&profile),
            tramontana_composition::app_folder(&profile, "notes"),
        ] {
            std::fs::create_dir_all(&folder).unwrap();
        }

        state.erase_everything().await.unwrap();

        assert!(state.service.list_projects().await.unwrap().is_empty());
        assert!(
            state
                .service
                .list_conversations(None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!tramontana_composition::recovery_store(&profile).exists());
        assert!(!tramontana_composition::browser_profiles(&profile).exists());
        assert!(!tramontana_composition::app_folders(&profile).exists());
        // The project's own folder is the person's, and stays exactly as it was.
        assert_eq!(
            std::fs::read_to_string(project_folder.join("notes.md")).unwrap(),
            "mine"
        );
        drop(state);

        // What the restart opens: an empty, working profile.
        let reopened =
            AppState::open_with_chunk_delay(&profile, 0, Arc::new(DesktopApprover::headless()))
                .await
                .unwrap();
        assert!(reopened.service.list_projects().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn nothing_is_deleted_while_a_task_runs() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("tramontana.sqlite");
        // Paced, so the task is still running when the deletion is asked for.
        let state =
            AppState::open_with_chunk_delay(&profile, 200, Arc::new(DesktopApprover::headless()))
                .await
                .unwrap();
        let conversation = state
            .service
            .create_conversation(None, "Running")
            .await
            .unwrap();
        let task_id = state
            .service
            .start_task(StartTaskRequest {
                conversation_id: Some(conversation.conversation_id),
                goal: "Say hello".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();

        let refused = state.erase_everything().await.unwrap_err();

        assert_eq!(
            refused.code,
            tramontana_core::contracts::ErrorCode::Conflict
        );
        assert_eq!(
            state.service.list_conversations(None).await.unwrap().len(),
            1
        );
        state.service.cancel_task(&task_id).await.unwrap();
    }

    #[tokio::test]
    async fn a_closed_watch_stops_forwarding_while_the_task_runs_on() {
        // A window that turned to another task used to leave its forwarding
        // running, unseen, until the task ended.
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open_with_chunk_delay(
            &temp.path().join("profile.sqlite3"),
            0,
            Arc::new(DesktopApprover::headless()),
        )
        .await
        .unwrap();
        let task_id = tramontana_core::contracts::TaskId::new("task-watched");
        // A live stream that never ends on its own: only closing can stop it.
        let (live, updates) =
            tokio::sync::broadcast::channel::<tramontana_core::contracts::TaskUpdate>(4);
        let (watch_id, stop) = state.watches.open();
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        crate::commands::forward_updates(
            Arc::clone(&state.service),
            task_id,
            0,
            Vec::new(),
            updates,
            Some(stop),
            move |update| sender.send(update).is_ok(),
        );

        state.watches.close(watch_id);
        // The forwarder dropped its sender: the window's end is closed.
        let ended = tokio::time::timeout(TASK_DEADLINE, received.recv())
            .await
            .expect("the forwarding did not stop");
        assert!(
            ended.is_none(),
            "forwarded after the watch closed: {ended:?}"
        );
        drop(live);
    }

    #[tokio::test]
    async fn a_person_reads_the_projects_git_status_and_diff_through_the_desktop() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        let git = |arguments: &[&str]| {
            let status = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(arguments)
                .current_dir(&repository)
                .status()
                .unwrap();
            assert!(status.success(), "git {arguments:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(repository.join("greeting.txt"), "hello\n").unwrap();
        git(&["add", "greeting.txt"]);
        git(&["commit", "-q", "-m", "initial"]);
        std::fs::write(repository.join("greeting.txt"), "hello, world\n").unwrap();
        std::fs::write(repository.join("notes.txt"), "new\n").unwrap();

        let state = AppState::open_with_chunk_delay(
            &directory.path().join("desktop.sqlite"),
            0,
            Arc::new(DesktopApprover::headless()),
        )
        .await
        .unwrap();
        let root = canonicalize_root(&repository).unwrap();
        let project = state
            .service
            .open_project("Diff fixture", vec![root.as_str().to_owned()])
            .await
            .unwrap();

        // The same tool ids the command asks for, through the same
        // composition, so neither can drift from what the core will run.
        let diff = state
            .service
            .read_project(&project.project_id, crate::commands::GIT_DIFF)
            .await
            .unwrap();
        let diff = diff.content["text"].as_str().unwrap().to_owned();
        assert!(diff.contains("-hello\n+hello, world"), "{diff}");
        let status = state
            .service
            .read_project(&project.project_id, crate::commands::GIT_STATUS)
            .await
            .unwrap();
        let status = status.content["text"].as_str().unwrap().to_owned();
        assert!(status.contains("?? notes.txt"), "{status}");
        assert!(state.service.list_tasks(None).await.unwrap().is_empty());
    }

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
        tokio::time::timeout(TASK_DEADLINE, async {
            while !state
                .service
                .get_task(&task_id)
                .await
                .unwrap()
                .state
                .is_terminal()
            {
                tokio::time::sleep(POLL).await;
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

        tokio::time::timeout(TASK_DEADLINE, async {
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
                tokio::time::sleep(POLL).await;
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
