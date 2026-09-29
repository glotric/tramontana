use async_trait::async_trait;

use crate::contracts::{
    AppConfig, CheckpointId, ConversationId, ConversationRecord, EffectiveConfiguration,
    EventEnvelope, ProjectId, ProjectRecord, ProviderEndpoint, TaskCheckpoint, TaskId, TaskRecord,
    TaskState, Timestamp,
};

/// Why a repository call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RepositoryError {
    /// The addressed record does not exist.
    #[error("{kind} {id} not found")]
    NotFound {
        /// What kind of record was addressed.
        kind: &'static str,
        /// The identifier that matched nothing.
        id: String,
    },
    /// A record or optimistic write conflicts with durable state.
    #[error("{kind} {id} conflicts with an existing record")]
    Conflict {
        /// What kind of record conflicted.
        kind: &'static str,
        /// The identifier that already exists.
        id: String,
    },
    /// Durable storage failed.
    #[error("repository unavailable: {0}")]
    Unavailable(String),
}

/// Projects and their approved roots.
///
/// A root is stored in canonical form. A grant recorded as a symlink would
/// authorize wherever that link happened to point, which is why canonicalizing
/// before persisting is a security property and not a tidiness one
/// (`SECURITY_AND_PERMISSIONS.md` §5).
#[async_trait]
pub trait ProjectRepository: Send + Sync {
    /// Create a project with its approved roots.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn create(&self, record: &ProjectRecord) -> Result<(), RepositoryError>;

    /// Read one project.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn get(&self, project_id: &ProjectId) -> Result<ProjectRecord, RepositoryError>;

    /// Every project, most recently created first.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(&self) -> Result<Vec<ProjectRecord>, RepositoryError>;

    /// Remove a project's record: its row and roots, and whatever the store
    /// keeps under it (ADR 0064). Its conversations and tasks are removed
    /// first by the caller. Never touches the project's files.
    ///
    /// The default refuses, for stores that cannot forget a project.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn delete(&self, project_id: &ProjectId) -> Result<(), RepositoryError> {
        let _ = project_id;
        Err(RepositoryError::Unavailable(
            "this store cannot remove a project".to_owned(),
        ))
    }
}

/// Conversations.
#[async_trait]
pub trait ConversationRepository: Send + Sync {
    /// Create a conversation.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn create(&self, record: &ConversationRecord) -> Result<(), RepositoryError>;

    /// Read one conversation.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn get(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<ConversationRecord, RepositoryError>;

    /// Conversations in a project, or projectless ones when `project_id` is absent.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(
        &self,
        project_id: Option<&ProjectId>,
    ) -> Result<Vec<ConversationRecord>, RepositoryError>;

    /// Mark a conversation archived.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn archive(
        &self,
        conversation_id: &ConversationId,
        at: &Timestamp,
    ) -> Result<(), RepositoryError>;

    /// Give a conversation a new title (ADR 0059).
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn rename(
        &self,
        conversation_id: &ConversationId,
        title: &str,
    ) -> Result<(), RepositoryError>;

    /// Remove a conversation (ADR 0059). Its tasks are removed first, with
    /// [`TaskRepository::delete`]; a store that cascades may also remove any
    /// left.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn delete(&self, conversation_id: &ConversationId) -> Result<(), RepositoryError>;
}

/// Task records.
///
/// The task's *state* is a projection of its event stream; this row is a cache
/// so reading a task does not require replaying it. When the two disagree the
/// events win, and [`TaskRepository::rebuild_from_events`] is how the cache is
/// corrected after an unclean stop.
#[async_trait]
pub trait TaskRepository: Send + Sync {
    /// Create a task row.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn create(&self, record: &TaskRecord) -> Result<(), RepositoryError>;

    /// Create a task and its non-empty initial event batch atomically.
    ///
    /// A task row without `taskCreated` has no canonical history. Concrete
    /// durable stores must commit both records in one transaction so process
    /// death cannot leave that orphan state behind.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Conflict`] when either identity already exists, and
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn create_with_initial_events(
        &self,
        record: &TaskRecord,
        events: &[EventEnvelope],
    ) -> Result<(), RepositoryError>;

    /// Read one task.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn get(&self, task_id: &TaskId) -> Result<TaskRecord, RepositoryError>;

    /// Tasks in a conversation, or every task when `conversation_id` is absent.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(
        &self,
        conversation_id: Option<&ConversationId>,
    ) -> Result<Vec<TaskRecord>, RepositoryError>;

    /// Update the cached state and sequence after an event was written.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when the task does not exist.
    async fn update_projection(
        &self,
        task_id: &TaskId,
        state: TaskState,
        last_sequence: u64,
        at: &Timestamp,
    ) -> Result<(), RepositoryError>;

    /// Recompute every task's cached projection from the durable events.
    ///
    /// Run at startup. A process that died between appending an event and
    /// updating the cache leaves the two disagreeing; the events are the record,
    /// so the cache is rebuilt from them rather than trusted.
    ///
    /// Returns the number of tasks whose cache was corrected.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn rebuild_from_events(&self) -> Result<usize, RepositoryError>;

    /// Remove a task's row and what is kept with it, such as its checkpoint
    /// (ADR 0059). Its events are removed separately, with
    /// [`crate::ports::EventStore::delete_stream`], after the row: a row
    /// without its events would be an invalid record, and events without a
    /// row are only unreachable.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn delete(&self, task_id: &TaskId) -> Result<(), RepositoryError>;
}

/// Secret-free effective-configuration manifests.
///
/// Markdown bodies stay in the project-authored plane. This repository stores
/// only the hashes and metadata needed to explain which configuration a task
/// used after the files change.
/// Where a profile keeps the model providers the user configured.
///
/// Profile-scoped, not project-scoped, and deliberately so.
/// `CONFIGURATION.md` §153 forbids project-authored Markdown from hard-coding
/// a cloud provider or embedding credentials: which endpoints exist is a
/// property of *this machine and this person*, not of a repository that gets
/// shared. A project that could name its own provider would carry a routing
/// decision into every checkout of it.
#[async_trait]
pub trait ProviderSettingsRepository: Send + Sync {
    /// Every configured endpoint, in a stable order.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(&self) -> Result<Vec<ProviderEndpoint>, RepositoryError>;

    /// Store an endpoint, replacing any with the same identifier.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn save(&self, endpoint: &ProviderEndpoint) -> Result<(), RepositoryError>;

    /// Remove an endpoint. Returns whether one was there.
    ///
    /// Removing the configuration does **not** remove the credential: that
    /// lives in the operating system store and is the user's to delete, with
    /// `tramontana secret forget` or their platform's own tool. Deleting it
    /// here would silently destroy something shared with other applications
    /// (ADR 0008).
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn delete(&self, endpoint_id: &str) -> Result<bool, RepositoryError>;
}

/// The Apps a profile has configured (ADR 0036).
///
/// Separate from [`ProviderSettingsRepository`] because the two answer
/// different questions — where models can be reached, and which servers
/// Tramontana starts — and because an App is started and stopped, which no
/// model endpoint is.
#[async_trait]
pub trait AppSettingsRepository: Send + Sync {
    /// Every configured App, in a stable order.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(&self) -> Result<Vec<AppConfig>, RepositoryError>;

    /// Store an App, replacing any with the same identifier.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn save(&self, app: &AppConfig) -> Result<(), RepositoryError>;

    /// Remove an App. Returns whether one was there.
    ///
    /// A secret the App named is left in the operating system's store, as a
    /// model endpoint's is: it is the person's, and other software may use
    /// it (ADR 0008).
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn delete(&self, app_id: &str) -> Result<bool, RepositoryError>;
}

#[async_trait]
pub trait ConfigurationRepository: Send + Sync {
    /// Save or refresh a resolved manifest for a project.
    async fn save(
        &self,
        project_id: &ProjectId,
        configuration: &EffectiveConfiguration,
    ) -> Result<(), RepositoryError>;

    /// Read a specific manifest by hash.
    async fn get(
        &self,
        project_id: &ProjectId,
        manifest_hash: &str,
    ) -> Result<EffectiveConfiguration, RepositoryError>;

    /// Read the most recently resolved manifest, if this project has one.
    async fn latest(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<EffectiveConfiguration>, RepositoryError>;
}

/// Durable task checkpoints.
///
/// A checkpoint is derived from the event stream and stored whole, because it
/// must remain readable after the code that produced it has changed. Storing
/// the fields separately and reassembling them would make every future
/// contract change a migration of history (`PROJECT_FORMAT.md` §4).
#[async_trait]
pub trait CheckpointRepository: Send + Sync {
    /// Store a checkpoint.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Conflict`] when a checkpoint already exists for this
    /// task at this sequence, and [`RepositoryError::Unavailable`] when
    /// storage failed.
    async fn save(&self, checkpoint: &TaskCheckpoint) -> Result<(), RepositoryError>;

    /// Read one checkpoint.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn get(&self, checkpoint_id: &CheckpointId) -> Result<TaskCheckpoint, RepositoryError>;

    /// The checkpoint at the highest sequence for a task, if it has one.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn latest(&self, task_id: &TaskId) -> Result<Option<TaskCheckpoint>, RepositoryError>;

    /// Every checkpoint for a task, oldest first.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(&self, task_id: &TaskId) -> Result<Vec<TaskCheckpoint>, RepositoryError>;
}

/// The person's profile and memory notes, in the local profile (ADR 0061).
#[async_trait]
pub trait PersonalRepository: Send + Sync {
    /// The profile, or the default one when nothing was ever saved.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn load(&self) -> Result<crate::contracts::PersonalProfile, RepositoryError>;

    /// Replace the profile, its notes included, in one write.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn save(
        &self,
        profile: &crate::contracts::PersonalProfile,
    ) -> Result<(), RepositoryError>;
}

/// Schedules (ADR 0063).
#[async_trait]
pub trait ScheduleRepository: Send + Sync {
    /// Every schedule, oldest first.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn list(&self) -> Result<Vec<crate::contracts::Schedule>, RepositoryError>;

    /// Create or replace a schedule.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn save(&self, schedule: &crate::contracts::Schedule) -> Result<(), RepositoryError>;

    /// Remove a schedule. Its conversation is kept.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when it does not exist.
    async fn delete(&self, schedule_id: &str) -> Result<(), RepositoryError>;
}
