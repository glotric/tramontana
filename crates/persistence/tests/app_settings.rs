//! Where a profile keeps its Apps (ADR 0036).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::contracts::{
    AppConfig, AppLaunch, AppRemote, AppSecret, AppVariable, SecretRef, SecretStore, Timestamp,
};
use tramontana_core::ports::AppSettingsRepository;
use tramontana_persistence::{Database, SqliteAppSettingsRepository};

async fn repository(directory: &tempfile::TempDir) -> SqliteAppSettingsRepository {
    let database = Database::open(&directory.path().join("apps.sqlite"))
        .await
        .unwrap();
    SqliteAppSettingsRepository::new(database)
}

fn notes() -> AppConfig {
    AppConfig::new(
        "notes",
        "Notes",
        AppLaunch {
            program: "/opt/notes/server".to_owned(),
            arguments: vec!["--stdio".to_owned()],
            working_directory: None,
            environment: vec![AppVariable {
                name: "LOG_LEVEL".to_owned(),
                value: "info".to_owned(),
            }],
            secrets: vec![AppSecret {
                name: "API_TOKEN".to_owned(),
                reference: SecretRef {
                    key: "notes.token".to_owned(),
                    store: SecretStore::Platform,
                },
            }],
        },
        Timestamp::new("2026-01-01T00:00:00.000Z"),
    )
}

#[tokio::test]
async fn an_app_comes_back_as_it_was_configured() {
    let directory = tempfile::tempdir().unwrap();
    let apps = repository(&directory).await;

    apps.save(&notes()).await.unwrap();
    assert_eq!(apps.list().await.unwrap(), vec![notes()]);
}

#[tokio::test]
async fn a_remote_app_comes_back_as_it_was_configured_beside_one_that_runs_here() {
    // ADR 0050: kept in the same column, read back apart from launch settings.
    let directory = tempfile::tempdir().unwrap();
    let apps = repository(&directory).await;
    let remote = AppConfig::remote(
        "search",
        "Search",
        AppRemote {
            url: "https://mcp.example.com/mcp".to_owned(),
            token: Some(SecretRef {
                key: "search.token".to_owned(),
                store: SecretStore::Platform,
            }),
            // Signed in to, so the client it registered as and where its
            // refresh token lives read back too (ADR 0056).
            client_id: Some("tramontana-client".to_owned()),
            refresh: Some(SecretRef {
                key: "search.refresh".to_owned(),
                store: SecretStore::Platform,
            }),
        },
        Timestamp::new("2026-01-02T00:00:00.000Z"),
    );
    apps.save(&notes()).await.unwrap();
    apps.save(&remote).await.unwrap();
    assert_eq!(apps.list().await.unwrap(), vec![notes(), remote]);
}

#[tokio::test]
async fn saving_the_same_app_again_replaces_it_rather_than_adding_one() {
    let directory = tempfile::tempdir().unwrap();
    let apps = repository(&directory).await;
    apps.save(&notes()).await.unwrap();

    let mut turned_off = notes();
    turned_off.enabled = false;
    turned_off.display_name = "Notes, later".to_owned();
    apps.save(&turned_off).await.unwrap();

    let stored = apps.list().await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].enabled);
    assert_eq!(stored[0].display_name, "Notes, later");
}

#[tokio::test]
async fn only_a_secrets_reference_is_written_never_its_value() {
    use sqlx::Connection;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("apps.sqlite");
    let database = Database::open(&path).await.unwrap();
    SqliteAppSettingsRepository::new(database)
        .save(&notes())
        .await
        .unwrap();

    // Read the row as anything else would: the profile is a file, and what
    // is in it is what matters here.
    let mut connection = sqlx::SqliteConnection::connect(path.to_str().unwrap())
        .await
        .unwrap();
    let row: String = sqlx::query_scalar("SELECT config FROM mcp_app WHERE app_id = ?")
        .bind("notes")
        .fetch_one(&mut connection)
        .await
        .unwrap();

    assert!(
        row.contains("notes.token"),
        "the reference is missing: {row}"
    );
    assert!(
        !row.contains("secret-value"),
        "a value reached the profile: {row}"
    );
    // The type has nowhere to put one: a secret is a reference and a name.
    assert!(row.contains("\"name\":\"API_TOKEN\""), "{row}");
}

#[tokio::test]
async fn removing_an_app_says_whether_one_was_there() {
    let directory = tempfile::tempdir().unwrap();
    let apps = repository(&directory).await;
    apps.save(&notes()).await.unwrap();

    assert!(apps.delete("notes").await.unwrap());
    assert!(apps.list().await.unwrap().is_empty());
    assert!(!apps.delete("notes").await.unwrap());
}
