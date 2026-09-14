//! The delete tool: nothing is removed that was not first kept.
//!
//! `BUILD_ORDER.md` §16 item 4, and `SECURITY_AND_PERMISSIONS.md` §5:
//! "Deletion should be recoverable". Every successful delete here is checked
//! against the copy it must have left in the recovery store, and every refusal
//! against the file it must have left in place.

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

const DELETE: &str = "builtin.fs.delete";
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

/// A project, and a recovery store beside a profile outside it.
struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let store = temp.path().join("profile.sqlite.recovery");
    Fixture { temp, root, store }
}

impl Fixture {
    fn tools(&self) -> FilesystemTools {
        FilesystemTools::new().with_recovery_store(self.store.clone())
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.root.join(path), content).unwrap();
    }

    fn content(&self, path: &str) -> String {
        std::fs::read_to_string(self.root.join(path)).unwrap()
    }

    /// Where the store keeps the copy of bytes with this hash.
    fn kept(&self, hash: &str) -> PathBuf {
        self.store.join(&hash[..2]).join(hash)
    }

    /// Every file in the store, temporary ones included.
    fn stored(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(shards) = std::fs::read_dir(&self.store) else {
            return files;
        };
        for shard in shards {
            for entry in std::fs::read_dir(shard.unwrap().path()).unwrap() {
                files.push(entry.unwrap().path());
            }
        }
        files
    }

    async fn read_hash(&self, path: &str) -> String {
        let request = authorize(
            READ,
            RiskClass::Read,
            "filesystem.read",
            &self.root,
            json!({ "path": path }),
        )
        .await;
        let outcome = FilesystemTools::new()
            .invoke(request, CancellationToken::new())
            .await
            .unwrap();
        outcome.content["contentHash"].as_str().unwrap().to_owned()
    }

    async fn delete(&self, path: &str, hash: &str) -> Result<ToolOutcome, ErrorEnvelope> {
        let request = self.request(path, hash).await;
        self.tools().invoke(request, CancellationToken::new()).await
    }

    async fn request(&self, path: &str, hash: &str) -> AuthorizedToolRequest {
        authorize(
            DELETE,
            RiskClass::Modify,
            "filesystem.write",
            &self.root,
            json!({ "path": path, "expectedHash": hash }),
        )
        .await
    }
}

fn hash_of(text: &str) -> String {
    content_hash_bytes(text.as_bytes())
}

#[tokio::test]
async fn delete_is_offered_only_where_there_is_a_recovery_store() {
    let f = fixture();
    assert!(
        !FilesystemTools::new()
            .list_tools()
            .iter()
            .any(|tool| tool.tool_id == DELETE)
    );
    let tools = f.tools().list_tools();
    let tool = tools.iter().find(|tool| tool.tool_id == DELETE).unwrap();
    assert_eq!(tool.risk_class, RiskClass::Modify);
    assert_eq!(tool.side_effect, SideEffect::LocalReversible);
    assert_eq!(tool.capability, "filesystem.write");
    assert_eq!(tool.replay_safety, ReplaySafety::Safe);
    assert!(tool.supports_cancellation);

    // Asked anyway, a provider with no store refuses and removes nothing.
    f.write("src/a.txt", "stay\n");
    let request = f.request("src/a.txt", &hash_of("stay\n")).await;
    let error = FilesystemTools::new()
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::CapabilityUnsupported);
    assert_eq!(f.content("src/a.txt"), "stay\n");
}

#[tokio::test]
async fn a_deleted_file_is_kept_byte_for_byte_before_it_is_removed() {
    let f = fixture();
    f.write("src/old.rs", "fn unused() {}\n");
    let hash = f.read_hash("src/old.rs").await;

    let outcome = f.delete("src/old.rs", &hash).await.unwrap();

    assert!(!f.root.join("src/old.rs").exists());
    assert_eq!(std::fs::read(f.kept(&hash)).unwrap(), b"fn unused() {}\n");
    assert_eq!(f.stored(), vec![f.kept(&hash)], "no temporary files left");
    assert_eq!(outcome.side_effect, SideEffect::LocalReversible);
    assert_eq!(outcome.content["path"], "src/old.rs");
    assert_eq!(outcome.content["contentHash"], hash.as_str());
    assert_eq!(outcome.content["recoverable"], true);
    assert_eq!(outcome.content["alreadyApplied"], false);
    // The record names the copy by its hash, never by where the store is.
    let store = f.temp.path().display().to_string();
    assert!(!outcome.content.to_string().contains(&store));
    assert!(!outcome.display_summary.contains(&store));
}

#[tokio::test]
async fn replaying_a_delete_changes_nothing_the_second_time() {
    let f = fixture();
    f.write("src/a.txt", "once\n");
    let hash = f.read_hash("src/a.txt").await;
    f.delete("src/a.txt", &hash).await.unwrap();

    let replay = f.delete("src/a.txt", &hash).await.unwrap();

    assert_eq!(replay.side_effect, SideEffect::None);
    assert_eq!(replay.content["alreadyApplied"], true);
    assert_eq!(std::fs::read(f.kept(&hash)).unwrap(), b"once\n");

    f.write("src/b.txt", "contract\n");
    idempotent_tool_contract(
        &f.tools(),
        DELETE,
        json!({ "path": "src/b.txt", "expectedHash": hash_of("contract\n") }),
        vec![f.root.display().to_string()],
    )
    .await;
    assert!(!f.root.join("src/b.txt").exists());
    assert_eq!(
        std::fs::read(f.kept(&hash_of("contract\n"))).unwrap(),
        b"contract\n"
    );
}

#[tokio::test]
async fn a_file_changed_since_it_was_read_is_neither_kept_nor_removed() {
    let f = fixture();
    f.write("src/a.txt", "as read\n");
    let hash = f.read_hash("src/a.txt").await;
    f.write("src/a.txt", "changed since\n");

    let error = f.delete("src/a.txt", &hash).await.unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(f.content("src/a.txt"), "changed since\n");
    assert!(f.stored().is_empty());
}

#[tokio::test]
async fn a_missing_file_with_no_copy_kept_is_not_found() {
    let f = fixture();

    let error = f
        .delete("src/missing.txt", &hash_of("x"))
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(f.stored().is_empty());
}

#[tokio::test]
async fn directories_are_not_deleted() {
    let f = fixture();
    std::fs::create_dir(f.root.join("src/folder")).unwrap();
    std::fs::write(f.root.join("src/folder/inside.txt"), "x").unwrap();

    let error = f.delete("src/folder", &hash_of("x")).await.unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(f.root.join("src/folder/inside.txt").exists());
    assert!(f.stored().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_is_never_deleted_and_what_it_points_to_is_untouched() {
    let f = fixture();
    let outside = f.temp.path().join("outside.txt");
    std::fs::write(&outside, "not the project's\n").unwrap();
    std::os::unix::fs::symlink(&outside, f.root.join("src/out.txt")).unwrap();
    f.write("src/real.txt", "real\n");
    std::os::unix::fs::symlink(f.root.join("src/real.txt"), f.root.join("src/in.txt")).unwrap();

    for (link, target_content, code) in [
        (
            "src/out.txt",
            "not the project's\n",
            ErrorCode::PermissionDenied,
        ),
        ("src/in.txt", "real\n", ErrorCode::Conflict),
    ] {
        let error = f.delete(link, &hash_of(target_content)).await.unwrap_err();
        assert_eq!(error.code, code, "{link}");
        assert!(
            std::fs::symlink_metadata(f.root.join(link))
                .unwrap()
                .file_type()
                .is_symlink(),
            "{link}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "not the project's\n"
    );
    assert_eq!(f.content("src/real.txt"), "real\n");
    assert!(f.stored().is_empty());
}

#[tokio::test]
async fn paths_that_leave_the_project_are_refused_and_nothing_is_removed() {
    let f = fixture();
    let outside = f.temp.path().join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();

    for path in ["../outside.txt".to_owned(), outside.display().to_string()] {
        let error = f.delete(&path, &hash_of("outside\n")).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{path}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside\n");
    }
    assert!(f.stored().is_empty());
}

#[tokio::test]
async fn a_different_file_already_under_the_hash_stops_the_delete() {
    let f = fixture();
    f.write("src/a.txt", "real content\n");
    let hash = f.read_hash("src/a.txt").await;
    std::fs::create_dir_all(f.kept(&hash).parent().unwrap()).unwrap();
    std::fs::write(f.kept(&hash), "corrupted\n").unwrap();

    let error = f.delete("src/a.txt", &hash).await.unwrap_err();

    assert_eq!(error.code, ErrorCode::ToolFailed);
    assert_eq!(f.content("src/a.txt"), "real content\n");
    assert_eq!(
        std::fs::read_to_string(f.kept(&hash)).unwrap(),
        "corrupted\n"
    );
}

#[tokio::test]
async fn identical_files_share_one_copy() {
    let f = fixture();
    f.write("src/a.txt", "same\n");
    f.write("src/b.txt", "same\n");
    let hash = f.read_hash("src/a.txt").await;

    f.delete("src/a.txt", &hash).await.unwrap();
    f.delete("src/b.txt", &hash).await.unwrap();

    assert!(!f.root.join("src/a.txt").exists());
    assert!(!f.root.join("src/b.txt").exists());
    assert_eq!(f.stored(), vec![f.kept(&hash)]);
}

#[tokio::test]
async fn a_malformed_hash_is_refused_and_nothing_is_removed() {
    let f = fixture();
    f.write("src/a.txt", "stay\n");

    for hash in ["", "abc", &hash_of("stay\n").to_uppercase()] {
        let error = f.delete("src/a.txt", hash).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{hash:?}");
    }
    assert_eq!(f.content("src/a.txt"), "stay\n");
    assert!(f.stored().is_empty());
}

#[tokio::test]
async fn a_cancelled_delete_leaves_the_file() {
    let f = fixture();
    f.write("src/a.txt", "stay\n");
    let hash = f.read_hash("src/a.txt").await;
    let request = f.request("src/a.txt", &hash).await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = f.tools().invoke(request, cancel).await.unwrap_err();

    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(f.content("src/a.txt"), "stay\n");
}

#[cfg(unix)]
#[tokio::test]
async fn the_recovery_store_is_private_to_this_user() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = fixture();
    f.write("src/secret.env", "TOKEN=fixture\n");
    let hash = f.read_hash("src/secret.env").await;

    f.delete("src/secret.env", &hash).await.unwrap();

    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&f.store), 0o700);
    assert_eq!(mode(f.kept(&hash).parent().unwrap()), 0o700);
    assert_eq!(mode(&f.kept(&hash)), 0o600);
}

#[tokio::test]
async fn unicode_names_are_deleted_and_kept() {
    let f = fixture();
    f.write("src/čaj.txt", "zelen\n");
    let hash = f.read_hash("src/čaj.txt").await;

    let outcome = f.delete("src/čaj.txt", &hash).await.unwrap();

    assert_eq!(outcome.content["path"], "src/čaj.txt");
    assert!(!f.root.join("src/čaj.txt").exists());
    assert_eq!(std::fs::read(f.kept(&hash)).unwrap(), "zelen\n".as_bytes());
}
