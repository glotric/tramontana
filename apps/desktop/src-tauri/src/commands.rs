//! The versioned command boundary.
//!
//! Each command validates its payload, calls an application service, and maps
//! the outcome to a normalized [`ErrorEnvelope`] (`docs/DEVELOPMENT.md` §9).
//! No command accepts a filesystem path or a shell string, and none makes a
//! decision the core has not already made.

use tauri::State;
use tauri::ipc::Channel;
use tramontana_core::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, TaskId, TaskSnapshot, TaskUpdate,
};
use tramontana_core::services::StartTaskRequest;

use crate::state::AppState;

/// Longest goal this boundary accepts.
///
/// Payloads are size-checked before reaching a service: the webview is
/// untrusted input like any other, however friendly it looks.
const MAX_GOAL_BYTES: usize = 4_096;

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
    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
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
                    if channel.send(TaskUpdate::Event { event }).is_err() {
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

/// Request cancellation of a running task.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) fn cancel_task(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.cancel_task(&task_id)
}

/// Read the current view of a task.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) fn get_task(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<TaskSnapshot, ErrorEnvelope> {
    state.service.get_task(&task_id)
}

/// Build an invalid-request error for a boundary rejection.
fn invalid(message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidRequest,
        message,
        CorrelationId::new("ipc"),
    )
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
