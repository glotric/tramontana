//! The shared `EventStore` contract suite.
//!
//! `TESTING_STRATEGY.md` §4 requires that every implementation of a stable port
//! obey one semantic suite. This is that suite for persistence. It is written
//! against the trait, not the fake, so M2's SQLite store runs the identical
//! cases by calling [`run_event_store_contract`] with its own constructor.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::contracts::{
    CorrelationId, EventDraft, EventEnvelope, EventId, EventPayload, TaskEventKind, TaskId,
    Timestamp,
};
use tramontana_core::ports::{EventStore, EventStoreError};
use tramontana_core::testing::InMemoryEventStore;

/// Build an event for `task_id` at `sequence`.
fn event_at(task_id: &TaskId, sequence: u64) -> EventEnvelope {
    EventDraft::user(
        TaskEventKind::TaskCompleted,
        EventPayload::TaskCompleted {
            summary: format!("event {sequence}"),
        },
    )
    .into_envelope(
        EventId::new(format!("event-{sequence}")),
        task_id.clone(),
        None,
        sequence,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
        CorrelationId::new("correlation-1"),
    )
}

/// Run the full contract against one implementation.
async fn run_event_store_contract(build: impl Fn() -> Arc<dyn EventStore>) {
    let task = TaskId::new("task-1");

    // An empty stream is at sequence 0, not an error.
    let store = build();
    assert_eq!(store.last_sequence(&task).await.unwrap(), 0);
    assert!(store.replay(&task, 0).await.unwrap().is_empty());

    // A first append expects 0 and yields sequence 1.
    let store = build();
    let last = store
        .append(&task, 0, vec![event_at(&task, 1)])
        .await
        .unwrap();
    assert_eq!(last, 1);
    assert_eq!(store.last_sequence(&task).await.unwrap(), 1);

    // A batch lands consecutively and atomically.
    let store = build();
    let last = store
        .append(&task, 0, vec![event_at(&task, 1), event_at(&task, 2)])
        .await
        .unwrap();
    assert_eq!(last, 2);
    let replayed = store.replay(&task, 0).await.unwrap();
    assert_eq!(
        replayed.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![1, 2]
    );

    // A stale expected sequence conflicts rather than interleaving. This is the
    // property that stops two writers corrupting one task's order.
    let store = build();
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
            actual: 1
        }
    );

    // A conflict leaves the stream untouched.
    assert_eq!(store.last_sequence(&task).await.unwrap(), 1);

    // Misnumbered events are rejected, and nothing partial is written.
    let store = build();
    let error = store
        .append(&task, 0, vec![event_at(&task, 1), event_at(&task, 3)])
        .await
        .unwrap_err();
    assert_eq!(
        error,
        EventStoreError::NonConsecutive {
            task_id: task.clone(),
            expected_first: 1
        }
    );
    assert_eq!(
        store.last_sequence(&task).await.unwrap(),
        0,
        "a rejected batch must not write its valid prefix"
    );

    // An event addressed to another task is rejected too.
    let store = build();
    let error = store
        .append(&task, 0, vec![event_at(&TaskId::new("other"), 1)])
        .await
        .unwrap_err();
    assert!(matches!(error, EventStoreError::NonConsecutive { .. }));

    // Replay is exclusive of `after_sequence`, which is what makes it usable
    // directly with the last sequence an interface already rendered.
    let store = build();
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
    let store = build();
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
}

#[tokio::test]
async fn in_memory_store_obeys_the_event_store_contract() {
    run_event_store_contract(|| Arc::new(InMemoryEventStore::new()) as Arc<dyn EventStore>).await;
}

#[tokio::test]
async fn concurrent_writers_cannot_interleave_into_one_stream() {
    // Two writers both believe the stream is at 0. Exactly one may win; the
    // other must be told the stream moved, not silently appended after it.
    let store: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let task = TaskId::new("task-1");

    let first = {
        let store = Arc::clone(&store);
        let task = task.clone();
        tokio::spawn(async move { store.append(&task, 0, vec![event_at(&task, 1)]).await })
    };
    let second = {
        let store = Arc::clone(&store);
        let task = task.clone();
        tokio::spawn(async move { store.append(&task, 0, vec![event_at(&task, 1)]).await })
    };

    let results = [first.await.unwrap(), second.await.unwrap()];
    let winners = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1, "exactly one writer may claim sequence 1");
    assert_eq!(store.last_sequence(&task).await.unwrap(), 1);
}
