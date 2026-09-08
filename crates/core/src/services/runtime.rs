use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    CancellationOrigin, ErrorCode, ErrorEnvelope, EventDraft, EventPayload, InvocationId,
    StreamFrame, TaskEventKind, TaskState,
};
use crate::ports::{
    ModelEvent, ModelProvider, ModelRequest, ToolInvocation, ToolProvider,
    WorkerHost, WorkerOutcome, WorkerRequest,
};
use crate::services::journal::TaskJournal;
use crate::services::projection::TaskProjectionWriter;
use crate::services::task_service::Providers;

/// Drives one task through its state machine.
///
/// Every state change is validated against [`TaskState::transition_to`] before
/// it is written, so an illegal move is a bug caught here rather than an
/// impossible record in the stream. The driver is also the only thing that
/// writes state changes, which is what keeps "exactly one terminal state" true.
pub(crate) struct TaskDriver {
    journal: TaskJournal,
    providers: Providers,
    projection: TaskProjectionWriter,
}

impl TaskDriver {
    pub(crate) fn new(
        journal: TaskJournal,
        providers: Providers,
        projection: TaskProjectionWriter,
    ) -> Self {
        Self {
            journal,
            providers,
            projection,
        }
    }

    /// Run the task to exactly one terminal state.
    pub(crate) async fn run(self, goal: String, cancel: CancellationToken) {
        let mut state = TaskState::Queued;

        // Each step checks cancellation first. A cancel that arrives before the
        // task really starts still produces a proper Cancelling -> Cancelled
        // pair rather than a task that never moved.
        for next in [
            TaskState::Preparing,
            TaskState::Routing,
            TaskState::StartingWorker,
        ] {
            if cancel.is_cancelled() {
                self.finish_cancelled(state).await;
                return;
            }
            match self.change_state(state, next, "task.advanced").await {
                Ok(current) => state = current,
                Err(error) => {
                    self.finish_failed(state, error).await;
                    return;
                }
            }
        }

        if let Err(error) = self
            .record(EventDraft::user(
                TaskEventKind::WorkerSelected,
                EventPayload::WorkerSelected {
                    agent_id: self.providers.worker.describe().agent_id,
                    display_name: self.providers.worker.describe().display_name,
                },
            ))
            .await
        {
            self.finish_failed(state, error).await;
            return;
        }

        state = match self
            .change_state(state, TaskState::Running, "worker.started")
            .await
        {
            Ok(current) => current,
            Err(error) => {
                self.finish_failed(state, error).await;
                return;
            }
        };

        let request = WorkerRequest {
            task_id: self.journal.task_id().clone(),
            goal,
        };
        let host = BrokeredHost {
            journal: self.journal.clone(),
            model: Arc::clone(&self.providers.model),
            tools: Arc::clone(&self.providers.tools),
        };

        match self
            .providers
            .worker
            .run(request, &host, cancel.clone())
            .await
        {
            Ok(WorkerOutcome::Completed { summary }) if !cancel.is_cancelled() => {
                self.finish_completed(state, summary).await;
            }
            // A worker that finished after cancellation was requested still
            // ends as cancelled: the user's decision outranks the late result.
            Ok(_) => self.finish_cancelled(state).await,
            Err(error) => self.finish_failed(state, error).await,
        }
    }

    /// Validate and journal a state change.
    async fn change_state(
        &self,
        from: TaskState,
        to: TaskState,
        reason_code: &str,
    ) -> Result<TaskState, ErrorEnvelope> {
        let current = from.transition_to(to).map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::Internal,
                error.to_string(),
                self.journal.correlation_id().clone(),
            )
        })?;

        self.record(EventDraft::user(
            TaskEventKind::TaskStateChanged,
            EventPayload::TaskStateChanged {
                previous: from,
                current,
                reason_code: reason_code.to_owned(),
            },
        ))
        .await?;

        Ok(current)
    }

    async fn record(&self, draft: EventDraft) -> Result<(), ErrorEnvelope> {
        let event = self.journal.append(draft).await?;
        self.projection.record(&event).await;
        Ok(())
    }

    async fn finish_completed(&self, from: TaskState, summary: String) {
        if self
            .change_state(from, TaskState::Completed, "worker.completed")
            .await
            .is_ok()
        {
            let _ = self
                .record(EventDraft::user(
                    TaskEventKind::TaskCompleted,
                    EventPayload::TaskCompleted { summary },
                ))
                .await;
        }
    }

    async fn finish_failed(&self, from: TaskState, error: ErrorEnvelope) {
        if self
            .change_state(from, TaskState::Failed, "task.failed")
            .await
            .is_ok()
        {
            let _ = self
                .record(EventDraft::user(
                    TaskEventKind::TaskFailed,
                    EventPayload::TaskFailed { error },
                ))
                .await;
        }
    }

    /// Converge to `Cancelled` through `Cancelling`.
    ///
    /// The intermediate state is not ceremony: it is the durable record that
    /// cancellation was requested, which is what distinguishes a task the user
    /// stopped from one that merely ended.
    async fn finish_cancelled(&self, from: TaskState) {
        let Ok(cancelling) = self
            .change_state(from, TaskState::Cancelling, "user.cancelled")
            .await
        else {
            return;
        };
        if self
            .change_state(cancelling, TaskState::Cancelled, "cleanup.complete")
            .await
            .is_ok()
        {
            let _ = self
                .record(EventDraft::user(
                    TaskEventKind::TaskCancelled,
                    EventPayload::TaskCancelled {
                        origin: CancellationOrigin::User,
                    },
                ))
                .await;
        }
    }
}

/// The broker a worker sees.
///
/// This is the whole point of [`WorkerHost`]: the worker holds no provider
/// handles, so a model call or a tool call is a *request* that the core
/// fulfils and records. From M3 the same path carries the permission decision,
/// and nothing about the worker has to change.
struct BrokeredHost {
    journal: TaskJournal,
    model: Arc<dyn ModelProvider>,
    tools: Arc<dyn ToolProvider>,
}

#[async_trait]
impl WorkerHost for BrokeredHost {
    async fn emit(&self, draft: EventDraft) -> Result<(), ErrorEnvelope> {
        self.journal.append(draft).await.map(|_| ())
    }

    async fn publish_frame(&self, frame: StreamFrame) {
        self.journal.publish_frame(frame);
    }

    async fn call_model(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ErrorEnvelope> {
        let request = ModelRequest {
            request_id: format!("request-{}", self.journal.task_id()),
            prompt: prompt.to_owned(),
        };
        let mut stream = self.model.stream(request, cancel.clone()).await?;

        // Deltas are coalesced here and committed once. Persisting every token
        // would make the durable stream enormous and tell the reader nothing
        // the final message does not (`CORE_CONTRACTS.md` §5).
        let mut answer = String::new();
        while let Some(event) = stream.recv().await {
            match event {
                ModelEvent::TextDelta { text, .. } => {
                    self.journal.publish_frame(StreamFrame {
                        stream_id: format!("stream-{}", self.journal.task_id()),
                        offset: answer.len() as u64,
                        text: text.clone(),
                    });
                    answer.push_str(&text);
                }
                ModelEvent::Completed { .. } => break,
                ModelEvent::Failed { error } => return Err(error),
            }
        }

        Ok(answer)
    }

    async fn call_tool(
        &self,
        tool_id: &str,
        arguments: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, ErrorEnvelope> {
        let invocation_id = InvocationId::new(format!("invocation-{tool_id}"));

        let descriptor = self
            .tools
            .list_tools()
            .into_iter()
            .find(|tool| tool.tool_id == tool_id)
            .ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::NotFound,
                    format!("unknown tool {tool_id}"),
                    self.journal.correlation_id().clone(),
                )
            })?;

        self.journal
            .append(EventDraft::user(
                TaskEventKind::ToolRequested,
                EventPayload::ToolRequested {
                    invocation_id: invocation_id.clone(),
                    tool_id: tool_id.to_owned(),
                    arguments_summary: summarize_arguments(&arguments),
                    risk_class: descriptor.risk_class,
                },
            ))
            .await?;

        let outcome = self
            .tools
            .invoke(
                ToolInvocation {
                    invocation_id: invocation_id.clone(),
                    tool_id: tool_id.to_owned(),
                    arguments,
                },
                cancel.clone(),
            )
            .await?;

        self.journal
            .append(EventDraft::user(
                TaskEventKind::ToolCompleted,
                EventPayload::ToolCompleted {
                    invocation_id,
                    display_summary: outcome.display_summary.clone(),
                    side_effect: outcome.side_effect,
                    truncated: outcome.truncated,
                },
            ))
            .await?;

        Ok(outcome.content)
    }
}

/// Describe tool arguments without reproducing them.
///
/// Argument values can carry file content, paths, or other project data, and
/// the durable event stream is shown to the user and exported. The names and
/// shape are enough for an activity card; the values are not the stream's to
/// keep.
fn summarize_arguments(arguments: &serde_json::Value) -> String {
    match arguments {
        serde_json::Value::Object(fields) if !fields.is_empty() => {
            let mut names: Vec<&str> = fields.keys().map(String::as_str).collect();
            names.sort_unstable();
            format!("{} argument(s): {}", names.len(), names.join(", "))
        }
        serde_json::Value::Object(_) | serde_json::Value::Null => "no arguments".to_owned(),
        other => format!("1 {} argument", kind_name(other)),
    }
}

/// The JSON type name of a value, for a safe summary.
fn kind_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}
