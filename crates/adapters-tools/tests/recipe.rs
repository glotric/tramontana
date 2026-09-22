//! The recipe runner: a project command, run exactly as resolved.
//!
//! `BUILD_ORDER.md` §16 item 6. Resolution and approval belong to the core and
//! are tested there; what matters here is that the tool runs only a resolved
//! command, runs it where and how it was told, and reports a failing command
//! as a result a model can read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use tramontana_adapters_tools::RecipeTools;
use tramontana_core::contracts::{
    ApprovalDecisionKind, AuthorizedToolRequest, Capability, CapabilityScope, ErrorCode,
    ErrorEnvelope, InvocationId, ProjectId, ReplaySafety, RiskClass, SideEffect, TaskId, Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolOutcome, ToolProvider};
use tramontana_core::services::{PermissionManager, RECIPE_TOOL_ID};
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

fn fixture() -> String {
    env!("CARGO_BIN_EXE_tramontana-process-tree-fixture").to_owned()
}

/// A genuine authorization: a dangerous request the user allowed once.
async fn authorize(root: &Path, arguments: serde_json::Value) -> AuthorizedToolRequest {
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        Arc::new(SequentialIdSource::new()),
    );
    let query = PermissionQuery {
        tool_id: RECIPE_TOOL_ID.to_owned(),
        capability: Capability::new("command.recipe"),
        risk_class: RiskClass::Dangerous,
        scope: CapabilityScope::project(
            ProjectId::new("project-1"),
            vec![root.display().to_string()],
        ),
        persistable: false,
        details: Vec::new(),
        site: None,
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
    authorized.expect("authorized")
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    (temp, root)
}

async fn run(root: &Path, arguments: serde_json::Value) -> Result<ToolOutcome, ErrorEnvelope> {
    let request = authorize(root, arguments).await;
    RecipeTools::new()
        .invoke(request, CancellationToken::new())
        .await
}

fn resolved(run: &[&str], inherit: &[&str]) -> serde_json::Value {
    json!({
        "recipe": "test",
        "run": run,
        "inherit": inherit,
        "declaredIn": ".tramontana/project.md",
    })
}

#[test]
fn the_recipe_tool_declares_a_dangerous_command_that_is_never_replayed() {
    let tools = RecipeTools::new().list_tools();
    let [tool] = tools.as_slice() else {
        panic!("one tool: {tools:?}")
    };
    assert_eq!(tool.tool_id, RECIPE_TOOL_ID);
    assert_eq!(tool.provider_id, "builtin");
    assert_eq!(tool.risk_class, RiskClass::Dangerous);
    assert_eq!(tool.side_effect, SideEffect::External);
    assert_eq!(tool.replay_safety, ReplaySafety::Unsafe);
    assert!(tool.supports_cancellation);
    // The model names a recipe; it has no way to supply a command.
    assert_eq!(
        tool.input_schema["properties"],
        json!({ "recipe": { "type": "string" } })
    );
    assert_eq!(tool.input_schema["additionalProperties"], false);
}

#[tokio::test]
async fn a_resolved_recipe_runs_its_command_in_the_project_root() {
    let (_temp, root) = project();
    let fixture = fixture();

    let outcome = run(&root, resolved(&[&fixture, "pwd"], &[])).await.unwrap();

    assert_eq!(outcome.content["succeeded"], true);
    assert_eq!(outcome.content["exitCode"], 0);
    assert_eq!(outcome.content["recipe"], "test");
    let printed = PathBuf::from(outcome.content["stdout"].as_str().unwrap().trim());
    assert_eq!(
        std::fs::canonicalize(printed).unwrap(),
        std::fs::canonicalize(&root).unwrap()
    );
    assert_eq!(outcome.side_effect, SideEffect::External);
    assert_eq!(outcome.display_summary, "Recipe `test` passed");
}

#[tokio::test]
async fn a_failing_command_is_a_result_the_model_can_read() {
    let (_temp, root) = project();
    let fixture = fixture();

    let outcome = run(&root, resolved(&[&fixture, "exit", "3"], &[]))
        .await
        .unwrap();

    assert_eq!(outcome.content["exitCode"], 3);
    assert_eq!(outcome.content["succeeded"], false);
    assert_eq!(outcome.display_summary, "Recipe `test` failed (exit 3)");
}

#[tokio::test]
async fn only_the_resolved_environment_reaches_the_command() {
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let (_temp, root) = project();
    let fixture = fixture();

    let outcome = run(&root, resolved(&[&fixture, "env"], &["PATH"]))
        .await
        .unwrap();

    let stdout = outcome.content["stdout"].as_str().unwrap();
    let names: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    assert!(!names.contains(&"CARGO_MANIFEST_DIR"), "{names:?}");
    assert!(names.contains(&"PATH"), "{names:?}");
    #[cfg(unix)]
    assert_eq!(names, vec!["PATH"]);
}

#[tokio::test]
async fn a_request_the_core_did_not_resolve_is_refused() {
    let (_temp, root) = project();
    let fixture = fixture();

    for arguments in [
        json!({ "recipe": "test" }),
        json!({ "recipe": "test", "run": [], "inherit": [] }),
        json!({ "recipe": "test", "run": [fixture, "exit", "0"] }),
        json!({ "run": [fixture, "exit", "0"], "inherit": [] }),
    ] {
        let error = run(&root, arguments.clone()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{arguments}");
    }
}

#[tokio::test]
async fn a_program_that_does_not_exist_is_not_found() {
    let (_temp, root) = project();

    let error = run(&root, resolved(&["tramontana-no-such-program"], &["PATH"]))
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::NotFound);
}
