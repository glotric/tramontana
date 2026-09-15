//! An App over stdio against a hostile stand-in server.
//!
//! `BUILD_ORDER.md` §20: malformed, oversized, crashing and hostile servers
//! must not crash or corrupt what called them. Each test starts
//! `tramontana-fake-mcp-server` in one of its modes and holds the adapter to
//! what it promises.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tramontana_adapters_mcp::{AppHealth, McpApp, McpLimits, McpServerSpec, StartError};
use tramontana_core::contracts::{
    ApprovalDecisionKind, AuthorizedToolRequest, Capability, CapabilityScope, ErrorCode,
    InvocationId, ProjectId, TaskId, Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

fn fake() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tramontana-fake-mcp-server"))
}

fn limits() -> McpLimits {
    McpLimits {
        start_timeout: Duration::from_secs(5),
        call_timeout: Duration::from_secs(5),
        ..McpLimits::default()
    }
}

fn spec(mode: &str, directory: &Path) -> McpServerSpec {
    McpServerSpec {
        app_id: "fixture".to_owned(),
        display_name: "Fixture".to_owned(),
        program: fake(),
        arguments: vec![mode.to_owned()],
        working_directory: directory.to_path_buf(),
        environment: vec![("KEEP_ME".to_owned(), "1".to_owned())],
        limits: limits(),
    }
}

async fn start(mode: &str, directory: &Path) -> McpApp {
    McpApp::start(spec(mode, directory)).await.unwrap()
}

/// An authorization for `tool`, from the real permission manager, answered
/// yes by a scripted person if a question is asked.
async fn authorize(
    app: &McpApp,
    tool: &str,
    arguments: Value,
    root: &Path,
) -> AuthorizedToolRequest {
    let descriptor = app
        .list_tools()
        .into_iter()
        .find(|descriptor| descriptor.tool_id == tool)
        .expect("the App offers the tool");
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        Arc::new(SequentialIdSource::new()),
    );
    let query = PermissionQuery {
        tool_id: descriptor.tool_id.clone(),
        capability: Capability::new(&descriptor.capability),
        risk_class: descriptor.risk_class,
        scope: CapabilityScope::project(
            ProjectId::new("project-1"),
            vec![root.display().to_string()],
        ),
        persistable: false,
        details: Vec::new(),
    };
    let (_, authorized) = manager
        .authorize(
            &query,
            &TaskId::new("task-1"),
            &InvocationId::new("invocation-1"),
            arguments,
            &Timestamp::new("2026-01-01T00:00:00.000Z"),
        )
        .await
        .unwrap();
    authorized.expect("the scripted person allowed it")
}

fn text(content: &Value) -> &str {
    content["text"].as_str().unwrap()
}

#[tokio::test]
async fn an_app_offers_its_tools_under_its_namespace_and_answers_a_call() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("ok", directory.path()).await;

    assert_eq!(app.protocol_version(), "2025-11-25");
    let mut ids: Vec<String> = app
        .list_tools()
        .into_iter()
        .map(|tool| tool.tool_id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "mcp.fixture.greeting",
            "mcp.fixture.inspect",
            "mcp.fixture.stamp"
        ]
    );

    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let outcome = app.invoke(request, CancellationToken::new()).await.unwrap();
    assert_eq!(text(&outcome.content), "Hello, Ada");
    assert!(!outcome.truncated);

    app.shutdown().await;
    assert!(matches!(app.health().await, AppHealth::Stopped { .. }));
}

#[tokio::test]
async fn the_server_is_given_its_folder_and_its_named_variables_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("ok", directory.path()).await;

    let request = authorize(&app, "mcp.fixture.inspect", json!({}), directory.path()).await;
    let outcome = app.invoke(request, CancellationToken::new()).await.unwrap();
    let seen: Value = serde_json::from_str(text(&outcome.content)).unwrap();
    assert_eq!(
        seen["env"],
        json!(["KEEP_ME"]),
        "the environment leaked: {seen}"
    );
    assert_eq!(
        std::fs::canonicalize(seen["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(directory.path()).unwrap()
    );
    app.shutdown().await;
}

#[tokio::test]
async fn a_line_that_is_not_json_is_passed_over() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("malformed", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let outcome = app.invoke(request, CancellationToken::new()).await.unwrap();
    assert_eq!(text(&outcome.content), "Hello, Ada");
    app.shutdown().await;
}

#[tokio::test]
async fn hostile_tool_names_and_schemas_are_refused_and_nothing_leaves_the_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("hostile-tools", directory.path()).await;

    let mut ids: Vec<String> = app
        .list_tools()
        .into_iter()
        .map(|tool| tool.tool_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["mcp.fixture.ok_tool", "mcp.fixture.rambling"]);
    assert!(
        app.list_tools()
            .iter()
            .all(|tool| tool.description.chars().count() <= 1_001)
    );
    // Search and search, ../escape, the long name, the reference and the
    // non-object schema.
    assert_eq!(app.refused().len(), 6, "{:?}", app.refused());
    app.shutdown().await;
}

#[tokio::test]
async fn pages_that_never_end_stop_at_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec("endless-pages", directory.path());
    // A tool limit high enough that only the page limit can stop listing.
    spec.limits.max_tools = 10_000;
    spec.limits.max_pages = 3;
    let app = McpApp::start(spec).await.unwrap();
    // Three pages of the same three tools: every name collides with itself,
    // so none is registered, and exactly three pages were read.
    assert!(app.list_tools().is_empty());
    assert_eq!(app.refused().len(), 9, "{:?}", app.refused());
    app.shutdown().await;
}

#[tokio::test]
async fn a_crash_during_a_call_fails_the_call_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("crash-on-call", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let error = app
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolFailed);
    assert!(matches!(app.health().await, AppHealth::Stopped { .. }));
    app.shutdown().await;
}

#[tokio::test]
async fn a_call_that_never_answers_times_out_and_the_app_is_told() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec("hang-on-call", directory.path());
    spec.limits.call_timeout = Duration::from_millis(500);
    let app = McpApp::start(spec).await.unwrap();

    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let error = app
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    wait_for(|| app.diagnostics().contains("cancelled:")).await;
    app.shutdown().await;
}

#[tokio::test]
async fn a_cancelled_call_returns_at_once_and_the_app_is_told() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("hang-on-call", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });

    let started = Instant::now();
    let error = app.invoke(request, cancel).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
    wait_for(|| app.diagnostics().contains("cancelled:")).await;
    app.shutdown().await;
}

#[tokio::test]
async fn a_message_past_the_limit_closes_the_connection() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("oversized-result", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let error = app
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitExceeded);
    assert_eq!(
        app.health().await,
        AppHealth::Stopped {
            reason: "the App sent a message larger than the limit".to_owned()
        }
    );
    app.shutdown().await;
}

#[tokio::test]
async fn a_large_result_is_cut_to_the_limit_and_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("large-text", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let outcome = app.invoke(request, CancellationToken::new()).await.unwrap();
    assert!(outcome.truncated);
    assert_eq!(outcome.content["truncated"], true);
    assert!(text(&outcome.content).len() <= 64 * 1024);
    assert!(outcome.display_summary.contains("cut to the limit"));
    app.shutdown().await;
}

#[tokio::test]
async fn a_result_the_app_marks_as_an_error_is_a_failed_call() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("error-result", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let error = app
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolFailed);
    assert!(
        error.message.contains("the App failed"),
        "{}",
        error.message
    );
    app.shutdown().await;
}

#[tokio::test]
async fn a_reply_of_the_wrong_shape_is_a_failed_call() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec("wrong-shape", directory.path());
    spec.limits.call_timeout = Duration::from_secs(1);
    let app = McpApp::start(spec).await.unwrap();
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let error = app
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error.code, ErrorCode::ToolFailed | ErrorCode::Timeout),
        "{error:?}"
    );
    app.shutdown().await;
}

#[tokio::test]
async fn arguments_outside_the_schema_never_reach_the_app() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("crash-on-call", directory.path()).await;
    // The App would crash on any call; a refused argument must not get that
    // far.
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": 5}),
        directory.path(),
    )
    .await;
    assert!(app.invoke(request, CancellationToken::new()).await.is_err());
    assert_eq!(app.health().await, AppHealth::Running);
    app.shutdown().await;
}

#[tokio::test]
async fn a_server_that_never_starts_is_given_up_on() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec("silent-start", directory.path());
    spec.limits.start_timeout = Duration::from_millis(500);
    let started = Instant::now();
    let error = McpApp::start(spec).await.unwrap_err();
    assert!(matches!(error, StartError::Initialize(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn a_configuration_that_cannot_be_run_as_given_is_refused_before_starting() {
    let directory = tempfile::tempdir().unwrap();
    let mut relative = spec("ok", directory.path());
    relative.program = PathBuf::from("tramontana-fake-mcp-server");
    assert!(matches!(
        McpApp::start(relative).await,
        Err(StartError::Invalid(_))
    ));
    let mut shouting = spec("ok", directory.path());
    shouting.app_id = "Fixture".to_owned();
    assert!(matches!(
        McpApp::start(shouting).await,
        Err(StartError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_server_writing_to_stderr_without_end_still_answers() {
    let directory = tempfile::tempdir().unwrap();
    let app = start("stderr-flood", directory.path()).await;
    let request = authorize(
        &app,
        "mcp.fixture.greeting",
        json!({"name": "Ada"}),
        directory.path(),
    )
    .await;
    let outcome = app.invoke(request, CancellationToken::new()).await.unwrap();
    assert_eq!(text(&outcome.content), "Hello, Ada");
    assert!(app.diagnostics().len() <= 8 * 1024 + 4);
    app.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_an_app_stops_what_it_started() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    let mut spec = spec("spawn-child", directory.path());
    spec.arguments.push(pid_file.display().to_string());
    // The stand-in starts `sleep` by name, so it needs a PATH to find it.
    spec.environment
        .push(("PATH".to_owned(), "/bin:/usr/bin".to_owned()));
    let app = McpApp::start(spec).await.unwrap();
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    assert!(alive(&pid), "the child never started");

    app.shutdown().await;
    wait_for(|| !alive(&pid)).await;
}

#[cfg(unix)]
fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
