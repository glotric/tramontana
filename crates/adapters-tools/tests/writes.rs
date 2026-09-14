//! The create tool: atomic, contained, and honest about replay.
//!
//! `BUILD_ORDER.md` §16 item 1, and the escapes `TESTING_STRATEGY.md` §5 lists
//! for writes. A read that escapes the project leaks a file; a write that
//! escapes it plants one, so every refusal here is also checked for the file
//! it must not have left behind.

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
use tramontana_adapters_tools::FilesystemTools;
use tramontana_core::contracts::{
    ApprovalDecisionKind, AuthorizedToolRequest, Capability, CapabilityScope, ErrorCode,
    ErrorEnvelope, InvocationId, ProjectId, ReplaySafety, RiskClass, SideEffect, TaskId, Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolOutcome, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::contract_suites::idempotent_tool_contract;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

const CREATE: &str = "builtin.fs.create";

/// A genuine write authorization: a `Modify` request the user allowed once.
async fn authorize_write(roots: &[&Path], arguments: serde_json::Value) -> AuthorizedToolRequest {
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        Arc::new(SequentialIdSource::new()),
    );
    let query = PermissionQuery {
        tool_id: CREATE.to_owned(),
        capability: Capability::new("filesystem.write"),
        risk_class: RiskClass::Modify,
        scope: CapabilityScope::project(
            ProjectId::new("project-1"),
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
        ),
        persistable: true,
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
    authorized.expect("the approver allows the write once")
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    (temp, root)
}

async fn create(root: &Path, path: &str, content: &str) -> Result<ToolOutcome, ErrorEnvelope> {
    let request = authorize_write(&[root], json!({ "path": path, "content": content })).await;
    FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
}

/// Temporary files the tool left in `directory`.
fn leftovers(directory: &Path) -> Vec<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".tramontana-"))
        .collect()
}

#[test]
fn the_create_tool_declares_a_modifying_write_that_is_safe_to_replay() {
    let tools = FilesystemTools::new().list_tools();
    let create = tools.iter().find(|tool| tool.tool_id == CREATE).unwrap();
    assert_eq!(create.risk_class, RiskClass::Modify);
    assert_eq!(create.side_effect, SideEffect::LocalReversible);
    assert_eq!(create.capability, "filesystem.write");
    assert_eq!(create.replay_safety, ReplaySafety::Safe);
    // Adding a write tool must not quietly widen the read tools. Named, not
    // "everything but create": the first version assumed create was the only
    // write, and failed the moment patch became a second one.
    for read in tools.iter().filter(|tool| {
        ["builtin.fs.list", "builtin.fs.read", "builtin.fs.search"].contains(&tool.tool_id.as_str())
    }) {
        assert_eq!(read.risk_class, RiskClass::Read, "{}", read.tool_id);
        assert_eq!(read.side_effect, SideEffect::None, "{}", read.tool_id);
    }
}

#[tokio::test]
async fn a_new_file_is_created_with_exactly_the_given_content() {
    let (_temp, root) = project();
    let outcome = create(&root, "src/lib.rs", "pub fn answer() -> u8 {\n    42\n}\n")
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn answer() -> u8 {\n    42\n}\n"
    );
    assert_eq!(outcome.side_effect, SideEffect::LocalReversible);
    assert_eq!(outcome.content["alreadyApplied"], false);
    assert_eq!(outcome.content["path"], "src/lib.rs");
    assert_eq!(
        outcome.content["bytes"],
        "pub fn answer() -> u8 {\n    42\n}\n".len()
    );
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn replaying_a_create_changes_nothing_the_second_time() {
    // The property recovery relies on, through the shared suite every tool
    // declaring safe replay is held to.
    let (_temp, root) = project();
    idempotent_tool_contract(
        &FilesystemTools::new(),
        CREATE,
        json!({ "path": "src/new.rs", "content": "fn new() {}\n" }),
        vec![root.display().to_string()],
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(root.join("src/new.rs")).unwrap(),
        "fn new() {}\n"
    );
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn an_existing_file_with_different_content_is_a_conflict_and_is_left_alone() {
    let (_temp, root) = project();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let error = create(&root, "src/main.rs", "fn main() { evil() }\n")
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "fn main() {}\n",
        "a create replaced an existing file"
    );
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn a_same_length_file_with_different_content_is_still_a_conflict() {
    // The length check is only a shortcut; equal lengths must still compare
    // bytes, or a same-sized file would be mistaken for this step.
    let (_temp, root) = project();
    std::fs::write(root.join("src/a.txt"), "aaaa").unwrap();
    let error = create(&root, "src/a.txt", "bbbb").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
        "aaaa"
    );
}

#[tokio::test]
async fn a_directory_at_the_path_is_a_conflict() {
    let (_temp, root) = project();
    let error = create(&root, "src", "text").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(root.join("src").is_dir());
}

#[tokio::test]
async fn a_missing_directory_is_refused_and_nothing_is_created() {
    let (_temp, root) = project();
    let error = create(&root, "missing/deeper/file.rs", "x")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(!root.join("missing").exists());
}

#[tokio::test]
async fn a_path_that_climbs_out_of_the_project_is_refused_and_plants_nothing() {
    let (temp, root) = project();
    let error = create(&root, "../planted.txt", "x").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(!temp.path().join("planted.txt").exists());
}

#[tokio::test]
async fn an_absolute_path_outside_the_project_is_refused_and_plants_nothing() {
    let (temp, root) = project();
    let outside = temp.path().join("planted.txt");
    let error = create(&root, &outside.display().to_string(), "x")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(!outside.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_directory_cannot_carry_a_write_out_of_the_project() {
    let (temp, root) = project();
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

    let error = create(&root, "escape/planted.txt", "x").await.unwrap_err();

    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(!outside.join("planted.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_at_the_path_is_never_written_through() {
    // Even one pointing inside the project: a create makes a new file, and a
    // symlink is not "no file".
    let (_temp, root) = project();
    std::fs::write(root.join("src/real.txt"), "original").unwrap();
    std::os::unix::fs::symlink(root.join("src/real.txt"), root.join("src/link.txt")).unwrap();

    let error = create(&root, "src/link.txt", "replacement")
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        std::fs::read_to_string(root.join("src/real.txt")).unwrap(),
        "original"
    );
}

#[tokio::test]
async fn oversized_content_is_refused_before_anything_is_written() {
    let (_temp, root) = project();
    let big = "a".repeat(1024 * 1024 + 1);
    let error = create(&root, "src/big.txt", &big).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitExceeded);
    assert!(!root.join("src/big.txt").exists());
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn a_cancelled_create_leaves_nothing_behind() {
    let (_temp, root) = project();
    let request = authorize_write(&[&root], json!({ "path": "src/c.rs", "content": "x" })).await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = FilesystemTools::new()
        .invoke(request, cancel)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(!root.join("src/c.rs").exists());
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn unicode_names_and_content_round_trip() {
    let (_temp, root) = project();
    let outcome = create(&root, "src/ünïcode — note.md", "naïve café ☕\n")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("src/ünïcode — note.md")).unwrap(),
        "naïve café ☕\n"
    );
    assert_eq!(outcome.content["bytes"], "naïve café ☕\n".len());
}
