//! The versioned command boundary.
//!
//! Each command validates its payload, calls an application service, and maps
//! the outcome to a normalized [`ErrorEnvelope`] (`docs/DEVELOPMENT.md` §9).
//! Filesystem paths enter only through the native directory picker, are
//! canonicalized by the platform adapter, and become explicit project roots.
//! No command accepts a shell string, and none makes a decision the core has
//! not already made.

use std::path::PathBuf;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ApprovalId, ApprovalRequest, CheckpointId, ConversationId,
    ConversationRecord, EffectiveConfiguration, ErrorCode, ErrorEnvelope, EventEnvelope,
    ExportManifest, ExportOptions, ProjectGrant, ProjectId, ProjectRecord, RoutingMode,
    SCHEMA_VERSION, TaskCheckpoint, TaskId, TaskRecord, TaskSnapshot, TaskUpdate,
};
use tramontana_core::services::StartTaskRequest;
use tramontana_platform::{CanonicalRoot, canonicalize_root, discover};

use tramontana_core::ports::{ApprovalStore, IdSource, ToolAdmission, ToolOutcome};
use tramontana_platform::UuidIdSource;

use tramontana_core::ports::Clock;
use tramontana_platform::SystemClock;

use crate::bundles::{self, ExportScope};
use crate::ipc_error;
use crate::settings;
use crate::state::AppState;

/// Longest goal this boundary accepts; shared with a host (SP1).
use tramontana_core::services::MAX_GOAL_BYTES;

/// Longest conversation title accepted from the webview.
const MAX_TITLE_BYTES: usize = 512;

/// Version information for the startup handshake.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoreInfo {
    /// Contract schema version reported by the running core.
    pub schema_version: u32,
    /// Version of the desktop shell.
    pub app_version: &'static str,
}

/// Report the contract schema and application version.
#[tauri::command]
pub(crate) fn core_info() -> CoreInfo {
    CoreInfo {
        schema_version: tramontana_core::SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION"),
    }
}

/// Project plus the effective configuration most recently resolved for it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectInspection {
    /// Version of this response envelope.
    pub schema_version: u32,
    /// Durable project record.
    pub project: ProjectRecord,
    /// Latest secret-free configuration manifest, when one exists.
    pub configuration: Option<EffectiveConfiguration>,
}

/// Show the native project-folder picker, then open and resolve that project.
///
/// Cancelling the picker returns `None` and changes no state. The webview never
/// supplies a filesystem path; the native adapter receives the user selection,
/// canonicalizes it, and passes only an approved root into core.
#[tauri::command]
pub(crate) async fn open_project(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ProjectInspection>, ErrorEnvelope> {
    let Some(root) = pick_project_root(&app).await? else {
        return Ok(None);
    };
    open_root(&state, &root).await.map(Some)
}

/// Open one canonical root as a durable project and resolve its configuration.
///
/// Separate from [`open_project`] because the picker is the only part of that
/// command a test cannot drive: a root reaches the core from the native dialog
/// and nowhere else, so what happens to it afterwards is what is left to
/// check.
pub(crate) async fn open_root(
    state: &AppState,
    root: &CanonicalRoot,
) -> Result<ProjectInspection, ErrorEnvelope> {
    let name = root
        .as_path()
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Untitled project")
        .to_owned();
    let project = state
        .service
        .open_project(name, vec![root.as_str().to_owned()])
        .await?;
    let configuration = state
        .service
        .resolve_configuration(
            &project.project_id,
            discover(root),
            &ConfigurationSelection::default(),
        )
        .await?;

    Ok(ProjectInspection {
        schema_version: SCHEMA_VERSION,
        project,
        configuration: Some(configuration),
    })
}

/// List recent projects from the durable profile.
#[tauri::command]
pub(crate) async fn list_projects(
    state: State<'_, AppState>,
) -> Result<Vec<ProjectRecord>, ErrorEnvelope> {
    state.service.list_projects().await
}

/// Inspect a durable project and its latest resolved configuration.
#[tauri::command]
pub(crate) async fn inspect_project(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<ProjectInspection, ErrorEnvelope> {
    let project = state.service.get_project(&project_id).await?;
    let configuration = state
        .service
        .get_effective_configuration(&project_id)
        .await?;
    Ok(ProjectInspection {
        schema_version: SCHEMA_VERSION,
        project,
        configuration,
    })
}

/// Create a durable conversation in a project, or a chat that belongs to
/// none (ADR 0057).
#[tauri::command]
pub(crate) async fn create_conversation(
    project_id: Option<ProjectId>,
    title: String,
    state: State<'_, AppState>,
) -> Result<ConversationRecord, ErrorEnvelope> {
    let title = title.trim().to_owned();
    if title.is_empty() {
        return Err(invalid("a conversation needs a title"));
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(invalid(format!(
            "a conversation title may be at most {MAX_TITLE_BYTES} bytes"
        )));
    }
    state.service.create_conversation(project_id, title).await
}

/// List durable conversations for one project, or the chats that belong to
/// none when no project is named (ADR 0057).
#[tauri::command]
pub(crate) async fn list_conversations(
    project_id: Option<ProjectId>,
    state: State<'_, AppState>,
) -> Result<Vec<ConversationRecord>, ErrorEnvelope> {
    state.service.list_conversations(project_id.as_ref()).await
}

/// Give a conversation a new title (ADR 0059).
///
/// # Errors
///
/// Whatever the service reports: an empty or oversized title, or an unknown
/// conversation.
#[tauri::command]
pub(crate) async fn rename_conversation(
    conversation_id: ConversationId,
    title: String,
    state: State<'_, AppState>,
) -> Result<ConversationRecord, ErrorEnvelope> {
    state
        .service
        .rename_conversation(&conversation_id, &title)
        .await
}

/// Delete a conversation and everything recorded under it (ADR 0059). The
/// window asks the person first; this does not.
///
/// # Errors
///
/// Whatever the service reports, including a conflict while one of its tasks
/// runs.
#[tauri::command]
pub(crate) async fn delete_conversation(
    conversation_id: ConversationId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.delete_conversation(&conversation_id).await
}

/// The transcript a share would save, for the person to read first
/// (ADR 0060).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn preview_transcript(
    conversation_id: ConversationId,
    options: tramontana_core::contracts::TranscriptOptions,
    state: State<'_, AppState>,
) -> Result<String, ErrorEnvelope> {
    state.service.transcript(&conversation_id, options).await
}

/// Save a conversation's transcript where the person chooses (ADR 0060).
///
/// The text is rendered again here from the record, never taken from the
/// window. Returns where it was written, or `None` when the person cancels.
///
/// # Errors
///
/// Whatever the service reports, or [`ErrorCode::Internal`] when the file
/// cannot be written.
#[tauri::command]
pub(crate) async fn save_transcript(
    conversation_id: ConversationId,
    options: tramontana_core::contracts::TranscriptOptions,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, ErrorEnvelope> {
    let conversation = state.service.get_conversation(&conversation_id).await?;
    let text = state.service.transcript(&conversation_id, options).await?;
    let extension = match options.format {
        tramontana_core::contracts::TranscriptFormat::Markdown => "md",
        tramontana_core::contracts::TranscriptFormat::Html => "html",
    };
    let stem: String = conversation
        .title
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == ' ' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(80)
        .collect();
    let stem = if stem.is_empty() {
        "Conversation".to_owned()
    } else {
        stem
    };
    let Some(selection) = dialog_answer(|answer| {
        app.dialog()
            .file()
            .set_title("Save the conversation")
            .set_file_name(format!("{stem}.{extension}"))
            .add_filter(
                if extension == "md" {
                    "Markdown"
                } else {
                    "Web page"
                },
                &[extension],
            )
            .save_file(answer);
    })
    .await?
    else {
        return Ok(None);
    };
    let path = selection
        .into_path()
        .map_err(|_| invalid("the chosen place is not a local file"))?;
    std::fs::write(&path, text)
        .map_err(|_| internal("the conversation could not be saved there"))?;
    Ok(Some(path.display().to_string()))
}

/// Remove a project from Tramontana, keeping its files (ADR 0064). The
/// window asks the person first; this does not.
///
/// # Errors
///
/// Whatever the service reports, including a conflict while one of its
/// tasks runs.
#[tauri::command]
pub(crate) async fn delete_project(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.delete_project(&project_id).await
}

/// Whether dictation is set up (ADR 0065).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn dictation_status(
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::DictationStatus, ErrorEnvelope> {
    state.service.dictation_status().await
}

/// Choose the `whisper-cli` program and then the model, in the native file
/// picker (ADR 0065). `None` when the person cancels either.
///
/// # Errors
///
/// Whatever the service reports, such as a file that cannot be read.
#[tauri::command]
pub(crate) async fn choose_dictation(
    language: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<tramontana_core::contracts::DictationStatus>, ErrorEnvelope> {
    let Some(binary) = pick_file(&app, "Choose the whisper.cpp program (whisper-cli)").await?
    else {
        return Ok(None);
    };
    let Some(model) = pick_file(&app, "Choose the whisper model (ggml-*.bin)").await? else {
        return Ok(None);
    };
    state
        .service
        .set_dictation(
            &binary.display().to_string(),
            &model.display().to_string(),
            language.as_deref(),
        )
        .await
        .map(Some)
}

/// What the model guide can see on this computer: `llama-server` where
/// installers put it, and model files in the usual download folders. Runs
/// and downloads nothing.
#[tauri::command]
pub(crate) fn model_setup_hints() -> Option<crate::model_hints::ModelSetupHints> {
    crate::model_hints::home().map(|home| crate::model_hints::find(&home))
}

/// The person's own skills (ADR 0070).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn list_personal_skills(
    state: State<'_, AppState>,
) -> Result<Vec<tramontana_core::contracts::PersonalSkill>, ErrorEnvelope> {
    state.service.personal_skills().await
}

/// Where the person's skills are kept, to show them.
#[tauri::command]
pub(crate) fn skills_folder(state: State<'_, AppState>) -> String {
    state.skills_folder.display().to_string()
}

/// Import a skill picked in the native dialog: a skill folder when `folder`,
/// otherwise a `.zip` or `.md` file. Only its `SKILL.md` is read; `None`
/// when the person cancels (ADR 0070).
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when it holds no usable `SKILL.md`, or
/// whatever the service reports.
#[tauri::command]
pub(crate) async fn import_skill(
    folder: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<tramontana_core::contracts::PersonalSkill>, ErrorEnvelope> {
    let picked = if folder {
        pick_folder(&app, "Choose a skill folder (with a SKILL.md)").await?
    } else {
        pick_file(&app, "Choose a skill: a .zip, or a .md file").await?
    };
    let Some(path) = picked else {
        return Ok(None);
    };
    let (name, markdown) = tramontana_platform::read_skill_source(&path)
        .map_err(|error| invalid(format!("Could not import the skill: {error}")))?;
    state.service.import_skill(&markdown, &name).await.map(Some)
}

/// Delete one of the person's skills (ADR 0070).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn remove_skill(
    id: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.remove_skill(&id).await
}

/// Forget the dictation setup; the files are not touched (ADR 0065).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn clear_dictation(
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::DictationStatus, ErrorEnvelope> {
    state.service.clear_dictation().await
}

/// Transcribe a recording the window sends as raw WAV bytes in the request
/// body (ADR 0065). Nothing is kept.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when the body is not raw bytes, or whatever
/// the service reports.
#[tauri::command]
pub(crate) async fn transcribe(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<String, ErrorEnvelope> {
    let tauri::ipc::InvokeBody::Raw(audio) = request.body() else {
        return Err(invalid("a recording is sent as raw bytes"));
    };
    state.service.transcribe(audio.clone()).await
}

/// Every schedule, with when it runs next (ADR 0063).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn list_schedules(
    state: State<'_, AppState>,
) -> Result<Vec<tramontana_core::contracts::ScheduleView>, ErrorEnvelope> {
    state.service.list_schedules().await
}

/// Create a schedule and its conversation (ADR 0063).
///
/// # Errors
///
/// Whatever the service reports, such as an empty goal.
#[tauri::command]
pub(crate) async fn create_schedule(
    schedule: tramontana_core::contracts::NewSchedule,
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::ScheduleView, ErrorEnvelope> {
    state.service.create_schedule(&schedule).await
}

/// Turn a schedule on or off (ADR 0063).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn set_schedule_enabled(
    schedule_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::Schedule, ErrorEnvelope> {
    state
        .service
        .set_schedule_enabled(&schedule_id, enabled)
        .await
}

/// Delete a schedule, keeping its conversation (ADR 0063).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn delete_schedule(
    schedule_id: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.delete_schedule(&schedule_id).await
}

/// Run a schedule now (ADR 0063).
///
/// # Errors
///
/// Whatever the service reports, such as its previous run still going.
#[tauri::command]
pub(crate) async fn run_schedule_now(
    schedule_id: String,
    state: State<'_, AppState>,
) -> Result<TaskId, ErrorEnvelope> {
    state.service.run_schedule_now(&schedule_id).await
}

/// The person's profile and memory notes (ADR 0061).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn personal_profile(
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::PersonalProfile, ErrorEnvelope> {
    state.service.personal_profile().await
}

/// Change what chats are told about the person (ADR 0061).
///
/// # Errors
///
/// Whatever the service reports, such as a field that is too long.
#[tauri::command]
pub(crate) async fn update_personal(
    details: tramontana_core::contracts::PersonalDetails,
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::PersonalProfile, ErrorEnvelope> {
    state.service.update_personal(&details).await
}

/// Add a note for chats to remember (ADR 0061).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn remember(
    text: String,
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::PersonalProfile, ErrorEnvelope> {
    state.service.remember(&text).await
}

/// Forget one note (ADR 0061).
///
/// # Errors
///
/// Whatever the service reports.
#[tauri::command]
pub(crate) async fn forget(
    note_id: String,
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::PersonalProfile, ErrorEnvelope> {
    state.service.forget(&note_id).await
}

/// Search conversation titles, what was asked in them, and project names
/// and folders (ADR 0059).
///
/// # Errors
///
/// Whatever the service reports, such as an empty query.
#[tauri::command]
pub(crate) async fn search(
    query: String,
    state: State<'_, AppState>,
) -> Result<tramontana_core::contracts::SearchResults, ErrorEnvelope> {
    state.service.search(&query).await
}

/// List durable tasks, optionally narrowed to a conversation.
#[tauri::command]
pub(crate) async fn list_tasks(
    conversation_id: Option<ConversationId>,
    state: State<'_, AppState>,
) -> Result<Vec<TaskRecord>, ErrorEnvelope> {
    state.service.list_tasks(conversation_id.as_ref()).await
}

/// Replay the canonical event record for one task.
#[tauri::command]
pub(crate) async fn replay_task_events(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<Vec<EventEnvelope>, ErrorEnvelope> {
    state.service.replay_task_events(&task_id, 0).await
}

/// What the person chose for one task in the composer.
///
/// Grouped rather than passed one by one, because they are one decision made
/// in one place, and the window already sends them as one object.
///
/// `admission` and `routing_mode` are ADR 0031's: absent, the task runs
/// read-only under the profile's mode, and an admission only lets the model
/// *ask* — every write and every recipe run is still put to the person.
/// `worker` is ADR 0048's, absent meaning the built-in worker as the routing
/// mode runs it. `skills` are the project's own, ticked from [`list_skills`];
/// their text is given to the model, and they grant nothing (ADR 0053).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskChoices {
    /// Which tools the task's worker may offer its model.
    pub(crate) admission: Option<ToolAdmission>,
    /// Where this task's models may run.
    pub(crate) routing_mode: Option<RoutingMode>,
    /// The worker to run it, by name.
    pub(crate) worker: Option<String>,
    /// The project's skills this task uses, by declared id.
    pub(crate) skills: Option<Vec<String>>,
    /// The person's own skills this task uses, by id (ADR 0070).
    pub(crate) personal_skills: Option<Vec<String>>,
    /// Files picked with [`choose_attachments`] for this message, by token
    /// (ADR 0058).
    pub(crate) attachments: Option<Vec<String>>,
}

/// Start a task and stream its updates over `channel`.
///
/// Returns the task identifier immediately; the work continues in the
/// background and reports through the channel. A long-running operation must
/// never block the interface (`CORE_CONTRACTS.md` §13).
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for an empty or oversized goal, or whatever
/// the service reports.
#[tauri::command]
pub(crate) async fn start_task(
    goal: String,
    conversation_id: Option<ConversationId>,
    choices: Option<TaskChoices>,
    channel: Channel<TaskUpdate>,
    state: State<'_, AppState>,
) -> Result<TaskId, ErrorEnvelope> {
    start_task_on(
        &state,
        goal,
        conversation_id,
        choices.unwrap_or_default(),
        move |update| channel.send(update).is_ok(),
    )
    .await
}

/// Start a task and forward its updates to `send`.
///
/// Separate from [`start_task`] because the channel is the window's half: the
/// rules about the goal, the project a conversation belongs to, and following
/// the task from its first event are the desktop's, and are what a test can
/// hold to.
pub(crate) async fn start_task_on<F>(
    state: &AppState,
    goal: String,
    conversation_id: Option<ConversationId>,
    choices: TaskChoices,
    send: F,
) -> Result<TaskId, ErrorEnvelope>
where
    F: FnMut(TaskUpdate) -> bool + Send + 'static,
{
    // The same rule a host applies to a remote desktop's goal.
    let goal = tramontana_core::services::checked_goal(&goal)?;

    // The project's files are read by the task itself now, with what this
    // task selects, so resolving the project here first would only write an
    // inventory the task is about to replace (ADRs 0052 and 0053).
    let service = state.service.clone();
    // Read, not taken: a task that fails to start leaves the files attached,
    // so the person can send again without picking them again.
    let tokens = choices.attachments.clone().unwrap_or_default();
    let attachments = state.staged_attachments.read(&tokens)?;
    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id,
            goal,
            admission: choices.admission,
            routing_mode: choices.routing_mode,
            worker: choices.worker,
            selection: choices.skills.map(|skill_ids| ConfigurationSelection {
                skill_ids,
                ..ConfigurationSelection::default()
            }),
            attachments,
            personal_skills: choices.personal_skills.unwrap_or_default(),
        })
        .await?;
    for token in &tokens {
        state.staged_attachments.discard(token);
    }

    // Subscribe from the beginning so the interface sees the creation event
    // even though it was written before this call returned.
    let (replayed, updates) = service.subscribe_task_events(&task_id, 0).await?;
    forward_updates(service, task_id.clone(), 0, replayed, updates, None, send);

    Ok(task_id)
}

/// Follow a task that is already running, from the last event the window has.
///
/// A window that reloads loses the channel `start_task` opened, while the task
/// goes on in this process. This reattaches: every durable event after
/// `after_sequence`, then the live stream, until the task ends. Streamed text
/// from before the reload is not repeated; the committed message will be.
///
/// Returns the watch's id, for [`unwatch_task`]: a window that turns to
/// another task stops the forwarding rather than leave it running, unseen,
/// until the task ends.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) async fn watch_task(
    task_id: TaskId,
    after_sequence: u64,
    channel: Channel<TaskUpdate>,
    state: State<'_, AppState>,
) -> Result<u64, ErrorEnvelope> {
    let service = state.service.clone();
    let (replayed, updates) = service
        .subscribe_task_events(&task_id, after_sequence)
        .await?;
    let (watch_id, stop) = state.watches.open();
    forward_updates(
        service,
        task_id,
        after_sequence,
        replayed,
        updates,
        Some(stop),
        move |update| channel.send(update).is_ok(),
    );
    Ok(watch_id)
}

/// Stop the forwarding [`watch_task`] started. Stopping one that has
/// already ended, or was never started, is not an error.
#[tauri::command]
pub(crate) fn unwatch_task(watch_id: u64, state: State<'_, AppState>) {
    state.watches.close(watch_id);
}

/// The forwardings [`watch_task`] started and a window has not stopped.
#[derive(Default)]
pub(crate) struct Watches {
    next: std::sync::atomic::AtomicU64,
    open: std::sync::Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<()>>>,
}

impl Watches {
    /// A new watch: its id, and the signal that resolves when it is closed.
    pub(crate) fn open(&self) -> (u64, tokio::sync::oneshot::Receiver<()>) {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        if let Ok(mut open) = self.open.lock() {
            open.insert(id, stop);
        }
        (id, stopped)
    }

    /// Close a watch. Dropping its sender is the signal.
    pub(crate) fn close(&self, id: u64) {
        if let Ok(mut open) = self.open.lock() {
            open.remove(&id);
        }
    }
}

/// Forward a task's updates to a window until the task ends.
///
/// `replayed` holds the durable events after `after_sequence`, and `updates`
/// the live stream, subscribed before the replay was read. An event can
/// arrive on both, so anything at or below the highest sequence already sent
/// is dropped: the window sees each event once, in order, with no gap. A
/// window that falls behind catches up from the store. `send` returns false
/// when the window has gone, and `stop` resolves when it stopped watching;
/// either ends the forwarding.
pub(crate) fn forward_updates<F>(
    service: std::sync::Arc<tramontana_core::services::TaskService>,
    task_id: TaskId,
    after_sequence: u64,
    replayed: Vec<EventEnvelope>,
    updates: tokio::sync::broadcast::Receiver<TaskUpdate>,
    stop: Option<tokio::sync::oneshot::Receiver<()>>,
    send: F,
) where
    F: FnMut(TaskUpdate) -> bool + Send + 'static,
{
    tokio::spawn(async move {
        let stopped = async move {
            match stop {
                Some(stop) => {
                    let _ = stop.await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = stopped => {}
            () = tramontana_core::services::forward_task_updates(
                service, task_id, after_sequence, replayed, updates, send,
            ) => {}
        }
    });
}

/// Show a native dialog and wait for its answer, without holding a runtime
/// thread: the dialog calls back, and the answer is passed on.
async fn dialog_answer<T: Send + 'static>(
    show: impl FnOnce(Box<dyn FnOnce(T) + Send>),
) -> Result<T, ErrorEnvelope> {
    let (send, receive) = tokio::sync::oneshot::channel();
    show(Box::new(move |answer| {
        let _ = send.send(answer);
    }));
    receive
        .await
        .map_err(|_| internal("the dialog closed unexpectedly"))
}

/// Ask the native picker for a folder; `None` when the person cancels.
///
/// The only way a filesystem path enters from the window: the webview never
/// supplies one.
async fn pick_folder(app: &AppHandle, title: &str) -> Result<Option<PathBuf>, ErrorEnvelope> {
    let Some(selection) =
        dialog_answer(|answer| app.dialog().file().set_title(title).pick_folder(answer)).await?
    else {
        return Ok(None);
    };
    selection
        .into_path()
        .map(Some)
        .map_err(|_| invalid("the selected folder is not a local directory"))
}

async fn pick_project_root(app: &AppHandle) -> Result<Option<CanonicalRoot>, ErrorEnvelope> {
    let Some(path) = pick_folder(app, "Open a Tramontana project").await? else {
        return Ok(None);
    };
    canonicalize_root(&path)
        .map(Some)
        .map_err(|error| invalid(format!("could not open the selected project: {error}")))
}

/// A bundle that was written, and what it holds.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportReport {
    pub schema_version: u32,
    /// The folder the person chose, to show them where it went.
    pub directory: String,
    /// The bundle's table of contents, including what it left out.
    pub manifest: ExportManifest,
}

/// Write a bundle for one task or one project into a folder the person picks.
///
/// Absolute paths and raw tool output stay out unless the person asks for
/// them, and the manifest says what was left out. `None` when they cancel.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] unless exactly one of a task or a project is
/// named, or whatever the export reports.
#[tauri::command]
pub(crate) async fn export_bundle(
    task_id: Option<TaskId>,
    project_id: Option<ProjectId>,
    include_absolute_paths: bool,
    include_raw_tool_output: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ExportReport>, ErrorEnvelope> {
    let scope = match (task_id, project_id) {
        (Some(task_id), None) => ExportScope::Task(task_id),
        (None, Some(project_id)) => ExportScope::Project(project_id),
        _ => return Err(invalid("export one task or one project")),
    };
    let Some(directory) = pick_folder(&app, "Choose where to write the bundle").await? else {
        return Ok(None);
    };
    let options = ExportOptions {
        include_absolute_paths,
        include_raw_tool_output,
        include_artifacts: false,
    };
    let manifest = bundles::export_to(&state.service, &scope, options, &directory).await?;
    Ok(Some(ExportReport {
        schema_version: SCHEMA_VERSION,
        directory: directory.display().to_string(),
        manifest,
    }))
}

/// A bundle the person picked and inspected, waiting for them to confirm.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StagedBundleView {
    /// What the import confirms, instead of a path.
    pub staging_id: String,
    pub inspection: bundles::BundleInspection,
}

/// Pick a bundle folder and say what it holds, writing nothing.
///
/// `None` when the person cancels.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when the folder is not a bundle, or the
/// bundle breaks a rule.
#[tauri::command]
pub(crate) async fn inspect_bundle(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<StagedBundleView>, ErrorEnvelope> {
    let Some(directory) = pick_folder(&app, "Choose a bundle to import").await? else {
        return Ok(None);
    };
    let inspection = bundles::inspect(&state.service, &directory)?;
    let staging_id = UuidIdSource::new().next("bundle");
    bundles::stage(&state.staged_bundle, staging_id.clone(), directory);
    Ok(Some(StagedBundleView {
        staging_id,
        inspection,
    }))
}

/// Import the bundle the person inspected, validating it again first.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when this is not the bundle last inspected, or
/// whatever the import reports.
#[tauri::command]
pub(crate) async fn import_staged_bundle(
    staging_id: String,
    state: State<'_, AppState>,
) -> Result<bundles::ImportReport, ErrorEnvelope> {
    let Some(directory) = bundles::take_staged(&state.staged_bundle, &staging_id) else {
        return Err(ipc_error(
            ErrorCode::NotFound,
            "inspect the bundle again before importing it",
        ));
    };
    bundles::import_from(&state.service, &directory).await
}

/// Pick files to attach to the next message (ADR 0058).
///
/// Each one is read here, checked as core would check it, and held under a
/// random token; the window is told only the token, the name and the size.
/// Empty when the person cancels.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] naming a file that cannot be attached, in
/// which case none of the chosen files is held.
#[tauri::command]
pub(crate) async fn choose_attachments(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<crate::attachments::StagedAttachmentView>, ErrorEnvelope> {
    let selection = dialog_answer(|answer| {
        app.dialog()
            .file()
            .set_title("Attach files")
            .pick_files(answer)
    })
    .await?
    .unwrap_or_default();
    let mut read = Vec::with_capacity(selection.len());
    for picked in selection {
        let path = picked
            .into_path()
            .map_err(|_| invalid("a selected file is not a local file"))?;
        let input = crate::attachments::read_picked(&path)?;
        // Asked now, so a file that cannot be attached is refused when it is
        // picked rather than when the message is sent (ADRs 0058 and 0062).
        state
            .service
            .accept_attachments(vec![input.clone()])
            .await?;
        read.push(input);
    }
    let ids = UuidIdSource::new();
    Ok(read
        .into_iter()
        .map(|input| {
            state
                .staged_attachments
                .stage(ids.next("attachment"), input)
        })
        .collect())
}

/// Stop holding a picked file the person removed before sending.
#[tauri::command]
pub(crate) async fn discard_attachment(
    token: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.staged_attachments.discard(&token);
    Ok(())
}

/// Ask the native picker for a file; `None` when the person cancels.
async fn pick_file(app: &AppHandle, title: &str) -> Result<Option<PathBuf>, ErrorEnvelope> {
    let Some(selection) =
        dialog_answer(|answer| app.dialog().file().set_title(title).pick_file(answer)).await?
    else {
        return Ok(None);
    };
    selection
        .into_path()
        .map(Some)
        .map_err(|_| invalid("the selected file is not a local file"))
}

/// Every configured endpoint, with whether its credential is stored.
///
/// What is configured, which can differ from what is running until the next
/// start: `list_model_endpoints` says what came up.
///
/// # Errors
///
/// [`ErrorCode::PersistenceError`] when the settings cannot be read.
#[tauri::command]
pub(crate) async fn list_configured_endpoints(
    state: State<'_, AppState>,
) -> Result<Vec<settings::ConfiguredEndpoint>, ErrorEnvelope> {
    settings::list(state.settings.as_ref(), state.vault.as_ref()).await
}

/// Save a new endpoint, stamped now.
async fn add_endpoint(
    state: &AppState,
    endpoint_id: &str,
    display_name: Option<String>,
    endpoint: settings::NewEndpoint,
) -> Result<bool, ErrorEnvelope> {
    settings::add(
        state.settings.as_ref(),
        endpoint_id,
        display_name,
        endpoint,
        SystemClock::new().now(),
    )
    .await
}

/// Configure an endpoint speaking the OpenAI chat-completions format.
///
/// Returns whether work sent to it leaves the device. Takes effect the next
/// time Tramontana opens.
///
/// # Errors
///
/// See [`settings::add`].
#[tauri::command]
pub(crate) async fn add_openai_endpoint(
    endpoint_id: String,
    display_name: Option<String>,
    base_url: String,
    model_id: String,
    credential_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<bool, ErrorEnvelope> {
    add_endpoint(
        &state,
        &endpoint_id,
        display_name,
        settings::NewEndpoint::OpenAiCompatible {
            base_url,
            model_id,
            credential_key: credential_key.filter(|key| !key.is_empty()),
        },
    )
    .await
}

/// Configure Anthropic's Messages API. Takes effect the next time Tramontana
/// opens.
///
/// # Errors
///
/// See [`settings::add`].
#[tauri::command]
pub(crate) async fn add_anthropic_endpoint(
    endpoint_id: String,
    display_name: Option<String>,
    base_url: String,
    model_id: String,
    credential_key: String,
    state: State<'_, AppState>,
) -> Result<bool, ErrorEnvelope> {
    add_endpoint(
        &state,
        &endpoint_id,
        display_name,
        settings::NewEndpoint::Anthropic {
            base_url,
            model_id,
            credential_key,
        },
    )
    .await
}

/// Configure a `llama-server` this application starts and stops, from an
/// executable and a model file the person picks. `None` when they cancel
/// either picker. Takes effect the next time Tramontana opens.
///
/// # Errors
///
/// See [`settings::add`].
#[tauri::command]
pub(crate) async fn add_llama_endpoint(
    endpoint_id: String,
    display_name: Option<String>,
    model_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<bool>, ErrorEnvelope> {
    let Some(binary_path) = pick_file(&app, "Choose the llama-server executable").await? else {
        return Ok(None);
    };
    let Some(model_path) = pick_file(&app, "Choose the GGUF model file").await? else {
        return Ok(None);
    };
    add_endpoint(
        &state,
        &endpoint_id,
        display_name,
        settings::NewEndpoint::LlamaServer {
            binary_path,
            model_path,
            model_id,
        },
    )
    .await
    .map(Some)
}

/// Configure an `mlx_lm.server` this application starts and stops, from an
/// executable and a model folder the person picks. `None` when they cancel
/// either picker. Apple Silicon only: elsewhere it is saved but unavailable.
/// Takes effect the next time Tramontana opens.
///
/// # Errors
///
/// See [`settings::add`].
#[tauri::command]
pub(crate) async fn add_mlx_endpoint(
    endpoint_id: String,
    display_name: Option<String>,
    model_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<bool>, ErrorEnvelope> {
    let Some(binary_path) = pick_file(&app, "Choose the mlx_lm.server executable").await? else {
        return Ok(None);
    };
    let Some(model_path) = pick_folder(&app, "Choose the MLX model folder").await? else {
        return Ok(None);
    };
    add_endpoint(
        &state,
        &endpoint_id,
        display_name,
        settings::NewEndpoint::MlxServer {
            binary_path,
            model_path,
            model_id,
        },
    )
    .await
    .map(Some)
}

/// Confirm a runtime's executable and model as they are now (ADR 0043). A
/// changed executable is held until this is done. Takes effect the next time
/// Tramontana opens.
///
/// # Errors
///
/// See [`settings::trust`].
#[tauri::command]
pub(crate) async fn trust_endpoint(
    endpoint_id: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    settings::trust(
        state.settings.as_ref(),
        &endpoint_id,
        SystemClock::new().now(),
    )
    .await
}

/// Whether a runtime's model is still what was recorded: `confirmed`,
/// `held` (changed), `unreadable` or `notRecorded`. Reads the whole model.
///
/// # Errors
///
/// See [`settings::check_runtime_model`].
#[tauri::command]
pub(crate) async fn check_endpoint_model(
    endpoint_id: String,
    state: State<'_, AppState>,
) -> Result<&'static str, ErrorEnvelope> {
    settings::check_runtime_model(state.settings.as_ref(), &endpoint_id).await
}

/// Turn an endpoint on or off, keeping its configuration.
///
/// # Errors
///
/// See [`settings::toggle`].
#[tauri::command]
pub(crate) async fn set_endpoint_enabled(
    endpoint_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    settings::toggle(
        state.settings.as_ref(),
        &endpoint_id,
        settings::Toggle::Enabled(enabled),
    )
    .await
}

/// Record whether the person verified this endpoint's tool-call support.
///
/// # Errors
///
/// See [`settings::toggle`].
#[tauri::command]
pub(crate) async fn set_endpoint_tool_calls(
    endpoint_id: String,
    supported: bool,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    settings::toggle(
        state.settings.as_ref(),
        &endpoint_id,
        settings::Toggle::ToolCalls(supported),
    )
    .await
}

/// Remove an endpoint. Its credential stays in the store.
///
/// # Errors
///
/// See [`settings::remove`].
#[tauri::command]
pub(crate) async fn remove_endpoint(
    endpoint_id: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    settings::remove(state.settings.as_ref(), &endpoint_id).await
}

/// Store a credential the person typed. Returns its length, never the value.
///
/// # Errors
///
/// See [`settings::store_credential`].
#[tauri::command]
pub(crate) async fn store_credential(
    key: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<usize, ErrorEnvelope> {
    settings::store_credential(state.vault.as_ref(), &key, &value).await
}

/// Remove a credential from the store. Returns whether one was there.
///
/// # Errors
///
/// See [`settings::forget_credential`].
#[tauri::command]
pub(crate) async fn forget_credential(
    key: String,
    state: State<'_, AppState>,
) -> Result<bool, ErrorEnvelope> {
    settings::forget_credential(state.vault.as_ref(), &key).await
}

/// Restart Tramontana, so endpoints configured since it opened are used.
///
/// Requested rather than immediate, so the application exits as quitting
/// does and stops its model runtimes first; restarting at once would leave
/// them running. Running tasks are interrupted as any unclean stop interrupts
/// them, and recovered on the next start; the window says so before offering
/// this.
#[tauri::command]
pub(crate) fn restart_application(app: AppHandle) {
    app.request_restart();
}

/// What "Delete all my data" removed from the credential store.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ErasureView {
    pub schema_version: u32,
    /// Credentials the profile named, now removed.
    pub credentials_removed: usize,
    /// Credentials it named that were already gone.
    pub credentials_absent: usize,
}

/// Delete everything this profile holds (ADR 0068). The window asks the
/// person first, and restarts the application after: the runtimes, Apps
/// and browser have been stopped.
///
/// # Errors
///
/// A conflict while a task runs, with nothing removed; or a storage,
/// credential-store or folder error.
#[tauri::command]
pub(crate) async fn erase_all_data(
    state: State<'_, AppState>,
) -> Result<ErasureView, ErrorEnvelope> {
    let report = state.erase_everything().await?;
    Ok(ErasureView {
        schema_version: SCHEMA_VERSION,
        credentials_removed: report.credentials_removed,
        credentials_absent: report.credentials_absent,
    })
}

/// Whether this build can restart itself.
///
/// Not under `tauri dev`: the development server stops when the application
/// exits, so a restarted window would have nothing to load and stay blank.
/// The window asks the person to quit and start it again instead.
#[tauri::command]
pub(crate) fn restart_supported() -> bool {
    !tauri::is_dev()
}

/// Request cancellation of a running task.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) async fn cancel_task(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.cancel_task(&task_id).await
}

/// Read the current view of a task.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) async fn get_task(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<TaskSnapshot, ErrorEnvelope> {
    state.service.get_task(&task_id).await
}

/// Stop this task and write an inspectable checkpoint.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown, or [`ErrorCode::Conflict`]
/// when the worker has not stopped.
#[tauri::command]
pub(crate) async fn create_checkpoint(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<TaskCheckpoint, ErrorEnvelope> {
    state.service.create_checkpoint(&task_id).await
}

/// Read the latest checkpoint without changing anything.
///
/// Inspection is separate from resuming on purpose: the interface shows what
/// was pending and how safe it is to repeat, and the person decides.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task is unknown.
#[tauri::command]
pub(crate) async fn latest_checkpoint(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<Option<TaskCheckpoint>, ErrorEnvelope> {
    state.service.latest_checkpoint(&task_id).await
}

/// Resume the exact checkpoint the user inspected.
///
/// The identifier is required rather than implied: resuming "the latest"
/// would let the interface act on a checkpoint the person never saw.
///
/// # Errors
///
/// [`ErrorCode::Conflict`] when the checkpoint is stale, the task is not
/// interrupted, or the worker or configuration has changed, and
/// [`ErrorCode::InterruptedUnsafe`] when the pending action cannot be repeated.
#[tauri::command]
pub(crate) async fn resume_task(
    task_id: TaskId,
    checkpoint_id: CheckpointId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.resume_task(&task_id, &checkpoint_id).await
}

/// Discard interrupted work without deleting history or undoing any effect.
///
/// # Errors
///
/// [`ErrorCode::Conflict`] when the task is not interrupted.
#[tauri::command]
pub(crate) async fn discard_checkpoint(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state.service.discard_checkpoint(&task_id).await
}

/// A skill a task can be given (ADR 0053).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillView {
    /// The `id` the skill declares, which a task names to select it.
    id: String,
    /// What a person calls it: its declared name, or its file name.
    display_name: String,
    /// Its one-line description, when it declares one.
    description: Option<String>,
}

/// The skills this project offers, in the order a person should read them.
///
/// From the configuration last resolved for the project, so it is the same
/// inventory the configuration view shows. A skill without an `id` cannot be
/// selected and is left out: naming it would offer a choice that resolution
/// would then ignore. Offering a skill is not selecting one, and selecting
/// one grants nothing.
///
/// # Errors
///
/// Whatever the service reports for an unknown project.
#[tauri::command]
pub(crate) async fn list_skills(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<Vec<SkillView>, ErrorEnvelope> {
    let configuration = match state
        .service
        .get_effective_configuration(&project_id)
        .await?
    {
        Some(configuration) => configuration,
        // A project nobody has resolved yet offers nothing rather than
        // failing: the composer simply shows no skills.
        None => return Ok(Vec::new()),
    };
    Ok(configuration
        .sources
        .into_iter()
        .filter(|source| {
            source.enabled && source.kind == tramontana_core::contracts::ConfigurationKind::Skill
        })
        .filter_map(|source| {
            let id = source.declared_id?;
            Some(SkillView {
                display_name: source
                    .declared_name
                    .unwrap_or_else(|| source.relative_path.clone()),
                description: source.declared_description,
                id,
            })
        })
        .collect())
}

/// Which of this project's skills look relevant to `goal` (ADR 0054).
///
/// A suggestion, not a selection: the window marks these and orders them
/// first, and a task still uses only the skills the person ticks. The rule is
/// core's, so the terminal suggests the same ones.
///
/// # Errors
///
/// Whatever the service reports for an unknown project.
#[tauri::command]
pub(crate) async fn suggest_skills(
    project_id: ProjectId,
    goal: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, ErrorEnvelope> {
    let Some(configuration) = state
        .service
        .get_effective_configuration(&project_id)
        .await?
    else {
        return Ok(Vec::new());
    };
    // A goal longer than a task may have is read up to that length rather
    // than refused: this runs while the person is still typing, and an error
    // where a suggestion belongs would be noise.
    let goal = match goal.char_indices().nth(MAX_GOAL_BYTES) {
        Some((at, _)) => &goal[..at],
        None => goal.as_str(),
    };
    Ok(tramontana_core::services::suggested_skills(
        &configuration,
        goal,
    ))
}

/// A worker a task can be handed to (ADR 0048).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkerView {
    /// The name a handoff uses.
    agent_id: String,
    /// What a person calls it.
    display_name: String,
    /// Whether its model runs off this device. `None` for the worker whose
    /// model the routing mode chooses.
    cloud: Option<bool>,
}

impl From<tramontana_core::services::WorkerOption> for WorkerView {
    fn from(worker: tramontana_core::services::WorkerOption) -> Self {
        Self {
            agent_id: worker.agent_id,
            display_name: worker.display_name,
            cloud: worker
                .model
                .map(|model| model.location == tramontana_core::contracts::ModelLocation::Cloud),
        }
    }
}

/// What a stopped task can be handed to, in the order to offer it (ADR 0055).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HandoffOfferView {
    /// The workers, best first; the first is what the panel suggests.
    workers: Vec<WorkerView>,
    /// Whether the task stopped because the work was beyond its model, so the
    /// suggestion is to escalate rather than to try another model here.
    escalating: bool,
}

/// The workers this interrupted task can be handed to, best first.
///
/// The order and the exclusions are the core's: the worker that already has
/// the task is left out, a task running Local Only is offered no model off
/// this device, and a task that ran out of what its model could do is offered
/// the cloud first. The window renders what it is given.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the task does not exist.
#[tauri::command]
pub(crate) async fn handoff_options(
    task_id: TaskId,
    state: State<'_, AppState>,
) -> Result<HandoffOfferView, ErrorEnvelope> {
    let offer = state.service.handoff_options(&task_id).await?;
    Ok(HandoffOfferView {
        workers: offer.workers.into_iter().map(WorkerView::from).collect(),
        escalating: offer.escalating,
    })
}

/// The workers a stopped task can be handed to: the built-in worker as the
/// routing mode runs it, and pinned to each configured model.
#[tauri::command]
pub(crate) fn list_workers(state: State<'_, AppState>) -> Vec<WorkerView> {
    state
        .service
        .workers()
        .into_iter()
        .map(WorkerView::from)
        .collect()
}

/// Hand the checkpoint the person inspected to the worker they chose, which
/// continues the task from its canonical record (ADR 0048).
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no such worker is offered, and
/// [`ErrorCode::Conflict`] when the worker already has the task, the
/// checkpoint is stale, or the task runs Local Only and the worker's model
/// does not.
#[tauri::command]
pub(crate) async fn hand_off_task(
    task_id: TaskId,
    checkpoint_id: CheckpointId,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    state
        .service
        .hand_off_to(&task_id, &checkpoint_id, &agent_id)
        .await
        .map(|_| ())
}

/// Build an invalid-request error for a boundary rejection.
fn invalid(message: impl Into<String>) -> ErrorEnvelope {
    ipc_error(ErrorCode::InvalidRequest, message)
}

fn internal(message: impl Into<String>) -> ErrorEnvelope {
    ipc_error(ErrorCode::Internal, message)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn core_info_reports_the_core_schema_version() {
        let info = core_info();
        assert_eq!(info.schema_version, tramontana_core::SCHEMA_VERSION);
        assert!(!info.app_version.is_empty());
    }

    #[test]
    fn boundary_rejections_are_normalized_errors() {
        let error = invalid("a task needs a goal");
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(!error.retryable, "retrying an invalid payload cannot help");
    }
}

/// Deliver a decision from an approval card.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no question is waiting — which happens when a
/// card is answered twice, or after it has already timed out.
#[tauri::command]
pub(crate) fn resolve_approval(
    approval_id: ApprovalId,
    decision: ApprovalDecisionKind,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    if state.approver.resolve(&approval_id, decision) {
        return Ok(());
    }
    Err(ipc_error(
        ErrorCode::NotFound,
        "that approval is no longer waiting for an answer",
    ))
}

/// Approval questions still waiting for an answer, oldest first.
///
/// A window asks for these when it opens. The event that raised a card is not
/// replayed, so a question asked before this window existed — or before it was
/// reloaded — would otherwise go unanswered while its task waited.
#[tauri::command]
pub(crate) fn pending_approvals(state: State<'_, AppState>) -> Vec<ApprovalRequest> {
    state.approver.pending()
}

/// The Git reads the Changes panel runs for the person (ADR 0032). The core
/// accepts any read that takes no arguments; these are the two this boundary
/// asks for.
pub(crate) const GIT_STATUS: &str = "builtin.git.status";
pub(crate) const GIT_DIFF: &str = "builtin.git.diff";

/// A project's working tree as Git sees it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectChanges {
    /// Version of this response envelope.
    pub schema_version: u32,
    /// Git's status, including files it does not track yet.
    pub status: String,
    /// Git's per-file summary and patch for tracked files.
    pub diff: String,
    /// Whether either read was cut short by its output limit.
    pub truncated: bool,
}

/// Read a project's Git status and diff for the person.
///
/// Belongs to no task and changes nothing: the core runs these reads only
/// where it would let a task read without asking (ADR 0032).
///
/// # Errors
///
/// Whatever the core reports: an unknown project, a refused read, or Git's
/// own failure, such as a project that is not a repository.
#[tauri::command]
pub(crate) async fn project_changes(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<ProjectChanges, ErrorEnvelope> {
    project_changes_on(&state, &project_id).await
}

/// Read a project's Git status and diff. See [`project_changes`].
pub(crate) async fn project_changes_on(
    state: &AppState,
    project_id: &ProjectId,
) -> Result<ProjectChanges, ErrorEnvelope> {
    let status = state.service.read_project(project_id, GIT_STATUS).await?;
    let diff = state.service.read_project(project_id, GIT_DIFF).await?;
    Ok(ProjectChanges {
        schema_version: SCHEMA_VERSION,
        status: text_of(&status),
        diff: text_of(&diff),
        truncated: status.truncated || diff.truncated,
    })
}

/// The text a Git read returned.
fn text_of(outcome: &ToolOutcome) -> String {
    outcome
        .content
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Every live grant for a project, so the user can see what they have allowed.
///
/// # Errors
///
/// A persistence error when the grant store is unavailable.
#[tauri::command]
pub(crate) async fn list_project_grants(
    project_id: ProjectId,
    state: State<'_, AppState>,
) -> Result<Vec<ProjectGrant>, ErrorEnvelope> {
    state
        .approvals
        .list_grants(&project_id)
        .await
        .map_err(|error| ipc_error(ErrorCode::PersistenceError, error.to_string()))
}

/// Revoke a grant. The next invocation it would have covered is refused.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no such grant exists.
#[tauri::command]
pub(crate) async fn revoke_project_grant(
    grant_id: String,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    let removed = state
        .approvals
        .revoke_grant(&grant_id)
        .await
        .map_err(|error| ipc_error(ErrorCode::PersistenceError, error.to_string()))?;

    if removed {
        Ok(())
    } else {
        Err(ipc_error(ErrorCode::NotFound, "no such grant"))
    }
}

/// One configured endpoint, as the interface sees it.
///
/// Deliberately not the stored `ProviderEndpoint`: what a window needs is
/// "can I use this, and if not why", and sending the whole configuration —
/// including a credential *reference* — to a webview would be handing out
/// more than the question needs.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelEndpointStatus {
    /// The endpoint identifier, which routing decisions also name.
    pub(crate) endpoint_id: String,
    /// The models it offers.
    pub(crate) models: Vec<String>,
    /// Whether work sent here leaves the device. Derived, never declared.
    pub(crate) leaves_the_device: bool,
    /// Whether it came up.
    pub(crate) available: bool,
    /// Why it did not, when it did not. Already normalized and secret-free.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable_reason: Option<String>,
    /// For a runtime Tramontana started, the accelerators it reported it can
    /// use; empty when it reported none. Absent for anything else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) accelerators: Option<Vec<String>>,
}

/// Report every configured model endpoint and whether it is usable.
///
/// The "list, health" half of the models/providers surface
/// (`ARCHITECTURE.md` §416). Configuring an endpoint is `tramontana provider`
/// for now; this window can show what is configured but not yet change it.
///
/// # Errors
///
/// Never fails: an endpoint that could not be brought up is reported as
/// unavailable rather than turning the whole list into an error.
#[tauri::command]
pub(crate) async fn list_model_endpoints(
    state: State<'_, AppState>,
) -> Result<Vec<ModelEndpointStatus>, ErrorEnvelope> {
    let plane = state.models.lock().await;
    let mut statuses = Vec::new();

    for provider in &plane.providers {
        let models = provider.list_models().await;
        let endpoint_id = provider.id().to_string();
        statuses.push(ModelEndpointStatus {
            accelerators: plane.accelerators(&endpoint_id).map(<[String]>::to_vec),
            endpoint_id,
            leaves_the_device: models
                .iter()
                .any(|model| model.location == tramontana_core::contracts::ModelLocation::Cloud),
            models: models.into_iter().map(|model| model.model_id).collect(),
            available: true,
            unavailable_reason: None,
        });
    }

    for failure in &plane.failures {
        statuses.push(ModelEndpointStatus {
            endpoint_id: failure.endpoint_id.clone(),
            models: Vec::new(),
            // Unknown, and false is the safe answer to show next to
            // "unavailable": nothing was sent anywhere.
            leaves_the_device: false,
            available: false,
            unavailable_reason: Some(failure.error.message.clone()),
            accelerators: None,
        });
    }

    Ok(statuses)
}

/// Every configured App, and how each is doing in this window (ADR 0040).
///
/// # Errors
///
/// [`ErrorCode::PersistenceError`] when the App settings cannot be read.
#[tauri::command]
pub(crate) async fn list_apps(
    state: State<'_, AppState>,
) -> Result<Vec<crate::apps::AppView>, ErrorEnvelope> {
    crate::apps::list(&state).await
}

/// Turn an App on or off. Takes effect the next time Tramontana opens.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no App has that identifier, or
/// [`ErrorCode::PersistenceError`].
#[tauri::command]
pub(crate) async fn set_app_enabled(
    app_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), ErrorEnvelope> {
    crate::apps::set_enabled(state.app_settings.as_ref(), &app_id, enabled).await
}

/// What was decided about an App's tools, newest first, from the record.
///
/// # Errors
///
/// Whatever reading the record reports.
#[tauri::command]
pub(crate) async fn app_permission_history(
    app_id: String,
    state: State<'_, AppState>,
) -> Result<crate::apps::AppHistory, ErrorEnvelope> {
    crate::apps::history(&state.service, &app_id).await
}
