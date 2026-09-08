use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    ConversationId, CorrelationId, ErrorCode, ErrorEnvelope, EventDraft, EventEnvelope,
    EventPayload, ProjectId, SCHEMA_VERSION, TaskEventKind, TaskId, TaskSnapshot, TaskState,
    TaskUpdate,
};
use crate::ports::{AgentProvider, Clock, EventStore, IdSource, ModelProvider, ToolProvider};
use crate::services::journal::{JournalPorts, TaskJournal};
use crate::services::runtime::TaskDriver;

/// How many updates a subscriber may fall behind before it must replay.
///
/// Lagging past this loses live updates, which is safe rather than fatal: the
/// durable events are in the store and reconnecting replays them.
const UPDATE_BUFFER: usize = 256;

/// A project: a name and, later, its approved roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecord {
    /// Stable identifier.
    pub project_id: ProjectId,
    /// Display name.
    pub name: String,
}

/// A conversation, optionally scoped to a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRecord {
    /// Stable identifier.
    pub conversation_id: ConversationId,
    /// Project this belongs to, when it has one.
    pub project_id: Option<ProjectId>,
    /// Display title.
    pub title: String,
}

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartTaskRequest {
    /// Conversation to attach the task to, when it has one.
    pub conversation_id: Option<ConversationId>,
    /// What the user asked for.
    pub goal: String,
}

/// Everything tracked about a live task.
pub(crate) struct TaskEntry {
    pub(crate) state: TaskState,
    pub(crate) last_sequence: u64,
    pub(crate) cancel: CancellationToken,
    pub(crate) updates: broadcast::Sender<TaskUpdate>,
}

/// The shared registry of live tasks.
pub(crate) type TaskRegistry = Arc<Mutex<HashMap<TaskId, TaskEntry>>>;

/// Update the projected snapshot from a freshly written event.
///
/// State lives in the event stream; this cache exists so `getTask` does not
/// replay the whole stream. A poisoned lock is skipped rather than panicking —
/// the durable record is already written, and the cache rebuilds on replay.
pub(crate) fn project(tasks: &TaskRegistry, event: &EventEnvelope) {
    if let Ok(mut tasks) = tasks.lock()
        && let Some(entry) = tasks.get_mut(&event.task_id)
    {
        entry.last_sequence = event.sequence;
        if let EventPayload::TaskStateChanged { current, .. } = &event.payload {
            entry.state = *current;
        }
    }
}

/// The application service the CLI and desktop shell both call.
///
/// Owns task lifecycle, event ordering, and cancellation. Adapters translate
/// their transport into these calls and translate results back; they add no
/// behavior of their own.
pub struct TaskService {
    store: Arc<dyn EventStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    model: Arc<dyn ModelProvider>,
    tools: Arc<dyn ToolProvider>,
    worker: Arc<dyn AgentProvider>,
    projects: Mutex<HashMap<ProjectId, ProjectRecord>>,
    conversations: Mutex<HashMap<ConversationId, ConversationRecord>>,
    tasks: TaskRegistry,
}

impl std::fmt::Debug for TaskService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskService").finish_non_exhaustive()
    }
}

impl TaskService {
    /// Assemble a service from its ports.
    #[must_use]
    pub fn new(
        store: Arc<dyn EventStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        model: Arc<dyn ModelProvider>,
        tools: Arc<dyn ToolProvider>,
        worker: Arc<dyn AgentProvider>,
    ) -> Self {
        Self {
            store,
            clock,
            ids,
            model,
            tools,
            worker,
            projects: Mutex::new(HashMap::new()),
            conversations: Mutex::new(HashMap::new()),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a project.
    ///
    /// M1 holds projects in memory; M2 moves them behind a repository port with
    /// the rest of the operational plane.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`] if internal state is unavailable.
    pub fn create_project(&self, name: impl Into<String>) -> Result<ProjectRecord, ErrorEnvelope> {
        let record = ProjectRecord {
            project_id: ProjectId::new(self.ids.next("project")),
            name: name.into(),
        };
        self.lock_projects()?
            .insert(record.project_id.clone(), record.clone());
        Ok(record)
    }

    /// Create a conversation, optionally inside a project.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when `project_id` names no known project.
    pub fn create_conversation(
        &self,
        project_id: Option<ProjectId>,
        title: impl Into<String>,
    ) -> Result<ConversationRecord, ErrorEnvelope> {
        if let Some(id) = &project_id
            && !self.lock_projects()?.contains_key(id)
        {
            return Err(self.error(ErrorCode::NotFound, format!("unknown project {id}")));
        }

        let record = ConversationRecord {
            conversation_id: ConversationId::new(self.ids.next("conversation")),
            project_id,
            title: title.into(),
        };
        self.lock_conversations()?
            .insert(record.conversation_id.clone(), record.clone());
        Ok(record)
    }

    /// Start a task and return once it is registered, not once it finishes.
    ///
    /// The caller gets an identifier immediately and follows progress through
    /// [`Self::subscribe_task_events`], which keeps a long-running operation
    /// from blocking an interface (`CORE_CONTRACTS.md` §13).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] for an unknown conversation, or a persistence
    /// error if the opening event cannot be journaled.
    pub async fn start_task(&self, request: StartTaskRequest) -> Result<TaskId, ErrorEnvelope> {
        if let Some(id) = &request.conversation_id
            && !self.lock_conversations()?.contains_key(id)
        {
            return Err(self.error(ErrorCode::NotFound, format!("unknown conversation {id}")));
        }

        let task_id = TaskId::new(self.ids.next("task"));
        let correlation_id = CorrelationId::new(self.ids.next("correlation"));
        let cancel = CancellationToken::new();
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);

        self.lock_tasks()?.insert(
            task_id.clone(),
            TaskEntry {
                state: TaskState::Queued,
                last_sequence: 0,
                cancel: cancel.clone(),
                updates: updates.clone(),
            },
        );

        let journal = TaskJournal::new(
            task_id.clone(),
            request.conversation_id.clone(),
            correlation_id,
            JournalPorts {
                store: Arc::clone(&self.store),
                clock: Arc::clone(&self.clock),
                ids: Arc::clone(&self.ids),
            },
            updates,
            0,
        );

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
        project(&self.tasks, &created);

        let driver = TaskDriver::new(
            journal,
            Arc::clone(&self.model),
            Arc::clone(&self.tools),
            Arc::clone(&self.worker),
            Arc::clone(&self.tasks),
        );

        let goal = request.goal;
        tokio::spawn(async move {
            driver.run(goal, cancel).await;
        });

        Ok(task_id)
    }

    /// Request cancellation.
    ///
    /// Returns once the request is recorded; the task converges to
    /// [`TaskState::Cancelled`] on its own. Cancelling an already-terminal task
    /// is a no-op rather than an error, because a caller cannot know the task
    /// ended between rendering a button and pressing it.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task is unknown.
    pub fn cancel_task(&self, task_id: &TaskId) -> Result<(), ErrorEnvelope> {
        let tasks = self.lock_tasks()?;
        let entry = tasks
            .get(task_id)
            .ok_or_else(|| self.error(ErrorCode::NotFound, format!("unknown task {task_id}")))?;
        entry.cancel.cancel();
        Ok(())
    }

    /// The current view of a task.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task is unknown.
    pub fn get_task(&self, task_id: &TaskId) -> Result<TaskSnapshot, ErrorEnvelope> {
        let tasks = self.lock_tasks()?;
        let entry = tasks
            .get(task_id)
            .ok_or_else(|| self.error(ErrorCode::NotFound, format!("unknown task {task_id}")))?;
        Ok(TaskSnapshot {
            schema_version: SCHEMA_VERSION,
            task_id: task_id.clone(),
            state: entry.state,
            last_sequence: entry.last_sequence,
        })
    }

    /// Subscribe to a task's updates, catching up from `after_sequence` first.
    ///
    /// Subscribing happens *before* replaying, so nothing that occurs during
    /// catch-up is lost. That can deliver an event twice; callers de-duplicate
    /// on sequence, which is far cheaper than missing one.
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
        let receiver = {
            let tasks = self.lock_tasks()?;
            let entry = tasks.get(task_id).ok_or_else(|| {
                self.error(ErrorCode::NotFound, format!("unknown task {task_id}"))
            })?;
            entry.updates.subscribe()
        };

        let replayed = self
            .store
            .replay(task_id, after_sequence)
            .await
            .map_err(|error| self.error(ErrorCode::PersistenceError, error.to_string()))?;

        Ok((replayed, receiver))
    }

    fn error(&self, code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
        ErrorEnvelope::new(
            code,
            message,
            CorrelationId::new(self.ids.next("correlation")),
        )
    }

    fn lock_projects(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<ProjectId, ProjectRecord>>, ErrorEnvelope> {
        self.projects
            .lock()
            .map_err(|_| self.error(ErrorCode::Internal, "project registry lock poisoned"))
    }

    fn lock_conversations(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<ConversationId, ConversationRecord>>, ErrorEnvelope>
    {
        self.conversations
            .lock()
            .map_err(|_| self.error(ErrorCode::Internal, "conversation registry lock poisoned"))
    }

    fn lock_tasks(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<TaskId, TaskEntry>>, ErrorEnvelope> {
        self.tasks
            .lock()
            .map_err(|_| self.error(ErrorCode::Internal, "task registry lock poisoned"))
    }
}
