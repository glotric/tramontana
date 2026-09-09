//! M4 recovery/resume regressions. All effects are synthetic counters.
#![allow(clippy::unwrap_used, clippy::panic)]

use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

#[derive(Debug, Default)]
struct CounterTool {
    completed_calls: AtomicUsize,
    pending_calls: AtomicUsize,
    entered: Notify,
    unsafe_effect: bool,
}

#[async_trait]
impl ToolProvider for CounterTool {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        let mut tools = FakeToolProvider::new().list_tools();
        if self.unsafe_effect {
            tools[0].side_effect = SideEffect::LocalReversible;
        }
        tools
    }
    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let is_pending = request.arguments()["value"] == "pending";
        if is_pending {
            if self.pending_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                cancel.cancelled().await;
                return Err(ErrorEnvelope::new(
                    ErrorCode::Cancelled,
                    "synthetic interruption",
                    CorrelationId::new("test"),
                ));
            }
        } else {
            self.completed_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(ToolOutcome {
            content: serde_json::json!({"value":"raw output must not be replayed"}),
            display_summary: "Synthetic step completed".into(),
            side_effect: self.list_tools()[0].side_effect,
            truncated: false,
        })
    }
}

#[derive(Debug, Default)]
struct TwoStepWorker {
    changed_arguments: AtomicBool,
    skipped: AtomicBool,
    runs: AtomicUsize,
}

#[async_trait]
impl AgentProvider for TwoStepWorker {
    fn describe(&self) -> AgentDescriptor {
        FakeWorker::new().describe()
    }
    async fn run(
        &self,
        request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let value = if self.changed_arguments.load(Ordering::SeqCst) {
            "changed"
        } else {
            "done"
        };
        let result = host
            .call_tool_step(
                "first",
                "fake.tool.echo",
                serde_json::json!({"value":value}),
                &cancel,
            )
            .await?;
        if request.checkpoint.is_some() {
            assert_eq!(result["skipped"], true);
            assert!(
                result.get("value").is_none(),
                "raw historical output is not a checkpoint"
            );
            self.skipped.store(true, Ordering::SeqCst);
        }
        host.call_tool_step(
            "second",
            "fake.tool.echo",
            serde_json::json!({"value":"pending"}),
            &cancel,
        )
        .await?;
        Ok(WorkerOutcome::Completed {
            summary: "Two steps completed".into(),
        })
    }
}

struct Harness {
    repos: Repositories,
    providers: Providers,
    ids: Arc<SequentialIdSource>,
    tool: Arc<CounterTool>,
    worker: Arc<TwoStepWorker>,
}

impl Harness {
    fn new(unsafe_effect: bool) -> Self {
        let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
        let repos = Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
        };
        let ids = Arc::new(SequentialIdSource::new());
        let tool = Arc::new(CounterTool {
            unsafe_effect,
            ..CounterTool::default()
        });
        let worker = Arc::new(TwoStepWorker::default());
        let (tools, permissions, releases) = assemble_tool_plane(
            vec![tool.clone()],
            Arc::new(InMemoryApprovalStore::new()),
            Arc::new(ScriptedApprover::unavailable()),
            ids.clone(),
        )
        .unwrap();
        let providers = Providers {
            model: Arc::new(FakeModelProvider::streaming(["answer"])),
            tools,
            permissions,
            releases,
            worker: worker.clone(),
        };
        Self {
            repos,
            providers,
            ids,
            tool,
            worker,
        }
    }
    fn service(&self) -> TaskService {
        TaskService::new(
            self.repos.clone(),
            self.providers.clone(),
            Arc::new(FakeClock::new()),
            self.ids.clone(),
        )
    }
    async fn start(&self, service: &TaskService) -> TaskId {
        let project = service
            .create_project("Synthetic project", vec!["/synthetic".into()])
            .await
            .unwrap();
        let conversation = service
            .create_conversation(Some(project.project_id), "Test")
            .await
            .unwrap();
        let task_id = service
            .start_task(StartTaskRequest {
                conversation_id: Some(conversation.conversation_id),
                goal: "Two steps".into(),
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.tool.entered.notified())
            .await
            .unwrap();
        task_id
    }
}

async fn stopped(service: &TaskService, task: &TaskId) -> TaskState {
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
    .unwrap()
}

#[tokio::test]
async fn explicit_resume_after_reopen_skips_completed_steps_and_retries_only_the_safe_pending_step()
{
    let harness = Harness::new(false);
    let service = harness.service();
    let task = harness.start(&service).await;
    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    assert_eq!(checkpoint.task_state, TaskState::Interrupted);
    assert_eq!(checkpoint.completed_step_ids, ["first"]);
    assert_eq!(
        checkpoint.pending_action.as_ref().unwrap().replay_safety,
        ReplaySafety::Safe
    );
    assert!(checkpoint.resume.eligible);
    drop(service);
    let reopened = harness.service();
    assert_eq!(reopened.recover().await.unwrap(), 0);
    assert_eq!(
        reopened.latest_checkpoint(&task).await.unwrap(),
        Some(checkpoint.clone())
    );
    assert_eq!(
        harness.worker.runs.load(Ordering::SeqCst),
        1,
        "recovery never starts a worker"
    );
    reopened
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    assert_eq!(stopped(&reopened, &task).await, TaskState::Completed);
    assert!(harness.worker.skipped.load(Ordering::SeqCst));
    assert_eq!(harness.tool.completed_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.tool.pending_calls.load(Ordering::SeqCst), 2);
    let events = reopened.replay_task_events(&task, 0).await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=events.len() as u64).collect::<Vec<_>>()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload, EventPayload::ToolCompleted { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn unsafe_pending_work_is_inspectable_but_cannot_be_resumed_and_discard_keeps_history() {
    let harness = Harness::new(true);
    let service = harness.service();
    let task = harness.start(&service).await;
    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    assert_eq!(
        checkpoint.pending_action.as_ref().unwrap().replay_safety,
        ReplaySafety::Unsafe
    );
    assert!(!checkpoint.resume.eligible);
    assert_eq!(
        service
            .resume_task(&task, &checkpoint.checkpoint_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InterruptedUnsafe
    );
    service.cancel_task(&task).await.unwrap();
    assert_eq!(
        service.get_task(&task).await.unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(
        service.latest_checkpoint(&task).await.unwrap(),
        Some(checkpoint)
    );
    assert_eq!(harness.tool.pending_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.worker.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changed_arguments_cannot_reuse_a_completed_step_identity() {
    let harness = Harness::new(false);
    let service = harness.service();
    let task = harness.start(&service).await;
    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    harness
        .worker
        .changed_arguments
        .store(true, Ordering::SeqCst);
    service
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    // Wait for the new attempt, since its initial state is also Interrupted.
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.worker.runs.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stopped(&service, &task).await, TaskState::Interrupted);
    assert_eq!(harness.tool.completed_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.tool.pending_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_resume_requests_cannot_launch_two_workers() {
    let harness = Harness::new(false);
    let service = harness.service();
    let task = harness.start(&service).await;
    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    let (first, second) = tokio::join!(
        service.resume_task(&task, &checkpoint.checkpoint_id),
        service.resume_task(&task, &checkpoint.checkpoint_id)
    );
    assert!(first.is_ok() ^ second.is_ok());
    assert_eq!(stopped(&service, &task).await, TaskState::Completed);
    assert_eq!(harness.tool.completed_calls.load(Ordering::SeqCst), 1);
}

fn envelope(
    payload: EventPayload,
    kind: TaskEventKind,
    task: &TaskId,
    sequence: u64,
) -> EventEnvelope {
    EventDraft::user(kind, payload).into_envelope(
        EventId::new(format!("event-{}-{sequence}", task)),
        task.clone(),
        None,
        sequence,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
        CorrelationId::new("recovery-test"),
    )
}

#[tokio::test]
async fn startup_recovers_every_nonterminal_state_once_without_executing_any_worker() {
    let harness = Harness::new(false);
    let service = harness.service();
    for (index, state) in [
        TaskState::Queued,
        TaskState::Preparing,
        TaskState::Routing,
        TaskState::StartingWorker,
        TaskState::Running,
        TaskState::AwaitingApproval,
        TaskState::Checkpointing,
        TaskState::Cancelling,
        TaskState::Interrupted,
    ]
    .into_iter()
    .enumerate()
    {
        let task = TaskId::new(format!("crashed-{index}"));
        let record = TaskRecord::new(
            task.clone(),
            None,
            None,
            "Synthetic crashed task",
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let events = vec![
            envelope(
                EventPayload::TaskCreated {
                    goal_summary: "Synthetic crashed task".into(),
                    conversation_id: None,
                },
                TaskEventKind::TaskCreated,
                &task,
                1,
            ),
            envelope(
                EventPayload::TaskStateChanged {
                    previous: TaskState::Queued,
                    current: state,
                    reason_code: "test.seed".into(),
                },
                TaskEventKind::TaskStateChanged,
                &task,
                2,
            ),
        ];
        harness
            .repos
            .tasks
            .create_with_initial_events(&record, &events)
            .await
            .unwrap();
    }
    service.recover().await.unwrap();
    for record in service.list_tasks(None).await.unwrap() {
        assert_eq!(record.state, TaskState::Interrupted);
        assert!(
            service
                .latest_checkpoint(&record.task_id)
                .await
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        service.recover().await.unwrap(),
        0,
        "recovery is idempotent"
    );
    assert_eq!(harness.worker.runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_profile_reader_never_recovers_or_launches_work() {
    let harness = Harness::new(false);
    let service = harness.service();
    let task = harness.start(&service).await;
    let reader = harness.service().with_execution_guard(None);
    assert_eq!(reader.recover().await.unwrap(), 0);
    assert_eq!(
        reader.get_task(&task).await.unwrap().state,
        TaskState::Running
    );
    assert_eq!(
        reader.create_checkpoint(&task).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        reader.cancel_task(&task).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        reader
            .start_task(StartTaskRequest {
                conversation_id: None,
                goal: "blocked".into()
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    service.create_checkpoint(&task).await.unwrap();
}

#[test]
fn missing_safety_in_older_started_events_is_unknown_and_blocks_replay() {
    let task = TaskId::new("old");
    let requested = envelope(
        EventPayload::ToolRequested {
            invocation_id: InvocationId::new("call"),
            tool_id: "fake.tool.echo".into(),
            arguments_summary: "omitted".into(),
            risk_class: RiskClass::Read,
            step_id: Some("step".into()),
            request_hash: Some("hash".into()),
        },
        TaskEventKind::ToolRequested,
        &task,
        1,
    );
    let started: EventPayload = serde_json::from_value(
        serde_json::json!({"kind":"toolStarted","invocationId":"call","toolId":"fake.tool.echo"}),
    )
    .unwrap();
    let events = [
        requested,
        envelope(started, TaskEventKind::ToolStarted, &task, 2),
    ];
    let checkpoint = derive_checkpoint(
        CheckpointId::new("checkpoint"),
        task,
        &events,
        None,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
    );
    assert_eq!(
        checkpoint.pending_action.unwrap().replay_safety,
        ReplaySafety::Unknown
    );
    assert!(!checkpoint.resume.eligible);
}

#[tokio::test]
async fn a_resumed_run_prepares_context_again_and_routes_against_it() {
    // Found by reading an exported record rather than by a failing test: a
    // resumed run journaled `interrupted -> preparing` in the resume command,
    // so the driver skipped its Preparing iteration — and skipped assembling
    // context along with it. The routing decision that followed named
    // `context.unavailable`, a manifest nobody had written.
    //
    // The property worth holding is not "an event exists" but "every routing
    // decision names a context that was actually assembled", which is what
    // makes a disclosure traceable to what was really sent.
    let harness = Harness::new(false);
    let service = harness.service();
    let task = harness.start(&service).await;
    let checkpoint = service.create_checkpoint(&task).await.unwrap();

    service
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();
    stopped(&service, &task).await;

    let events = service.replay_task_events(&task, 0).await.unwrap();
    let assembled: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ContextPrepared { manifest } => {
                Some(manifest.context_manifest_id.clone())
            }
            _ => None,
        })
        .collect();
    let routed: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::RoutingDecided { decision } => Some(decision.context_manifest_id.clone()),
            _ => None,
        })
        .collect();

    assert!(
        assembled.len() >= 2,
        "a resumed run must assemble context again, not reuse a stale manifest"
    );
    assert_eq!(
        routed.len(),
        assembled.len(),
        "every preparation should be followed by a routing decision"
    );
    for context in &routed {
        assert!(
            assembled.contains(context),
            "a routing decision named {context}, which was never assembled"
        );
    }
}
