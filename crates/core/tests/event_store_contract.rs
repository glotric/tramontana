//! The in-memory `EventStore` runs the shared contract suite.
//!
//! The suite itself lives in `tramontana_core::testing::contract_suites` so
//! that `tramontana-persistence` runs the identical cases against SQLite. A
//! divergence between the two is a defect in one implementation, never a
//! difference in what was tested.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::contracts::TaskId;
use tramontana_core::ports::EventStore;
use tramontana_core::testing::InMemoryEventStore;
use tramontana_core::testing::contract_suites::{event_at, event_store_contract};

#[tokio::test]
async fn in_memory_store_obeys_the_event_store_contract() {
    event_store_contract(|| async { Arc::new(InMemoryEventStore::new()) as Arc<dyn EventStore> })
        .await;
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
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "exactly one writer may claim sequence 1"
    );
    assert_eq!(store.last_sequence(&task).await.unwrap(), 1);
}
