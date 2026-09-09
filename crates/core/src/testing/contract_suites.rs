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

use crate::contracts::{
    CheckpointId, CorrelationId, EventDraft, EventEnvelope, EventId, EventPayload, PendingAction,
    PendingActionKind, ReplaySafety, ResumeEligibility, TaskCheckpoint, TaskEventKind, TaskId,
    TaskState, Timestamp,
};
use crate::ports::{CheckpointRepository, EventStore, EventStoreError, RepositoryError};

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
