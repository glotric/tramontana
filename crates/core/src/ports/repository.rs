use async_trait::async_trait;

use crate::contracts::{
    CheckpointId, ConversationId, ConversationRecord, EffectiveConfiguration, EventEnvelope,
    ProjectId, ProjectRecord, TaskCheckpoint, TaskId, TaskRecord, TaskState, Timestamp,
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
}

/// Secret-free effective-configuration manifests.
///
/// Markdown bodies stay in the project-authored plane. This repository stores
/// only the hashes and metadata needed to explain which configuration a task
/// used after the files change.
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
