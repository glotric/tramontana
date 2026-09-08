//! The SQLite `EventStore` runs the same shared contract suite as the fake.
//!
//! This is the point of the suite: `crates/core` and this crate exercise
//! identical cases, so the in-memory store used by every other test is a
//! faithful stand-in for the real one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::contracts::TaskId;
use tramontana_core::ports::EventStore;
use tramontana_core::testing::contract_suites::{event_at, event_store_contract};
use tramontana_persistence::{Database, SqliteEventStore};

/// A fresh migrated database on disk.
///
/// On disk rather than in memory: this suite doubles as the evidence that
/// migrations run and that the file-backed path works, which an in-memory
/// database would not prove.
async fn fresh_store(directory: &tempfile::TempDir, name: &str) -> Arc<dyn EventStore> {
    let database = Database::open(&directory.path().join(format!("{name}.sqlite")))
        .await
        .unwrap();
    Arc::new(SqliteEventStore::new(database)) as Arc<dyn EventStore>
}

#[tokio::test]
async fn sqlite_store_obeys_the_event_store_contract() {
    let directory = tempfile::tempdir().unwrap();
    let counter = std::sync::atomic::AtomicUsize::new(0);

    event_store_contract(|| {
        let index = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let directory = &directory;
        async move { fresh_store(directory, &format!("case-{index}")).await }
    })
    .await;
}

#[tokio::test]
async fn concurrent_writers_cannot_interleave_into_one_stream() {
    let directory = tempfile::tempdir().unwrap();
    let store = fresh_store(&directory, "concurrent").await;
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

#[tokio::test]
async fn a_reopened_database_still_has_its_events() {
    // The whole point of persistence: the record outlives the process that
    // wrote it. M1 could not make this claim.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("restart.sqlite");
    let task = TaskId::new("task-1");

    {
        let database = Database::open(&path).await.unwrap();
        let store = SqliteEventStore::new(database.clone());
        store
            .append(&task, 0, vec![event_at(&task, 1), event_at(&task, 2)])
            .await
            .unwrap();
        database.close().await;
    }

    let database = Database::open(&path).await.unwrap();
    let store = SqliteEventStore::new(database);
    assert_eq!(store.last_sequence(&task).await.unwrap(), 2);
    let replayed = store.replay(&task, 0).await.unwrap();
    assert_eq!(
        replayed.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn migrating_an_already_migrated_database_is_a_no_op() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("idempotent.sqlite");

    for _ in 0..3 {
        Database::open(&path).await.unwrap().close().await;
    }

    let database = Database::open(&path).await.unwrap();
    let store = SqliteEventStore::new(database);
    assert_eq!(
        store.last_sequence(&TaskId::new("task-1")).await.unwrap(),
        0
    );
}
