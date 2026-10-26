//! A write's approval names the files it would touch, never their content.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

/// A write tool, as the filesystem adapter declares one.
#[derive(Debug)]
struct Writes;

#[async_trait]
impl ToolProvider for Writes {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: "fake.fs.move".to_owned(),
            provider_id: "fake".to_owned(),
            display_name: "Move".to_owned(),
            description: "fixture".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            risk_class: RiskClass::Modify,
            side_effect: SideEffect::LocalReversible,
            capability: "fs.write".to_owned(),
            supports_cancellation: true,
            replay_safety: ReplaySafety::Safe,
        }]
    }

    async fn invoke(
        &self,
        _request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        panic!("a denied write ran");
    }
}

/// Asks for one move, carrying file content in its arguments.
#[derive(Debug)]
struct Mover;

#[async_trait]
impl AgentProvider for Mover {
    fn describe(&self) -> AgentDescriptor {
        FakeWorker::new().describe()
    }

    async fn run(
        &self,
        _request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        let _ = host
            .call_tool_step(
                "move",
                "fake.fs.move",
                serde_json::json!({
                    "from": "draft.txt",
                    "to": "notes/final.txt",
                    "content": "the file's secret body",
                }),
                &cancel,
            )
            .await;
        Ok(WorkerOutcome::Completed {
            summary: "asked".to_owned(),
        })
    }
}

/// Keeps every question it is asked, and refuses them all.
#[derive(Debug, Default)]
struct Recording(Mutex<Vec<ApprovalRequest>>);

#[async_trait]
impl Approver for Recording {
    async fn ask(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        self.0.lock().unwrap().push(request.clone());
        Ok(ApprovalDecisionKind::Deny)
    }
}

#[tokio::test]
async fn a_writes_approval_names_the_files_and_not_their_content() {
    let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let asked = Arc::new(Recording::default());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(Writes)],
        Arc::new(InMemoryApprovalStore::new()),
        asked.clone(),
        ids.clone(),
    )
    .unwrap();
    let service = TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: vec![Arc::new(FakeModelProvider::streaming(["answer"]))],
            tools,
            permissions,
            releases,
            worker: Arc::new(Mover),
        },
        Arc::new(FakeClock::new()),
        ids,
    );
    let project = service
        .create_project("Synthetic project", vec!["/synthetic".into()])
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Test")
        .await
        .unwrap();
    let task = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Move the draft".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !service.get_task(&task).await.unwrap().state.is_terminal() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();

    let asked = asked.0.lock().unwrap();
    assert_eq!(asked.len(), 1, "the write was not put to anyone");
    let details: Vec<(&str, &str)> = asked[0]
        .details
        .iter()
        .map(|detail| (detail.label.as_str(), detail.value.as_str()))
        .collect();
    assert!(details.contains(&("From", "draft.txt")), "{details:?}");
    assert!(details.contains(&("To", "notes/final.txt")), "{details:?}");
    assert!(
        !details
            .iter()
            .any(|(_, value)| value.contains("secret body")),
        "file content reached the prompt: {details:?}"
    );
}
