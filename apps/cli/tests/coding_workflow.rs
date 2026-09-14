//! M7's exit scenario, end to end.
//!
//! The reference worker changes a fixture Git repository, shows the diff, runs
//! the project's test recipe once a person approves it, and records the
//! result. Then a recipe that starts a process tree is cancelled: the whole
//! tree stops, and an identical tree started outside Tramontana keeps running.
//!
//! Real: the CLI binary for setup and for reading the record back, SQLite, the
//! OpenAI-compatible HTTP adapter, the file, Git and recipe tools, the command
//! broker, Git, and the processes. Synthetic: the model, a local server
//! scripted turn by turn, and the person. The CLI's approval prompt refuses
//! when there is no terminal, so the run is assembled the way the CLI
//! assembles it (`wiring.rs`), with a scripted approver in place of the prompt
//! that records every question it is asked.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated integration fixture assertions"
)]

mod support;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;
use tramontana_composition::Supervision;
use tramontana_core::contracts::{
    ApprovalDecisionKind, EventEnvelope, EventPayload, TaskEventKind, TaskId,
};
use tramontana_core::testing::ScriptedApprover;

/// The model's side of the conversation: read, patch using the read's hash,
/// look at the diff, run the test and answer; then, for the second task, run
/// the slow recipe.
fn respond(turn: usize, body: &Value) -> (Value, &'static str) {
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
        2 => call(turn, body, "Show uncommitted changes", json!({})),
        3 => call(
            turn,
            body,
            "Run one of the commands",
            json!({ "recipe": "test" }),
        ),
        4 => answer("The greeting now names Tramontana, and the project's test passes."),
        _ => call(
            turn,
            body,
            "Run one of the commands",
            json!({ "recipe": "slow" }),
        ),
    }
}

/// `slow.sh` started outside Tramontana: the same program, the same sleeps.
/// Stopped, all of it, however the test ends. Its output goes nowhere.
struct Bystander(std::process::Child);

impl Bystander {
    fn start(root: &Path) -> Self {
        Self(
            Command::new("sh")
                .arg("slow.sh")
                .current_dir(root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.0.id()).unwrap()
    }

    /// Its shell and the sleeps it started.
    fn tree(&self, sleeps: [u32; 2]) -> BTreeSet<i32> {
        let mut tree: BTreeSet<i32> = sleepers(sleeps)
            .into_iter()
            .filter(|pid| parent(*pid) == Some(self.pid()))
            .collect();
        tree.insert(self.pid());
        tree
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        let children = Command::new("pgrep")
            .args(["-P", &self.pid().to_string()])
            .output();
        if let Ok(children) = children {
            for child in String::from_utf8_lossy(&children.stdout).lines() {
                let _ = Command::new("kill").args(["-KILL", child.trim()]).status();
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The recipe's tree: its sleeps, and the shell that started them.
fn recipe_tree(sleeps: [u32; 2], bystander: &Bystander) -> BTreeSet<i32> {
    let sleeping: Vec<i32> = sleepers(sleeps)
        .into_iter()
        .filter(|pid| parent(*pid) != Some(bystander.pid()))
        .collect();
    let mut tree: BTreeSet<i32> = sleeping.iter().filter_map(|pid| parent(*pid)).collect();
    tree.extend(sleeping);
    tree
}

fn summaries(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ToolCompleted {
                display_summary, ..
            } => Some(display_summary.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_worker_fixes_a_repository_tests_it_and_a_cancelled_recipe_takes_its_whole_tree() {
    let f = fixture();
    // Set up through the CLI, as a person would.
    let bodies = scripted_model(&f.profile, respond);
    let fixing = conversation(&f, "Fix the greeting");
    let slow = conversation(&f, "Run the slow recipe");
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let (fixed, stopped) = runtime.block_on(async {
        let running = assemble(&f.profile, approver.clone(), Supervision::Skip).await;
        let service = &running.service;

        // The worker changes the repository, looks at the diff, and runs the
        // project's test.
        let task = start(
            service,
            &fixing,
            "Make greeting.txt greet Tramontana, then run the project's test.",
        )
        .await;
        let events = finished(service, &task, Duration::from_secs(60)).await;
        assert_eq!(
            events.last().unwrap().kind,
            TaskEventKind::TaskCompleted,
            "{events:?}"
        );
        let summaries = summaries(&events);
        assert_eq!(summaries.len(), 4, "{summaries:?}");
        assert_eq!(summaries[3], "Recipe `test` passed");

        // The model was shown the diff, then the test's result.
        let seen: Vec<Value> = (0..5)
            .map(|_| bodies.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        let diff = observation(&seen[3]);
        assert!(
            diff.contains("-Hello, world") && diff.contains("+Hello, Tramontana"),
            "{diff}"
        );
        let result = observation(&seen[4]);
        assert!(
            result.contains("1 test passed") && result.contains("\"exitCode\":0"),
            "{result}"
        );

        // A person was asked twice: the edit, and the test with its exact
        // command. The read and the diff were not questions.
        let asked = approver.asked();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(
            asked[1]
                .details
                .iter()
                .any(|detail| detail.label == "Command" && detail.value == "sh check.sh"),
            "{:?}",
            asked[1].details
        );

        // The task recorded what it changed.
        let changed = events.iter().find_map(|event| match &event.payload {
            EventPayload::ArtifactRecorded { artifact } => Some(artifact.summary()),
            _ => None,
        });
        assert_eq!(
            changed.as_deref(),
            Some("1 file change(s): modified greeting.txt")
        );

        // A recipe is cancelled while its tree runs, beside an identical tree
        // Tramontana did not start.
        let bystander = Bystander::start(&f.root);
        let task2 = start(service, &slow, "Run the slow recipe.").await;
        let started = Instant::now();
        let tree = loop {
            let tree = recipe_tree(f.sleeps, &bystander);
            if tree.len() == 3 && bystander.tree(f.sleeps).len() == 3 {
                break tree;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the recipe's tree never started: {tree:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let untouched = bystander.tree(f.sleeps);
        service.cancel_task(&task2).await.unwrap();
        let events2 = finished(service, &task2, Duration::from_secs(60)).await;
        assert_eq!(
            events2.last().unwrap().kind,
            TaskEventKind::TaskCancelled,
            "{events2:?}"
        );
        let stopping = Instant::now();
        while tree.iter().any(|pid| is_alive(*pid)) {
            assert!(
                stopping.elapsed() < Duration::from_secs(5),
                "still running after the cancel: {:?}",
                tree.iter()
                    .filter(|pid| is_alive(**pid))
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            untouched.iter().all(|pid| is_alive(*pid)),
            "the cancel reached a process Tramontana did not start"
        );
        let asked = approver.asked();
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert!(
            asked[2]
                .details
                .iter()
                .any(|detail| detail.label == "Command" && detail.value == "sh slow.sh")
        );
        drop(bystander);
        (task, task2)
    });

    // The change is on disk and in Git.
    assert_eq!(
        std::fs::read_to_string(f.root.join("greeting.txt")).unwrap(),
        "Hello, Tramontana\n"
    );
    assert!(git(&f.root, &["diff", "--stat"]).contains("greeting.txt"));

    // And the record reads back from a separate process.
    let kinds = |task: &TaskId| -> Vec<String> {
        cli_json(&f.profile, &["task", "replay", task.as_str()])
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["kind"].as_str().unwrap().to_owned())
            .collect()
    };
    let fixed = kinds(&fixed);
    assert!(fixed.iter().any(|kind| kind == "artifactRecorded"));
    assert_eq!(fixed.last().map(String::as_str), Some("taskCompleted"));
    assert_eq!(
        kinds(&stopped).last().map(String::as_str),
        Some("taskCancelled")
    );
}
