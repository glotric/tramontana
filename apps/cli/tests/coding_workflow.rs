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

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tramontana_adapters_tools::{FilesystemTools, GitTools, RecipeTools};
use tramontana_composition::{
    ModelPlane, Supervision, assemble_models, assemble_worker, recovery_store,
};
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ConversationId, EventEnvelope, EventPayload, TaskEventKind, TaskId,
};
use tramontana_core::ports::{
    ApprovalStore, Approver, EventStore, ProviderSettingsRepository, SecretResolver, ToolAdmission,
    ToolProvider,
};
use tramontana_core::services::{Providers, Repositories, StartTaskRequest, TaskService};
use tramontana_core::testing::{
    InMemoryApprovalStore, InMemorySecretVault, ScriptedApprover, assemble_tool_plane,
};
use tramontana_persistence::{
    Database, SqliteCheckpointRepository, SqliteConfigurationRepository,
    SqliteConversationRepository, SqliteEventStore, SqliteProjectRepository,
    SqliteProviderSettingsRepository, SqliteTaskRepository,
};
use tramontana_platform::{SystemClock, UuidIdSource, canonicalize_root, discover};

fn cli(profile: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args(args)
        .arg("--profile")
        .arg(profile)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn cli_json(profile: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&cli(profile, args).stdout).unwrap()
}

/// Git, unaffected by whoever runs the test.
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    profile: PathBuf,
    /// The durations `slow.sh` sleeps for, unique to this run so no other
    /// process is mistaken for its tree.
    sleeps: [u32; 2],
}

/// A committed Git repository whose project declares two recipes: a test that
/// passes only once the greeting names Tramontana, and a slow process tree.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    std::fs::create_dir_all(root.join(".tramontana")).unwrap();
    std::fs::write(root.join("greeting.txt"), "Hello, world\n").unwrap();
    std::fs::write(
        root.join("check.sh"),
        "grep -qx 'Hello, Tramontana' greeting.txt || { echo 'greeting.txt does not greet Tramontana'; exit 1; }\n\
         echo '1 test passed'\n",
    )
    .unwrap();
    let base = 3000 + std::process::id() % 5000;
    let sleeps = [base, base + 1];
    std::fs::write(
        root.join("slow.sh"),
        format!("sleep {} &\nsleep {} &\nwait\n", sleeps[0], sleeps[1]),
    )
    .unwrap();
    std::fs::write(
        root.join(".tramontana/project.md"),
        "---\nid: fixture\nrecipes:\n  test: [sh, check.sh]\n  slow: [sh, slow.sh]\n---\n# Fixture\n",
    )
    .unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "fixture"]);
    let profile = temp.path().join("m7.sqlite");
    Fixture {
        _temp: temp,
        root,
        profile,
        sleeps,
    }
}

/// A synthetic OpenAI-compatible model. Each request gets the next scripted
/// turn, and its body is passed to the test.
fn serve(listener: TcpListener, bodies: mpsc::Sender<Value>) {
    for turn in 0_usize.. {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let body = read_body(&mut socket);
        let (delta, finish) = respond(turn, &body);
        let payload = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":delta,"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":finish}]})
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        socket.write_all(response.as_bytes()).unwrap();
        if bodies.send(body).is_err() {
            return;
        }
    }
}

fn read_body(socket: &mut std::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let read = socket.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
        assert!(bytes.len() < 2_000_000);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let len: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + len {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + len]).unwrap();
            }
        }
    }
}

/// The model's side of the conversation. It picks tools by description,
/// because the names on the wire are aliases, and takes the edit's expected
/// hash from what the read returned.
fn respond(turn: usize, body: &Value) -> (Value, &'static str) {
    let call = |description: &str, arguments: Value| {
        let call = json!({
            "index": 0,
            "id": format!("call-{turn}"),
            "type": "function",
            "function": { "name": tool_named(body, description), "arguments": arguments.to_string() },
        });
        (json!({ "tool_calls": [call] }), "tool_calls")
    };
    match turn {
        0 => call("Read a bounded", json!({ "path": "greeting.txt" })),
        1 => {
            let seen = observation(body);
            let read: Value = serde_json::from_str(&seen).unwrap();
            // The adapter wraps the brokered result: `{content, truncated}`.
            let hash = read["content"]["output"]["contentHash"]
                .as_str()
                .unwrap_or_else(|| panic!("no contentHash in the read the model saw: {seen}"))
                .to_owned();
            call(
                "Replace one exact occurrence",
                json!({
                    "path": "greeting.txt",
                    "expectedHash": hash,
                    "oldText": "Hello, world",
                    "newText": "Hello, Tramontana",
                }),
            )
        }
        2 => call("Show uncommitted changes", json!({})),
        3 => call("Run one of the commands", json!({ "recipe": "test" })),
        4 => (
            json!({ "content": "The greeting now names Tramontana, and the project's test passes." }),
            "stop",
        ),
        _ => call("Run one of the commands", json!({ "recipe": "slow" })),
    }
}

fn tool_named(body: &Value, description: &str) -> Value {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| {
            tool["function"]["description"]
                .as_str()
                .unwrap()
                .starts_with(description)
        })
        .unwrap_or_else(|| panic!("no tool described as {description:?} was offered"))["function"]
        ["name"]
        .clone()
}

/// The latest tool result the model was shown.
fn observation(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

struct Running {
    service: Arc<TaskService>,
    approver: Arc<ScriptedApprover>,
    _models: ModelPlane,
}

/// The CLI's assembly (`wiring.rs`), with a scripted approver answering
/// "allow once" in place of the terminal prompt, and what
/// `--allow-writes --allow-recipes` admits.
async fn assemble(profile: &Path) -> Running {
    let database = Database::open(profile).await.unwrap();
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![
            Arc::new(FilesystemTools::new().with_recovery_store(recovery_store(profile)))
                as Arc<dyn ToolProvider>,
            Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
            Arc::new(RecipeTools::new()) as Arc<dyn ToolProvider>,
        ],
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        approver.clone() as Arc<dyn Approver>,
        Arc::new(UuidIdSource::new()),
    )
    .unwrap();
    let configured = SqliteProviderSettingsRepository::new(database.clone())
        .list()
        .await
        .unwrap();
    let resolver = Arc::new(InMemorySecretVault::new()) as Arc<dyn SecretResolver>;
    let models = assemble_models(&configured, &resolver, Supervision::Skip).await;
    assert!(models.failures.is_empty());
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(SqliteProjectRepository::new(database.clone())),
            conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
            tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
            events: Arc::new(SqliteEventStore::new(database.clone())) as Arc<dyn EventStore>,
            checkpoints: Arc::new(SqliteCheckpointRepository::new(database.clone())),
            configuration: Arc::new(SqliteConfigurationRepository::new(database)),
        },
        Providers {
            models: models.providers.clone(),
            tools,
            permissions,
            releases,
            worker: assemble_worker(true, ToolAdmission::READ_WRITE.with_recipes()),
        },
        Arc::new(SystemClock::new()),
        Arc::new(UuidIdSource::new()),
    );
    service.recover().await.unwrap();
    Running {
        service: Arc::new(service),
        approver,
        _models: models,
    }
}

/// Start a task as `tramontana task` does: the project's configuration is
/// resolved from disk first.
async fn start(service: &TaskService, conversation: &ConversationId, goal: &str) -> TaskId {
    let project_id = service
        .get_conversation(conversation)
        .await
        .unwrap()
        .project_id
        .unwrap();
    let project = service.get_project(&project_id).await.unwrap();
    let root = canonicalize_root(Path::new(&project.roots[0])).unwrap();
    service
        .resolve_configuration(
            &project_id,
            discover(&root),
            &ConfigurationSelection::default(),
        )
        .await
        .unwrap();
    service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.clone()),
            goal: goal.to_owned(),
        })
        .await
        .unwrap()
}

async fn finished(service: &TaskService, task: &TaskId) -> Vec<EventEnvelope> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let events = service.replay_task_events(task, 0).await.unwrap();
        if events.last().is_some_and(|event| {
            matches!(
                event.kind,
                TaskEventKind::TaskCompleted
                    | TaskEventKind::TaskFailed
                    | TaskEventKind::TaskCancelled
            )
        }) {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "the task never ended: {events:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Whether a process is running. A zombie has exited, so it counts as gone.
/// Without `ps` this panics rather than calling every process gone.
fn is_alive(pid: i32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available to tell whether a process is running");
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

fn parent(pid: i32) -> Option<i32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available");
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// The running processes that are this run's sleeps.
fn sleepers(sleeps: [u32; 2]) -> Vec<i32> {
    let output = Command::new("pgrep")
        .args(["-f", &format!("^sleep ({}|{})$", sleeps[0], sleeps[1])])
        .output()
        .expect("`pgrep` must be available");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| is_alive(*pid))
        .collect()
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    // Set up through the CLI, as a person would.
    cli(
        &f.profile,
        &[
            "provider",
            "add-openai",
            "fixture",
            "--base-url",
            &base,
            "--model",
            "synthetic",
        ],
    );
    cli(&f.profile, &["provider", "tool-calls", "fixture"]);
    let project = cli_json(&f.profile, &["project", "open", f.root.to_str().unwrap()]);
    let project_id = project["project"]["projectId"].as_str().unwrap().to_owned();
    let conversation = |title: &str| {
        let created = cli_json(
            &f.profile,
            &["conversation", "create", title, "--project-id", &project_id],
        );
        ConversationId::new(created["conversationId"].as_str().unwrap())
    };
    let fixing = conversation("Fix the greeting");
    let slow = conversation("Run the slow recipe");

    let (sender, bodies) = mpsc::channel();
    std::thread::spawn(move || serve(listener, sender));
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let (fixed, stopped) = runtime.block_on(async {
        let running = assemble(&f.profile).await;
        let service = &running.service;

        // The worker changes the repository, looks at the diff, and runs the
        // project's test.
        let task = start(
            service,
            &fixing,
            "Make greeting.txt greet Tramontana, then run the project's test.",
        )
        .await;
        let events = finished(service, &task).await;
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
        let asked = running.approver.asked();
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
        let events2 = finished(service, &task2).await;
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
        let asked = running.approver.asked();
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
