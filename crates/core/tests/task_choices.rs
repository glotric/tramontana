//! A task's own admission and routing mode: chosen when it starts, recorded,
//! and kept when it is resumed (ADR 0031).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

/// A model endpoint that counts the requests reaching it.
#[derive(Debug)]
struct Endpoint {
    provider_id: &'static str,
    model_id: &'static str,
    location: ModelLocation,
    reached: AtomicUsize,
}

impl Endpoint {
    fn new(
        provider_id: &'static str,
        model_id: &'static str,
        location: ModelLocation,
    ) -> Arc<Self> {
        Arc::new(Self {
            provider_id,
            model_id,
            location,
            reached: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ModelProvider for Endpoint {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.provider_id)
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        vec![ModelDescriptor {
            provider_id: self.provider_id.to_owned(),
            model_id: self.model_id.to_owned(),
            display_name: self.model_id.to_owned(),
            location: self.location,
            capabilities: vec![
                ModelCapability::Text,
                ModelCapability::Streaming,
                ModelCapability::Cancellation,
                ModelCapability::ToolCalls,
            ],
            context_limit: None,
            health: Health::Available,
            health_reason_code: None,
        }]
    }

    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        self.reached.fetch_add(1, Ordering::SeqCst);
        FakeModelProvider::streaming(["answer"])
            .stream(request, cancel)
            .await
    }
}

/// A tool whose first run waits until the task is interrupted, so the task
/// can be checkpointed with a step pending. Later runs finish at once.
#[derive(Debug, Default)]
struct HeldTool {
    runs: AtomicUsize,
    entered: Notify,
}

#[async_trait]
impl ToolProvider for HeldTool {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        FakeToolProvider::new().list_tools()
    }

    async fn invoke(
        &self,
        _request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.notify_one();
            cancel.cancelled().await;
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "synthetic interruption",
                CorrelationId::new("test"),
            ));
        }
        Ok(ToolOutcome {
            content: serde_json::json!({"value": "done"}),
            display_summary: "Held step completed".into(),
            side_effect: self.list_tools()[0].side_effect,
            truncated: false,
            changes: Vec::new(),
        })
    }
}

/// A worker named for the admission it was built with. It asks for one
/// model turn and, when `uses_tool`, runs one tool step.
#[derive(Debug)]
struct Named {
    name: &'static str,
    uses_tool: bool,
}

#[async_trait]
impl AgentProvider for Named {
    fn describe(&self) -> AgentDescriptor {
        let mut descriptor = FakeWorker::new().describe();
        descriptor.display_name = self.name.to_owned();
        descriptor
    }

    async fn run(
        &self,
        request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        host.call_model(&request.goal, &cancel).await?;
        if self.uses_tool {
            host.call_tool_step(
                "held",
                "fake.tool.echo",
                serde_json::json!({"value": "held"}),
                &cancel,
            )
            .await?;
        }
        Ok(WorkerOutcome::Completed {
            summary: format!("{} finished", self.name),
        })
    }
}

fn named(admission: ToolAdmission) -> &'static str {
    match (admission.writes, admission.recipes) {
        (false, false) => "read-only",
        (true, false) => "read-write",
        (false, true) => "read-only with recipes",
        (true, true) => "read-write with recipes",
    }
}

/// What a composition root that lets each task choose would give the service.
fn choice(uses_tool: bool) -> WorkerChoice {
    Arc::new(move |admission| {
        Arc::new(Named {
            name: named(admission),
            uses_tool,
        }) as Arc<dyn AgentProvider>
    })
}

/// One durable profile, with a cloud and a local endpoint, that several
/// processes can open in turn.
struct Profile {
    repositories: Repositories,
    ids: Arc<SequentialIdSource>,
    cloud: Arc<Endpoint>,
    local: Arc<Endpoint>,
    tool: Arc<HeldTool>,
    uses_tool: bool,
}

impl Profile {
    fn new(uses_tool: bool) -> Self {
        let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
        Self {
            repositories: Repositories {
                projects: Arc::new(InMemoryProjectRepository::new()),
                conversations: Arc::new(InMemoryConversationRepository::new()),
                tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
                events,
                configuration: Arc::new(InMemoryConfigurationRepository::new()),
                checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
            },
            ids: Arc::new(SequentialIdSource::new()),
            cloud: Endpoint::new("cloud", "remote", ModelLocation::Cloud),
            local: Endpoint::new("local", "on-device", ModelLocation::Local),
            tool: Arc::new(HeldTool::default()),
            uses_tool,
        }
    }

    /// A service over this profile, as one process would open it.
    fn open(&self, mode: RoutingMode, choice: Option<WorkerChoice>) -> TaskService {
        let (tools, permissions, releases) = assemble_tool_plane(
            vec![self.tool.clone()],
            Arc::new(InMemoryApprovalStore::new()),
            Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
            self.ids.clone(),
        )
        .unwrap();
        let service = TaskService::new(
            self.repositories.clone(),
            Providers {
                models: vec![self.cloud.clone(), self.local.clone()],
                tools,
                permissions,
                releases,
                worker: Arc::new(Named {
                    name: "the service's own",
                    uses_tool: self.uses_tool,
                }),
            },
            Arc::new(FakeClock::new()),
            self.ids.clone(),
        )
        .with_routing_mode(mode);
        match choice {
            Some(choice) => service.with_worker_choice(choice),
            None => service,
        }
    }
}

async fn settled(service: &TaskService, task: &TaskId) -> TaskState {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = service.get_task(task).await.unwrap().state;
            if state.is_terminal() || state == TaskState::Interrupted {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task never settled")
}

async fn history(service: &TaskService, task: &TaskId) -> Vec<EventPayload> {
    service
        .replay_task_events(task, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

fn workers(history: &[EventPayload]) -> Vec<String> {
    history
        .iter()
        .filter_map(|payload| match payload {
            EventPayload::WorkerSelected { descriptor } => Some(descriptor.display_name.clone()),
            _ => None,
        })
        .collect()
}

fn decisions(history: &[EventPayload]) -> Vec<RoutingDecision> {
    history
        .iter()
        .filter_map(|payload| match payload {
            EventPayload::RoutingDecided { decision } => Some(decision.clone()),
            _ => None,
        })
        .collect()
}

fn created_mode(history: &[EventPayload]) -> Option<RoutingMode> {
    history.iter().find_map(|payload| match payload {
        EventPayload::TaskCreated { routing_mode, .. } => *routing_mode,
        _ => None,
    })
}

fn goal(text: &str) -> StartTaskRequest {
    StartTaskRequest {
        goal: text.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_task_started_local_only_stays_on_the_device_while_the_profile_prefers_the_cloud() {
    let profile = Profile::new(false);
    let service = profile.open(RoutingMode::CloudPreferred, None);

    // The profile's own mode, for contrast: a task that names no mode is
    // routed to the cloud endpoint. Without this, the Local Only task below
    // could pass only because the cloud was never a candidate.
    let preferring = service.start_task(goal("Say hello")).await.unwrap();
    settled(&service, &preferring).await;
    let preferred = history(&service, &preferring).await;
    assert_eq!(created_mode(&preferred), Some(RoutingMode::CloudPreferred));
    assert_eq!(decisions(&preferred)[0].model.model_id, "remote");

    let cloud_before = profile.cloud.reached.load(Ordering::SeqCst);
    let local_only = service
        .start_task(StartTaskRequest {
            routing_mode: Some(RoutingMode::LocalOnly),
            ..goal("Say hello")
        })
        .await
        .unwrap();
    assert_eq!(settled(&service, &local_only).await, TaskState::Completed);

    let recorded = history(&service, &local_only).await;
    assert_eq!(created_mode(&recorded), Some(RoutingMode::LocalOnly));
    let decided = decisions(&recorded);
    assert!(!decided.is_empty(), "the task routed nothing");
    for decision in decided {
        assert_eq!(decision.mode, RoutingMode::LocalOnly);
        assert_eq!(decision.model.model_id, "on-device");
        assert!(!decision.leaves_the_device());
    }
    assert_eq!(
        profile.cloud.reached.load(Ordering::SeqCst),
        cloud_before,
        "a Local Only task reached the cloud endpoint"
    );
    assert_eq!(
        service.routing_mode(),
        RoutingMode::CloudPreferred,
        "one task's mode changed the profile's"
    );
}

#[tokio::test]
async fn a_task_runs_the_worker_its_admission_chose() {
    let profile = Profile::new(false);
    let service = profile.open(RoutingMode::Automatic, Some(choice(false)));

    for (admission, expected) in [
        (Some(ToolAdmission::READ_WRITE), "read-write"),
        (
            Some(ToolAdmission::READ_ONLY.with_recipes()),
            "read-only with recipes",
        ),
        (None, "the service's own"),
    ] {
        let task = service
            .start_task(StartTaskRequest {
                admission,
                ..goal("Say hello")
            })
            .await
            .unwrap();
        assert_eq!(settled(&service, &task).await, TaskState::Completed);
        assert_eq!(workers(&history(&service, &task).await), [expected]);
    }
}

#[tokio::test]
async fn a_service_that_cannot_choose_a_worker_refuses_an_admission_and_records_nothing() {
    let profile = Profile::new(false);
    let service = profile.open(RoutingMode::Automatic, None);

    let refused = service
        .start_task(StartTaskRequest {
            admission: Some(ToolAdmission::READ_WRITE),
            ..goal("Write something")
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::CapabilityUnsupported);
    assert!(service.list_tasks(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_resumed_task_keeps_the_worker_and_mode_it_started_with() {
    let profile = Profile::new(true);
    let first = profile.open(RoutingMode::Automatic, Some(choice(true)));
    let project = first
        .create_project("Synthetic project", vec!["/synthetic".into()])
        .await
        .unwrap();
    let conversation = first
        .create_conversation(Some(project.project_id), "Test")
        .await
        .unwrap();
    let task = first
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Hold a step".into(),
            admission: Some(ToolAdmission::READ_WRITE),
            routing_mode: Some(RoutingMode::LocalOnly),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), profile.tool.entered.notified())
        .await
        .unwrap();
    let checkpoint = first.create_checkpoint(&task).await.unwrap();
    assert!(checkpoint.resume.eligible);
    drop(first);

    // A process that cannot build the recorded worker refuses, rather than
    // continue the task with its model offered something else.
    let fixed = profile.open(RoutingMode::Automatic, None);
    fixed.recover().await.unwrap();
    let refused = fixed
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    drop(fixed);

    // The profile prefers the cloud now. The task was started Local Only, so
    // it continues Local Only, with the worker it started with.
    let reopened = profile.open(RoutingMode::CloudPreferred, Some(choice(true)));
    reopened.recover().await.unwrap();
    reopened
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    assert_eq!(settled(&reopened, &task).await, TaskState::Completed);

    let recorded = history(&reopened, &task).await;
    assert_eq!(workers(&recorded), ["read-write", "read-write"]);
    let decided = decisions(&recorded);
    assert!(
        decided.len() >= 2,
        "expected decisions before and after the resume, got {}",
        decided.len()
    );
    for decision in decided {
        assert_eq!(decision.mode, RoutingMode::LocalOnly);
        assert_eq!(decision.model.model_id, "on-device");
    }
    assert_eq!(profile.cloud.reached.load(Ordering::SeqCst), 0);
    assert_eq!(profile.tool.runs.load(Ordering::SeqCst), 2);
}
