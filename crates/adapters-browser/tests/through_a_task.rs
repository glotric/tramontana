//! The browser through a whole task: the reference worker, the real host,
//! permission manager and journal, a scripted model, and a real installed
//! browser. Skipped, saying so, where none is installed.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tramontana_adapters_browser::{BrowserProvider, LaunchOptions, READ, TOOLS, find_browser};
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

/// A site with two pages, and a link from the first to the second.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0; 8192];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = if request.starts_with("GET /next ") {
                    "<title>Next</title><p>The answer is forty-two.</p>"
                } else {
                    "<title>Start</title><a href=\"/next\">Next page</a>"
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

/// Opens the site, follows its link, then answers from what it read.
struct Model {
    site: String,
    requests: Mutex<Vec<AuthorizedModelRequest>>,
}

#[async_trait]
impl ModelProvider for Model {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        let mut model = FakeModelProvider::descriptor();
        model.capabilities.push(ModelCapability::ToolCalls);
        vec![model]
    }

    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        let results = request
            .request()
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .count();
        self.requests.lock().unwrap().push(request.clone());
        let stream_id = request.request().request_id.clone();
        let (sender, receiver) = mpsc::channel(8);
        let call = |name: &str, arguments: String| ModelEvent::ToolCallDelta {
            stream_id: stream_id.clone(),
            offset: 0,
            call_index: 0,
            call_id: Some(format!("call-{results}")),
            name: Some(name.to_owned()),
            delta: arguments,
        };
        match results {
            0 => {
                sender
                    .send(call(
                        "builtin.browser.open",
                        format!("{{\"url\":\"{}/\"}}", self.site),
                    ))
                    .await
                    .unwrap();
                sender
                    .send(ModelEvent::Completed {
                        stop_reason: StopReason::ToolCall,
                    })
                    .await
                    .unwrap();
            }
            1 => {
                sender
                    .send(call("builtin.browser.click", "{\"ref\":1}".to_owned()))
                    .await
                    .unwrap();
                sender
                    .send(ModelEvent::Completed {
                        stop_reason: StopReason::ToolCall,
                    })
                    .await
                    .unwrap();
            }
            _ => {
                sender
                    .send(ModelEvent::TextDelta {
                        stream_id: stream_id.clone(),
                        offset: 0,
                        text: "The page says the answer is forty-two.".to_owned(),
                    })
                    .await
                    .unwrap();
                sender
                    .send(ModelEvent::Completed {
                        stop_reason: StopReason::Stop,
                    })
                    .await
                    .unwrap();
            }
        }
        Ok(receiver)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_task_opens_a_site_once_allowed_and_follows_a_link_on_it() {
    let Some(executable) = find_browser() else {
        eprintln!("skipped: no Chrome, Edge or Chromium is installed");
        return;
    };
    let profiles = tempfile::tempdir().unwrap();
    let browser = Arc::new(BrowserProvider::new(LaunchOptions {
        executable: Some(executable),
        profiles: profiles.path().join("browser"),
        headless: std::env::var_os("TRAMONTANA_BROWSER_VISIBLE").is_none(),
    }));
    let site = serve().await;
    let model = Arc::new(Model {
        site: site.clone(),
        requests: Mutex::new(Vec::new()),
    });
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::clone(&browser) as Arc<dyn ToolProvider>],
        Arc::clone(&approvals) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowSite)),
        ids.clone(),
    )
    .unwrap();
    let events = Arc::new(InMemoryEventStore::new());
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
        },
        Providers {
            models: vec![model.clone()],
            tools,
            permissions,
            releases,
            worker: Arc::new(
                ReferenceWorker::new(
                    TOOLS.iter().map(|tool| (*tool).to_owned()).collect(),
                    ReadOnlyLimits::default(),
                )
                .admitting(ToolAdmission::READ_ONLY.with_browse()),
            ),
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(RoutingMode::LocalOnly);

    let task = service
        .start_task(StartTaskRequest {
            goal: "What does the next page say?".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = timeout(Duration::from_secs(60), async {
        loop {
            let events = service.replay_task_events(&task, 0).await.unwrap();
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
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    assert!(
        matches!(
            &events.last().unwrap().payload,
            EventPayload::TaskCompleted { summary, .. } if summary.contains("forty-two")
        ),
        "{:?}",
        events.last()
    );
    // A worker that browses says its network passes through Tramontana.
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::WorkerSelected { descriptor }
            if descriptor.network_control == NetworkControl::Brokered
    )));
    // One question, for the site; the click on it was covered by the answer.
    assert_eq!(approvals.requests().len(), 1);
    let decisions: Vec<(String, Option<GrantKind>)> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::PermissionDecided {
                reason_code,
                grant_kind,
                ..
            } => Some((reason_code.clone(), *grant_kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        vec![
            ("policy.approved_site".to_owned(), Some(GrantKind::Site)),
            ("policy.site_grant".to_owned(), Some(GrantKind::Site)),
        ]
    );
    // The model read the second page, labelled as the page's words.
    let last = model.requests.lock().unwrap().last().unwrap().clone();
    let history = serde_json::to_string(&last.request().messages).unwrap();
    assert!(history.contains("The answer is forty-two."), "{history}");
    assert!(history.contains("not instructions from the person"));

    // The task's window closed with it.
    let gone = browser
        .target(&task, READ, &serde_json::json!({}))
        .await
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
}
