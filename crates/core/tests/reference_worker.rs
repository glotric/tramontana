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
use tramontana_core::config::{ConfigurationSelection, RawSource};
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
    Write,
    /// Names the project's declared `test` recipe.
    Recipe,
    /// Names a recipe the project does not declare.
    UndeclaredRecipe,
}
struct Model {
    scenario: Scenario,
    cloud: bool,
    capable: bool,
    requests: Mutex<Vec<AuthorizedModelRequest>>,
    pause_after_read: AtomicBool,
    /// Fail the turn that follows a tool result.
    fail_after_read: AtomicBool,
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
            fail_after_read: AtomicBool::new(false),
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
        if matches!(self.scenario, Scenario::Failure)
            || (has_result && self.fail_after_read.load(Ordering::SeqCst))
        {
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
        if has_result
            && matches!(
                self.scenario,
                Scenario::Answer
                    | Scenario::Write
                    | Scenario::Recipe
                    | Scenario::Many
                    | Scenario::UndeclaredRecipe
            )
        {
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
                            match self.scenario {
                                Scenario::Unknown => "builtin.fs.write",
                                Scenario::Write => FAKE_WRITE_TOOL_ID,
                                Scenario::Recipe | Scenario::UndeclaredRecipe => RECIPE_TOOL_ID,
                                _ => "fake.tool.echo",
                            }
                            .into(),
                        ),
                        delta: match self.scenario {
                            Scenario::Invalid => "{\"value\":42}",
                            Scenario::Write => "{\"key\":\"greeting\",\"value\":\"hello\"}",
                            Scenario::Recipe => "{\"recipe\":\"test\"}",
                            Scenario::UndeclaredRecipe => "{\"recipe\":\"deploy\"}",
                            _ => "{\"value\":\"repository fact\"}",
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
        Self::with(
            model,
            vec![tool as Arc<dyn ToolProvider>],
            Arc::new(ReferenceWorker::new(vec!["fake.tool.echo".into()], limits)),
            approver,
        )
    }
    fn with(
        model: Arc<Model>,
        tools: Vec<Arc<dyn ToolProvider>>,
        worker: Arc<dyn AgentProvider>,
        approver: Arc<dyn Approver>,
    ) -> Self {
        let events = Arc::new(InMemoryEventStore::new());
        let ids = Arc::new(SequentialIdSource::new());
        let (tools, permissions, releases) = assemble_tool_plane(
            tools,
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
                worker,
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
/// Like [`finished`], for a paused clock that must pass a long timer: it steps
/// in tenths of a second rather than milliseconds.
async fn finished_within(
    service: &TaskService,
    task: &TaskId,
    limit: Duration,
) -> Vec<EventEnvelope> {
    timeout(limit, async {
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
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
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
async fn a_turn_asking_for_several_calls_runs_the_first_and_the_model_asks_again() {
    // A real model batches the steps of a goal into one turn (ADR 0029).
    let model = Arc::new(Model::new(Scenario::Many));
    let tool = Arc::new(Tool::new());
    let h = harness(model.clone(), tool.clone(), ReadOnlyLimits::default());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;

    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1, "only the first ran");
    assert!(
        events.iter().any(|event| matches!(&event.payload,
            EventPayload::DiagnosticRecorded { code, summary }
                if code == "reference.callsNotRun" && summary.contains("2 tool calls"))),
        "{events:?}"
    );
    // The model's next turn shows one call and its result, not two calls.
    let requests = model.requests.lock().unwrap();
    let calls = requests[1]
        .request()
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| matches!(part, MessagePart::ToolCall { .. }))
        .count();
    assert_eq!(calls, 1);
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
        let entered = tokio::time::Instant::now();
        if explicit {
            service.cancel_task(&task).await.unwrap();
        }
        let events = finished_within(&service, &task, TOOL_RUN_LIMIT * 2).await;
        if explicit {
            assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCancelled);
        } else {
            // Stopped by the broker's backstop, not the turn's deadline,
            // which bounds only the model call (ADR 0027). The model is told,
            // as the call's result, and answers (ADR 0029).
            assert!(entered.elapsed() >= TOOL_RUN_LIMIT);
            assert!(
                events.iter().any(|event| matches!(&event.payload,
                    EventPayload::ToolFailed { error, .. } if error.code == ErrorCode::Timeout)),
                "{events:?}"
            );
            assert_eq!(
                events.last().unwrap().kind,
                TaskEventKind::TaskCompleted,
                "{events:?}"
            );
        }
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    }
}

/// A person who takes their time.
struct SlowApprover(Duration);
#[async_trait]
impl Approver for SlowApprover {
    async fn ask(
        &self,
        _request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        tokio::time::sleep(self.0).await;
        Ok(ApprovalDecisionKind::AllowOnce)
    }
}

#[tokio::test(start_paused = true)]
async fn a_slow_approval_and_a_long_tool_run_are_not_cut_off_by_the_turn() {
    // Each takes twice the turn's deadline: a person reading the request, then
    // a run as long as a real test suite's.
    let turn = Duration::from_millis(ReadOnlyLimits::default().turn_ms);
    let mut write = HeldWrite::declaring(ReplaySafety::Safe);
    write.run_for = Some(turn * 2);
    let write = Arc::new(write);
    let h = writing(
        Arc::new(Model::new(Scenario::Write)),
        write.clone(),
        ToolAdmission::READ_WRITE,
        Arc::new(SlowApprover(turn * 2)),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let started = tokio::time::Instant::now();
    let task = start(&service, true).await;
    let events = finished_within(&service, &task, turn * 10).await;

    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    assert_eq!(write.inner.value("greeting").as_deref(), Some("hello"));
    assert!(started.elapsed() >= turn * 4);
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

/// The idempotent fake write, held mid-flight on request so a test can
/// interrupt it after the broker recorded that it started.
struct HeldWrite {
    inner: FakeWriteTool,
    hold: AtomicBool,
    calls: AtomicUsize,
    entered: Notify,
    /// Report the change under this path instead, as a tool whose file name
    /// carries a credential would.
    reported_path: Option<&'static str>,
    /// Take this long to run, as a real test suite would.
    run_for: Option<Duration>,
}
impl HeldWrite {
    fn declaring(safety: ReplaySafety) -> Self {
        Self {
            inner: FakeWriteTool::declaring(safety),
            hold: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            reported_path: None,
            run_for: None,
        }
    }
}
#[async_trait]
impl ToolProvider for HeldWrite {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.inner.list_tools()
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
        if let Some(duration) = self.run_for {
            tokio::time::sleep(duration).await;
        }
        let mut outcome = self.inner.invoke(request, cancel).await?;
        if let Some(path) = self.reported_path {
            for change in &mut outcome.changes {
                change.path = path.to_owned();
                change.from = Some(path.to_owned());
            }
        }
        Ok(outcome)
    }
}
fn reference(admission: ToolAdmission) -> Arc<dyn AgentProvider> {
    let mut tools = vec!["fake.tool.echo".to_owned()];
    if admission.writes {
        tools.push(FAKE_WRITE_TOOL_ID.to_owned());
    }
    Arc::new(ReferenceWorker::new(tools, ReadOnlyLimits::default()).admitting(admission))
}
fn writing(
    model: Arc<Model>,
    write: Arc<HeldWrite>,
    admission: ToolAdmission,
    approver: Arc<dyn Approver>,
) -> Harness {
    Harness::with(
        model,
        vec![Arc::new(Tool::new()) as Arc<dyn ToolProvider>, write],
        reference(admission),
        approver,
    )
}
fn offered(model: &Model) -> Vec<String> {
    model.requests.lock().unwrap()[0]
        .request()
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

/// The built-in recipe tool as the broker sees it, recording exactly what it
/// was handed instead of running anything.
struct FakeRecipe {
    received: Mutex<Vec<serde_json::Value>>,
}
impl FakeRecipe {
    fn new() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
        }
    }
    fn received(&self) -> Vec<serde_json::Value> {
        self.received.lock().unwrap().clone()
    }
}
#[async_trait]
impl ToolProvider for FakeRecipe {
    fn id(&self) -> ProviderId {
        ProviderId::new("builtin")
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: RECIPE_TOOL_ID.to_owned(),
            provider_id: "builtin".to_owned(),
            display_name: "Run recipe".to_owned(),
            description: "Run a declared recipe by id.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "recipe": { "type": "string" } },
                "required": ["recipe"],
                "additionalProperties": false,
            }),
            risk_class: RiskClass::Dangerous,
            side_effect: SideEffect::External,
            capability: "command.recipe".to_owned(),
            supports_cancellation: true,
            replay_safety: ReplaySafety::Unsafe,
        }]
    }
    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        self.received
            .lock()
            .unwrap()
            .push(request.arguments().clone());
        Ok(ToolOutcome {
            content: serde_json::json!({ "recipe": "test", "exitCode": 0, "succeeded": true }),
            display_summary: "Recipe `test` passed".to_owned(),
            side_effect: SideEffect::External,
            truncated: false,
            changes: Vec::new(),
        })
    }
}
fn with_recipes(
    model: Arc<Model>,
    recipe: Arc<FakeRecipe>,
    admission: ToolAdmission,
    approver: Arc<dyn Approver>,
) -> Harness {
    let mut tools = vec!["fake.tool.echo".to_owned()];
    if admission.recipes {
        tools.push(RECIPE_TOOL_ID.to_owned());
    }
    Harness::with(
        model,
        vec![Arc::new(Tool::new()) as Arc<dyn ToolProvider>, recipe],
        Arc::new(ReferenceWorker::new(tools, ReadOnlyLimits::default()).admitting(admission)),
        approver,
    )
}
/// A task in a project whose `project.md` declares one recipe.
async fn start_with_recipes(service: &TaskService) -> TaskId {
    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    service
        .resolve_configuration(
            &project.project_id,
            vec![RawSource {
                relative_path: ".tramontana/project.md".to_owned(),
                kind: ConfigurationKind::ProjectOverview,
                content: b"---\nid: fixture\nrecipes:\n  test: [cargo, test, --workspace]\n---\n# Fixture\n"
                    .to_vec(),
            }],
            &ConfigurationSelection::default(),
        )
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Run the tests")
        .await
        .unwrap();
    service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Run the tests.".into(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_approved_recipe_runs_exactly_the_declared_command() {
    let model = Arc::new(Model::new(Scenario::Recipe));
    let recipe = Arc::new(FakeRecipe::new());
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let h = with_recipes(
        model.clone(),
        recipe.clone(),
        ToolAdmission::READ_ONLY.with_recipes(),
        approver.clone(),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start_with_recipes(&service).await;
    let events = finished(&service, &task).await;

    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    assert!(offered(&model).contains(&RECIPE_TOOL_ID.to_owned()));
    // The person saw the exact command, and where it came from, first.
    let asked = approver.asked();
    assert_eq!(asked.len(), 1);
    let shown: Vec<(&str, &str)> = asked[0]
        .details
        .iter()
        .map(|detail| (detail.label.as_str(), detail.value.as_str()))
        .collect();
    assert!(
        shown.contains(&("Command", "cargo test --workspace")),
        "{shown:?}"
    );
    assert!(
        shown.contains(&("Declared in", ".tramontana/project.md")),
        "{shown:?}"
    );
    // This task routes Local Only, which does not reach commands, and the
    // person is told so before approving (EXECUTION_BOUNDARY.md).
    assert!(
        shown.iter().any(|(label, value)| *label == "Network"
            && value.contains("Local Only governs where models run, not commands")),
        "{shown:?}"
    );
    assert_eq!(asked[0].risk_class, RiskClass::Dangerous);
    // Never "always for this project": every run is its own question.
    assert!(
        !asked[0]
            .eligible_decisions
            .contains(&ApprovalDecisionKind::AllowProject)
    );
    // And the tool was handed exactly that command, not the model's words.
    let received = recipe.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["run"],
        serde_json::json!(["cargo", "test", "--workspace"])
    );
    assert_eq!(
        received[0]["inherit"],
        serde_json::json!(COMMAND_ENVIRONMENT)
    );
}

#[tokio::test]
async fn a_recipe_the_project_does_not_declare_is_refused_before_anyone_is_asked() {
    let model = Arc::new(Model::new(Scenario::UndeclaredRecipe));
    let recipe = Arc::new(FakeRecipe::new());
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let h = with_recipes(
        model.clone(),
        recipe.clone(),
        ToolAdmission::READ_ONLY.with_recipes(),
        approver.clone(),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start_with_recipes(&service).await;
    let events = finished(&service, &task).await;

    // Nobody was asked and nothing ran.
    assert!(approver.asked().is_empty());
    assert!(recipe.received().is_empty());
    // The model was told why, as the call's result, and answered instead of
    // the task ending (ADR 0029).
    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    let requests = model.requests.lock().unwrap();
    let told = requests[1]
        .request()
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find_map(|part| match part {
            MessagePart::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the model was not shown the refused call's result"));
    assert_eq!(told["status"], "failed", "{told}");
    assert_eq!(told["errorCode"], "notFound", "{told}");
    assert!(
        told["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{told}"
    );
}

#[tokio::test]
async fn recipes_are_not_offered_unless_admitted_and_never_run_unapproved() {
    // Writes do not bring recipes with them.
    let model = Arc::new(Model::new(Scenario::Recipe));
    let recipe = Arc::new(FakeRecipe::new());
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let h = with_recipes(
        model.clone(),
        recipe.clone(),
        ToolAdmission::READ_WRITE,
        approver.clone(),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start_with_recipes(&service).await;
    fail(
        &finished(&service, &task).await,
        ErrorCode::PermissionDenied,
    );
    assert!(!offered(&model).contains(&RECIPE_TOOL_ID.to_owned()));
    assert!(approver.asked().is_empty());
    assert!(recipe.received().is_empty());

    // Admitted, but nobody says yes.
    for approver in [
        ScriptedApprover::always(ApprovalDecisionKind::Deny),
        ScriptedApprover::unavailable(),
    ] {
        let model = Arc::new(Model::new(Scenario::Recipe));
        let recipe = Arc::new(FakeRecipe::new());
        let h = with_recipes(
            model,
            recipe.clone(),
            ToolAdmission::READ_ONLY.with_recipes(),
            Arc::new(approver),
        );
        let service = h.service(RoutingMode::LocalOnly);
        let task = start_with_recipes(&service).await;
        fail(
            &finished(&service, &task).await,
            ErrorCode::PermissionDenied,
        );
        assert!(recipe.received().is_empty());
    }
}

#[tokio::test]
async fn an_admitted_write_is_offered_and_runs_once_approved() {
    let model = Arc::new(Model::new(Scenario::Write));
    let write = Arc::new(HeldWrite::declaring(ReplaySafety::Safe));
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let h = writing(
        model.clone(),
        write.clone(),
        ToolAdmission::READ_WRITE,
        approver.clone(),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;
    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    assert_eq!(offered(&model), vec!["fake.tool.echo", FAKE_WRITE_TOOL_ID]);
    // Being offered the write was not permission to run it.
    assert_eq!(approver.asked().len(), 1);
    assert_eq!(write.calls.load(Ordering::SeqCst), 1);
    assert_eq!(write.inner.value("greeting").as_deref(), Some("hello"));
}

#[tokio::test]
async fn a_write_nobody_approved_never_runs() {
    for approver in [
        ScriptedApprover::always(ApprovalDecisionKind::Deny),
        ScriptedApprover::unavailable(),
    ] {
        let model = Arc::new(Model::new(Scenario::Write));
        let write = Arc::new(HeldWrite::declaring(ReplaySafety::Safe));
        let h = writing(
            model,
            write.clone(),
            ToolAdmission::READ_WRITE,
            Arc::new(approver),
        );
        let service = h.service(RoutingMode::LocalOnly);
        let task = start(&service, true).await;
        fail(
            &finished(&service, &task).await,
            ErrorCode::PermissionDenied,
        );
        assert_eq!(write.calls.load(Ordering::SeqCst), 0);
        assert_eq!(write.inner.value("greeting"), None);
    }
}

fn recorded(events: &[EventEnvelope]) -> Vec<&ArtifactRecord> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ArtifactRecorded { artifact } => Some(artifact),
            _ => None,
        })
        .collect()
}
fn approved_write(model: Arc<Model>) -> Harness {
    writing(
        model,
        Arc::new(HeldWrite::declaring(ReplaySafety::Safe)),
        ToolAdmission::READ_WRITE,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
    )
}
async fn model_asked(model: &Model, times: usize) {
    timeout(Duration::from_secs(5), async {
        while model.requests.lock().unwrap().len() < times {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_task_that_wrote_ends_with_the_files_it_changed() {
    let h = approved_write(Arc::new(Model::new(Scenario::Write)));
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;

    let artifacts = recorded(&events);
    assert_eq!(artifacts.len(), 1, "{events:?}");
    assert_eq!(artifacts[0].summary(), "1 file change(s): created greeting");
    assert_eq!(
        serde_json::to_value(&artifacts[0].content).unwrap(),
        serde_json::json!({
            "kind": "changedFiles",
            "changes": [{
                "change": "created",
                "path": "greeting",
                "contentHash": content_hash("hello"),
                "alreadyApplied": false,
            }],
        })
    );
    // The completion points at it.
    let Some(EventPayload::TaskCompleted {
        artifacts: named, ..
    }) = events.last().map(|event| &event.payload)
    else {
        panic!("expected completion: {events:?}");
    };
    assert_eq!(named, &vec![artifacts[0].artifact_id.clone()]);
}

#[tokio::test]
async fn a_task_that_changed_nothing_records_no_summary() {
    let model = Arc::new(Model::new(Scenario::Answer));
    let h = harness(model, Arc::new(Tool::new()), ReadOnlyLimits::default());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;

    assert!(recorded(&events).is_empty(), "{events:?}");
    let Some(EventPayload::TaskCompleted { artifacts, .. }) =
        events.last().map(|event| &event.payload)
    else {
        panic!("expected completion: {events:?}");
    };
    assert!(artifacts.is_empty());
}

#[tokio::test]
async fn a_task_that_stopped_half_way_still_says_what_it_changed() {
    // It failed after the write.
    let model = Model::new(Scenario::Write);
    model.fail_after_read.store(true, Ordering::SeqCst);
    let h = approved_write(Arc::new(model));
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;
    fail(&events, ErrorCode::ProviderUnavailable);
    let artifacts = recorded(&events);
    assert_eq!(artifacts.len(), 1, "{events:?}");
    assert_eq!(artifacts[0].summary(), "1 file change(s): created greeting");

    // It was cancelled after the write.
    let model = Arc::new(Model::new(Scenario::Write));
    model.pause_after_read.store(true, Ordering::SeqCst);
    let h = approved_write(model.clone());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    model_asked(&model, 2).await;
    service.cancel_task(&task).await.unwrap();
    let events = finished(&service, &task).await;
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCancelled);
    let artifacts = recorded(&events);
    assert_eq!(artifacts.len(), 1, "{events:?}");
    assert_eq!(artifacts[0].summary(), "1 file change(s): created greeting");
}

#[tokio::test]
async fn a_resumed_task_counts_what_it_changed_before_the_interruption_once() {
    let model = Arc::new(Model::new(Scenario::Write));
    model.pause_after_read.store(true, Ordering::SeqCst);
    let h = approved_write(model.clone());
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    model_asked(&model, 2).await;
    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    drop(service);

    let service = h.service(RoutingMode::LocalOnly);
    service.recover().await.unwrap();
    model.pause_after_read.store(false, Ordering::SeqCst);
    service
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    let events = finished(&service, &task).await;

    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    // The interruption recorded no summary of its own, and the write before
    // it is counted once, not dropped and not doubled.
    let artifacts = recorded(&events);
    assert_eq!(artifacts.len(), 1, "{events:?}");
    assert_eq!(artifacts[0].summary(), "1 file change(s): created greeting");
}

#[tokio::test]
async fn a_changed_path_is_redacted_before_it_is_recorded() {
    // Arguments carrying a credential are refused before any tool runs, but a
    // tool can still report a path that has one.
    let mut write = HeldWrite::declaring(ReplaySafety::Safe);
    write.reported_path = Some("API_KEY=fixture-secret");
    let h = writing(
        Arc::new(Model::new(Scenario::Write)),
        Arc::new(write),
        ToolAdmission::READ_WRITE,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    let events = finished(&service, &task).await;

    assert_eq!(recorded(&events).len(), 1, "{events:?}");
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("fixture-secret"),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_read_only_worker_is_never_offered_a_write() {
    // Registered but not admitted, and the model asks for it anyway.
    let model = Arc::new(Model::new(Scenario::Write));
    let write = Arc::new(HeldWrite::declaring(ReplaySafety::Safe));
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let h = writing(
        model.clone(),
        write.clone(),
        ToolAdmission::READ_ONLY,
        approver.clone(),
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    fail(
        &finished(&service, &task).await,
        ErrorCode::PermissionDenied,
    );
    assert_eq!(offered(&model), vec!["fake.tool.echo"]);
    assert_eq!(write.calls.load(Ordering::SeqCst), 0);
    assert!(approver.asked().is_empty());

    // Naming a write without admitting it refuses the session before any
    // model call, rather than quietly dropping the write from the catalogue.
    let model = Arc::new(Model::new(Scenario::Write));
    let write = Arc::new(HeldWrite::declaring(ReplaySafety::Safe));
    let h = Harness::with(
        model.clone(),
        vec![
            Arc::new(Tool::new()) as Arc<dyn ToolProvider>,
            write.clone(),
        ],
        Arc::new(ReferenceWorker::new(
            vec!["fake.tool.echo".into(), FAKE_WRITE_TOOL_ID.into()],
            ReadOnlyLimits::default(),
        )),
        approver,
    );
    let service = h.service(RoutingMode::LocalOnly);
    let task = start(&service, true).await;
    fail(
        &finished(&service, &task).await,
        ErrorCode::CapabilityUnsupported,
    );
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(write.calls.load(Ordering::SeqCst), 0);
}

/// A worker presenting another's descriptor. Any worker may ask for a governed
/// turn, and nothing obliges its descriptor to reveal the admission level.
struct Disguised {
    runs: Arc<dyn AgentProvider>,
    looks_like: AgentDescriptor,
}
#[async_trait]
impl AgentProvider for Disguised {
    fn describe(&self) -> AgentDescriptor {
        self.looks_like.clone()
    }
    async fn run(
        &self,
        request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        self.runs.run(request, host, cancel).await
    }
}

#[tokio::test]
async fn a_resume_cannot_change_what_the_model_may_be_offered() {
    for (started, resumed, scenario) in [
        (
            ToolAdmission::READ_WRITE,
            ToolAdmission::READ_ONLY,
            Scenario::Write,
        ),
        (
            ToolAdmission::READ_ONLY,
            ToolAdmission::READ_WRITE,
            Scenario::Answer,
        ),
    ] {
        for disguised in [false, true] {
            let model = Arc::new(Model::new(scenario));
            model.pause_after_read.store(true, Ordering::SeqCst);
            let write = Arc::new(HeldWrite::declaring(ReplaySafety::Safe));
            let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
            let mut h = writing(model.clone(), write, started, approver);
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
            assert!(cp.resume.eligible);
            drop(service);
            model.pause_after_read.store(false, Ordering::SeqCst);
            h.providers.worker = if disguised {
                Arc::new(Disguised {
                    runs: reference(resumed),
                    looks_like: reference(started).describe(),
                })
            } else {
                reference(resumed)
            };
            let reopened = h.service(RoutingMode::LocalOnly);
            reopened.recover().await.unwrap();
            // Stopping for the checkpoint already recorded one refusal.
            let before = reopened
                .replay_task_events(&task, 0)
                .await
                .unwrap()
                .last()
                .unwrap()
                .sequence;
            let outcome = reopened.resume_task(&task, &cp.checkpoint_id).await;
            if disguised {
                // Past the descriptor check, the session's own record refuses,
                // and the task waits for a person rather than failing.
                outcome.unwrap();
                let refusal = timeout(Duration::from_secs(5), async {
                    loop {
                        let events = reopened.replay_task_events(&task, before).await.unwrap();
                        if let Some(summary) =
                            events.iter().find_map(|event| match &event.payload {
                                EventPayload::DiagnosticRecorded { code, summary }
                                    if code == "resume.refused" =>
                                {
                                    Some(summary.clone())
                                }
                                _ => None,
                            })
                        {
                            return summary;
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                assert!(refusal.contains("admission"), "{refusal}");
                assert_eq!(
                    reopened.get_task_record(&task).await.unwrap().state,
                    TaskState::Interrupted
                );
            } else {
                // The reference worker describes each level differently, and a
                // checkpoint remembers the descriptor it was taken against.
                assert_eq!(outcome.unwrap_err().code, ErrorCode::Conflict);
            }
            // Either way, the model was not called again under the other level.
            assert_eq!(
                model.requests.lock().unwrap().len(),
                2,
                "{started:?} -> {resumed:?}, disguised: {disguised}"
            );
        }
    }
}

#[tokio::test]
async fn an_interrupted_write_is_retried_only_when_it_declared_that_safe() {
    for safety in [ReplaySafety::Safe, ReplaySafety::Unsafe] {
        let model = Arc::new(Model::new(Scenario::Write));
        let write = Arc::new(HeldWrite::declaring(safety));
        write.hold.store(true, Ordering::SeqCst);
        let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
        let h = writing(
            model.clone(),
            write.clone(),
            ToolAdmission::READ_WRITE,
            approver,
        );
        let service = h.service(RoutingMode::LocalOnly);
        let task = start(&service, true).await;
        timeout(Duration::from_secs(2), write.entered.notified())
            .await
            .unwrap();
        let cp = service.create_checkpoint(&task).await.unwrap();
        assert_eq!(cp.pending_action.as_ref().unwrap().replay_safety, safety);
        drop(service);
        write.hold.store(false, Ordering::SeqCst);
        let reopened = h.service(RoutingMode::LocalOnly);
        reopened.recover().await.unwrap();
        let resumed = reopened.resume_task(&task, &cp.checkpoint_id).await;
        if safety == ReplaySafety::Safe {
            assert!(cp.resume.eligible);
            resumed.unwrap();
            let events = finished(&reopened, &task).await;
            assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
            assert_eq!(write.calls.load(Ordering::SeqCst), 2);
            assert_eq!(write.inner.value("greeting").as_deref(), Some("hello"));
        } else {
            assert!(!cp.resume.eligible);
            assert_eq!(resumed.unwrap_err().code, ErrorCode::InterruptedUnsafe);
            assert_eq!(write.calls.load(Ordering::SeqCst), 1);
            assert_eq!(write.inner.invocations(), 0);
        }
    }
}
