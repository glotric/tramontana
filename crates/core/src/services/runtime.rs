use std::sync::Arc;

mod model;
mod read_only;
mod tool;

pub(crate) use read_only::recorded_admission;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AuthorizedToolRequest, CancellationOrigin, CapabilityScope, ContextManifest, DataClass,
    EffectiveConfiguration, ErrorCode, ErrorEnvelope, EventDraft, EventPayload, HandoffBundle,
    InvocationId, MessageId, MessageRole, MessageStatus, ModelRequest, NormalizedMessage,
    PermissionOrigin, ReplaySafety, RiskClass, RoutingDecision, RoutingMode, StopReason,
    StreamFrame, TaskCheckpoint, TaskEventKind, TaskState,
};
use crate::ports::{AgentProvider, ModelProvider, WorkerHost, WorkerOutcome, WorkerRequest};
use crate::ports::{Clock, PermissionQuery};
use crate::services::EarlierTurns;
use crate::services::PermissionManager;
use crate::services::ToolRegistry;
use crate::services::checkpoint::ResumePlan;
use crate::services::instructions::{ComposedInstructions, ProjectInstructions};
use crate::services::journal::TaskJournal;
use crate::services::model_turn::{ModelTurnAccumulator, ModelTurnLimits};
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
    /// The text of the project-authored sources in force, when the adapter
    /// could read them. Held apart from the configuration because the record
    /// names sources and the model needs their words (ADR 0052).
    instructions: ProjectInstructions,
    /// The earlier turns of the task's conversation, chosen when it was
    /// created and chosen again identically on resume (ADR 0057).
    earlier: Arc<EarlierTurns>,
    /// The files the person attached to this task's message (ADR 0058).
    attachments: Arc<Vec<crate::contracts::TaskAttachment>>,
    /// What the person's profile tells a chat's model (ADR 0061).
    personal: Option<Arc<str>>,
    /// The person's own skills the task was given (ADR 0070).
    skills: Arc<Vec<crate::contracts::TaskSkill>>,
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
    /// handoff or when the task chose its own admission. Held separately so
    /// the composition root does not have to be rebuilt to run one task
    /// under a different worker.
    worker: Option<Arc<dyn AgentProvider>>,
    resume: ResumePlan,
    interrupt: CancellationToken,
    /// Where a task the worker could not finish saves its checkpoint, so it
    /// can be handed to another worker (ADR 0048).
    checkpoints: Option<Arc<dyn crate::ports::CheckpointRepository>>,
}

/// Why a task stopped because its worker could not go on, rather than
/// failed: another worker can take it from where it stopped (ADR 0048).
pub(crate) const WORKER_STOPPED: &str = "worker.stopped";

/// Whether a worker's error means the worker cannot go on, rather than that
/// the task went wrong: its budget is spent, or its model is unavailable,
/// too slow, or answered in a way that cannot be used. A refusal, a
/// cancellation or an unsafe history still ends the task.
fn stops_the_worker(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::LimitExceeded
            | ErrorCode::ProviderUnavailable
            | ErrorCode::Timeout
            | ErrorCode::ProtocolError
            | ErrorCode::CapabilityUnsupported
    )
}

/// Whether a worker stopped because the work was beyond the model it ran on,
/// rather than because something around the model failed (ADR 0055).
///
/// Only two codes say so. `LimitExceeded` is a task that spent its whole
/// budget without finishing, and `CapabilityUnsupported` is a model that
/// cannot do what the work needs at all. Both are evidence about the model.
///
/// A timeout, an unavailable provider and a malformed reply are not. A larger
/// model is slower, not faster; an endpoint that was down says nothing about
/// the work; and a reply this build could not parse is as likely to be this
/// adapter's fault as the model's. Sending a person's project off their
/// device on that evidence would not be honest.
#[must_use]
pub fn beyond_the_model(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::LimitExceeded | ErrorCode::CapabilityUnsupported
    )
}

/// The error a worker last stopped on, from the task's own record.
///
/// Read backwards, and abandoned at any state change that is not another
/// interruption: an older stop describes a run that has since continued.
#[must_use]
pub fn stopped_on(events: &[crate::contracts::EventEnvelope]) -> Option<ErrorCode> {
    for event in events.iter().rev() {
        match &event.payload {
            EventPayload::TaskStateChanged { current, .. }
                if *current != TaskState::Interrupted =>
            {
                return None;
            }
            EventPayload::DiagnosticRecorded { code, .. } => {
                if let Some(rest) = code.strip_prefix(&format!("{WORKER_STOPPED}.")) {
                    return serde_json::from_value(serde_json::Value::String(rest.to_owned())).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether another worker could take this one's task: the built-in worker,
/// in any of its configurations, whose tasks are portable (ADR 0048).
fn can_be_handed_on(descriptor: &crate::contracts::AgentDescriptor) -> bool {
    descriptor.checkpoint_support == crate::contracts::CheckpointSupport::Portable
        && descriptor
            .agent_id
            .strip_prefix(crate::services::REFERENCE_AGENT_ID)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('@'))
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
            instructions: ProjectInstructions::default(),
            earlier: Arc::new(EarlierTurns::default()),
            attachments: Arc::new(Vec::new()),
            personal: None,
            skills: Arc::new(Vec::new()),
            goal_redactions: Vec::new(),
            checkpoint: None,
            handoff: None,
            worker: None,
            resume: ResumePlan::default(),
            interrupt: CancellationToken::new(),
            checkpoints: None,
        }
    }

    /// Save a checkpoint here when the worker stops without finishing, so
    /// the task can be handed on. Without it such a task fails.
    pub(crate) fn with_checkpoints(
        mut self,
        checkpoints: Arc<dyn crate::ports::CheckpointRepository>,
    ) -> Self {
        self.checkpoints = Some(checkpoints);
        self
    }

    /// Run this task under `mode`.
    pub(crate) fn with_routing_mode(mut self, mode: RoutingMode) -> Self {
        self.routing_mode = mode;
        self
    }

    /// Run this task under `worker` rather than the service's own: the one
    /// its admission chose when it started, or the one that recorded it.
    pub(crate) fn with_worker(mut self, worker: Arc<dyn AgentProvider>) -> Self {
        self.worker = Some(worker);
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

    /// Attach the text of the project-authored sources in force.
    pub(crate) fn with_instructions(mut self, instructions: ProjectInstructions) -> Self {
        self.instructions = instructions;
        self
    }

    /// Give the task the earlier turns of its conversation (ADR 0057).
    pub(crate) fn with_earlier_turns(mut self, earlier: EarlierTurns) -> Self {
        self.earlier = Arc::new(earlier);
        self
    }

    /// Give the task the files attached to its message (ADR 0058).
    pub(crate) fn with_attachments(
        mut self,
        attachments: Vec<crate::contracts::TaskAttachment>,
    ) -> Self {
        self.attachments = Arc::new(attachments);
        self
    }

    /// Tell the task's model what the person's profile says (ADR 0061).
    pub(crate) fn with_personal(mut self, personal: Option<String>) -> Self {
        self.personal = personal.map(Arc::from);
        self
    }

    /// Give the task's model the person's own skills (ADR 0070).
    pub(crate) fn with_skills(mut self, skills: Vec<crate::contracts::TaskSkill>) -> Self {
        self.skills = Arc::new(skills);
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

        // Composed once, before the manifest that must describe it: the
        // record says which of the project's files travelled, so the two
        // cannot disagree (ADR 0052).
        let instructions = self
            .instructions
            .compose(crate::services::PROJECT_INSTRUCTION_BYTES)
            .map(Arc::new);

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
                TaskState::Preparing => {
                    match self.record_context(&goal, instructions.as_deref()).await {
                        Ok(manifest) => {
                            context = Some(manifest);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
                TaskState::Routing => match self.record_route(context.as_ref(), &cancel).await {
                    Ok(_) => Ok(()),
                    Err(error) => Err(error),
                },
                _ => Ok(()),
            };
            if let Err(error) = recorded {
                if cancel.is_cancelled() {
                    self.finish_cancelled(state).await;
                } else if self.can_pause(&error) {
                    // No model for this worker now: kept for another.
                    self.pause(state, error).await;
                } else {
                    self.finish_failed(state, error).await;
                }
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
            models: self.providers.models.clone(),
            releases: Arc::clone(&self.providers.releases),
            // Initial provenance constrains what a worker may request. The
            // model host prepares and routes each actual call independently.
            context: context.clone(),
            model_gate: tokio::sync::Mutex::new(()),
            instructions: instructions.clone(),
            earlier: Arc::clone(&self.earlier),
            attachments: Arc::clone(&self.attachments),
            personal: self.personal.clone(),
            skills: Arc::clone(&self.skills),
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
                configuration: self.configuration.clone().map(Arc::new),
            },
        };

        // `worker()`, not `providers.worker`: on a handoff the task is run by
        // the worker taking it over, and the descriptor recorded above is
        // already that one. Reading them from different places is how the
        // record and the execution come apart.
        let outcome = self.worker().run(request, &host, cancel.clone()).await;
        // Whatever the ending, including an interruption: a resumed task
        // opens what it needs again, and is asked about its sites again.
        self.providers
            .tools
            .task_ended(self.journal.task_id())
            .await;
        self.providers
            .permissions
            .task_ended(self.journal.task_id());
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
            // Brokered calls return cancellation as a normalized error. The
            // task still owns the user's lifecycle decision; do not depend
            // on each worker translating that error into an outcome.
            Err(error) if cancel.is_cancelled() && error.code == ErrorCode::Cancelled => {
                self.finish_cancelled(state).await;
            }
            Err(error) if self.can_pause(&error) => self.pause(state, error).await,
            Err(error) => self.finish_failed(state, error).await,
        }
    }

    /// Whether this error stops the worker rather than the task, and the
    /// task can be kept for another worker.
    fn can_pause(&self, error: &ErrorEnvelope) -> bool {
        stops_the_worker(error.code)
            && self.checkpoints.is_some()
            && can_be_handed_on(&self.worker().describe())
    }

    /// Stop a task its worker could not finish, keeping it for another
    /// worker: interrupted, with the reason and a checkpoint (ADR 0048).
    async fn pause(&self, from: TaskState, error: ErrorEnvelope) {
        let Some(checkpoints) = &self.checkpoints else {
            return self.finish_failed(from, error).await;
        };
        // The reason first: whoever stops reading at the state change has it.
        let _ = self
            .record(EventDraft::user(
                TaskEventKind::DiagnosticRecorded,
                EventPayload::DiagnosticRecorded {
                    // Which error, as its contract name: `worker.stopped.limitExceeded`.
                    code: format!(
                        "{WORKER_STOPPED}.{}",
                        serde_json::to_value(error.code)
                            .ok()
                            .and_then(|code| code.as_str().map(str::to_owned))
                            .unwrap_or_default()
                    ),
                    summary: error.message,
                },
            ))
            .await;
        if self
            .change_state(from, TaskState::Interrupted, WORKER_STOPPED)
            .await
            .is_err()
        {
            return;
        }
        let Ok(events) = self.journal.replay().await else {
            return;
        };
        let checkpoint = crate::services::checkpoint::derive_checkpoint(
            crate::contracts::CheckpointId::new(self.journal.next_id("checkpoint")),
            self.journal.task_id().clone(),
            &events,
            self.configuration
                .as_ref()
                .map(|configuration| configuration.manifest_hash.clone()),
            self.journal.now(),
        );
        if checkpoints.save(&checkpoint).await.is_ok() {
            let _ = self
                .record(EventDraft::user(
                    TaskEventKind::CheckpointSaved,
                    EventPayload::CheckpointSaved {
                        checkpoint_id: checkpoint.checkpoint_id.clone(),
                        event_sequence: checkpoint.event_sequence,
                        completed_step_count: checkpoint.completed_step_ids.len() as u64,
                        pending_action: checkpoint.pending_action.clone(),
                        resume: checkpoint.resume.clone(),
                    },
                ))
                .await;
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
        instructions: Option<&ComposedInstructions>,
    ) -> Result<crate::contracts::ContextManifest, ErrorEnvelope> {
        let assembled = assemble_context(
            self.journal.next_id("context"),
            self.journal.task_id().clone(),
            goal,
            self.goal_redactions.clone(),
            self.configuration.as_ref(),
            instructions,
            self.clock.now(),
        );
        // The earlier turns come first, then the attached files, as the
        // model reads them (ADRs 0057 and 0058).
        let manifest = if self.earlier.is_empty()
            && self.attachments.is_empty()
            && self.personal.is_none()
            && self.skills.is_empty()
        {
            assembled
        } else {
            // The profile is personal data the person wrote (ADR 0061):
            // sensitive, and named so a disclosure can say so.
            let mut sources: Vec<_> = self
                .personal
                .iter()
                .map(|text| {
                    crate::contracts::ContextSource::redacted(
                        crate::services::PERSONAL_SOURCE_ID,
                        crate::contracts::ContextCategory::Instruction,
                        crate::contracts::DataClass::Sensitive,
                        text,
                        Vec::new(),
                    )
                })
                .collect();
            // The person's own skills, after the profile (ADR 0070).
            sources.extend(crate::services::skill_sources(&self.skills));
            sources.extend(self.earlier.sources());
            sources.extend(crate::services::attachment_sources(&self.attachments));
            sources.extend(assembled.sources);
            crate::contracts::ContextManifest::new(
                assembled.context_manifest_id,
                assembled.task_id,
                assembled.destination_class,
                sources,
                assembled.created_at,
            )
        };

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
        cancel: &CancellationToken,
    ) -> Result<RoutingDecision, ErrorEnvelope> {
        let snapshot = AvailabilitySnapshot {
            snapshot_id: self.journal.next_id("availability"),
            models: tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ErrorEnvelope::new(ErrorCode::Cancelled, "Routing cancelled", self.journal.correlation_id().clone())),
                () = tokio::time::sleep(std::time::Duration::from_secs(30)) => return Err(ErrorEnvelope::new(ErrorCode::Timeout, "Model discovery timed out", self.journal.correlation_id().clone())),
                models = async {
                let mut available = Vec::new();
                for provider in &self.providers.models {
                    available.extend(provider.list_models().await);
                }
                available
                } => models,
            },
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
        input.required = self.worker().pinned_model();

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

    /// Record what this task changed, from its own `toolCompleted` events.
    ///
    /// On every ending — completed, failed or cancelled — because a task that
    /// stopped half-way is exactly when a person needs to know what it
    /// touched. Built from the whole task record, so a resumed task's summary
    /// includes what it did before the interruption. Nothing is recorded when
    /// nothing changed (ADR 0026).
    async fn record_changes(&self) -> Option<crate::contracts::ArtifactId> {
        let events = self.journal.replay().await.ok()?;
        let changes: Vec<crate::contracts::FileChange> = events
            .iter()
            .flat_map(|event| match &event.payload {
                EventPayload::ToolCompleted { changes, .. } => changes.clone(),
                _ => Vec::new(),
            })
            .collect();
        if changes.is_empty() {
            return None;
        }
        let artifact = crate::contracts::ArtifactRecord::changed_files(
            crate::contracts::ArtifactId::new(self.journal.next_id("artifact")),
            changes,
        );
        let artifact_id = artifact.artifact_id.clone();
        self.record(EventDraft::user(
            TaskEventKind::ArtifactRecorded,
            EventPayload::ArtifactRecorded { artifact },
        ))
        .await
        .ok()?;
        Some(artifact_id)
    }

    async fn finish_completed(&self, from: TaskState, summary: String) {
        if self
            .change_state(from, TaskState::Completed, "worker.completed")
            .await
            .is_ok()
        {
            let artifacts = self.record_changes().await.into_iter().collect();
            let _ = self
                .record(EventDraft::user(
                    TaskEventKind::TaskCompleted,
                    EventPayload::TaskCompleted { summary, artifacts },
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
            let _ = self.record_changes().await;
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
            let _ = self.record_changes().await;
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
    tools: ToolBroker,
    /// Every configured way to reach a model.
    models: Vec<Arc<dyn ModelProvider>>,
    /// What decides whether a payload may reach the destination.
    releases: Arc<ReleaseManager>,
    /// The context assembled for this task.
    context: Option<ContextManifest>,
    /// The project's own instructions, as every turn will send them.
    instructions: Option<Arc<ComposedInstructions>>,
    /// The earlier turns of the conversation, as every turn will send them.
    earlier: Arc<EarlierTurns>,
    /// The files attached to the task's message, as every turn will send them.
    attachments: Arc<Vec<crate::contracts::TaskAttachment>>,
    /// What the person's profile tells a chat's model, as every turn sends it.
    personal: Option<Arc<str>>,
    /// The person's own skills, as every turn sends them.
    skills: Arc<Vec<crate::contracts::TaskSkill>>,
    /// Serialize requests so approvals and model starts cannot overlap per host.
    model_gate: tokio::sync::Mutex<()>,
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
            ReleaseError::InvalidPayload(code) => *code,
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
    /// The configuration the task started with, which recipes are resolved
    /// against (ADR 0025). `None` for a projectless task, which has none.
    pub(crate) configuration: Option<Arc<EffectiveConfiguration>>,
}

impl std::fmt::Debug for ToolBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolBroker").finish_non_exhaustive()
    }
}

#[async_trait]
impl WorkerHost for BrokeredHost {
    async fn read_only_turn(
        &self,
        tools: &[String],
        limits: &crate::ports::ReadOnlyLimits,
        cancel: &CancellationToken,
    ) -> Result<crate::ports::ReadOnlyTurn, ErrorEnvelope> {
        self.run_turn(
            tools,
            crate::ports::ToolAdmission::READ_ONLY,
            limits,
            None,
            cancel,
        )
        .await
    }
    async fn governed_turn(
        &self,
        tools: &[String],
        admission: crate::ports::ToolAdmission,
        limits: &crate::ports::ReadOnlyLimits,
        cancel: &CancellationToken,
    ) -> Result<crate::ports::ReadOnlyTurn, ErrorEnvelope> {
        self.run_turn(tools, admission, limits, None, cancel).await
    }
    async fn pinned_turn(
        &self,
        tools: &[String],
        admission: crate::ports::ToolAdmission,
        limits: &crate::ports::ReadOnlyLimits,
        model: &crate::contracts::RoutedModel,
        cancel: &CancellationToken,
    ) -> Result<crate::ports::ReadOnlyTurn, ErrorEnvelope> {
        self.run_turn(tools, admission, limits, Some(model), cancel)
            .await
    }
    async fn emit(&self, draft: EventDraft) -> Result<(), ErrorEnvelope> {
        if matches!(&draft.payload, EventPayload::DiagnosticRecorded { code, .. } if code.starts_with("reference."))
        {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "Reserved host event",
                self.journal.correlation_id().clone(),
            ));
        }
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
        self.run_model_request(prompt, cancel).await
    }

    async fn call_tool_step(
        &self,
        step_id: &str,
        tool_id: &str,
        arguments: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, ErrorEnvelope> {
        self.run_tool_step(step_id, tool_id, arguments, cancel)
            .await
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{EventEnvelope, EventId, TaskEventKind, TaskId, Timestamp};

    fn event(kind: TaskEventKind, payload: EventPayload) -> EventEnvelope {
        EventEnvelope {
            schema_version: crate::contracts::SCHEMA_VERSION,
            event_id: EventId::new("event"),
            task_id: TaskId::new("task"),
            conversation_id: None,
            sequence: 1,
            kind,
            payload,
            occurred_at: Timestamp::new("2026-01-01T00:00:00.000Z"),
            correlation_id: crate::contracts::CorrelationId::new("correlation"),
            visibility: crate::contracts::Visibility::User,
            causation_id: None,
        }
    }

    fn stopped(code: &str) -> EventEnvelope {
        event(
            TaskEventKind::DiagnosticRecorded,
            EventPayload::DiagnosticRecorded {
                code: format!("{WORKER_STOPPED}.{code}"),
                summary: "stopped".to_owned(),
            },
        )
    }

    #[test]
    fn only_a_spent_budget_or_a_missing_capability_is_evidence_about_the_model() {
        assert!(beyond_the_model(ErrorCode::LimitExceeded));
        assert!(beyond_the_model(ErrorCode::CapabilityUnsupported));
        // A larger model is slower, an endpoint that was down says nothing
        // about the work, and a reply this build could not read may be this
        // adapter's fault (ADR 0055).
        assert!(!beyond_the_model(ErrorCode::Timeout));
        assert!(!beyond_the_model(ErrorCode::ProviderUnavailable));
        assert!(!beyond_the_model(ErrorCode::ProtocolError));
        // And a stop that is not the worker's at all is not evidence either.
        assert!(!beyond_the_model(ErrorCode::Cancelled));
        assert!(!beyond_the_model(ErrorCode::PermissionDenied));
    }

    #[test]
    fn the_stop_read_back_is_the_latest_one() {
        let events = [stopped("providerUnavailable"), stopped("limitExceeded")];
        assert_eq!(stopped_on(&events), Some(ErrorCode::LimitExceeded));
    }

    #[test]
    fn a_stop_from_a_run_that_has_since_continued_is_not_read() {
        // Interrupted, then continued: the older diagnostic describes a run
        // that is over, and offering to escalate on it would be wrong.
        let events = [
            stopped("limitExceeded"),
            event(
                TaskEventKind::TaskStateChanged,
                EventPayload::TaskStateChanged {
                    previous: TaskState::Interrupted,
                    current: TaskState::Running,
                    reason_code: "user.resumedCheckpoint".to_owned(),
                },
            ),
        ];
        assert_eq!(stopped_on(&events), None);
    }

    #[test]
    fn a_task_nobody_stopped_has_no_reason() {
        assert_eq!(stopped_on(&[]), None);
        let unrelated = [event(
            TaskEventKind::DiagnosticRecorded,
            EventPayload::DiagnosticRecorded {
                code: "reference.callsNotRun".to_owned(),
                summary: "two calls".to_owned(),
            },
        )];
        assert_eq!(stopped_on(&unrelated), None);
    }
}
