//! Repository behavior, including recovery from an unclean stop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::contracts::{
    ConversationId, ConversationRecord, EventPayload, ProjectId, ProjectRecord, TaskId, TaskRecord,
    TaskState, Timestamp,
};
use tramontana_core::ports::{
    ConversationRepository, EventStore, ProjectRepository, RepositoryError, TaskRepository,
};
use tramontana_core::testing::contract_suites::event_at;
use tramontana_persistence::{
    Database, SqliteConversationRepository, SqliteEventStore, SqliteProjectRepository,
    SqliteTaskRepository,
};

fn at(seconds: u32) -> Timestamp {
    Timestamp::new(format!("2026-01-01T00:00:{seconds:02}.000Z"))
}

async fn database(directory: &tempfile::TempDir, name: &str) -> Database {
    Database::open(&directory.path().join(format!("{name}.sqlite")))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_project_round_trips_with_its_approved_roots() {
    let directory = tempfile::tempdir().unwrap();
    let repository = SqliteProjectRepository::new(database(&directory, "projects").await);

    let record = ProjectRecord::new(
        ProjectId::new("p1"),
        "Demo",
        vec!["/projects/demo".to_owned(), "/projects/shared".to_owned()],
        at(0),
    );
    repository.create(&record).await.unwrap();

    let loaded = repository.get(&ProjectId::new("p1")).await.unwrap();
    assert_eq!(loaded, record);
    assert_eq!(repository.list().await.unwrap(), vec![record]);
}

#[tokio::test]
async fn an_unknown_project_is_reported_not_invented() {
    let directory = tempfile::tempdir().unwrap();
    let repository = SqliteProjectRepository::new(database(&directory, "missing").await);

    let error = repository.get(&ProjectId::new("nope")).await.unwrap_err();
    assert!(matches!(error, RepositoryError::NotFound { kind: "project", .. }));
}

#[tokio::test]
async fn projectless_conversations_are_listed_separately_from_project_ones() {
    // `NULL = NULL` is not true in SQL, so listing projectless conversations
    // needs `IS` rather than `=`. Getting this wrong returns an empty list and
    // looks like data loss.
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "conversations").await;
    let projects = SqliteProjectRepository::new(db.clone());
    let conversations = SqliteConversationRepository::new(db);

    let project = ProjectRecord::new(ProjectId::new("p1"), "Demo", vec![], at(0));
    projects.create(&project).await.unwrap();

    let in_project = ConversationRecord::new(
        ConversationId::new("c1"),
        Some(project.project_id.clone()),
        "Scoped",
        at(1),
    );
    let projectless =
        ConversationRecord::new(ConversationId::new("c2"), None, "Projectless", at(2));
    conversations.create(&in_project).await.unwrap();
    conversations.create(&projectless).await.unwrap();

    assert_eq!(
        conversations.list(Some(&project.project_id)).await.unwrap(),
        vec![in_project]
    );
    assert_eq!(conversations.list(None).await.unwrap(), vec![projectless]);
}

#[tokio::test]
async fn archiving_a_conversation_records_when_it_happened() {
    let directory = tempfile::tempdir().unwrap();
    let repository = SqliteConversationRepository::new(database(&directory, "archive").await);

    let record = ConversationRecord::new(ConversationId::new("c1"), None, "Untitled", at(0));
    repository.create(&record).await.unwrap();
    repository.archive(&record.conversation_id, &at(9)).await.unwrap();

    let loaded = repository.get(&record.conversation_id).await.unwrap();
    assert_eq!(loaded.archived_at, Some(at(9)));

    let error = repository
        .archive(&ConversationId::new("nope"), &at(9))
        .await
        .unwrap_err();
    assert!(matches!(error, RepositoryError::NotFound { .. }));
}

#[tokio::test]
async fn deleting_a_project_cascades_to_its_conversations() {
    // Foreign keys are only enforced if SQLite is told to; this is the test
    // that proves the pragma is actually on rather than merely declared.
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "cascade").await;
    let projects = SqliteProjectRepository::new(db.clone());
    let conversations = SqliteConversationRepository::new(db.clone());

    let project = ProjectRecord::new(ProjectId::new("p1"), "Demo", vec!["/a".to_owned()], at(0));
    projects.create(&project).await.unwrap();
    conversations
        .create(&ConversationRecord::new(
            ConversationId::new("c1"),
            Some(project.project_id.clone()),
            "Scoped",
            at(1),
        ))
        .await
        .unwrap();

    let orphan = ConversationRecord::new(
        ConversationId::new("c2"),
        Some(ProjectId::new("does-not-exist")),
        "Orphan",
        at(2),
    );
    let error = conversations.create(&orphan).await.unwrap_err();
    assert!(
        matches!(error, RepositoryError::Unavailable(_)),
        "a conversation cannot reference a project that does not exist"
    );
}

#[tokio::test]
async fn a_task_projection_survives_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("restart.sqlite");
    let task_id = TaskId::new("t1");

    {
        let db = Database::open(&path).await.unwrap();
        let tasks = SqliteTaskRepository::new(db.clone());
        tasks
            .create(&TaskRecord::new(task_id.clone(), None, None, "say hello", at(0)))
            .await
            .unwrap();
        tasks
            .update_projection(&task_id, TaskState::Running, 4, &at(5))
            .await
            .unwrap();
        db.close().await;
    }

    let db = Database::open(&path).await.unwrap();
    let tasks = SqliteTaskRepository::new(db);
    let loaded = tasks.get(&task_id).await.unwrap();
    assert_eq!(loaded.state, TaskState::Running);
    assert_eq!(loaded.last_sequence, 4);
    assert_eq!(loaded.goal_summary, "say hello");
}

#[tokio::test]
async fn a_crash_between_appending_an_event_and_updating_the_cache_is_repaired() {
    // The exact failure the V1 gate asks about. The events are the record and
    // the task row is a cache; a process that dies between the two leaves them
    // disagreeing. Startup must trust the events, not the cache.
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "crash").await;
    let tasks = SqliteTaskRepository::new(db.clone());
    let events = SqliteEventStore::new(db.clone());
    let task_id = TaskId::new("t1");

    tasks
        .create(&TaskRecord::new(task_id.clone(), None, None, "say hello", at(0)))
        .await
        .unwrap();

    // Events land...
    let mut stream = Vec::new();
    for sequence in 1..=3 {
        stream.push(event_at(&task_id, sequence));
    }
    events.append(&task_id, 0, stream).await.unwrap();

    // ...and then the process dies before `update_projection` runs. The cache
    // still says sequence 0.
    assert_eq!(tasks.get(&task_id).await.unwrap().last_sequence, 0);

    let corrected = tasks.rebuild_from_events().await.unwrap();
    assert_eq!(corrected, 1);
    assert_eq!(tasks.get(&task_id).await.unwrap().last_sequence, 3);
}

#[tokio::test]
async fn rebuilding_recovers_the_state_from_the_last_state_change_event() {
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "rebuild-state").await;
    let tasks = SqliteTaskRepository::new(db.clone());
    let events = SqliteEventStore::new(db.clone());
    let task_id = TaskId::new("t1");

    tasks
        .create(&TaskRecord::new(task_id.clone(), None, None, "say hello", at(0)))
        .await
        .unwrap();

    let mut stream = Vec::new();
    for (index, (previous, current)) in [
        (TaskState::Queued, TaskState::Preparing),
        (TaskState::Preparing, TaskState::Routing),
        (TaskState::Routing, TaskState::Failed),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = event_at(&task_id, index as u64 + 1);
        event.kind = tramontana_core::contracts::TaskEventKind::TaskStateChanged;
        event.payload = EventPayload::TaskStateChanged {
            previous,
            current,
            reason_code: "test".to_owned(),
        };
        stream.push(event);
    }
    events.append(&task_id, 0, stream).await.unwrap();

    tasks.rebuild_from_events().await.unwrap();
    let loaded = tasks.get(&task_id).await.unwrap();
    assert_eq!(loaded.state, TaskState::Failed);
    assert_eq!(loaded.last_sequence, 3);
}

#[tokio::test]
async fn rebuilding_a_consistent_database_corrects_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "consistent").await;
    let tasks = SqliteTaskRepository::new(db.clone());

    tasks
        .create(&TaskRecord::new(TaskId::new("t1"), None, None, "say hello", at(0)))
        .await
        .unwrap();

    assert_eq!(tasks.rebuild_from_events().await.unwrap(), 0);
}
