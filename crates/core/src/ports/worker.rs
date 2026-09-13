use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AgentDescriptor, ErrorEnvelope, EventDraft, HandoffBundle, StreamFrame, TaskCheckpoint, TaskId,
};

/// What a worker was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRequest {
    /// The task this session belongs to.
    pub task_id: TaskId,
    /// The user's goal, already minimized for this destination.
    pub goal: String,
    /// Canonical checkpoint on explicit resume. Never opaque worker state.
    pub checkpoint: Option<TaskCheckpoint>,
    /// Canonical handoff when a *different* worker is taking the task over.
    ///
    /// Separate from `checkpoint` because the two answer different questions.
    /// A checkpoint says "here is where you left off" to the worker that was
    /// already running. A handoff says "here is what happened, none of it
    /// yours" to one that was not — and carries the normalized history a
    /// stranger needs, with nothing private to the previous worker in it
    /// (`PROJECT_FORMAT.md` §4).
    pub handoff: Option<HandoffBundle>,
}

/// How a worker session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkerOutcome {
    /// The worker finished the task.
    Completed {
        /// Secret-free summary of what it did.
        summary: String,
    },
    /// The worker stopped because cancellation was requested.
    Cancelled,
}

/// The broker a worker must go through for every effect.
///
/// This is the security boundary in `ARCHITECTURE.md` §5.5: a cooperative
/// worker cannot execute a tool, call a model, or write an event directly. It
/// asks, and the core decides — which is what makes routing, permission, and
/// audit apply uniformly to an embedded worker and an external one.
#[async_trait]
pub trait WorkerHost: Send + Sync {
    /// Append a durable event to the task's stream.
    ///
    /// # Errors
    ///
    /// Returns a normalized error when the event cannot be journaled.
    async fn emit(&self, draft: EventDraft) -> Result<(), ErrorEnvelope>;

    /// Publish one ephemeral stream frame.
    ///
    /// Frames are transport: they are not persisted and a dropped frame is not
    /// data loss (`CORE_CONTRACTS.md` §5).
    async fn publish_frame(&self, frame: StreamFrame);

    /// Ask the core to run a model request on the worker's behalf.
    ///
    /// This legacy text path accepts only the task's initially included goal.
    /// Each call prepares, routes and authorizes independently; a worker may
    /// not concatenate new context or tool output into `prompt`. Structured
    /// history requires the forthcoming host-owned tool-history path.
    ///
    /// # Errors
    ///
    /// Returns a normalized error when routing, the provider, or cancellation
    /// prevents a result.
    async fn call_model(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ErrorEnvelope>;

    /// Run a stable logical step. Reusing an ID with changed arguments is refused.
    /// A completed step returns a safe summary with `skipped: true`, not its raw
    /// historical output. Workers must not treat that summary as raw tool data.
    /// Arguments pass the core's supported schema subset before approval or
    /// execution. The host records canonical call/result messages, including
    /// bounded/redacted content and explicit failure results, for new attempts.
    async fn call_tool_step(
        &self,
        step_id: &str,
        tool_id: &str,
        arguments: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, ErrorEnvelope>;
}

/// A specialist worker, embedded or external.
#[async_trait]
pub trait AgentProvider: Send + Sync {
    /// What this worker is and what it may be trusted with.
    ///
    /// The core recomputes eligibility from these facts rather than taking the
    /// worker's word for it (`CORE_CONTRACTS.md` §7.2).
    fn describe(&self) -> AgentDescriptor;

    /// Run a task to completion or cancellation.
    ///
    /// Every effect goes through `host`. Cancellation must converge to
    /// [`WorkerOutcome::Cancelled`] rather than leaving work in flight.
    ///
    /// # Errors
    ///
    /// Returns a normalized error when the worker cannot finish the task.
    async fn run(
        &self,
        request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope>;
}
