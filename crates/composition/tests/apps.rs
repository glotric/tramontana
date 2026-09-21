//! What a running App contributes to a task that admits Apps.
//!
//! Real: the stand-in MCP server this repository ships, over stdio, started
//! the way both applications start Apps. It offers three tools: two that say
//! they only read, and `stamp`, which claims nothing.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::PathBuf;
use std::sync::Arc;

use tramontana_composition::start_apps;
use tramontana_core::contracts::{AppConfig, AppLaunch, SCHEMA_VERSION, Timestamp};
use tramontana_core::ports::SecretResolver;
use tramontana_core::testing::InMemorySecretVault;

/// The stand-in server, built beside this test by a workspace build.
fn fake_server() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the test binary lives under the target directory")
        .join("tramontana-fake-mcp-server");
    assert!(
        path.exists(),
        "{} is missing; build the workspace (`cargo test --workspace`) first",
        path.display()
    );
    path
}

#[tokio::test]
async fn only_the_tools_an_app_says_only_read_are_offered_to_a_model() {
    let profile = tempfile::tempdir().unwrap();
    let app = AppConfig {
        schema_version: SCHEMA_VERSION,
        app_id: "fixture".to_owned(),
        display_name: "Fixture".to_owned(),
        enabled: true,
        launch: AppLaunch {
            program: fake_server().display().to_string(),
            arguments: vec!["ok".to_owned()],
            working_directory: None,
            environment: Vec::new(),
            secrets: Vec::new(),
        },
        created_at: Timestamp::new("2026-01-01T00:00:00.000Z"),
    };
    let resolver = Arc::new(InMemorySecretVault::new()) as Arc<dyn SecretResolver>;

    let plane = start_apps(&[app], &resolver, &profile.path().join("p.sqlite")).await;
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);

    // All three are registered, so a call to `stamp` is still found and
    // asked about; only the two that claim to read are put before a model.
    let registered: Vec<String> = plane
        .apps
        .iter()
        .flat_map(|app| {
            tramontana_core::ports::ToolProvider::list_tools(app.as_ref())
                .into_iter()
                .map(|tool| tool.tool_id)
        })
        .collect();
    assert_eq!(registered.len(), 3, "{registered:?}");
    assert!(registered.contains(&"mcp.fixture.stamp".to_owned()));

    let mut offered = plane.tool_ids();
    offered.sort();
    assert_eq!(offered, ["mcp.fixture.greeting", "mcp.fixture.inspect"]);
    plane.shutdown().await;
}
