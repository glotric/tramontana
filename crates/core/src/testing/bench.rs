//! A whole service over in-memory parts, and the waits and streams that
//! integration tests drive it with. Every integration test assembled these
//! by hand; one copy keeps them from drifting apart.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    FakeClock, InMemoryApprovalStore, SequentialIdSource, assemble_tool_plane,
    in_memory_repositories,
};
use crate::contracts::{
    ConversationId, CorrelationId, ErrorCode, ErrorEnvelope, EventEnvelope, ModelEvent, StopReason,
    TaskEventKind, TaskId, TaskState,
};
use crate::ports::{AgentProvider, Approver, EventStore, IdSource, ModelProvider, ToolProvider};
use crate::services::{Providers, StartTaskRequest, TaskService};

/// Providers over `tools`, with a fresh approval store and `approver`
/// answering both tool and release questions, numbered by `ids`.
///
/// # Panics
///
/// When a tool provider offers a tool the registry refuses.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "a test double the registry refuses is a broken test"
)]
pub fn test_providers(
    models: Vec<Arc<dyn ModelProvider>>,
    tools: Vec<Arc<dyn ToolProvider>>,
    approver: Arc<dyn Approver>,
    worker: Arc<dyn AgentProvider>,
    ids: Arc<dyn IdSource>,
) -> Providers {
    let (tools, permissions, releases) =
        assemble_tool_plane(tools, Arc::new(InMemoryApprovalStore::new()), approver, ids)
            .expect("test tools register");
    Providers {
        models,
        tools,
        permissions,
        releases,
        worker,
    }
}

/// A service over `events` and the fake clock, with one id source shared by
/// the service and its tool plane.
///
/// # Panics
///
/// As [`test_providers`].
#[must_use]
pub fn test_service(
    events: Arc<dyn EventStore>,
    models: Vec<Arc<dyn ModelProvider>>,
    tools: Vec<Arc<dyn ToolProvider>>,
    approver: Arc<dyn Approver>,
    worker: Arc<dyn AgentProvider>,
) -> TaskService {
    let ids = Arc::new(SequentialIdSource::new());
    TaskService::new(
        in_memory_repositories(events),
        test_providers(models, tools, approver, worker, ids.clone()),
        Arc::new(FakeClock::new()),
        ids,
    )
}

/// A request to run `goal` in `conversation`, with every choice defaulted.
#[must_use]
pub fn start_request(conversation: &ConversationId, goal: impl Into<String>) -> StartTaskRequest {
    StartTaskRequest {
        conversation_id: Some(conversation.clone()),
        goal: goal.into(),
        ..StartTaskRequest::default()
    }
}

/// A conversation inside a new project rooted at `/work/demo`: tools are
/// only permitted inside an opened project.
///
/// # Panics
///
/// When the service refuses either.
#[allow(
    clippy::expect_used,
    reason = "an in-memory service that cannot create a project is a broken test"
)]
pub async fn conversation_in_a_project(service: &TaskService) -> ConversationId {
    let project = service
        .create_project("Demo", vec!["/work/demo".to_owned()])
        .await
        .expect("project");
    service
        .create_conversation(Some(project.project_id), "First")
        .await
        .expect("conversation")
        .conversation_id
}

/// Poll every `every` until the task's state satisfies `done`.
///
/// # Panics
///
/// When `limit` passes first, or the task cannot be read.
#[allow(
    clippy::expect_used,
    reason = "a wait that never ends is the failure report"
)]
pub async fn wait_state(
    service: &TaskService,
    task: &TaskId,
    limit: Duration,
    every: Duration,
    done: impl Fn(TaskState) -> bool,
) -> TaskState {
    tokio::time::timeout(limit, async {
        loop {
            let state = service.get_task(task).await.expect("task").state;
            if done(state) {
                return state;
            }
            tokio::time::sleep(every).await;
        }
    })
    .await
    .expect("the task never settled")
}

/// Poll every `every` until the task's events satisfy `done`, and return them.
///
/// # Panics
///
/// When `limit` passes first, or the events cannot be read.
#[allow(
    clippy::expect_used,
    reason = "a wait that never ends is the failure report"
)]
pub async fn wait_events(
    service: &TaskService,
    task: &TaskId,
    limit: Duration,
    every: Duration,
    done: impl Fn(&[EventEnvelope]) -> bool,
) -> Vec<EventEnvelope> {
    tokio::time::timeout(limit, async {
        loop {
            let events = service.replay_task_events(task, 0).await.expect("events");
            if done(&events) {
                return events;
            }
            tokio::time::sleep(every).await;
        }
    })
    .await
    .expect("the task never finished")
}

/// Whether the last event ends the task: completed, failed or cancelled.
#[must_use]
pub fn ended(events: &[EventEnvelope]) -> bool {
    events.last().is_some_and(|event| {
        matches!(
            event.kind,
            TaskEventKind::TaskCompleted | TaskEventKind::TaskFailed | TaskEventKind::TaskCancelled
        )
    })
}

/// A finished model stream that answers `text`.
#[must_use]
pub fn answer_stream(stream_id: String, text: impl Into<String>) -> mpsc::Receiver<ModelEvent> {
    finished(
        ModelEvent::TextDelta {
            stream_id,
            offset: 0,
            text: text.into(),
        },
        StopReason::Stop,
    )
}

/// A finished model stream that calls `name` with `arguments`, as `call_id`.
#[must_use]
pub fn tool_call_stream(
    stream_id: String,
    call_id: impl Into<String>,
    name: impl Into<String>,
    arguments: impl Into<String>,
) -> mpsc::Receiver<ModelEvent> {
    finished(
        ModelEvent::ToolCallDelta {
            stream_id,
            offset: 0,
            call_index: 0,
            call_id: Some(call_id.into()),
            name: Some(name.into()),
            delta: arguments.into(),
        },
        StopReason::ToolCall,
    )
}

fn finished(event: ModelEvent, stop_reason: StopReason) -> mpsc::Receiver<ModelEvent> {
    let (sender, receiver) = mpsc::channel(2);
    // A fresh channel with room for both: neither send can fail.
    let _ = sender.try_send(event);
    let _ = sender.try_send(ModelEvent::Completed { stop_reason });
    receiver
}

/// Hold a model request until it is cancelled, then fail it as cancelled.
pub async fn held_until_cancelled(cancel: &CancellationToken) -> ErrorEnvelope {
    cancel.cancelled().await;
    ErrorEnvelope::new(
        ErrorCode::Cancelled,
        "cancelled",
        CorrelationId::new("test"),
    )
}
