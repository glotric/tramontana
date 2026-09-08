//! The `task` subcommand: run one task and stream its activity.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{ConversationId, TaskId, TaskState, TaskUpdate};
use tramontana_core::services::StartTaskRequest;

use crate::configuration;
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

    /// Cancel the task after this many milliseconds.
    ///
    /// Exercises the cancellation path end to end without needing a terminal
    /// interrupt, so the behavior is scriptable and testable.
    #[arg(long, value_name = "MS")]
    cancel_after_ms: Option<u64>,

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
        None => run(args).await,
    }
}

async fn run(args: TaskArgs) -> ExitCode {
    let profile = wiring::profile_path(args.profile);
    let service = match wiring::open(&profile, args.chunk_delay_ms).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };

    let conversation_id = args.conversation_id.map(ConversationId::new);
    if let Some(conversation_id) = &conversation_id {
        let conversation = match service.get_conversation(conversation_id).await {
            Ok(conversation) => conversation,
            Err(error) => return render::application_error(&error),
        };
        if let Some(project_id) = conversation.project_id {
            let selection = ConfigurationSelection {
                active_path: args.active_path,
                agent_id: args.agent_id,
                workflow_id: args.workflow_id,
                skill_ids: args.skill_ids,
            };
            if let Err(message) =
                configuration::resolve_project(&service, &project_id, &selection).await
            {
                eprintln!("error: configuration could not be resolved: {message}");
                return ExitCode::FAILURE;
            }
        }
    }

    let task_id = match service
        .start_task(StartTaskRequest {
            conversation_id,
            goal: args.goal,
        })
        .await
    {
        Ok(task_id) => task_id,
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            return ExitCode::FAILURE;
        }
    };

    // Subscribe from sequence 0 so nothing between start and here is missed.
    let (replayed, mut updates) = match service.subscribe_task_events(&task_id, 0).await {
        Ok(subscription) => subscription,
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            return ExitCode::FAILURE;
        }
    };

    if let Some(delay_ms) = args.cancel_after_ms {
        let service = Arc::clone(&service);
        let task_id = task_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = service.cancel_task(&task_id).await;
        });
    }

    let mut highest_rendered = 0;
    let mut terminal_rendered = false;
    for event in replayed {
        highest_rendered = highest_rendered.max(event.sequence);
        terminal_rendered |= event.is_terminal_outcome();
        emit_event(&event, args.json);
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
                let terminal = event.is_terminal_outcome();
                emit_event(&event, args.json);
                if terminal {
                    terminal_rendered = true;
                }
            }
            Ok(TaskUpdate::Frame { frame }) => {
                if !args.json {
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
                            terminal_rendered |= event.is_terminal_outcome();
                            emit_event(&event, args.json);
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
            if !args.json {
                println!("\nterminal state: {:?}", snapshot.state);
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

async fn list(args: TaskListArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let conversation_id = args.conversation_id.map(ConversationId::new);
    match service.list_tasks(conversation_id.as_ref()).await {
        Ok(tasks) => render::json(&tasks),
        Err(error) => render::application_error(&error),
    }
}

async fn show(args: TaskShowArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    match service.get_task_record(&TaskId::new(args.task_id)).await {
        Ok(task) => render::json(&task),
        Err(error) => render::application_error(&error),
    }
}

async fn replay(args: TaskReplayArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    match service
        .replay_task_events(&TaskId::new(args.task_id), args.after_sequence)
        .await
    {
        Ok(events) => render::json(&events),
        Err(error) => render::application_error(&error),
    }
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
