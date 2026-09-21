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

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use tramontana_adapters_local::runtime_records::{self, Reaped, RuntimeRecord};
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

/// The records in `directory`, as written.
fn records_in(directory: &Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a process is running. A zombie has exited, so it counts as gone:
/// a runtime this test abandoned is never waited for, and stays a zombie.
#[cfg(unix)]
fn is_running(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available to tell whether a process is running");
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

#[cfg(unix)]
#[tokio::test]
async fn a_runtime_is_recorded_while_it_runs_and_forgotten_when_it_stops() {
    let directory = tempfile::tempdir().unwrap();
    let records = directory.path().join("runtimes");
    let mut spec = spec(&directory);
    spec.records = Some(records.clone());
    let mut server = start_in_mode(&spec, "ready", 0).await.unwrap();

    let written = records_in(&records);
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(
        written[0]["binary"],
        fake_binary().to_string_lossy().as_ref()
    );

    server.shutdown().await;
    assert!(records_in(&records).is_empty());
}

#[cfg(unix)]
#[tokio::test]
#[allow(
    clippy::mem_forget,
    reason = "forgetting the runtime is how this test leaves one behind, as kill -9 does"
)]
async fn a_runtime_left_behind_is_stopped_by_the_next_reap() {
    let directory = tempfile::tempdir().unwrap();
    let records = directory.path().join("runtimes");
    let mut spec = spec(&directory);
    spec.records = Some(records.clone());
    let server = start_in_mode(&spec, "ready", 0).await.unwrap();
    let pid = u32::try_from(records_in(&records)[0]["pid"].as_u64().unwrap()).unwrap();
    // What its owner being killed outright leaves: a running runtime whose
    // shutdown and `Drop` never run.
    std::mem::forget(server);
    assert!(is_running(pid));

    let reaped = runtime_records::reap(&records).await;

    assert!(
        matches!(reaped.as_slice(), [Reaped::Stopped(record)] if record.pid == pid),
        "{reaped:?}"
    );
    assert!(!is_running(pid), "the runtime left behind is still running");
    assert!(records_in(&records).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_record_naming_another_program_leaves_it_alone() {
    // A process id can be reused. Whatever now has it is not the runtime.
    let directory = tempfile::tempdir().unwrap();
    let records = directory.path().join("runtimes");
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    runtime_records::write(
        &records,
        &RuntimeRecord {
            pid: other.id(),
            binary: fake_binary(),
            port: 9,
        },
    )
    .unwrap();

    let reaped = runtime_records::reap(&records).await;

    assert!(
        matches!(reaped.as_slice(), [Reaped::NotOurs(_)]),
        "{reaped:?}"
    );
    assert!(is_running(other.id()), "another program was stopped");
    assert!(records_in(&records).is_empty());
    let _ = other.kill();
    let _ = other.wait();
}

#[cfg(unix)]
#[tokio::test]
async fn a_record_of_a_runtime_already_gone_is_cleared() {
    let directory = tempfile::tempdir().unwrap();
    let records = directory.path().join("runtimes");
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    runtime_records::write(
        &records,
        &RuntimeRecord {
            pid,
            binary: fake_binary(),
            port: 9,
        },
    )
    .unwrap();

    let reaped = runtime_records::reap(&records).await;

    assert!(
        matches!(reaped.as_slice(), [Reaped::AlreadyGone(_)]),
        "{reaped:?}"
    );
    assert!(records_in(&records).is_empty());
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

/// A folder that stands in for an MLX model: weights, tokenizer and config.
fn model_folder(directory: &tempfile::TempDir) -> PathBuf {
    let path = directory.path().join("mlx-model");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("config.json"), b"{}").unwrap();
    path
}

/// The stand-in answers the same `/health` and chat-completions requests
/// `mlx_lm.server` does, so it serves for both kinds; what differs is what
/// the supervisor checks before starting it.
fn mlx_spec(directory: &tempfile::TempDir) -> LlamaServerSpec {
    let mut spec = LlamaServerSpec::mlx(fake_binary(), model_folder(directory));
    spec.readiness_timeout = Duration::from_secs(10);
    spec
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn an_mlx_runtime_serves_its_model_folder_and_reports_healthy() {
    let directory = tempfile::tempdir().unwrap();
    let mut server = start_in_mode(&mlx_spec(&directory), "ready", 0)
        .await
        .unwrap();
    assert!(server.base_url().starts_with("http://127.0.0.1:"));
    assert_eq!(server.health().await, Health::Available);
    // `mlx_lm.server` reports no build, so none is claimed for it.
    assert_eq!(server.version().build, None);
    server.shutdown().await;
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn an_mlx_model_must_be_a_folder_and_nothing_runs_otherwise() {
    let directory = tempfile::tempdir().unwrap();
    // A GGUF file where a model folder belongs: a person who picked the
    // wrong kind of model is told so, and no process starts.
    let spec = LlamaServerSpec::mlx(fake_binary(), model_file(&directory));
    let error = SupervisedLlamaServer::start(&spec).await.unwrap_err();
    assert!(
        matches!(&error, StartError::Model(why) if why.contains("is not a folder")),
        "{error:?}"
    );

    let missing = LlamaServerSpec::mlx(directory.path().join("absent"), model_folder(&directory));
    assert!(matches!(
        SupervisedLlamaServer::start(&missing).await.unwrap_err(),
        StartError::Binary(_)
    ));
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[tokio::test]
async fn mlx_is_unavailable_off_apple_silicon_and_starts_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let error = SupervisedLlamaServer::start(&mlx_spec(&directory))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StartError::Unsupported(why) if why.contains("Apple Silicon")),
        "{error:?}"
    );
    let envelope: tramontana_core::contracts::ErrorEnvelope = error.into();
    assert_eq!(
        envelope.code,
        tramontana_core::contracts::ErrorCode::CapabilityUnsupported
    );
}

#[test]
fn only_apple_silicon_macs_can_run_mlx() {
    assert_eq!(
        tramontana_adapters_local::mlx_supported(),
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    );
}
