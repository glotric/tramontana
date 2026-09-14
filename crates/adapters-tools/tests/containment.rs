//! Filesystem containment for the read-only tools.
//!
//! `TESTING_STRATEGY.md` §5. These are the escape attempts: every one of them
//! must be refused, and the refusal must be a normalized `permissionDenied`
//! rather than an accident of the filesystem returning an error.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tramontana_adapters_tools::FilesystemTools;
use tramontana_core::contracts::{
    ApprovalDecisionKind, Capability, CapabilityScope, ErrorCode, InvocationId, ProjectId,
    RiskClass, TaskId, Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

/// Build a genuine authorization for `roots`.
///
/// Goes through the real permission manager rather than fabricating one,
/// because `AuthorizedToolRequest` deliberately cannot be constructed outside
/// the core — which is the property under test everywhere else.
async fn authorize(
    tool_id: &str,
    roots: &[&Path],
    arguments: serde_json::Value,
) -> tramontana_core::contracts::AuthorizedToolRequest {
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::Deny)),
        Arc::new(SequentialIdSource::new()),
    );

    let query = PermissionQuery {
        tool_id: tool_id.to_owned(),
        capability: Capability::new("filesystem.read"),
        risk_class: RiskClass::Read,
        scope: CapabilityScope::project(
            ProjectId::new("project-1"),
            roots.iter().map(|r| r.display().to_string()).collect(),
        ),
        persistable: true,
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
    authorized.expect("a read inside an opened project is a safe default")
}

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("README.md"), "# Demo\n").unwrap();
    (temp, root)
}

#[tokio::test]
async fn a_file_inside_the_project_can_be_read() {
    let (_temp, root) = project();
    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "src/main.rs" }),
    )
    .await;

    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        outcome.content["text"]
            .as_str()
            .unwrap()
            .contains("fn main")
    );
    // The path shown back is project-relative: an absolute one names a machine.
    assert_eq!(outcome.content["path"], "src/main.rs");
    assert!(!outcome.display_summary.contains(root.to_str().unwrap()));
}

#[tokio::test]
async fn traversal_out_of_the_project_is_refused() {
    let (temp, root) = project();
    std::fs::write(temp.path().join("secret.txt"), "credentials").unwrap();

    for escape in [
        "../secret.txt",
        "../../etc/passwd",
        "src/../../secret.txt",
        "./../secret.txt",
    ] {
        let request = authorize(
            "builtin.fs.read",
            &[&root],
            serde_json::json!({ "path": escape }),
        )
        .await;
        let error = FilesystemTools::new()
            .invoke(request, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::PermissionDenied,
            "{escape} was not refused"
        );
    }
}

#[tokio::test]
async fn an_absolute_path_outside_the_project_is_refused() {
    let (temp, root) = project();
    let outside = temp.path().join("secret.txt");
    std::fs::write(&outside, "credentials").unwrap();

    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": outside.display().to_string() }),
    )
    .await;
    let error = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_escaping_the_project_is_refused() {
    // The escape that string matching misses: a link *inside* the project
    // whose target is outside it.
    let (temp, root) = project();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "credentials").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "escape/secret.txt" }),
    )
    .await;
    let error = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[cfg(unix)]
#[tokio::test]
async fn a_search_does_not_follow_a_symlink_out_of_the_project() {
    let (temp, root) = project();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "NEEDLE-outside").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    std::fs::write(root.join("inside.txt"), "NEEDLE-inside").unwrap();

    let request = authorize(
        "builtin.fs.search",
        &[&root],
        serde_json::json!({ "query": "NEEDLE" }),
    )
    .await;
    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();

    let rendered = outcome.content.to_string();
    assert!(rendered.contains("NEEDLE-inside"));
    assert!(
        !rendered.contains("NEEDLE-outside"),
        "search followed a symlink out of the project: {rendered}"
    );
}

#[tokio::test]
async fn a_task_with_no_approved_roots_can_read_nothing() {
    // A projectless task must not reach the filesystem at all.
    let (_temp, root) = project();
    let request = authorize(
        "builtin.fs.read",
        &[],
        serde_json::json!({ "path": "README.md" }),
    )
    .await;
    let error = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let _ = root;
}

#[tokio::test]
async fn binary_content_is_described_rather_than_returned() {
    let (_temp, root) = project();
    std::fs::write(root.join("blob.bin"), [0x7f, b'E', b'L', b'F', 0x00, 0x01]).unwrap();

    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "blob.bin" }),
    )
    .await;
    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(outcome.content["binary"], true);
    assert!(outcome.content.get("text").is_none());
}

#[tokio::test]
async fn a_large_file_is_truncated_and_says_so() {
    let (_temp, root) = project();
    let limits = tramontana_adapters_tools::ReadLimits {
        max_bytes: 64,
        ..Default::default()
    };
    std::fs::write(root.join("big.txt"), "x".repeat(4_096)).unwrap();

    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "big.txt" }),
    )
    .await;
    let outcome = FilesystemTools::with_limits(limits)
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        outcome.truncated,
        "truncation must be reported on the outcome"
    );
    assert_eq!(outcome.content["text"].as_str().unwrap().len(), 64);
    assert_eq!(outcome.content["byteCount"], 4_096);
    assert!(outcome.display_summary.contains("truncated"));
}

#[tokio::test]
async fn cancellation_stops_a_tool_before_it_reads() {
    let (_temp, root) = project();
    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "README.md" }),
    )
    .await;

    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = FilesystemTools::new()
        .invoke(request, cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
}

#[tokio::test]
async fn a_listing_is_scoped_and_sorted() {
    let (_temp, root) = project();
    let request = authorize(
        "builtin.fs.list",
        &[&root],
        serde_json::json!({ "path": "." }),
    )
    .await;
    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();

    let names: Vec<&str> = outcome.content["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["README.md", "src"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_path_repointed_after_validation_is_refused() {
    // The time-of-check/time-of-use race. The argument is validated against a
    // path, but the bytes come from a handle, and the two are resolved at
    // different instants. Here the file is replaced with a symlink pointing
    // outside the project *after* it would have been validated — the read must
    // refuse rather than return the attacker's file.
    let (temp, root) = project();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "CREDENTIALS").unwrap();

    let target = root.join("swappable.txt");
    std::fs::write(&target, "harmless").unwrap();

    // Perform the swap before invoking, which is the state an attacker who
    // won the race would leave behind.
    std::fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), &target).unwrap();

    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "swappable.txt" }),
    )
    .await;
    let error = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn an_ordinary_read_still_succeeds_with_the_identity_check() {
    // The race check must not break the common case.
    let (_temp, root) = project();
    let request = authorize(
        "builtin.fs.read",
        &[&root],
        serde_json::json!({ "path": "README.md" }),
    )
    .await;
    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();
    assert!(outcome.content["text"].as_str().unwrap().contains("Demo"));
}
