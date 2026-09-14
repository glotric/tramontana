//! V3: V2's recovery promises, re-proven with the real worker, tools and
//! storage.
//!
//! V2 proved checkpoint, resume, export and import against fakes, and
//! `BUILD_ORDER.md` §17 asks for that suite again with the real worker, a real
//! model, and real write and command tools. Here each promise runs through the
//! reference worker, the real file and recipe tools, SQLite and the CLI:
//!
//! - after one write has completed, a second write held at its approval
//!   survives the application stopping; the checkpoint reports it as safe to
//!   replay; reopening launches nothing; resuming runs it exactly once and
//!   never repeats the completed one;
//! - an interrupted recipe is reported unsafe to replay, its process tree is
//!   gone, it is never re-run, and discarding it keeps the history;
//! - the finished record exports and imports through separate CLI processes,
//!   without any of this machine's absolute paths.
//!
//! By default the model is a local server scripted turn by turn. The ignored
//! `live_` tests drive the same scenarios with a real local model through the
//! managed `llama-server` path: set `TRAMONTANA_LIVE_LLAMA_SERVER` to the
//! binary and `TRAMONTANA_LIVE_MODEL` to the GGUF file, and run them with
//! `--ignored`. A real model chooses its own steps, so those assert only what
//! must hold whatever it chooses.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated integration fixture assertions"
)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use support::*;
use tokio::sync::Notify;
use tramontana_composition::Supervision;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ApprovalRequest, ErrorCode, EventEnvelope, EventPayload,
    PendingActionKind, ReplaySafety, TaskEventKind, TaskState,
};
use tramontana_core::ports::{Approver, PermissionError};
use tramontana_core::testing::ScriptedApprover;

/// A person who approves the first `allow` questions, then has not answered
/// yet: asked, and holding the question open.
struct Hesitant {
    allow: usize,
    questions: AtomicUsize,
    held: Notify,
}

impl Hesitant {
    fn allowing(allow: usize) -> Self {
        Self {
            allow,
            questions: AtomicUsize::new(0),
            held: Notify::new(),
        }
    }
}

#[async_trait]
impl Approver for Hesitant {
    async fn ask(
        &self,
        _request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        if self.questions.fetch_add(1, Ordering::SeqCst) < self.allow {
            return Ok(ApprovalDecisionKind::AllowOnce);
        }
        self.held.notify_one();
        std::future::pending().await
    }
}

/// Where the model comes from.
enum Model {
    /// A scripted server; the body of each request it answered arrives here.
    Scripted(mpsc::Receiver<Value>),
    /// A real local model through the managed `llama-server` path.
    Live,
}

impl Model {
    fn configure(f: &Fixture, live: bool, script: Script) -> Self {
        if !live {
            return Self::Scripted(scripted_model(&f.profile, script));
        }
        let server = std::env::var("TRAMONTANA_LIVE_LLAMA_SERVER")
            .expect("set TRAMONTANA_LIVE_LLAMA_SERVER to a llama-server binary");
        let model = std::env::var("TRAMONTANA_LIVE_MODEL")
            .expect("set TRAMONTANA_LIVE_MODEL to a GGUF file");
        cli(
            &f.profile,
            &[
                "provider",
                "add-llama",
                "live",
                "--binary",
                &server,
                "--model-path",
                &model,
                "--model",
                "live-model",
            ],
        );
        cli(&f.profile, &["provider", "tool-calls", "live"]);
        Self::Live
    }

    fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    fn supervision(&self) -> Supervision {
        match self {
            Self::Scripted(_) => Supervision::Skip,
            Self::Live => Supervision::Start,
        }
    }

    /// How long a task may take: a real model is slower than a script.
    fn patience(&self) -> Duration {
        match self {
            Self::Scripted(_) => Duration::from_secs(60),
            Self::Live => Duration::from_secs(300),
        }
    }

    /// Add the requests the scripted model has answered since last asked.
    fn count_answered(&self, count: &mut usize) {
        if let Self::Scripted(bodies) = self {
            while bodies.try_recv().is_ok() {
                *count += 1;
            }
        }
    }
}

const WRITES: [&str; 2] = ["builtin.fs.patch", "builtin.fs.create"];

/// The invocations of write tools that started, in order.
fn writes_started(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ToolStarted {
                invocation_id,
                tool_id,
                ..
            } if WRITES.contains(&tool_id.as_str()) => Some(invocation_id.to_string()),
            _ => None,
        })
        .collect()
}

/// What a task did, briefly, for a failure message: each tool it asked for,
/// what came of it, and how the task ended.
fn describe(events: &[EventEnvelope]) -> String {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ToolRequested {
                tool_id,
                arguments_summary,
                ..
            } => Some(format!("asked for {tool_id} ({arguments_summary})")),
            EventPayload::ToolCompleted {
                display_summary, ..
            } => Some(format!("-> {display_summary}")),
            EventPayload::ToolFailed { error, .. } => Some(format!("-> failed: {:?}", error.code)),
            EventPayload::TaskCompleted { summary, .. } => Some(format!("completed: {summary}")),
            EventPayload::TaskFailed { error } => {
                Some(format!("failed: {:?}: {}", error.code, error.message))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Read the greeting and patch it with the hash the read returned; create
/// notes.txt; after the restart, run the project's test and answer.
fn fixing(turn: usize, body: &Value) -> (Value, &'static str) {
    match turn {
        0 => call(
            turn,
            body,
            "Read a bounded",
            json!({ "path": "greeting.txt" }),
        ),
        1 => call(
            turn,
            body,
            "Replace one exact occurrence",
            json!({
                "path": "greeting.txt",
                "expectedHash": read_hash(body),
                "oldText": "Hello, world",
                "newText": "Hello, Tramontana",
            }),
        ),
        2 => call(
            turn,
            body,
            "Create a new text file",
            json!({ "path": "notes.txt", "content": "done\n" }),
        ),
        3 => call(
            turn,
            body,
            "Run one of the commands",
            json!({ "recipe": "test" }),
        ),
        _ => answer("The greeting names Tramontana, notes.txt exists, and the test passes."),
    }
}

const FIXING: &str = "Do these three things in order. \
    First, in greeting.txt, replace the text `Hello, world` with `Hello, Tramontana`: \
    read the file and use its contentHash as expectedHash. \
    Second, create a file notes.txt containing the word done. \
    Third, run the project's `test` recipe and report the result.";

/// Run the slow recipe.
fn slow(turn: usize, body: &Value) -> (Value, &'static str) {
    match turn {
        0 => call(
            turn,
            body,
            "Run one of the commands",
            json!({ "recipe": "slow" }),
        ),
        _ => answer("The slow recipe ran."),
    }
}

const SLOW: &str = "Run the project's `slow` recipe.";

fn a_held_write_is_resumed_once_without_repeating_the_completed_one(live: bool) {
    let f = fixture();
    let model = Model::configure(&f, live, fixing);
    let fixing_conversation = conversation(&f, "Fix the greeting");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut answered = 0;

    // The first write is approved and completes. The application stops while
    // the second is still being asked about.
    let (task, checkpoint) = runtime.block_on(async {
        let hesitant = Arc::new(Hesitant::allowing(1));
        let first = assemble(&f.profile, hesitant.clone(), model.supervision()).await;
        let task = start(&first.service, &fixing_conversation, FIXING).await;
        // Until the second write is asked about, or the task ends without one.
        let held = tokio::time::timeout(model.patience(), async {
            loop {
                tokio::select! {
                    () = hesitant.held.notified() => return true,
                    () = tokio::time::sleep(Duration::from_millis(200)) => {
                        if first.service.get_task(&task).await.unwrap().state.is_terminal() {
                            return false;
                        }
                    }
                }
            }
        })
        .await;
        if !matches!(held, Ok(true)) {
            let events = first.service.replay_task_events(&task, 0).await.unwrap();
            panic!(
                "the model never asked for a second write ({}): {}",
                if held.is_err() {
                    "timed out"
                } else {
                    "the task ended"
                },
                describe(&events)
            );
        }
        let checkpoint = first.service.create_checkpoint(&task).await.unwrap();
        (task, checkpoint)
    });
    assert_eq!(checkpoint.task_state, TaskState::Interrupted);
    let pending = checkpoint
        .pending_action
        .as_ref()
        .expect("the write that was being asked about is pending");
    // Held at its approval, so it never started: safe to redo, whatever the
    // tool declares. A declaration decides only for a write that started and
    // was cut off, which M4's suite covers.
    assert_eq!(pending.kind, PendingActionKind::Approval, "{pending:?}");
    assert_eq!(pending.replay_safety, ReplaySafety::Safe, "{pending:?}");
    assert!(checkpoint.resume.eligible, "{checkpoint:?}");
    assert!(
        !f.root.join("notes.txt").exists(),
        "a write ran before it was approved"
    );
    if !live {
        assert_eq!(
            std::fs::read_to_string(f.root.join("greeting.txt")).unwrap(),
            "Hello, Tramontana\n",
            "the approved first write did not complete"
        );
        assert_eq!(checkpoint.completed_step_ids.len(), 2, "{checkpoint:?}");
    }
    model.count_answered(&mut answered);

    // Another process reads the same checkpoint back.
    let inspected = cli_json(&f.profile, &["task", "inspect", task.as_str()]);
    assert_eq!(
        inspected["checkpoint"]["pendingAction"]["replaySafety"], "safe",
        "{inspected}"
    );

    // Reopened: recovery launches nothing, and resuming runs the held write
    // once and leaves the completed one alone.
    let events = runtime.block_on(async {
        let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
        let second = assemble(&f.profile, approver.clone(), model.supervision()).await;
        assert_eq!(
            second.service.get_task(&task).await.unwrap().state,
            TaskState::Interrupted
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let before = answered;
        model.count_answered(&mut answered);
        assert_eq!(answered, before, "reopening asked the model something");

        second
            .service
            .resume_task(&task, &checkpoint.checkpoint_id)
            .await
            .unwrap();
        let events = finished(&second.service, &task, model.patience()).await;
        assert_eq!(
            events.last().unwrap().kind,
            TaskEventKind::TaskCompleted,
            "{events:?}"
        );
        // The held write was asked about again, with fresh permission.
        let asked = approver.asked();
        assert!(!asked.is_empty());
        if !live {
            // The held write, then the test.
            assert_eq!(asked.len(), 2, "{asked:?}");
        }
        events
    });

    let started = writes_started(&events);
    let distinct: BTreeSet<&String> = started.iter().collect();
    assert_eq!(
        distinct.len(),
        started.len(),
        "a write was started twice: {started:?}"
    );
    assert!(f.root.join("notes.txt").exists());
    assert_eq!(
        std::fs::read_to_string(f.root.join("greeting.txt")).unwrap(),
        "Hello, Tramontana\n"
    );
    if !live {
        assert_eq!(started.len(), 2, "{started:?}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("notes.txt")).unwrap(),
            "done\n"
        );
        let changed = events.iter().find_map(|event| match &event.payload {
            EventPayload::ArtifactRecorded { artifact } => Some(artifact.summary()),
            _ => None,
        });
        assert_eq!(
            changed.as_deref(),
            Some("2 file change(s): modified greeting.txt, created notes.txt")
        );
    }

    // Out through one process, in through another, into a fresh profile.
    let bundle = f.temp.path().join("bundle");
    cli(
        &f.profile,
        &[
            "bundle",
            "export",
            bundle.to_str().unwrap(),
            "--task-id",
            task.as_str(),
        ],
    );
    let root = f.root.canonicalize().unwrap();
    for entry in std::fs::read_dir(&bundle).unwrap() {
        let path = entry.unwrap().path();
        let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        for name in [f.root.to_str().unwrap(), root.to_str().unwrap()] {
            assert!(
                !text.contains(name),
                "{} names this machine's path {name}",
                path.display()
            );
        }
    }
    let other = f.temp.path().join("other.sqlite");
    cli(&other, &["bundle", "import", bundle.to_str().unwrap()]);
    let imported = cli_json(&other, &["task", "replay", task.as_str()]);
    let imported = imported.as_array().unwrap();
    let kinds: Vec<&str> = imported
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.last(), Some(&"taskCompleted"), "{kinds:?}");
    assert!(kinds.contains(&"artifactRecorded"), "{kinds:?}");
    assert_eq!(imported.len(), events.len());
    if !model.is_live() {
        assert!(
            imported
                .iter()
                .any(|event| event["payload"]["displaySummary"] == "Recipe `test` passed"),
            "the imported record does not show the test's result"
        );
    }
}

fn an_interrupted_recipe_is_never_replayed_and_discarding_keeps_history(live: bool) {
    let f = fixture();
    let model = Model::configure(&f, live, slow);
    let slow_conversation = conversation(&f, "Run the slow recipe");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut answered = 0;

    let (task, checkpoint, tree) = runtime.block_on(async {
        let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
        let first = assemble(&f.profile, approver, model.supervision()).await;
        let task = start(&first.service, &slow_conversation, SLOW).await;
        let waited = std::time::Instant::now();
        let tree = loop {
            let tree = sleepers(f.sleeps);
            if tree.len() == 2 {
                break tree;
            }
            assert!(
                waited.elapsed() < model.patience(),
                "the recipe never started: {tree:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let checkpoint = first.service.create_checkpoint(&task).await.unwrap();
        (task, checkpoint, tree)
    });
    let pending = checkpoint
        .pending_action
        .as_ref()
        .expect("the running recipe is pending");
    assert_eq!(pending.replay_safety, ReplaySafety::Unsafe, "{pending:?}");
    assert!(!checkpoint.resume.eligible, "{checkpoint:?}");
    // Stopping took the recipe's processes with it.
    let stopping = std::time::Instant::now();
    while tree.iter().any(|pid| is_alive(*pid)) {
        assert!(
            stopping.elapsed() < Duration::from_secs(5),
            "still running after the stop: {tree:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    model.count_answered(&mut answered);

    runtime.block_on(async {
        let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
        let second = assemble(&f.profile, approver.clone(), model.supervision()).await;
        let refused = second
            .service
            .resume_task(&task, &checkpoint.checkpoint_id)
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::InterruptedUnsafe);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(sleepers(f.sleeps).is_empty(), "the recipe ran again");
        assert!(
            approver.asked().is_empty(),
            "someone was asked to run it again"
        );
        let before = answered;
        model.count_answered(&mut answered);
        assert_eq!(
            answered, before,
            "the model was asked something after the stop"
        );

        let history = second.service.replay_task_events(&task, 0).await.unwrap();
        second.service.discard_checkpoint(&task).await.unwrap();
        let after = second.service.replay_task_events(&task, 0).await.unwrap();
        assert!(
            after.len() >= history.len() && after[..history.len()] == history[..],
            "discarding changed the history"
        );
        assert!(
            second
                .service
                .resume_task(&task, &checkpoint.checkpoint_id)
                .await
                .is_err()
        );
    });
}

#[test]
fn a_held_write_is_resumed_once_without_repeating_the_completed_one_and_the_record_exports() {
    a_held_write_is_resumed_once_without_repeating_the_completed_one(false);
}

#[test]
fn an_interrupted_recipe_is_never_replayed_and_discarding_it_keeps_the_history() {
    an_interrupted_recipe_is_never_replayed_and_discarding_keeps_history(false);
}

#[test]
#[ignore = "needs a local model: set TRAMONTANA_LIVE_LLAMA_SERVER and TRAMONTANA_LIVE_MODEL"]
fn live_a_held_write_is_resumed_once_without_repeating_the_completed_one_and_the_record_exports() {
    a_held_write_is_resumed_once_without_repeating_the_completed_one(true);
}

#[test]
#[ignore = "needs a local model: set TRAMONTANA_LIVE_LLAMA_SERVER and TRAMONTANA_LIVE_MODEL"]
fn live_an_interrupted_recipe_is_never_replayed_and_discarding_it_keeps_the_history() {
    an_interrupted_recipe_is_never_replayed_and_discarding_keeps_history(true);
}
