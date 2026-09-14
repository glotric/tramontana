//! The patch tool: one exact edit, against the file the caller actually read.
//!
//! `BUILD_ORDER.md` §16 item 2. The property that matters most is not that an
//! edit applies but that a stale or ambiguous one does not: a patch computed
//! from an old reading, or naming more than one place, must change nothing.
//! Every refusal here is checked against the bytes it must have left alone.

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
    content_hash_bytes,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolOutcome, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::contract_suites::idempotent_tool_contract;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

const PATCH: &str = "builtin.fs.patch";
const READ: &str = "builtin.fs.read";

async fn authorize(
    tool: &str,
    risk: RiskClass,
    capability: &str,
    root: &Path,
    arguments: serde_json::Value,
) -> AuthorizedToolRequest {
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        Arc::new(SequentialIdSource::new()),
    );
    let query = PermissionQuery {
        tool_id: tool.to_owned(),
        capability: Capability::new(capability),
        risk_class: risk,
        scope: CapabilityScope::project(
            ProjectId::new("project-1"),
            vec![root.display().to_string()],
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
    authorized.expect("authorized")
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    (temp, root)
}

/// The `contentHash` the read tool reports, as a model would receive it.
async fn read_hash(root: &Path, path: &str) -> String {
    let request = authorize(
        READ,
        RiskClass::Read,
        "filesystem.read",
        root,
        json!({ "path": path }),
    )
    .await;
    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();
    outcome.content["contentHash"].as_str().unwrap().to_owned()
}

async fn patch(
    root: &Path,
    path: &str,
    expected: &str,
    old_text: &str,
    new_text: &str,
) -> Result<ToolOutcome, ErrorEnvelope> {
    let request = authorize(
        PATCH,
        RiskClass::Modify,
        "filesystem.write",
        root,
        json!({ "path": path, "expectedHash": expected, "oldText": old_text, "newText": new_text }),
    )
    .await;
    FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
}

fn leftovers(directory: &Path) -> Vec<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".tramontana-"))
        .collect()
}

fn write(root: &Path, path: &str, content: &str) {
    std::fs::write(root.join(path), content).unwrap();
}

fn content(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap()
}

#[test]
fn the_patch_tool_declares_a_modifying_write_that_is_safe_to_replay() {
    let tools = FilesystemTools::new().list_tools();
    let patch = tools.iter().find(|tool| tool.tool_id == PATCH).unwrap();
    assert_eq!(patch.risk_class, RiskClass::Modify);
    assert_eq!(patch.side_effect, SideEffect::LocalReversible);
    assert_eq!(patch.capability, "filesystem.write");
    assert_eq!(patch.replay_safety, ReplaySafety::Safe);
}

#[tokio::test]
async fn a_read_reports_the_hash_a_patch_checks() {
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "pub fn one() {}\n");
    assert_eq!(
        read_hash(&root, "src/lib.rs").await,
        content_hash_bytes(b"pub fn one() {}\n")
    );
}

#[tokio::test]
async fn an_edit_applies_once_at_the_one_place_it_names() {
    let (_temp, root) = project();
    write(&root, "src/main.rs", "fn main() {\n    old();\n}\n");
    let before = read_hash(&root, "src/main.rs").await;

    let outcome = patch(&root, "src/main.rs", &before, "old()", "new()")
        .await
        .unwrap();

    assert_eq!(
        content(&root, "src/main.rs"),
        "fn main() {\n    new();\n}\n"
    );
    assert_eq!(outcome.side_effect, SideEffect::LocalReversible);
    assert_eq!(outcome.content["alreadyApplied"], false);
    assert_eq!(outcome.content["line"], 2);
    assert_eq!(outcome.content["previousHash"], before);
    // The new hash is what a follow-up edit uses as its precondition.
    assert_eq!(
        outcome.content["contentHash"],
        content_hash_bytes(b"fn main() {\n    new();\n}\n")
    );
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn replaying_a_patch_changes_nothing_the_second_time() {
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "pub fn value() -> u8 { 1 }\n");
    idempotent_tool_contract(
        &FilesystemTools::new(),
        PATCH,
        json!({
            "path": "src/lib.rs",
            "expectedHash": content_hash_bytes(b"pub fn value() -> u8 { 1 }\n"),
            "oldText": "{ 1 }",
            "newText": "{ 2 }",
        }),
        vec![root.display().to_string()],
    )
    .await;
    assert_eq!(content(&root, "src/lib.rs"), "pub fn value() -> u8 { 2 }\n");
}

#[tokio::test]
async fn a_replay_finds_the_edit_even_where_the_replacement_overlaps_itself() {
    // "ac" -> "aaa": the replacement "aa" occurs at 0 and, overlapping, at 1.
    // Only reversing at 1 gives back the original, so a search that skipped
    // overlapping places would wrongly call the replay a conflict.
    let (_temp, root) = project();
    write(&root, "src/x.txt", "ac");
    idempotent_tool_contract(
        &FilesystemTools::new(),
        PATCH,
        json!({
            "path": "src/x.txt",
            "expectedHash": content_hash_bytes(b"ac"),
            "oldText": "c",
            "newText": "aa",
        }),
        vec![root.display().to_string()],
    )
    .await;
    assert_eq!(content(&root, "src/x.txt"), "aaa");
}

#[tokio::test]
async fn a_file_changed_since_it_was_read_is_a_conflict_and_is_left_alone() {
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "let x = 1;\n");
    let stale = read_hash(&root, "src/lib.rs").await;
    write(&root, "src/lib.rs", "let x = 1;\nlet y = 2;\n");

    let error = patch(&root, "src/lib.rs", &stale, "x = 1", "x = 9")
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/lib.rs"), "let x = 1;\nlet y = 2;\n");
    assert!(leftovers(&root.join("src")).is_empty());
}

#[tokio::test]
async fn a_replay_after_someone_else_changed_the_file_is_a_conflict_not_already_applied() {
    // The edit's text is still present, but the file is no longer what the
    // edit made. Claiming "already applied" here would hide someone's change.
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "a = 1\n");
    let before = read_hash(&root, "src/lib.rs").await;
    patch(&root, "src/lib.rs", &before, "a = 1", "a = 2")
        .await
        .unwrap();
    write(&root, "src/lib.rs", "a = 2\nb = 3\n");

    let error = patch(&root, "src/lib.rs", &before, "a = 1", "a = 2")
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/lib.rs"), "a = 2\nb = 3\n");
}

#[tokio::test]
async fn text_that_is_missing_or_ambiguous_is_refused_and_nothing_changes() {
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "call();\ncall();\n");
    let hash = read_hash(&root, "src/lib.rs").await;

    let missing = patch(&root, "src/lib.rs", &hash, "absent()", "x()")
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::InvalidRequest);

    let ambiguous = patch(&root, "src/lib.rs", &hash, "call();", "done();")
        .await
        .unwrap_err();
    assert_eq!(ambiguous.code, ErrorCode::InvalidRequest);
    assert!(
        ambiguous.message.contains("2 times"),
        "{}",
        ambiguous.message
    );

    assert_eq!(content(&root, "src/lib.rs"), "call();\ncall();\n");
}

#[tokio::test]
async fn a_no_op_empty_or_malformed_edit_is_refused() {
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "keep();\n");
    let hash = read_hash(&root, "src/lib.rs").await;

    for (expected, old_text, new_text) in [
        (hash.as_str(), "keep();", "keep();"),
        (hash.as_str(), "keep();", ""),
        (hash.as_str(), "", "x"),
        ("not-a-hash", "keep();", "x();"),
        (&hash.to_uppercase()[..], "keep();", "x();"),
    ] {
        let error = patch(&root, "src/lib.rs", expected, old_text, new_text)
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidRequest,
            "{old_text:?} -> {new_text:?}"
        );
    }
    assert_eq!(content(&root, "src/lib.rs"), "keep();\n");
}

#[tokio::test]
async fn binary_and_non_utf8_files_are_refused_and_left_alone() {
    let (_temp, root) = project();
    std::fs::write(root.join("src/image.bin"), b"PNG\x00\x01\x02").unwrap();
    std::fs::write(root.join("src/latin1.txt"), b"caf\xe9\n").unwrap();

    for path in ["src/image.bin", "src/latin1.txt"] {
        let bytes = std::fs::read(root.join(path)).unwrap();
        let error = patch(&root, path, &content_hash_bytes(&bytes), "caf", "cab")
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{path}");
        assert_eq!(std::fs::read(root.join(path)).unwrap(), bytes, "{path}");
    }
}

#[tokio::test]
async fn missing_files_directories_and_escapes_are_refused() {
    let (temp, root) = project();
    std::fs::write(temp.path().join("outside.txt"), "secret = 1\n").unwrap();
    let outside_hash = content_hash_bytes(b"secret = 1\n");

    let missing = patch(&root, "src/absent.rs", &outside_hash, "a", "b")
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);

    let directory = patch(&root, "src", &outside_hash, "a", "b")
        .await
        .unwrap_err();
    assert_eq!(directory.code, ErrorCode::Conflict);

    let climb = patch(&root, "../outside.txt", &outside_hash, "1", "2")
        .await
        .unwrap_err();
    assert_eq!(climb.code, ErrorCode::PermissionDenied);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("outside.txt")).unwrap(),
        "secret = 1\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_at_the_path_is_never_patched_through() {
    let (_temp, root) = project();
    write(&root, "src/real.txt", "value = 1\n");
    std::os::unix::fs::symlink(root.join("src/real.txt"), root.join("src/link.txt")).unwrap();

    let error = patch(
        &root,
        "src/link.txt",
        &content_hash_bytes(b"value = 1\n"),
        "1",
        "2",
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/real.txt"), "value = 1\n");
}

#[cfg(unix)]
#[tokio::test]
async fn the_files_permissions_survive_the_edit() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_temp, root) = project();
    write(&root, "src/run.sh", "echo old\n");
    std::fs::set_permissions(
        root.join("src/run.sh"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let hash = read_hash(&root, "src/run.sh").await;

    patch(&root, "src/run.sh", &hash, "old", "new")
        .await
        .unwrap();

    let mode = std::fs::metadata(root.join("src/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o750, "an edit reset the file's permissions");
}

#[tokio::test]
async fn line_endings_are_preserved_exactly() {
    let (_temp, root) = project();
    write(&root, "src/win.txt", "one\r\ntwo\r\nthree\r\n");
    let hash = read_hash(&root, "src/win.txt").await;

    patch(&root, "src/win.txt", &hash, "two", "TWO")
        .await
        .unwrap();

    assert_eq!(content(&root, "src/win.txt"), "one\r\nTWO\r\nthree\r\n");
}

#[tokio::test]
async fn a_replay_that_cannot_be_verified_cheaply_is_a_conflict_rather_than_a_claim() {
    // After this edit the replacement occurs 71 times — beyond what the replay
    // check examines. The edit *was* applied, but the tool cannot afford to
    // prove it, so it refuses rather than claim an effect it did not verify.
    let (_temp, root) = project();
    write(
        &root,
        "src/many.txt",
        &format!("{}TARGET\n", "x\n".repeat(70)),
    );
    let before = read_hash(&root, "src/many.txt").await;
    patch(&root, "src/many.txt", &before, "TARGET", "x")
        .await
        .unwrap();
    let applied = content(&root, "src/many.txt");

    let error = patch(&root, "src/many.txt", &before, "TARGET", "x")
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/many.txt"), applied);
}

#[tokio::test]
async fn a_cancelled_patch_changes_nothing() {
    let (_temp, root) = project();
    write(&root, "src/lib.rs", "a();\n");
    let hash = read_hash(&root, "src/lib.rs").await;
    let request = authorize(
        PATCH,
        RiskClass::Modify,
        "filesystem.write",
        &root,
        json!({ "path": "src/lib.rs", "expectedHash": hash, "oldText": "a()", "newText": "b()" }),
    )
    .await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = FilesystemTools::new()
        .invoke(request, cancel)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(content(&root, "src/lib.rs"), "a();\n");
    assert!(leftovers(&root.join("src")).is_empty());
}
