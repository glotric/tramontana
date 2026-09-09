//! Reusable contract suites.
//!
//! `TESTING_STRATEGY.md` §4 requires that every implementation of a stable port
//! obey one shared semantic suite. These are written against the trait, never
//! against a particular implementation, so an in-memory fake and a real
//! database run identical cases and any divergence is a defect in one of them
//! rather than a difference in what was tested.
//!
//! They live in the library rather than in a test file because the
//! implementations they verify live in other crates.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::contracts::{
    CheckpointId, CorrelationId, EventDraft, EventEnvelope, EventId, EventPayload, PendingAction,
    PendingActionKind, ReplaySafety, ResumeEligibility, TaskCheckpoint, TaskEventKind, TaskId,
    TaskState, Timestamp,
};
use crate::contracts::{ModelEvent, ModelRequest, StopReason};
use crate::ports::{
    CheckpointRepository, EventStore, EventStoreError, ModelProvider, RepositoryError,
};

/// Build an event for `task_id` at `sequence`.
#[must_use]
pub fn event_at(task_id: &TaskId, sequence: u64) -> EventEnvelope {
    EventDraft::user(
        TaskEventKind::TaskCompleted,
        EventPayload::TaskCompleted {
            summary: format!("event {sequence}"),
        },
    )
    .into_envelope(
        EventId::new(format!("event-{task_id}-{sequence}")),
        task_id.clone(),
        None,
        sequence,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
        CorrelationId::new("correlation-1"),
    )
}

/// Assert that `build` produces a store obeying the full `EventStore` contract.
///
/// `build` must return a *fresh, empty* store each time; several cases depend
/// on starting from nothing.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn event_store_contract<F, Fut>(build: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn EventStore>>,
{
    let task = TaskId::new("task-1");

    // An empty stream is at sequence 0, not an error.
    let store = build().await;
    assert_eq!(store.last_sequence(&task).await.unwrap(), 0);
    assert!(store.replay(&task, 0).await.unwrap().is_empty());

    // A first append expects 0 and yields sequence 1.
    let store = build().await;
    assert_eq!(
        store
            .append(&task, 0, vec![event_at(&task, 1)])
            .await
            .unwrap(),
        1
    );
    assert_eq!(store.last_sequence(&task).await.unwrap(), 1);

    // A batch lands consecutively.
    let store = build().await;
    assert_eq!(
        store
            .append(&task, 0, vec![event_at(&task, 1), event_at(&task, 2)])
            .await
            .unwrap(),
        2
    );
    let replayed = store.replay(&task, 0).await.unwrap();
    assert_eq!(
        replayed.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![1, 2]
    );

    // Round trip: what comes back is what went in.
    let original = event_at(&task, 1);
    let store = build().await;
    store
        .append(&task, 0, vec![original.clone()])
        .await
        .unwrap();
    let round_tripped = store.replay(&task, 0).await.unwrap();
    assert_eq!(round_tripped, vec![original]);

    // A stale expected sequence conflicts rather than interleaving. This is
    // what stops two writers corrupting one task's order.
    let store = build().await;
    store
        .append(&task, 0, vec![event_at(&task, 1)])
        .await
        .unwrap();
    let error = store
        .append(&task, 0, vec![event_at(&task, 1)])
        .await
        .unwrap_err();
    assert_eq!(
        error,
        EventStoreError::SequenceConflict {
            task_id: task.clone(),
            expected: 0,
            actual: 1,
        }
    );
    assert_eq!(
        store.last_sequence(&task).await.unwrap(),
        1,
        "a conflict must leave the stream untouched"
    );

    // Misnumbered events are rejected, and nothing partial is written.
    let store = build().await;
    let error = store
        .append(&task, 0, vec![event_at(&task, 1), event_at(&task, 3)])
        .await
        .unwrap_err();
    assert_eq!(
        error,
        EventStoreError::NonConsecutive {
            task_id: task.clone(),
            expected_first: 1,
        }
    );
    assert_eq!(
        store.last_sequence(&task).await.unwrap(),
        0,
        "a rejected batch must not write its valid prefix"
    );

    // An event addressed to another task is rejected.
    let store = build().await;
    let error = store
        .append(&task, 0, vec![event_at(&TaskId::new("other"), 1)])
        .await
        .unwrap_err();
    assert!(matches!(error, EventStoreError::NonConsecutive { .. }));

    // Replay is exclusive of `after_sequence`, so it can be called directly
    // with the last sequence an interface already rendered.
    let store = build().await;
    store
        .append(
            &task,
            0,
            vec![event_at(&task, 1), event_at(&task, 2), event_at(&task, 3)],
        )
        .await
        .unwrap();
    let tail = store.replay(&task, 1).await.unwrap();
    assert_eq!(
        tail.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(store.replay(&task, 3).await.unwrap().is_empty());

    // Streams are isolated per task.
    let store = build().await;
    let other = TaskId::new("task-2");
    store
        .append(&task, 0, vec![event_at(&task, 1)])
        .await
        .unwrap();
    assert_eq!(store.last_sequence(&other).await.unwrap(), 0);
    store
        .append(&other, 0, vec![event_at(&other, 1)])
        .await
        .unwrap();
    assert_eq!(store.replay(&task, 0).await.unwrap().len(), 1);
    assert_eq!(store.replay(&other, 0).await.unwrap().len(), 1);

    // Appending after a successful append continues the stream.
    let store = build().await;
    store
        .append(&task, 0, vec![event_at(&task, 1)])
        .await
        .unwrap();
    store
        .append(&task, 1, vec![event_at(&task, 2)])
        .await
        .unwrap();
    assert_eq!(store.last_sequence(&task).await.unwrap(), 2);
}

/// The tasks [`checkpoint_repository_contract`] writes checkpoints for.
///
/// A durable store may require a checkpoint to belong to a real task, so a
/// harness for one needs to know which task rows to create first. Exported so
/// that requirement does not become a duplicated literal in every harness.
pub const CHECKPOINT_CONTRACT_TASKS: [&str; 2] = ["task-1", "task-2"];

/// Build a checkpoint for `task_id` at `sequence`.
#[must_use]
pub fn checkpoint_at(task_id: &TaskId, sequence: u64) -> TaskCheckpoint {
    let mut checkpoint = TaskCheckpoint::new(
        CheckpointId::new(format!("checkpoint-{task_id}-{sequence}")),
        task_id.clone(),
        sequence,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
        TaskState::Interrupted,
        ResumeEligibility::same_worker("checkpoint.derived"),
    );
    checkpoint.completed_step_ids = vec![format!("invocation-{sequence}")];
    checkpoint.pending_action = Some(PendingAction {
        kind: PendingActionKind::Tool,
        reference_id: format!("invocation-{}", sequence + 1),
        replay_safety: ReplaySafety::Unknown,
        display_summary: "a tool was in flight".to_owned(),
    });
    checkpoint
}

/// Assert that `build` produces a repository obeying the checkpoint contract.
///
/// `build` must return a *fresh, empty* repository each time.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn checkpoint_repository_contract<F, Fut>(build: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn CheckpointRepository>>,
{
    let [task, other] = CHECKPOINT_CONTRACT_TASKS.map(TaskId::new);

    // A task with no checkpoints has no latest one, which is not an error.
    let repository = build().await;
    assert_eq!(repository.latest(&task).await.unwrap(), None);
    assert!(repository.list(&task).await.unwrap().is_empty());

    // A saved checkpoint round-trips whole, pending action included.
    let repository = build().await;
    let saved = checkpoint_at(&task, 4);
    repository.save(&saved).await.unwrap();
    let read = repository.get(&saved.checkpoint_id).await.unwrap();
    assert_eq!(read, saved, "a checkpoint must survive storage unchanged");

    // The latest is the highest sequence, not the most recently written.
    let repository = build().await;
    for sequence in [9, 2, 6] {
        repository
            .save(&checkpoint_at(&task, sequence))
            .await
            .unwrap();
    }
    assert_eq!(
        repository
            .latest(&task)
            .await
            .unwrap()
            .unwrap()
            .event_sequence,
        9
    );
    let sequences: Vec<u64> = repository
        .list(&task)
        .await
        .unwrap()
        .iter()
        .map(|checkpoint| checkpoint.event_sequence)
        .collect();
    assert_eq!(sequences, vec![2, 6, 9], "list is oldest first");

    // Checkpoints do not leak between tasks.
    let repository = build().await;
    repository.save(&checkpoint_at(&task, 1)).await.unwrap();
    repository.save(&checkpoint_at(&other, 5)).await.unwrap();
    assert_eq!(repository.list(&task).await.unwrap().len(), 1);
    assert_eq!(
        repository
            .latest(&other)
            .await
            .unwrap()
            .unwrap()
            .event_sequence,
        5
    );

    // One point in a stream has one checkpoint.
    let repository = build().await;
    repository.save(&checkpoint_at(&task, 3)).await.unwrap();
    let mut duplicate = checkpoint_at(&task, 3);
    duplicate.checkpoint_id = CheckpointId::new("checkpoint-duplicate");
    match repository.save(&duplicate).await {
        Err(RepositoryError::Conflict { kind, .. }) => assert_eq!(kind, "checkpoint"),
        other => panic!("a second checkpoint at one sequence must conflict, got {other:?}"),
    }

    // An unknown identifier is not found rather than empty.
    let repository = build().await;
    match repository.get(&CheckpointId::new("absent")).await {
        Err(RepositoryError::NotFound { kind, .. }) => assert_eq!(kind, "checkpoint"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// What a provider should be configured to do for one contract case.
///
/// The suite has to drive an adapter that talks to a real endpoint as well as
/// an in-process fake, so it asks for a *behavior* rather than constructing a
/// provider itself. A harness translates each scenario into whatever its
/// implementation needs — a script for a fake, a stubbed response for an HTTP
/// adapter.
///
/// Deliberately *not* `#[non_exhaustive]`: adding a case should break every
/// harness until it says what its implementation does, because a scenario
/// silently unhandled is a provider silently untested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderScenario {
    /// Stream these chunks in order, then finish normally.
    Streams(Vec<String>),
    /// Fail without producing any content.
    Fails,
    /// Produce content slowly enough that a cancellation lands mid-stream.
    Slow,
}

/// The request the suite sends. Providers must not depend on its content.
#[must_use]
pub fn model_request(request_id: &str) -> ModelRequest {
    use crate::contracts::{MessageId, MessageRole, NormalizedMessage, TaskId, Timestamp};

    ModelRequest::new(
        request_id,
        TaskId::new("task-1"),
        vec![NormalizedMessage::text(
            MessageId::new("message-1"),
            None,
            MessageRole::User,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
            "Say hello",
        )],
        "context-1",
    )
}

/// Drain a stream, returning everything it produced.
async fn drain(receiver: &mut tokio::sync::mpsc::Receiver<ModelEvent>) -> Vec<ModelEvent> {
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }
    events
}

/// Assert that `build` produces a provider obeying the `ModelProvider` contract.
///
/// `CORE_CONTRACTS.md` §8. The property that matters most is the last one:
/// **exactly one terminal event ends a stream**, including when cancellation
/// arrives while the underlying request is still in flight. A provider that
/// drops its stream on cancel, or emits both a `completed` and a `failed`,
/// leaves the core unable to say what happened — and the core commits the
/// coalesced answer on that signal.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn model_provider_contract<F, Fut>(build: F)
where
    F: Fn(ProviderScenario) -> Fut,
    Fut: Future<Output = Arc<dyn ModelProvider>>,
{
    // A provider reports models under its own identity, so a routing decision
    // naming a provider can be resolved back to one.
    let provider = build(ProviderScenario::Streams(vec!["hi".into()])).await;
    let models = provider.list_models().await;
    assert!(
        !models.is_empty(),
        "a provider must offer at least one model"
    );
    for model in &models {
        assert_eq!(
            model.provider_id,
            provider.id().to_string(),
            "a model claims a provider other than the one offering it"
        );
        assert!(!model.model_id.is_empty());
    }

    // A successful stream: content, then exactly one terminal event, then the
    // receiver closes.
    let provider = build(ProviderScenario::Streams(vec![
        "Hello".into(),
        ", ".into(),
        "world".into(),
    ]))
    .await;
    let mut stream = provider
        .stream(model_request("request-1"), CancellationToken::new())
        .await
        .expect("a scripted success must start");
    let events = drain(&mut stream).await;

    let terminal: Vec<&ModelEvent> = events.iter().filter(|event| event.is_terminal()).collect();
    assert_eq!(
        terminal.len(),
        1,
        "a stream must end with exactly one terminal event, got {events:?}"
    );
    assert!(
        events.last().is_some_and(ModelEvent::is_terminal),
        "the terminal event must be last"
    );
    assert!(
        matches!(
            terminal[0],
            ModelEvent::Completed {
                stop_reason: StopReason::Stop
            }
        ),
        "a scripted success must complete with `stop`, got {:?}",
        terminal[0]
    );

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "Hello, world",
        "the content did not survive streaming"
    );

    // Every delta names the stream it belongs to, and offsets never go
    // backwards — a subscriber orders on them.
    let mut previous: Option<u64> = None;
    for event in &events {
        if let ModelEvent::TextDelta {
            stream_id, offset, ..
        } = event
        {
            assert_eq!(stream_id, "request-1", "a delta named a different stream");
            if let Some(previous) = previous {
                assert!(*offset > previous, "offsets went backwards: {events:?}");
            }
            previous = Some(*offset);
        }
    }

    // A failure is a normalized envelope, either as a terminal event or as a
    // refusal to start. Both are legitimate; producing neither is not.
    let provider = build(ProviderScenario::Fails).await;
    match provider
        .stream(model_request("request-2"), CancellationToken::new())
        .await
    {
        Ok(mut stream) => {
            let events = drain(&mut stream).await;
            let terminal: Vec<&ModelEvent> =
                events.iter().filter(|event| event.is_terminal()).collect();
            assert_eq!(terminal.len(), 1, "a failing stream must still end once");
            assert!(
                matches!(terminal[0], ModelEvent::Failed { .. }),
                "a scripted failure ended as {:?}",
                terminal[0]
            );
        }
        Err(error) => assert!(
            !error.message.is_empty(),
            "a refusal must say something a person can act on"
        ),
    }

    // Cancellation mid-stream converges to exactly one terminal event rather
    // than the stream simply stopping.
    let provider = build(ProviderScenario::Slow).await;
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(model_request("request-3"), cancel.clone())
        .await
        .expect("a slow stream must start");
    cancel.cancel();

    let events = tokio::time::timeout(std::time::Duration::from_secs(10), drain(&mut stream))
        .await
        .expect("a cancelled stream must converge, not hang");
    let terminal: Vec<&ModelEvent> = events.iter().filter(|event| event.is_terminal()).collect();
    assert_eq!(
        terminal.len(),
        1,
        "a cancelled stream must end with exactly one terminal event, got {events:?}"
    );

    // Cancelled before anything was requested: still one terminal event.
    let provider = build(ProviderScenario::Slow).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    if let Ok(mut stream) = provider.stream(model_request("request-4"), cancel).await {
        let events = tokio::time::timeout(std::time::Duration::from_secs(10), drain(&mut stream))
            .await
            .expect("a pre-cancelled stream must converge, not hang");
        assert_eq!(
            events.iter().filter(|event| event.is_terminal()).count(),
            1,
            "a pre-cancelled stream must still end exactly once, got {events:?}"
        );
    }
}
