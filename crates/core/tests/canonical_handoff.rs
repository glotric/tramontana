//! Handing an interrupted task to a different worker, from canonical state.
//!
//! This is validation gate V2's fifth step made executable: *"configure a
//! second fake worker with different capabilities, hand the task to it from
//! canonical state, and confirm it receives a coherent goal, history, and
//! context without the first worker's private data"* (`BUILD_ORDER.md` §13).
//!
//! It is also where V2's first re-plan trigger would fire. If the canonical
//! record is not enough for a differently-configured worker to continue, the
//! handed-over task fails here — before M10 is built on the assumption that it
//! is enough.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

/// A tool whose second step blocks, so a task can be caught mid-flight.
#[derive(Debug, Default)]
struct BlockingTool {
    entered: Notify,
    calls: AtomicUsize,
}

#[async_trait]
impl ToolProvider for BlockingTool {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
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
        if request.arguments()["value"] == "blocking" {
            self.entered.notify_one();
            cancel.cancelled().await;
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "synthetic interruption",
                CorrelationId::new("test"),
            ));
        }
        Ok(ToolOutcome {
            content: serde_json::json!({ "echoed": "first worker's private-looking output" }),
            display_summary: "First step completed".into(),
            side_effect: SideEffect::None,
            truncated: false,
            changes: Vec::new(),
        })
    }
}

/// The worker that starts the task and is interrupted partway through.
#[derive(Debug)]
struct StartingWorker {
    checkpoint_support: CheckpointSupport,
    /// What it sends for the shared step. A receiving worker reconstructs this
    /// from the goal, so a mismatch here is a worker disagreeing with the
    /// record about what a step *was*.
    first_step_value: String,
}

#[async_trait]
impl AgentProvider for StartingWorker {
    fn describe(&self) -> AgentDescriptor {
        AgentDescriptor {
            agent_id: "fake.starting".into(),
            display_name: "Starting fake worker".into(),
            checkpoint_support: self.checkpoint_support,
            ..FakeWorker::new().describe()
        }
    }

    async fn run(
        &self,
        _request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        host.call_tool_step(
            "reference-tool",
            FAKE_TOOL_ID,
            serde_json::json!({ "value": self.first_step_value }),
            &cancel,
        )
        .await?;
        host.call_tool_step(
            "blocking-step",
            FAKE_TOOL_ID,
            serde_json::json!({ "value": "blocking" }),
            &cancel,
        )
        .await?;
        Ok(WorkerOutcome::Completed {
            summary: "the first worker finished".into(),
        })
    }
}

const GOAL: &str = "Explain the permission boundary";

struct Harness {
    repos: Repositories,
    providers: Providers,
    ids: Arc<SequentialIdSource>,
    tool: Arc<BlockingTool>,
}

impl Harness {
    fn new(support: CheckpointSupport, first_step_value: &str) -> Self {
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
        let tool = Arc::new(BlockingTool::default());
        let (tools, permissions, releases) = assemble_tool_plane(
            vec![tool.clone()],
            Arc::new(InMemoryApprovalStore::new()),
            Arc::new(ScriptedApprover::unavailable()),
            ids.clone(),
        )
        .unwrap();

        Self {
            repos,
            providers: Providers {
                models: vec![Arc::new(FakeModelProvider::streaming(["answer"]))],
                tools,
                permissions,
                releases,
                worker: Arc::new(StartingWorker {
                    checkpoint_support: support,
                    first_step_value: first_step_value.to_owned(),
                }),
            },
            ids,
            tool,
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

    /// Start a task and stop once its second step is blocking.
    async fn interrupted(&self, service: &TaskService) -> (TaskId, TaskCheckpoint) {
        let project = service
            .create_project("Synthetic project", vec!["/synthetic".into()])
            .await
            .unwrap();
        let conversation = service
            .create_conversation(Some(project.project_id), "Handoff")
            .await
            .unwrap();
        let task_id = service
            .start_task(StartTaskRequest {
                conversation_id: Some(conversation.conversation_id),
                goal: GOAL.into(),
            })
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), self.tool.entered.notified())
            .await
            .expect("the task never reached its blocking step");
        let checkpoint = service.create_checkpoint(&task_id).await.unwrap();
        (task_id, checkpoint)
    }
}

/// Wait for a task to stop moving.
async fn settled(service: &TaskService, task_id: &TaskId) -> TaskState {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = service.get_task(task_id).await.unwrap().state;
            if state.is_terminal() || state == TaskState::Interrupted {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task never settled")
}

#[tokio::test]
async fn a_second_worker_continues_from_canonical_state_without_repeating_a_completed_step() {
    let harness = Harness::new(CheckpointSupport::Portable, GOAL);
    let service = harness.service();
    let (task_id, checkpoint) = harness.interrupted(&service).await;

    assert_eq!(
        checkpoint.resume.mode,
        Some(ResumeMode::CanonicalHandoff),
        "a fully brokered worker's checkpoint should be portable"
    );
    assert_eq!(checkpoint.completed_step_ids, vec!["reference-tool"]);
    let calls_before = harness.tool.calls.load(Ordering::SeqCst);

    let receiving: Arc<dyn AgentProvider> = Arc::new(ReceivingWorker::new());
    let handoff = service
        .hand_off_task(&task_id, &checkpoint.checkpoint_id, receiving)
        .await
        .expect("the canonical record was not enough to hand the task over");

    assert_eq!(settled(&service, &task_id).await, TaskState::Completed);

    // The second worker did not run the step the first one finished. The
    // *core* refused it, so this holds even for a worker that ignored the
    // watermark it was given.
    let events = service.replay_task_events(&task_id, 0).await.unwrap();
    let skipped = events.iter().any(|event| {
        matches!(&event.payload, EventPayload::DiagnosticRecorded { code, .. }
            if code == "resume.stepSkipped")
    });
    assert!(skipped, "the completed step was not recorded as skipped");
    assert_eq!(
        harness.tool.calls.load(Ordering::SeqCst) - calls_before,
        0,
        "the receiving worker re-ran a completed step"
    );

    // It was handed a coherent goal and history.
    assert_eq!(handoff.goal, GOAL);
    assert!(!handoff.history.is_empty());
    assert_eq!(handoff.completed_step_ids, vec!["reference-tool"]);

    // And the record shows a different worker took over, with its own standing.
    let took_over = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::WorkerSelected { descriptor } => Some(descriptor.clone()),
            _ => None,
        })
        .next_back()
        .expect("no worker was recorded as taking over");
    assert_eq!(took_over.agent_id, RECEIVING_AGENT_ID);
    assert_eq!(took_over.tool_control, ToolControl::DirectConstrained);
}

#[tokio::test]
async fn the_receiving_worker_reads_back_what_it_inherited() {
    // "Confirm it receives a coherent goal, history, and context." The proof
    // is in the task's own record: the second worker writes down what it
    // understood, so a person reading the stream can judge whether the handoff
    // was actually intelligible rather than merely well-typed.
    let harness = Harness::new(CheckpointSupport::Portable, GOAL);
    let service = harness.service();
    let (task_id, checkpoint) = harness.interrupted(&service).await;

    service
        .hand_off_task(
            &task_id,
            &checkpoint.checkpoint_id,
            Arc::new(ReceivingWorker::new()),
        )
        .await
        .unwrap();
    settled(&service, &task_id).await;

    let events = service.replay_task_events(&task_id, 0).await.unwrap();
    let text = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::DiagnosticRecorded { code, summary } if code == HANDOFF_RECEIVED => {
                Some(summary.clone())
            }
            _ => None,
        })
        .expect("the receiving worker recorded nothing it understood");

    assert!(text.contains(GOAL), "the goal did not survive the handoff");
    assert!(
        text.contains("Starting fake worker"),
        "the receiving worker was not told who held the task"
    );
    assert!(
        text.contains("reference-tool"),
        "the receiving worker was not told what was already done"
    );
    assert!(
        text.contains("Was in flight"),
        "the receiving worker was not told what was interrupted"
    );
}

#[tokio::test]
async fn nothing_private_to_the_first_worker_reaches_the_second() {
    let harness = Harness::new(CheckpointSupport::Portable, GOAL);
    let service = harness.service();
    let (task_id, checkpoint) = harness.interrupted(&service).await;

    let handoff = service
        .hand_off_task(
            &task_id,
            &checkpoint.checkpoint_id,
            Arc::new(ReceivingWorker::new()),
        )
        .await
        .unwrap();

    let rendered = serde_json::to_string(&handoff).unwrap();
    // The first worker's tool returned content that a naive handoff might have
    // carried along. Only the recorded summary is canonical.
    assert!(
        !rendered.contains("private-looking output"),
        "raw output from the first worker's tool reached the second"
    );
    assert!(
        handoff
            .previous_worker
            .as_ref()
            .is_some_and(|worker| worker.agent_id == "fake.starting"),
        "the previous worker appears as provenance"
    );
    // Provenance is a descriptor, not a session: there is nowhere in the
    // bundle for opaque continuation data to travel.
    assert!(!rendered.contains("sessionStateRef"));
}

#[tokio::test]
async fn a_checkpoint_that_is_not_portable_cannot_be_handed_over() {
    // A worker that needs its own session state cannot be replaced by one
    // reading the canonical record, and the refusal says so rather than
    // handing the second worker something it cannot act on.
    let harness = Harness::new(CheckpointSupport::SameWorker, GOAL);
    let service = harness.service();
    let (task_id, checkpoint) = harness.interrupted(&service).await;

    assert_eq!(checkpoint.resume.mode, Some(ResumeMode::SameWorker));
    let error = service
        .hand_off_task(
            &task_id,
            &checkpoint.checkpoint_id,
            Arc::new(ReceivingWorker::new()),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("not portable"));
    assert_eq!(settled(&service, &task_id).await, TaskState::Interrupted);
}

#[tokio::test]
async fn handing_a_task_to_the_worker_that_already_had_it_is_refused() {
    let harness = Harness::new(CheckpointSupport::Portable, GOAL);
    let service = harness.service();
    let (task_id, checkpoint) = harness.interrupted(&service).await;

    let error = service
        .hand_off_task(
            &task_id,
            &checkpoint.checkpoint_id,
            Arc::new(StartingWorker {
                checkpoint_support: CheckpointSupport::Portable,
                first_step_value: GOAL.to_owned(),
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("resume it instead"));
}

#[tokio::test]
async fn a_receiving_worker_that_disagrees_with_the_record_is_refused_not_repeated() {
    // The receiving worker reconstructs the shared step from the goal. Here
    // the first worker ran it with something else, so the two disagree about
    // what that step *was* — and the core refuses rather than letting an
    // effect happen twice under one identity.
    let harness = Harness::new(CheckpointSupport::Portable, "something else entirely");
    let service = harness.service();
    let (task_id, checkpoint) = harness.interrupted(&service).await;
    let calls_before = harness.tool.calls.load(Ordering::SeqCst);

    service
        .hand_off_task(
            &task_id,
            &checkpoint.checkpoint_id,
            Arc::new(ReceivingWorker::new()),
        )
        .await
        .unwrap();

    // Interrupted, not failed: an effect nobody can account for is something a
    // person reconciles, and a failed task would invite starting over — which
    // is the one thing that could duplicate the effect.
    assert_eq!(settled(&service, &task_id).await, TaskState::Interrupted);
    assert_eq!(
        harness.tool.calls.load(Ordering::SeqCst) - calls_before,
        0,
        "a step whose identity was disputed was run anyway"
    );

    let events = service.replay_task_events(&task_id, 0).await.unwrap();
    let refusal = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::DiagnosticRecorded { code, summary } if code == "resume.refused" => {
                Some(summary.clone())
            }
            _ => None,
        })
        .expect("the refusal was not explained in the record");
    assert!(refusal.contains("different tool or arguments"));
}
