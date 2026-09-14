//! Embedded strategy through the real host, policy, journal and M4 recovery.
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{
    sync::{Notify, mpsc},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Answer,
    Loop,
    Invalid,
    Unknown,
    Many,
    Huge,
    Silent,
    Failure,
    Length,
    ExcessUsage,
}
struct Model {
    scenario: Scenario,
    cloud: bool,
    capable: bool,
    requests: Mutex<Vec<AuthorizedModelRequest>>,
    pause_after_read: AtomicBool,
    entered: Notify,
}
impl Model {
    fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            cloud: false,
            capable: true,
            requests: Mutex::new(vec![]),
            pause_after_read: AtomicBool::new(false),
            entered: Notify::new(),
        }
    }
}
#[async_trait]
impl ModelProvider for Model {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }
    async fn list_models(&self) -> Vec<ModelDescriptor> {
        let mut d = FakeModelProvider::descriptor();
        if self.capable {
            d.capabilities.push(ModelCapability::ToolCalls);
        }
        if self.cloud {
            d.location = ModelLocation::Cloud;
        }
        vec![d]
    }
    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        let has_result = request
            .request()
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool);
        self.requests.lock().unwrap().push(request.clone());
        self.entered.notify_one();
        if matches!(self.scenario, Scenario::Silent)
            || (has_result && self.pause_after_read.load(Ordering::SeqCst))
        {
            cancel.cancelled().await;
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "cancelled",
                CorrelationId::new("test"),
            ));
        }
        if matches!(self.scenario, Scenario::Failure) {
            return Err(ErrorEnvelope::new(
                ErrorCode::ProviderUnavailable,
                "API_KEY=fixture-secret",
                CorrelationId::new("test"),
            ));
        }
        let (sender, receiver) = mpsc::channel(8);
        let stream_id = request.request().request_id.clone();
        if matches!(self.scenario, Scenario::ExcessUsage) {
            sender
                .send(ModelEvent::Usage {
                    usage: Usage {
                        output_tokens: Some(1025),
                        input_tokens: None,
                        estimated: false,
                    },
                })
                .await
                .unwrap();
        }
        if has_result && matches!(self.scenario, Scenario::Answer) {
            sender
                .send(ModelEvent::TextDelta {
                    stream_id,
                    offset: 0,
                    text: "The fixture contains the requested repository fact.".into(),
                })
                .await
                .unwrap();
            sender
                .send(ModelEvent::Completed {
                    stop_reason: StopReason::Stop,
                })
                .await
                .unwrap();
        } else if matches!(self.scenario, Scenario::Huge | Scenario::Length) {
            sender
                .send(ModelEvent::TextDelta {
                    stream_id,
                    offset: 0,
                    text: if matches!(self.scenario, Scenario::Huge) {
                        "x".repeat(65_537)
                    } else {
                        "partial".into()
                    },
                })
                .await
                .unwrap();
            sender
                .send(ModelEvent::Completed {
                    stop_reason: StopReason::Length,
                })
                .await
                .unwrap();
        } else {
            let count = if matches!(self.scenario, Scenario::Many) {
                2
            } else {
                1
            };
            for i in 0..count {
                sender
                    .send(ModelEvent::ToolCallDelta {
                        stream_id: stream_id.clone(),
                        offset: i,
                        call_index: i,
                        call_id: Some(format!("provider-call-{i}")),
                        name: Some(
                            if matches!(self.scenario, Scenario::Unknown) {
                                "builtin.fs.write"
                            } else {
                                "fake.tool.echo"
                            }
                            .into(),
                        ),
                        delta: if matches!(self.scenario, Scenario::Invalid) {
                            "{\"value\":42}"
                        } else {
                            "{\"value\":\"repository fact\"}"
                        }
                        .into(),
                    })
                    .await
                    .unwrap();
            }
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
struct Tool {
    calls: AtomicUsize,
    hold: AtomicBool,
    secret: bool,
    entered: Notify,
}
impl Tool {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
            secret: false,
            entered: Notify::new(),
        }
    }
}
#[async_trait]
impl ToolProvider for Tool {
    fn id(&self) -> ProviderId {
        FakeToolProvider::new().id()
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        FakeToolProvider::new().list_tools()
    }
    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.hold.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let mut outcome = FakeToolProvider::new().invoke(request, cancel).await?;
        if self.secret {
            outcome.content = serde_json::json!({"text":"API_KEY=fixture-secret"});
        }
        Ok(outcome)
    }
}
struct Harness {
    repositories: Repositories,
    providers: Providers,
    ids: Arc<SequentialIdSource>,
    clock: Arc<FakeClock>,
}
impl Harness {
    fn new(
        model: Arc<Model>,
        tool: Arc<Tool>,
        limits: ReadOnlyLimits,
        approver: Arc<dyn Approver>,
    ) -> Self {
        let events = Arc::new(InMemoryEventStore::new());
        let ids = Arc::new(SequentialIdSource::new());
        let (tools, permissions, releases) = assemble_tool_plane(
            vec![tool],
            Arc::new(InMemoryApprovalStore::new()),
            approver,
            ids.clone(),
        )
        .unwrap();
        Self {
            repositories: Repositories {
                projects: Arc::new(InMemoryProjectRepository::new()),
                conversations: Arc::new(InMemoryConversationRepository::new()),
                tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
                events,
                configuration: Arc::new(InMemoryConfigurationRepository::new()),
                checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
            },
            providers: Providers {
                models: vec![model],
                tools,
                permissions,
                releases,
                worker: Arc::new(ReferenceWorker::new(vec!["fake.tool.echo".into()], limits)),
            },
            ids,
            clock: Arc::new(FakeClock::new()),
        }
    }
    fn service(&self, mode: RoutingMode) -> TaskService {
        TaskService::new(
            self.repositories.clone(),
            self.providers.clone(),
            self.clock.clone(),
            self.ids.clone(),
        )
        .with_routing_mode(mode)
    }
}
async fn start(service: &TaskService, project: bool) -> TaskId {
    let conversation_id = if project {
        let p = service
            .create_project("Fixture", vec!["/fixture".into()])
            .await
            .unwrap();
        Some(
            service
                .create_conversation(Some(p.project_id), "Repository question")
                .await
                .unwrap()
                .conversation_id,
        )
    } else {
        None
    };
    service
        .start_task(StartTaskRequest {
            conversation_id,
            goal: "What fact does this repository contain?".into(),
        })
        .await
        .unwrap()
}
async fn finished(service: &TaskService, task: &TaskId) -> Vec<EventEnvelope> {
    timeout(Duration::from_secs(35), async {
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
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap()
}
fn fail(events: &[EventEnvelope], code: ErrorCode) {
    assert!(
        events.iter().any(
            |event| matches!(&event.payload,EventPayload::TaskFailed {error} if error.code==code)
        ),
        "expected {code:?}: {events:?}"
    );
}
fn harness(model: Arc<Model>, tool: Arc<Tool>, limits: ReadOnlyLimits) -> Harness {
    Harness::new(
        model,
        tool,
        limits,
        Arc::new(ScriptedApprover::unavailable()),
    )
}

#[tokio::test]
async fn answers_through_brokered_tools_and_released_canonical_history() {
    let model = Arc::new(Model::new(Scenario::Answer));
    let tool = Arc::new(Tool::new());
    let h = harness(model.clone(), tool.clone(), ReadOnlyLimits::default());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].request().messages.iter().any(|message| matches!(&message.parts[0],MessagePart::ToolResult {content,..} if content["output"]["echoed"]=="repository fact")));
        assert_ne!(
            requests[0].plan().context_manifest_id,
            requests[1].plan().context_manifest_id
        );
    }
    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    assert_eq!(checkpoint.schema_version, 1);
    assert_eq!(checkpoint.completed_step_ids, vec!["reference-1"]);
    assert!(checkpoint.pending_action.is_none());
    let mut request = ExportRequest::at(h.clock.now());
    request.tasks = vec![service.get_task_record(&task).await.unwrap()];
    request.events = events;
    request.checkpoints = vec![checkpoint];
    let bundle = build_export(request);
    let restored = validate_import(&bundle, ImportLimits::default()).unwrap();
    assert_eq!(restored.tasks.len(), 1);
    for event in restored.events.values().flatten() {
        if let EventPayload::MessageCommitted { message } = &event.payload {
            for part in &message.parts {
                match part {
                    MessagePart::ToolCall { arguments, .. } => {
                        assert_eq!(arguments["omitted"], REDACTED_OUTPUT)
                    }
                    MessagePart::ToolResult { content, .. } => assert_eq!(content, REDACTED_OUTPUT),
                    _ => {}
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn malformed_unoffered_and_failed_model_turns_cannot_run_tools() {
    for (scenario, code) in [
        (Scenario::Invalid, ErrorCode::InvalidRequest),
        (Scenario::Unknown, ErrorCode::PermissionDenied),
        (Scenario::Many, ErrorCode::LimitExceeded),
        (Scenario::Huge, ErrorCode::LimitExceeded),
        (Scenario::Length, ErrorCode::LimitExceeded),
        (Scenario::Failure, ErrorCode::ProviderUnavailable),
        (Scenario::Silent, ErrorCode::Timeout),
        (Scenario::ExcessUsage, ErrorCode::LimitExceeded),
    ] {
        let model = Arc::new(Model::new(scenario));
        let tool = Arc::new(Tool::new());
        let h = harness(model, tool.clone(), ReadOnlyLimits::default());
        let service = h.service(RoutingMode::LocalOnly);
        let task = start(&service, true).await;
        let events = finished(&service, &task).await;
        fail(&events, code);
        assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("fixture-secret")
        );
    }
}

#[tokio::test]
async fn each_cumulative_budget_stops_before_an_extra_dispatch() {
    for dimension in ["steps", "tools", "time", "input", "output", "tokens"] {
        let model = Arc::new(Model::new(Scenario::Loop));
        let tool = Arc::new(Tool::new());
        let mut limits = ReadOnlyLimits::default();
        match dimension {
            "steps" => limits.steps = 1,
            "tools" => limits.tools = 1,
            "time" => limits.time_ms = limits.turn_ms,
            "input" => limits.input_bytes = 1,
            "output" => limits.output_bytes = 327_680,
            "tokens" => limits.output_tokens = 1024,
            _ => unreachable!(),
        }
        let h = harness(model.clone(), tool.clone(), limits);
        let service = h.service(RoutingMode::LocalOnly);
        let task = start(&service, true).await;
        let events = finished(&service, &task).await;
        fail(&events, ErrorCode::LimitExceeded);
        assert!(model.requests.lock().unwrap().len() <= 1);
        assert!(tool.calls.load(Ordering::SeqCst) <= 1);
    }
}

#[tokio::test]
async fn unsupported_models_local_only_and_projectless_tools_fail_closed() {
    for case in ["unsupported", "cloud", "projectless"] {
        let mut m = Model::new(Scenario::Answer);
        m.capable = case != "unsupported";
        m.cloud = case == "cloud";
        let model = Arc::new(m);
        let tool = Arc::new(Tool::new());
        let h = harness(model.clone(), tool.clone(), ReadOnlyLimits::default());
        let service = h.service(RoutingMode::LocalOnly);
        let task = start(&service, case != "projectless").await;
        let events = finished(&service, &task).await;
        assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskFailed);
        assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
        if case != "projectless" {
            assert!(model.requests.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn checkpoint_reopen_retries_only_the_pending_read_with_fresh_permission() {
    let model = Arc::new(Model::new(Scenario::Answer));
    let tool = Arc::new(Tool::new());
    tool.hold.store(true, Ordering::SeqCst);
    let h = harness(model.clone(), tool.clone(), ReadOnlyLimits::default());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    timeout(Duration::from_secs(2), tool.entered.notified())
        .await
        .unwrap();
    let cp = service.create_checkpoint(&task).await.unwrap();
    assert!(cp.resume.eligible);
    assert_eq!(
        cp.pending_action.as_ref().unwrap().replay_safety,
        ReplaySafety::Safe
    );
    drop(service);
    tool.hold.store(false, Ordering::SeqCst);
    let reopened = h.service(RoutingMode::LocalOnly);
    reopened.recover().await.unwrap();
    reopened
        .resume_task(&task, &cp.checkpoint_id)
        .await
        .unwrap();
    let events = finished(&reopened, &task).await;
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == TaskEventKind::ToolRequested)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.payload, EventPayload::PermissionDecided { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn completed_observations_survive_reopen_without_replaying_tools_or_resetting_budgets() {
    let model = Arc::new(Model::new(Scenario::Answer));
    model.pause_after_read.store(true, Ordering::SeqCst);
    let tool = Arc::new(Tool::new());
    let limits = ReadOnlyLimits {
        steps: 2,
        ..ReadOnlyLimits::default()
    };
    let h = harness(model.clone(), tool.clone(), limits);
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    timeout(Duration::from_secs(2), async {
        while model.requests.lock().unwrap().len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cp = service.create_checkpoint(&task).await.unwrap();
    assert_eq!(cp.completed_step_ids, vec!["reference-1"]);
    assert!(cp.resume.eligible);
    drop(service);
    model.pause_after_read.store(false, Ordering::SeqCst);
    let reopened = h.service(RoutingMode::LocalOnly);
    reopened.recover().await.unwrap();
    reopened
        .resume_task(&task, &cp.checkpoint_id)
        .await
        .unwrap();
    let events = finished(&reopened, &task).await;
    fail(&events, ErrorCode::LimitExceeded);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn silent_tools_obey_timeouts_and_explicit_cancellation() {
    for explicit in [false, true] {
        let model = Arc::new(Model::new(Scenario::Answer));
        let tool = Arc::new(Tool::new());
        tool.hold.store(true, Ordering::SeqCst);
        let h = harness(model, tool.clone(), ReadOnlyLimits::default());
        let service = h.service(RoutingMode::LocalOnly);
        let task = start(&service, true).await;
        tool.entered.notified().await;
        if explicit {
            service.cancel_task(&task).await.unwrap();
        }
        let events = finished(&service, &task).await;
        if explicit {
            assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCancelled);
        } else {
            fail(&events, ErrorCode::Timeout);
        }
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn tool_credentials_are_redacted_before_durability_and_next_model_release() {
    let model = Arc::new(Model::new(Scenario::Answer));
    let mut tool = Tool::new();
    tool.secret = true;
    let h = harness(model.clone(), Arc::new(tool), ReadOnlyLimits::default());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("fixture-secret")
    );
    for request in model.requests.lock().unwrap().iter() {
        assert!(
            !serde_json::to_string(request.request())
                .unwrap()
                .contains("fixture-secret")
        );
    }
}

#[tokio::test]
async fn repeated_model_interruptions_resume_without_phantom_pending_actions() {
    let model = Arc::new(Model::new(Scenario::Answer));
    model.pause_after_read.store(true, Ordering::SeqCst);
    let tool = Arc::new(Tool::new());
    let h = harness(model.clone(), tool.clone(), ReadOnlyLimits::default());
    let mut service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    for expected_requests in [2, 3] {
        timeout(Duration::from_secs(2), async {
            while model.requests.lock().unwrap().len() < expected_requests {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let cp = service.create_checkpoint(&task).await.unwrap();
        assert!(cp.resume.eligible);
        assert_eq!(cp.completed_step_ids, vec!["reference-1"]);
        assert_eq!(
            cp.pending_action.as_ref().unwrap().kind,
            PendingActionKind::Model
        );
        drop(service);
        service = h.service(RoutingMode::LocalOnly);
        service.recover().await.unwrap();
        if expected_requests == 3 {
            model.pause_after_read.store(false, Ordering::SeqCst);
        }
        service.resume_task(&task, &cp.checkpoint_id).await.unwrap();
    }
    let events = finished(&service, &task).await;
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.requests.lock().unwrap().len(), 4);
}

struct DenySecondRelease(AtomicUsize);
#[async_trait]
impl Approver for DenySecondRelease {
    async fn ask(
        &self,
        _request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        Ok(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            ApprovalDecisionKind::AllowOnce
        } else {
            ApprovalDecisionKind::Deny
        })
    }
}

#[tokio::test]
async fn a_cloud_observation_needs_fresh_approval_and_denial_stops_dispatch() {
    let mut configured = Model::new(Scenario::Answer);
    configured.cloud = true;
    let model = Arc::new(configured);
    let tool = Arc::new(Tool::new());
    let approver = Arc::new(DenySecondRelease(AtomicUsize::new(0)));
    let h = Harness::new(
        model.clone(),
        tool.clone(),
        ReadOnlyLimits::default(),
        approver.clone(),
    );
    let service = h.service(RoutingMode::CloudPreferred);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;
    fail(&events, ErrorCode::PermissionDenied);
    assert_eq!(approver.0.load(Ordering::SeqCst), 2);
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
}
