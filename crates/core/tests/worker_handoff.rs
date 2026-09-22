//! Handing a task between two configurations of the built-in worker, each
//! pinned to a model (ADR 0048), through the real service, host, router,
//! permission and release managers, and journal.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

/// A model that asks for the echo tool until it has seen `enough` results,
/// then answers.
struct Model {
    provider: &'static str,
    location: ModelLocation,
    enough: usize,
    requests: Mutex<Vec<AuthorizedModelRequest>>,
}

impl Model {
    fn new(provider: &'static str, location: ModelLocation, enough: usize) -> Arc<Self> {
        Arc::new(Self {
            provider,
            location,
            enough,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn pin(&self) -> PinnedModel {
        PinnedModel {
            model: RoutedModel {
                provider_id: self.provider.into(),
                model_id: "model".into(),
            },
            display_name: format!("The {} model", self.provider),
            location: self.location,
        }
    }
}

#[async_trait]
impl ModelProvider for Model {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.provider)
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        let mut model = FakeModelProvider::descriptor();
        model.provider_id = self.provider.into();
        model.model_id = "model".into();
        model.location = self.location;
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
        if results >= self.enough {
            sender
                .send(ModelEvent::TextDelta {
                    stream_id,
                    offset: 0,
                    text: format!("Done by {} after {results} results.", self.provider),
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
                    call_id: Some(format!("call-{results}")),
                    name: Some("fake.tool.echo".into()),
                    delta: "{\"value\":\"a fact\"}".into(),
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

/// The built-in worker as the composition root builds it: two steps when the
/// router chooses, the default when pinned, so the first runs out.
fn service(mode: RoutingMode, models: &[Arc<Model>], approver: ScriptedApprover) -> TaskService {
    let events = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new()) as Arc<dyn ToolProvider>],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(approver),
        ids.clone(),
    )
    .unwrap();
    let tools_offered = vec!["fake.tool.echo".to_owned()];
    let short = ReadOnlyLimits {
        steps: 2,
        ..ReadOnlyLimits::default()
    };
    let automatic = {
        let tools = tools_offered.clone();
        let short = short.clone();
        Arc::new(move |admission| {
            Arc::new(ReferenceWorker::new(tools.clone(), short.clone()).admitting(admission))
                as Arc<dyn AgentProvider>
        }) as WorkerChoice
    };
    let pinned = {
        let tools = tools_offered.clone();
        Arc::new(move |admission, model: &PinnedModel| {
            Arc::new(
                ReferenceWorker::new(tools.clone(), ReadOnlyLimits::default())
                    .admitting(admission)
                    .pinned(model.clone()),
            ) as Arc<dyn AgentProvider>
        }) as PinnedWorkerChoice
    };
    TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
        },
        Providers {
            models: models
                .iter()
                .map(|model| Arc::clone(model) as Arc<dyn ModelProvider>)
                .collect(),
            tools,
            permissions,
            releases,
            worker: automatic(ToolAdmission::READ_ONLY),
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(mode)
    .with_worker_choice(automatic)
    .with_pinned_workers(models.iter().map(|model| model.pin()).collect(), pinned)
}

async fn start(service: &TaskService, admission: ToolAdmission) -> TaskId {
    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Question")
        .await
        .unwrap();
    service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Find the fact".into(),
            admission: Some(admission),
            ..Default::default()
        })
        .await
        .unwrap()
}

/// Wait until the task has ended, or has stopped for another worker.
async fn settled(service: &TaskService, task: &TaskId, after: usize) -> Vec<EventEnvelope> {
    timeout(Duration::from_secs(10), async {
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
            let stopped = fresh.last().is_some_and(|event| {
                event.kind == TaskEventKind::CheckpointSaved
            }) && fresh.iter().any(|event| matches!(&event.payload,
                EventPayload::TaskStateChanged { reason_code, .. } if reason_code == "worker.stopped"));
            if ended || stopped {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap()
}

fn stopped_with(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::DiagnosticRecorded { code, .. }
                if code.starts_with("worker.stopped.") =>
            {
                Some(code.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_task_that_ran_out_of_steps_is_handed_to_another_model_and_finishes() {
    let small = Model::new("a-small", ModelLocation::Local, usize::MAX);
    let large = Model::new("b-large", ModelLocation::Local, 2);
    let service = service(
        RoutingMode::LocalOnly,
        &[small.clone(), large.clone()],
        ScriptedApprover::unavailable(),
    );

    // Both models are offered by name, after the one the router chooses.
    let offered: Vec<String> = service
        .workers()
        .into_iter()
        .map(|worker| worker.agent_id)
        .collect();
    assert_eq!(
        offered,
        [
            "builtin.reference",
            "builtin.reference@a-small/model",
            "builtin.reference@b-large/model"
        ]
    );

    // The router picks the small model; two steps are not enough, so the
    // task stops for another worker instead of failing.
    let task = start(&service, ToolAdmission::READ_WRITE).await;
    let events = settled(&service, &task, 0).await;
    assert_eq!(stopped_with(&events), ["worker.stopped.limitExceeded"]);
    assert_eq!(
        service.get_task(&task).await.unwrap().state,
        TaskState::Interrupted
    );
    let checkpoint = service.latest_checkpoint(&task).await.unwrap().unwrap();
    assert!(checkpoint.resume.eligible, "{checkpoint:?}");
    let seen = events.len();

    // Resuming the same worker gives it no more budget: it stops again.
    service
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    let events = settled(&service, &task, seen).await;
    assert_eq!(stopped_with(&events).len(), 2);
    let checkpoint = service.latest_checkpoint(&task).await.unwrap().unwrap();
    let seen = events.len();

    // A name nobody offered is refused.
    let unknown = service
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@elsewhere/x",
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::NotFound);

    // The large model takes it, with a budget of its own and the whole
    // canonical history, and finishes.
    let asked_small = small.asked();
    service
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@b-large/model",
        )
        .await
        .unwrap();
    let events = settled(&service, &task, seen).await;
    let Some(EventPayload::TaskCompleted { summary, .. }) = events.last().map(|e| &e.payload)
    else {
        panic!("expected completion: {:?}", events.last());
    };
    assert!(
        summary.starts_with("Done by b-large after 2 results"),
        "{summary}"
    );
    assert_eq!(
        small.asked(),
        asked_small,
        "the small model is not asked again"
    );

    // The record says who took over, with what, and why the model was used.
    let fresh = &events[seen..];
    assert!(fresh.iter().any(|event| matches!(&event.payload,
        EventPayload::TaskStateChanged { reason_code, .. } if reason_code == "user.handedOver")));
    assert!(fresh.iter().any(|event| matches!(&event.payload,
        EventPayload::WorkerSelected { descriptor }
            if descriptor.agent_id == "builtin.reference@b-large/model"
                && descriptor.display_name == "Embedded read-write worker on The b-large model")));
    let routed: Vec<&RoutingDecision> = fresh
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::RoutingDecided { decision }
                if decision.model.provider_id == "b-large" =>
            {
                Some(decision)
            }
            _ => None,
        })
        .collect();
    assert!(!routed.is_empty());
    assert!(routed.iter().all(|decision| {
        decision
            .reason_codes
            .contains(&routing_reason::USER_SELECTED.to_owned())
    }));
}

#[tokio::test]
async fn a_task_can_start_on_the_worker_a_person_named() {
    let small = Model::new("a-small", ModelLocation::Local, usize::MAX);
    let large = Model::new("b-large", ModelLocation::Local, 0);
    let service = service(
        RoutingMode::LocalOnly,
        &[small.clone(), large.clone()],
        ScriptedApprover::unavailable(),
    );
    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Question")
        .await
        .unwrap();
    let task = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id.clone()),
            goal: "Find the fact".into(),
            worker: Some("builtin.reference@b-large/model".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = settled(&service, &task, 0).await;
    assert_eq!(events.last().unwrap().kind, TaskEventKind::TaskCompleted);
    // It ran on the model that was named, and the other was never asked.
    assert!(events.iter().any(|event| matches!(&event.payload,
        EventPayload::WorkerSelected { descriptor }
            if descriptor.agent_id == "builtin.reference@b-large/model")));
    assert_eq!(small.asked(), 0);
    assert!(large.asked() > 0);

    // A name nobody offers is refused, and no task is written for it.
    let before = service
        .list_tasks(Some(&conversation.conversation_id))
        .await
        .unwrap()
        .len();
    let unknown = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id.clone()),
            goal: "Find the fact".into(),
            worker: Some("builtin.reference@nobody/model".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::NotFound);
    assert_eq!(
        service
            .list_tasks(Some(&conversation.conversation_id))
            .await
            .unwrap()
            .len(),
        before
    );
}

#[tokio::test]
async fn a_local_only_task_cannot_start_on_a_model_off_this_device() {
    let small = Model::new("a-small", ModelLocation::Local, usize::MAX);
    let cloud = Model::new("c-cloud", ModelLocation::Cloud, 0);
    let service = service(
        RoutingMode::LocalOnly,
        &[small.clone(), cloud.clone()],
        ScriptedApprover::unavailable(),
    );
    let refused = service
        .start_task(StartTaskRequest {
            goal: "Find the fact".into(),
            worker: Some("builtin.reference@c-cloud/model".into()),
            routing_mode: Some(RoutingMode::LocalOnly),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(
        refused.message.contains("Local Only"),
        "{}",
        refused.message
    );
    assert_eq!(cloud.asked(), 0);

    // The same worker under Automatic is the person's own choice, and runs.
    let service = service_with(RoutingMode::Automatic, &[small, cloud.clone()]);
    let task = service
        .start_task(StartTaskRequest {
            goal: "Find the fact".into(),
            worker: Some("builtin.reference@c-cloud/model".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = settled(&service, &task, 0).await;
    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    assert!(cloud.asked() > 0);
}

/// The same service, with a person at the approvals.
fn service_with(mode: RoutingMode, models: &[Arc<Model>]) -> TaskService {
    service(
        mode,
        models,
        ScriptedApprover::always(ApprovalDecisionKind::AllowOnce),
    )
}

#[tokio::test]
async fn a_pinned_cloud_model_is_asked_about_under_automatic_and_refused_under_local_only() {
    // Automatic: the person chose the cloud worker, so it may be used, and
    // what is released to it is put to them.
    let small = Model::new("a-small", ModelLocation::Local, usize::MAX);
    let cloud = Model::new("c-cloud", ModelLocation::Cloud, 2);
    let tasks = service(
        RoutingMode::Automatic,
        &[small.clone(), cloud.clone()],
        ScriptedApprover::always(ApprovalDecisionKind::AllowOnce),
    );
    let task = start(&tasks, ToolAdmission::READ_ONLY).await;
    let events = settled(&tasks, &task, 0).await;
    assert_eq!(cloud.asked(), 0, "Automatic kept the task on the device");
    let checkpoint = tasks.latest_checkpoint(&task).await.unwrap().unwrap();
    tasks
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@c-cloud/model",
        )
        .await
        .unwrap();
    let events = settled(&tasks, &task, events.len()).await;
    assert_eq!(
        events.last().unwrap().kind,
        TaskEventKind::TaskCompleted,
        "{events:?}"
    );
    assert!(cloud.asked() > 0);
    assert!(
        events
            .iter()
            .any(|event| event.kind == TaskEventKind::ContextReleased),
        "what the cloud model was sent is on the record"
    );

    // Local Only: the same choice is refused, and the task stops again
    // rather than reaching the cloud.
    let small = Model::new("a-small", ModelLocation::Local, usize::MAX);
    let cloud = Model::new("c-cloud", ModelLocation::Cloud, 2);
    let tasks = service(
        RoutingMode::LocalOnly,
        &[small.clone(), cloud.clone()],
        ScriptedApprover::always(ApprovalDecisionKind::AllowOnce),
    );
    let task = start(&tasks, ToolAdmission::READ_ONLY).await;
    let events = settled(&tasks, &task, 0).await;
    let checkpoint = tasks.latest_checkpoint(&task).await.unwrap().unwrap();
    let refused = tasks
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@c-cloud/model",
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(
        refused.message.contains("Local Only"),
        "{}",
        refused.message
    );
    assert_eq!(cloud.asked(), 0);
    // Nothing moved: the task is still waiting, on the same checkpoint.
    assert_eq!(
        tasks.replay_task_events(&task, 0).await.unwrap().len(),
        events.len()
    );
}

#[tokio::test]
async fn a_handoff_keeps_what_the_task_was_allowed_and_a_pinned_task_resumes_on_its_model() {
    let small = Model::new("a-small", ModelLocation::Local, usize::MAX);
    let large = Model::new("b-large", ModelLocation::Local, usize::MAX);
    let tasks = service(
        RoutingMode::LocalOnly,
        &[small.clone(), large.clone()],
        ScriptedApprover::unavailable(),
    );
    let task = start(&tasks, ToolAdmission::READ_ONLY.with_apps()).await;
    let events = settled(&tasks, &task, 0).await;
    let checkpoint = tasks.latest_checkpoint(&task).await.unwrap().unwrap();
    tasks
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@b-large/model",
        )
        .await
        .unwrap();
    // The large model loops too, and runs out of its own forty steps.
    let events = settled(&tasks, &task, events.len()).await;
    assert_eq!(stopped_with(&events).len(), 2);

    // Handing it to the worker that already has it is a resume, not a handoff.
    let checkpoint = tasks.latest_checkpoint(&task).await.unwrap().unwrap();
    let same = tasks
        .hand_off_to(
            &task,
            &checkpoint.checkpoint_id,
            "builtin.reference@b-large/model",
        )
        .await
        .unwrap_err();
    assert_eq!(same.code, ErrorCode::Conflict);

    // A resume finds the pinned worker, with Apps still admitted: a worker
    // that dropped them would be refused at its first turn.
    let asked = large.asked();
    tasks
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    let events = settled(&tasks, &task, events.len()).await;
    assert_eq!(stopped_with(&events).len(), 3, "{events:?}");
    assert_eq!(
        stopped_with(&events).last().map(String::as_str),
        Some("worker.stopped.limitExceeded")
    );
    assert_eq!(large.asked(), asked, "no budget was given back");
}
