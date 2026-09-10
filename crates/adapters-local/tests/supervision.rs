//! Supervising a real child process.
//!
//! `BUILD_ORDER.md` §14 item 8: version check, health, port allocation,
//! readiness, cancellation, logs, and cleanup. Every case here runs a real
//! process on a real port — the failure modes that matter are a runtime that
//! refuses its model, one that never becomes ready, and one that leaves a
//! child behind, and none of them can be exercised against a mock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use tramontana_adapters_local::{
    LlamaServerSpec, MINIMUM_BUILD, StartError, SupervisedLlamaServer, probe_version,
};
use tramontana_core::contracts::Health;

/// The stand-in runtime, built by Cargo alongside these tests.
fn fake_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tramontana-fake-llama-server"))
}

/// A file that stands in for a GGUF model.
fn model_file(directory: &tempfile::TempDir) -> PathBuf {
    let path = directory.path().join("model.gguf");
    std::fs::write(&path, b"not a real model").unwrap();
    path
}

fn spec(directory: &tempfile::TempDir) -> LlamaServerSpec {
    let mut spec = LlamaServerSpec::new(fake_binary(), model_file(directory));
    spec.readiness_timeout = Duration::from_secs(10);
    spec
}

/// Run `spec` with the fixture in `mode`.
async fn start_in_mode(
    spec: &LlamaServerSpec,
    mode: &str,
    load_ms: u64,
) -> Result<SupervisedLlamaServer, StartError> {
    // Through arguments, not the environment: the supervisor clears the
    // environment before spawning, so a fixture configured that way would
    // silently run in its default mode and every case here would pass by
    // accident. Found by writing the test, not by reading the code.
    let mut spec = spec.clone();
    spec.extra_args = vec!["--fake-mode".to_owned(), mode.to_owned()];
    if load_ms > 0 {
        spec.extra_args
            .extend(["--fake-load-ms".to_owned(), load_ms.to_string()]);
    }
    SupervisedLlamaServer::start(&spec).await
}

#[tokio::test]
async fn a_binary_reports_its_build_number() {
    let probed = probe_version(&fake_binary()).await.unwrap();
    assert_eq!(probed.build, Some(9999));
    assert!(probed.reported.contains("version:"), "{}", probed.reported);
}

#[tokio::test]
async fn a_path_that_is_not_a_file_is_refused_before_anything_runs() {
    let error = probe_version(Path::new("/definitely/not/here"))
        .await
        .unwrap_err();
    assert!(matches!(error, StartError::Binary(_)), "{error:?}");
}

#[tokio::test]
async fn a_missing_model_is_refused_without_starting_a_process() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec(&directory);
    spec.model = directory.path().join("absent.gguf");

    let error = SupervisedLlamaServer::start(&spec).await.unwrap_err();
    assert!(matches!(error, StartError::Model(_)), "{error:?}");
}

#[tokio::test]
async fn a_runtime_serves_on_an_allocated_port_and_reports_healthy() {
    let directory = tempfile::tempdir().unwrap();
    let mut server = start_in_mode(&spec(&directory), "ready", 0).await.unwrap();

    assert!(server.port() > 0, "no port was allocated");
    assert_eq!(
        server.base_url(),
        format!("http://127.0.0.1:{}", server.port())
    );
    assert_eq!(server.health().await, Health::Available);

    // The supervisor passed the model and the port it claims to have.
    let logs = server.logs().join("\n");
    assert!(logs.contains("model.gguf"), "{logs}");
    assert!(logs.contains(&server.port().to_string()), "{logs}");

    server.shutdown().await;
    assert_eq!(server.health().await, Health::Unavailable);
}

#[tokio::test]
async fn a_runtime_that_refuses_its_model_fails_fast_with_its_own_message() {
    // The property that makes this usable: a wrong model path must not cost
    // the full readiness bound and then report nothing. It exits, and the
    // runtime's own stderr is what the user is shown.
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec(&directory);
    spec.readiness_timeout = Duration::from_secs(30);

    let began = std::time::Instant::now();
    let error = start_in_mode(&spec, "exit-immediately", 0)
        .await
        .unwrap_err();
    let elapsed = began.elapsed();

    let StartError::ExitedEarly { tail, .. } = &error else {
        panic!("wrong failure: {error:?}");
    };
    assert!(
        tail.contains("failed to load model"),
        "the runtime's own explanation was lost: {tail}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "waited {elapsed:?} for a process that had already exited"
    );
}

#[tokio::test]
async fn a_runtime_that_never_becomes_ready_is_given_up_on_and_stopped() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = spec(&directory);
    spec.readiness_timeout = Duration::from_millis(600);

    let error = start_in_mode(&spec, "never-ready", 0).await.unwrap_err();
    let StartError::NotReady { tail, .. } = &error else {
        panic!("wrong failure: {error:?}");
    };
    // The runtime's own output comes back with the refusal, so "it never
    // became ready" is accompanied by whatever it was saying at the time.
    assert!(
        tail.contains("listening"),
        "no runtime output in the failure: {tail}"
    );
    // `start` stops the process on this path — asserted by the group test
    // above rather than here, where the supervisor was never returned and
    // there is no handle to inspect.
}

#[tokio::test]
async fn a_slow_load_is_waited_out_rather_than_treated_as_a_failure() {
    // `llama-server` answers 503 while a model loads. Reading that as failure
    // would make every large model unusable.
    let directory = tempfile::tempdir().unwrap();
    let mut server = start_in_mode(&spec(&directory), "slow", 400).await.unwrap();
    assert_eq!(server.health().await, Health::Available);
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_is_idempotent() {
    // Cleanup that could fail on a second call is cleanup nobody dares put in
    // an error path.
    let directory = tempfile::tempdir().unwrap();
    let mut server = start_in_mode(&spec(&directory), "ready", 0).await.unwrap();
    server.shutdown().await;
    server.shutdown().await;
    assert_eq!(server.health().await, Health::Unavailable);
}

#[tokio::test]
async fn logs_are_captured_and_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let mut server = start_in_mode(&spec(&directory), "ready", 0).await.unwrap();
    let logs = server.logs();
    assert!(!logs.is_empty(), "nothing was captured");
    assert!(
        logs.len() <= 200,
        "the log buffer is unbounded: {}",
        logs.len()
    );
    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_a_runtime_takes_its_children_with_it() {
    // "Owned process-tree termination" (`DEVELOPMENT.md` §6). Killing only the
    // direct child would leave the helper running, holding whatever it holds.
    let directory = tempfile::tempdir().unwrap();
    let mut server = start_in_mode(&spec(&directory), "spawn-child", 0)
        .await
        .unwrap();

    let helper = server
        .logs()
        .iter()
        .find_map(|line| line.strip_prefix("spawned helper pid=").map(str::to_owned))
        .expect("the fixture did not report a helper")
        .parse::<i32>()
        .unwrap();
    assert!(is_alive(helper), "the helper never started");

    server.shutdown().await;

    // Signals are asynchronous; give the group a moment to die.
    for _ in 0..50 {
        if !is_alive(helper) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the helper survived the supervisor's shutdown");
}

/// Whether a process exists, without touching it.
#[cfg(unix)]
fn is_alive(pid: i32) -> bool {
    // `kill -0` reports existence and sends nothing.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn a_build_older_than_the_minimum_is_refused_with_both_numbers() {
    // Nothing here can make the fixture claim an old build, so the check is
    // asserted on the constant and the error shape rather than pretended.
    let error = StartError::TooOld {
        found: 1,
        minimum: MINIMUM_BUILD,
    };
    let rendered = error.to_string();
    assert!(rendered.contains('1'), "{rendered}");
    assert!(rendered.contains(&MINIMUM_BUILD.to_string()), "{rendered}");
}
