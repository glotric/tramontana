//! What is disclosed is what is sent, and nothing leaves without a decision.
//!
//! `SECURITY_AND_PERMISSIONS.md` §8 and `BUILD_ORDER.md` §14 item 5. Three
//! properties are the point of the whole boundary:
//!
//! - a cloud endpoint cannot be reached without an approval;
//! - the disclosed source categories and bytes are the transmitted ones
//!   (`TESTING_STRATEGY.md` §5);
//! - the disclosure describes the payload without becoming a copy of it.

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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

const GOAL: &str = "Say hello";

fn at() -> Timestamp {
    Timestamp::new("2026-01-01T00:00:00.000Z")
}

/// A manifest holding the goal plus one instruction source.
fn manifest(instruction_included: bool) -> ContextManifest {
    let instruction = if instruction_included {
        ContextSource::included(
            "AGENTS.md",
            ContextCategory::Instruction,
            DataClass::Project,
            "be careful",
        )
    } else {
        ContextSource::excluded(
            "AGENTS.md",
            ContextCategory::Instruction,
            DataClass::Project,
            "be careful",
            "context.notActive",
        )
    };
    ContextManifest::new(
        "context-1",
        TaskId::new("task-1"),
        DestinationClass::Local,
        vec![
            ContextSource::included(
                "conversation.goal",
                ContextCategory::Conversation,
                DataClass::Project,
                GOAL,
            ),
            instruction,
        ],
        at(),
    )
}

fn decision(location: ExecutionLocation) -> RoutingDecision {
    RoutingDecision::new(
        "decision-1",
        RoutingMode::CloudPreferred,
        RoutedModel {
            provider_id: "openai".to_owned(),
            model_id: "gpt-oss".to_owned(),
        },
        location,
        "availability-1",
        vec![routing_reason::CLOUD_PREFERRED.to_owned()],
        "context-1",
    )
}

fn cloud() -> ExecutionLocation {
    ExecutionLocation::Cloud {
        provider_id: "openai".to_owned(),
        region: None,
    }
}

fn on_device() -> ExecutionLocation {
    ExecutionLocation::Local {
        target_id: "this-device".to_owned(),
        display_name: "This device".to_owned(),
    }
}

/// A request carrying `texts` as separate user messages.
fn request(texts: &[&str]) -> ModelRequest {
    ModelRequest::new(
        "request-1",
        TaskId::new("task-1"),
        texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                NormalizedMessage::text(
                    MessageId::new(format!("message-{index}")),
                    None,
                    MessageRole::User,
                    at(),
                    *text,
                )
            })
            .collect(),
        "context-1",
    )
}

fn scope() -> CapabilityScope {
    CapabilityScope::project(ProjectId::new("project-1"), vec!["/work".to_owned()])
}

fn manager(approver: Arc<dyn Approver>) -> ReleaseManager {
    ReleaseManager::new(
        Arc::new(InMemoryApprovalStore::new()),
        Some(approver),
        Arc::new(SequentialIdSource::new()),
    )
}

#[test]
fn release_accounts_for_tool_definitions_and_all_structured_fields() {
    let mut request = request(&[GOAL]);
    request.tools.push(NormalizedToolDefinition {
        name: "builtin.fs.read".into(),
        description: "Read a file".into(),
        input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
    });
    let mut call = NormalizedMessage::text(
        MessageId::new("call"),
        None,
        MessageRole::Assistant,
        at(),
        "",
    );
    call.parts = vec![MessagePart::ToolCall {
        invocation_id: InvocationId::new("invocation"),
        tool_id: "builtin.fs.read".into(),
        arguments: serde_json::json!({"path":"README.md"}),
    }];
    let mut result =
        NormalizedMessage::text(MessageId::new("result"), None, MessageRole::Tool, at(), "");
    result.parts = vec![MessagePart::ToolResult {
        invocation_id: InvocationId::new("invocation"),
        content: serde_json::json!({"text":"Fixture"}),
        truncated: true,
    }];
    request.messages.extend([call, result]);
    let content = model_input_content(&request).unwrap();
    let manifest = ContextManifest::new(
        "context-1",
        request.task_id.clone(),
        DestinationClass::Cloud,
        content
            .iter()
            .enumerate()
            .map(|(i, content)| {
                ContextSource::included(
                    format!("source-{i}"),
                    if i == 2 {
                        ContextCategory::ToolResult
                    } else {
                        ContextCategory::Conversation
                    },
                    DataClass::Project,
                    content,
                )
            })
            .collect(),
        at(),
    );
    let decision = decision(cloud());
    let scope = scope();
    let context = ReleaseContext {
        manifest: &manifest,
        decision: &decision,
        mode: RoutingMode::CloudPreferred,
        scope: &scope,
    };
    let manager = manager(Arc::new(ScriptedApprover::unavailable()));
    let plan = manager.plan("release", &request, &context, at()).unwrap();
    assert_eq!(
        plan.total_bytes,
        content
            .iter()
            .map(|content| content.len() as u64)
            .sum::<u64>()
    );
    assert_eq!(plan.tools_offered, vec!["builtin.fs.read"]);
    assert_eq!(
        plan.released
            .iter()
            .map(|entry| entry.source_count)
            .sum::<u32>(),
        4
    );
    for field in [
        "description",
        "schema",
        "tool-id",
        "invocation-id",
        "truncated",
    ] {
        let mut changed = request.clone();
        match field {
            "description" => changed.tools[0].description = "Undisclosed project content".into(),
            "schema" => {
                changed.tools[0].input_schema["description"] = "Undisclosed schema content".into()
            }
            "tool-id" => {
                if let MessagePart::ToolCall { tool_id, .. } = &mut changed.messages[1].parts[0] {
                    *tool_id = "other.tool".into();
                }
            }
            "invocation-id" => {
                if let MessagePart::ToolCall { invocation_id, .. } =
                    &mut changed.messages[1].parts[0]
                {
                    *invocation_id = InvocationId::new("other");
                }
                if let MessagePart::ToolResult { invocation_id, .. } =
                    &mut changed.messages[2].parts[0]
                {
                    *invocation_id = InvocationId::new("other");
                }
            }
            _ => {
                if let MessagePart::ToolResult { truncated, .. } = &mut changed.messages[2].parts[0]
                {
                    *truncated = false;
                }
            }
        }
        assert!(
            matches!(
                manager.plan("changed", &changed, &context, at()),
                Err(ReleaseError::UndisclosedContent { .. })
            ),
            "{field}"
        );
    }
    let mut understated = manifest.clone();
    understated.sources[0].byte_count = 0;
    assert!(matches!(
        manager.plan(
            "understated",
            &request,
            &ReleaseContext {
                manifest: &understated,
                ..context
            },
            at()
        ),
        Err(ReleaseError::UndisclosedContent { .. })
    ));
}

#[tokio::test]
async fn a_local_destination_is_released_without_asking_anyone() {
    // A prompt that fires when nothing is at stake is how people are trained
    // to dismiss prompts. Nothing left the device, so nothing is asked.
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::Deny));
    let manifest = manifest(false);
    let decision = decision(on_device());

    let authorized = manager(approver.clone())
        .authorize(
            "release-1",
            request(&[GOAL]),
            &ReleaseContext {
                manifest: &manifest,
                decision: &decision,
                mode: RoutingMode::Automatic,
                scope: &scope(),
            },
            at(),
        )
        .await
        .expect("a local release was refused");

    assert!(
        approver.asked().is_empty(),
        "a local release asked the user"
    );
    assert!(!authorized.plan().requires_disclosure);
    assert_eq!(
        authorized
            .plan()
            .authorization
            .as_ref()
            .unwrap()
            .reason_code,
        release_reason::LOCAL_DESTINATION
    );
}

#[tokio::test]
async fn a_cloud_destination_asks_and_a_refusal_stops_it() {
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::Deny));
    let manifest = manifest(false);
    let decision = decision(cloud());

    let error = manager(approver.clone())
        .authorize(
            "release-1",
            request(&[GOAL]),
            &ReleaseContext {
                manifest: &manifest,
                decision: &decision,
                mode: RoutingMode::CloudPreferred,
                scope: &scope(),
            },
            at(),
        )
        .await
        .expect_err("a refused release produced an authorization");

    assert_eq!(
        error,
        ReleaseError::Denied {
            reason_code: release_reason::USER_DENIED
        }
    );

    // The question that was put names the destination and the release, and
    // offers no standing grant: the standing policy for cloud use is the
    // routing mode, not a second grant that would answer it again.
    let asked = approver.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].capability.as_str(), RELEASE_CAPABILITY);
    assert!(asked[0].requested_scope.releases_data);
    assert_eq!(asked[0].risk_class, RiskClass::Dangerous);
    assert_eq!(asked[0].side_effect, SideEffect::External);
    assert!(
        !asked[0]
            .eligible_decisions
            .contains(&ApprovalDecisionKind::AllowProject),
        "a cloud release offered a standing project grant"
    );
}

#[tokio::test]
async fn a_cloud_destination_with_nobody_to_ask_fails_closed() {
    // Not a refusal: there was no user. It still does not go.
    let manifest = manifest(false);
    let decision = decision(cloud());
    let manager = ReleaseManager::new(
        Arc::new(InMemoryApprovalStore::new()),
        None,
        Arc::new(SequentialIdSource::new()),
    );

    let error = manager
        .authorize(
            "release-1",
            request(&[GOAL]),
            &ReleaseContext {
                manifest: &manifest,
                decision: &decision,
                mode: RoutingMode::CloudPreferred,
                scope: &scope(),
            },
            at(),
        )
        .await
        .expect_err("a headless run released to the cloud");

    assert_eq!(
        error,
        ReleaseError::Denied {
            reason_code: release_reason::NO_APPROVER
        }
    );
}

#[test]
fn the_disclosed_categories_and_bytes_are_the_transmitted_ones() {
    // `TESTING_STRATEGY.md` §5. The manifest offers two sources; the payload
    // carries one. A plan derived from the manifest would disclose both.
    let manifest = manifest(true);
    let decision = decision(cloud());
    let plan = manager(Arc::new(ScriptedApprover::always(
        ApprovalDecisionKind::AllowOnce,
    )))
    .plan(
        "release-1",
        &request(&[GOAL]),
        &ReleaseContext {
            manifest: &manifest,
            decision: &decision,
            mode: RoutingMode::CloudPreferred,
            scope: &scope(),
        },
        at(),
    )
    .unwrap();

    assert_eq!(plan.released.len(), 1, "{:?}", plan.released);
    assert_eq!(plan.released[0].category, ContextCategory::Conversation);
    assert_eq!(plan.released[0].byte_count, GOAL.len() as u64);
    assert_eq!(plan.total_bytes, GOAL.len() as u64);
    assert!(
        plan.total_bytes < manifest.total_bytes,
        "the plan disclosed everything the manifest gathered, not what was sent"
    );

    // And the source that was gathered and not sent is named, not dropped:
    // "did my instructions reach the model?" has to be answerable.
    assert_eq!(plan.exclusions.len(), 1);
    assert_eq!(plan.exclusions[0].source_id, "AGENTS.md");
    assert_eq!(
        plan.exclusions[0].reason_code,
        release_reason::NOT_TRANSMITTED
    );
}

#[test]
fn a_payload_the_assembled_context_never_accounted_for_is_refused() {
    // The dangerous direction. A request that has grown a source since the
    // context was assembled would otherwise be disclosed as the smaller thing.
    let manifest = manifest(false);
    let decision = decision(cloud());
    let error = manager(Arc::new(ScriptedApprover::always(
        ApprovalDecisionKind::AllowOnce,
    )))
    .plan(
        "release-1",
        &request(&[GOAL, "and here is the private key from my notes"]),
        &ReleaseContext {
            manifest: &manifest,
            decision: &decision,
            mode: RoutingMode::CloudPreferred,
            scope: &scope(),
        },
        at(),
    )
    .expect_err("undisclosed content was planned for release");

    let ReleaseError::UndisclosedContent { content_hash } = error else {
        panic!("wrong refusal: {error:?}");
    };
    assert_eq!(
        content_hash,
        tramontana_core::contracts::content_hash("and here is the private key from my notes")
    );
}

#[test]
fn a_source_the_manifest_withheld_cannot_be_released_by_a_payload() {
    // Withholding has to mean something. A payload that carries an excluded
    // source is not a smaller disclosure — it is an undisclosed one.
    let manifest = manifest(false);
    let decision = decision(cloud());
    let error = manager(Arc::new(ScriptedApprover::always(
        ApprovalDecisionKind::AllowOnce,
    )))
    .plan(
        "release-1",
        &request(&[GOAL, "be careful"]),
        &ReleaseContext {
            manifest: &manifest,
            decision: &decision,
            mode: RoutingMode::CloudPreferred,
            scope: &scope(),
        },
        at(),
    )
    .expect_err("a withheld source was released");

    assert!(matches!(error, ReleaseError::UndisclosedContent { .. }));
}

#[test]
fn a_plan_describes_the_payload_without_copying_it() {
    let manifest = manifest(true);
    let decision = decision(cloud());
    let plan = manager(Arc::new(ScriptedApprover::always(
        ApprovalDecisionKind::AllowOnce,
    )))
    .plan(
        "release-1",
        &request(&[GOAL]),
        &ReleaseContext {
            manifest: &manifest,
            decision: &decision,
            mode: RoutingMode::CloudPreferred,
            scope: &scope(),
        },
        at(),
    )
    .unwrap();

    let rendered = serde_json::to_string(&plan).unwrap();
    assert!(
        !rendered.contains(GOAL),
        "the disclosure copied the content it was disclosing"
    );
    assert!(!rendered.contains("be careful"), "{rendered}");
    assert!(!plan.summary().contains(GOAL), "{}", plan.summary());
}

/// A cloud provider that records the payload it was handed.
///
/// Recording the text is what lets a test assert that a credential did not
/// reach a provider, which is the only assertion that actually matters.
#[derive(Debug, Default)]
struct RecordingEndpoint {
    seen: std::sync::Mutex<Vec<String>>,
}

impl RecordingEndpoint {
    fn payloads(&self) -> Vec<String> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl ModelProvider for RecordingEndpoint {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        FakeModelProvider::streaming(["ok"]).list_models().await
    }

    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(serde_json::to_string(request.request()).unwrap());
        }
        FakeModelProvider::streaming(["ok"])
            .stream(request, cancel)
            .await
    }
}

#[tokio::test]
async fn a_credential_pasted_into_a_goal_reaches_nothing() {
    // The whole redaction path, end to end. Every value here is synthetic.
    const PASTED: &str = "sk-proj-A1b2C3d4E5f6G7h8I9j0K1l2M3n4";

    let endpoint = Arc::new(RecordingEndpoint::default());
    let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        ids.clone(),
    )
    .unwrap();

    let service = TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: vec![endpoint.clone()],
            tools,
            permissions,
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        ids,
    );

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: format!("call the API with {PASTED} and report back"),
            ..Default::default()
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service
                .get_task(&task_id)
                .await
                .unwrap()
                .state
                .is_terminal()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task never settled");

    // Not in the durable record — not the task-created event, not the
    // manifest, not the release plan, not the committed message.
    let recorded = service.replay_task_events(&task_id, 0).await.unwrap();
    let rendered = serde_json::to_string(&recorded).unwrap();
    assert!(
        !rendered.contains(PASTED),
        "a pasted credential survived into the durable record"
    );
    assert!(
        rendered.contains("[redacted]"),
        "nothing was redacted at all"
    );

    // And above all, not in what was handed to the provider.
    let payloads = endpoint.payloads();
    assert_eq!(payloads.len(), 1, "the model was not called at all");
    assert!(
        !payloads[0].contains(PASTED),
        "a pasted credential was sent to a model provider"
    );

    // The release says what was taken out, so the removal is visible rather
    // than silent — a user who pasted a key should be able to see that it did
    // not go, not merely hope.
    let plan = recorded
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ContextReleased { plan } => Some(plan),
            _ => None,
        })
        .expect("no release was recorded");
    assert_eq!(
        plan.redactions,
        vec![redaction_reason::PROVIDER_API_KEY],
        "the release plan did not report the redaction it carried"
    );
}

/// A cloud provider that records every time it is reached.
#[derive(Debug, Default)]
struct CloudEndpoint {
    reached: AtomicUsize,
}

#[async_trait]
impl ModelProvider for CloudEndpoint {
    fn id(&self) -> ProviderId {
        ProviderId::new("openai")
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        vec![ModelDescriptor {
            provider_id: "openai".to_owned(),
            model_id: "gpt-oss".to_owned(),
            display_name: "gpt-oss".to_owned(),
            location: ModelLocation::Cloud,
            capabilities: vec![
                ModelCapability::Text,
                ModelCapability::Streaming,
                ModelCapability::Cancellation,
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
        let (sender, receiver) = mpsc::channel(4);
        let stream_id = request.request().request_id.clone();
        tokio::spawn(async move {
            if cancel.is_cancelled() {
                let _ = sender
                    .send(ModelEvent::Completed {
                        stop_reason: StopReason::Cancelled,
                    })
                    .await;
                return;
            }
            let _ = sender
                .send(ModelEvent::TextDelta {
                    stream_id,
                    offset: 0,
                    text: "answer".into(),
                })
                .await;
            let _ = sender
                .send(ModelEvent::Completed {
                    stop_reason: StopReason::Stop,
                })
                .await;
        });
        Ok(receiver)
    }
}

/// Run one task to a terminal state against `answer`, and return its events.
async fn run_to_completion(
    endpoint: Arc<CloudEndpoint>,
    answer: ApprovalDecisionKind,
) -> Vec<EventEnvelope> {
    let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::always(answer)),
        ids.clone(),
    )
    .unwrap();

    let service = TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: vec![endpoint],
            tools,
            permissions,
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(RoutingMode::CloudPreferred);

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: GOAL.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service
                .get_task(&task_id)
                .await
                .unwrap()
                .state
                .is_terminal()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task never settled");

    service.replay_task_events(&task_id, 0).await.unwrap()
}

#[tokio::test]
async fn a_refused_release_never_reaches_the_endpoint() {
    let endpoint = Arc::new(CloudEndpoint::default());
    let events = run_to_completion(endpoint.clone(), ApprovalDecisionKind::Deny).await;

    assert_eq!(
        endpoint.reached.load(Ordering::SeqCst),
        0,
        "a refused release still reached the cloud endpoint"
    );
    // And nothing in the record claims a model ran.
    assert!(
        !events
            .iter()
            .any(|event| event.kind == TaskEventKind::ModelStarted),
        "a refused release left a modelStarted event behind"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == TaskEventKind::TaskFailed),
        "a refused release did not fail the task"
    );
}

#[tokio::test]
async fn an_approved_release_is_recorded_before_the_model_starts() {
    let endpoint = Arc::new(CloudEndpoint::default());
    let events = run_to_completion(endpoint.clone(), ApprovalDecisionKind::AllowOnce).await;

    assert_eq!(endpoint.reached.load(Ordering::SeqCst), 1);

    let released = events
        .iter()
        .position(|event| event.kind == TaskEventKind::ContextReleased)
        .expect("no release was recorded");
    let started = events
        .iter()
        .position(|event| event.kind == TaskEventKind::ModelStarted)
        .expect("no model start was recorded");
    assert!(
        released < started,
        "the record says a model started before anyone decided what it could see"
    );

    let EventPayload::ContextReleased { plan } = &events[released].payload else {
        panic!("wrong payload");
    };
    assert!(plan.requires_disclosure);
    assert_eq!(
        plan.authorization.as_ref().unwrap().reason_code,
        release_reason::APPROVED_ONCE
    );
    assert!(plan.authorization.as_ref().unwrap().approval_id.is_some());
    assert_eq!(plan.destination.model_id, "gpt-oss");
    assert_eq!(plan.total_bytes, GOAL.len() as u64);

    // The durable record of a release does not carry what was released.
    let rendered = serde_json::to_string(&events[released]).unwrap();
    assert!(!rendered.contains(GOAL), "{rendered}");
}
