//! What each write tool reports it changed (ADR 0026).
//!
//! The task's changed-file summary is built from these reports, not from a
//! scan of the disk or from Git, so each must name the right file, the right
//! kind of change and the right hashes, and a replay must say it changed
//! nothing new.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use tramontana_adapters_tools::FilesystemTools;
use tramontana_core::contracts::{
    ApprovalDecisionKind, Capability, CapabilityScope, FileChange, FileChangeKind, InvocationId,
    ProjectId, RiskClass, TaskId, Timestamp, content_hash_bytes,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolOutcome, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

struct Project {
    _temp: tempfile::TempDir,
    root: PathBuf,
    tools: FilesystemTools,
}

fn project() -> Project {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    let tools = FilesystemTools::new().with_recovery_store(temp.path().join("recovery"));
    Project {
        _temp: temp,
        root,
        tools,
    }
}

impl Project {
    async fn call(&self, tool: &str, arguments: serde_json::Value) -> ToolOutcome {
        let (risk, capability) = if tool == "builtin.fs.read" {
            (RiskClass::Read, "filesystem.read")
        } else {
            (RiskClass::Modify, "filesystem.write")
        };
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
                vec![self.root.display().to_string()],
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
        self.tools
            .invoke(authorized.unwrap(), CancellationToken::new())
            .await
            .unwrap()
    }
}

fn hash(text: &str) -> String {
    content_hash_bytes(text.as_bytes())
}

fn reported(kind: FileChangeKind, path: &str, content: &str, already_applied: bool) -> FileChange {
    FileChange {
        change: kind,
        path: path.to_owned(),
        from: None,
        content_hash: Some(hash(content)),
        previous_hash: None,
        already_applied,
    }
}

#[tokio::test]
async fn each_write_reports_the_change_it_made() {
    let p = project();

    let created = p
        .call(
            "builtin.fs.create",
            json!({ "path": "src/a.txt", "content": "one\n" }),
        )
        .await;
    assert_eq!(
        created.changes,
        vec![reported(
            FileChangeKind::Created,
            "src/a.txt",
            "one\n",
            false
        )]
    );

    // The same create again made nothing new, and says so.
    let replayed = p
        .call(
            "builtin.fs.create",
            json!({ "path": "src/a.txt", "content": "one\n" }),
        )
        .await;
    assert_eq!(
        replayed.changes,
        vec![reported(
            FileChangeKind::Created,
            "src/a.txt",
            "one\n",
            true
        )]
    );

    let patched = p
        .call(
            "builtin.fs.patch",
            json!({
                "path": "src/a.txt",
                "expectedHash": hash("one\n"),
                "oldText": "one",
                "newText": "two",
            }),
        )
        .await;
    assert_eq!(
        patched.changes,
        vec![FileChange {
            previous_hash: Some(hash("one\n")),
            ..reported(FileChangeKind::Modified, "src/a.txt", "two\n", false)
        }]
    );
    let replayed = p
        .call(
            "builtin.fs.patch",
            json!({
                "path": "src/a.txt",
                "expectedHash": hash("one\n"),
                "oldText": "one",
                "newText": "two",
            }),
        )
        .await;
    assert_eq!(
        replayed.changes,
        vec![reported(
            FileChangeKind::Modified,
            "src/a.txt",
            "two\n",
            true
        )]
    );

    let moved = p
        .call(
            "builtin.fs.move",
            json!({ "from": "src/a.txt", "to": "docs/b.txt", "expectedHash": hash("two\n") }),
        )
        .await;
    assert_eq!(
        moved.changes,
        vec![FileChange {
            from: Some("src/a.txt".to_owned()),
            ..reported(FileChangeKind::Moved, "docs/b.txt", "two\n", false)
        }]
    );
    let replayed = p
        .call(
            "builtin.fs.move",
            json!({ "from": "src/a.txt", "to": "docs/b.txt", "expectedHash": hash("two\n") }),
        )
        .await;
    assert_eq!(
        replayed.changes,
        vec![FileChange {
            from: Some("src/a.txt".to_owned()),
            ..reported(FileChangeKind::Moved, "docs/b.txt", "two\n", true)
        }]
    );

    let deleted = p
        .call(
            "builtin.fs.delete",
            json!({ "path": "docs/b.txt", "expectedHash": hash("two\n") }),
        )
        .await;
    assert_eq!(
        deleted.changes,
        vec![reported(
            FileChangeKind::Deleted,
            "docs/b.txt",
            "two\n",
            false
        )]
    );
    let replayed = p
        .call(
            "builtin.fs.delete",
            json!({ "path": "docs/b.txt", "expectedHash": hash("two\n") }),
        )
        .await;
    assert_eq!(
        replayed.changes,
        vec![reported(
            FileChangeKind::Deleted,
            "docs/b.txt",
            "two\n",
            true
        )]
    );
}

#[tokio::test]
async fn a_read_reports_no_change() {
    let p = project();
    std::fs::write(p.root.join("src/a.txt"), "one\n").unwrap();

    let read = p
        .call("builtin.fs.read", json!({ "path": "src/a.txt" }))
        .await;

    assert!(read.changes.is_empty());
}
