//! The `task` subcommand: run one task and stream its activity.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use tramontana_core::contracts::{TaskState, TaskUpdate};
use tramontana_core::services::StartTaskRequest;

use crate::render::{event_line, frame_line};
use crate::wiring;

/// Arguments for `tramontana task`.
#[derive(Debug, Args)]
pub(crate) struct TaskArgs {
    /// What to ask the worker to do.
    #[arg(default_value = "Say hello")]
    goal: String,

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

    /// Database file to use. Defaults to `.tramontana-local/tramontana.sqlite`.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

/// Run one task to a terminal state, streaming its activity.
pub(crate) fn execute(args: TaskArgs) -> ExitCode {
    // The CLI owns the runtime; the services are runtime-agnostic.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(args))
}

async fn run(args: TaskArgs) -> ExitCode {
    let profile = args.profile.unwrap_or_else(wiring::default_profile_path);
    let service = match wiring::open(&profile, args.chunk_delay_ms).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };

    let task_id = match service
        .start_task(StartTaskRequest {
            conversation_id: None,
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
    for event in replayed {
        highest_rendered = highest_rendered.max(event.sequence);
        emit_event(&event, args.json);
    }

    loop {
        match updates.recv().await {
            Ok(TaskUpdate::Event { event }) => {
                // Subscription precedes replay, so an event can arrive twice.
                // De-duplicating on sequence is cheaper than risking a gap.
                if event.sequence <= highest_rendered {
                    continue;
                }
                highest_rendered = event.sequence;
                let terminal = matches!(
                    event.payload,
                    tramontana_core::contracts::EventPayload::TaskCompleted { .. }
                        | tramontana_core::contracts::EventPayload::TaskFailed { .. }
                        | tramontana_core::contracts::EventPayload::TaskCancelled { .. }
                );
                emit_event(&event, args.json);
                if terminal {
                    break;
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
