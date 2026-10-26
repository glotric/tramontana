//! V4's desktop end-to-end run of the V3 scenario (`BUILD_ORDER.md` §19).
//!
//! The scenario V3 proved in a terminal, run again through the desktop's own
//! composition: the profile it opens, the tools it registers, the approver its
//! window answers through, the Changes panel's reads, and the bundle staging
//! behind its import. A model changes a file and runs the project's test, a
//! person approves each step, and the work is exported and read back into a
//! second profile.
//!
//! Real: SQLite, the OpenAI-compatible HTTP adapter over a socket, Git, the
//! filesystem and recipe tools, the command broker, the permission boundary,
//! and `AppState` as the window assembles it. Synthetic: the model, scripted
//! turn by turn, and the person, who answers whatever question is waiting.
//!
//! Not covered, and covered elsewhere: the webview and the JavaScript in it,
//! which `apps/desktop/src/*.test.tsx` holds to the same contracts; the IPC
//! marshalling of each command's arguments, which the generated contracts fix
//! at compile time on both sides; and the native pickers, which are the one
//! way a path enters and cannot be driven from a test. Tauri's WebDriver
//! support covers Linux and Windows, not macOS, where this is developed
//! (ADR 0033).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "an isolated fixture scenario; a panic is the failure report"
)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tramontana_core::contracts::ExportOptions;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ApprovalRequest, ConversationId, EventPayload, TaskEventKind, TaskId,
};
use tramontana_core::ports::{Clock as _, ToolAdmission};
use tramontana_core::services::TaskService;
use tramontana_persistence::{Database, SqliteProviderSettingsRepository};
use tramontana_platform::{SystemClock, canonicalize_root};

use tramontana_test_support::{call, git, observation, read_body, read_hash, reply};

use crate::approval::DesktopApprover;
use crate::state::AppState;

/// How long any single wait in the scenario may take before it is a failure.
const PATIENCE: Duration = Duration::from_secs(20);

/// A committed Git repository whose project declares the test recipe V3 ran,
/// and the profile the desktop opens beside it.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    profile: PathBuf,
    bundles: PathBuf,
}

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
    std::fs::write(
        root.join(".tramontana/project.md"),
        "---\nid: fixture\nrecipes:\n  test: [sh, check.sh]\n---\n# Fixture\n",
    )
    .unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "fixture"]);
    let bundles = temp.path().join("bundles");
    std::fs::create_dir_all(&bundles).unwrap();
    let profile = temp.path().join("desktop.sqlite");
    Fixture {
        _temp: temp,
        root,
        profile,
        bundles,
    }
}

/// Configure a scripted OpenAI-compatible endpoint in `profile` and serve it.
///
/// Written before the window opens, because that is when the desktop reads
/// what to assemble: an endpoint added while it is running takes effect the
/// next time it starts, which is what "Restart now" is for.
async fn scripted_model(profile: &Path) -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    let database = Database::open(profile).await.unwrap();
    let settings = SqliteProviderSettingsRepository::new(database);
    crate::settings::add(
        &settings,
        "fixture",
        None,
        crate::settings::NewEndpoint::OpenAiCompatible {
            base_url: base,
            model_id: "synthetic".to_owned(),
            credential_key: None,
        },
        SystemClock::new().now(),
    )
    .await
    .unwrap();
    crate::settings::toggle(
        &settings,
        "fixture",
        crate::settings::Toggle::ToolCalls(true),
    )
    .await
    .unwrap();
    listener
}

/// One scripted turn: what the model sends back for request number `turn`.
type Script = fn(usize, &Value) -> (Value, &'static str);

/// Serve the scripted turns until the listener is dropped, keeping every
/// request body so the test can see what the model was shown.
fn serve(listener: TcpListener, script: Script) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let bodies = Arc::clone(&seen);
    std::thread::spawn(move || {
        for turn in 0_usize.. {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            socket.set_read_timeout(Some(PATIENCE)).unwrap();
            let body = read_body(&mut socket);
            let (delta, finish) = script(turn, &body);
            if reply(&mut socket, &delta, finish).is_err() {
                return;
            }
            bodies.lock().unwrap().push(body);
        }
    });
    seen
}

/// Read the greeting, patch it using that read's hash, run the project's test,
/// and answer. V3's demonstration, turn for turn.
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
        2 => call(
            turn,
            body,
            "Run one of the commands",
            json!({ "recipe": "test" }),
        ),
        _ => (
            json!({ "content": "The greeting names Tramontana, and the test passes." }),
            "stop",
        ),
    }
}

/// A person at the window, answering whatever question is waiting.
///
/// Through the same two calls the window makes: `pending_approvals` to see
/// what is being asked, and `resolve_approval` to answer it.
fn a_person(
    approver: Arc<DesktopApprover>,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<ApprovalRequest>>>,
) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&asked);
    let handle = tokio::spawn(async move {
        loop {
            for request in approver.pending() {
                recorded.lock().unwrap().push(request.clone());
                approver.resolve(&request.approval_id, ApprovalDecisionKind::AllowOnce);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    (handle, asked)
}

/// The task's events once its record is closed.
///
/// Waits for a terminal *event*, not a terminal task state. The row is
/// updated when the state changes, and the events that close the record —
/// what the task changed, and how it ended — are written after it, so a
/// reader that waits on the row can see a finished task whose record is
/// still being written.
async fn finished(
    service: &TaskService,
    task: &TaskId,
) -> Vec<tramontana_core::contracts::EventEnvelope> {
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            let events = service.replay_task_events(task, 0).await.unwrap();
            if events
                .last()
                .is_some_and(tramontana_core::contracts::EventEnvelope::is_terminal_outcome)
            {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    match waited {
        Ok(events) => events,
        Err(_) => {
            let events = service.replay_task_events(task, 0).await.unwrap();
            let seen: Vec<String> = events
                .iter()
                .map(|event| format!("{:?} {:?}", event.kind, event.payload))
                .collect();
            panic!("the task never ended:\n{}", seen.join("\n"))
        }
    }
}

fn summaries(events: &[tramontana_core::contracts::EventEnvelope]) -> Vec<String> {
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

#[tokio::test]
async fn the_desktop_carries_the_v3_scenario_from_a_project_to_a_bundle() {
    let f = fixture();
    let listener = scripted_model(&f.profile).await;
    let seen = serve(listener, respond);
    let approver = Arc::new(DesktopApprover::waiting());
    let (person, asked) = a_person(Arc::clone(&approver));

    // The window opens on the profile, assembling what the person configured.
    let state = AppState::open_with_chunk_delay(&f.profile, 0, Arc::clone(&approver))
        .await
        .unwrap();

    // 1. Open the project. Its configuration resolves from disk, as the
    //    inspector shows it.
    let root = canonicalize_root(&f.root).unwrap();
    let opened = crate::commands::open_root(&state, &root).await.unwrap();
    let project_id = opened.project.project_id.clone();
    assert!(
        opened
            .configuration
            .as_ref()
            .is_some_and(|configuration| configuration.diagnostics.is_empty()),
        "{:?}",
        opened.configuration
    );

    // 2. A conversation, and a task the person allowed to write and to run the
    //    project's recipes. Admission only lets the model ask (ADR 0031).
    let conversation = state
        .service
        .create_conversation(Some(project_id.clone()), "Fix the greeting".to_owned())
        .await
        .unwrap();
    let task = start(&state, &conversation.conversation_id).await;

    // 3. The model reads, patches and runs the test; the person approves the
    //    two that need approving.
    let events = finished(&state.service, &task).await;
    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    let ran = summaries(&events);
    assert_eq!(ran.len(), 3, "{ran:?}");
    assert_eq!(ran[2], "Recipe `test` passed");

    // The read was not a question; the write and the command were, and the
    // command's question named the exact command (ADR 0028).
    let asked = asked.lock().unwrap().clone();
    let questions: Vec<&str> = asked
        .iter()
        .map(|request| request.tool_id.as_str())
        .collect();
    assert_eq!(
        questions,
        ["builtin.fs.patch", "builtin.recipe.run"],
        "{questions:?}"
    );
    assert!(
        asked[1]
            .details
            .iter()
            .any(|detail| detail.label == "Command" && detail.value == "sh check.sh"),
        "{:?}",
        asked[1].details
    );

    // 4. The Changes panel reads the working tree the person is reviewing.
    let changes = crate::commands::project_changes_on(&state, &project_id)
        .await
        .unwrap();
    assert!(
        changes.diff.contains("-Hello, world") && changes.diff.contains("+Hello, Tramontana"),
        "{}",
        changes.diff
    );
    assert!(!changes.truncated);

    // The change is on disk, and the task recorded what it changed.
    assert_eq!(
        std::fs::read_to_string(f.root.join("greeting.txt")).unwrap(),
        "Hello, Tramontana\n"
    );
    let artifact = events.iter().find_map(|event| match &event.payload {
        EventPayload::ArtifactRecorded { artifact } => Some(artifact.summary()),
        _ => None,
    });
    assert_eq!(
        artifact.as_deref(),
        Some("1 file change(s): modified greeting.txt")
    );

    // The model was shown the test's own output, not a summary of it.
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 4, "{} model turns", bodies.len());
    let result = observation(&bodies[3]);
    assert!(
        result.contains("1 test passed") && result.contains("\"exitCode\":0"),
        "{result}"
    );

    // 5. Export what happened, and read it back into a profile that has never
    //    seen this project. The bundle is what leaves; the import runs none of
    //    it.
    let written = crate::bundles::export_to(
        &state.service,
        &crate::bundles::ExportScope::Task(task.clone()),
        ExportOptions {
            include_absolute_paths: false,
            include_raw_tool_output: false,
            include_artifacts: false,
        },
        &f.bundles,
    )
    .await
    .unwrap();
    assert!(!written.entries.is_empty());
    assert_eq!(written.task_ids, std::slice::from_ref(&task));

    person.abort();
    state.shut_down().await;
    drop(state);

    let elsewhere = f.bundles.parent().unwrap().join("second.sqlite");
    let second =
        AppState::open_with_chunk_delay(&elsewhere, 0, Arc::new(DesktopApprover::headless()))
            .await
            .unwrap();
    // Inspected first, staged, and imported by the identifier the inspection
    // was given: the window confirms what it was shown, not a path.
    let inspection = crate::bundles::inspect(&second.service, &f.bundles).unwrap();
    assert_eq!(inspection.contains.tasks, 1);
    crate::bundles::stage(
        &second.staged_bundle,
        "staging-1".to_owned(),
        f.bundles.clone(),
    );
    let staged = crate::bundles::take_staged(&second.staged_bundle, "staging-1").unwrap();
    let report = crate::bundles::import_from(&second.service, &staged)
        .await
        .unwrap();
    assert!(!report.executed_anything);
    assert_eq!(report.imported.tasks, 1);

    // The record reads back whole, in the same order, in a profile that did
    // none of the work.
    let there = second.service.replay_task_events(&task, 0).await.unwrap();
    let kinds = |events: &[tramontana_core::contracts::EventEnvelope]| -> Vec<TaskEventKind> {
        events.iter().map(|event| event.kind).collect()
    };
    assert_eq!(kinds(&there), kinds(&events));
    second.shut_down().await;
}

/// Start a task as the composer does: this conversation, allowed to write and
/// to run the project's recipes, under the profile's routing mode.
async fn start(state: &AppState, conversation: &ConversationId) -> TaskId {
    crate::commands::start_task_on(
        state,
        "Make greeting.txt greet Tramontana, then run the project's test.".to_owned(),
        Some(conversation.clone()),
        crate::commands::TaskChoices {
            admission: Some(ToolAdmission::READ_WRITE.with_recipes()),
            ..crate::commands::TaskChoices::default()
        },
        |_| true,
    )
    .await
    .unwrap()
}

/// The stand-in MCP server, built beside this test by a workspace build.
fn fake_mcp_server() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .expect("the test binary lives under the target directory")
        .join(format!(
            "tramontana-fake-mcp-server{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        path.exists(),
        "{} is missing; build the workspace (`cargo test --workspace`) first",
        path.display()
    );
    path
}

/// The model greets through the App's tool once, then answers.
fn greet(turn: usize, body: &Value) -> (Value, &'static str) {
    match turn {
        0 => call(turn, body, "Says hello", json!({ "name": "Ada" })),
        _ => (json!({ "content": "The App said hello." }), "stop"),
    }
}

/// The Apps screen, from the running window's side (ADR 0040): what an App
/// offers and whether a model may be offered it, how it is doing, what was
/// decided about its tools, and turning it off — with no value of anything
/// it is given, and its program by file name only.
#[tokio::test]
async fn the_apps_screen_shows_an_app_its_tools_its_decisions_and_nothing_secret() {
    let f = fixture();
    let listener = scripted_model(&f.profile).await;
    // Configured as the CLI's `app add` does, before the window opens.
    {
        let database = Database::open(&f.profile).await.unwrap();
        let settings = tramontana_persistence::SqliteAppSettingsRepository::new(database);
        tramontana_core::ports::AppSettingsRepository::save(
            &settings,
            &tramontana_core::contracts::AppConfig {
                schema_version: tramontana_core::contracts::SCHEMA_VERSION,
                app_id: "fixture".to_owned(),
                display_name: "Fixture".to_owned(),
                enabled: true,
                launch: tramontana_core::contracts::AppLaunch {
                    program: fake_mcp_server().display().to_string(),
                    arguments: vec!["ok".to_owned()],
                    working_directory: None,
                    environment: vec![tramontana_core::contracts::AppVariable {
                        name: "GREETING_STYLE".to_owned(),
                        value: "a-value-that-must-not-appear".to_owned(),
                    }],
                    secrets: Vec::new(),
                },
                remote: None,
                created_at: tramontana_core::contracts::Timestamp::new("2026-01-01T00:00:00.000Z"),
            },
        )
        .await
        .unwrap();
    }
    let _seen = serve(listener, greet);
    let approver = Arc::new(DesktopApprover::waiting());
    let (person, _asked) = a_person(Arc::clone(&approver));
    let state = AppState::open_with_chunk_delay(&f.profile, 0, Arc::clone(&approver))
        .await
        .unwrap();

    // What it offers, and how it is doing.
    let apps = crate::apps::list(&state).await.unwrap();
    assert_eq!(apps.len(), 1, "{apps:?}");
    let app = &apps[0];
    assert_eq!(app.status, crate::apps::AppStatus::Running);
    assert_eq!(
        app.program,
        format!("tramontana-fake-mcp-server{}", std::env::consts::EXE_SUFFIX)
    );
    assert_eq!(app.variables, ["GREETING_STYLE"]);
    assert!(app.protocol_version.is_some());
    let offered: Vec<(&str, bool)> = app
        .tools
        .iter()
        .map(|tool| (tool.tool_id.as_str(), tool.offered))
        .collect();
    assert!(
        offered.contains(&("mcp.fixture.greeting", true)),
        "{offered:?}"
    );
    assert!(
        offered.contains(&("mcp.fixture.stamp", false)),
        "{offered:?}"
    );
    // Nothing the App is given, and no folder, reaches the screen.
    let shown = serde_json::to_string(&apps).unwrap();
    assert!(!shown.contains("a-value-that-must-not-appear"), "{shown}");
    assert!(
        !shown.contains(fake_mcp_server().parent().unwrap().to_str().unwrap()),
        "{shown}"
    );

    // A task that admits Apps calls its tool, and the person allows it.
    let root = canonicalize_root(&f.root).unwrap();
    let opened = crate::commands::open_root(&state, &root).await.unwrap();
    let conversation = state
        .service
        .create_conversation(Some(opened.project.project_id.clone()), "Greet".to_owned())
        .await
        .unwrap();
    let task = crate::commands::start_task_on(
        &state,
        "Ask the App to greet Ada.".to_owned(),
        Some(conversation.conversation_id.clone()),
        crate::commands::TaskChoices {
            admission: Some(ToolAdmission::READ_ONLY.with_apps()),
            ..crate::commands::TaskChoices::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    let events = finished(&state.service, &task).await;
    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );

    // What was decided about it: that call, and nothing about built-in tools.
    let history = crate::apps::history(&state.service, "fixture")
        .await
        .unwrap();
    assert!(!history.partial);
    assert_eq!(history.decisions.len(), 1, "{:?}", history.decisions);
    let decision = &history.decisions[0];
    assert_eq!(decision.tool_id, "mcp.fixture.greeting");
    assert!(decision.allowed);
    assert_eq!(decision.outcome.as_deref(), Some("completed"));
    assert_eq!(decision.task_id, task);
    // Another App's history is its own.
    assert!(
        crate::apps::history(&state.service, "other")
            .await
            .unwrap()
            .decisions
            .is_empty()
    );

    // Turned off, it keeps running until the next start, and says so.
    crate::apps::set_enabled(state.app_settings.as_ref(), "fixture", false)
        .await
        .unwrap();
    let apps = crate::apps::list(&state).await.unwrap();
    assert_eq!(apps[0].status, crate::apps::AppStatus::StopsOnRestart);
    assert!(!apps[0].enabled);
    assert!(
        crate::apps::set_enabled(state.app_settings.as_ref(), "missing", true)
            .await
            .is_err()
    );

    person.abort();
    state.shut_down().await;
}
