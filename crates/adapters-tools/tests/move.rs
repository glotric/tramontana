//! The move tool: no replacing, no crossing folders, no losing the file.
//!
//! `BUILD_ORDER.md` §16 item 3, "move/rename within one approved root". A move
//! has two names to get wrong, so every refusal here is checked against both:
//! the file must still be where it was, and nothing may have appeared where it
//! was going.

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

const MOVE: &str = "builtin.fs.move";
const READ: &str = "builtin.fs.read";

async fn authorize(
    tool: &str,
    risk: RiskClass,
    capability: &str,
    roots: &[&Path],
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
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
        ),
        persistable: true,
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
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    (temp, root)
}

fn write(root: &Path, path: &str, content: &str) {
    std::fs::write(root.join(path), content).unwrap();
}

fn content(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap()
}

/// The `contentHash` the read tool reports, as a model would receive it.
async fn read_hash(root: &Path, path: &str) -> String {
    let request = authorize(
        READ,
        RiskClass::Read,
        "filesystem.read",
        &[root],
        json!({ "path": path }),
    )
    .await;
    let outcome = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();
    outcome.content["contentHash"].as_str().unwrap().to_owned()
}

async fn move_in(
    roots: &[&Path],
    from: &str,
    to: &str,
    hash: &str,
) -> Result<ToolOutcome, ErrorEnvelope> {
    let request = authorize(
        MOVE,
        RiskClass::Modify,
        "filesystem.write",
        roots,
        json!({ "from": from, "to": to, "expectedHash": hash }),
    )
    .await;
    FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
}

async fn move_file(
    root: &Path,
    from: &str,
    to: &str,
    hash: &str,
) -> Result<ToolOutcome, ErrorEnvelope> {
    move_in(&[root], from, to, hash).await
}

fn hash_of(text: &str) -> String {
    content_hash_bytes(text.as_bytes())
}

#[test]
fn the_move_tool_declares_a_modifying_write_that_is_safe_to_replay() {
    let tools = FilesystemTools::new().list_tools();
    let tool = tools.iter().find(|tool| tool.tool_id == MOVE).unwrap();
    assert_eq!(tool.risk_class, RiskClass::Modify);
    assert_eq!(tool.side_effect, SideEffect::LocalReversible);
    assert_eq!(tool.capability, "filesystem.write");
    assert_eq!(tool.replay_safety, ReplaySafety::Safe);
    assert!(tool.supports_cancellation);
}

#[tokio::test]
async fn a_file_is_renamed_with_its_content() {
    let (_temp, root) = project();
    write(&root, "src/old.txt", "keep me\n");
    let hash = read_hash(&root, "src/old.txt").await;

    let outcome = move_file(&root, "src/old.txt", "src/new.txt", &hash)
        .await
        .unwrap();

    assert!(!root.join("src/old.txt").exists());
    assert_eq!(content(&root, "src/new.txt"), "keep me\n");
    assert_eq!(outcome.side_effect, SideEffect::LocalReversible);
    assert_eq!(outcome.content["from"], "src/old.txt");
    assert_eq!(outcome.content["to"], "src/new.txt");
    assert_eq!(outcome.content["contentHash"], hash.as_str());
    assert_eq!(outcome.content["alreadyApplied"], false);
}

#[tokio::test]
async fn a_file_moves_into_another_directory_of_the_project() {
    let (_temp, root) = project();
    write(&root, "src/notes.md", "# Notes\n");
    let hash = read_hash(&root, "src/notes.md").await;

    move_file(&root, "src/notes.md", "docs/notes.md", &hash)
        .await
        .unwrap();

    assert!(!root.join("src/notes.md").exists());
    assert_eq!(content(&root, "docs/notes.md"), "# Notes\n");
}

#[cfg(unix)]
#[tokio::test]
async fn the_files_permissions_survive_the_move() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_temp, root) = project();
    write(&root, "src/run.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        root.join("src/run.sh"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let hash = read_hash(&root, "src/run.sh").await;

    move_file(&root, "src/run.sh", "src/start.sh", &hash)
        .await
        .unwrap();

    let mode = std::fs::metadata(root.join("src/start.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o750);
}

#[tokio::test]
async fn replaying_a_move_changes_nothing_the_second_time() {
    let (_temp, root) = project();
    write(&root, "src/a.txt", "once\n");
    let hash = read_hash(&root, "src/a.txt").await;
    move_file(&root, "src/a.txt", "src/b.txt", &hash)
        .await
        .unwrap();

    let replay = move_file(&root, "src/a.txt", "src/b.txt", &hash)
        .await
        .unwrap();

    assert_eq!(replay.side_effect, SideEffect::None);
    assert_eq!(replay.content["alreadyApplied"], true);
    assert!(!root.join("src/a.txt").exists());
    assert_eq!(content(&root, "src/b.txt"), "once\n");

    write(&root, "src/c.txt", "contract\n");
    idempotent_tool_contract(
        &FilesystemTools::new(),
        MOVE,
        json!({ "from": "src/c.txt", "to": "docs/c.txt", "expectedHash": hash_of("contract\n") }),
        vec![root.display().to_string()],
    )
    .await;
    assert_eq!(content(&root, "docs/c.txt"), "contract\n");
}

#[tokio::test]
async fn an_existing_destination_is_a_conflict_and_both_files_are_left_alone() {
    let (_temp, root) = project();
    write(&root, "src/a.txt", "mine\n");
    write(&root, "src/b.txt", "theirs\n");
    let hash = read_hash(&root, "src/a.txt").await;

    let error = move_file(&root, "src/a.txt", "src/b.txt", &hash)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/a.txt"), "mine\n");
    assert_eq!(content(&root, "src/b.txt"), "theirs\n");
}

#[tokio::test]
async fn a_file_changed_since_it_was_read_is_a_conflict_and_is_not_moved() {
    let (_temp, root) = project();
    write(&root, "src/a.txt", "as read\n");
    let hash = read_hash(&root, "src/a.txt").await;
    write(&root, "src/a.txt", "changed since\n");

    let error = move_file(&root, "src/a.txt", "src/b.txt", &hash)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/a.txt"), "changed since\n");
    assert!(!root.join("src/b.txt").exists());
}

#[tokio::test]
async fn a_file_under_both_names_is_reported_rather_than_finished() {
    // What an interruption between the link and the unlink leaves behind.
    let (_temp, root) = project();
    write(&root, "src/a.txt", "halfway\n");
    let hash = read_hash(&root, "src/a.txt").await;
    std::fs::hard_link(root.join("src/a.txt"), root.join("src/b.txt")).unwrap();

    let error = move_file(&root, "src/a.txt", "src/b.txt", &hash)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(
        error.message.contains("interrupted move"),
        "{}",
        error.message
    );
    assert_eq!(content(&root, "src/a.txt"), "halfway\n");
    assert_eq!(content(&root, "src/b.txt"), "halfway\n");
}

#[tokio::test]
async fn a_rename_that_only_changes_letter_case_never_loses_the_file() {
    // Where names ignore case, as by default on macOS and Windows, both
    // spellings reach one entry: the move must refuse rather than remove the
    // "old" name. Where names are case-sensitive it is an ordinary rename.
    let (_temp, root) = project();
    write(&root, "src/Name.txt", "precious\n");
    let hash = read_hash(&root, "src/Name.txt").await;

    let result = move_file(&root, "src/Name.txt", "src/name.txt", &hash).await;

    let survivors: Vec<String> = std::fs::read_dir(root.join("src"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.eq_ignore_ascii_case("name.txt"))
        .collect();
    assert_eq!(survivors.len(), 1, "{survivors:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("src").join(&survivors[0])).unwrap(),
        "precious\n"
    );
    match result {
        Ok(_) => assert_eq!(survivors[0], "name.txt"),
        Err(error) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(survivors[0], "Name.txt");
        }
    }
}

#[tokio::test]
async fn a_missing_source_is_not_found_and_nothing_is_created() {
    let (_temp, root) = project();

    let error = move_file(&root, "src/missing.txt", "src/new.txt", &hash_of("x"))
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(!root.join("src/new.txt").exists());
}

#[tokio::test]
async fn a_missing_source_with_other_content_at_the_destination_is_not_already_applied() {
    let (_temp, root) = project();
    write(&root, "src/new.txt", "someone else's\n");

    let error = move_file(&root, "src/gone.txt", "src/new.txt", &hash_of("mine\n"))
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(content(&root, "src/new.txt"), "someone else's\n");
}

#[tokio::test]
async fn directories_are_not_moved() {
    let (_temp, root) = project();
    std::fs::create_dir(root.join("src/folder")).unwrap();

    let error = move_file(&root, "src/folder", "src/renamed", &hash_of("x"))
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(root.join("src/folder").is_dir());
    assert!(!root.join("src/renamed").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_is_never_moved_and_what_it_points_to_is_untouched() {
    let (temp, root) = project();
    // Pointing outside, it fails containment before anything else is asked.
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "not the project's\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("src/out.txt")).unwrap();
    // Pointing inside, it is still not a regular file.
    write(&root, "src/real.txt", "real\n");
    std::os::unix::fs::symlink(root.join("src/real.txt"), root.join("src/in.txt")).unwrap();

    for (link, target_content, code) in [
        (
            "src/out.txt",
            "not the project's\n",
            ErrorCode::PermissionDenied,
        ),
        ("src/in.txt", "real\n", ErrorCode::Conflict),
    ] {
        let error = move_file(&root, link, "src/moved.txt", &hash_of(target_content))
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{link}");
        assert!(
            std::fs::symlink_metadata(root.join(link))
                .unwrap()
                .file_type()
                .is_symlink(),
            "{link}"
        );
        assert!(
            std::fs::symlink_metadata(root.join("src/moved.txt")).is_err(),
            "{link}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "not the project's\n"
    );
    assert_eq!(content(&root, "src/real.txt"), "real\n");
}

#[tokio::test]
async fn a_missing_destination_directory_is_refused_and_the_file_stays() {
    let (_temp, root) = project();
    write(&root, "src/a.txt", "stay\n");
    let hash = read_hash(&root, "src/a.txt").await;

    let error = move_file(&root, "src/a.txt", "nowhere/a.txt", &hash)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(content(&root, "src/a.txt"), "stay\n");
    assert!(!root.join("nowhere").exists());
}

#[tokio::test]
async fn moves_that_leave_the_project_are_refused_and_nothing_moves() {
    let (temp, root) = project();
    write(&root, "src/a.txt", "inside\n");
    let hash = read_hash(&root, "src/a.txt").await;
    let outside = temp.path().join("outside.txt");

    for to in ["../outside.txt".to_owned(), outside.display().to_string()] {
        let error = move_file(&root, "src/a.txt", &to, &hash).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{to}");
        assert_eq!(content(&root, "src/a.txt"), "inside\n");
        assert!(!outside.exists(), "{to}");
    }

    std::fs::write(&outside, "outside\n").unwrap();
    let error = move_file(&root, "../outside.txt", "src/in.txt", &hash_of("outside\n"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside\n");
    assert!(!root.join("src/in.txt").exists());
}

#[tokio::test]
async fn a_move_never_crosses_from_one_approved_folder_to_another() {
    let temp = tempfile::tempdir().unwrap();
    let one = temp.path().join("one");
    let two = temp.path().join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(one.join("a.txt"), "first folder\n").unwrap();
    let hash = read_hash(&one, "a.txt").await;
    let destination = std::fs::canonicalize(&two).unwrap().join("a.txt");

    let error = move_in(
        &[&one, &two],
        "a.txt",
        &destination.display().to_string(),
        &hash,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(
        std::fs::read_to_string(one.join("a.txt")).unwrap(),
        "first folder\n"
    );
    assert!(!two.join("a.txt").exists());
}

#[tokio::test]
async fn a_malformed_hash_is_refused_and_nothing_moves() {
    let (_temp, root) = project();
    write(&root, "src/a.txt", "stay\n");

    for hash in ["", "abc", &hash_of("stay\n").to_uppercase()] {
        let error = move_file(&root, "src/a.txt", "src/b.txt", hash)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{hash:?}");
    }
    assert_eq!(content(&root, "src/a.txt"), "stay\n");
    assert!(!root.join("src/b.txt").exists());
}

#[tokio::test]
async fn a_cancelled_move_leaves_the_file_where_it_was() {
    let (_temp, root) = project();
    write(&root, "src/a.txt", "stay\n");
    let hash = read_hash(&root, "src/a.txt").await;
    let request = authorize(
        MOVE,
        RiskClass::Modify,
        "filesystem.write",
        &[&root],
        json!({ "from": "src/a.txt", "to": "src/b.txt", "expectedHash": hash }),
    )
    .await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = FilesystemTools::new()
        .invoke(request, cancel)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(content(&root, "src/a.txt"), "stay\n");
    assert!(!root.join("src/b.txt").exists());
}

#[tokio::test]
async fn unicode_names_round_trip() {
    let (_temp, root) = project();
    write(&root, "src/čaj.txt", "zelen\n");
    let hash = read_hash(&root, "src/čaj.txt").await;

    let outcome = move_file(&root, "src/čaj.txt", "docs/お茶.txt", &hash)
        .await
        .unwrap();

    assert_eq!(outcome.content["to"], "docs/お茶.txt");
    assert!(!root.join("src/čaj.txt").exists());
    assert_eq!(content(&root, "docs/お茶.txt"), "zelen\n");
}
