//! The command broker, against real processes.
//!
//! `BUILD_ORDER.md` §16 item 5 and the M7 exit criterion: "cancellation kills
//! the entire owned process tree without affecting unrelated processes".
//! `TESTING_STRATEGY.md` names what must be shown absent: argument injection,
//! inherited environment leakage, wrong working directory, excessive output,
//! timeout, cancellation, and orphan children.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tramontana_adapters_tools::{CommandError, CommandSpec, run_command};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tramontana-process-tree-fixture"))
}

fn spec(directory: &Path, arguments: &[&str]) -> CommandSpec {
    CommandSpec {
        program: fixture().into_os_string(),
        arguments: arguments.iter().map(OsString::from).collect(),
        directory: directory.to_path_buf(),
        inherit: Vec::new(),
        set: Vec::new(),
        deadline: Duration::from_secs(30),
        output_limit: 64 * 1024,
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The ids of processes that recorded themselves in `dir`, once there are
/// `count` of them.
async fn recorded(dir: &Path, count: usize) -> Vec<i32> {
    let started = Instant::now();
    loop {
        let ids: Vec<i32> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        if ids.len() >= count {
            return ids;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "only {} of {count} processes started",
            ids.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Whether a process is running, without touching it.
///
/// A zombie has already exited and only waits for whoever inherited it to reap
/// it — which, for a grandchild, is not this test — so it counts as gone.
///
/// Without `ps` this panics rather than answering: a probe that could not look
/// would call every process gone, and every tree test would pass vacuously.
#[cfg(unix)]
fn is_alive(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available to tell whether a process is running");
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

/// A process started outside the broker, killed and reaped however the test
/// ends. Its output goes nowhere: holding the test harness's own pipe open, a
/// bystander that outlived a failed test once stalled the whole run.
#[cfg(unix)]
struct Bystander(std::process::Child);

#[cfg(unix)]
impl Bystander {
    fn start(dir: &Path) -> Self {
        Self(
            std::process::Command::new(fixture())
                .args(["tree", dir.to_str().unwrap(), "0", "0"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

#[cfg(unix)]
impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The processes among `pids` still running after a few seconds' grace.
#[cfg(unix)]
async fn survivors(pids: &[i32]) -> Vec<i32> {
    let started = Instant::now();
    loop {
        let alive: Vec<i32> = pids.iter().copied().filter(|&pid| is_alive(pid)).collect();
        if alive.is_empty() || started.elapsed() > Duration::from_secs(5) {
            return alive;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn arguments_arrive_exactly_as_given_with_no_shell_in_between() {
    let temp = tempfile::tempdir().unwrap();
    let hostile = [
        "; echo injected",
        "$(whoami)",
        "`id`",
        "a b",
        "*",
        "--flag=\"quoted\"",
        "&& exit 7",
        "|",
        "",
    ];
    let mut arguments = vec!["args"];
    arguments.extend(hostile);

    let output = run_command(&spec(temp.path(), &arguments), &CancellationToken::new())
        .await
        .unwrap();

    assert!(output.succeeded());
    assert_eq!(lines(&output.stdout.kept), hostile);
}

#[tokio::test]
async fn only_named_and_deliberate_variables_reach_the_command() {
    // Cargo sets this for every test process. It stands for everything this
    // process holds that a command was not given.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let temp = tempfile::tempdir().unwrap();
    let mut spec = spec(temp.path(), &["env"]);
    spec.inherit = vec!["PATH".to_owned(), "TRAMONTANA_UNSET_VARIABLE".to_owned()];
    spec.set = vec![("TRAMONTANA_FIXTURE".to_owned(), "set on purpose".into())];

    let output = run_command(&spec, &CancellationToken::new()).await.unwrap();

    let lines = lines(&output.stdout.kept);
    let names: BTreeSet<String> = lines
        .iter()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
        .collect();
    assert!(!names.contains("CARGO_MANIFEST_DIR"), "{names:?}");
    assert!(!names.contains("TRAMONTANA_UNSET_VARIABLE"), "{names:?}");
    assert!(names.contains("PATH"), "{names:?}");
    assert!(lines.contains(&"TRAMONTANA_FIXTURE=set on purpose".to_owned()));
    #[cfg(unix)]
    assert_eq!(
        names,
        BTreeSet::from(["PATH".to_owned(), "TRAMONTANA_FIXTURE".to_owned()])
    );
}

#[tokio::test]
async fn the_command_runs_where_it_is_told_to() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("work here");
    std::fs::create_dir(&directory).unwrap();

    let output = run_command(&spec(&directory, &["pwd"]), &CancellationToken::new())
        .await
        .unwrap();

    let printed = PathBuf::from(lines(&output.stdout.kept)[0].clone());
    assert_eq!(
        std::fs::canonicalize(printed).unwrap(),
        std::fs::canonicalize(&directory).unwrap()
    );
}

#[tokio::test]
async fn a_missing_directory_or_malformed_variable_stops_the_command_before_it_starts() {
    let temp = tempfile::tempdir().unwrap();
    let missing = spec(&temp.path().join("missing"), &["exit", "0"]);
    assert_eq!(
        run_command(&missing, &CancellationToken::new())
            .await
            .unwrap_err(),
        CommandError::NoDirectory
    );

    for name in ["A=B", "", "NUL\0"] {
        let mut malformed = spec(temp.path(), &["exit", "0"]);
        malformed.set = vec![(name.to_owned(), "x".into())];
        assert_eq!(
            run_command(&malformed, &CancellationToken::new())
                .await
                .unwrap_err(),
            CommandError::InvalidEnvironment,
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn a_program_that_does_not_exist_fails_to_start() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = spec(temp.path(), &[]);
    spec.program = "tramontana-no-such-program".into();
    spec.inherit = vec!["PATH".to_owned()];

    assert_eq!(
        run_command(&spec, &CancellationToken::new())
            .await
            .unwrap_err(),
        CommandError::Spawn(std::io::ErrorKind::NotFound)
    );
}

#[tokio::test]
async fn the_exit_code_is_reported_as_it_was() {
    let temp = tempfile::tempdir().unwrap();

    let output = run_command(
        &spec(temp.path(), &["exit", "3"]),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(output.exit_code, Some(3));
    assert!(!output.succeeded());
}

#[tokio::test]
async fn standard_input_is_empty_and_never_waits_for_anyone() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = spec(temp.path(), &["stdin"]);
    spec.deadline = Duration::from_secs(10);

    let output = run_command(&spec, &CancellationToken::new()).await.unwrap();

    assert_eq!(lines(&output.stdout.kept), ["0"]);
}

#[tokio::test]
async fn output_past_the_limit_is_counted_but_not_kept() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = spec(temp.path(), &["flood", "1000000", "250000"]);
    spec.output_limit = 1000;
    let started = Instant::now();

    let output = run_command(&spec, &CancellationToken::new()).await.unwrap();

    // Read to the end without keeping it, so a noisy command never stalls.
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(output.succeeded());
    assert_eq!(output.stdout.kept, vec![b'o'; 1000]);
    assert_eq!(output.stdout.total, 1_000_000);
    assert!(output.stdout.truncated());
    assert!(!output.stdout.cut_off);
    assert_eq!(output.stderr.kept, vec![b'e'; 1000]);
    assert_eq!(output.stderr.total, 250_000);
    assert!(output.stderr.truncated());
}

#[tokio::test]
async fn output_within_the_limit_is_kept_whole() {
    let temp = tempfile::tempdir().unwrap();

    let output = run_command(
        &spec(temp.path(), &["flood", "10", "5"]),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(output.stdout.kept, vec![b'o'; 10]);
    assert_eq!(output.stderr.kept, vec![b'e'; 5]);
    assert!(!output.stdout.truncated());
    assert!(!output.stderr.truncated());
}

#[cfg(unix)]
#[tokio::test]
async fn the_deadline_kills_the_whole_tree() {
    let temp = tempfile::tempdir().unwrap();
    let pids = temp.path().join("pids");
    std::fs::create_dir(&pids).unwrap();
    let mut spec = spec(temp.path(), &["tree", pids.to_str().unwrap(), "2", "2"]);
    spec.deadline = Duration::from_secs(3);
    let started = Instant::now();

    let run = tokio::spawn(async move { run_command(&spec, &CancellationToken::new()).await });
    let tree = recorded(&pids, 7).await;

    assert_eq!(run.await.unwrap().unwrap_err(), CommandError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(survivors(&tree).await, Vec::<i32>::new());
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_kills_the_whole_tree_and_nothing_else() {
    let temp = tempfile::tempdir().unwrap();
    let pids = temp.path().join("pids");
    let elsewhere = temp.path().join("elsewhere");
    std::fs::create_dir(&pids).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    // A process the broker did not start, doing the same thing.
    let _bystander = Bystander::start(&elsewhere);
    let bystander_pid = recorded(&elsewhere, 1).await[0];

    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let spec = spec(temp.path(), &["tree", pids.to_str().unwrap(), "2", "2"]);
    let run = tokio::spawn(async move { run_command(&spec, &token).await });
    let tree = recorded(&pids, 7).await;
    // Alive first, so "gone" below is an observation, not a probe that sees
    // nothing.
    assert_eq!(survivors_now(&tree), tree);
    let cancelled_at = Instant::now();
    cancel.cancel();

    assert_eq!(run.await.unwrap().unwrap_err(), CommandError::Cancelled);
    assert!(cancelled_at.elapsed() < Duration::from_secs(5));
    assert_eq!(survivors(&tree).await, Vec::<i32>::new());
    assert!(is_alive(bystander_pid), "an unrelated process was killed");
}

/// The processes among `pids` running at this moment.
#[cfg(unix)]
fn survivors_now(pids: &[i32]) -> Vec<i32> {
    pids.iter().copied().filter(|&pid| is_alive(pid)).collect()
}

#[cfg(unix)]
#[tokio::test]
async fn a_leader_that_exits_early_leaves_no_orphan_behind() {
    let temp = tempfile::tempdir().unwrap();
    let pids = temp.path().join("pids");
    std::fs::create_dir(&pids).unwrap();
    let started = Instant::now();

    let output = run_command(
        &spec(temp.path(), &["orphan", pids.to_str().unwrap()]),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    // The leader succeeded, and the broker did not wait out the child it left.
    assert!(output.succeeded());
    assert!(started.elapsed() < Duration::from_secs(10));
    // The child held both pipes. Streams that ended rather than being cut off
    // show the broker killed it before collecting output — not merely later,
    // when the tree was dropped.
    assert!(!output.stdout.cut_off, "stdout was held open");
    assert!(!output.stderr.cut_off, "stderr was held open");
    let both = recorded(&pids, 2).await;
    assert_eq!(survivors(&both).await, Vec::<i32>::new());
}

#[cfg(unix)]
#[tokio::test]
async fn a_caller_that_stops_waiting_still_leaves_nothing_running() {
    let temp = tempfile::tempdir().unwrap();
    let pids = temp.path().join("pids");
    std::fs::create_dir(&pids).unwrap();
    let mut spec = spec(temp.path(), &["tree", pids.to_str().unwrap(), "1", "2"]);
    spec.deadline = Duration::from_secs(60);

    let run = tokio::spawn(async move { run_command(&spec, &CancellationToken::new()).await });
    let tree = recorded(&pids, 3).await;
    assert_eq!(survivors_now(&tree), tree);
    run.abort();
    let _ = run.await;

    assert_eq!(survivors(&tree).await, Vec::<i32>::new());
}
