//! Writing, inspecting and importing bundles for the window.
//!
//! The commands only ask the native picker where; the work is here, where it
//! can be tested without a window. It mirrors the CLI's `bundle` commands:
//! export says what it withheld, inspection writes nothing, and import runs
//! nothing a bundle contains.

use std::path::{Path, PathBuf};

use tramontana_core::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, ExportBundle, ExportManifest, ExportOptions,
    ProjectId, SCHEMA_VERSION, TaskId,
};
use tramontana_core::services::{ImportLimits, TaskService};

/// What to export.
#[derive(Debug, Clone)]
pub(crate) enum ExportScope {
    /// One task, with its conversation and the project it belongs to.
    Task(TaskId),
    /// A whole project: its conversations and their tasks.
    Project(ProjectId),
}

/// A bundle folder the person inspected, waiting for them to confirm.
///
/// Held by the shell, so the window confirms by identifier and never hands a
/// filesystem path back.
#[derive(Debug, Clone)]
pub(crate) struct StagedBundle {
    pub(crate) id: String,
    pub(crate) directory: PathBuf,
}

/// Where the shell keeps the one bundle waiting to be confirmed.
pub(crate) type StagedSlot = std::sync::Mutex<Option<StagedBundle>>;

/// Remember an inspected folder under `id`, replacing any earlier one.
pub(crate) fn stage(slot: &StagedSlot, id: String, directory: PathBuf) {
    if let Ok(mut staged) = slot.lock() {
        *staged = Some(StagedBundle { id, directory });
    }
}

/// Take the folder staged under `id`, once.
///
/// A confirmation naming anything else empties the slot as well: the person
/// inspects again rather than import a bundle they were not shown.
pub(crate) fn take_staged(slot: &StagedSlot, id: &str) -> Option<PathBuf> {
    slot.lock()
        .ok()?
        .take()
        .filter(|staged| staged.id == id)
        .map(|staged| staged.directory)
}

/// How many records of each kind a bundle holds, or an import wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleCounts {
    pub projects: usize,
    pub conversations: usize,
    pub tasks: usize,
    pub events: usize,
    pub checkpoints: usize,
    pub configurations: usize,
    /// Opaque worker state that is refused rather than restored.
    pub quarantined: usize,
}

/// What a bundle holds, read without writing anything.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleInspection {
    pub schema_version: u32,
    /// Its table of contents, including what the export left out.
    pub manifest: ExportManifest,
    pub contains: BundleCounts,
}

/// What an import wrote.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReport {
    pub schema_version: u32,
    pub imported: BundleCounts,
    /// Always false, and stated rather than assumed: an imported task is a
    /// record, and nothing in the bundle was run to produce it.
    pub executed_anything: bool,
}

/// Write a bundle into `directory`, returning its manifest.
///
/// # Errors
///
/// Whatever the export reports, or [`ErrorCode::PersistenceError`] when the
/// bundle could not be written.
pub(crate) async fn export_to(
    service: &TaskService,
    scope: &ExportScope,
    options: ExportOptions,
    directory: &Path,
) -> Result<ExportManifest, ErrorEnvelope> {
    let bundle = match scope {
        ExportScope::Task(task_id) => service.export_task(task_id, options).await?,
        ExportScope::Project(project_id) => {
            service.export_project(Some(project_id), options).await?
        }
    };
    tramontana_platform::write_bundle(&bundle, directory).map_err(|error| {
        failure(
            ErrorCode::PersistenceError,
            format!("could not write the bundle: {error}"),
        )
    })?;
    Ok(bundle.manifest)
}

/// Read and validate a bundle, writing nothing.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when the folder is not a bundle, or the
/// bundle breaks a rule.
pub(crate) fn inspect(
    service: &TaskService,
    directory: &Path,
) -> Result<BundleInspection, ErrorEnvelope> {
    let bundle = read(directory)?;
    let records = service.stage_import(&bundle, ImportLimits::default())?;
    Ok(BundleInspection {
        schema_version: SCHEMA_VERSION,
        contains: BundleCounts {
            projects: records.projects.len(),
            conversations: records.conversations.len(),
            tasks: records.tasks.len(),
            events: records.events.values().map(Vec::len).sum(),
            checkpoints: records.checkpoints.len(),
            configurations: records.configurations.len(),
            quarantined: records.quarantined.len(),
        },
        manifest: bundle.manifest,
    })
}

/// Validate a bundle again and write its records into this profile.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when the bundle is refused, or
/// [`ErrorCode::Conflict`] when a record it holds already exists here.
pub(crate) async fn import_from(
    service: &TaskService,
    directory: &Path,
) -> Result<ImportReport, ErrorEnvelope> {
    let bundle = read(directory)?;
    let summary = service
        .import_bundle(&bundle, ImportLimits::default())
        .await?;
    Ok(ImportReport {
        schema_version: SCHEMA_VERSION,
        imported: BundleCounts {
            projects: summary.projects,
            conversations: summary.conversations,
            tasks: summary.tasks,
            events: summary.events,
            checkpoints: summary.checkpoints,
            configurations: summary.configurations,
            quarantined: summary.quarantined,
        },
        executed_anything: false,
    })
}

fn read(directory: &Path) -> Result<ExportBundle, ErrorEnvelope> {
    tramontana_platform::read_bundle(directory).map_err(|error| {
        failure(
            ErrorCode::InvalidRequest,
            format!("could not read the bundle: {error}"),
        )
    })
}

fn failure(code: ErrorCode, message: String) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("ipc"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tramontana_core::config::ConfigurationSelection;
    use tramontana_core::contracts::EventEnvelope;
    use tramontana_core::services::StartTaskRequest;
    use tramontana_platform::{canonicalize_root, discover};

    use super::*;
    use crate::approval::DesktopApprover;
    use crate::state::AppState;

    async fn open(path: &Path) -> AppState {
        AppState::open_with_chunk_delay(path, 0, Arc::new(DesktopApprover::headless()))
            .await
            .unwrap()
    }

    /// A profile holding one finished task in a project.
    async fn finished_task(state: &AppState, project_directory: &Path) -> TaskId {
        std::fs::create_dir_all(project_directory).unwrap();
        let root = canonicalize_root(project_directory).unwrap();
        let project = state
            .service
            .open_project("Bundle fixture", vec![root.as_str().to_owned()])
            .await
            .unwrap();
        state
            .service
            .resolve_configuration(
                &project.project_id,
                discover(&root),
                &ConfigurationSelection::default(),
            )
            .await
            .unwrap();
        let conversation = state
            .service
            .create_conversation(Some(project.project_id), "Bundles")
            .await
            .unwrap();
        let task_id = state
            .service
            .start_task(StartTaskRequest {
                conversation_id: Some(conversation.conversation_id),
                goal: "Leave a record".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        // Generous, because a loaded CI runner can take seconds to run the
        // fixture's task; checked every few milliseconds rather than spun on,
        // so the wait does not compete with the task for the database.
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                // The outcome event itself, not the state: the state turns
                // terminal one event before `taskCompleted` is written, and a
                // record read in between is one event short of the export.
                let events = state.service.replay_task_events(&task_id, 0).await.unwrap();
                if events
                    .last()
                    .is_some_and(EventEnvelope::is_terminal_outcome)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task_id
    }

    #[tokio::test]
    async fn a_task_exported_from_one_profile_is_inspected_then_imported_into_another_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let first = open(&directory.path().join("first.sqlite")).await;
        let task_id = finished_task(&first, &directory.path().join("project")).await;
        let original = first.service.replay_task_events(&task_id, 0).await.unwrap();

        let bundle = directory.path().join("bundle");
        let manifest = export_to(
            &first.service,
            &ExportScope::Task(task_id.clone()),
            ExportOptions::default(),
            &bundle,
        )
        .await
        .unwrap();
        assert_eq!(manifest.task_ids, std::slice::from_ref(&task_id));
        assert!(!manifest.options.include_raw_tool_output);
        // Withheld by default, and said so: the manifest names both.
        for withheld in [
            tramontana_core::contracts::OmissionCategory::AbsolutePath,
            tramontana_core::contracts::OmissionCategory::RawToolOutput,
        ] {
            assert!(
                manifest
                    .omissions
                    .iter()
                    .any(|omission| omission.category == withheld && omission.count > 0),
                "the manifest does not say it withheld {withheld:?}"
            );
        }

        let second = open(&directory.path().join("second.sqlite")).await;
        let inspection = inspect(&second.service, &bundle).unwrap();
        assert_eq!(inspection.contains.tasks, 1);
        assert_eq!(inspection.contains.events, original.len());
        assert!(
            second.service.list_tasks(None).await.unwrap().is_empty(),
            "inspecting a bundle wrote into the profile"
        );

        let report = import_from(&second.service, &bundle).await.unwrap();
        assert_eq!(report.imported.tasks, 1);
        assert_eq!(report.imported.events, original.len());
        assert!(!report.executed_anything);
        let imported = second
            .service
            .replay_task_events(&task_id, 0)
            .await
            .unwrap();
        // The same record, event for event, except where the export withheld
        // something: a device path or a raw tool output is replaced by a
        // marker, which is why payloads are not compared whole.
        assert_eq!(
            imported
                .iter()
                .map(|event| (event.sequence, event.kind))
                .collect::<Vec<_>>(),
            original
                .iter()
                .map(|event| (event.sequence, event.kind))
                .collect::<Vec<_>>()
        );
        let root = canonicalize_root(&directory.path().join("project")).unwrap();
        // Searched for as JSON writes it: a Windows path's backslashes are
        // escaped there, and the raw path would never be found, which would
        // also let the second check pass without proving anything.
        let quoted = serde_json::to_string(root.as_str()).unwrap();
        let needle = quoted.trim_matches('"');
        assert!(
            serde_json::to_string(&original).unwrap().contains(needle),
            "the original never named the device path, so this proves nothing"
        );
        assert!(
            !serde_json::to_string(&imported).unwrap().contains(needle),
            "a device path travelled in the bundle"
        );

        let again = import_from(&second.service, &bundle).await.unwrap_err();
        assert_eq!(
            again.code,
            ErrorCode::Conflict,
            "a second import was refused, but not as a conflict: {again:?}"
        );
    }

    #[test]
    fn an_import_confirms_only_the_bundle_last_inspected_and_only_once() {
        let slot = StagedSlot::new(None);
        stage(
            &slot,
            "bundle-1".to_owned(),
            PathBuf::from("/chosen/by/picker"),
        );
        assert_eq!(
            take_staged(&slot, "bundle-1"),
            Some(PathBuf::from("/chosen/by/picker"))
        );
        assert_eq!(take_staged(&slot, "bundle-1"), None, "confirmed twice");

        stage(
            &slot,
            "bundle-2".to_owned(),
            PathBuf::from("/chosen/by/picker"),
        );
        assert_eq!(take_staged(&slot, "bundle-other"), None);
        assert_eq!(
            take_staged(&slot, "bundle-2"),
            None,
            "a mismatched confirmation left the bundle staged"
        );
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_bundle_is_refused_with_a_reason() {
        let directory = tempfile::tempdir().unwrap();
        let state = open(&directory.path().join("profile.sqlite")).await;
        let empty = directory.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let refused = inspect(&state.service, &empty).unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidRequest);
        assert!(refused.message.starts_with("could not read the bundle"));
    }
}
