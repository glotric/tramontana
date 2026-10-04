//! Deleting everything a profile holds (ADR 0068).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tramontana_core::contracts::{
    AppConfig, AppLaunch, AppRemote, AppSecret, ErrorCode, ProviderConfig, ProviderEndpoint,
    SCHEMA_VERSION, SecretRef, SecretStore, Timestamp,
};
use tramontana_core::ports::{
    AppSettingsRepository, ApprovalStore, Approver, EventStore, ProfileEraser,
    ProviderSettingsRepository, RepositoryError, SecretVault, ToolProvider,
};
use tramontana_core::services::{
    ErasureReport, ProfileErasure, Providers, Repositories, StartTaskRequest, TaskService,
};
use tramontana_core::testing::{
    FakeClock, FakeModelProvider, FakeToolProvider, FakeWorker, InMemoryApprovalStore,
    InMemoryCheckpointRepository, InMemoryConfigurationRepository, InMemoryConversationRepository,
    InMemoryEventStore, InMemoryProjectRepository, InMemorySecretVault, InMemoryTaskRepository,
    ScriptedApprover, SequentialIdSource, assemble_tool_plane,
};

fn secret(key: &str) -> SecretRef {
    SecretRef {
        key: key.to_owned(),
        store: SecretStore::Platform,
    }
}

/// Endpoints and Apps that name three credentials, one of them twice.
struct Settings;

#[async_trait]
impl ProviderSettingsRepository for Settings {
    async fn list(&self) -> Result<Vec<ProviderEndpoint>, RepositoryError> {
        Ok(vec![ProviderEndpoint::new(
            "anthropic",
            "Anthropic",
            ProviderConfig::Anthropic {
                base_url: "https://api.anthropic.com".to_owned(),
                model_id: "claude-test".to_owned(),
                credential: secret("endpoint.key"),
            },
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        )])
    }
    async fn save(&self, _: &ProviderEndpoint) -> Result<(), RepositoryError> {
        unimplemented!("not used")
    }
    async fn delete(&self, _: &str) -> Result<bool, RepositoryError> {
        unimplemented!("not used")
    }
}

#[async_trait]
impl AppSettingsRepository for Settings {
    async fn list(&self) -> Result<Vec<AppConfig>, RepositoryError> {
        Ok(vec![AppConfig {
            schema_version: SCHEMA_VERSION,
            app_id: "notes".to_owned(),
            display_name: "Notes".to_owned(),
            enabled: true,
            launch: AppLaunch {
                program: String::new(),
                arguments: Vec::new(),
                working_directory: None,
                environment: Vec::new(),
                secrets: vec![AppSecret {
                    name: "NOTES_TOKEN".to_owned(),
                    reference: secret("endpoint.key"),
                }],
            },
            remote: Some(AppRemote {
                url: "https://notes.example".to_owned(),
                token: Some(secret("app.token")),
                client_id: None,
                refresh: Some(secret("app.refresh")),
            }),
            created_at: Timestamp::new("2026-01-01T00:00:00.000Z"),
        }])
    }
    async fn save(&self, _: &AppConfig) -> Result<(), RepositoryError> {
        unimplemented!("not used")
    }
    async fn delete(&self, _: &str) -> Result<bool, RepositoryError> {
        unimplemented!("not used")
    }
}

/// Counts erasures, and fails when told to.
#[derive(Default)]
struct Eraser {
    calls: AtomicUsize,
    fail: bool,
}

#[async_trait]
impl ProfileEraser for Eraser {
    async fn erase_everything(&self) -> Result<(), RepositoryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(RepositoryError::Unavailable("disk full".to_owned()))
        } else {
            Ok(())
        }
    }
}

fn service(chunk_delay_ms: u64) -> Arc<TaskService> {
    let (registry, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new()) as Arc<dyn ToolProvider>],
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::unavailable()) as Arc<dyn Approver>,
        Arc::new(SequentialIdSource::new()),
    )
    .unwrap();
    let events = Arc::new(InMemoryEventStore::new()) as Arc<dyn EventStore>;
    Arc::new(TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(Arc::clone(&events))),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
        },
        Providers {
            models: vec![Arc::new(FakeModelProvider::streaming_paced(
                ["one ", "two ", "three"],
                chunk_delay_ms,
            ))],
            tools: registry,
            permissions,
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        Arc::new(SequentialIdSource::new()),
    ))
}

async fn vault_holding(keys: &[&str]) -> InMemorySecretVault {
    let vault = InMemorySecretVault::new();
    for key in keys {
        vault.store(&secret(key), "synthetic").await.unwrap();
    }
    vault
}

#[tokio::test]
async fn every_credential_the_profile_names_goes_with_its_records() {
    let service = service(0);
    // `app.refresh` was already gone from the store; `unrelated` is named by
    // nothing in this profile and must be left alone.
    let vault = vault_holding(&["endpoint.key", "app.token", "unrelated"]).await;
    let eraser = Eraser::default();

    let report = service
        .erase_everything(&ProfileErasure {
            records: &eraser,
            endpoints: &Settings,
            apps: &Settings,
            vault: &vault,
        })
        .await
        .unwrap();

    assert_eq!(
        report,
        ErasureReport {
            credentials_removed: 2,
            credentials_absent: 1,
        }
    );
    assert_eq!(eraser.calls.load(Ordering::SeqCst), 1);
    for gone in ["endpoint.key", "app.token"] {
        assert!(!vault.contains(&secret(gone)).await.unwrap(), "{gone}");
    }
    assert!(vault.contains(&secret("unrelated")).await.unwrap());
}

#[tokio::test]
async fn nothing_is_erased_while_a_task_is_running() {
    let service = service(200);
    let task = service
        .start_task(StartTaskRequest {
            goal: "count slowly".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    let vault = vault_holding(&["endpoint.key"]).await;
    let eraser = Eraser::default();

    let refused = service
        .erase_everything(&ProfileErasure {
            records: &eraser,
            endpoints: &Settings,
            apps: &Settings,
            vault: &vault,
        })
        .await
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::Conflict);
    assert_eq!(eraser.calls.load(Ordering::SeqCst), 0);
    assert!(vault.contains(&secret("endpoint.key")).await.unwrap());
    service.cancel_task(&task).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn a_failed_erasure_can_be_tried_again() {
    // Credentials go first, so a store failure after them leaves records
    // that still name them: the second attempt finds nothing left to
    // remove there, and finishes.
    let service = service(0);
    let vault = vault_holding(&["endpoint.key", "app.token", "app.refresh"]).await;
    let failing = Eraser {
        fail: true,
        ..Eraser::default()
    };
    let erasure = |records| ProfileErasure {
        records,
        endpoints: &Settings,
        apps: &Settings,
        vault: &vault,
    };

    let error = service
        .erase_everything(&erasure(&failing))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PersistenceError);

    let working = Eraser::default();
    let report = service.erase_everything(&erasure(&working)).await.unwrap();
    assert_eq!(report.credentials_removed, 0);
    assert_eq!(report.credentials_absent, 3);
    assert_eq!(working.calls.load(Ordering::SeqCst), 1);
}
