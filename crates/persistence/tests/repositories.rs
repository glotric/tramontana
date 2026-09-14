//! Repository behavior, including recovery from an unclean stop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::contracts::{
    ConfigurationKind, ConversationId, ConversationRecord, EventPayload, ProjectId, ProjectRecord,
    TaskId, TaskRecord, TaskState, Timestamp,
};
use tramontana_core::ports::{
    ConfigurationRepository, ConversationRepository, EventStore, ProjectRepository,
    RepositoryError, TaskRepository,
};
use tramontana_core::testing::contract_suites::event_at;
use tramontana_persistence::{
    Database, SqliteConfigurationRepository, SqliteConversationRepository, SqliteEventStore,
    SqliteProjectRepository, SqliteTaskRepository,
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
async fn the_committed_v1_fixture_migrates_to_the_current_schema() {
    use sqlx::{Connection, Executor};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("migrate-v1.sqlite");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    connection
        .execute(sqlx::raw_sql(include_str!(
            "fixtures/database/v1/schema.sql"
        )))
        .await
        .unwrap();
    connection.close().await.unwrap();

    let db = Database::open(&path).await.unwrap();
    let project_id = ProjectId::new("fixture-project");
    let project = SqliteProjectRepository::new(db.clone())
        .get(&project_id)
        .await
        .unwrap();
    assert_eq!(project.name, "Fixture project");
    let existing_task = SqliteTaskRepository::new(db.clone())
        .get(&TaskId::new("fixture-existing-task"))
        .await
        .unwrap();
    assert_eq!(existing_task.goal_summary, "Existing fixture goal");
    assert_eq!(existing_task.last_sequence, 1);
    let existing_events = SqliteEventStore::new(db.clone())
        .replay(&existing_task.task_id, 0)
        .await
        .unwrap();
    assert_eq!(existing_events.len(), 1);

    let manifest = tramontana_core::config::resolve(Vec::new(), at(1));
    let configurations = SqliteConfigurationRepository::new(db.clone());
    configurations.save(&project_id, &manifest).await.unwrap();
    assert_eq!(
        configurations.latest(&project_id).await.unwrap(),
        Some(manifest)
    );

    // Migration 0002 also added the task manifest column.
    let task = TaskRecord::new(
        TaskId::new("fixture-task"),
        None,
        Some(project_id),
        "fixture goal",
        at(2),
    )
    .with_configuration_manifest(
        configurations
            .latest(&ProjectId::new("fixture-project"))
            .await
            .unwrap()
            .unwrap()
            .manifest_hash,
    );
    SqliteTaskRepository::new(db).create(&task).await.unwrap();
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
    assert!(matches!(
        error,
        RepositoryError::NotFound {
            kind: "project",
            ..
        }
    ));
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
    repository
        .archive(&record.conversation_id, &at(9))
        .await
        .unwrap();

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
            .create(&TaskRecord::new(
                task_id.clone(),
                None,
                None,
                "say hello",
                at(0),
            ))
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
        .create(&TaskRecord::new(
            task_id.clone(),
            None,
            None,
            "say hello",
            at(0),
        ))
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
        .create(&TaskRecord::new(
            task_id.clone(),
            None,
            None,
            "say hello",
            at(0),
        ))
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
        .create(&TaskRecord::new(
            TaskId::new("t1"),
            None,
            None,
            "say hello",
            at(0),
        ))
        .await
        .unwrap();

    assert_eq!(tasks.rebuild_from_events().await.unwrap(), 0);
}

#[tokio::test]
async fn task_and_initial_events_commit_as_one_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "atomic-task-create").await;
    let tasks = SqliteTaskRepository::new(db.clone());
    let events = SqliteEventStore::new(db);

    // Occupy event-1 in another stream. The task insert succeeds first, then
    // the duplicate event ID forces the event insert to fail. The transaction
    // must roll the task row back as well.
    let occupied_task = TaskId::new("occupied");
    let occupied_event = event_at(&occupied_task, 1);
    let occupied_event_id = occupied_event.event_id.clone();
    events
        .append(&occupied_task, 0, vec![occupied_event])
        .await
        .unwrap();

    let task_id = TaskId::new("new-task");
    let record = TaskRecord::new(task_id.clone(), None, None, "goal", at(0));
    let mut event = event_at(&task_id, 1);
    event.event_id = occupied_event_id;
    event.kind = tramontana_core::contracts::TaskEventKind::TaskCreated;
    event.payload = EventPayload::TaskCreated {
        goal_summary: "goal".to_owned(),
        conversation_id: None,
    };
    let error = tasks
        .create_with_initial_events(&record, &[event])
        .await
        .unwrap_err();
    assert!(matches!(error, RepositoryError::Conflict { .. }));
    assert!(matches!(
        tasks.get(&task_id).await.unwrap_err(),
        RepositoryError::NotFound { .. }
    ));
    assert_eq!(events.last_sequence(&task_id).await.unwrap(), 0);
}

#[tokio::test]
async fn what_was_redacted_from_an_instruction_file_survives_a_round_trip() {
    // The hash and byte count in this table describe the *redacted* text. The
    // codes say which rules fired, and a manifest that lost them on the way to
    // storage would leave a hash that quietly differs from the file on disk
    // with nothing to explain why. Synthetic value.
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "redactions").await;
    let projects = SqliteProjectRepository::new(db.clone());
    let configurations = SqliteConfigurationRepository::new(db);
    let project_id = ProjectId::new("p1");
    projects
        .create(&ProjectRecord::new(
            project_id.clone(),
            "p1",
            vec!["/projects/p1".to_owned()],
            at(0),
        ))
        .await
        .unwrap();

    let raw = b"# Rules\n\nclient_secret: s3cr3t-value\n";
    let manifest = tramontana_core::config::resolve(
        vec![tramontana_core::config::RawSource {
            relative_path: "AGENTS.md".to_owned(),
            kind: ConfigurationKind::RootAgents,
            content: raw.to_vec(),
        }],
        at(1),
    );
    assert!(
        !manifest.sources[0].redactions.is_empty(),
        "nothing redacted"
    );

    configurations.save(&project_id, &manifest).await.unwrap();
    let loaded = configurations.latest(&project_id).await.unwrap().unwrap();

    assert_eq!(loaded.sources[0].redactions, manifest.sources[0].redactions);
    assert_eq!(
        loaded.sources[0].content_hash,
        manifest.sources[0].content_hash
    );
    assert_eq!(loaded.sources[0].byte_count, manifest.sources[0].byte_count);
    assert!(
        loaded.sources[0].byte_count < raw.len() as u64,
        "the stored byte count describes the file rather than what may be released"
    );
}

#[tokio::test]
async fn configuration_manifests_round_trip_and_are_scoped_by_project() {
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory, "configuration").await;
    let projects = SqliteProjectRepository::new(db.clone());
    let configurations = SqliteConfigurationRepository::new(db);

    for project_id in ["p1", "p2"] {
        projects
            .create(&ProjectRecord::new(
                ProjectId::new(project_id),
                project_id,
                vec![format!("/projects/{project_id}")],
                at(0),
            ))
            .await
            .unwrap();
    }

    let manifest = tramontana_core::config::resolve(
        vec![tramontana_core::config::RawSource {
            relative_path: "AGENTS.md".to_owned(),
            kind: ConfigurationKind::RootAgents,
            content: b"# Instructions".to_vec(),
        }],
        at(1),
    );

    // `source_id` is project-scoped. The same portable relative path in two
    // projects must not collide in the database.
    configurations
        .save(&ProjectId::new("p1"), &manifest)
        .await
        .unwrap();
    configurations
        .save(&ProjectId::new("p2"), &manifest)
        .await
        .unwrap();

    assert_eq!(
        configurations
            .get(&ProjectId::new("p1"), &manifest.manifest_hash)
            .await
            .unwrap(),
        manifest
    );
    assert_eq!(
        configurations.latest(&ProjectId::new("p2")).await.unwrap(),
        Some(manifest)
    );
}

#[tokio::test]
async fn provider_endpoints_round_trip_and_never_hold_a_credential() {
    use tramontana_core::contracts::{
        ModelLocation, ProviderConfig, ProviderEndpoint, SecretRef, SecretStore,
    };
    use tramontana_core::ports::ProviderSettingsRepository;
    use tramontana_persistence::SqliteProviderSettingsRepository;

    let directory = tempfile::tempdir().unwrap();
    let settings = SqliteProviderSettingsRepository::new(database(&directory, "providers").await);

    assert!(settings.list().await.unwrap().is_empty());

    let local = ProviderEndpoint::new(
        "local-llama",
        "Local llama",
        ProviderConfig::LlamaServer {
            binary_path: "/opt/llama-server".to_owned(),
            model_path: "/models/m.gguf".to_owned(),
            model_id: "m".to_owned(),
            extra_args: vec!["--ctx-size".to_owned(), "8192".to_owned()],
        },
        at(1),
    );
    let cloud = ProviderEndpoint::new(
        "anthropic",
        "Anthropic",
        ProviderConfig::Anthropic {
            base_url: "https://api.anthropic.com".to_owned(),
            model_id: "claude-test".to_owned(),
            credential: SecretRef {
                key: "anthropic.apiKey".to_owned(),
                store: SecretStore::Platform,
            },
        },
        at(2),
    );
    settings.save(&local).await.unwrap();
    settings.save(&cloud).await.unwrap();

    // Ordered, so two runs show the same thing.
    let listed = settings.list().await.unwrap();
    assert_eq!(listed, vec![local.clone(), cloud.clone()]);
    assert!(listed.iter().all(|endpoint| !endpoint.supports_tool_calls));

    // Location is derived on the way out, not stored and read back.
    assert_eq!(listed[0].location(), ModelLocation::Local);
    assert_eq!(listed[1].location(), ModelLocation::Cloud);

    // Saving the same identifier edits rather than duplicates.
    let mut edited = cloud.clone();
    edited.supports_tool_calls = true;
    edited.enabled = false;
    edited.display_name = "Anthropic (off)".to_owned();
    settings.save(&edited).await.unwrap();
    let listed = settings.list().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(!listed[1].enabled);
    assert!(listed[1].supports_tool_calls);
    assert_eq!(listed[1].display_name, "Anthropic (off)");

    // Only the reference is on disk, never a value — there is nowhere in the
    // type for one, and this asserts the row agrees.
    // Read the file directly rather than through the repository, so this
    // asserts what is on disk rather than what the type round-trips. The
    // pool stays private: a test is not a reason to widen an interface.
    // Every file the database spans, not just the main one: in WAL mode a
    // recent write lives in `-wal` and reading only `.sqlite` would find
    // nothing and pass an "is the secret absent?" check for the wrong reason.
    let mut on_disk = String::new();
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("providers.sqlite"))
        {
            on_disk.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    assert!(
        on_disk.contains("anthropic.apiKey"),
        "the reference is not stored"
    );
    assert!(
        !on_disk.contains("sk-ant-"),
        "a credential value reached the database file"
    );

    assert!(settings.delete("anthropic").await.unwrap());
    assert!(!settings.delete("anthropic").await.unwrap());
    assert_eq!(settings.list().await.unwrap().len(), 1);
}
