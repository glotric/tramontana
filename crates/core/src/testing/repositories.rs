use std::sync::Mutex;

use async_trait::async_trait;

use crate::contracts::{
    CheckpointId, ConversationId, ConversationRecord, EffectiveConfiguration, EventPayload,
    ProjectId, ProjectRecord, TaskCheckpoint, TaskId, TaskRecord, TaskState, Timestamp,
};
use crate::ports::{
    CheckpointRepository, ConfigurationRepository, ConversationRepository, EventStore,
    ProjectRepository, RepositoryError, TaskRepository,
};

/// The full in-memory repository set, with tasks projected from `events`.
pub fn in_memory_repositories(
    events: std::sync::Arc<dyn EventStore>,
) -> crate::services::Repositories {
    use std::sync::Arc;
    crate::services::Repositories {
        projects: Arc::new(InMemoryProjectRepository::new()),
        conversations: Arc::new(InMemoryConversationRepository::new()),
        tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
        events,
        configuration: Arc::new(InMemoryConfigurationRepository::new()),
        checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
    }
}

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
    default_new!("An empty repository.");
}

#[async_trait]
impl ProjectRepository for InMemoryProjectRepository {
    async fn delete(&self, project_id: &ProjectId) -> Result<(), RepositoryError> {
        let mut projects = lock(&self.projects)?;
        let before = projects.len();
        projects.retain(|record| record.project_id != *project_id);
        if projects.len() == before {
            return Err(RepositoryError::not_found("project", project_id));
        }
        Ok(())
    }

    async fn create(&self, record: &ProjectRecord) -> Result<(), RepositoryError> {
        lock(&self.projects)?.push(record.clone());
        Ok(())
    }

    async fn get(&self, project_id: &ProjectId) -> Result<ProjectRecord, RepositoryError> {
        lock(&self.projects)?
            .iter()
            .find(|record| record.project_id == *project_id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("project", project_id))
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
    default_new!("An empty repository.");
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
            .ok_or_else(|| RepositoryError::not_found("conversation", conversation_id))
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

    async fn rename(
        &self,
        conversation_id: &ConversationId,
        title: &str,
    ) -> Result<(), RepositoryError> {
        let mut conversations = lock(&self.conversations)?;
        let record = conversations
            .iter_mut()
            .find(|record| record.conversation_id == *conversation_id)
            .ok_or_else(|| RepositoryError::not_found("conversation", conversation_id))?;
        title.clone_into(&mut record.title);
        Ok(())
    }

    async fn delete(&self, conversation_id: &ConversationId) -> Result<(), RepositoryError> {
        let mut conversations = lock(&self.conversations)?;
        let before = conversations.len();
        conversations.retain(|record| record.conversation_id != *conversation_id);
        if conversations.len() == before {
            return Err(RepositoryError::not_found("conversation", conversation_id));
        }
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

opaque_debug!(InMemoryTaskRepository);

impl InMemoryTaskRepository {
    default_new!("An empty repository with no event store, so recovery is a no-op.");

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
    async fn delete(&self, task_id: &TaskId) -> Result<(), RepositoryError> {
        let mut tasks = lock(&self.tasks)?;
        let before = tasks.len();
        tasks.retain(|record| record.task_id != *task_id);
        if tasks.len() == before {
            return Err(RepositoryError::not_found("task", task_id));
        }
        Ok(())
    }

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
                        ..
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
            .ok_or_else(|| RepositoryError::not_found("task", task_id))
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
            .ok_or_else(|| RepositoryError::not_found("task", task_id))?;
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
    default_new!("An empty repository.");
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
            .ok_or_else(|| RepositoryError::not_found("configuration manifest", manifest_hash))
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

/// Checkpoints held in memory.
#[derive(Debug, Default)]
pub struct InMemoryCheckpointRepository {
    checkpoints: Mutex<Vec<TaskCheckpoint>>,
}

impl InMemoryCheckpointRepository {
    default_new!("An empty repository.");
}

#[async_trait]
impl CheckpointRepository for InMemoryCheckpointRepository {
    async fn save(&self, checkpoint: &TaskCheckpoint) -> Result<(), RepositoryError> {
        let mut checkpoints = lock(&self.checkpoints)?;
        let taken = checkpoints.iter().any(|existing| {
            existing.task_id == checkpoint.task_id
                && existing.event_sequence == checkpoint.event_sequence
        });
        if taken {
            // One point in a stream has one checkpoint. Two would disagree
            // about what was pending there, and nothing could say which was
            // right.
            return Err(RepositoryError::Conflict {
                kind: "checkpoint",
                id: format!("{}@{}", checkpoint.task_id, checkpoint.event_sequence),
            });
        }
        checkpoints.push(checkpoint.clone());
        Ok(())
    }

    async fn get(&self, checkpoint_id: &CheckpointId) -> Result<TaskCheckpoint, RepositoryError> {
        lock(&self.checkpoints)?
            .iter()
            .find(|checkpoint| checkpoint.checkpoint_id == *checkpoint_id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("checkpoint", checkpoint_id))
    }

    async fn latest(&self, task_id: &TaskId) -> Result<Option<TaskCheckpoint>, RepositoryError> {
        Ok(lock(&self.checkpoints)?
            .iter()
            .filter(|checkpoint| checkpoint.task_id == *task_id)
            .max_by_key(|checkpoint| checkpoint.event_sequence)
            .cloned())
    }

    async fn list(&self, task_id: &TaskId) -> Result<Vec<TaskCheckpoint>, RepositoryError> {
        let mut found: Vec<TaskCheckpoint> = lock(&self.checkpoints)?
            .iter()
            .filter(|checkpoint| checkpoint.task_id == *task_id)
            .cloned()
            .collect();
        found.sort_by_key(|checkpoint| checkpoint.event_sequence);
        Ok(found)
    }
}

/// The person's profile, held in memory (ADR 0061).
#[derive(Debug, Default)]
pub struct InMemoryPersonalRepository {
    profile: Mutex<Option<crate::contracts::PersonalProfile>>,
}

impl InMemoryPersonalRepository {
    default_new!("An empty store: loading gives the default profile.");
}

#[async_trait]
impl crate::ports::PersonalRepository for InMemoryPersonalRepository {
    async fn load(&self) -> Result<crate::contracts::PersonalProfile, RepositoryError> {
        Ok(lock(&self.profile)?.clone().unwrap_or_default())
    }

    async fn save(
        &self,
        profile: &crate::contracts::PersonalProfile,
    ) -> Result<(), RepositoryError> {
        *lock(&self.profile)? = Some(profile.clone());
        Ok(())
    }
}

/// Schedules, held in memory (ADR 0063).
#[derive(Debug, Default)]
pub struct InMemoryScheduleRepository {
    schedules: Mutex<Vec<crate::contracts::Schedule>>,
}

impl InMemoryScheduleRepository {
    default_new!("An empty store.");
}

#[async_trait]
impl crate::ports::ScheduleRepository for InMemoryScheduleRepository {
    async fn list(&self) -> Result<Vec<crate::contracts::Schedule>, RepositoryError> {
        Ok(lock(&self.schedules)?.clone())
    }

    async fn save(&self, schedule: &crate::contracts::Schedule) -> Result<(), RepositoryError> {
        let mut schedules = lock(&self.schedules)?;
        match schedules
            .iter_mut()
            .find(|kept| kept.schedule_id == schedule.schedule_id)
        {
            Some(kept) => *kept = schedule.clone(),
            None => schedules.push(schedule.clone()),
        }
        Ok(())
    }

    async fn delete(&self, schedule_id: &str) -> Result<(), RepositoryError> {
        let mut schedules = lock(&self.schedules)?;
        let before = schedules.len();
        schedules.retain(|kept| kept.schedule_id != schedule_id);
        if schedules.len() == before {
            return Err(RepositoryError::not_found("schedule", schedule_id));
        }
        Ok(())
    }
}

/// The dictation setup, held in memory (ADR 0065).
#[derive(Debug, Default)]
pub struct InMemoryDictationRepository {
    setup: Mutex<Option<crate::contracts::DictationSetup>>,
}

impl InMemoryDictationRepository {
    default_new!("Nothing set up.");
}

#[async_trait]
impl crate::ports::DictationRepository for InMemoryDictationRepository {
    async fn load(&self) -> Result<Option<crate::contracts::DictationSetup>, RepositoryError> {
        Ok(lock(&self.setup)?.clone())
    }

    async fn save(&self, setup: &crate::contracts::DictationSetup) -> Result<(), RepositoryError> {
        *lock(&self.setup)? = Some(setup.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<(), RepositoryError> {
        *lock(&self.setup)? = None;
        Ok(())
    }
}

/// The person's skill library, in memory (ADR 0070).
#[derive(Debug, Default)]
pub struct InMemorySkillLibrary {
    files: Mutex<std::collections::BTreeMap<String, String>>,
}

impl InMemorySkillLibrary {
    default_new!("An empty library.");

    /// A library holding `files`, as `(folder, SKILL.md)` pairs, without the
    /// checks an import makes: as if copied in by hand.
    #[must_use]
    pub fn holding(files: &[(&str, &str)]) -> Self {
        Self {
            files: Mutex::new(
                files
                    .iter()
                    .map(|(folder, markdown)| ((*folder).to_owned(), (*markdown).to_owned()))
                    .collect(),
            ),
        }
    }
}

#[async_trait]
impl crate::ports::SkillLibrary for InMemorySkillLibrary {
    async fn list(&self) -> Result<Vec<crate::ports::SkillFile>, RepositoryError> {
        Ok(lock(&self.files)?
            .iter()
            .map(|(folder, markdown)| crate::ports::SkillFile {
                folder: folder.clone(),
                markdown: markdown.clone(),
            })
            .collect())
    }

    async fn add(&self, file: &crate::ports::SkillFile) -> Result<(), RepositoryError> {
        let mut files = lock(&self.files)?;
        if files.contains_key(&file.folder) {
            return Err(RepositoryError::Conflict {
                kind: "skill",
                id: file.folder.clone(),
            });
        }
        files.insert(file.folder.clone(), file.markdown.clone());
        Ok(())
    }

    async fn remove(&self, folder: &str) -> Result<(), RepositoryError> {
        lock(&self.files)?
            .remove(folder)
            .map(|_| ())
            .ok_or_else(|| RepositoryError::not_found("skill", folder))
    }
}

/// A transcriber that answers with what it was built with, and counts the
/// audio it was given (ADR 0065).
#[derive(Debug)]
pub struct FakeTranscriber {
    text: String,
    heard: Mutex<Vec<usize>>,
}

impl FakeTranscriber {
    /// A transcriber that hears `text` in anything.
    #[must_use]
    pub fn hearing(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            heard: Mutex::new(Vec::new()),
        }
    }

    /// The sizes of the audio it was given, in order.
    #[must_use]
    pub fn heard(&self) -> Vec<usize> {
        self.heard
            .lock()
            .map(|heard| heard.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl crate::ports::Transcriber for FakeTranscriber {
    fn inspect(
        &self,
        _binary_path: &str,
        _model_path: &str,
        recorded_at: crate::contracts::Timestamp,
    ) -> Result<crate::contracts::RuntimeProvenance, crate::contracts::ErrorEnvelope> {
        Ok(crate::contracts::RuntimeProvenance {
            executable_sha256: "0".repeat(64),
            executable_bytes: 1,
            model_sha256: "1".repeat(64),
            model_bytes: 1,
            recorded_at,
        })
    }

    async fn transcribe(
        &self,
        _setup: &crate::contracts::DictationSetup,
        wav: Vec<u8>,
    ) -> Result<String, crate::contracts::ErrorEnvelope> {
        if let Ok(mut heard) = self.heard.lock() {
            heard.push(wav.len());
        }
        Ok(self.text.clone())
    }
}
