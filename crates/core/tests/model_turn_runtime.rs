//! A model protocol failure must not fall through into the fake worker's tool.
#![allow(clippy::unwrap_used, clippy::panic, reason = "assertions in tests")]

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
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
    /// A stream that opens and then says nothing before the cancel.
    ///
    /// The counterpart to `OpeningSilent`: the cancel lands on the other side
    /// of the provider's `stream`, and the two must leave the same record.
    SilentEmpty,
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
            if !matches!(scenario, Scenario::SilentEmpty) {
                let _ = sender
                    .send(ModelEvent::TextDelta {
                        stream_id: stream_id.clone(),
                        offset: 0,
                        text: "Partial".into(),
                    })
                    .await;
            }
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
                Scenario::Silent | Scenario::OpeningSilent | Scenario::SilentEmpty => {
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
    (
        test_service(
            events.clone(),
            vec![Arc::new(ScriptedModel(scenario))],
            vec![Arc::new(FakeToolProvider::new())],
            Arc::new(ScriptedApprover::unavailable()),
            Arc::new(FakeWorker::new()),
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
            .start_task(start_request(&conversation.conversation_id, "hello"))
            .await
            .unwrap();
        let state = wait_state(
            &service,
            &task_id,
            Duration::from_secs(130),
            Duration::from_millis(10),
            TaskState::is_terminal,
        )
        .await;
        assert_eq!(state, TaskState::Failed, "{scenario:?}");
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
    for scenario in [
        Scenario::Silent,
        Scenario::OpeningSilent,
        Scenario::SilentEmpty,
    ] {
        let (service, events) = service(scenario);
        let task_id = service
            .start_task(StartTaskRequest {
                conversation_id: None,
                goal: "hello".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        wait_events(
            &service,
            &task_id,
            Duration::from_secs(1),
            Duration::from_millis(1),
            |events| {
                events
                    .iter()
                    .any(|event| event.kind == TaskEventKind::ModelStarted)
            },
        )
        .await;
        service.cancel_task(&task_id).await.unwrap();
        let state = wait_state(
            &service,
            &task_id,
            Duration::from_secs(1),
            Duration::from_millis(1),
            TaskState::is_terminal,
        )
        .await;
        assert_eq!(state, TaskState::Cancelled);
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

        // What the model had said when the cancel landed is kept; a turn
        // stopped before it said anything leaves no message claiming it
        // spoke, because there is nothing to coalesce.
        let partials: Vec<String> = records
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::MessageCommitted { message }
                    if message.status == MessageStatus::Partial =>
                {
                    Some(
                        message
                            .parts
                            .iter()
                            .map(|part| match part {
                                MessagePart::Text { text } => text.clone(),
                                _ => String::new(),
                            })
                            .collect::<String>(),
                    )
                }
                _ => None,
            })
            .collect();
        match scenario {
            Scenario::Silent => assert_eq!(partials, ["Partial"], "{scenario:?}"),
            _ => assert!(partials.is_empty(), "{scenario:?}: {partials:?}"),
        }
    }
}
