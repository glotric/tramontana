//! Erasing a whole profile (ADR 0068).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use sqlx::Row as _;
use tramontana_core::contracts::{
    ConversationId, ConversationRecord, ProjectId, ProjectRecord, ProviderConfig, ProviderEndpoint,
    SecretRef, SecretStore, TaskId, TaskRecord, Timestamp,
};
use tramontana_core::ports::{
    ConversationRepository, EventStore, ProfileEraser, ProjectRepository,
    ProviderSettingsRepository, TaskRepository,
};
use tramontana_core::testing::contract_suites::event_at;
use tramontana_persistence::{
    Database, SqliteConversationRepository, SqliteEventStore, SqliteProfileEraser,
    SqliteProjectRepository, SqliteProviderSettingsRepository, SqliteTaskRepository,
};

/// Text that appears in the records and nowhere in the schema.
const MARKER: &str = "zebra-marmalade-7781";

fn at(seconds: u32) -> Timestamp {
    Timestamp::new(format!("2026-01-01T00:00:{seconds:02}.000Z"))
}

/// A profile holding a project, a conversation, a task with an event, and
/// an endpoint, each carrying the marker.
async fn filled(path: &std::path::Path) -> Database {
    let db = Database::open(path).await.unwrap();
    let project = ProjectId::new("project-1");
    SqliteProjectRepository::new(db.clone())
        .create(&ProjectRecord::new(
            project.clone(),
            format!("Project {MARKER}"),
            vec![format!("/work/{MARKER}")],
            at(0),
        ))
        .await
        .unwrap();
    let conversation = ConversationId::new("conversation-1");
    SqliteConversationRepository::new(db.clone())
        .create(&ConversationRecord::new(
            conversation.clone(),
            Some(project.clone()),
            format!("Chat about {MARKER}"),
            at(1),
        ))
        .await
        .unwrap();
    let task = TaskId::new("task-1");
    SqliteTaskRepository::new(db.clone())
        .create(&TaskRecord::new(
            task.clone(),
            Some(conversation),
            Some(project),
            format!("Summarize {MARKER}"),
            at(2),
        ))
        .await
        .unwrap();
    SqliteEventStore::new(db.clone())
        .append(&task, 0, vec![event_at(&task, 1)])
        .await
        .unwrap();
    SqliteProviderSettingsRepository::new(db.clone())
        .save(&ProviderEndpoint::new(
            "anthropic",
            format!("Endpoint {MARKER}"),
            ProviderConfig::Anthropic {
                base_url: "https://api.anthropic.com".to_owned(),
                model_id: "claude-test".to_owned(),
                credential: SecretRef {
                    key: "anthropic.apiKey".to_owned(),
                    store: SecretStore::Platform,
                },
            },
            at(3),
        ))
        .await
        .unwrap();
    db
}

/// How many rows each of the profile's own tables holds.
async fn row_counts(path: &std::path::Path) -> Vec<(String, i64)> {
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let tables: Vec<String> = sqlx::query(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .iter()
    .map(|row| row.get::<String, _>("name"))
    .collect();
    let mut counts = Vec::new();
    for table in tables {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM \"{table}\""
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        counts.push((table, count));
    }
    pool.close().await;
    counts
}

/// Whether the marker can be read anywhere in the database's files.
fn marker_on_disk(path: &std::path::Path) -> bool {
    ["", "-wal", "-shm"].iter().any(|suffix| {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        std::fs::read(file).is_ok_and(|bytes| {
            bytes
                .windows(MARKER.len())
                .any(|window| window == MARKER.as_bytes())
        })
    })
}

#[tokio::test]
async fn every_record_is_removed_and_the_profile_opens_as_new() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profile.sqlite");
    let db = filled(&path).await;
    assert!(
        row_counts(&path).await.iter().any(|(_, count)| *count > 0),
        "the fixture wrote nothing"
    );

    SqliteProfileEraser::new(db.clone())
        .erase_everything()
        .await
        .unwrap();

    let left: Vec<_> = row_counts(&path)
        .await
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect();
    assert!(left.is_empty(), "rows survived: {left:?}");
    // Still migrated and usable: a new project can be opened at once.
    SqliteProjectRepository::new(db.clone())
        .create(&ProjectRecord::new(
            ProjectId::new("after"),
            "After",
            vec!["/work/after".to_owned()],
            at(9),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn nothing_erased_can_be_read_back_from_the_files() {
    // A deleted row stays in the file's free pages and in the write-ahead
    // log until something overwrites it. "Deleted" has to be true of the
    // files, not only of the queries.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profile.sqlite");
    let db = filled(&path).await;
    assert!(
        marker_on_disk(&path),
        "the fixture's text never reached disk"
    );

    SqliteProfileEraser::new(db.clone())
        .erase_everything()
        .await
        .unwrap();

    assert!(!marker_on_disk(&path), "erased text is still in the files");
}
