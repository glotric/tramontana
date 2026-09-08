//! The versioned command boundary.
//!
//! Each command validates its payload, calls an application service, and maps
//! the outcome to a normalized [`ErrorEnvelope`] (`docs/DEVELOPMENT.md` §9).
//! Filesystem paths enter only through the native directory picker, are
//! canonicalized by the platform adapter, and become explicit project roots.
//! No command accepts a shell string, and none makes a decision the core has
//! not already made.

use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{
    ConversationId, ConversationRecord, CorrelationId, EffectiveConfiguration, ErrorCode,
    ErrorEnvelope, EventEnvelope, ProjectId, ProjectRecord, SCHEMA_VERSION, TaskId, TaskRecord,
    TaskSnapshot, TaskUpdate,
};
use tramontana_core::services::StartTaskRequest;
use tramontana_platform::{CanonicalRoot, canonicalize_root, discover};

use crate::state::AppState;

/// Longest goal this boundary accepts.
///
/// Payloads are size-checked before reaching a service: the webview is
/// untrusted input like any other, however friendly it looks.
const MAX_GOAL_BYTES: usize = 4_096;

/// Longest conversation title accepted from the webview.
const MAX_TITLE_BYTES: usize = 512;

/// Version information for the startup handshake.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoreInfo {
    /// Contract schema version reported by the running core.
    pub schema_version: u32,
    /// Version of the desktop shell.
    pub app_version: &'static str,
}

/// Report the contract schema and application version.
#[tauri::command]
pub(crate) fn core_info() -> CoreInfo {
    CoreInfo {
        schema_version: tramontana_core::SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION"),
    }
}

/// Project plus the effective configuration most recently resolved for it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectInspection {
    /// Version of this response envelope.
    pub schema_version: u32,
    /// Durable project record.
    pub project: ProjectRecord,
    /// Latest secret-free configuration manifest, when one exists.
    pub configuration: Option<EffectiveConfiguration>,
}

/// Show the native project-folder picker, then open and resolve that project.
///
/// Cancelling the picker returns `None` and changes no state. The webview never
/// supplies a filesystem path; the native adapter receives the user selection,
/// canonicalizes it, and passes only an approved root into core.
#[tauri::command]
pub(crate) async fn open_project(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ProjectInspection>, ErrorEnvelope> {
    let Some(root) = pick_project_root(&app).await? else {
        return Ok(None);
    };
    let name = root
        .as_path()
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Untitled project")
        .to_owned();
    let project = state
        .service
        .open_project(name, vec![root.as_str().to_owned()])
        .await?;
    let configuration = state
        .service
        .resolve_configuration(
            &project.project_id,
            discover(&root),
            &ConfigurationSelection::default(),
        )
        .await?;

    Ok(Some(ProjectInspection {
        schema_version: SCHEMA_VERSION,
        project,
        configuration: Some(configuration),
    }))
}

/// List recent projects from the durable profile.
#[tauri::command]
pub(crate) async fn list_projects(
    state: State<'_, AppState>,
) -> Result<Vec<ProjectRecord>, ErrorEnvelope> {
    state.service.list_projects().await
}

/// Inspect a durable project and its latest resolved configuration.
#[tauri::command]
pub(crate) async fn inspect_project(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<ProjectInspection, ErrorEnvelope> {
    let project = state.service.get_project(&project_id).await?;
    let configuration = state
        .service
        .get_effective_configuration(&project_id)
        .await?;
    Ok(ProjectInspection {
        schema_version: SCHEMA_VERSION,
        project,
        configuration,
    })
}

/// Create a durable conversation in a project.
#[tauri::command]
pub(crate) async fn create_conversation(
    project_id: ProjectId,
    title: String,
    state: State<'_, AppState>,
) -> Result<ConversationRecord, ErrorEnvelope> {
    let title = title.trim().to_owned();
    if title.is_empty() {
        return Err(invalid("a conversation needs a title"));
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(invalid(format!(
            "a conversation title may be at most {MAX_TITLE_BYTES} bytes"
        )));
    }
    state
        .service
        .create_conversation(Some(project_id), title)
        .await
}

/// List durable conversations for one project.
#[tauri::command]
pub(crate) async fn list_conversations(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<Vec<ConversationRecord>, ErrorEnvelope> {
    state.service.list_conversations(Some(&project_id)).await
}

/// List durable tasks, optionally narrowed to a conversation.
#[tauri::command]
pub(crate) async fn list_tasks(
    conversation_id: Option<ConversationId>,
    state: State<'_, AppState>,
) -> Result<Vec<TaskRecord>, ErrorEnvelope> {
    state.service.list_tasks(conversation_id.as_ref()).await
}

/// Replay the canonical event record for one task.
#[tauri::command]
pub(crate) async fn replay_task_events(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<Vec<EventEnvelope>, ErrorEnvelope> {
    state.service.replay_task_events(&task_id, 0).await
}

/// Start a task and stream its updates over `channel`.
///
/// Returns the task identifier immediately; the work continues in the
/// background and reports through the channel. A long-running operation must
/// never block the interface (`CORE_CONTRACTS.md` §13).
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for an empty or oversized goal, or whatever
/// the service reports.
#[tauri::command]
pub(crate) async fn start_task(
    goal: String,
    conversation_id: Option<ConversationId>,
    channel: Channel<TaskUpdate>,
    state: State<'_, AppState>,
) -> Result<TaskId, ErrorEnvelope> {
    let goal = goal.trim().to_owned();
    if goal.is_empty() {
        return Err(invalid("a task needs a goal"));
    }
    if goal.len() > MAX_GOAL_BYTES {
        return Err(invalid(format!(
            "a goal may be at most {MAX_GOAL_BYTES} bytes"
        )));
    }

    let service = state.service.clone();
    if let Some(conversation_id) = &conversation_id {
        let conversation = service.get_conversation(conversation_id).await?;
        if let Some(project_id) = conversation.project_id {
            resolve_stored_project(&service, &project_id).await?;
        }
    }
    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id,
            goal,
        })
        .await?;

    // Subscribe from the beginning so the interface sees the creation event
    // even though it was written before this call returned.
    let (replayed, mut updates) = service.subscribe_task_events(&task_id, 0).await?;

    let forward_task_id = task_id.clone();
    tokio::spawn(async move {
        let mut highest = 0;
        for event in replayed {
            highest = highest.max(event.sequence);
            if channel
                .send(TaskUpdate::Event {
                    event: Box::new(event),
                })
                .is_err()
            {
                return; // The window went away.
            }
        }

        loop {
            match updates.recv().await {
                Ok(TaskUpdate::Event { event }) => {
                    // Subscription precedes replay, so an event can arrive
                    // twice. Dropping the duplicate here keeps the interface
                    // from having to know that.
                    if event.sequence <= highest {
                        continue;
                    }
                    highest = event.sequence;
                    let terminal = matches!(
                        &event.payload,
                        tramontana_core::contracts::EventPayload::TaskCompleted { .. }
                            | tramontana_core::contracts::EventPayload::TaskFailed { .. }
                            | tramontana_core::contracts::EventPayload::TaskCancelled { .. }
                    );
                    if channel.send(TaskUpdate::Event { event }).is_err() {
                        return;
                    }
                    if terminal {
                        return;
                    }
                }
                Ok(update) => {
                    if channel.send(update).is_err() {
                        return;
                    }
                }
                // Lagging is recoverable: durable events are in the store, so
                // catch up rather than reporting a failure to the user.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let Ok((missed, _)) = service
                        .subscribe_task_events(&forward_task_id, highest)
                        .await
                    else {
                        return;
                    };
                    for event in missed {
                        highest = highest.max(event.sequence);
                        if channel
                            .send(TaskUpdate::Event {
                                event: Box::new(event),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    Ok(task_id)
}

async fn pick_project_root(app: &AppHandle) -> Result<Option<CanonicalRoot>, ErrorEnvelope> {
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Open a Tramontana project")
        .pick_folder(move |selection| {
            let _ = send.send(selection);
        });
    let selection = receive
        .await
        .map_err(|_| internal("the project picker closed unexpectedly"))?;
    let Some(selection) = selection else {
        return Ok(None);
    };
    let path = selection
        .into_path()
        .map_err(|_| invalid("the selected project is not a local directory"))?;
    canonicalize_root(&path)
        .map(Some)
        .map_err(|error| invalid(format!("could not open the selected project: {error}")))
}

async fn resolve_stored_project(
    service: &tramontana_core::services::TaskService,
    project_id: &ProjectId,
) -> Result<EffectiveConfiguration, ErrorEnvelope> {
    let project = service.get_project(project_id).await?;
    let [root] = project.roots.as_slice() else {
        return Err(invalid(format!(
            "project {project_id} has {} roots; M2 configuration discovery supports the default single-root project case",
            project.roots.len()
        )));
    };
    let root = canonicalize_root(std::path::Path::new(root)).map_err(|error| {
        invalid(format!(
            "could not reopen the approved project root: {error}"
        ))
    })?;
    service
        .resolve_configuration(
            project_id,
            discover(&root),
            &ConfigurationSelection::default(),
        )
        .await
}

/// Request cancellation of a running task.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) async fn cancel_task(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.cancel_task(&task_id).await
}

/// Read the current view of a task.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) async fn get_task(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<TaskSnapshot, ErrorEnvelope> {
    state.service.get_task(&task_id).await
}

/// Build an invalid-request error for a boundary rejection.
fn invalid(message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidRequest,
        message,
        CorrelationId::new("ipc"),
    )
}

fn internal(message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(ErrorCode::Internal, message, CorrelationId::new("ipc"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn core_info_reports_the_core_schema_version() {
        let info = core_info();
        assert_eq!(info.schema_version, tramontana_core::SCHEMA_VERSION);
        assert!(!info.app_version.is_empty());
    }

    #[test]
    fn boundary_rejections_are_normalized_errors() {
        let error = invalid("a task needs a goal");
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(!error.retryable, "retrying an invalid payload cannot help");
    }
}
