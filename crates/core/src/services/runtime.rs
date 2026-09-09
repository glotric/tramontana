use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AuthorizedToolRequest, CancellationOrigin, CapabilityScope, ContextManifest, DataClass,
    EffectiveConfiguration, ErrorCode, ErrorEnvelope, EventDraft, EventPayload, HandoffBundle,
    InvocationId, MessageId, MessageRole, MessageStatus, ModelEvent, ModelRequest,
    NormalizedMessage, PermissionOrigin, ReplaySafety, RiskClass, RoutingDecision, RoutingMode,
    SideEffect, StopReason, StreamFrame, TaskCheckpoint, TaskEventKind, TaskState,
};
use crate::ports::{AgentProvider, ModelProvider, WorkerHost, WorkerOutcome, WorkerRequest};
use crate::ports::{Clock, PermissionQuery};
use crate::services::PermissionManager;
use crate::services::ToolRegistry;
use crate::services::checkpoint::ResumePlan;
use crate::services::journal::TaskJournal;
use crate::services::preparation::assemble_context;
use crate::services::release::{ReleaseContext, ReleaseError, ReleaseManager};
use crate::services::routing::{AvailabilitySnapshot, RoutingInput, route};
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
    /// The limits every tool call this task makes inherits, taken from the
    /// project's approved roots. A projectless task gets an empty scope, which
    /// the permission manager reads as no filesystem access at all.
    scope: CapabilityScope,
    /// Source of decision timestamps.
    clock: Arc<dyn Clock>,
    /// The routing mode in force for this task.
    routing_mode: RoutingMode,
    /// The effective configuration this task started under, when it has one.
    ///
    /// Held so the context manifest can name the instruction sources in force
    /// rather than only their combined hash.
    configuration: Option<EffectiveConfiguration>,
    /// What was removed from the goal when the task was created.
    ///
    /// Carried rather than recomputed: the goal the driver receives is already
    /// redacted, so asking it again would find nothing and the manifest would
    /// claim no credential was ever there.
    goal_redactions: Vec<String>,
    checkpoint: Option<TaskCheckpoint>,
    /// The canonical record handed to a *different* worker taking over.
    handoff: Option<HandoffBundle>,
    /// The worker actually running, which is not `providers.worker` on a
    /// handoff. Held separately so the composition root does not have to be
    /// rebuilt to run one task under a different worker.
    worker: Option<Arc<dyn AgentProvider>>,
    resume: ResumePlan,
    interrupt: CancellationToken,
}

impl TaskDriver {
    pub(crate) fn new(
        journal: TaskJournal,
        providers: Providers,
        scope: CapabilityScope,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            journal,
            providers,
            scope,
            clock,
            routing_mode: RoutingMode::Automatic,
            configuration: None,
            goal_redactions: Vec::new(),
            checkpoint: None,
            handoff: None,
            worker: None,
            resume: ResumePlan::default(),
            interrupt: CancellationToken::new(),
        }
    }

    /// Run this task under `mode`.
    pub(crate) fn with_routing_mode(mut self, mode: RoutingMode) -> Self {
        self.routing_mode = mode;
        self
    }

    /// Record what was redacted out of the goal at ingestion.
    pub(crate) fn with_goal_redactions(mut self, redactions: Vec<String>) -> Self {
        self.goal_redactions = redactions;
        self
    }

    /// Attach the effective configuration this task resolved at start.
    pub(crate) fn with_configuration(
        mut self,
        configuration: Option<EffectiveConfiguration>,
    ) -> Self {
        self.configuration = configuration;
        self
    }

    /// Run this task under `worker`, continuing from `handoff`.
    pub(crate) fn handing_over(
        mut self,
        worker: Arc<dyn AgentProvider>,
        handoff: HandoffBundle,
        resume: ResumePlan,
        interrupt: CancellationToken,
    ) -> Self {
        self.worker = Some(worker);
        self.handoff = Some(handoff);
        self.resume = resume;
        self.interrupt = interrupt;
        self
    }

    /// The worker this run uses: the one taking over, or the configured one.
    fn worker(&self) -> &Arc<dyn AgentProvider> {
        self.worker.as_ref().unwrap_or(&self.providers.worker)
    }

    pub(crate) fn continuing(
        mut self,
        checkpoint: Option<TaskCheckpoint>,
        resume: ResumePlan,
        interrupt: CancellationToken,
    ) -> Self {
        self.checkpoint = checkpoint;
        self.resume = resume;
        self.interrupt = interrupt;
        self
    }

    /// Run the task to exactly one terminal state.
    pub(crate) async fn run(self, goal: String, cancel: CancellationToken) {
        let mut state = if self.checkpoint.is_some() {
            TaskState::Preparing
        } else {
            TaskState::Queued
        };

        // A resumed run prepares context again rather than reusing what an
        // earlier attempt assembled: the project may have changed while the
        // task was interrupted, and routing against a stale manifest would
        // describe a release that did not happen.
        let mut context: Option<ContextManifest> = None;
        let mut route: Option<RoutingDecision> = None;

        // Each step checks cancellation first. A cancel that arrives before the
        // task really starts still produces a proper Cancelling -> Cancelled
        // pair rather than a task that never moved.
        //
        // Preparing and Routing each leave a record of what they decided, so
        // "what was this task given, and why that model?" is answerable from
        // the stream rather than from someone's memory of the policy.
        for next in [
            TaskState::Preparing,
            TaskState::Routing,
            TaskState::StartingWorker,
        ] {
            if cancel.is_cancelled() {
                self.finish_cancelled(state).await;
                return;
            }

            // Explicit resume journals Interrupted -> Preparing before it
            // returns to its caller, so a second command cannot observe a
            // still-resumable task. The transition is therefore already
            // written and must not be written twice.
            //
            // Only the transition, though. Skipping the *work* of a state
            // along with its transition is how a resumed run came to record a
            // routing decision naming a context manifest that was never
            // assembled — visible in an exported record as
            // `contextManifestId: "context.unavailable"`.
            if next != state {
                match self.change_state(state, next, "task.advanced").await {
                    Ok(current) => state = current,
                    Err(error) => {
                        self.finish_failed(state, error).await;
                        return;
                    }
                }
            }

            let recorded = match state {
                TaskState::Preparing => match self.record_context(&goal).await {
                    Ok(manifest) => {
                        context = Some(manifest);
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                TaskState::Routing => match self.record_route(context.as_ref()).await {
                    Ok(decision) => {
                        route = Some(decision);
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                _ => Ok(()),
            };
            if let Err(error) = recorded {
                self.finish_failed(state, error).await;
                return;
            }
        }

        let descriptor = self.worker().describe();
        if let Err(error) = self
            .record(EventDraft::user(
                TaskEventKind::WorkerSelected,
                EventPayload::WorkerSelected {
                    descriptor: descriptor.clone(),
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
            goal: goal.clone(),
            checkpoint: self.checkpoint.clone(),
            handoff: self.handoff.clone(),
        };
        let host = BrokeredHost {
            journal: self.journal.clone(),
            model: Arc::clone(&self.providers.model),
            releases: Arc::clone(&self.providers.releases),
            // The assembled context and the decision that chose a destination,
            // held whole rather than by identifier: the release plan is
            // derived from both, and a host that carried only an identifier
            // would have to look them up to say what it was sending.
            context: context.clone(),
            route: route.clone(),
            routing_mode: self.routing_mode,
            scope: self.scope.clone(),
            tools: ToolBroker {
                registry: Arc::clone(&self.providers.tools),
                permissions: Arc::clone(&self.providers.permissions),
                scope: self.scope.clone(),
                origin: PermissionOrigin::Worker {
                    agent_id: descriptor.agent_id.clone(),
                },
                clock: Arc::clone(&self.clock),
                resume: Arc::new(tokio::sync::Mutex::new(self.resume.clone())),
                interrupt: self.interrupt.clone(),
            },
        };

        // `worker()`, not `providers.worker`: on a handoff the task is run by
        // the worker taking it over, and the descriptor recorded above is
        // already that one. Reading them from different places is how the
        // record and the execution come apart.
        let outcome = self.worker().run(request, &host, cancel.clone()).await;
        if self.interrupt.is_cancelled() {
            let _ = self
                .change_state(state, TaskState::Interrupted, "user.checkpoint")
                .await;
            return;
        }
        match outcome {
            Ok(WorkerOutcome::Completed { summary }) if !cancel.is_cancelled() => {
                self.finish_completed(state, summary).await;
            }
            // A worker that finished after cancellation was requested still
            // ends as cancelled: the user's decision outranks the late result.
            Ok(_) => self.finish_cancelled(state).await,
            Err(error) => self.finish_failed(state, error).await,
        }
    }

    /// Assemble the context for this task and record what it holds.
    ///
    /// Returns the manifest so routing can name the release it is deciding
    /// about, and can see what data classes it would be sending — the link
    /// that makes a disclosure traceable to what was actually assembled.
    async fn record_context(
        &self,
        goal: &str,
    ) -> Result<crate::contracts::ContextManifest, ErrorEnvelope> {
        let manifest = assemble_context(
            self.journal.next_id("context"),
            self.journal.task_id().clone(),
            goal,
            self.goal_redactions.clone(),
            self.configuration.as_ref(),
            self.clock.now(),
        );

        self.record(EventDraft::user(
            TaskEventKind::ContextPrepared,
            EventPayload::ContextPrepared {
                manifest: manifest.clone(),
            },
        ))
        .await?;
        Ok(manifest)
    }

    /// Choose a route for the context just prepared and record why.
    ///
    /// A refusal is recorded as a task failure rather than silently skipped:
    /// "no model could serve this" is something the user has to be told, and
    /// the reasons are the same vocabulary a successful decision uses.
    async fn record_route(
        &self,
        context: Option<&ContextManifest>,
    ) -> Result<RoutingDecision, ErrorEnvelope> {
        let snapshot = AvailabilitySnapshot {
            snapshot_id: self.journal.next_id("availability"),
            models: self.providers.model.list_models().await,
            taken_at: self.clock.now(),
        };

        let mut input = RoutingInput::new(
            self.routing_mode,
            snapshot,
            context.map_or("context.unavailable", |manifest| {
                manifest.context_manifest_id.as_str()
            }),
        );
        input.carries_private_data = context.is_some_and(|manifest| {
            manifest
                .released_data_classes()
                .iter()
                .any(|class| *class != DataClass::Public)
        });

        let decision = route(self.journal.next_id("decision"), &input).map_err(|refusal| {
            ErrorEnvelope::new(
                ErrorCode::ProviderUnavailable,
                format!("no model could serve this task: {refusal}"),
                self.journal.correlation_id().clone(),
            )
        })?;

        self.record(EventDraft::user(
            TaskEventKind::RoutingDecided,
            EventPayload::RoutingDecided {
                decision: decision.clone(),
            },
        ))
        .await?;
        Ok(decision)
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
        self.journal.append(draft).await.map(|_| ())
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
        if self.interrupt.is_cancelled() || error.code == ErrorCode::InterruptedUnsafe {
            let _ = self
                .change_state(from, TaskState::Interrupted, "task.resumeRefused")
                .await;
            let _ = self
                .record(EventDraft::user(
                    TaskEventKind::DiagnosticRecorded,
                    EventPayload::DiagnosticRecorded {
                        code: "resume.refused".into(),
                        summary: error.message,
                    },
                ))
                .await;
            return;
        }
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
        if self.interrupt.is_cancelled() {
            let _ = self
                .change_state(from, TaskState::Interrupted, "user.checkpoint")
                .await;
            return;
        }
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
    tools: ToolBroker,
    /// What decides whether a payload may reach the destination.
    releases: Arc<ReleaseManager>,
    /// The context assembled for this task.
    context: Option<ContextManifest>,
    /// The decision that chose where it goes.
    route: Option<RoutingDecision>,
    /// The standing policy in force.
    routing_mode: RoutingMode,
    /// The limits this task runs under.
    scope: CapabilityScope,
}

impl BrokeredHost {
    /// Turn a refused release into the error a worker sees.
    ///
    /// A refusal is a permission outcome, not a provider fault: the model was
    /// reachable and the user said no. Reporting it as anything else would
    /// invite a retry against a decision that was already made.
    fn release_refused(&self, error: &ReleaseError) -> ErrorEnvelope {
        let code = match error {
            ReleaseError::Denied { .. } => ErrorCode::PermissionDenied,
            _ => ErrorCode::Internal,
        };
        ErrorEnvelope::new(
            code,
            error.to_string(),
            self.journal.correlation_id().clone(),
        )
    }
}

/// Everything needed to turn a worker's tool request into an audited action.
///
/// Bundled because the five pieces are meaningless apart: a registry with no
/// policy would authorize nothing, and a policy with no scope would authorize
/// everything.
#[derive(Clone)]
pub(crate) struct ToolBroker {
    /// Where tools are found.
    pub(crate) registry: Arc<ToolRegistry>,
    /// What decides whether they may run.
    pub(crate) permissions: Arc<PermissionManager>,
    /// The limits every request inherits.
    pub(crate) scope: CapabilityScope,
    /// Who is asking, for the audit record.
    pub(crate) origin: PermissionOrigin,
    /// Source of decision timestamps.
    pub(crate) clock: Arc<dyn Clock>,
    /// What a resumed run must not do again.
    pub(crate) resume: Arc<tokio::sync::Mutex<ResumePlan>>,
    pub(crate) interrupt: CancellationToken,
}

impl std::fmt::Debug for ToolBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolBroker").finish_non_exhaustive()
    }
}

#[async_trait]
impl WorkerHost for BrokeredHost {
    async fn emit(&self, draft: EventDraft) -> Result<(), ErrorEnvelope> {
        // Workers cannot forge lifecycle, approval, release, or completion
        // ledger entries. What is left is an observation about the worker's
        // own conduct, which is the only thing it is the authority on.
        if !matches!(
            (&draft.kind, &draft.payload),
            (
                TaskEventKind::DiagnosticRecorded,
                EventPayload::DiagnosticRecorded { .. }
            )
        ) {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "Workers may emit only diagnostic observations",
                self.journal.correlation_id().clone(),
            ));
        }
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
        // One identifier for the request, the stream, and the frames it
        // publishes, so a delta can be tied back to the `modelStarted` event
        // without anyone inventing a second name for the same thing.
        let stream_id = format!("stream-{}", self.journal.task_id());

        // Fail closed. A model call with no assembled context and no routing
        // decision is one that cannot be disclosed, so it does not happen.
        let (Some(manifest), Some(decision)) = (self.context.as_ref(), self.route.as_ref()) else {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "a model request needs an assembled context and a routing decision",
                self.journal.correlation_id().clone(),
            ));
        };

        let mut request = ModelRequest::new(
            stream_id.clone(),
            self.journal.task_id().clone(),
            vec![NormalizedMessage::text(
                MessageId::new(self.journal.next_id("message")),
                self.journal.conversation_id().cloned(),
                MessageRole::User,
                self.journal.now(),
                prompt,
            )],
            // The release this request was built from. There is no fallback
            // value: a request that cannot name the context it came from is
            // one nobody can describe, and describing it is the whole job of
            // the boundary below. A resumed run that skipped preparation used
            // to reach here and record `context.unavailable`; now it cannot
            // reach here at all.
            manifest.context_manifest_id.clone(),
        );
        // What is being sent, for routing and disclosure. Everything a task
        // carries today is derived from an opened project.
        request.data_classes = vec![DataClass::Project];

        // Nothing reaches a provider without a release decision. For a local
        // destination that costs a record and no question; for a cloud one it
        // is the disclosure and the approval
        // (`SECURITY_AND_PERMISSIONS.md` §8).
        let authorized = self
            .releases
            .authorize(
                self.journal.next_id("release"),
                request,
                &ReleaseContext {
                    manifest,
                    decision,
                    mode: self.routing_mode,
                    scope: &self.scope,
                },
                self.journal.now(),
            )
            .await
            .map_err(|error| self.release_refused(&error))?;

        // Recorded before the request goes, not after: if the process dies
        // mid-stream the record still says what left the device.
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ContextReleased,
                EventPayload::ContextReleased {
                    plan: Box::new(authorized.plan().clone()),
                },
            ))
            .await?;

        // Only now is it true that a model request has started. Emitted by the
        // host rather than the worker, and after the release rather than
        // before it, so a refused release cannot leave a record saying a model
        // ran (`CORE_CONTRACTS.md` §5.1).
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ModelStarted,
                EventPayload::ModelStarted {
                    stream_id: stream_id.clone(),
                    model_id: decision.model.model_id.clone(),
                },
            ))
            .await?;

        let mut stream = self.model.stream(authorized, cancel.clone()).await?;

        // Deltas are coalesced here and committed once. Persisting every token
        // would make the durable stream enormous and tell the reader nothing
        // the final message does not (`CORE_CONTRACTS.md` §5).
        let mut answer = String::new();
        let mut stop_reason = None;
        let mut failure = None;
        while let Some(event) = stream.recv().await {
            match event {
                ModelEvent::TextDelta { text, .. } => {
                    self.journal.publish_frame(StreamFrame {
                        stream_id: stream_id.clone(),
                        offset: answer.len() as u64,
                        text: text.clone(),
                    });
                    answer.push_str(&text);
                }
                // A tool call assembled by the model, and what the request
                // cost, are M6's to act on. Ignored rather than treated as
                // text, which would put JSON fragments in the answer.
                ModelEvent::ToolCallDelta { .. } | ModelEvent::Usage { .. } => {}
                ModelEvent::Completed {
                    stop_reason: reason,
                } => {
                    stop_reason = Some(reason);
                    break;
                }
                ModelEvent::Failed { error } => {
                    failure = Some(error);
                    break;
                }
            }
        }

        // The frames are gone the moment they are delivered. Committing the
        // coalesced answer is what puts the model's reply in the durable
        // record at all — without it an exported task shows that a model was
        // asked and never what it said (`CORE_CONTRACTS.md` §5).
        let status = match stop_reason {
            Some(StopReason::Stop) => MessageStatus::Committed,
            // Cut short: the content up to that point is kept and marked, so a
            // reader can tell a finished answer from a truncated one.
            _ => MessageStatus::Partial,
        };
        let mut message = NormalizedMessage::text(
            MessageId::new(self.journal.next_id("message")),
            self.journal.conversation_id().cloned(),
            MessageRole::Assistant,
            self.journal.now(),
            answer.clone(),
        );
        message.task_id = Some(self.journal.task_id().clone());
        message.status = status;

        self.journal
            .append(EventDraft::user(
                TaskEventKind::MessageCommitted,
                EventPayload::MessageCommitted { message },
            ))
            .await?;

        if let Some(error) = failure {
            return Err(error);
        }
        if stop_reason.is_none() {
            return Err(ErrorEnvelope::new(
                ErrorCode::Internal,
                "Model stream ended without an outcome",
                self.journal.correlation_id().clone(),
            ));
        }
        Ok(answer)
    }

    async fn call_tool_step(
        &self,
        step_id: &str,
        tool_id: &str,
        arguments: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, ErrorEnvelope> {
        let correlation = self.journal.correlation_id().clone();
        if step_id.is_empty()
            || step_id.len() > 128
            || !step_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(ErrorEnvelope::new(
                ErrorCode::InvalidRequest,
                "Invalid tool step identity",
                correlation,
            ));
        }
        // Serialize logical steps, including concurrent duplicate calls.
        let mut plan = self.tools.resume.lock().await;
        if cancel.is_cancelled() {
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "Tool call cancelled before execution",
                correlation,
            ));
        }
        if plan.uncertain.contains(step_id) {
            return Err(ErrorEnvelope::new(
                ErrorCode::InterruptedUnsafe,
                "An earlier effect for this step is uncertain",
                correlation,
            ));
        }
        let hash = crate::services::checkpoint::request_hash(tool_id, &arguments);
        if plan
            .requests
            .get(step_id)
            .is_some_and(|previous| previous != &hash)
        {
            return Err(ErrorEnvelope::new(
                ErrorCode::InterruptedUnsafe,
                "A recorded step cannot be reused with a different tool or arguments",
                correlation,
            ));
        }
        let invocation_id = InvocationId::new(format!("{}-step-{step_id}", self.journal.task_id()));
        let correlation = self.journal.correlation_id().clone();

        // 0. Refuse to repeat a step this task already completed.
        //
        //    Enforced here rather than left to the worker. A worker that
        //    forgets to consult the watermark would otherwise duplicate the
        //    effect, and "resume without repeating a side effect" would be a
        //    convention rather than a guarantee (`BUILD_ORDER.md` §12).
        if let Some(completed) = plan.already_done(step_id) {
            self.journal
                .append(EventDraft::user(
                    TaskEventKind::DiagnosticRecorded,
                    EventPayload::DiagnosticRecorded {
                        code: "resume.stepSkipped".to_owned(),
                        summary: format!("{tool_id} was completed before the interruption"),
                    },
                ))
                .await?;
            return Ok(serde_json::json!({
                "skipped": true,
                "reason": "alreadyCompleted",
                "summary": completed.summary,
            }));
        }

        // 1. Resolve. An unknown tool never reaches policy: there is nothing
        //    to evaluate, and inventing a scope for it would be guessing.
        let Some((capability, risk_class)) = self.tools.registry.requirements(tool_id) else {
            return Err(ErrorEnvelope::new(
                ErrorCode::NotFound,
                format!("unknown tool {tool_id}"),
                correlation,
            ));
        };

        // 2. Announce the request before it is judged, so the record shows
        //    what was asked for even when the answer is no.
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ToolRequested,
                EventPayload::ToolRequested {
                    invocation_id: invocation_id.clone(),
                    tool_id: tool_id.to_owned(),
                    arguments_summary: summarize_arguments(&arguments),
                    risk_class,
                    step_id: Some(step_id.to_owned()),
                    request_hash: Some(hash.clone()),
                },
            ))
            .await?;
        plan.requests.insert(step_id.to_owned(), hash.clone());

        // 3. Decide.
        let query = PermissionQuery {
            tool_id: tool_id.to_owned(),
            capability,
            risk_class,
            scope: self.tools.scope.clone(),
            persistable: risk_class != RiskClass::Dangerous,
        };
        let requested_at = self.tools.clock.now();
        let authorization = self.tools.permissions.authorize(
            &query,
            self.journal.task_id(),
            &invocation_id,
            arguments,
            &requested_at,
        );
        let (outcome, authorized) = tokio::select! {
            result = authorization => result,
            () = cancel.cancelled() => return Err(ErrorEnvelope::new(ErrorCode::Cancelled,
                "Cancelled while awaiting permission", correlation)),
        }
        .map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                error.to_string(),
                correlation.clone(),
            )
        })?;

        // 4. Record the decision — allowed or not. "Every tool invocation
        //    carries origin, requested capability, scope, risk class, routing
        //    destination, and approval result in an auditable event"
        //    (`AGENTS.md`, Security and privacy).
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ApprovalResolved,
                EventPayload::PermissionDecided {
                    invocation_id: invocation_id.clone(),
                    origin: self.tools.origin.clone(),
                    capability: query.capability.to_string(),
                    scope: query.scope.clone(),
                    allowed: outcome.is_allowed(),
                    reason_code: outcome.reason_code().to_owned(),
                    grant_kind: authorized.as_ref().map(AuthorizedToolRequest::grant_kind),
                },
            ))
            .await?;

        let Some(authorized) = authorized else {
            let denial = ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                format!("{tool_id} was not permitted: {}", outcome.reason_code()),
                correlation,
            );
            self.journal
                .append(EventDraft::user(
                    TaskEventKind::ToolFailed,
                    EventPayload::ToolFailed {
                        invocation_id,
                        error: denial.clone(),
                    },
                ))
                .await?;
            // The worker receives a structured refusal, "not an opportunity to
            // rephrase the same request indefinitely"
            // (`SECURITY_AND_PERMISSIONS.md` §4).
            return Err(denial);
        };

        // 5. Run it.
        let Some(registered) = self.tools.registry.get(tool_id) else {
            return Err(ErrorEnvelope::new(
                ErrorCode::NotFound,
                "Tool was removed",
                correlation,
            ));
        };
        let replay_safety = if registered.descriptor.side_effect == SideEffect::None {
            ReplaySafety::Safe
        } else {
            ReplaySafety::Unsafe
        };
        if cancel.is_cancelled() {
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "Tool call cancelled before execution",
                correlation,
            ));
        }
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ToolStarted,
                EventPayload::ToolStarted {
                    invocation_id: invocation_id.clone(),
                    tool_id: tool_id.to_owned(),
                    replay_safety,
                },
            ))
            .await?;

        if replay_safety != ReplaySafety::Safe {
            plan.uncertain.insert(step_id.to_owned());
        }

        let outcome = registered.provider.invoke(authorized, cancel.clone()).await;
        if self.tools.interrupt.is_cancelled() {
            return Err(ErrorEnvelope::new(
                ErrorCode::InterruptedUnsafe,
                "Tool interrupted; inspect the checkpoint before continuing",
                correlation,
            ));
        }
        match outcome {
            Ok(outcome) => {
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
                plan.completed.insert(
                    step_id.to_owned(),
                    crate::services::checkpoint::CompletedStep {
                        request_hash: hash,
                        summary: outcome.display_summary.clone(),
                    },
                );
                plan.uncertain.remove(step_id);
                Ok(outcome.content)
            }
            Err(error) => {
                self.journal
                    .append(EventDraft::user(
                        TaskEventKind::ToolFailed,
                        EventPayload::ToolFailed {
                            invocation_id,
                            error: error.clone(),
                        },
                    ))
                    .await?;
                Err(error)
            }
        }
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
