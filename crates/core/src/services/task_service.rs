use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::{ConfigurationSelection, RawSource, resolve_with_selection};
use crate::contracts::{
    CapabilityScope, ConversationId, ConversationRecord, CorrelationId, EffectiveConfiguration,
    ErrorCode, ErrorEnvelope, EventDraft, EventEnvelope, EventId, EventPayload, ProjectId,
    ProjectRecord, RoutingMode, SCHEMA_VERSION, TaskEventKind, TaskId, TaskRecord, TaskSnapshot,
    TaskUpdate,
};
use crate::ports::{
    AgentProvider, CheckpointRepository, Clock, ConfigurationRepository, ConversationRepository,
    EventStore, IdSource, ModelProvider, ProjectRepository, RepositoryError, TaskRepository,
    ToolAdmission,
};
use crate::services::journal::{JournalPorts, TaskJournal};
use crate::services::projection::TaskProjectionWriter;
use crate::services::runtime::TaskDriver;
use crate::services::{PermissionManager, ToolRegistry};

/// How many updates a subscriber may fall behind before it must replay.
///
/// Lagging past this loses live updates, which is safe rather than fatal: the
/// durable events are in the store and reconnecting replays them.
const UPDATE_BUFFER: usize = 256;

/// What to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartTaskRequest {
    /// Conversation to attach the task to, when it has one.
    pub conversation_id: Option<ConversationId>,
    /// What the user asked for.
    pub goal: String,
    /// Which tools this task's worker may offer its model.
    ///
    /// `None` runs the service's own worker, whatever it admits. `Some` asks
    /// for a worker built for this task, which only a service given a
    /// [`WorkerChoice`] can provide. Every write or recipe the model then
    /// asks for is still approved as it happens (ADRs 0021, 0025 and 0031).
    pub admission: Option<ToolAdmission>,
    /// Where this task's models may run. `None` takes the profile's mode.
    ///
    /// Recorded when the task is created, and kept if it is resumed.
    pub routing_mode: Option<RoutingMode>,
}

/// Runtime state for a task that is currently running.
///
/// Only cancellation and live delivery live here. Everything durable is in the
/// repositories, which is why a task survives a restart even though this map
/// does not.
pub(crate) struct LiveTask {
    pub(crate) cancel: CancellationToken,
    pub(crate) updates: broadcast::Sender<TaskUpdate>,
    pub(crate) interrupt: CancellationToken,
    pub(crate) finished: tokio::sync::watch::Receiver<bool>,
}

/// Tasks running in this process.
pub(crate) type LiveTasks = Arc<Mutex<HashMap<TaskId, LiveTask>>>;

/// The repositories the service persists through.
///
/// Grouped so the constructor stays readable and the composition root assembles
/// storage in one place.
#[derive(Clone)]
pub struct Repositories {
    /// Projects and their approved roots.
    pub projects: Arc<dyn ProjectRepository>,
    /// Conversations.
    pub conversations: Arc<dyn ConversationRepository>,
    /// Task rows and their cached projection.
    pub tasks: Arc<dyn TaskRepository>,
    /// The durable event journal.
    pub events: Arc<dyn EventStore>,
    /// Secret-free effective-configuration manifests.
    pub configuration: Arc<dyn ConfigurationRepository>,
    /// Derived checkpoints kept alongside canonical events.
    pub checkpoints: Arc<dyn CheckpointRepository>,
}

impl std::fmt::Debug for Repositories {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repositories").finish_non_exhaustive()
    }
}

/// The providers a task runs against.
#[derive(Clone)]
pub struct Providers {
    /// Inference, one entry per configured endpoint.
    ///
    /// A list rather than a single provider because the router has always
    /// reasoned over *many* models from an availability snapshot; one
    /// provider was a placeholder for the milestones before endpoints could
    /// be configured. The driver dispatches to the one the routing decision
    /// named, so the record and the execution cannot come apart.
    pub models: Vec<Arc<dyn ModelProvider>>,
    /// Where tools are found. Registration grants nothing.
    pub tools: Arc<ToolRegistry>,
    /// What decides whether a tool may run.
    pub permissions: Arc<PermissionManager>,
    /// What decides whether a payload may reach a destination, and records
    /// what it disclosed.
    pub releases: Arc<crate::services::ReleaseManager>,
    /// The worker.
    pub worker: Arc<dyn AgentProvider>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers").finish_non_exhaustive()
    }
}

/// Builds a task's worker from what that task may offer its model.
///
/// For a service that runs tasks under different admissions in one process,
/// as the desktop does. A service without one runs every task with
/// [`Providers::worker`] (ADR 0031).
pub type WorkerChoice = Arc<dyn Fn(ToolAdmission) -> Arc<dyn AgentProvider> + Send + Sync>;

/// Builds the built-in worker pinned to one model, at an admission (ADR 0048).
pub type PinnedWorkerChoice =
    Arc<dyn Fn(ToolAdmission, &super::PinnedModel) -> Arc<dyn AgentProvider> + Send + Sync>;

/// A worker a task can be handed to, as a person chooses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOption {
    /// The worker's identity, which a handoff names.
    pub agent_id: String,
    /// What a person calls it.
    pub display_name: String,
    /// The model it is pinned to, or `None` for the one the router chooses.
    pub model: Option<super::PinnedModel>,
}

/// The application service the CLI and desktop shell both call.
///
/// Owns task lifecycle, event ordering, and cancellation. Adapters translate
/// their transport into these calls and translate results back; they add no
/// behavior of their own.
pub struct TaskService {
    pub(super) repositories: Repositories,
    pub(super) providers: Providers,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) ids: Arc<dyn IdSource>,
    pub(super) live: LiveTasks,
    pub(super) lifecycle: tokio::sync::Mutex<()>,
    /// The routing mode a task in this profile starts under, unless it names
    /// its own.
    ///
    /// Behind a lock because `setRoutingMode` (`CORE_CONTRACTS.md` §13) changes
    /// it while tasks are running, and a task reads it once when it starts.
    pub(super) routing_mode: Mutex<RoutingMode>,
    /// Builds a worker for a task that asks for its own admission.
    pub(super) workers: Option<WorkerChoice>,
    /// The models a person may pin the built-in worker to, and how to build
    /// it pinned (ADR 0048).
    pub(super) pinned: Option<(Vec<super::PinnedModel>, PinnedWorkerChoice)>,
    pub(super) execution_allowed: bool,
    pub(super) execution_guard: Option<Arc<dyn Send + Sync>>,
}

impl std::fmt::Debug for TaskService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskService").finish_non_exhaustive()
    }
}

impl TaskService {
    /// Assemble a service from its storage, providers, and platform ports.
    #[must_use]
    pub fn new(
        repositories: Repositories,
        providers: Providers,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
    ) -> Self {
        Self {
            repositories,
            providers,
            clock,
            ids,
            live: Arc::new(Mutex::new(HashMap::new())),
            lifecycle: tokio::sync::Mutex::new(()),
            // Local-first by default: work stays on the device until someone
            // chooses otherwise.
            routing_mode: Mutex::new(RoutingMode::Automatic),
            workers: None,
            pinned: None,
            execution_allowed: true,
            execution_guard: None,
        }
    }

    /// Start every task under `mode`.
    #[must_use]
    pub fn with_routing_mode(self, mode: RoutingMode) -> Self {
        self.set_routing_mode(mode);
        self
    }

    /// Let each task ask for its own admission, and build its worker with
    /// `workers` (ADR 0031).
    #[must_use]
    pub fn with_worker_choice(mut self, workers: WorkerChoice) -> Self {
        self.workers = Some(workers);
        self
    }

    /// Offer the built-in worker pinned to each of `models` as a worker a
    /// task can be handed to, built with `choose` (ADR 0048).
    #[must_use]
    pub fn with_pinned_workers(
        mut self,
        models: Vec<super::PinnedModel>,
        choose: PinnedWorkerChoice,
    ) -> Self {
        self.pinned = Some((models, choose));
        self
    }

    /// The workers a task can be handed to: the built-in worker as the
    /// router runs it, then pinned to each model, in the order given.
    #[must_use]
    pub fn workers(&self) -> Vec<WorkerOption> {
        let mut options = vec![WorkerOption {
            agent_id: super::REFERENCE_AGENT_ID.to_owned(),
            display_name: "Automatic: the model the routing mode chooses".to_owned(),
            model: None,
        }];
        if let Some((models, _)) = &self.pinned {
            options.extend(models.iter().map(|model| WorkerOption {
                agent_id: model.agent_id(),
                display_name: model.display_name.clone(),
                model: Some(model.clone()),
            }));
        }
        options
    }

    /// Change the routing mode. `setRoutingMode` in `CORE_CONTRACTS.md` §13.
    ///
    /// Takes effect for tasks that route after this returns; a task already
    /// past its routing decision keeps the destination it was given, because
    /// the record must say where the work actually went.
    pub fn set_routing_mode(&self, mode: RoutingMode) {
        match self.routing_mode.lock() {
            Ok(mut current) => *current = mode,
            Err(poisoned) => *poisoned.into_inner() = mode,
        }
    }

    /// The routing mode in force.
    #[must_use]
    pub fn routing_mode(&self) -> RoutingMode {
        self.routing_mode
            .lock()
            .map_or(RoutingMode::Automatic, |mode| *mode)
    }

    /// Bind production execution to an adapter-owned exclusive profile lease.
    /// A reader without the lease may inspect records but cannot recover or run work.
    #[must_use]
    pub fn with_execution_guard(mut self, guard: Option<Arc<dyn Send + Sync>>) -> Self {
        self.execution_allowed = guard.is_some();
        self.execution_guard = guard;
        self
    }

    /// Repair any task whose cached projection disagrees with its events.
    ///
    /// Run at startup. A process that died between appending an event and
    /// updating the cache leaves the two disagreeing; the events are the
    /// record, so the cache is rebuilt from them rather than trusted.
    ///
    /// Returns the number of tasks corrected.
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn recover(&self) -> Result<usize, ErrorEnvelope> {
        self.recover_interrupted().await
    }

    /// Create a project with its already-canonicalized roots.
    ///
    /// Roots arrive canonical because canonicalizing is a platform concern and
    /// a grant recorded as a symlink would authorize wherever that link
    /// pointed (`SECURITY_AND_PERMISSIONS.md` §5).
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn create_project(
        &self,
        name: impl Into<String> + Send,
        roots: Vec<String>,
    ) -> Result<ProjectRecord, ErrorEnvelope> {
        let record = ProjectRecord::new(
            ProjectId::new(self.ids.next("project")),
            name,
            roots,
            self.clock.now(),
        );
        self.repositories
            .projects
            .create(&record)
            .await
            .map_err(|error| self.repository_error(&error))?;
        Ok(record)
    }

    /// Open an existing project with the same approved roots, or create it.
    ///
    /// Exact root-list identity is intentional: overlapping roots can belong
    /// to distinct projects, while selecting the same project directory twice
    /// should not create duplicate recent-project entries.
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn open_project(
        &self,
        name: impl Into<String> + Send,
        roots: Vec<String>,
    ) -> Result<ProjectRecord, ErrorEnvelope> {
        if let Some(existing) = self
            .list_projects()
            .await?
            .into_iter()
            .find(|project| project.roots == roots)
        {
            return Ok(existing);
        }
        self.create_project(name, roots).await
    }

    /// Read one project.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist.
    pub async fn get_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectRecord, ErrorEnvelope> {
        self.repositories
            .projects
            .get(project_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Every project, most recently created first.
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn list_projects(&self) -> Result<Vec<ProjectRecord>, ErrorEnvelope> {
        self.repositories
            .projects
            .list()
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Resolve and persist the project-authored configuration inventory.
    ///
    /// Filesystem discovery is supplied by a platform adapter as bounded raw
    /// sources. Precedence, validation, task selection, and persistence stay
    /// in the application service so CLI and desktop cannot disagree.
    pub async fn resolve_configuration(
        &self,
        project_id: &ProjectId,
        sources: Vec<RawSource>,
        selection: &ConfigurationSelection,
    ) -> Result<EffectiveConfiguration, ErrorEnvelope> {
        self.get_project(project_id).await?;
        let configuration = resolve_with_selection(sources, selection, self.clock.now());
        self.repositories
            .configuration
            .save(project_id, &configuration)
            .await
            .map_err(|error| self.repository_error(&error))?;
        Ok(configuration)
    }

    /// Read the most recently resolved effective configuration for a project.
    pub async fn get_effective_configuration(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<EffectiveConfiguration>, ErrorEnvelope> {
        self.get_project(project_id).await?;
        self.repositories
            .configuration
            .latest(project_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Create a conversation, optionally inside a project.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when `project_id` names no known project.
    pub async fn create_conversation(
        &self,
        project_id: Option<ProjectId>,
        title: impl Into<String> + Send,
    ) -> Result<ConversationRecord, ErrorEnvelope> {
        if let Some(id) = &project_id {
            self.get_project(id).await?;
        }
        let record = ConversationRecord::new(
            ConversationId::new(self.ids.next("conversation")),
            project_id,
            title,
            self.clock.now(),
        );
        self.repositories
            .conversations
            .create(&record)
            .await
            .map_err(|error| self.repository_error(&error))?;
        Ok(record)
    }

    /// Conversations in a project, or projectless ones when absent.
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn list_conversations(
        &self,
        project_id: Option<&ProjectId>,
    ) -> Result<Vec<ConversationRecord>, ErrorEnvelope> {
        self.repositories
            .conversations
            .list(project_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Read one conversation.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist.
    pub async fn get_conversation(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<ConversationRecord, ErrorEnvelope> {
        self.repositories
            .conversations
            .get(conversation_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Mark a conversation archived.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist.
    pub async fn archive_conversation(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<(), ErrorEnvelope> {
        self.repositories
            .conversations
            .archive(conversation_id, &self.clock.now())
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Tasks in a conversation, or every task when absent.
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn list_tasks(
        &self,
        conversation_id: Option<&ConversationId>,
    ) -> Result<Vec<TaskRecord>, ErrorEnvelope> {
        self.repositories
            .tasks
            .list(conversation_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Read the complete durable task record.
    ///
    /// Unlike [`TaskService::get_task`], this includes project, conversation,
    /// goal, timestamps, and the configuration manifest captured at start.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task does not exist.
    pub async fn get_task_record(&self, task_id: &TaskId) -> Result<TaskRecord, ErrorEnvelope> {
        self.repositories
            .tasks
            .get(task_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Replay durable events after `after_sequence` without subscribing live.
    ///
    /// This is the inspection/recovery surface used by headless adapters. It
    /// deliberately returns only canonical events, never transient frames.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task does not exist, or a persistence
    /// error when replay fails.
    pub async fn replay_task_events(
        &self,
        task_id: &TaskId,
        after_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, ErrorEnvelope> {
        self.get_task_record(task_id).await?;
        self.repositories
            .events
            .replay(task_id, after_sequence)
            .await
            .map_err(|error| self.error(ErrorCode::PersistenceError, error.to_string()))
    }

    /// Start a task and return once it is registered, not once it finishes.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] for an unknown conversation, or a persistence
    /// error if the opening event cannot be journaled.
    pub async fn start_task(&self, request: StartTaskRequest) -> Result<TaskId, ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_execution()?;
        // Both chosen before anything is written, so a refusal leaves no
        // task behind.
        let worker = match (request.admission, &self.workers) {
            (None, _) => Arc::clone(&self.providers.worker),
            (Some(admission), Some(choose)) => choose(admission),
            (Some(_), None) => {
                return Err(self.error(
                    ErrorCode::CapabilityUnsupported,
                    "This application runs every task with one worker, and cannot choose one for this task",
                ));
            }
        };
        let routing_mode = request.routing_mode.unwrap_or_else(|| self.routing_mode());
        let project_id = match &request.conversation_id {
            Some(id) => {
                let conversation = self
                    .repositories
                    .conversations
                    .get(id)
                    .await
                    .map_err(|error| self.repository_error(&error))?;
                conversation.project_id
            }
            None => None,
        };

        let task_id = TaskId::new(self.ids.next("task"));
        let correlation_id = CorrelationId::new(self.ids.next("correlation"));
        let cancel = CancellationToken::new();
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);

        let configuration = match &project_id {
            Some(project_id) => self
                .repositories
                .configuration
                .latest(project_id)
                .await
                .map_err(|error| self.repository_error(&error))?,
            None => None,
        };

        // Every tool call this task makes inherits the project's approved
        // roots. A projectless task gets an empty scope, which the permission
        // manager reads as no filesystem access at all.
        let scope = match &project_id {
            Some(id) => CapabilityScope::project(
                id.clone(),
                self.repositories
                    .projects
                    .get(id)
                    .await
                    .map_err(|error| self.repository_error(&error))?
                    .roots,
            ),
            None => CapabilityScope {
                project_id: None,
                roots: Vec::new(),
                releases_data: false,
            },
        };

        // Redacted here, at the one point a goal enters the system, and not
        // on the way out. Redacting at output means the durable event, the
        // task record, the checkpoint, and the export bundle all held the
        // credential first, and every consumer that forgets is a leak. From
        // here on there is only the redacted text
        // (`SECURITY_AND_PERMISSIONS.md` §7).
        let goal = crate::services::redact(&request.goal);

        let created_at = self.clock.now();
        let mut record = TaskRecord::new(
            task_id.clone(),
            request.conversation_id.clone(),
            project_id,
            goal.text.clone(),
            created_at,
        );
        if let Some(configuration) = &configuration {
            record = record.with_configuration_manifest(&configuration.manifest_hash);
        }

        let mut initial_events = vec![
            EventDraft::user(
                TaskEventKind::TaskCreated,
                EventPayload::TaskCreated {
                    goal_summary: goal.text.clone(),
                    conversation_id: request.conversation_id.clone(),
                    routing_mode: Some(routing_mode),
                },
            )
            .into_envelope(
                EventId::new(self.ids.next("event")),
                task_id.clone(),
                request.conversation_id.clone(),
                1,
                self.clock.now(),
                correlation_id.clone(),
            ),
        ];
        // Cloned before the event takes ownership: the driver needs the same
        // manifest to name the instruction sources in its context record, and
        // reading it back from storage would be a second source of truth for
        // something already in hand.
        let resolved_configuration = configuration.clone();
        if let Some(configuration) = configuration {
            initial_events.push(
                EventDraft::user(
                    TaskEventKind::ConfigurationResolved,
                    EventPayload::ConfigurationResolved { configuration },
                )
                .into_envelope(
                    EventId::new(self.ids.next("event")),
                    task_id.clone(),
                    request.conversation_id.clone(),
                    2,
                    self.clock.now(),
                    correlation_id.clone(),
                ),
            );
        }

        self.repositories
            .tasks
            .create_with_initial_events(&record, &initial_events)
            .await
            .map_err(|error| self.repository_error(&error))?;

        let interrupt = CancellationToken::new();
        let (finished, finish_receiver) = tokio::sync::watch::channel(false);
        self.lock_live()?.insert(
            task_id.clone(),
            LiveTask {
                cancel: cancel.clone(),
                updates: updates.clone(),
                interrupt: interrupt.clone(),
                finished: finish_receiver,
            },
        );

        let projection = TaskProjectionWriter {
            tasks: Arc::clone(&self.repositories.tasks),
        };
        let journal = TaskJournal::new(
            task_id.clone(),
            request.conversation_id.clone(),
            correlation_id,
            JournalPorts {
                store: Arc::clone(&self.repositories.events),
                clock: Arc::clone(&self.clock),
                ids: Arc::clone(&self.ids),
                projection,
            },
            updates,
            u64::try_from(initial_events.len()).unwrap_or(u64::MAX),
        );

        // The initial batch is already durable. Publish it only after the live
        // subscription exists; replay remains the source of truth if nobody
        // is listening yet.
        for event in &initial_events {
            journal.publish_persisted(event);
        }

        let driver = TaskDriver::new(
            journal,
            self.providers.clone(),
            scope,
            Arc::clone(&self.clock),
        )
        .with_routing_mode(routing_mode)
        .with_worker(worker)
        .with_configuration(resolved_configuration)
        .with_goal_redactions(goal.reason_codes.clone())
        .with_checkpoints(Arc::clone(&self.repositories.checkpoints))
        .continuing(None, super::ResumePlan::default(), interrupt);
        // The redacted text, not `request.goal`. The worker, the manifest, and
        // the model payload all see the same thing the record does.
        let goal = goal.text;
        let execution_guard = self.execution_guard.clone();
        tokio::spawn(async move {
            let _execution_guard = execution_guard;
            driver.run(goal, cancel).await;
            let _ = finished.send(true);
        });

        Ok(task_id)
    }

    /// Request cancellation.
    ///
    /// Cancelling a task that is not running in this process is a no-op rather
    /// than an error: after a restart the task exists but its cancellation
    /// token does not, and the caller cannot know that.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task is unknown to storage.
    pub async fn cancel_task(&self, task_id: &TaskId) -> Result<(), ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_execution()?;
        self.repositories
            .tasks
            .get(task_id)
            .await
            .map_err(|error| self.repository_error(&error))?;

        if self.get_task_record(task_id).await?.state == crate::contracts::TaskState::Interrupted {
            return self.discard_interrupted(task_id).await;
        }
        if let Some(live) = self.lock_live()?.get(task_id) {
            live.cancel.cancel();
        }
        Ok(())
    }

    /// The current view of a task, read from durable storage.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task is unknown.
    pub async fn get_task(&self, task_id: &TaskId) -> Result<TaskSnapshot, ErrorEnvelope> {
        let record = self
            .repositories
            .tasks
            .get(task_id)
            .await
            .map_err(|error| self.repository_error(&error))?;

        Ok(TaskSnapshot {
            schema_version: SCHEMA_VERSION,
            task_id: record.task_id,
            state: record.state,
            last_sequence: record.last_sequence,
        })
    }

    /// Subscribe to a task's updates, catching up from `after_sequence` first.
    ///
    /// Subscribing happens *before* replaying, so nothing occurring during
    /// catch-up is lost. That can deliver an event twice; callers de-duplicate
    /// on sequence, which is far cheaper than missing one.
    ///
    /// A task that is not running in this process still replays its durable
    /// events; its receiver simply never yields.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task is unknown, or a persistence error
    /// when replay fails.
    pub async fn subscribe_task_events(
        &self,
        task_id: &TaskId,
        after_sequence: u64,
    ) -> Result<(Vec<EventEnvelope>, broadcast::Receiver<TaskUpdate>), ErrorEnvelope> {
        self.repositories
            .tasks
            .get(task_id)
            .await
            .map_err(|error| self.repository_error(&error))?;

        let receiver = match self.lock_live()?.get(task_id) {
            Some(live) => live.updates.subscribe(),
            None => broadcast::channel(1).0.subscribe(),
        };

        let replayed = self
            .repositories
            .events
            .replay(task_id, after_sequence)
            .await
            .map_err(|error| self.error(ErrorCode::PersistenceError, error.to_string()))?;

        Ok((replayed, receiver))
    }

    pub(super) fn error(&self, code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
        ErrorEnvelope::new(
            code,
            message,
            CorrelationId::new(self.ids.next("correlation")),
        )
    }

    /// Map a repository failure onto the normalized error taxonomy.
    pub(super) fn repository_error(&self, error: &RepositoryError) -> ErrorEnvelope {
        let code = match error {
            RepositoryError::NotFound { .. } => ErrorCode::NotFound,
            RepositoryError::Conflict { .. } => ErrorCode::Conflict,
            RepositoryError::Unavailable(_) => ErrorCode::PersistenceError,
        };
        self.error(code, error.to_string())
    }

    pub(super) fn lock_live(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<TaskId, LiveTask>>, ErrorEnvelope> {
        self.live
            .lock()
            .map_err(|_| self.error(ErrorCode::Internal, "live task registry lock poisoned"))
    }
}
