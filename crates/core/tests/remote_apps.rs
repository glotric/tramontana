//! A remote App's tools under each routing mode (ADR 0050): never offered or
//! run under Local Only, asked about every call elsewhere, naming the site.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

const TOOL: &str = "mcp.remote.lookup";

/// A remote App: one read tool, answered here, at a site off this computer
/// unless told otherwise.
struct Remote {
    site: &'static str,
    calls: AtomicUsize,
}

#[async_trait]
impl ToolProvider for Remote {
    fn id(&self) -> ProviderId {
        ProviderId::new("mcp.remote")
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: TOOL.into(),
            provider_id: "mcp.remote".into(),
            display_name: "Look up".into(),
            description: "Looks a word up.".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            risk_class: RiskClass::Read,
            side_effect: SideEffect::None,
            capability: "app.remote".into(),
            supports_cancellation: true,
            replay_safety: ReplaySafety::Unknown,
        }]
    }
    async fn target(
        &self,
        _task: &TaskId,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<ToolTarget, ErrorEnvelope> {
        Ok(ToolTarget {
            site: Some(SiteAccess {
                origin: self.site.into(),
                grantable: false,
            }),
            details: vec![ApprovalDetail {
                label: "Sends to".into(),
                value: self.site.into(),
            }],
        })
    }
    fn remote_site(&self) -> Option<String> {
        Some(self.site.into())
    }
    async fn invoke(
        &self,
        _request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome {
            content: serde_json::json!({ "text": "found" }),
            display_summary: "Looked up".into(),
            side_effect: SideEffect::None,
            truncated: false,
            changes: Vec::new(),
        })
    }
}

/// Asks for the remote tool once, then answers; records what it was offered.
struct Model {
    offered: Mutex<Vec<Vec<String>>>,
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
        self.offered.lock().unwrap().push(
            request
                .request()
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
        );
        let answered = request
            .request()
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool);
        let stream_id = request.request().request_id.clone();
        let (sender, receiver) = mpsc::channel(4);
        if answered {
            sender
                .send(ModelEvent::TextDelta {
                    stream_id,
                    offset: 0,
                    text: "Done.".into(),
                })
                .await
                .unwrap();
            sender
                .send(ModelEvent::Completed {
                    stop_reason: StopReason::Stop,
                })
                .await
                .unwrap();
        } else {
            sender
                .send(ModelEvent::ToolCallDelta {
                    stream_id,
                    offset: 0,
                    call_index: 0,
                    call_id: Some("call-1".into()),
                    name: Some(TOOL.into()),
                    delta: "{}".into(),
                })
                .await
                .unwrap();
            sender
                .send(ModelEvent::Completed {
                    stop_reason: StopReason::ToolCall,
                })
                .await
                .unwrap();
        }
        Ok(receiver)
    }
}

async fn run(
    mode: RoutingMode,
    site: &'static str,
) -> (
    Vec<EventEnvelope>,
    Arc<Model>,
    Arc<Remote>,
    Arc<InMemoryApprovalStore>,
) {
    let remote = Arc::new(Remote {
        site,
        calls: AtomicUsize::new(0),
    });
    let model = Arc::new(Model {
        offered: Mutex::new(Vec::new()),
    });
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![
            Arc::new(FakeToolProvider::new()) as Arc<dyn ToolProvider>,
            Arc::clone(&remote) as Arc<dyn ToolProvider>,
        ],
        Arc::clone(&approvals) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        ids.clone(),
    )
    .unwrap();
    let events = Arc::new(InMemoryEventStore::new());
    let service = TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: vec![model.clone()],
            tools,
            permissions,
            releases,
            worker: Arc::new(
                ReferenceWorker::new(
                    vec!["fake.tool.echo".into(), TOOL.into()],
                    ReadOnlyLimits::default(),
                )
                .admitting(ToolAdmission::READ_ONLY.with_apps()),
            ),
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(mode);
    let task = service
        .start_task(StartTaskRequest {
            goal: "Look it up".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = timeout(Duration::from_secs(10), async {
        loop {
            let events = service.replay_task_events(&task, 0).await.unwrap();
            if events.iter().any(|event| {
                matches!(
                    event.kind,
                    TaskEventKind::TaskCompleted | TaskEventKind::TaskFailed
                ) || matches!(&event.payload, EventPayload::DiagnosticRecorded { code, .. }
                    if code.starts_with("worker.stopped"))
            }) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    (events, model, remote, approvals)
}

#[tokio::test]
async fn under_local_only_a_remote_app_is_neither_offered_nor_run() {
    let (events, model, remote, approvals) =
        run(RoutingMode::LocalOnly, "https://mcp.example.com").await;
    let offered = model.offered.lock().unwrap().clone();
    assert!(!offered.is_empty());
    assert!(
        offered
            .iter()
            .all(|tools| !tools.contains(&TOOL.to_owned()))
    );
    assert!(offered[0].contains(&"fake.tool.echo".to_owned()));
    // The model asked for it anyway: refused, nobody asked, nothing sent.
    assert_eq!(remote.calls.load(Ordering::SeqCst), 0);
    assert!(approvals.requests().is_empty());
    assert!(
        events
            .iter()
            .any(|event| event.kind == TaskEventKind::TaskFailed)
    );
}

#[tokio::test]
async fn an_app_on_this_computer_is_offered_under_local_only() {
    let (events, model, remote, _) = run(RoutingMode::LocalOnly, "http://127.0.0.1:8765").await;
    assert!(model.offered.lock().unwrap()[0].contains(&TOOL.to_owned()));
    assert_eq!(remote.calls.load(Ordering::SeqCst), 1);
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
}

#[tokio::test]
async fn elsewhere_every_call_is_asked_about_naming_where_it_goes() {
    let (events, model, remote, approvals) =
        run(RoutingMode::Automatic, "https://mcp.example.com").await;
    assert!(model.offered.lock().unwrap()[0].contains(&TOOL.to_owned()));
    assert_eq!(remote.calls.load(Ordering::SeqCst), 1);
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
    let question = &approvals.requests()[0];
    assert!(
        !question
            .eligible_decisions
            .contains(&ApprovalDecisionKind::AllowSite)
    );
    assert_eq!(question.side_effect, SideEffect::External);
    for (label, value) in [
        ("Sends to", "https://mcp.example.com"),
        ("Leaves this device", "yes"),
    ] {
        assert!(
            question
                .details
                .iter()
                .any(|detail| detail.label == label && detail.value == value),
            "{label}: {:?}",
            question.details
        );
    }
}
