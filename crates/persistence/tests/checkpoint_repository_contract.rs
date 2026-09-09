//! The SQLite `CheckpointRepository` runs the same suite as the fake.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::contracts::{TaskId, TaskRecord, Timestamp};
use tramontana_core::ports::{CheckpointRepository, TaskRepository};
use tramontana_core::testing::contract_suites::{
    CHECKPOINT_CONTRACT_TASKS, checkpoint_repository_contract,
};
use tramontana_persistence::{Database, SqliteCheckpointRepository, SqliteTaskRepository};

/// A fresh migrated database holding the tasks the suite checkpoints.
///
/// On disk rather than in memory, so this doubles as evidence that the
/// migration creating `task_checkpoint` runs. The task rows exist because the
/// schema makes a checkpoint belong to a task; that foreign key is the point,
/// so the harness satisfies it rather than the schema relaxing for a test.
async fn fresh_repository(
    directory: &tempfile::TempDir,
    name: &str,
) -> Arc<dyn CheckpointRepository> {
    let database = Database::open(&directory.path().join(format!("{name}.sqlite")))
        .await
        .unwrap();

    let tasks = SqliteTaskRepository::new(database.clone());
    for task_id in CHECKPOINT_CONTRACT_TASKS {
        tasks
            .create(&TaskRecord::new(
                TaskId::new(task_id),
                None,
                None,
                "a task with checkpoints",
                Timestamp::new("2026-01-01T00:00:00.000Z"),
            ))
            .await
            .unwrap();
    }

    Arc::new(SqliteCheckpointRepository::new(database)) as Arc<dyn CheckpointRepository>
}

#[tokio::test]
async fn sqlite_obeys_the_checkpoint_contract() {
    let directory = tempfile::tempdir().unwrap();
    let counter = std::sync::atomic::AtomicUsize::new(0);

    checkpoint_repository_contract(|| {
        let index = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let directory = &directory;
        async move { fresh_repository(directory, &format!("case-{index}")).await }
    })
    .await;
}

#[tokio::test]
async fn a_checkpoint_must_belong_to_a_task_that_exists() {
    // The foreign key is a correctness property, not tidiness: a checkpoint
    // naming a task nobody can open explains nothing and resumes nothing.
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("orphan.sqlite"))
        .await
        .unwrap();
    let repository = SqliteCheckpointRepository::new(database);

    let orphan = tramontana_core::testing::contract_suites::checkpoint_at(
        &TaskId::new("task-that-was-never-created"),
        1,
    );
    assert!(repository.save(&orphan).await.is_err());
}
