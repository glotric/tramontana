//! Real TaskService/host routing and approval, with synthetic external boundaries.
#![allow(clippy::unwrap_used, reason = "assertions in tests")]
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    sync::{Notify, mpsc},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

#[derive(Debug, Clone, Copy)]
enum Work {
    Repeat,
    Concurrent,
    Tampered,
    Once,
}
#[derive(Debug)]
struct Worker(Work);
#[async_trait]
impl AgentProvider for Worker {
    fn describe(&self) -> AgentDescriptor {
        FakeWorker::new().describe()
    }
    async fn run(
        &self,
        request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        match self.0 {
            Work::Concurrent => {
                let (first, second) = tokio::join!(
                    host.call_model(&request.goal, &cancel),
                    host.call_model(&request.goal, &cancel)
                );
                first?;
                second?;
            }
            Work::Tampered => {
                host.call_model("undisclosed worker content", &cancel)
                    .await?;
            }
            work => {
                host.call_model(&request.goal, &cancel).await?;
                if matches!(work, Work::Repeat) {
                    host.call_model(&request.goal, &cancel).await?;
                }
            }
        }
        Ok(WorkerOutcome::Completed {
            summary: "Finished fixture".into(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Discovery {
    Normal,
    Silent,
    Disappear,
}
#[derive(Debug)]
struct Model {
    discovery: Discovery,
    lists: AtomicUsize,
    cloud: bool,
    requests: Mutex<Vec<AuthorizedModelRequest>>,
    discovered: Notify,
}
impl Model {
    fn new(cloud: bool, discovery: Discovery) -> Self {
        Self {
            cloud,
            discovery,
            lists: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
            discovered: Notify::new(),
        }
    }
}
#[async_trait]
impl ModelProvider for Model {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }
    async fn list_models(&self) -> Vec<ModelDescriptor> {
        let n = self.lists.fetch_add(1, Ordering::SeqCst);
        if n > 0 {
            self.discovered.notify_one();
            match self.discovery {
                Discovery::Silent => std::future::pending::<()>().await,
                Discovery::Disappear => return vec![],
                Discovery::Normal => {}
            }
        }
        let mut descriptor = FakeModelProvider::descriptor();
        descriptor.location = if self.cloud {
            ModelLocation::Cloud
        } else {
            ModelLocation::Local
        };
        vec![descriptor]
    }
    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        self.requests.lock().unwrap().push(request.clone());
        FakeModelProvider::streaming(["Fixture answer"])
            .stream(request, cancel)
            .await
    }
}

#[derive(Debug, Clone, Copy)]
enum Answer {
    Allow,
    DenySecond,
    Silent,
}
#[derive(Debug)]
struct Approval {
    answer: Answer,
    count: AtomicUsize,
    asked: Notify,
}
impl Approval {
    fn new(answer: Answer) -> Self {
        Self {
            answer,
            count: AtomicUsize::new(0),
            asked: Notify::new(),
        }
    }
}
#[async_trait]
impl Approver for Approval {
    async fn ask(
        &self,
        _request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        self.asked.notify_one();
        match self.answer {
            Answer::Silent => std::future::pending().await,
            Answer::DenySecond if n > 0 => Ok(ApprovalDecisionKind::Deny),
            _ => Ok(ApprovalDecisionKind::AllowOnce),
        }
    }
}
fn service(model: Arc<Model>, approval: Arc<Approval>, work: Work) -> TaskService {
    test_service(
        Arc::new(InMemoryEventStore::new()),
        vec![model],
        vec![Arc::new(FakeToolProvider::new())],
        approval,
        Arc::new(Worker(work)),
    )
    .with_routing_mode(RoutingMode::CloudPreferred)
}
async fn start(service: &TaskService) -> TaskId {
    service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "Inspect fixture".into(),
            ..Default::default()
        })
        .await
        .unwrap()
}
async fn finished(service: &TaskService, task: &TaskId) -> Vec<EventEnvelope> {
    wait_state(
        service,
        task,
        Duration::from_secs(130),
        Duration::from_millis(10),
        TaskState::is_terminal,
    )
    .await;
    service.replay_task_events(task, 0).await.unwrap()
}
fn failed(events: &[EventEnvelope], code: ErrorCode) {
    assert!(events.iter().any(
        |event| matches!(&event.payload,EventPayload::TaskFailed {error} if error.code==code)
    ));
}

#[tokio::test(start_paused = true)]
async fn repeated_and_concurrent_requests_get_distinct_ordered_release_records() {
    for work in [Work::Repeat, Work::Concurrent] {
        let model = Arc::new(Model::new(true, Discovery::Normal));
        let approval = Arc::new(Approval::new(Answer::Allow));
        let service = service(model.clone(), approval.clone(), work);
        let task = start(&service).await;
        let events = finished(&service, &task).await;
        assert_eq!(
            service.get_task(&task).await.unwrap().state,
            TaskState::Completed,
            "{events:?}"
        );
        assert_eq!(approval.count.load(Ordering::SeqCst), 2);
        assert_eq!(model.lists.load(Ordering::SeqCst), 3); // Initial plan plus each call.
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_ne!(
            requests[0].request().request_id,
            requests[1].request().request_id
        );
        assert_ne!(
            requests[0].plan().context_manifest_id,
            requests[1].plan().context_manifest_id
        );
        assert_ne!(
            requests[0].plan().decision_id,
            requests[1].plan().decision_id
        );
        for request in requests.iter() {
            let context=events.iter().position(|e|matches!(&e.payload,EventPayload::ContextPrepared {manifest} if manifest.context_manifest_id==request.plan().context_manifest_id)).unwrap();
            let route=events.iter().position(|e|matches!(&e.payload,EventPayload::RoutingDecided {decision} if decision.decision_id==request.plan().decision_id)).unwrap();
            let release=events.iter().position(|e|matches!(&e.payload,EventPayload::ContextReleased {plan} if plan.release_id==request.plan().release_id)).unwrap();
            let start=events.iter().position(|e|matches!(&e.payload,EventPayload::ModelStarted {stream_id,..} if stream_id==&request.request().request_id)).unwrap();
            assert!(context < route && route < release && release < start);
        }
        let starts: Vec<_> = events
            .iter()
            .filter(|e| e.kind == TaskEventKind::ModelStarted)
            .map(|e| e.sequence)
            .collect();
        let first_message = events
            .iter()
            .find(|e| e.kind == TaskEventKind::MessageCommitted)
            .unwrap()
            .sequence;
        assert!(starts[0] < first_message && first_message < starts[1]);
    }
}

#[tokio::test(start_paused = true)]
async fn a_second_denial_does_not_reuse_the_first_approval() {
    let model = Arc::new(Model::new(true, Discovery::Normal));
    let approval = Arc::new(Approval::new(Answer::DenySecond));
    let service = service(model.clone(), approval.clone(), Work::Repeat);
    let task = start(&service).await;
    failed(
        &finished(&service, &task).await,
        ErrorCode::PermissionDenied,
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert_eq!(approval.count.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn workers_cannot_make_new_content_eligible_by_requesting_a_manifest() {
    let model = Arc::new(Model::new(true, Discovery::Normal));
    let approval = Arc::new(Approval::new(Answer::Allow));
    let service = service(model.clone(), approval.clone(), Work::Tampered);
    let task = start(&service).await;
    let events = finished(&service, &task).await;
    failed(&events, ErrorCode::PermissionDenied);
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(approval.count.load(Ordering::SeqCst), 0);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("undisclosed worker content")
    );
}

#[tokio::test(start_paused = true)]
async fn changed_availability_is_checked_before_approval_or_inference() {
    let model = Arc::new(Model::new(true, Discovery::Disappear));
    let approval = Arc::new(Approval::new(Answer::Allow));
    let service = service(model.clone(), approval.clone(), Work::Once);
    let task = start(&service).await;
    failed(
        &finished(&service, &task).await,
        ErrorCode::ProviderUnavailable,
    );
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(approval.count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn configuration_is_disclosed_as_excluded_not_silently_uploaded() {
    let model = Arc::new(Model::new(true, Discovery::Normal));
    let service = service(
        model.clone(),
        Arc::new(Approval::new(Answer::Allow)),
        Work::Once,
    );
    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    service
        .resolve_configuration(
            &project.project_id,
            vec![tramontana_core::config::RawSource {
                relative_path: "AGENTS.md".into(),
                kind: ConfigurationKind::RootAgents,
                content: b"# Fixture instructions\nAPI_KEY=synthetic-secret".to_vec(),
            }],
            &tramontana_core::config::ConfigurationSelection::default(),
        )
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Fixture")
        .await
        .unwrap();
    let task = service
        .start_task(start_request(
            &conversation.conversation_id,
            "Inspect fixture",
        ))
        .await
        .unwrap();
    let events = finished(&service, &task).await;
    assert_eq!(
        service.get_task(&task).await.unwrap().state,
        TaskState::Completed
    );
    let requests = model.requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.request().messages.len(), 1);
    assert!(request.plan().redactions.is_empty()); // Excluded source redactions do not describe sent text.
    assert!(
        request
            .plan()
            .exclusions
            .iter()
            .any(|source| source.source_id == "AGENTS.md"
                && source.reason_code == "release.notTransmitted")
    );
    assert!(
        !serde_json::to_string(request.request())
            .unwrap()
            .contains("Fixture instructions")
    );
    let manifest = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ContextPrepared { manifest }
                if manifest.context_manifest_id == request.plan().context_manifest_id =>
            {
                Some(manifest)
            }
            _ => None,
        })
        .unwrap();
    assert!(
        manifest
            .sources
            .iter()
            .any(|source| source.source_id == "AGENTS.md" && !source.included)
    );
    assert!(!manifest.redactions.is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancellation_and_deadlines_cover_discovery_and_approval_waits() {
    for work in [Work::Once, Work::Concurrent] {
        for during_discovery in [false, true] {
            for cancel in [false, true] {
                let model = Arc::new(Model::new(
                    true,
                    if during_discovery {
                        Discovery::Silent
                    } else {
                        Discovery::Normal
                    },
                ));
                let approval = Arc::new(Approval::new(if during_discovery {
                    Answer::Allow
                } else {
                    Answer::Silent
                }));
                let service = service(model.clone(), approval.clone(), work);
                let task = start(&service).await;
                if during_discovery {
                    timeout(Duration::from_secs(1), model.discovered.notified())
                        .await
                        .unwrap();
                } else {
                    timeout(Duration::from_secs(1), approval.asked.notified())
                        .await
                        .unwrap();
                }
                if cancel {
                    service.cancel_task(&task).await.unwrap();
                }
                let events = finished(&service, &task).await;
                if cancel {
                    assert_eq!(
                        service.get_task(&task).await.unwrap().state,
                        TaskState::Cancelled
                    );
                } else {
                    failed(&events, ErrorCode::Timeout);
                }
                assert!(model.requests.lock().unwrap().is_empty());
                assert!(!events.iter().any(|e| e.kind == TaskEventKind::ModelStarted));
            }
        }
    }
}
