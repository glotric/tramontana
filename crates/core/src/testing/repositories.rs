use std::sync::Mutex;

use async_trait::async_trait;

use crate::contracts::{
    ConversationId, ConversationRecord, EffectiveConfiguration, EventPayload, ProjectId,
    ProjectRecord, TaskId, TaskRecord, TaskState, Timestamp,
};
use crate::ports::{
    ConfigurationRepository, ConversationRepository, EventStore, ProjectRepository,
    RepositoryError, TaskRepository,
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
        f.debug_struct("InMemoryTaskRepository")
            .finish_non_exhaustive()
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

    async fn create_with_initial_events(
        &self,
        record: &TaskRecord,
        initial_events: &[crate::contracts::EventEnvelope],
    ) -> Result<(), RepositoryError> {
        if initial_events.is_empty()
            || initial_events.iter().enumerate().any(|(index, event)| {
                event.task_id != record.task_id
                    || event.conversation_id != record.conversation_id
                    || event.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
            })
            || !matches!(
                initial_events
                    .first()
                    .map(|event| (&event.kind, &event.payload)),
                Some((
                    crate::contracts::TaskEventKind::TaskCreated,
                    EventPayload::TaskCreated {
                        goal_summary,
                        conversation_id,
                    }
                )) if goal_summary == &record.goal_summary
                    && conversation_id == &record.conversation_id
            )
        {
            return Err(RepositoryError::Unavailable(
                "initial task events must be consecutive from sequence 1 for the new task"
                    .to_owned(),
            ));
        }
        if lock(&self.tasks)?
            .iter()
            .any(|existing| existing.task_id == record.task_id)
        {
            return Err(RepositoryError::Conflict {
                kind: "task",
                id: record.task_id.to_string(),
            });
        }
        let Some(event_store) = &self.events else {
            return Err(RepositoryError::Unavailable(
                "atomic task creation requires an event store".to_owned(),
            ));
        };
        event_store
            .append(&record.task_id, 0, initial_events.to_vec())
            .await
            .map_err(|error| RepositoryError::Unavailable(error.to_string()))?;

        let mut persisted = record.clone();
        persisted.last_sequence = u64::try_from(initial_events.len()).unwrap_or(u64::MAX);
        if let Some(event) = initial_events.last() {
            persisted.updated_at = event.occurred_at.clone();
        }
        lock(&self.tasks)?.push(persisted);
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

        let tasks_to_rebuild: Vec<(TaskId, Timestamp)> = lock(&self.tasks)?
            .iter()
            .map(|record| (record.task_id.clone(), record.created_at.clone()))
            .collect();

        let mut corrected = 0;
        for (task_id, created_at) in tasks_to_rebuild {
            let stream = events
                .replay(&task_id, 0)
                .await
                .map_err(|error| RepositoryError::Unavailable(error.to_string()))?;

            let actual_sequence = stream.last().map_or(0, |event| event.sequence);
            let actual_updated_at = stream
                .last()
                .map(|event| event.occurred_at.clone())
                .unwrap_or(created_at);
            let actual_state = stream.iter().rev().find_map(|event| match &event.payload {
                EventPayload::TaskStateChanged { current, .. } => Some(*current),
                _ => None,
            });

            let mut tasks = lock(&self.tasks)?;
            let Some(record) = tasks.iter_mut().find(|r| r.task_id == task_id) else {
                continue;
            };
            let state = actual_state.unwrap_or(record.state);
            if record.last_sequence == actual_sequence
                && record.state == state
                && record.updated_at == actual_updated_at
            {
                continue;
            }
            record.last_sequence = actual_sequence;
            record.state = state;
            record.updated_at = actual_updated_at;
            corrected += 1;
        }

        Ok(corrected)
    }
}

/// Effective-configuration manifests held in memory.
#[derive(Debug, Default)]
pub struct InMemoryConfigurationRepository {
    manifests: Mutex<Vec<(ProjectId, EffectiveConfiguration)>>,
}

impl InMemoryConfigurationRepository {
    /// An empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConfigurationRepository for InMemoryConfigurationRepository {
    async fn save(
        &self,
        project_id: &ProjectId,
        configuration: &EffectiveConfiguration,
    ) -> Result<(), RepositoryError> {
        let mut manifests = lock(&self.manifests)?;
        if let Some((_, existing)) = manifests.iter_mut().find(|(stored_project, stored)| {
            stored_project == project_id && stored.manifest_hash == configuration.manifest_hash
        }) {
            *existing = configuration.clone();
        } else {
            manifests.push((project_id.clone(), configuration.clone()));
        }
        Ok(())
    }

    async fn get(
        &self,
        project_id: &ProjectId,
        manifest_hash: &str,
    ) -> Result<EffectiveConfiguration, RepositoryError> {
        lock(&self.manifests)?
            .iter()
            .find(|(stored_project, stored)| {
                stored_project == project_id && stored.manifest_hash == manifest_hash
            })
            .map(|(_, stored)| stored.clone())
            .ok_or_else(|| RepositoryError::NotFound {
                kind: "configuration manifest",
                id: manifest_hash.to_owned(),
            })
    }

    async fn latest(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<EffectiveConfiguration>, RepositoryError> {
        Ok(lock(&self.manifests)?
            .iter()
            .filter(|(stored_project, _)| stored_project == project_id)
            .map(|(_, stored)| stored)
            .max_by(|a, b| {
                a.resolved_at
                    .cmp(&b.resolved_at)
                    .then_with(|| a.manifest_hash.cmp(&b.manifest_hash))
            })
            .cloned())
    }
}
