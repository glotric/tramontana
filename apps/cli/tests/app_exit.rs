//! M9's exit scenario, end to end.
//!
//! `BUILD_ORDER.md` §20: "The test App can run one read tool after approval,
//! is blocked after grant revocation, and malformed results cannot crash or
//! corrupt the task."
//!
//! Real: the CLI binary for configuring the App, SQLite, the MCP adapter over
//! a real stdio process, the registry and the permission manager, and the
//! OpenAI-compatible HTTP adapter. Synthetic: the App (the stand-in server
//! this repository ships), the model (scripted turn by turn), and the person
//! (a scripted approver, because the CLI's prompt refuses without a terminal).

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated integration fixture assertions"
)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use support::*;
use tramontana_composition::Supervision;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ApprovalRequest, EventPayload, ProjectId, TaskEventKind,
};
use tramontana_core::ports::{ApprovalStore, Approver, PermissionError};
use tramontana_core::testing::ScriptedApprover;

/// A person who allows the first question for the whole project, and refuses
/// everything after it. One person, changing their mind once, so the same
/// assembly can show both what a grant allows and what revoking it stops.
#[derive(Debug, Default)]
struct AllowsThenRefuses {
    asked: std::sync::Mutex<Vec<ApprovalRequest>>,
    refusing: std::sync::atomic::AtomicBool,
}

impl AllowsThenRefuses {
    fn asked(&self) -> Vec<ApprovalRequest> {
        self.asked
            .lock()
            .map(|asked| asked.clone())
            .unwrap_or_default()
    }

    fn refuse_from_now_on(&self) {
        self.refusing
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl Approver for AllowsThenRefuses {
    async fn ask(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(request.clone());
        }
        if self.refusing.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(ApprovalDecisionKind::Deny);
        }
        Ok(ApprovalDecisionKind::AllowProject)
    }
}

fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tramontana"))
        .parent()
        .expect("the test binary has a directory")
        .join("tramontana-fake-mcp-server")
}

/// Configure the stand-in App in `mode`, through the CLI as a person would.
fn configure_app(profile: &std::path::Path, mode: &str) {
    cli(
        profile,
        &[
            "app",
            "add",
            "fixture",
            "--program",
            fake_server().to_str().unwrap(),
            "--arg",
            mode,
        ],
    );
}

/// The model calls the App's read tool once, then answers.
///
/// Keyed on this task's own conversation rather than on `turn`, which counts
/// every request the scripted server has ever answered: this script drives
/// two tasks, and the second one's first request is not turn zero.
fn greet(turn: usize, body: &Value) -> (Value, &'static str) {
    if answered_already(body) {
        return answer("I asked the App.");
    }
    call(turn, body, "Says hello", json!({ "name": "Ada" }))
}

/// Whether this conversation already carries a tool's reply, however it went.
fn answered_already(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
}

/// The model tries the App's tool, then answers whatever came back.
fn try_once(turn: usize, body: &Value) -> (Value, &'static str) {
    match turn {
        0 => call(turn, body, "Says hello", json!({ "name": "Ada" })),
        _ => answer("The App did not answer usefully."),
    }
}

#[test]
fn an_apps_read_runs_once_approved_and_is_blocked_once_the_grant_is_revoked() {
    let f = fixture();
    configure_app(&f.profile, "ok");
    let bodies = scripted_model(&f.profile, greet);
    let first = conversation(&f, "Ask the App");
    let second = conversation(&f, "Ask the App again");
    // Allows it for the project the first time, and refuses afterwards: only
    // the grant could let a later call through without another yes, and
    // revoking it is what this proves.
    let approver = Arc::new(AllowsThenRefuses::default());
    let runtime = tokio::runtime::Runtime::new().unwrap();

    runtime.block_on(async {
        let running = assemble(&f.profile, approver.clone(), Supervision::Skip).await;
        let service = &running.service;

        // 1. The App's read tool runs, after the person approves it.
        let task = start(service, &first, "Ask the App to greet Ada.").await;
        let events = finished(service, &task, Duration::from_secs(30)).await;
        assert_eq!(
            events.last().unwrap().kind,
            TaskEventKind::TaskCompleted,
            "{events:?}"
        );
        let ran: Vec<String> = events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::ToolCompleted {
                    display_summary, ..
                } => Some(display_summary.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert!(ran[0].contains("Greeting returned"), "{ran:?}");
        // The App's tool was offered. Names on the wire are provider-neutral
        // aliases, so what identifies it is what it says it does; that it was
        // the App's tool which ran is what the summary above shows.
        let seen = bodies.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            seen["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["description"] == "Says hello to someone."),
            "the App's tool was not offered: {}",
            seen["tools"]
        );
        let shown = observation(&bodies.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(shown.contains("Hello, Ada"), "{shown}");

        // The person was asked, and the question named the App and did not
        // vouch for what it says about itself (ADR 0035).
        let asked = approver.asked();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(
            asked[0]
                .details
                .iter()
                .any(|detail| detail.label == "App" && detail.value == "fixture"),
            "{:?}",
            asked[0].details
        );
        assert!(
            asked[0]
                .details
                .iter()
                .any(|detail| detail.label == "The App says"
                    && detail.value.contains("not verified")),
            "{:?}",
            asked[0].details
        );

        // 2. Revoke the grant. Nothing else changes.
        let project = ProjectId::new(
            service
                .get_conversation(&first)
                .await
                .unwrap()
                .project_id
                .unwrap()
                .as_str(),
        );
        let grants = running.approvals.list_grants(&project).await.unwrap();
        assert_eq!(grants.len(), 1, "{grants:?}");
        assert_eq!(grants[0].tool_id, "mcp.fixture.greeting");
        assert!(
            running
                .approvals
                .revoke_grant(&grants[0].grant_id)
                .await
                .unwrap()
        );

        // The next call is asked about again, and this time the answer is no.
        approver.refuse_from_now_on();
        let task = start(service, &second, "Ask the App to greet Ada.").await;
        let events = finished(service, &task, Duration::from_secs(30)).await;
        let decisions: Vec<(bool, String)> = events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::PermissionDecided {
                    allowed,
                    reason_code,
                    ..
                } => Some((*allowed, reason_code.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(decisions.len(), 1, "{decisions:?}");
        assert!(
            !decisions[0].0,
            "the revoked grant still let the call through: {decisions:?}"
        );
        // Asked again, because nothing covered the call any more: one
        // question in each task, and the second one is the point.
        assert_eq!(approver.asked().len(), 2);
        assert!(
            !events
                .iter()
                .any(|event| matches!(&event.payload, EventPayload::ToolCompleted { .. })),
            "a tool ran after its grant was revoked"
        );
    });
}

#[test]
fn a_malformed_result_fails_the_call_and_leaves_the_record_whole() {
    let f = fixture();
    // The stand-in answers a call with something that is not a tool result.
    configure_app(&f.profile, "wrong-shape");
    let _bodies = scripted_model(&f.profile, try_once);
    let conversation = conversation(&f, "Ask a broken App");
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let runtime = tokio::runtime::Runtime::new().unwrap();

    runtime.block_on(async {
        let running = assemble(&f.profile, approver, Supervision::Skip).await;
        let service = &running.service;
        let task = start(service, &conversation, "Ask the App to greet Ada.").await;
        let events = finished(service, &task, Duration::from_secs(90)).await;

        // The task ended in exactly one terminal state, and the call failed
        // rather than the task dying with it.
        let terminal: Vec<TaskEventKind> = events
            .iter()
            .map(|event| event.kind)
            .filter(|kind| {
                matches!(
                    kind,
                    TaskEventKind::TaskCompleted
                        | TaskEventKind::TaskFailed
                        | TaskEventKind::TaskCancelled
                )
            })
            .collect();
        assert_eq!(terminal.len(), 1, "{terminal:?}");
        assert!(
            events
                .iter()
                .any(|event| matches!(&event.payload, EventPayload::ToolFailed { .. })),
            "the malformed result was not recorded as a failed call: {events:?}"
        );

        // The record reads back whole, in order, with nothing after the end.
        let sequences: Vec<u64> = events.iter().map(|event| event.sequence).collect();
        let mut ordered = sequences.clone();
        ordered.sort_unstable();
        ordered.dedup();
        assert_eq!(sequences, ordered, "the record is out of order or repeats");
        assert_eq!(
            service.get_task(&task).await.unwrap().last_sequence,
            *sequences.last().unwrap()
        );
    });
}
