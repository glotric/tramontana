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
    EventStore, IdSource, ModelProvider, ProjectRepository, ProjectSourceReader, RepositoryError,
    TaskRepository, ToolAdmission,
};
use crate::services::ProjectInstructions;
use crate::services::journal::{JournalPorts, TaskJournal};
use crate::services::projection::TaskProjectionWriter;
use crate::services::runtime::TaskDriver;
use crate::services::{PermissionManager, ToolRegistry};

/// Where the dictation setup is kept, and what transcribes (ADR 0065).
pub(super) type DictationPorts = (
    Arc<dyn crate::ports::DictationRepository>,
    Arc<dyn crate::ports::Transcriber>,
);

/// Where schedules are kept, and the local clock they run by (ADR 0063).
pub(super) type SchedulePorts = (
    Arc<dyn crate::ports::ScheduleRepository>,
    Arc<dyn crate::ports::LocalClock>,
);

/// How long one message's documents may take to read (ADR 0062).
pub const DOCUMENT_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The longest conversation title kept, in bytes (ADR 0059).
pub const MAX_CONVERSATION_TITLE_BYTES: usize = 200;

/// The longest goal an interface may submit, in bytes.
///
/// The webview and a remote desktop are untrusted input like any other, so
/// both adapters check a goal against this before it reaches a service.
pub const MAX_GOAL_BYTES: usize = 4_096;

/// A goal as an interface submitted it: trimmed, not empty, not too long.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] naming which rule it broke.
pub fn checked_goal(goal: &str) -> Result<String, ErrorEnvelope> {
    let goal = goal.trim();
    let refused = |message: String| {
        ErrorEnvelope::new(
            ErrorCode::InvalidRequest,
            message,
            crate::contracts::CorrelationId::new("ipc"),
        )
    };
    if goal.is_empty() {
        return Err(refused("a task needs a goal".to_owned()));
    }
    if goal.len() > MAX_GOAL_BYTES {
        return Err(refused(format!(
            "a goal may be at most {MAX_GOAL_BYTES} bytes"
        )));
    }
    Ok(goal.to_owned())
}

/// The longest search asked for, in bytes (ADR 0059).
pub const MAX_SEARCH_QUERY_BYTES: usize = 200;

/// At most this many conversations, and this many projects, are returned by
/// one search.
const SEARCH_RESULTS: usize = 50;

/// A matching message, shortened for a result list.
fn excerpt(text: &str) -> String {
    const CHARS: usize = 120;
    if text.chars().count() <= CHARS {
        return text.to_owned();
    }
    let short: String = text.chars().take(CHARS - 1).collect();
    format!("{}…", short.trim_end())
}

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
    /// The worker to run it, by name, from [`TaskService::workers`]
    /// (ADR 0048). `None` is the built-in worker as the routing mode runs
    /// it. A worker pinned to a model off this device is refused to a task
    /// that runs Local Only, before the task is written.
    pub worker: Option<String>,
    /// Which of the project's reusable definitions this task uses: its
    /// skills, and an agent profile or workflow when one is named
    /// (ADR 0053).
    ///
    /// `None` selects none of them, which is what a task that says nothing
    /// has always run under. Selecting a definition contributes its text and
    /// nothing else: no skill grants a tool, a root, or a destination.
    pub selection: Option<ConfigurationSelection>,
    /// Files the person attached to this message (ADR 0058). Checked, and
    /// their text redacted, before anything is written.
    pub attachments: Vec<super::AttachmentInput>,
    /// The person's own skills this task uses, by id, in order (ADR 0070).
    /// Each contributes its text and nothing else, as a project's skill does.
    pub personal_skills: Vec<String>,
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

/// A repository result, as the service reports it.
///
/// `.or_envelope(self)` in place of
/// `.map_err(|error| self.repository_error(&error))` at every storage call.
pub(super) trait OrEnvelope<T> {
    /// Map a failure through [`TaskService::repository_error`].
    fn or_envelope(self, service: &TaskService) -> Result<T, ErrorEnvelope>;
}

impl<T> OrEnvelope<T> for Result<T, RepositoryError> {
    fn or_envelope(self, service: &TaskService) -> Result<T, ErrorEnvelope> {
        self.map_err(|error| service.repository_error(&error))
    }
}

/// Forget a run once its driver has ended, unless the task has been started
/// again since — a resume registers a new run under the same id.
///
/// Without this every task stayed registered, with its update buffer, for the
/// life of the process. A forgotten run reads exactly as a finished one: its
/// events are durable, and a late subscriber replays them.
pub(crate) fn forget_run(
    live: &LiveTasks,
    task_id: &TaskId,
    run: &tokio::sync::watch::Receiver<bool>,
) {
    if let Ok(mut live) = live.lock()
        && live
            .get(task_id)
            .is_some_and(|entry| entry.finished.same_channel(run))
    {
        live.remove(task_id);
    }
}

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

/// What an interrupted task can be handed to, in the order to offer it.
///
/// Built by [`TaskService::handoff_options`], which owns the ordering: an
/// application renders what it is given rather than deciding for itself which
/// model a stopped task should try next (ADR 0055).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffOffer {
    /// The workers, best first. Empty when none can take the task.
    pub workers: Vec<WorkerOption>,
    /// Whether the task stopped because the work was beyond the model it ran
    /// on, rather than because something around the model failed.
    ///
    /// When it was, a model off this device is offered first: the model that
    /// had the task has just shown it could not carry it, and Tramontana
    /// knows nothing else about which local model is stronger (ADR 0051).
    /// Nothing moves without the person, and Local Only still refuses.
    pub escalating: bool,
}

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
    /// Held across every read-change-save of a schedule, so the scheduler's
    /// copy cannot overwrite a delete or a pause made while it ran one.
    /// Taken before `lifecycle`, never after.
    pub(super) schedule_lock: tokio::sync::Mutex<()>,
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
    /// How the project's own Markdown is read at the moment a task starts.
    /// Absent in a service with no project files to read, such as a test
    /// harness; a task then runs on the last inventory the project resolved,
    /// without its text (ADR 0052).
    pub(super) project_sources: Option<Arc<dyn ProjectSourceReader>>,
    /// What reads PDF and Word attachments (ADR 0062). Without one, only
    /// text files can be attached.
    pub(super) documents: Option<Arc<dyn crate::ports::DocumentReader>>,
    /// The person's profile and memory (ADR 0061). Without it, chats are
    /// told nothing about the person.
    pub(super) personal: Option<Arc<dyn crate::ports::PersonalRepository>>,
    /// Where schedules are kept, and the local clock they run by
    /// (ADR 0063). Without them, nothing is scheduled.
    pub(super) schedules: Option<SchedulePorts>,
    /// Where the dictation setup is kept, and what transcribes (ADR 0065).
    pub(super) dictation: Option<DictationPorts>,
    /// The person's own skills (ADR 0070). Without a library, tasks use
    /// only their project's skills.
    pub(super) skill_library: Option<Arc<dyn crate::ports::SkillLibrary>>,
    /// Removes a conversation's or a project's record in one commit
    /// (ADR 0067). Without it, removal is the ordered sequence of deletes.
    pub(super) record_removal: Option<Arc<dyn crate::ports::RecordRemoval>>,
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
            schedule_lock: tokio::sync::Mutex::new(()),
            // Local-first by default: work stays on the device until someone
            // chooses otherwise.
            routing_mode: Mutex::new(RoutingMode::Automatic),
            workers: None,
            pinned: None,
            project_sources: None,
            documents: None,
            personal: None,
            schedules: None,
            dictation: None,
            skill_library: None,
            record_removal: None,
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

    /// Read the project's own instruction files with `reader` when a task
    /// starts, so its worker is given them (ADR 0052).
    #[must_use]
    pub fn with_project_sources(mut self, reader: Arc<dyn ProjectSourceReader>) -> Self {
        self.project_sources = Some(reader);
        self
    }

    /// Keep the dictation setup in `store`, and transcribe with
    /// `transcriber` (ADR 0065).
    #[must_use]
    pub fn with_dictation(
        mut self,
        store: Arc<dyn crate::ports::DictationRepository>,
        transcriber: Arc<dyn crate::ports::Transcriber>,
    ) -> Self {
        self.dictation = Some((store, transcriber));
        self
    }

    /// Remove conversations and projects through `removal`, in one commit
    /// each, and sweep events a past removal left behind (ADR 0067).
    #[must_use]
    pub fn with_record_removal(mut self, removal: Arc<dyn crate::ports::RecordRemoval>) -> Self {
        self.record_removal = Some(removal);
        self
    }

    /// Keep schedules in `store`, and run them by `clock` (ADR 0063).
    #[must_use]
    pub fn with_schedules(
        mut self,
        store: Arc<dyn crate::ports::ScheduleRepository>,
        clock: Arc<dyn crate::ports::LocalClock>,
    ) -> Self {
        self.schedules = Some((store, clock));
        self
    }

    /// Keep the person's profile and memory in `store`, and tell chats
    /// about them (ADR 0061).
    #[must_use]
    pub fn with_personal(mut self, store: Arc<dyn crate::ports::PersonalRepository>) -> Self {
        self.personal = Some(store);
        self
    }

    fn personal_store(&self) -> Result<&Arc<dyn crate::ports::PersonalRepository>, ErrorEnvelope> {
        self.personal.as_ref().ok_or_else(|| {
            self.error(
                ErrorCode::CapabilityUnsupported,
                "This application keeps no personal profile",
            )
        })
    }

    /// The person's profile and memory notes (ADR 0061).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::CapabilityUnsupported`] when the service keeps no
    /// profile, or a persistence error.
    pub async fn personal_profile(
        &self,
    ) -> Result<crate::contracts::PersonalProfile, ErrorEnvelope> {
        self.personal_store()?.load().await.or_envelope(self)
    }

    /// Change what the person told Tramontana about themselves.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for a field that cannot be kept, or as
    /// [`Self::personal_profile`].
    pub async fn update_personal(
        &self,
        details: &crate::contracts::PersonalDetails,
    ) -> Result<crate::contracts::PersonalProfile, ErrorEnvelope> {
        let profile = super::apply_details(self.personal_profile().await?, details)?;
        self.personal_store()?
            .save(&profile)
            .await
            .or_envelope(self)?;
        Ok(profile)
    }

    /// Add a note for chats to remember. Only the person adds one; nothing
    /// is remembered on its own (ADR 0061).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for an empty or long note, or when the
    /// profile holds as many as it may, or as [`Self::personal_profile`].
    pub async fn remember(
        &self,
        text: &str,
    ) -> Result<crate::contracts::PersonalProfile, ErrorEnvelope> {
        let mut profile = self.personal_profile().await?;
        let note = super::new_note(&profile, self.ids.next("note"), text, self.clock.now())?;
        profile.notes.push(note);
        self.personal_store()?
            .save(&profile)
            .await
            .or_envelope(self)?;
        Ok(profile)
    }

    /// Forget one note.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when no note has that id, or as
    /// [`Self::personal_profile`].
    pub async fn forget(
        &self,
        note_id: &str,
    ) -> Result<crate::contracts::PersonalProfile, ErrorEnvelope> {
        let mut profile = self.personal_profile().await?;
        let before = profile.notes.len();
        profile.notes.retain(|note| note.note_id != note_id);
        if profile.notes.len() == before {
            return Err(self.error(ErrorCode::NotFound, "No note has that id"));
        }
        self.personal_store()?
            .save(&profile)
            .await
            .or_envelope(self)?;
        Ok(profile)
    }

    /// What a chat starting now is told about the person, if anything.
    /// A profile that cannot be read tells nothing: less context is safe.
    async fn personal_for(&self, project_id: Option<&ProjectId>) -> Option<String> {
        if project_id.is_some() {
            return None;
        }
        let profile = self.personal.as_ref()?.load().await.ok()?;
        super::personal_context(&profile)
    }

    /// Read PDF and Word attachments with `reader` (ADR 0062).
    #[must_use]
    pub fn with_document_reader(mut self, reader: Arc<dyn crate::ports::DocumentReader>) -> Self {
        self.documents = Some(reader);
        self
    }

    /// Check a message's attachments and read their text, as a task started
    /// with them would (ADRs 0058 and 0062). The desktop asks this as soon as
    /// a file is picked, so a refusal is shown then.
    ///
    /// A document is read off the async runtime, and given
    /// [`DOCUMENT_READ_DEADLINE`] to finish.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] naming a file that cannot be attached,
    /// or [`ErrorCode::Timeout`] when a document takes too long to read.
    pub async fn accept_attachments(
        &self,
        inputs: Vec<super::AttachmentInput>,
    ) -> Result<Vec<crate::contracts::TaskAttachment>, ErrorEnvelope> {
        let documents = self.documents.clone();
        let reads_documents = documents.as_ref().is_some_and(|reader| {
            inputs
                .iter()
                .any(|input| reader.media_type(&input.name).is_some())
        });
        if !reads_documents {
            return super::accept_attachments(&inputs, None);
        }
        let reading = tokio::task::spawn_blocking(move || {
            super::accept_attachments(&inputs, documents.as_deref())
        });
        match tokio::time::timeout(DOCUMENT_READ_DEADLINE, reading).await {
            Ok(Ok(accepted)) => accepted,
            Ok(Err(_)) => Err(self.error(
                ErrorCode::InvalidRequest,
                "An attached document could not be read",
            )),
            Err(_) => Err(self.error(
                ErrorCode::Timeout,
                "An attached document took too long to read",
            )),
        }
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
            .or_envelope(self)?;
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
            .or_envelope(self)
    }

    /// Every project, most recently created first.
    ///
    /// # Errors
    ///
    /// A persistence error when storage is unavailable.
    pub async fn list_projects(&self) -> Result<Vec<ProjectRecord>, ErrorEnvelope> {
        self.repositories.projects.list().await.or_envelope(self)
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
            .or_envelope(self)?;
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
            .or_envelope(self)
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
            .or_envelope(self)?;
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
            .or_envelope(self)
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
            .or_envelope(self)
    }

    /// Give a conversation a new title (ADR 0059).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for an empty title or one over
    /// [`MAX_CONVERSATION_TITLE_BYTES`], or [`ErrorCode::NotFound`].
    pub async fn rename_conversation(
        &self,
        conversation_id: &ConversationId,
        title: &str,
    ) -> Result<ConversationRecord, ErrorEnvelope> {
        let title = title.trim();
        if title.is_empty() || title.len() > MAX_CONVERSATION_TITLE_BYTES {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                format!("A conversation's title must be 1 to {MAX_CONVERSATION_TITLE_BYTES} bytes"),
            ));
        }
        self.repositories
            .conversations
            .rename(conversation_id, title)
            .await
            .or_envelope(self)?;
        self.get_conversation(conversation_id).await
    }

    /// Delete a conversation and everything recorded under it: its tasks,
    /// their events and checkpoints (ADR 0059).
    ///
    /// Refused while any of its tasks is still under way, so a person stops
    /// work before throwing its record away. A task that was interrupted is
    /// not under way, and goes with the rest.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist, [`ErrorCode::Conflict`]
    /// while one of its tasks runs, or a persistence error.
    pub async fn delete_conversation(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<(), ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        self.get_conversation(conversation_id).await?;
        let tasks = self.list_tasks(Some(conversation_id)).await?;
        self.none_under_way(
            &tasks,
            "Stop the conversation's running task before deleting it",
        )
        .await?;
        if let Some(removal) = &self.record_removal {
            return removal
                .remove_conversation(conversation_id)
                .await
                .or_envelope(self);
        }
        self.remove_tasks(&tasks).await?;
        self.repositories
            .conversations
            .delete(conversation_id)
            .await
            .or_envelope(self)
    }

    /// Remove Tramontana's record of a project: its conversations, their
    /// tasks, events and checkpoints, its schedules and its configuration
    /// record (ADR 0064). The folder and every file in it are untouched:
    /// nothing here reads or writes the project's roots.
    ///
    /// Refused while any of its tasks is under way.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist, [`ErrorCode::Conflict`]
    /// while one of its tasks runs, or a persistence error.
    pub async fn delete_project(&self, project_id: &ProjectId) -> Result<(), ErrorEnvelope> {
        let _schedules = self.schedule_lock.lock().await;
        let _lifecycle = self.lifecycle.lock().await;
        self.get_project(project_id).await?;
        let conversations = self.list_conversations(Some(project_id)).await?;
        let mut tasks = Vec::new();
        for conversation in &conversations {
            tasks.extend(self.list_tasks(Some(&conversation.conversation_id)).await?);
        }
        // A task started without a conversation can still belong to it.
        tasks.extend(self.list_tasks(None).await?.into_iter().filter(|task| {
            task.conversation_id.is_none() && task.project_id.as_ref() == Some(project_id)
        }));
        self.none_under_way(&tasks, "Stop the project's running task before removing it")
            .await?;
        if let Some(removal) = &self.record_removal {
            return removal.remove_project(project_id).await.or_envelope(self);
        }
        if let Some((schedules, _)) = &self.schedules {
            for schedule in schedules.list().await.or_envelope(self)? {
                if schedule.project_id.as_ref() == Some(project_id) {
                    schedules
                        .delete(&schedule.schedule_id)
                        .await
                        .or_envelope(self)?;
                }
            }
        }
        self.remove_tasks(&tasks).await?;
        for conversation in &conversations {
            self.repositories
                .conversations
                .delete(&conversation.conversation_id)
                .await
                .or_envelope(self)?;
        }
        self.repositories
            .projects
            .delete(project_id)
            .await
            .or_envelope(self)
    }

    /// Refuse with `refusal` while any of `tasks` is under way, waiting a
    /// moment for one whose record has ended but whose driver is finishing.
    pub(super) async fn none_under_way(
        &self,
        tasks: &[TaskRecord],
        refusal: &str,
    ) -> Result<(), ErrorEnvelope> {
        if tasks.iter().any(|task| {
            !(task.state.is_terminal() || task.state == crate::contracts::TaskState::Interrupted)
        }) {
            return Err(self.error(ErrorCode::Conflict, refusal));
        }
        let finishing: Vec<_> = {
            let live = self.lock_live()?;
            tasks
                .iter()
                .filter_map(|task| live.get(&task.task_id).map(|live| live.finished.clone()))
                .collect()
        };
        for mut finished in finishing {
            let done = matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    finished.wait_for(|done| *done),
                )
                .await,
                Ok(Ok(_))
            );
            if !done && !*finished.borrow() {
                return Err(self.error(ErrorCode::Conflict, refusal));
            }
        }
        Ok(())
    }

    /// Remove tasks and their events, each row before its events: events
    /// without a row are unreachable, a row without its events is invalid.
    async fn remove_tasks(&self, tasks: &[TaskRecord]) -> Result<(), ErrorEnvelope> {
        for task in tasks {
            self.repositories
                .tasks
                .delete(&task.task_id)
                .await
                .or_envelope(self)?;
            self.repositories
                .events
                .delete_stream(&task.task_id)
                .await
                .map_err(|error| self.error(ErrorCode::PersistenceError, error.to_string()))?;
        }
        Ok(())
    }

    /// A readable transcript of one conversation, to share as a file
    /// (ADR 0060): its tasks oldest first, each with what was asked, the
    /// names of its attached files, and its final answer.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when it does not exist, or a persistence error.
    pub async fn transcript(
        &self,
        conversation_id: &ConversationId,
        options: crate::contracts::TranscriptOptions,
    ) -> Result<String, ErrorEnvelope> {
        let conversation = self.get_conversation(conversation_id).await?;
        let project_name = match &conversation.project_id {
            Some(id) => Some(self.get_project(id).await?.name),
            None => None,
        };
        let mut tasks = self.list_tasks(Some(conversation_id)).await?;
        tasks.sort_by(|a, b| {
            (a.created_at.as_str(), a.task_id.as_str())
                .cmp(&(b.created_at.as_str(), b.task_id.as_str()))
        });
        let mut turns = Vec::with_capacity(tasks.len());
        for task in tasks {
            let events = self.replay_task_events(&task.task_id, 0).await?;
            turns.push((task, events));
        }
        Ok(super::render_transcript(
            &conversation,
            project_name.as_deref(),
            &turns,
            &options,
        ))
    }

    /// Search conversation titles, what was asked in them, and project names
    /// and folders (`PHASE_1_SPEC.md` §4.1, ADR 0059).
    ///
    /// Plain text, ignoring case: no index and no semantic search. Answers
    /// are not searched, because they live only in each task's events.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for an empty query or one over
    /// [`MAX_SEARCH_QUERY_BYTES`], or a persistence error.
    pub async fn search(
        &self,
        query: &str,
    ) -> Result<crate::contracts::SearchResults, ErrorEnvelope> {
        use crate::contracts::{ConversationHit, SearchField, SearchResults};
        let query = query.trim();
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                format!("A search must be 1 to {MAX_SEARCH_QUERY_BYTES} bytes"),
            ));
        }
        let needle = query.to_lowercase();
        let found = |text: &str| text.to_lowercase().contains(&needle);

        let projects = self.list_projects().await?;
        let mut conversations = self.list_conversations(None).await?;
        for project in &projects {
            conversations.extend(self.list_conversations(Some(&project.project_id)).await?);
        }
        let tasks = self.list_tasks(None).await?;

        let mut hits = Vec::new();
        for conversation in conversations {
            let project = conversation
                .project_id
                .as_ref()
                .and_then(|id| projects.iter().find(|project| &project.project_id == id));
            let asked = || {
                tasks
                    .iter()
                    .filter(|task| {
                        task.conversation_id.as_ref() == Some(&conversation.conversation_id)
                    })
                    .find(|task| found(&task.goal_summary))
            };
            let (matched_in, excerpt) = if found(&conversation.title) {
                (SearchField::Title, None)
            } else if let Some(task) = asked() {
                (SearchField::Goal, Some(excerpt(&task.goal_summary)))
            } else if project.is_some_and(|project| {
                found(&project.name) || project.roots.iter().any(|root| found(root))
            }) {
                (SearchField::Project, None)
            } else {
                continue;
            };
            hits.push(ConversationHit {
                project_name: project.map(|project| project.name.clone()),
                conversation,
                matched_in,
                excerpt,
            });
        }
        hits.sort_by(|a, b| {
            (
                b.conversation.created_at.as_str(),
                b.conversation.conversation_id.as_str(),
            )
                .cmp(&(
                    a.conversation.created_at.as_str(),
                    a.conversation.conversation_id.as_str(),
                ))
        });
        hits.truncate(SEARCH_RESULTS);
        let mut matching: Vec<_> = projects
            .into_iter()
            .filter(|project| found(&project.name) || project.roots.iter().any(|root| found(root)))
            .collect();
        matching.truncate(SEARCH_RESULTS);
        Ok(SearchResults {
            schema_version: crate::contracts::SCHEMA_VERSION,
            conversations: hits,
            projects: matching,
        })
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
            .or_envelope(self)
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
        self.repositories.tasks.get(task_id).await.or_envelope(self)
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
    pub async fn start_task(&self, mut request: StartTaskRequest) -> Result<TaskId, ErrorEnvelope> {
        self.require_execution()?;
        // Before anything is written, so a refused file leaves no task — and
        // before the lifecycle lock, because reading a document can take
        // seconds and touches no task state: a person's Cancel must not wait
        // behind it.
        let attachments = self
            .accept_attachments(std::mem::take(&mut request.attachments))
            .await?;
        let _lifecycle = self.lifecycle.lock().await;
        // Chosen before anything is written, so a refusal leaves no task
        // behind.
        let routing_mode = request.routing_mode.unwrap_or_else(|| self.routing_mode());
        let worker = match (request.admission, request.worker.as_deref(), &self.workers) {
            (None, None, _) => Arc::clone(&self.providers.worker),
            (Some(admission), None, Some(choose)) => choose(admission),
            (admission, Some(name), _) => {
                // A named worker: the built-in one, or it pinned to a model
                // the person chose (ADR 0048).
                let admission = admission.unwrap_or(ToolAdmission::READ_ONLY);
                if routing_mode == RoutingMode::LocalOnly && self.is_cloud_worker(name) {
                    return Err(self.error(
                        ErrorCode::Conflict,
                        "This task runs Local Only, so it cannot run on a model off this device",
                    ));
                }
                self.build_worker(name, admission)
                    .ok_or_else(|| self.error(ErrorCode::NotFound, "No such worker is offered"))?
            }
            (Some(_), None, None) => {
                return Err(self.error(
                    ErrorCode::CapabilityUnsupported,
                    "This application runs every task with one worker, and cannot choose one for this task",
                ));
            }
        };
        let project_id = match &request.conversation_id {
            Some(id) => {
                let conversation = self
                    .repositories
                    .conversations
                    .get(id)
                    .await
                    .or_envelope(self)?;
                conversation.project_id
            }
            None => None,
        };

        let task_id = TaskId::new(self.ids.next("task"));
        let correlation_id = CorrelationId::new(self.ids.next("correlation"));
        let cancel = CancellationToken::new();
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);

        // Read now, not from the last inventory the project resolved: a task's
        // record must describe the instructions it actually ran under, and the
        // worker is about to be given their text (ADR 0052). What the task
        // selects is its own, not whatever the project last resolved with: a
        // task must not inherit a skill because someone inspected the
        // configuration with it earlier (ADR 0053).
        let selection_default = ConfigurationSelection::default();
        let (configuration, instructions) = match (&project_id, &self.project_sources) {
            (Some(project_id), Some(reader)) => {
                let (configuration, instructions) = crate::config::resolve_with_bodies(
                    reader.read(project_id).await?,
                    request.selection.as_ref().unwrap_or(&selection_default),
                    self.clock.now(),
                );
                // Saved only when it is new. A resume reads the manifest back
                // by hash, so an unsaved one would lose the project's recipes;
                // rewriting an identical one would move its `resolvedAt` every
                // time a task started, and the configuration view would report
                // a change that never happened.
                let stored = self
                    .repositories
                    .configuration
                    .latest(project_id)
                    .await
                    .or_envelope(self)?;
                if stored.as_ref().map(|stored| &stored.manifest_hash)
                    != Some(&configuration.manifest_hash)
                {
                    self.repositories
                        .configuration
                        .save(project_id, &configuration)
                        .await
                        .or_envelope(self)?;
                }
                (Some(configuration), instructions)
            }
            (Some(project_id), None) => (
                self.repositories
                    .configuration
                    .latest(project_id)
                    .await
                    .or_envelope(self)?,
                ProjectInstructions::default(),
            ),
            (None, _) => (None, ProjectInstructions::default()),
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
                    .or_envelope(self)?
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
        // Chosen now and recorded, so a resumed task is told the same
        // (ADR 0061). Only a chat is told anything.
        let personal = self.personal_for(project_id.as_ref()).await;
        // Read now and recorded whole, so a resume is given the same text
        // even if the skill was changed or removed since (ADR 0070).
        let skills = self.skills_for(&request.personal_skills).await?;

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
                    attachments: attachments.clone(),
                    personal: personal.clone(),
                    skills: skills.clone(),
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
            .or_envelope(self)?;

        let interrupt = CancellationToken::new();
        let (finished, finish_receiver) = tokio::sync::watch::channel(false);
        let run = finish_receiver.clone();
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

        let earlier = self.earlier_turns(&record).await;
        let driver = TaskDriver::new(
            journal,
            self.providers.clone(),
            scope,
            Arc::clone(&self.clock),
        )
        .with_routing_mode(routing_mode)
        .with_worker(worker)
        .with_configuration(resolved_configuration)
        .with_instructions(instructions)
        .with_earlier_turns(earlier)
        .with_attachments(attachments)
        .with_personal(personal)
        .with_skills(skills)
        .with_goal_redactions(goal.reason_codes.clone())
        .with_checkpoints(Arc::clone(&self.repositories.checkpoints))
        .continuing(None, super::ResumePlan::default(), interrupt);
        // The redacted text, not `request.goal`. The worker, the manifest, and
        // the model payload all see the same thing the record does.
        let goal = goal.text;
        let execution_guard = self.execution_guard.clone();
        let live = Arc::clone(&self.live);
        let spawned_id = task_id.clone();
        tokio::spawn(async move {
            let _execution_guard = execution_guard;
            driver.run(goal, cancel).await;
            let _ = finished.send(true);
            forget_run(&live, &spawned_id, &run);
        });

        Ok(task_id)
    }

    /// The earlier turns of `task`'s conversation (ADR 0057).
    ///
    /// Only the newest completed tasks created before it are read, so the
    /// same task chooses the same turns on resume. A task whose record cannot
    /// be read is left out: less context is safe, and a turn made up from a
    /// partial record would not be.
    pub(crate) async fn earlier_turns(&self, task: &TaskRecord) -> super::EarlierTurns {
        let Some(conversation_id) = &task.conversation_id else {
            return super::EarlierTurns::default();
        };
        let Ok(mut tasks) = self.list_tasks(Some(conversation_id)).await else {
            return super::EarlierTurns::default();
        };
        tasks.retain(|record| {
            record.state == crate::contracts::TaskState::Completed
                && (record.created_at.as_str(), record.task_id.as_str())
                    < (task.created_at.as_str(), task.task_id.as_str())
        });
        tasks.sort_by(|a, b| {
            (b.created_at.as_str(), b.task_id.as_str())
                .cmp(&(a.created_at.as_str(), a.task_id.as_str()))
        });
        tasks.truncate(super::EARLIER_TURNS);
        let mut earlier = Vec::with_capacity(tasks.len());
        for record in tasks {
            if let Ok(events) = self.replay_task_events(&record.task_id, 0).await {
                earlier.push((record, events));
            }
        }
        super::EarlierTurns::choose(task, &earlier)
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
        use crate::contracts::TaskState;
        self.require_execution()?;
        // A running task is signalled at once, without the lifecycle lock: a
        // stop the person asked for must not wait behind another task's start.
        if self.get_task_record(task_id).await?.state != TaskState::Interrupted {
            return self.signal_cancel(task_id);
        }
        // Discarding interrupted work changes the record, and must not race
        // a resume of the same task. Read again under the lock: a resume may
        // have started it in between, and then it is cancelled instead.
        let _lifecycle = self.lifecycle.lock().await;
        if self.get_task_record(task_id).await?.state == TaskState::Interrupted {
            return self.discard_interrupted(task_id).await;
        }
        self.signal_cancel(task_id)
    }

    /// Cancel the run of `task_id` in this process, if there is one.
    fn signal_cancel(&self, task_id: &TaskId) -> Result<(), ErrorEnvelope> {
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
            .or_envelope(self)?;

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
            .or_envelope(self)?;

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
