//! The `task` subcommand: run one task and stream its activity.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{
    CheckpointId, ConversationId, EventEnvelope, EventPayload, TaskId, TaskState, TaskUpdate,
};
use tramontana_core::services::{StartTaskRequest, TaskService};

use crate::render::{self, event_line, frame_line};
use crate::wiring;

/// Arguments for `tramontana task`.
#[derive(Debug, Args)]
pub(crate) struct TaskArgs {
    /// Inspect durable tasks instead of running one.
    #[command(subcommand)]
    command: Option<TaskCommand>,

    /// What to ask the worker to do.
    #[arg(default_value = "Say hello")]
    goal: String,

    /// Conversation to attach the task to.
    #[arg(long, value_name = "CONVERSATION_ID")]
    conversation_id: Option<String>,

    /// Project-relative path that determines scoped instruction activation.
    #[arg(long, value_name = "RELATIVE_PATH")]
    active_path: Option<String>,

    /// Agent profile definition to activate.
    #[arg(long, value_name = "ID")]
    agent_id: Option<String>,

    /// Workflow definition to activate.
    #[arg(long, value_name = "ID")]
    workflow_id: Option<String>,

    /// Skill definition to activate. Repeat to set explicit precedence order.
    #[arg(long = "skill-id", value_name = "ID")]
    skill_ids: Vec<String>,

    /// One of your own skills to use, by id (`tramontana skill list`).
    /// Repeat to use more, in order.
    #[arg(long = "personal-skill", value_name = "ID")]
    personal_skills: Vec<String>,

    /// Cancel the task after this many milliseconds.
    ///
    /// Exercises the cancellation path end to end without needing a terminal
    /// interrupt, so the behavior is scriptable and testable.
    #[arg(long, value_name = "MS")]
    cancel_after_ms: Option<u64>,

    /// Let the reference worker create and patch files in the project.
    ///
    /// Each write is still asked about on the terminal as it happens, and is
    /// refused when there is no terminal to ask on. Without this flag the
    /// model is never offered a write at all.
    #[arg(long)]
    allow_writes: bool,

    /// Let the reference worker ask to run the project's recipes, the
    /// commands `.tramontana/project.md` declares under `recipes:`.
    ///
    /// The model can only name a recipe. Every run is asked about on the
    /// terminal, showing the exact command, and is refused when there is no
    /// terminal to ask on. Without this flag the model is never offered one.
    #[arg(long)]
    allow_recipes: bool,

    /// Offer the model the tools of the profile's enabled Apps that say they
    /// only read.
    ///
    /// Every call is still asked about on the terminal: what an App says
    /// about its own tool decides what may be offered, never what may run
    /// (ADRs 0035 and 0036).
    #[arg(long)]
    apps: bool,

    /// Let the reference worker browse the web in a window of your installed
    /// Chrome, Edge or Chromium, with a throwaway profile of its own.
    ///
    /// The first visit to each site is asked about on the terminal, showing
    /// the address; after that the task may read, click and type there.
    /// Submitting a form is asked about every time. Local Only does not
    /// cover the browser: it governs where models run (ADR 0047).
    #[arg(long)]
    browse: bool,

    /// Where this task's models may run. Recorded with the task and kept if
    /// it is resumed. Without it the task routes under the default mode,
    /// automatic.
    #[arg(long, value_enum, value_name = "MODE")]
    mode: Option<ModeArg>,

    /// Run it on one model rather than the one the mode chooses:
    /// `builtin.reference@<endpoint>/<model>` (ADR 0048). An unknown name
    /// lists what is offered. A model off this device is refused under
    /// Local Only.
    #[arg(long, value_name = "WORKER")]
    worker: Option<String>,

    /// Attach a file to this message: a text file's contents, or the text of
    /// a PDF or Word (.docx) document, are given to the model before the
    /// goal, as data (ADRs 0058 and 0062). Repeat for more files, up to five. Only the file's text is kept, in the task's record; the task
    /// is given no access to its folder.
    #[arg(long = "attach", value_name = "PATH")]
    attach: Vec<std::path::PathBuf>,

    /// Emit each event as a JSON line instead of a formatted table.
    #[arg(long)]
    json: bool,

    /// Pause between model chunks, so streaming is observable.
    #[arg(long, value_name = "MS", default_value_t = 120)]
    chunk_delay_ms: u64,

    /// Database file to use. Also configurable with `TRAMONTANA_PROFILE`.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// List durable task records, optionally in one conversation.
    List(TaskListArgs),
    /// Show one complete durable task record.
    Show(TaskShowArgs),
    /// Replay canonical task events without transient stream frames.
    Replay(TaskReplayArgs),
    /// Stop and checkpoint this task, or inspectably snapshot recovered work.
    Checkpoint(TaskShowArgs),
    /// Show the durable record and latest checkpoint, including replay safety.
    Inspect(TaskShowArgs),
    /// Explicitly resume the latest inspected checkpoint with the same worker.
    Resume(TaskResumeArgs),
    /// Discard interrupted continuation without deleting history or undoing effects.
    Discard(TaskShowArgs),
    /// Print what a different worker would need to continue this task.
    Handoff(HandoffArgs),
}

#[derive(Debug, Args)]
struct HandoffArgs {
    #[arg(value_name = "TASK_ID")]
    task_id: String,

    /// Checkpoint to hand over. Omit to print the bundle without handing it on.
    #[arg(long, requires = "to")]
    checkpoint_id: Option<String>,

    /// Worker to hand the task to: `builtin.reference` for the model the
    /// routing mode chooses, or `builtin.reference@<endpoint>/<model>` for
    /// one configured model. An unknown name lists what is offered.
    #[arg(long, value_name = "AGENT_ID", default_value = "fake.receiving")]
    to: String,

    #[arg(long)]
    json: bool,

    #[arg(long, default_value_t = 120)]
    chunk_delay_ms: u64,

    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
struct TaskResumeArgs {
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Exact checkpoint ID shown by task inspect.
    #[arg(long)]
    checkpoint_id: String,
    /// Resume with writes allowed. Must match how the task was started: a
    /// resume cannot add or drop what the model may be offered.
    #[arg(long)]
    allow_writes: bool,
    /// Resume with recipes allowed. Must match how the task was started.
    #[arg(long)]
    allow_recipes: bool,
    /// Resume with the profile's Apps allowed. Must match how the task was
    /// started.
    #[arg(long)]
    apps: bool,
    /// Resume with the browser allowed. Must match how the task was started.
    #[arg(long)]
    browse: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 120)]
    chunk_delay_ms: u64,
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
struct TaskListArgs {
    #[arg(long, value_name = "CONVERSATION_ID")]
    conversation_id: Option<String>,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
struct TaskShowArgs {
    #[arg(value_name = "TASK_ID")]
    task_id: String,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
struct TaskReplayArgs {
    #[arg(value_name = "TASK_ID")]
    task_id: String,

    /// Return events strictly after this durable sequence.
    #[arg(long, value_name = "SEQUENCE", default_value_t = 0)]
    after_sequence: u64,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

/// Run one task to a terminal state, streaming its activity.
pub(crate) async fn execute(mut args: TaskArgs) -> ExitCode {
    match args.command.take() {
        Some(TaskCommand::List(args)) => list(args).await,
        Some(TaskCommand::Show(args)) => show(args).await,
        Some(TaskCommand::Replay(args)) => replay(args).await,
        Some(TaskCommand::Checkpoint(args)) => checkpoint_command(args, "checkpoint").await,
        Some(TaskCommand::Inspect(args)) => checkpoint_command(args, "inspect").await,
        Some(TaskCommand::Discard(args)) => checkpoint_command(args, "discard").await,
        Some(TaskCommand::Handoff(args)) => handoff(args).await,
        Some(TaskCommand::Resume(args)) => resume(args).await,
        None => run(args).await,
    }
}

/// A routing mode, as `--mode` spells it.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum ModeArg {
    /// Models run on this device, and nothing is sent to a cloud model.
    LocalOnly,
    /// A model is chosen; anything sent to a cloud model is asked about first.
    Automatic,
    /// A cloud model is chosen when one is available; each release is asked about.
    CloudPreferred,
}

impl From<ModeArg> for tramontana_core::contracts::RoutingMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::LocalOnly => Self::LocalOnly,
            ModeArg::Automatic => Self::Automatic,
            ModeArg::CloudPreferred => Self::CloudPreferred,
        }
    }
}

/// What the reference worker may offer its model, from `--allow-writes`,
/// `--allow-recipes`, `--apps` and `--browse`: separate choices, none
/// implying another.
fn admission(
    allow_writes: bool,
    allow_recipes: bool,
    apps: bool,
    browse: bool,
) -> tramontana_core::ports::ToolAdmission {
    tramontana_core::ports::ToolAdmission {
        writes: allow_writes,
        recipes: allow_recipes,
        apps,
        browse,
    }
}

async fn run(args: TaskArgs) -> ExitCode {
    let profile = wiring::profile_path(args.profile);
    let (service, mut models, apps) = match wiring::open_running(
        &profile,
        args.chunk_delay_ms,
        admission(
            args.allow_writes,
            args.allow_recipes,
            args.apps,
            args.browse,
        ),
    )
    .await
    {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };

    let conversation_id = args.conversation_id.map(ConversationId::new);
    // The task carries its own selection rather than resolving the project
    // first: what a task uses is the task's, and starting one now resolves
    // the project's files itself (ADRs 0052 and 0053).
    let selection = ConfigurationSelection {
        active_path: args.active_path,
        agent_id: args.agent_id,
        workflow_id: args.workflow_id,
        skill_ids: args.skill_ids,
    };
    if selection.skill_ids.is_empty() {
        suggest(&service, conversation_id.as_ref(), &args.goal).await;
    }

    // Read here, where the person named the file; whether it may be
    // attached is core's to say (ADR 0058).
    let mut attachments = Vec::with_capacity(args.attach.len());
    for path in &args.attach {
        match read_attachment(path) {
            Ok(input) => attachments.push(input),
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::FAILURE;
            }
        }
    }

    let task_id = match service
        .start_task(StartTaskRequest {
            conversation_id,
            goal: args.goal,
            routing_mode: args.mode.map(Into::into),
            worker: args.worker.clone(),
            selection: Some(selection),
            attachments,
            personal_skills: args.personal_skills.clone(),
            ..Default::default()
        })
        .await
    {
        Ok(task_id) => task_id,
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            if error.code == tramontana_core::contracts::ErrorCode::NotFound {
                eprintln!("workers:");
                for worker in service.workers() {
                    eprintln!("  {}  {}", worker.agent_id, worker.display_name);
                }
            }
            return ExitCode::FAILURE;
        }
    };

    let outcome = stream_task(service, task_id, args.json, args.cancel_after_ms, 0).await;

    // The graceful path. Every early return above drops the plane instead,
    // which stops the same runtimes without the grace period — correct, and
    // the reason cleanup lives in `Drop` as well as in a method.
    models.shutdown().await;
    apps.shutdown().await;
    outcome
}

async fn stream_task(
    service: Arc<TaskService>,
    task_id: TaskId,
    json: bool,
    cancel_after_ms: Option<u64>,
    after_sequence: u64,
) -> ExitCode {
    // Subscribe before replay so nothing between start/resume and here is missed.
    let (replayed, mut updates) = match service
        .subscribe_task_events(&task_id, after_sequence)
        .await
    {
        Ok(subscription) => subscription,
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            return ExitCode::FAILURE;
        }
    };

    if let Some(delay_ms) = cancel_after_ms {
        let service = Arc::clone(&service);
        let task_id = task_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = service.cancel_task(&task_id).await;
        });
    }

    let mut highest_rendered = after_sequence;
    let mut terminal_rendered = false;
    for event in replayed {
        highest_rendered = highest_rendered.max(event.sequence);
        terminal_rendered |= is_stopped(&event);
        emit_event(&event, json);
    }

    while !terminal_rendered {
        match updates.recv().await {
            Ok(TaskUpdate::Event { event }) => {
                // Subscription precedes replay, so an event can arrive twice.
                // De-duplicating on sequence is cheaper than risking a gap.
                if event.sequence <= highest_rendered {
                    continue;
                }
                highest_rendered = event.sequence;
                let terminal = is_stopped(&event);
                emit_event(&event, json);
                if terminal {
                    terminal_rendered = true;
                }
            }
            Ok(TaskUpdate::Frame { frame }) => {
                if !json {
                    println!("{}", frame_line(&frame));
                }
            }
            // Lagging is recoverable: the durable events are in the store, so
            // catch up by replaying rather than reporting a failure.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                match service
                    .subscribe_task_events(&task_id, highest_rendered)
                    .await
                {
                    Ok((missed, _)) => {
                        for event in missed {
                            highest_rendered = highest_rendered.max(event.sequence);
                            terminal_rendered |= is_stopped(&event);
                            emit_event(&event, json);
                        }
                    }
                    Err(error) => {
                        eprintln!("warning: could not replay after lag: {}", error.message);
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            // `TaskUpdate` is non-exhaustive: a newer core may deliver a kind
            // this build has never seen, and ignoring it is correct — the
            // durable events still arrive as `Event`.
            Ok(_) => {}
        }
    }

    match service.get_task(&task_id).await {
        Ok(snapshot) => {
            // A run that ended interrupted leaves a checkpoint behind, so the
            // next command can inspect what was pending rather than having to
            // reconstruct it.
            if snapshot.state == TaskState::Interrupted
                && let Err(error) = service.create_checkpoint(&task_id).await
            {
                return render::application_error(&error);
            }
            if !json {
                println!("\nstate: {:?}", snapshot.state);
            }
            // A cancelled or failed task is a non-zero exit so a script can
            // tell what happened without parsing output.
            match snapshot.state {
                TaskState::Completed => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            }
        }
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            ExitCode::FAILURE
        }
    }
}

fn is_stopped(event: &EventEnvelope) -> bool {
    event.is_terminal_outcome()
        || matches!(
            &event.payload,
            EventPayload::TaskStateChanged {
                current: TaskState::Interrupted,
                ..
            }
        )
}

async fn checkpoint_command(args: TaskShowArgs, operation: &str) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    let task_id = TaskId::new(args.task_id);
    match operation {
        "checkpoint" => render::result(service.create_checkpoint(&task_id).await),
        "discard" => match service.discard_checkpoint(&task_id).await {
            Ok(()) => render::result(service.get_task_record(&task_id).await),
            Err(error) => render::application_error(&error),
        },
        _ => match (
            service.get_task_record(&task_id).await,
            service.latest_checkpoint(&task_id).await,
        ) {
            (Ok(task), Ok(checkpoint)) => {
                render::json(&serde_json::json!({"task":task,"checkpoint":checkpoint}))
            }
            (Err(error), _) | (_, Err(error)) => render::application_error(&error),
        },
    }
}

async fn resume(args: TaskResumeArgs) -> ExitCode {
    // Held for the whole command: dropping it stops any runtime this process
    // started, and dropping it early would stop the model mid-answer.
    let (service, _models, _apps) = match wiring::open_running(
        &wiring::profile_path(args.profile),
        args.chunk_delay_ms,
        admission(
            args.allow_writes,
            args.allow_recipes,
            args.apps,
            args.browse,
        ),
    )
    .await
    {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let task_id = TaskId::new(args.task_id);
    let sequence = match service.get_task(&task_id).await {
        Ok(snapshot) => snapshot.last_sequence,
        Err(error) => return render::application_error(&error),
    };
    if let Err(error) = service
        .resume_task(&task_id, &CheckpointId::new(args.checkpoint_id))
        .await
    {
        return render::application_error(&error);
    }
    stream_task(service, task_id, args.json, None, sequence).await
}

async fn list(args: TaskListArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    let conversation_id = args.conversation_id.map(ConversationId::new);
    render::result(service.list_tasks(conversation_id.as_ref()).await)
}

async fn show(args: TaskShowArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    render::result(service.get_task_record(&TaskId::new(args.task_id)).await)
}

async fn replay(args: TaskReplayArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    render::result(
        service
            .replay_task_events(&TaskId::new(args.task_id), args.after_sequence)
            .await,
    )
}

/// Print one event in the selected format.
fn emit_event(event: &tramontana_core::contracts::EventEnvelope, json: bool) {
    if json {
        match serde_json::to_string(event) {
            Ok(line) => println!("{line}"),
            Err(error) => eprintln!("warning: could not serialize event: {error}"),
        }
    } else {
        println!("{}", event_line(event));
    }
}

/// Show what a different worker would need to continue this task, and
/// optionally hand it over.
///
/// Printing and handing over are the same command because they are the same
/// decision seen twice: the bundle a person reads is exactly the bundle the
/// second worker is given. A separate "preview" would be a different artifact
/// and would prove nothing about the one that matters.
async fn handoff(args: HandoffArgs) -> ExitCode {
    // Held for the whole command: dropping it stops any runtime this process
    // started, and dropping it early would stop the model mid-answer.
    let (service, _models, _apps) = match wiring::open_running(
        &wiring::profile_path(args.profile),
        args.chunk_delay_ms,
        tramontana_core::ports::ToolAdmission::READ_ONLY,
    )
    .await
    {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let task_id = TaskId::new(args.task_id);

    let Some(checkpoint_id) = args.checkpoint_id else {
        return render::result(service.handoff_task(&task_id).await);
    };

    let checkpoint_id = CheckpointId::new(checkpoint_id);
    let offered = service.workers();
    let handed = if offered.iter().any(|worker| worker.agent_id == args.to) {
        // The built-in worker, as the router runs it or pinned to a model,
        // offered what the task was allowed (ADR 0048).
        service
            .hand_off_to(&task_id, &checkpoint_id, &args.to)
            .await
    } else if let Some(receiving) = wiring::worker_named(&args.to) {
        service
            .hand_off_task(&task_id, &checkpoint_id, receiving)
            .await
    } else {
        eprintln!("error: unknown worker {:?}; available:", args.to);
        // In the core's order, so the terminal suggests what the window would
        // (ADR 0055): the first is the one to reach for.
        match service.handoff_options(&task_id).await {
            Ok(offer) => {
                if offer.escalating {
                    eprintln!(
                        "  this task ran out of what its model could do, so a model off this \
                         device is offered first"
                    );
                }
                for (position, worker) in offer.workers.iter().enumerate() {
                    let mark = if position == 0 { "*" } else { " " };
                    eprintln!("{mark} {}  {}", worker.agent_id, worker.display_name);
                }
            }
            Err(_) => {
                for worker in &offered {
                    eprintln!("  {}  {}", worker.agent_id, worker.display_name);
                }
            }
        }
        for name in wiring::available_workers() {
            if !offered.iter().any(|worker| worker.agent_id == name) {
                eprintln!("  {name}");
            }
        }
        return ExitCode::FAILURE;
    };
    let handed = match handed {
        Ok(handed) => handed,
        Err(error) => return render::application_error(&error),
    };

    if args.json {
        // The bundle first, so what the second worker was given is on the
        // record before any of its activity scrolls past.
        let _ = render::json(&handed);
    } else {
        println!("handed to {} at event {}", args.to, handed.event_sequence);
    }
    stream_task(service, task_id, args.json, None, handed.event_sequence).await
}

/// Say which of the project's skills look relevant, and how to use one.
///
/// Only when the person named none: a note beside a choice already made
/// would be noise. It goes to stderr, because `--json` puts the task's
/// events on stdout and a note is not one of them. Nothing is selected by it
/// (ADR 0054).
async fn suggest(
    service: &tramontana_core::services::TaskService,
    conversation_id: Option<&ConversationId>,
    goal: &str,
) {
    let Some(conversation_id) = conversation_id else {
        return;
    };
    let Ok(conversation) = service.get_conversation(conversation_id).await else {
        return;
    };
    let Some(project_id) = conversation.project_id else {
        return;
    };
    let Ok(Some(configuration)) = service.get_effective_configuration(&project_id).await else {
        return;
    };
    let suggested = tramontana_core::services::suggested_skills(&configuration, goal);
    if suggested.is_empty() {
        return;
    }
    eprintln!(
        "note: these skills match this goal: {}",
        suggested.join(", ")
    );
    eprintln!("      this task uses none; pass --skill-id <ID> to use one");
}

/// Read a file named with `--attach`, at most one byte more than an
/// attachment may hold, so an oversized file is refused without being read
/// whole.
fn read_attachment(
    path: &std::path::Path,
) -> Result<tramontana_core::services::AttachmentInput, String> {
    use std::io::Read;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_owned();
    let limit = u64::try_from(
        tramontana_core::services::ATTACHMENT_BYTES.max(tramontana_core::services::DOCUMENT_BYTES),
    )
    .unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    Ok(tramontana_core::services::AttachmentInput { name, bytes })
}
