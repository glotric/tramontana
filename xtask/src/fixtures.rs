//! Deterministic cross-language contract fixtures.
//!
//! `TESTING_STRATEGY.md` §4 requires fixtures that Rust writes and TypeScript
//! validates. They are only worth having if regenerating them is reproducible,
//! and until M4 they were not: the instructions in the fixture README pointed
//! at `tramontana task`, which since M2 uses a real clock, real UUIDs, and a
//! SQLite profile. Every regeneration produced a different file, so nobody
//! regenerated them, so they silently fell behind the contracts they were
//! supposed to pin.
//!
//! This assembles the same application service over the deterministic fakes:
//! a fake clock, a sequential identifier source, in-memory storage. Running it
//! twice produces identical bytes, which is what makes a stale fixture a
//! visible diff instead of a judgement call.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use tramontana_core::contracts::{ApprovalDecisionKind, EventEnvelope, TaskId, TaskUpdate};
use tramontana_core::ports::{ApprovalStore, Approver, EventStore, ToolProvider};
use tramontana_core::services::{Providers, Repositories, StartTaskRequest, TaskService};
use tramontana_core::testing::{
    FakeClock, FakeModelProvider, FakeToolProvider, FakeWorker, InMemoryApprovalStore,
    InMemoryConfigurationRepository, InMemoryConversationRepository, InMemoryEventStore,
    InMemoryProjectRepository, InMemoryTaskRepository, ScriptedApprover, SequentialIdSource,
    assemble_tool_plane,
};

/// How long a fixture run may take before it is treated as hung.
///
/// Generous, because it is a deadlock guard rather than a schedule: nothing
/// here waits on a clock in the successful case.
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Write every committed contract fixture into `dir`.
///
/// # Errors
///
/// Fails when a task does not reach a terminal state, or when a file cannot be
/// written.
pub(crate) fn emit_fixtures(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting a runtime for fixture generation")?;

    let streams = runtime.block_on(async {
        Ok::<_, anyhow::Error>(vec![
            ("task-completed.ndjson", completed_stream().await?),
            ("task-cancelled.ndjson", cancelled_stream().await?),
        ])
    })?;

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating fixture directory {}", dir.display()))?;

    let mut written = Vec::new();
    for (name, events) in streams {
        let path = dir.join(name);
        let mut document = String::new();
        for event in &events {
            document.push_str(&serde_json::to_string(event)?);
            document.push('\n');
        }
        std::fs::write(&path, document).with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Assemble a service over deterministic fakes.
///
/// The chunk delay is a parameter because the two fixtures need opposite
/// things from it: the completed run wants no pause at all, and the cancelled
/// run wants a pause long enough that cancellation lands *inside* the stream
/// rather than after it.
fn service(chunk_delay_ms: u64) -> Result<Arc<TaskService>> {
    let ids = Arc::new(SequentialIdSource::new());
    let (registry, permissions) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new()) as Arc<dyn ToolProvider>],
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)) as Arc<dyn Approver>,
        Arc::clone(&ids) as Arc<dyn tramontana_core::ports::IdSource>,
    )
    .map_err(|error| anyhow!("could not register the fixture tools: {error}"))?;

    let events = Arc::new(InMemoryEventStore::new()) as Arc<dyn EventStore>;
    Ok(Arc::new(TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            // The task repository holds the store too: creating a task and its
            // first events is one transaction, and a fake that could not do
            // that would not be exercising the same path the product does.
            tasks: Arc::new(InMemoryTaskRepository::with_events(Arc::clone(&events))),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(tramontana_core::testing::InMemoryCheckpointRepository::new()),
        },
        Providers {
            model: Arc::new(FakeModelProvider::streaming_paced(
                ["Hello", ", ", "world"],
                chunk_delay_ms,
            )),
            tools: registry,
            permissions,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        ids,
    )))
}

/// A task that runs to completion.
async fn completed_stream() -> Result<Vec<EventEnvelope>> {
    let service = service(0)?;
    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "Say hello".to_owned(),
        })
        .await
        .map_err(|error| anyhow!("could not start the fixture task: {}", error.message))?;

    await_terminal(&service, &task_id).await
}

/// A task cancelled while the model is still streaming.
///
/// Cancellation is triggered by *observing* the durable `modelStarted` event
/// rather than by a timer. A timer would make the sequence at which
/// cancellation lands vary between machines, which is exactly the property the
/// old fixture README had to apologize for.
async fn cancelled_stream() -> Result<Vec<EventEnvelope>> {
    let service = service(5_000)?;
    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "Say hello".to_owned(),
        })
        .await
        .map_err(|error| anyhow!("could not start the fixture task: {}", error.message))?;

    let (_, mut updates) = service
        .subscribe_task_events(&task_id, 0)
        .await
        .map_err(|error| anyhow!("could not subscribe: {}", error.message))?;

    tokio::time::timeout(RUN_TIMEOUT, async {
        while let Ok(update) = updates.recv().await {
            if let TaskUpdate::Event { event } = update
                && matches!(
                    event.payload,
                    tramontana_core::contracts::EventPayload::ModelStarted { .. }
                )
            {
                return Ok::<(), anyhow::Error>(());
            }
        }
        Err(anyhow!("the task ended before the model started"))
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for the model to start"))??;

    service
        .cancel_task(&task_id)
        .await
        .map_err(|error| anyhow!("could not cancel: {}", error.message))?;

    await_terminal(&service, &task_id).await
}

/// Replay a task's events once its stream has been closed by an outcome event.
///
/// Waiting for a *terminal state* is one event too early: the state change to
/// `cancelled` is written before the `taskCancelled` event that carries the
/// outcome, so a reader that stops at the state sometimes captures a stream
/// missing its last line. `is_terminal_outcome` exists for exactly this
/// distinction (`CORE_CONTRACTS.md` §5), and the reproducibility test found
/// the difference within a single run.
async fn await_terminal(service: &TaskService, task_id: &TaskId) -> Result<Vec<EventEnvelope>> {
    let (replayed, mut updates) = service
        .subscribe_task_events(task_id, 0)
        .await
        .map_err(|error| anyhow!("could not subscribe: {}", error.message))?;

    let already_closed = replayed.iter().any(EventEnvelope::is_terminal_outcome);
    if !already_closed {
        tokio::time::timeout(RUN_TIMEOUT, async {
            while let Ok(update) = updates.recv().await {
                if let TaskUpdate::Event { event } = update
                    && event.is_terminal_outcome()
                {
                    return Ok::<(), anyhow::Error>(());
                }
            }
            Err(anyhow!("the update channel closed before an outcome event"))
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for a terminal outcome"))??;
    }

    service
        .replay_task_events(task_id, 0)
        .await
        .map_err(|error| anyhow!("could not replay: {}", error.message))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;

    /// Where the committed fixtures live.
    fn committed() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/fixtures/contracts/v1")
    }

    /// Regenerate into a directory of this test's own.
    ///
    /// Named per call site because these tests share a process: one label for
    /// all of them would have them deleting each other's output.
    fn regenerate(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tramontana-fixtures-{label}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        emit_fixtures(&dir).unwrap();
        dir
    }

    #[test]
    fn regenerating_twice_produces_identical_bytes() {
        // The property the old fixtures lacked. Without it a stale fixture is
        // a judgement call rather than a diff, which is how they fell three
        // milestones behind the contracts they were meant to pin.
        let first = regenerate("repeat-a");
        let second = regenerate("repeat-b");

        for name in ["task-completed.ndjson", "task-cancelled.ndjson"] {
            assert_eq!(
                std::fs::read_to_string(first.join(name)).unwrap(),
                std::fs::read_to_string(second.join(name)).unwrap(),
                "{name} is not reproducible"
            );
        }

        std::fs::remove_dir_all(&first).ok();
        std::fs::remove_dir_all(&second).ok();
    }

    #[test]
    fn the_committed_fixtures_are_current() {
        // A contract change that alters an event now fails here, in Rust,
        // rather than later in the TypeScript schema test with no instruction
        // for what to do about it. Regenerate with:
        //     cargo run -p xtask -- emit-fixtures
        let regenerated = regenerate("current");

        for name in ["task-completed.ndjson", "task-cancelled.ndjson"] {
            let fresh = std::fs::read_to_string(regenerated.join(name)).unwrap();
            let stored = std::fs::read_to_string(committed().join(name)).unwrap_or_default();
            assert_eq!(
                fresh, stored,
                "{name} is stale; run `cargo run -p xtask -- emit-fixtures`"
            );
        }

        std::fs::remove_dir_all(&regenerated).ok();
    }
}
