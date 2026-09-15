//! `tramontana task --mode`: a task's routing mode, chosen from the CLI.
//!
//! V3 found the CLI could not choose Local Only; its tasks routed under the
//! default mode. The mode is recorded with the task (ADR 0031), which is what
//! these read back.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn task(profile: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args(["task", "Say hello", "--chunk-delay-ms", "0", "--json"])
        .args(extra)
        .arg("--profile")
        .arg(profile)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The mode recorded in the task's first event. Read from the record rather
/// than from the exit status: a projectless stand-in task may fail later for
/// reasons that have nothing to do with its mode.
fn recorded_mode(output: &Output) -> Value {
    let first = String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .next()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .expect("the task wrote no events");
    assert_eq!(first["kind"], "taskCreated");
    first["payload"]["routingMode"].clone()
}

#[test]
fn a_task_started_with_a_mode_records_it() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");

    let local_only = task(&profile, &["--mode", "local-only"]);
    assert_eq!(recorded_mode(&local_only), "localOnly");

    let default = task(&profile, &[]);
    assert_eq!(recorded_mode(&default), "automatic");
}

#[test]
fn a_mode_that_does_not_exist_is_refused_before_anything_runs() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");

    let refused = task(&profile, &["--mode", "nowhere"]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty(), "a task started anyway");
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert!(stderr.contains("local-only"), "{stderr}");
}
