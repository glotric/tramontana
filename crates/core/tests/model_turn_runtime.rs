//! A model protocol failure must not fall through into the fake worker's tool.
#![allow(clippy::unwrap_used, clippy::panic, reason = "assertions in tests")]

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

#[derive(Debug, Clone, Copy)]
enum Scenario {
    ToolCall,
    TooLarge,
    WrongOffset,
    Eof,
    Length,
    Trailing,
    Silent,
    OpeningSilent,
}

#[derive(Debug)]
struct ScriptedModel(Scenario);

#[async_trait]
impl ModelProvider for ScriptedModel {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }
    async fn list_models(&self) -> Vec<ModelDescriptor> {
        vec![FakeModelProvider::descriptor()]
    }
    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        if matches!(self.0, Scenario::OpeningSilent) {
            std::future::pending::<()>().await;
        }
        let (sender, receiver) = mpsc::channel(4);
        let stream_id = request.request().request_id.clone();
        let scenario = self.0;
        tokio::spawn(async move {
            let _ = sender
                .send(ModelEvent::TextDelta {
                    stream_id: stream_id.clone(),
                    offset: 0,
                    text: "Partial".into(),
                })
                .await;
            let event = match scenario {
                Scenario::ToolCall => ModelEvent::ToolCallDelta {
                    stream_id: stream_id.clone(),
                    offset: 1,
                    call_index: 0,
                    call_id: Some("call".into()),
                    name: Some("builtin.fs.list".into()),
                    delta: "{}".into(),
                },
                Scenario::TooLarge => ModelEvent::TextDelta {
                    stream_id: stream_id.clone(),
                    offset: 1,
                    text: "x".repeat(1_048_576),
                },
                Scenario::WrongOffset => ModelEvent::TextDelta {
                    stream_id: stream_id.clone(),
                    offset: 5,
                    text: "invalid".into(),
                },
                Scenario::Eof => return,
                Scenario::Length => ModelEvent::Completed {
                    stop_reason: StopReason::Length,
                },
                Scenario::Trailing => ModelEvent::Completed {
                    stop_reason: StopReason::Stop,
                },
                Scenario::Silent | Scenario::OpeningSilent => {
                    cancel.cancelled().await;
                    return;
                }
            };
            let _ = sender.send(event).await;
            if matches!(scenario, Scenario::Length) {
                return;
            }
            let _ = sender
                .send(ModelEvent::Completed {
                    stop_reason: if matches!(scenario, Scenario::ToolCall) {
                        StopReason::ToolCall
                    } else {
                        StopReason::Stop
                    },
                })
                .await;
        });
        Ok(receiver)
    }
}

fn service(scenario: Scenario) -> (TaskService, Arc<InMemoryEventStore>) {
    let events = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::unavailable()),
        ids.clone(),
    )
    .unwrap();
    (
        TaskService::new(
            Repositories {
                projects: Arc::new(InMemoryProjectRepository::new()),
                conversations: Arc::new(InMemoryConversationRepository::new()),
                tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
                events: events.clone(),
                configuration: Arc::new(InMemoryConfigurationRepository::new()),
                checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
            },
            Providers {
                models: vec![Arc::new(ScriptedModel(scenario))],
                tools,
                permissions,
                releases,
                worker: Arc::new(FakeWorker::new()),
            },
            Arc::new(FakeClock::new()),
            ids,
        ),
        events,
    )
}

#[tokio::test(start_paused = true)]
async fn invalid_or_unsupported_turns_fail_without_a_tool_invocation() {
    for (scenario, code) in [
        (Scenario::ToolCall, ErrorCode::CapabilityUnsupported),
        (Scenario::TooLarge, ErrorCode::LimitExceeded),
        (Scenario::WrongOffset, ErrorCode::ProtocolError),
        (Scenario::Eof, ErrorCode::ProtocolError),
        (Scenario::Length, ErrorCode::LimitExceeded),
        (Scenario::Trailing, ErrorCode::ProtocolError),
        (Scenario::Silent, ErrorCode::Timeout),
        (Scenario::OpeningSilent, ErrorCode::Timeout),
    ] {
        let (service, events) = service(scenario);
        let project = service
            .create_project("Fixture", vec!["/fixture".into()])
            .await
            .unwrap();
        let conversation = service
            .create_conversation(Some(project.project_id), "Test")
            .await
            .unwrap();
        let task_id = service
            .start_task(StartTaskRequest {
                conversation_id: Some(conversation.conversation_id),
                goal: "hello".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(130), async {
            loop {
                let snapshot = service.get_task(&task_id).await.unwrap();
                if snapshot.state.is_terminal() {
                    break snapshot;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(snapshot.state, TaskState::Failed, "{scenario:?}");
        let records = events.replay(&task_id, 0).await.unwrap();
        assert!(
            !records
                .iter()
                .any(|record| record.kind == TaskEventKind::ToolRequested),
            "{scenario:?}"
        );
        assert!(records.iter().any(|record| matches!(&record.payload,EventPayload::TaskFailed {error} if error.code==code)),"{scenario:?}: {records:?}");
        if !matches!(scenario, Scenario::OpeningSilent) {
            assert!(
                records.iter().any(
                    |record| matches!(&record.payload,EventPayload::MessageCommitted {message}
                if message.role==MessageRole::Assistant && message.status==MessageStatus::Partial
                && message.parts==vec![MessagePart::Text {text:"Partial".into()}])
                ),
                "{scenario:?}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn user_cancellation_during_a_model_call_is_cancelled_not_failed() {
    for scenario in [Scenario::Silent, Scenario::OpeningSilent] {
        let (service, events) = service(scenario);
        let task_id = service
            .start_task(StartTaskRequest {
                conversation_id: None,
                goal: "hello".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if events
                    .replay(&task_id, 0)
                    .await
                    .unwrap()
                    .iter()
                    .any(|event| event.kind == TaskEventKind::ModelStarted)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        service.cancel_task(&task_id).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let state = service.get_task(&task_id).await.unwrap().state;
                if state.is_terminal() {
                    assert_eq!(state, TaskState::Cancelled);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let records = events.replay(&task_id, 0).await.unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|event| event.kind == TaskEventKind::TaskCancelled)
                .count(),
            1
        );
        assert!(!records.iter().any(|event| matches!(
            event.kind,
            TaskEventKind::TaskFailed | TaskEventKind::ToolRequested
        )));
    }
}
