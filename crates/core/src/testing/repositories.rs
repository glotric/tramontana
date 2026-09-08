use std::sync::Mutex;

use async_trait::async_trait;

use crate::contracts::{
    ConversationId, ConversationRecord, EventPayload, ProjectId, ProjectRecord, TaskId, TaskRecord,
    TaskState, Timestamp,
};
use crate::ports::{
    ConversationRepository, EventStore, ProjectRepository, RepositoryError, TaskRepository,
};

/// Lock a map, mapping a poisoned lock to a normalized error.
fn lock<T>(value: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, RepositoryError> {
    value
        .lock()
        .map_err(|_| RepositoryError::Unavailable("repository lock poisoned".to_owned()))
}

/// Projects held in memory.
#[derive(Debug, Default)]
pub struct InMemoryProjectRepository {
    projects: Mutex<Vec<ProjectRecord>>,
}

impl InMemoryProjectRepository {
    /// An empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ProjectRepository for InMemoryProjectRepository {
    async fn create(&self, record: &ProjectRecord) -> Result<(), RepositoryError> {
        lock(&self.projects)?.push(record.clone());
        Ok(())
    }

    async fn get(&self, project_id: &ProjectId) -> Result<ProjectRecord, RepositoryError> {
        lock(&self.projects)?
            .iter()
            .find(|record| record.project_id == *project_id)
            .cloned()
            .ok_or_else(|| RepositoryError::NotFound {
                kind: "project",
                id: project_id.to_string(),
            })
    }

    async fn list(&self) -> Result<Vec<ProjectRecord>, RepositoryError> {
        let mut projects = lock(&self.projects)?.clone();
        projects.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.project_id.cmp(&b.project_id))
        });
        Ok(projects)
    }
}

/// Conversations held in memory.
#[derive(Debug, Default)]
pub struct InMemoryConversationRepository {
    conversations: Mutex<Vec<ConversationRecord>>,
}

impl InMemoryConversationRepository {
    /// An empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConversationRepository for InMemoryConversationRepository {
    async fn create(&self, record: &ConversationRecord) -> Result<(), RepositoryError> {
        lock(&self.conversations)?.push(record.clone());
        Ok(())
    }

    async fn get(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<ConversationRecord, RepositoryError> {
        lock(&self.conversations)?
            .iter()
            .find(|record| record.conversation_id == *conversation_id)
            .cloned()
            .ok_or_else(|| RepositoryError::NotFound {
                kind: "conversation",
                id: conversation_id.to_string(),
            })
    }

    async fn list(
        &self,
        project_id: Option<&ProjectId>,
    ) -> Result<Vec<ConversationRecord>, RepositoryError> {
        Ok(lock(&self.conversations)?
            .iter()
            .filter(|record| record.project_id.as_ref() == project_id)
            .cloned()
            .collect())
    }

    async fn archive(
        &self,
        conversation_id: &ConversationId,
        at: &Timestamp,
    ) -> Result<(), RepositoryError> {
        let mut conversations = lock(&self.conversations)?;
        let record = conversations
            .iter_mut()
            .find(|record| record.conversation_id == *conversation_id)
            .ok_or_else(|| RepositoryError::NotFound {
                kind: "conversation",
                id: conversation_id.to_string(),
            })?;
        record.archived_at = Some(at.clone());
        Ok(())
    }
}

/// Task rows held in memory, optionally backed by an event store.
///
/// The event store is what makes [`TaskRepository::rebuild_from_events`]
/// meaningful here: without it the fake could claim recovery works while doing
/// nothing, and the contract suite would pass against a lie.
#[derive(Default)]
pub struct InMemoryTaskRepository {
    tasks: Mutex<Vec<TaskRecord>>,
    events: Option<std::sync::Arc<dyn EventStore>>,
}

impl std::fmt::Debug for InMemoryTaskRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryTaskRepository").finish_non_exhaustive()
    }
}

impl InMemoryTaskRepository {
    /// An empty repository with no event store, so recovery is a no-op.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty repository that can rebuild projections from `events`.
    #[must_use]
    pub fn with_events(events: std::sync::Arc<dyn EventStore>) -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
            events: Some(events),
        }
    }
}

#[async_trait]
impl TaskRepository for InMemoryTaskRepository {
    async fn create(&self, record: &TaskRecord) -> Result<(), RepositoryError> {
        lock(&self.tasks)?.push(record.clone());
        Ok(())
    }

    async fn get(&self, task_id: &TaskId) -> Result<TaskRecord, RepositoryError> {
        lock(&self.tasks)?
            .iter()
            .find(|record| record.task_id == *task_id)
            .cloned()
            .ok_or_else(|| RepositoryError::NotFound {
                kind: "task",
                id: task_id.to_string(),
            })
    }

    async fn list(
        &self,
        conversation_id: Option<&ConversationId>,
    ) -> Result<Vec<TaskRecord>, RepositoryError> {
        Ok(lock(&self.tasks)?
            .iter()
            .filter(|record| {
                conversation_id.is_none() || record.conversation_id.as_ref() == conversation_id
            })
            .cloned()
            .collect())
    }

    async fn update_projection(
        &self,
        task_id: &TaskId,
        state: TaskState,
        last_sequence: u64,
        at: &Timestamp,
    ) -> Result<(), RepositoryError> {
        let mut tasks = lock(&self.tasks)?;
        let record = tasks
            .iter_mut()
            .find(|record| record.task_id == *task_id)
            .ok_or_else(|| RepositoryError::NotFound {
                kind: "task",
                id: task_id.to_string(),
            })?;
        record.state = state;
        record.last_sequence = last_sequence;
        record.updated_at = at.clone();
        Ok(())
    }

    async fn rebuild_from_events(&self) -> Result<usize, RepositoryError> {
        let Some(events) = &self.events else {
            return Ok(0);
        };

        let ids: Vec<TaskId> = lock(&self.tasks)?
            .iter()
            .map(|record| record.task_id.clone())
            .collect();

        let mut corrected = 0;
        for task_id in ids {
            let stream = events
                .replay(&task_id, 0)
                .await
                .map_err(|error| RepositoryError::Unavailable(error.to_string()))?;

            let actual_sequence = stream.last().map_or(0, |event| event.sequence);
            let actual_state = stream.iter().rev().find_map(|event| match &event.payload {
                EventPayload::TaskStateChanged { current, .. } => Some(*current),
                _ => None,
            });

            let mut tasks = lock(&self.tasks)?;
            let Some(record) = tasks.iter_mut().find(|r| r.task_id == task_id) else {
                continue;
            };
            let state = actual_state.unwrap_or(record.state);
            if record.last_sequence == actual_sequence && record.state == state {
                continue;
            }
            record.last_sequence = actual_sequence;
            record.state = state;
            corrected += 1;
        }

        Ok(corrected)
    }
}
