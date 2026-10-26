//! PDF and Word attachments through the application service, with the real
//! reader (ADR 0062).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good fixtures; a panic is the failure report"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use tramontana_adapters_documents::{Documents, PDF, WORD};
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

fn service() -> TaskService {
    test_service(
        Arc::new(InMemoryEventStore::new()),
        vec![Arc::new(FakeModelProvider::streaming(["Read it."]))],
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(ScriptedApprover::unavailable()),
        // No tools: a chat whose model is asked to answer, which is the case
        // under test.
        Arc::new(ReferenceWorker::new(Vec::new(), ReadOnlyLimits::default())),
    )
    .with_routing_mode(RoutingMode::LocalOnly)
    .with_document_reader(Arc::new(Documents::new()))
}

#[tokio::test]
async fn a_pdf_attached_to_a_chat_is_recorded_as_its_text() {
    let service = service();
    let chat = service.create_conversation(None, "Chat").await.unwrap();
    let task = service
        .start_task(StartTaskRequest {
            attachments: vec![AttachmentInput {
                name: "agreement.pdf".into(),
                bytes: common::pdf(&["Supplier agreement", "Payment within 30 days"]),
            }],
            ..start_request(&chat.conversation_id, "Summarize it.")
        })
        .await
        .unwrap();
    let events = wait_events(
        &service,
        &task,
        Duration::from_secs(20),
        Duration::from_millis(20),
        |events| {
            events.iter().any(|event| {
                matches!(
                    event.kind,
                    TaskEventKind::TaskCompleted | TaskEventKind::TaskFailed
                )
            })
        },
    )
    .await;
    let attached = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::TaskCreated { attachments, .. } => Some(attachments.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(attached[0].name, "agreement.pdf");
    assert_eq!(attached[0].media_type, PDF);
    assert_eq!(
        attached[0].text,
        "Supplier agreement\n\nPayment within 30 days"
    );
    assert!(is_document(&attached[0]));
}

#[tokio::test]
async fn a_word_document_is_read_and_a_broken_one_refused_before_any_task_exists() {
    let service = service();
    let accepted = service
        .accept_attachments(vec![AttachmentInput {
            name: "minutes.docx".into(),
            bytes: common::word(&common::body(
                "<w:p><w:r><w:t>Minutes of the safety review</w:t></w:r></w:p>",
            )),
        }])
        .await
        .unwrap();
    assert_eq!(accepted[0].media_type, WORD);
    assert_eq!(accepted[0].text, "Minutes of the safety review");

    let chat = service.create_conversation(None, "Chat").await.unwrap();
    let refused = service
        .start_task(StartTaskRequest {
            attachments: vec![AttachmentInput {
                name: "broken.docx".into(),
                bytes: b"PK\x03\x04 not really".to_vec(),
            }],
            ..start_request(&chat.conversation_id, "Read it.")
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
    assert!(refused.message.contains("broken.docx"));
    assert!(
        service
            .list_tasks(Some(&chat.conversation_id))
            .await
            .unwrap()
            .is_empty()
    );
}
