//! M1 walking-skeleton integration tests.
//!
//! These drive the real application service through the real ports, with fake
//! adapters underneath, and assert the properties `BUILD_ORDER.md` §8 requires:
//! ordered events, exactly one terminal state, cancellation at every stage, and
//! subscription that survives a reconnect.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;
use std::time::Duration;

use tramontana_core::config::{ConfigurationSelection, RawSource};
use tramontana_core::contracts::{
    ConfigurationKind, ConversationId, EventPayload, MessagePart, MessageRole, TaskEventKind,
    TaskId, TaskState, TaskUpdate,
};
use tramontana_core::ports::{ApprovalStore, Approver, EventStore, ToolProvider};
use tramontana_core::services::{Providers, Repositories, StartTaskRequest, TaskService};
use tramontana_core::testing::{
    FakeClock, FakeModelProvider, FakeToolProvider, FakeWorker, InMemoryApprovalStore,
    InMemoryConfigurationRepository, InMemoryConversationRepository, InMemoryEventStore,
    InMemoryProjectRepository, InMemoryTaskRepository, ScriptedApprover, SequentialIdSource,
    assemble_tool_plane,
};

/// Assemble a service over an event store the test can also inspect.
fn service_with_store() -> (Arc<TaskService>, Arc<InMemoryEventStore>) {
    let (registry, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new()) as Arc<dyn ToolProvider>],
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::unavailable()) as Arc<dyn Approver>,
        Arc::new(SequentialIdSource::new()),
    )
    .expect("the fake tool registers");

    let store = Arc::new(InMemoryEventStore::new());
    let events = Arc::clone(&store) as Arc<dyn EventStore>;
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(Arc::clone(&events))),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(tramontana_core::testing::InMemoryCheckpointRepository::new()),
        },
        Providers {
            models: vec![Arc::new(FakeModelProvider::streaming([
                "Hello", ", ", "world",
            ]))],
            tools: registry,
            permissions,
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        Arc::new(SequentialIdSource::new()),
    );
    (Arc::new(service), store)
}

fn service() -> Arc<TaskService> {
    service_with_store().0
}

/// Create a project and a conversation inside it.
///
/// Tools are only permitted inside an opened project
/// (`SECURITY_AND_PERMISSIONS.md` §3.1), so a task that means to use one needs
/// a conversation that belongs to a project. A projectless task reaching for a
/// tool is a separate case, asserted on its own below.
async fn conversation_in_a_project(service: &TaskService) -> ConversationId {
    let project = service
        .create_project("Demo", vec!["/work/demo".to_owned()])
        .await
        .expect("project");
    service
        .create_conversation(Some(project.project_id), "First")
        .await
        .expect("conversation")
        .conversation_id
}

#[tokio::test]
async fn opening_the_same_roots_reuses_the_project_record() {
    let service = service();
    let roots = vec!["/projects/demo".to_owned()];
    let first = service.open_project("Demo", roots.clone()).await.unwrap();
    let reopened = service
        .open_project("Ignored replacement name", roots)
        .await
        .unwrap();

    assert_eq!(reopened, first);
    assert_eq!(service.list_projects().await.unwrap(), vec![first]);
}

/// Poll until the task reaches a terminal state, or fail.
///
/// Polls rather than sleeping a fixed duration: the fakes are deterministic but
/// still asynchronous, and a fixed sleep is exactly the flake
/// `TESTING_STRATEGY.md` §11 forbids.
async fn await_terminal(service: &TaskService, task_id: &TaskId) -> TaskState {
    for _ in 0..1_000 {
        let snapshot = service.get_task(task_id).await.unwrap();
        if snapshot.state.is_terminal() {
            return snapshot.state;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("task {task_id} never reached a terminal state");
}

#[tokio::test]
async fn a_project_task_captures_the_latest_configuration_manifest() {
    let service = service();
    let project = service
        .create_project("Demo", vec!["/projects/demo".to_owned()])
        .await
        .unwrap();
    let configuration = service
        .resolve_configuration(
            &project.project_id,
            vec![RawSource {
                relative_path: "AGENTS.md".to_owned(),
                kind: ConfigurationKind::RootAgents,
                content: b"# Project instructions".to_vec(),
            }],
            &ConfigurationSelection::default(),
        )
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id.clone()), "Configured")
        .await
        .unwrap();
    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "use the project configuration".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();

    let task = service
        .list_tasks(None)
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.task_id == task_id)
        .unwrap();
    assert_eq!(
        task.configuration_manifest_hash.as_deref(),
        Some(configuration.manifest_hash.as_str())
    );

    let (events, _) = service.subscribe_task_events(&task_id, 0).await.unwrap();
    assert!(matches!(
        &events[1].payload,
        EventPayload::ConfigurationResolved { configuration: stored }
            if stored.manifest_hash == configuration.manifest_hash
    ));
}

#[tokio::test]
async fn a_task_runs_to_completion_with_an_ordered_event_stream() {
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        await_terminal(&service, &task_id).await,
        TaskState::Completed
    );

    let events = store.replay(&task_id, 0).await.unwrap();

    // Sequences start at 1 and increase by exactly one, with no gaps.
    let sequences: Vec<u64> = events.iter().map(|event| event.sequence).collect();
    assert_eq!(sequences, (1..=events.len() as u64).collect::<Vec<_>>());

    // The state machine walked the contract's path, in order.
    let states: Vec<TaskState> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::TaskStateChanged { current, .. } => Some(*current),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![
            TaskState::Preparing,
            TaskState::Routing,
            TaskState::StartingWorker,
            TaskState::Running,
            TaskState::Completed,
        ]
    );

    let kinds: Vec<TaskEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(kinds.first(), Some(&TaskEventKind::TaskCreated));
    assert_eq!(kinds.last(), Some(&TaskEventKind::TaskCompleted));
    assert!(kinds.contains(&TaskEventKind::WorkerSelected));
    assert!(kinds.contains(&TaskEventKind::ToolRequested));
    assert!(kinds.contains(&TaskEventKind::ToolCompleted));

    // The lifecycle events are not the only durable observation. The broker
    // also commits the exact validated call and a bounded result, so replay or
    // export does not need to reconstruct a worker-private transcript.
    let (invocation_id, tool_id, arguments) = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::MessageCommitted { message }
                if message.role == MessageRole::Assistant =>
            {
                message.parts.iter().find_map(|part| match part {
                    MessagePart::ToolCall {
                        invocation_id,
                        tool_id,
                        arguments,
                    } => Some((invocation_id.clone(), tool_id.clone(), arguments.clone())),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("the broker commits the validated tool call");
    assert_eq!(tool_id, "fake.tool.echo");
    assert_eq!(arguments["value"], "Hello, world");
    assert!(events.iter().any(|event| match &event.payload {
        EventPayload::MessageCommitted { message } if message.role == MessageRole::Tool => {
            message.parts.iter().any(|part| {
                matches!(
                    part,
                    MessagePart::ToolResult {
                        invocation_id: result_invocation,
                        content,
                        truncated: false,
                    } if result_invocation == &invocation_id
                        && content["status"] == "completed"
                        && content["output"]["echoed"] == "Hello, world"
                )
            })
        }
        _ => false,
    }));

    let task = service.get_task_record(&task_id).await.unwrap();
    assert_eq!(task.last_sequence, events.last().unwrap().sequence);
    assert_eq!(
        service.recover().await.unwrap(),
        0,
        "a cleanly completed task must not need restart repair"
    );
}

#[tokio::test]
async fn a_task_reaches_exactly_one_terminal_state() {
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    await_terminal(&service, &task_id).await;

    let events = store.replay(&task_id, 0).await.unwrap();
    let terminal_count = events
        .iter()
        .filter(|event| match &event.payload {
            EventPayload::TaskStateChanged { current, .. } => current.is_terminal(),
            _ => false,
        })
        .count();
    assert_eq!(terminal_count, 1, "a task must end exactly once");
}

#[tokio::test]
async fn cancelling_before_the_work_starts_still_converges_to_cancelled() {
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    // Racing the driver deliberately: the cancel may land before it has moved
    // past Queued. Either way the task must end cancelled, never completed.
    service.cancel_task(&task_id).await.unwrap();

    assert_eq!(
        await_terminal(&service, &task_id).await,
        TaskState::Cancelled
    );

    let events = store.replay(&task_id, 0).await.unwrap();
    let states: Vec<TaskState> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::TaskStateChanged { current, .. } => Some(*current),
            _ => None,
        })
        .collect();
    // Cancelling is recorded, not skipped: it is the durable evidence that the
    // user asked, as opposed to the task merely ending.
    assert!(states.contains(&TaskState::Cancelling));
    assert_eq!(states.last(), Some(&TaskState::Cancelled));
}

#[tokio::test]
async fn cancelling_after_completion_does_not_move_a_terminal_task() {
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        await_terminal(&service, &task_id).await,
        TaskState::Completed
    );
    let sequence_before = store.last_sequence(&task_id).await.unwrap();

    // A late cancel is accepted rather than erroring — the caller could not
    // have known — but it must not append anything or change the outcome.
    service.cancel_task(&task_id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(
        service.get_task(&task_id).await.unwrap().state,
        TaskState::Completed
    );
    assert_eq!(
        store.last_sequence(&task_id).await.unwrap(),
        sequence_before
    );
}

#[tokio::test]
async fn a_subscriber_replays_missed_events_and_then_receives_live_ones() {
    let service = service();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    // Subscribe from the very beginning, as a freshly opened window would.
    let (replayed, mut live) = service.subscribe_task_events(&task_id, 0).await.unwrap();
    assert!(
        !replayed.is_empty(),
        "the creation event is durable before start_task returns"
    );

    await_terminal(&service, &task_id).await;

    let mut seen: Vec<u64> = replayed.iter().map(|event| event.sequence).collect();
    while let Ok(update) = live.try_recv() {
        if let TaskUpdate::Event { event } = update {
            seen.push(event.sequence);
        }
    }
    seen.sort_unstable();
    seen.dedup();

    // Every durable sequence is accounted for exactly once across replay plus
    // live delivery: reconnecting loses nothing.
    assert_eq!(seen, (1..=*seen.last().unwrap()).collect::<Vec<_>>());
}

#[tokio::test]
async fn resubscribing_after_a_disconnect_recovers_the_gap() {
    let service = service();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    await_terminal(&service, &task_id).await;

    // Simulates a window reload mid-task: the old receiver is gone, and the
    // interface reconnects with the last sequence it rendered.
    let (from_scratch, _) = service.subscribe_task_events(&task_id, 0).await.unwrap();
    let resume_point = from_scratch[2].sequence;
    let (after_gap, _) = service
        .subscribe_task_events(&task_id, resume_point)
        .await
        .unwrap();

    assert_eq!(after_gap.len(), from_scratch.len() - 3);
    assert_eq!(after_gap.first().unwrap().sequence, resume_point + 1);
}

#[tokio::test]
async fn streamed_frames_reconstruct_the_answer_exactly_once() {
    // Frames are what the interface concatenates to show text arriving. If any
    // layer republishes content the broker already streamed, the reader sees
    // the answer twice — which is a correctness bug in the transport, not a
    // cosmetic one, because the interface cannot tell the difference.
    let service = service();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let (_, mut updates) = service.subscribe_task_events(&task_id, 0).await.unwrap();

    await_terminal(&service, &task_id).await;

    let mut streamed = String::new();
    while let Ok(update) = updates.try_recv() {
        if let TaskUpdate::Frame { frame } = update {
            streamed.push_str(&frame.text);
        }
    }

    // The fake model is scripted with exactly these chunks.
    assert_eq!(streamed, "Hello, world");
}

#[tokio::test]
async fn a_projectless_task_cannot_use_a_tool_at_all() {
    // Discovered by wiring real tools: with no project, a task has no approved
    // roots, so a read is not "a read inside an already-opened project" and
    // the safe default does not apply. With no approver configured, it is
    // refused. That is `SECURITY_AND_PERMISSIONS.md` §3.1 working, not a
    // regression — projectless tasks have no implicit access to anything.
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let state = await_terminal(&service, &task_id).await;
    assert_eq!(state, TaskState::Failed);

    let events = store.replay(&task_id, 0).await.unwrap();
    let decision = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::PermissionDecided {
                allowed,
                reason_code,
                ..
            } => Some((*allowed, reason_code.clone())),
            _ => None,
        })
        .expect("a decision is recorded even when the answer is no");

    assert!(!decision.0, "a projectless tool call must be refused");
    assert_eq!(decision.1, "policy.no_approver");
}

#[tokio::test]
async fn every_tool_call_records_an_origin_scope_and_decision() {
    // The M3 exit criterion. A permitted action that leaves no decision event
    // satisfies the permission system and fails the audit requirement.
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation_in_a_project(&service).await),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    await_terminal(&service, &task_id).await;

    let events = store.replay(&task_id, 0).await.unwrap();
    let decision = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::PermissionDecided {
                origin,
                capability,
                scope,
                allowed,
                reason_code,
                ..
            } => Some((
                origin.clone(),
                capability.clone(),
                scope.clone(),
                *allowed,
                reason_code.clone(),
            )),
            _ => None,
        })
        .expect("every tool call records a decision");

    let (origin, capability, scope, allowed, reason_code) = decision;
    assert!(matches!(
        origin,
        tramontana_core::contracts::PermissionOrigin::Worker { .. }
    ));
    assert_eq!(capability, "fake.echo");
    assert_eq!(scope.roots, vec!["/work/demo".to_owned()]);
    assert!(allowed);
    assert_eq!(reason_code, "policy.safe_default");

    // And the full lifecycle is in the stream, in order.
    let kinds: Vec<TaskEventKind> = events.iter().map(|event| event.kind).collect();
    for expected in [
        TaskEventKind::ToolRequested,
        TaskEventKind::ApprovalResolved,
        TaskEventKind::ToolStarted,
        TaskEventKind::ToolCompleted,
    ] {
        assert!(
            kinds.contains(&expected),
            "missing {expected:?} in {kinds:?}"
        );
    }
}

#[tokio::test]
async fn a_projectless_task_records_no_conversation() {
    let (service, store) = service_with_store();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    await_terminal(&service, &task_id).await;

    let events = store.replay(&task_id, 0).await.unwrap();
    assert!(
        events.iter().all(|event| event.conversation_id.is_none()),
        "a projectless task must not invent a conversation"
    );
}

#[tokio::test]
async fn a_task_can_be_attached_to_a_conversation_in_a_project() {
    let (service, store) = service_with_store();

    let project = service.create_project("Demo", Vec::new()).await.unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id.clone()), "First")
        .await
        .unwrap();

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id.clone()),
            goal: "say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    await_terminal(&service, &task_id).await;

    let events = store.replay(&task_id, 0).await.unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.conversation_id.as_ref() == Some(&conversation.conversation_id))
    );
}

#[tokio::test]
async fn unknown_projects_conversations_and_tasks_are_reported_not_invented() {
    use tramontana_core::contracts::{ErrorCode, ProjectId};

    let service = service();

    let error = service
        .create_conversation(Some(ProjectId::new("nope")), "x")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    let error = service
        .start_task(StartTaskRequest {
            conversation_id: Some(ConversationId::new("nope")),
            goal: "x".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    let error = service.get_task(&TaskId::new("nope")).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    let error = service.cancel_task(&TaskId::new("nope")).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}
