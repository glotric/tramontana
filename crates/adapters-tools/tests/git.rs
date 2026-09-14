//! What the Git tools show: a change, as a person or a model would review it.
//!
//! `BUILD_ORDER.md` §16 item 7 and the M7 exit criterion: the worker changes a
//! fixture repository and shows the diff. A summary alone says which files
//! moved; the patch says what actually changed in them.

#![cfg(unix)]
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
use tramontana_adapters_tools::{GitTools, ReadLimits};
use tramontana_core::contracts::{
    AuthorizedToolRequest, Capability, CapabilityScope, InvocationId, ProjectId, RiskClass, TaskId,
    Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolOutcome, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

/// A read inside the project, which policy allows without asking anyone.
async fn authorize(tool: &str, root: &Path) -> AuthorizedToolRequest {
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::unavailable()),
        Arc::new(SequentialIdSource::new()),
    );
    let query = PermissionQuery {
        tool_id: tool.to_owned(),
        capability: Capability::new("git.read"),
        risk_class: RiskClass::Read,
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
            json!({}),
            &Timestamp::new("2026-01-01T00:00:00.000Z"),
        )
        .await
        .unwrap();
    authorized.expect("a read inside the project needs no question")
}

/// Run `git` for the test's own setup, isolated from the user's configuration.
fn git(repo: &Path, arguments: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.email=test@example.com", "-c", "user.name=Test"])
        .args(arguments)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

/// A repository with one committed file.
fn repository(content: &str) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "."]);
    std::fs::write(repo.join("tracked.txt"), content).unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    (temp, repo)
}

async fn run(tools: &GitTools, tool: &str, repo: &Path) -> ToolOutcome {
    tools
        .invoke(authorize(tool, repo).await, CancellationToken::new())
        .await
        .unwrap()
}

fn text(outcome: &ToolOutcome) -> &str {
    outcome.content["text"].as_str().unwrap()
}

#[tokio::test]
async fn the_diff_shows_the_patch_after_the_summary() {
    let (_temp, repo) = repository("one\ntwo\nthree\n");
    std::fs::write(repo.join("tracked.txt"), "one\n2\nthree\n").unwrap();

    let outcome = run(&GitTools::new(), "builtin.git.diff", &repo).await;
    let text = text(&outcome);

    // Which files changed, and by how much...
    let summary = text.find("tracked.txt | 2 +-").expect(text);
    // ...then what changed in them.
    let patch = text.find("@@").expect(text);
    assert!(summary < patch, "{text}");
    assert!(
        text.contains("\n-two\n") && text.contains("\n+2\n"),
        "{text}"
    );
    assert_eq!(outcome.content["truncated"], false);
}

#[tokio::test]
async fn a_new_file_is_listed_by_status_not_hidden() {
    // A file the worker created is not tracked yet, so no diff can show it;
    // status is where it appears, and it must.
    let (_temp, repo) = repository("one\n");
    std::fs::write(repo.join("new.txt"), "created\n").unwrap();

    let status = run(&GitTools::new(), "builtin.git.status", &repo).await;

    assert!(
        text(&status).lines().any(|line| line == "?? new.txt"),
        "{}",
        text(&status)
    );
}

#[tokio::test]
async fn a_patch_larger_than_the_limit_is_cut_to_it_and_says_so() {
    let before: String = (0..200).map(|line| format!("line {line}\n")).collect();
    let after: String = (0..200).map(|line| format!("changed {line}\n")).collect();
    let (_temp, repo) = repository(&before);
    std::fs::write(repo.join("tracked.txt"), after).unwrap();
    let tools = GitTools::with_limits(ReadLimits {
        max_bytes: 512,
        ..ReadLimits::default()
    });

    let outcome = run(&tools, "builtin.git.diff", &repo).await;

    assert_eq!(outcome.content["truncated"], true);
    assert!(outcome.truncated);
    assert!(text(&outcome).len() <= 512, "{}", text(&outcome).len());
    assert!(outcome.content["byteCount"].as_u64().unwrap() > 512);
}
