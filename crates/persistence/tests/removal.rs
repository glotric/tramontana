//! Removing a conversation or a project in one commit (ADR 0067).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::contracts::{
    ConversationId, ConversationRecord, ProjectId, ProjectRecord, TaskId, TaskRecord, Timestamp,
};
use tramontana_core::ports::{
    ConversationRepository, EventStore, ProjectRepository, RecordRemoval, RepositoryError,
    TaskRepository,
};
use tramontana_core::testing::contract_suites::event_at;
use tramontana_persistence::{
    Database, SqliteConversationRepository, SqliteEventStore, SqliteProjectRepository,
    SqliteRecordRemoval, SqliteTaskRepository,
};

fn at(seconds: u32) -> Timestamp {
    Timestamp::new(format!("2026-01-01T00:00:{seconds:02}.000Z"))
}

/// A project with a conversation holding `chat-task`, a conversation-less
/// `loose-task`, and an unrelated `other-task`, each with one event.
async fn filled(path: &std::path::Path) -> Database {
    let db = Database::open(path).await.unwrap();
    let project = ProjectId::new("project-1");
    SqliteProjectRepository::new(db.clone())
        .create(&ProjectRecord::new(
            project.clone(),
            "Project",
            vec!["/work/p".into()],
            at(0),
        ))
        .await
        .unwrap();
    let conversation = ConversationId::new("conversation-1");
    SqliteConversationRepository::new(db.clone())
        .create(&ConversationRecord::new(
            conversation.clone(),
            Some(project.clone()),
            "Chat",
            at(1),
        ))
        .await
        .unwrap();
    let tasks = SqliteTaskRepository::new(db.clone());
    let events = SqliteEventStore::new(db.clone());
    for (task, conversation, project) in [
        ("chat-task", Some(conversation), Some(project.clone())),
        ("loose-task", None, Some(project)),
        ("other-task", None, None),
    ] {
        let task = TaskId::new(task);
        tasks
            .create(&TaskRecord::new(
                task.clone(),
                conversation,
                project,
                "Goal",
                at(2),
            ))
            .await
            .unwrap();
        events
            .append(&task, 0, vec![event_at(&task, 1)])
            .await
            .unwrap();
    }
    db
}

async fn event_count(db: &Database, task: &str) -> usize {
    SqliteEventStore::new(db.clone())
        .replay(&TaskId::new(task), 0)
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn removing_a_conversation_takes_its_tasks_events_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let db = filled(&directory.path().join("p.sqlite")).await;
    SqliteRecordRemoval::new(db.clone())
        .remove_conversation(&ConversationId::new("conversation-1"))
        .await
        .unwrap();

    assert_eq!(event_count(&db, "chat-task").await, 0);
    assert!(
        SqliteTaskRepository::new(db.clone())
            .get(&TaskId::new("chat-task"))
            .await
            .is_err()
    );
    assert_eq!(event_count(&db, "loose-task").await, 1);
    assert_eq!(event_count(&db, "other-task").await, 1);
}

#[tokio::test]
async fn removing_a_project_takes_every_task_recorded_against_it() {
    let directory = tempfile::tempdir().unwrap();
    let db = filled(&directory.path().join("p.sqlite")).await;
    SqliteRecordRemoval::new(db.clone())
        .remove_project(&ProjectId::new("project-1"))
        .await
        .unwrap();

    assert_eq!(event_count(&db, "chat-task").await, 0);
    assert_eq!(event_count(&db, "loose-task").await, 0);
    assert_eq!(event_count(&db, "other-task").await, 1);
    let tasks = SqliteTaskRepository::new(db.clone());
    assert!(tasks.get(&TaskId::new("loose-task")).await.is_err());
    assert!(tasks.get(&TaskId::new("other-task")).await.is_ok());
    assert!(
        SqliteConversationRepository::new(db.clone())
            .get(&ConversationId::new("conversation-1"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_unknown_record_is_not_found_and_changes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let db = filled(&directory.path().join("p.sqlite")).await;
    let removal = SqliteRecordRemoval::new(db.clone());
    assert!(matches!(
        removal
            .remove_conversation(&ConversationId::new("nope"))
            .await,
        Err(RepositoryError::NotFound { .. })
    ));
    assert!(matches!(
        removal.remove_project(&ProjectId::new("nope")).await,
        Err(RepositoryError::NotFound { .. })
    ));
    for task in ["chat-task", "loose-task", "other-task"] {
        assert_eq!(event_count(&db, task).await, 1, "{task}");
    }
}

#[tokio::test]
async fn a_removal_that_fails_after_deleting_events_keeps_them() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("p.sqlite");
    let db = filled(&path).await;
    // Refuse the parent row's delete, which runs after the events' delete.
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse BEFORE DELETE ON conversation
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    assert!(matches!(
        SqliteRecordRemoval::new(db.clone())
            .remove_conversation(&ConversationId::new("conversation-1"))
            .await,
        Err(RepositoryError::Unavailable(_))
    ));
    assert_eq!(event_count(&db, "chat-task").await, 1);
    assert!(
        SqliteTaskRepository::new(db.clone())
            .get(&TaskId::new("chat-task"))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn the_sweep_removes_only_events_whose_task_is_gone() {
    let directory = tempfile::tempdir().unwrap();
    let db = filled(&directory.path().join("p.sqlite")).await;
    // What an interrupted removal before ADR 0067 left: a row without its
    // events deleted.
    SqliteTaskRepository::new(db.clone())
        .delete(&TaskId::new("other-task"))
        .await
        .unwrap();

    let removal = SqliteRecordRemoval::new(db.clone());
    assert_eq!(removal.remove_orphaned_events().await.unwrap(), 1);
    assert_eq!(event_count(&db, "other-task").await, 0);
    assert_eq!(event_count(&db, "chat-task").await, 1);
    assert_eq!(event_count(&db, "loose-task").await, 1);
    assert_eq!(removal.remove_orphaned_events().await.unwrap(), 0);
}
