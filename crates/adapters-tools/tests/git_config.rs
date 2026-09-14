//! Git tools never run a program the repository configures.
//!
//! `SECURITY_AND_PERMISSIONS.md` §5: "An opened Git repository does not imply
//! permission to execute its hooks or scripts." Plain read commands will run a
//! program named in Git's configuration — `core.fsmonitor` during `status`
//! and `diff`, a `textconv` driver during `diff` — and the repository can
//! supply that configuration: a committed `.gitconfig` becomes the user's
//! "global" file when `HOME` points at the project, and `.git/config` is read
//! by every command. Each test arms such a program, runs every read-only Git
//! tool, and checks that nothing ran.

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
use tramontana_adapters_tools::GitTools;
use tramontana_core::contracts::{
    AuthorizedToolRequest, Capability, CapabilityScope, InvocationId, ProjectId, RiskClass, TaskId,
    Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

const TOOLS: [&str; 3] = ["builtin.git.status", "builtin.git.diff", "builtin.git.log"];

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

/// A repository with one committed file, a program that leaves a mark each
/// time it runs, and the directory the marks go to.
struct Armed {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    hook: PathBuf,
    marks: PathBuf,
}

fn armed() -> Armed {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let marks = temp.path().join("marks");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&marks).unwrap();
    git(&repo, &["init", "-q", "."]);
    std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let hook = temp.path().join("hook.sh");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch \"{}/$$\"\n", marks.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    // A change for `diff` to look at, in a file the textconv driver claims.
    std::fs::write(repo.join(".gitattributes"), "* diff=armed\n").unwrap();
    std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();
    Armed {
        _temp: temp,
        repo,
        hook,
        marks,
    }
}

/// Run every read-only Git tool, returning which ones succeeded.
async fn run_every_tool(repo: &Path) -> Vec<(&'static str, bool)> {
    let mut results = Vec::new();
    for tool in TOOLS {
        let request = authorize(tool, repo).await;
        let succeeded = GitTools::new()
            .invoke(request, CancellationToken::new())
            .await
            .is_ok();
        results.push((tool, succeeded));
    }
    results
}

fn marks_left(marks: &Path) -> usize {
    std::fs::read_dir(marks).unwrap().count()
}

#[tokio::test]
async fn a_committed_gitconfig_cannot_make_git_run_a_program() {
    let armed = armed();
    std::fs::write(
        armed.repo.join(".gitconfig"),
        format!(
            "[core]\n\tfsmonitor = {hook}\n[diff \"armed\"]\n\ttextconv = {hook}\n",
            hook = armed.hook.display()
        ),
    )
    .unwrap();

    let results = run_every_tool(&armed.repo).await;

    assert_eq!(
        marks_left(&armed.marks),
        0,
        "a program named in the repository's committed .gitconfig was run"
    );
    assert!(results.iter().all(|(_, ok)| *ok), "{results:?}");
}

#[tokio::test]
async fn the_repositorys_own_config_cannot_make_git_run_a_program_either() {
    let armed = armed();
    let hook = armed.hook.display().to_string();
    git(&armed.repo, &["config", "core.fsmonitor", &hook]);
    git(&armed.repo, &["config", "diff.armed.textconv", &hook]);

    let results = run_every_tool(&armed.repo).await;

    assert_eq!(
        marks_left(&armed.marks),
        0,
        "a program named in the repository's .git/config was run"
    );
    assert!(results.iter().all(|(_, ok)| *ok), "{results:?}");
}

#[tokio::test]
async fn a_filter_the_repository_defines_is_switched_off() {
    // A clean filter is a program Git runs while it compares a changed file
    // with the index — during `status` and `diff`, not only `add`.
    let armed = armed();
    std::fs::write(armed.repo.join(".gitattributes"), "* filter=armed\n").unwrap();
    let hook = armed.hook.display().to_string();
    git(&armed.repo, &["config", "filter.armed.clean", &hook]);
    git(&armed.repo, &["config", "filter.armed.smudge", &hook]);

    let results = run_every_tool(&armed.repo).await;

    assert_eq!(
        marks_left(&armed.marks),
        0,
        "a filter defined in the repository's .git/config was run"
    );
    assert!(results.iter().all(|(_, ok)| *ok), "{results:?}");
}

#[tokio::test]
async fn a_hook_in_the_repository_does_not_run_when_git_reads_it() {
    // `status` rewrites the index when it refreshes a file whose timestamp
    // changed but whose content did not, and writing the index fires this
    // hook. Restoring the committed content under a new timestamp makes that
    // happen, so the test cannot pass merely because the index went untouched.
    let armed = armed();
    std::fs::write(armed.repo.join("tracked.txt"), "one\n").unwrap();
    std::fs::copy(&armed.hook, armed.repo.join(".git/hooks/post-index-change")).unwrap();

    let results = run_every_tool(&armed.repo).await;

    assert_eq!(
        marks_left(&armed.marks),
        0,
        "a hook in the repository ran while Git only read it"
    );
    assert!(results.iter().all(|(_, ok)| *ok), "{results:?}");
}
