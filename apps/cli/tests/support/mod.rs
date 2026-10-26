//! Shared by the CLI's end-to-end tests: the CLI binary, Git, a fixture
//! repository, a scripted model, the CLI's own assembly with a chosen
//! approver, and a look at real processes.

#![allow(
    dead_code,
    unused_imports,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a panic is the failure report, and each test file uses a part of it"
)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tramontana_adapters_tools::{FilesystemTools, GitTools, RecipeTools};
use tramontana_composition::{
    AppPlane, ModelPlane, Supervision, assemble_models, assemble_worker, recovery_store, start_apps,
};
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{ConversationId, EventEnvelope, TaskEventKind, TaskId};
use tramontana_core::ports::{
    AppSettingsRepository, ApprovalStore, Approver, EventStore, ProviderSettingsRepository,
    SecretResolver, ToolAdmission, ToolProvider,
};
use tramontana_core::services::{Providers, Repositories, StartTaskRequest, TaskService};
use tramontana_core::testing::{InMemoryApprovalStore, InMemorySecretVault, assemble_tool_plane};
use tramontana_persistence::{
    Database, SqliteAppSettingsRepository, SqliteCheckpointRepository,
    SqliteConfigurationRepository, SqliteConversationRepository, SqliteEventStore,
    SqliteProjectRepository, SqliteProviderSettingsRepository, SqliteTaskRepository,
};
use tramontana_platform::{SystemClock, UuidIdSource, canonicalize_root, discover};
pub use tramontana_test_support::{answer, call, git, observation, read_body, read_hash, reply};

/// Run the CLI against `profile`, requiring success.
pub fn cli(profile: &Path, args: &[&str]) -> Output {
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

pub fn cli_json(profile: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&cli(profile, args).stdout).unwrap()
}

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub root: PathBuf,
    pub profile: PathBuf,
    /// The durations `slow.sh` sleeps for, unique to this run so no other
    /// process is mistaken for its tree.
    pub sleeps: [u32; 2],
}

/// A committed Git repository whose project declares two recipes: a test that
/// passes only once the greeting names Tramontana, and a slow process tree.
pub fn fixture() -> Fixture {
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
    // Unique to this process and to each fixture in it, so tests running side
    // by side never see each other's sleeps.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let base = 3000
        + (std::process::id() % 2500) * 2
        + NEXT.fetch_add(2, std::sync::atomic::Ordering::SeqCst) * 5000;
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
    let profile = temp.path().join("tramontana.sqlite");
    Fixture {
        temp,
        root,
        profile,
        sleeps,
    }
}

/// A conversation in the fixture's project, created through the CLI.
pub fn conversation(f: &Fixture, title: &str) -> ConversationId {
    let project = cli_json(&f.profile, &["project", "open", f.root.to_str().unwrap()]);
    let project_id = project["project"]["projectId"].as_str().unwrap();
    let created = cli_json(
        &f.profile,
        &["conversation", "create", title, "--project-id", project_id],
    );
    ConversationId::new(created["conversationId"].as_str().unwrap())
}

/// One scripted turn: what the model sends back for request number `turn`.
pub type Script = fn(usize, &Value) -> (Value, &'static str);

/// Configure a scripted OpenAI-compatible model for `profile` and serve it.
/// Each request gets the next turn of `script`, and its body is passed on.
pub fn scripted_model(profile: &Path, script: Script) -> mpsc::Receiver<Value> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    cli(
        profile,
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
    cli(profile, &["provider", "tool-calls", "fixture"]);
    let (sender, bodies) = mpsc::channel();
    std::thread::spawn(move || serve(listener, sender, script));
    bodies
}

fn serve(listener: TcpListener, bodies: mpsc::Sender<Value>, script: Script) {
    for turn in 0_usize.. {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let body = read_body(&mut socket);
        let (delta, finish) = script(turn, &body);
        reply(&mut socket, &delta, finish).unwrap();
        if bodies.send(body).is_err() {
            return;
        }
    }
}

pub struct Running {
    pub service: Arc<TaskService>,
    /// Where grants live, so a test can revoke one and see what changes.
    pub approvals: Arc<InMemoryApprovalStore>,
    _models: ModelPlane,
    /// The profile's Apps, stopped when this is dropped.
    _apps: AppPlane,
}

/// The CLI's assembly (`wiring.rs`), with `approver` in place of the terminal
/// prompt, and what `--allow-writes --allow-recipes` admits. `supervision`
/// starts a managed `llama-server` when the profile has one.
pub async fn assemble(
    profile: &Path,
    approver: Arc<dyn Approver>,
    supervision: Supervision,
) -> Running {
    let database = Database::open(profile).await.unwrap();
    let resolver = Arc::new(InMemorySecretVault::new()) as Arc<dyn SecretResolver>;
    // The profile's Apps, as the CLI starts them: registered beside the
    // built-in tools and under the same permission boundary (ADR 0036).
    let configured_apps = SqliteAppSettingsRepository::new(database.clone())
        .list()
        .await
        .unwrap();
    let apps = start_apps(&configured_apps, &resolver, profile).await;
    assert!(apps.failures.is_empty(), "{:?}", apps.failures);
    let app_tools = apps.tool_ids();
    let mut providers: Vec<Arc<dyn ToolProvider>> = vec![
        Arc::new(FilesystemTools::new().with_recovery_store(recovery_store(profile)))
            as Arc<dyn ToolProvider>,
        Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
        Arc::new(RecipeTools::new()) as Arc<dyn ToolProvider>,
    ];
    providers.extend(apps.providers());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        providers,
        Arc::clone(&approvals) as Arc<dyn ApprovalStore>,
        approver,
        Arc::new(UuidIdSource::new()),
    )
    .unwrap();
    let configured = SqliteProviderSettingsRepository::new(database.clone())
        .list()
        .await
        .unwrap();
    let models = assemble_models(&configured, &resolver, supervision).await;
    assert!(models.failures.is_empty(), "{:?}", models.failures);
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
            worker: assemble_worker(
                true,
                ToolAdmission::READ_WRITE.with_recipes().with_apps(),
                &app_tools,
            ),
        },
        Arc::new(SystemClock::new()),
        Arc::new(UuidIdSource::new()),
    );
    service.recover().await.unwrap();
    Running {
        service: Arc::new(service),
        approvals,
        _models: models,
        _apps: apps,
    }
}

/// Start a task as `tramontana task` does: the project's configuration is
/// resolved from disk first.
pub async fn start(service: &TaskService, conversation: &ConversationId, goal: &str) -> TaskId {
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
            ..Default::default()
        })
        .await
        .unwrap()
}

/// The task's events once it has ended, waiting at most `limit`.
///
/// Ended means the terminal event is durable *and* the task row's cache has
/// caught up with it. The journal writes the event first and the cache a
/// moment after, so a caller that saw only the event could read a task row
/// one sequence behind — which a slow CI runner did.
pub async fn finished(service: &TaskService, task: &TaskId, limit: Duration) -> Vec<EventEnvelope> {
    let deadline = Instant::now() + limit;
    loop {
        let events = service.replay_task_events(task, 0).await.unwrap();
        let ended = events.last().filter(|event| {
            matches!(
                event.kind,
                TaskEventKind::TaskCompleted
                    | TaskEventKind::TaskFailed
                    | TaskEventKind::TaskCancelled
            )
        });
        if let Some(last) = ended
            && service.get_task(task).await.unwrap().last_sequence == last.sequence
        {
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
pub fn is_alive(pid: i32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available to tell whether a process is running");
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

pub fn parent(pid: i32) -> Option<i32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available");
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// The running processes that are this fixture's sleeps.
pub fn sleepers(sleeps: [u32; 2]) -> Vec<i32> {
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
