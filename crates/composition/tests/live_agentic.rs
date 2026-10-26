//! The browser and a handoff with a real model, assembled as both
//! applications assemble them: a real `mlx_lm.server`, the real file and
//! browser tools, the reference worker and the task service.
//!
//! Runs only when `TRAMONTANA_LIVE_MLX_SERVER` names an `mlx_lm.server` and
//! `TRAMONTANA_LIVE_MLX_MODEL` an MLX model folder whose tool calls work, and
//! the browser part only where Chrome, Edge or Chromium is installed.
//! Otherwise each test says so and passes, as the other live variants do.
//! What a small model does is not deterministic; these check that the
//! product lets it succeed, and print what it did.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tramontana_adapters_tools::{FilesystemTools, GitTools, RecipeTools};
use tramontana_composition::{
    ModelPlane, Supervision, assemble_models, offered_tools, pinnable_models, pinned_worker_choice,
    worker_choice,
};
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

fn live() -> Option<(String, String)> {
    match (
        std::env::var("TRAMONTANA_LIVE_MLX_SERVER"),
        std::env::var("TRAMONTANA_LIVE_MLX_MODEL"),
    ) {
        (Ok(server), Ok(model)) => Some((server, model)),
        _ => {
            eprintln!("skipped: set TRAMONTANA_LIVE_MLX_SERVER and TRAMONTANA_LIVE_MLX_MODEL");
            None
        }
    }
}

fn at() -> Timestamp {
    Timestamp::new("2026-09-22T12:00:00.000Z")
}

fn mlx(server: &str, model: &str) -> ProviderEndpoint {
    let mut endpoint = ProviderEndpoint::new(
        "mlx",
        "Qwen 2.5 1.5B (MLX)",
        ProviderConfig::MlxServer {
            binary_path: server.to_owned(),
            model_path: model.to_owned(),
            model_id: "qwen".to_owned(),
            extra_args: Vec::new(),
            provenance: None,
        },
        at(),
    );
    endpoint.supports_tool_calls = true;
    endpoint
}

/// A local endpoint nothing answers at, as a model whose runtime stopped.
fn unreachable() -> ProviderEndpoint {
    let mut endpoint = ProviderEndpoint::new(
        "gone",
        "A model that stopped",
        ProviderConfig::OpenAiCompatible {
            base_url: "http://127.0.0.1:9".to_owned(),
            model_id: "gone".to_owned(),
            credential: None,
        },
        at(),
    );
    endpoint.supports_tool_calls = true;
    endpoint
}

/// The service both applications build, over `plane`, with `worker` as the
/// one a task gets when it names no admission.
async fn service(
    plane: &ModelPlane,
    tools: Vec<Arc<dyn ToolProvider>>,
    worker: Arc<dyn AgentProvider>,
) -> TaskService {
    let ids = Arc::new(SequentialIdSource::new());
    let (registry, permissions, releases) = assemble_tool_plane(
        tools,
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowSite)),
        ids.clone(),
    )
    .unwrap();
    let events = Arc::new(InMemoryEventStore::new());
    TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: plane.providers.clone(),
            tools: registry,
            permissions,
            releases,
            worker,
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(RoutingMode::LocalOnly)
    .with_worker_choice(worker_choice(true, Vec::new()))
    .with_pinned_workers(
        pinnable_models(&plane.providers).await,
        pinned_worker_choice(Vec::new()),
    )
}

/// Wait until the task ended, or stopped for another worker.
async fn settled(service: &TaskService, task: &TaskId, after: usize) -> Vec<EventEnvelope> {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let events = service.replay_task_events(task, 0).await.unwrap();
            let fresh = &events[after.min(events.len())..];
            let ended = fresh.iter().any(|event| {
                matches!(
                    event.kind,
                    TaskEventKind::TaskCompleted
                        | TaskEventKind::TaskFailed
                        | TaskEventKind::TaskCancelled
                )
            });
            let stopped = fresh
                .last()
                .is_some_and(|event| event.kind == TaskEventKind::CheckpointSaved)
                && fresh.iter().any(|event| matches!(&event.payload,
                    EventPayload::TaskStateChanged { reason_code, .. } if reason_code == "worker.stopped"));
            if ended || stopped {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}

/// What the task did, one line per step, for the evidence note.
fn narrate(events: &[EventEnvelope]) {
    for event in events {
        let line = match &event.payload {
            EventPayload::ToolRequested { tool_id, .. } => format!("asked for {tool_id}"),
            EventPayload::PermissionDecided {
                allowed,
                reason_code,
                ..
            } => format!("  permission: {allowed} ({reason_code})"),
            EventPayload::ToolCompleted {
                display_summary, ..
            } => format!("  done: {display_summary}"),
            EventPayload::ToolFailed { error, .. } => format!("  failed: {:?}", error.code),
            EventPayload::WorkerSelected { descriptor } => {
                format!("worker: {}", descriptor.display_name)
            }
            EventPayload::DiagnosticRecorded { code, summary }
                if !code.starts_with("reference.budget") =>
            {
                format!("note {code}: {summary}")
            }
            EventPayload::TaskCompleted { summary, .. } => format!("completed: {summary}"),
            EventPayload::TaskFailed { error } => {
                format!("failed: {:?}: {}", error.code, error.message)
            }
            _ => continue,
        };
        eprintln!("  {line}");
    }
}

/// A site whose first page links to a second that holds a word.
async fn site() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0; 8192];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = if request.starts_with("GET /answer ") {
                    "<title>Answer</title><h1>The answer</h1><p>The secret word is <b>marigold</b>.</p>"
                } else {
                    "<title>Welcome</title><h1>Welcome</h1><p>The secret word is not on this page.</p><a href=\"/answer\">The answer page</a>"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    base
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_model_browses_to_an_answer() {
    let Some((server, model)) = live() else {
        return;
    };
    let Some(browser_path) = tramontana_adapters_browser_path() else {
        eprintln!("skipped: no Chrome, Edge or Chromium is installed");
        return;
    };
    let mut plane = assemble_models(
        &[mlx(&server, &model)],
        &(Arc::new(InMemorySecretVault::new()) as Arc<dyn SecretResolver>),
        Supervision::Start,
    )
    .await;
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
    let profiles = tempfile::tempdir().unwrap();
    let browser = Arc::new(tramontana_composition::BrowserProvider::new(
        tramontana_adapters_browser_options(browser_path, profiles.path()),
    ));
    let admission = ToolAdmission::READ_ONLY.with_browse();
    let service = service(
        &plane,
        vec![
            Arc::new(FilesystemTools::new()) as Arc<dyn ToolProvider>,
            Arc::new(GitTools::new()),
            Arc::new(RecipeTools::new()),
            Arc::clone(&browser) as Arc<dyn ToolProvider>,
        ],
        Arc::new(
            ReferenceWorker::new(offered_tools(admission, &[]), ReadOnlyLimits::default())
                .admitting(admission),
        ),
    )
    .await;

    let base = site().await;
    // One step: a page is opened and read. Required.
    let (answer, _) = browse(
        &service,
        format!("Use the browser to open {base}/answer and tell me the secret word written there."),
        "one step",
    )
    .await;
    // Two steps: a link must be followed. Reported, not required: a model
    // of 1.5B parameters answered from the first page every time
    // (docs/evidence/M12-hardening.md).
    let (_, followed) = browse(
        &service,
        format!(
            "Use the browser to open {base}/ , follow the link to the answer page, and tell me the secret word written there."
        ),
        "two steps",
    )
    .await;
    eprintln!(
        "two steps: the model {} the link",
        if followed {
            "followed"
        } else {
            "did not follow"
        }
    );
    plane.shutdown().await;
    browser.shutdown().await;
    assert!(
        answer.to_lowercase().contains("marigold"),
        "the answer was not found: {answer}"
    );
}

/// Run one browsing task; its answer, and whether it clicked anything.
async fn browse(service: &TaskService, goal: String, label: &str) -> (String, bool) {
    let started = Instant::now();
    let task = service
        .start_task(StartTaskRequest {
            goal,
            ..Default::default()
        })
        .await
        .unwrap();
    let events = settled(service, &task, 0).await;
    eprintln!("{label}, {:.0?}:", started.elapsed());
    narrate(&events);
    if std::env::var_os("TRAMONTANA_LIVE_SHOW").is_some() {
        for event in &events {
            if let EventPayload::MessageCommitted { message } = &event.payload {
                eprintln!(
                    "{:?}: {}",
                    message.role,
                    serde_json::to_string(&message.parts).unwrap()
                );
            }
        }
    }
    let clicked = events.iter().any(|event| {
        matches!(&event.payload,
        EventPayload::ToolRequested { tool_id, .. } if tool_id == "builtin.browser.click")
    });
    let Some(EventPayload::TaskCompleted { summary, .. }) = events.last().map(|e| &e.payload)
    else {
        panic!("the task did not complete: {:?}", events.last());
    };
    (summary.clone(), clicked)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_whose_model_stopped_is_handed_to_a_real_one() {
    let Some((server, model)) = live() else {
        return;
    };
    let mut plane = assemble_models(
        &[unreachable(), mlx(&server, &model)],
        &(Arc::new(InMemorySecretVault::new()) as Arc<dyn SecretResolver>),
        Supervision::Start,
    )
    .await;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("README.md"),
        "# Fixture\n\nThe release codename is Bora.\n",
    )
    .unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    let pins = pinnable_models(&plane.providers).await;
    let gone = pins
        .iter()
        .find(|pin| pin.model.provider_id == "gone")
        .unwrap()
        .clone();
    let admission = ToolAdmission::READ_ONLY;
    // The task starts on the worker pinned to the model that stopped.
    let service = service(
        &plane,
        vec![
            Arc::new(FilesystemTools::new()) as Arc<dyn ToolProvider>,
            Arc::new(GitTools::new()),
            Arc::new(RecipeTools::new()),
        ],
        Arc::new(
            ReferenceWorker::new(offered_tools(admission, &[]), ReadOnlyLimits::default())
                .admitting(admission)
                .pinned(gone),
        ),
    )
    .await;
    let project = service
        .open_project("Fixture", vec![root.to_string_lossy().into_owned()])
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Codename")
        .await
        .unwrap();
    let task = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Read README.md and tell me the release codename.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = settled(&service, &task, 0).await;
    eprintln!("on the model that stopped:");
    narrate(&events);
    assert_eq!(
        service.get_task(&task).await.unwrap().state,
        TaskState::Interrupted,
        "the task was not kept for another worker"
    );

    let checkpoint = service.latest_checkpoint(&task).await.unwrap().unwrap();
    let started = Instant::now();
    service
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@mlx/qwen",
        )
        .await
        .unwrap();
    let all = settled(&service, &task, events.len()).await;
    eprintln!("handed to Qwen, {:.0?}:", started.elapsed());
    narrate(&all[events.len()..]);
    plane.shutdown().await;

    let Some(EventPayload::TaskCompleted { summary, .. }) = all.last().map(|e| &e.payload) else {
        panic!("the handed-over task did not complete: {:?}", all.last());
    };
    assert!(
        summary.contains("Bora"),
        "the answer was not found: {summary}"
    );
}

fn tramontana_adapters_browser_path() -> Option<std::path::PathBuf> {
    tramontana_composition::find_browser()
}

fn tramontana_adapters_browser_options(
    executable: std::path::PathBuf,
    profiles: &std::path::Path,
) -> tramontana_composition::BrowserLaunchOptions {
    tramontana_composition::BrowserLaunchOptions {
        executable: Some(executable),
        profiles: profiles.join("browser"),
        headless: std::env::var_os("TRAMONTANA_BROWSER_VISIBLE").is_none(),
    }
}
