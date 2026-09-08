use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    ConversationId, ConversationRecord, CorrelationId, ErrorCode, ErrorEnvelope, EventDraft,
    EventEnvelope, EventPayload, ProjectId, ProjectRecord, SCHEMA_VERSION, TaskEventKind, TaskId,
    TaskRecord, TaskSnapshot, TaskUpdate,
};
use crate::ports::{
    AgentProvider, Clock, ConversationRepository, EventStore, IdSource, ModelProvider,
    ProjectRepository, RepositoryError, TaskRepository, ToolProvider,
};
use crate::services::journal::{JournalPorts, TaskJournal};
use crate::services::projection::TaskProjectionWriter;
use crate::services::runtime::TaskDriver;

/// How many updates a subscriber may fall behind before it must replay.
///
/// Lagging past this loses live updates, which is safe rather than fatal: the
/// durable events are in the store and reconnecting replays them.
const UPDATE_BUFFER: usize = 256;

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartTaskRequest {
    /// Conversation to attach the task to, when it has one.
    pub conversation_id: Option<ConversationId>,
    /// What the user asked for.
    pub goal: String,
}

/// Runtime state for a task that is currently running.
///
/// Only cancellation and live delivery live here. Everything durable is in the
/// repositories, which is why a task survives a restart even though this map
/// does not.
pub(crate) struct LiveTask {
    pub(crate) cancel: CancellationToken,
    pub(crate) updates: broadcast::Sender<TaskUpdate>,
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
}

impl std::fmt::Debug for Repositories {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repositories").finish_non_exhaustive()
    }
}

/// The providers a task runs against.
#[derive(Clone)]
pub struct Providers {
    /// Inference.
    pub model: Arc<dyn ModelProvider>,
    /// Tools.
    pub tools: Arc<dyn ToolProvider>,
    /// The worker.
    pub worker: Arc<dyn AgentProvider>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers").finish_non_exhaustive()
    }
}

/// The application service the CLI and desktop shell both call.
///
/// Owns task lifecycle, event ordering, and cancellation. Adapters translate
/// their transport into these calls and translate results back; they add no
/// behavior of their own.
pub struct TaskService {
    repositories: Repositories,
    providers: Providers,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    live: LiveTasks,
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
        }
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
        self.repositories
            .tasks
            .rebuild_from_events()
            .await
            .map_err(|error| self.repository_error(&error))
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

    /// Read one project.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist.
    pub async fn get_project(&self, project_id: &ProjectId) -> Result<ProjectRecord, ErrorEnvelope> {
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

    /// Start a task and return once it is registered, not once it finishes.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] for an unknown conversation, or a persistence
    /// error if the opening event cannot be journaled.
    pub async fn start_task(&self, request: StartTaskRequest) -> Result<TaskId, ErrorEnvelope> {
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

        self.repositories
            .tasks
            .create(&TaskRecord::new(
                task_id.clone(),
                request.conversation_id.clone(),
                project_id,
                request.goal.clone(),
                self.clock.now(),
            ))
            .await
            .map_err(|error| self.repository_error(&error))?;

        self.lock_live()?.insert(
            task_id.clone(),
            LiveTask { cancel: cancel.clone(), updates: updates.clone() },
        );

        let journal = TaskJournal::new(
            task_id.clone(),
            request.conversation_id.clone(),
            correlation_id,
            JournalPorts {
                store: Arc::clone(&self.repositories.events),
                clock: Arc::clone(&self.clock),
                ids: Arc::clone(&self.ids),
            },
            updates,
            0,
        );

        let projection = TaskProjectionWriter {
            tasks: Arc::clone(&self.repositories.tasks),
            clock: Arc::clone(&self.clock),
        };

        // Written before returning, so a caller that subscribes immediately
        // afterwards always replays a non-empty stream.
        let created = journal
            .append(EventDraft::user(
                TaskEventKind::TaskCreated,
                EventPayload::TaskCreated {
                    goal_summary: request.goal.clone(),
                    conversation_id: request.conversation_id.clone(),
                },
            ))
            .await?;
        projection.record(&created).await;

        let driver = TaskDriver::new(journal, self.providers.clone(), projection);
        let goal = request.goal;
        tokio::spawn(async move {
            driver.run(goal, cancel).await;
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
        self.repositories
            .tasks
            .get(task_id)
            .await
            .map_err(|error| self.repository_error(&error))?;

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

    fn error(&self, code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
        ErrorEnvelope::new(code, message, CorrelationId::new(self.ids.next("correlation")))
    }

    /// Map a repository failure onto the normalized error taxonomy.
    fn repository_error(&self, error: &RepositoryError) -> ErrorEnvelope {
        let code = match error {
            RepositoryError::NotFound { .. } => ErrorCode::NotFound,
            RepositoryError::Unavailable(_) => ErrorCode::PersistenceError,
        };
        self.error(code, error.to_string())
    }

    fn lock_live(&self) -> Result<std::sync::MutexGuard<'_, HashMap<TaskId, LiveTask>>, ErrorEnvelope> {
        self.live
            .lock()
            .map_err(|_| self.error(ErrorCode::Internal, "live task registry lock poisoned"))
    }
}
