#![allow(clippy::unwrap_used, reason = "assertions in tests")]
use std::sync::Arc;
use tramontana_core::{contracts::*, services::*, testing::*};

fn at() -> Timestamp {
    Timestamp::new("2026-01-01T00:00:00Z")
}
fn source(id: &str) -> ModelContentProvenance {
    ModelContentProvenance {
        source_id: id.into(),
        data_class: DataClass::Project,
        redactions: vec![],
    }
}
fn input(turn: u32) -> ModelContextInput {
    let mut request = ModelRequest::new(
        format!("request-{turn}"),
        TaskId::new("task"),
        vec![NormalizedMessage::text(
            MessageId::new("goal"),
            None,
            MessageRole::User,
            at(),
            "Inspect the README",
        )],
        format!("context-{turn}"),
    );
    request.tools.push(NormalizedToolDefinition {
        name: "builtin.fs.read".into(),
        description: "Read a file".into(),
        input_schema: serde_json::json!({"type":"object"}),
    });
    let mut messages = vec![source("conversation.goal")];
    if turn > 1 {
        let mut call = NormalizedMessage::text(
            MessageId::new("call"),
            None,
            MessageRole::Assistant,
            at(),
            "Looking",
        );
        call.parts.push(MessagePart::ToolCall {
            invocation_id: InvocationId::new("invocation"),
            tool_id: "builtin.fs.read".into(),
            arguments: serde_json::json!({"path":"README.md"}),
        });
        let mut result =
            NormalizedMessage::text(MessageId::new("result"), None, MessageRole::Tool, at(), "");
        result.parts = vec![MessagePart::ToolResult {
            invocation_id: InvocationId::new("invocation"),
            content: serde_json::json!({"text":"Fixture docs"}),
            truncated: true,
        }];
        request.messages.extend([call, result]);
        messages.extend([source("assistant.call"), source("tool.read")]);
    }
    ModelContextInput {
        request,
        messages,
        tools: vec![source("definition.read")],
    }
}
fn snapshot(cloud: bool, tools: bool) -> AvailabilitySnapshot {
    let mut model = FakeModelProvider::descriptor();
    model.location = if cloud {
        ModelLocation::Cloud
    } else {
        ModelLocation::Local
    };
    if tools {
        model.capabilities.push(ModelCapability::ToolCalls);
    }
    AvailabilitySnapshot {
        snapshot_id: "availability".into(),
        models: vec![model],
        taken_at: at(),
    }
}
fn prepare(input: ModelContextInput) -> Result<PreparedModelContext, ErrorEnvelope> {
    prepare_model_context(
        input,
        "decision",
        RoutingMode::LocalOnly,
        snapshot(false, true),
        at(),
    )
}

#[test]
fn selected_content_and_provenance_stay_paired() {
    let mut input = input(2);
    input.messages[2].data_class = DataClass::Sensitive;
    input.messages[2].redactions = vec![redaction_reason::PROVIDER_API_KEY.into()];
    input.request.data_classes = vec![DataClass::Public]; // Not authoritative.
    let prepared = prepare(input).unwrap();
    assert_eq!(
        prepared.request.data_classes,
        vec![DataClass::Sensitive, DataClass::Project]
    );
    assert_eq!(prepared.manifest.sources.len(), 5);
    assert_eq!(
        prepared.manifest.sources[1].source_id,
        "assistant.call.part-0"
    );
    assert_eq!(
        prepared.manifest.sources[2].source_id,
        "assistant.call.part-1"
    );
    assert_eq!(
        prepared.manifest.sources[3].category,
        ContextCategory::ToolResult
    );
    assert_eq!(
        prepared.manifest.sources[4].category,
        ContextCategory::RepositoryMetadata
    );
    assert_eq!(
        prepared.manifest.redactions,
        vec![redaction_reason::PROVIDER_API_KEY]
    );
    for (source, content) in prepared
        .manifest
        .sources
        .iter()
        .zip(model_input_content(&prepared.request).unwrap())
    {
        assert_eq!(source.content_hash, content_hash(&content));
        assert_eq!(source.byte_count, content.len() as u64);
    }
    assert!(
        !serde_json::to_string(&prepared.manifest)
            .unwrap()
            .contains("Fixture docs")
    );
    assert!(!format!("{prepared:?}").contains("Fixture docs"));
}

#[test]
fn incomplete_or_ambiguous_provenance_is_refused() {
    for scenario in [
        "missing",
        "duplicate",
        "part-collision",
        "empty-multipart",
        "large-id",
        "bad-code",
        "many-codes",
    ] {
        let mut input = input(2);
        match scenario {
            "missing" => {
                input.messages.pop();
            }
            "duplicate" => input.tools[0].source_id = input.messages[0].source_id.clone(),
            "part-collision" => input.tools[0].source_id = "assistant.call.part-0".into(),
            "empty-multipart" => input.messages[1].source_id.clear(),
            "large-id" => input.messages[0].source_id = "x".repeat(257),
            "bad-code" => input.messages[0].redactions = vec!["not a reason".into()],
            "many-codes" => input.messages[0].redactions = vec!["redaction.test".into(); 33],
            _ => unreachable!(),
        }
        assert_eq!(
            prepare(input).unwrap_err().code,
            ErrorCode::InvalidRequest,
            "{scenario}"
        );
    }
}

#[test]
fn secrets_are_refused_without_rewriting_arguments_or_metadata() {
    for scenario in [
        "text",
        "arguments",
        "result",
        "definition",
        "schema",
        "classified",
        "source",
    ] {
        let mut input = input(2);
        match scenario {
            "text" => {
                input.request.messages[0].parts = vec![MessagePart::Text {
                    text: "API_KEY=fixture-secret".into(),
                }]
            }
            "arguments" => {
                if let MessagePart::ToolCall { arguments, .. } =
                    &mut input.request.messages[1].parts[1]
                {
                    *arguments = serde_json::json!({"nested":{"password":"fixture-secret"}});
                }
            }
            "result" => {
                if let MessagePart::ToolResult { content, .. } =
                    &mut input.request.messages[2].parts[0]
                {
                    *content =
                        serde_json::json!({"text":"ok\nAuthorization: Bearer fixture-secret"});
                }
            }
            "definition" => input.request.tools[0].description = "API_KEY=fixture-secret".into(),
            "schema" => {
                input.request.tools[0].input_schema["description"] = "API_KEY=fixture-secret".into()
            }
            "classified" => input.messages[0].data_class = DataClass::Secret,
            "source" => input.messages[0].source_id = "API_KEY=fixture-secret".into(),
            _ => unreachable!(),
        }
        let error = prepare(input).unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{scenario}");
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }
    let mut redacted = input(2);
    redacted.request.messages[0].parts = vec![MessagePart::Text {
        text: "API_KEY=[redacted]".into(),
    }];
    redacted.messages[0].redactions = vec![redaction_reason::CREDENTIAL_ASSIGNMENT.into()];
    assert!(prepare(redacted).is_ok());
}

#[test]
fn routing_checks_each_turn_and_never_enables_an_ineligible_fallback() {
    assert_eq!(
        prepare_model_context(
            input(1),
            "decision",
            RoutingMode::LocalOnly,
            snapshot(false, false),
            at()
        )
        .unwrap_err()
        .code,
        ErrorCode::CapabilityUnsupported
    );
    assert_eq!(
        prepare_model_context(
            input(1),
            "decision",
            RoutingMode::LocalOnly,
            snapshot(true, true),
            at()
        )
        .unwrap_err()
        .code,
        ErrorCode::ProviderUnavailable
    );
    let local = prepare(input(1)).unwrap();
    assert_eq!(local.manifest.destination_class, DestinationClass::Local);
    assert!(local.decision.fallback_order.is_empty());
    let cloud = prepare_model_context(
        input(2),
        "decision-2",
        RoutingMode::CloudPreferred,
        snapshot(true, true),
        at(),
    )
    .unwrap();
    assert_eq!(cloud.manifest.destination_class, DestinationClass::Cloud);
    assert!(cloud.decision.requires_disclosure);
    assert_ne!(
        cloud.manifest.context_manifest_id,
        local.manifest.context_manifest_id
    );
    assert_eq!(
        cloud.decision.context_manifest_id,
        cloud.manifest.context_manifest_id
    );
}

#[tokio::test]
async fn each_cloud_turn_requires_its_own_release_and_denial_stops_the_next() {
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    let manager = ReleaseManager::new(
        Arc::new(InMemoryApprovalStore::new()),
        Some(approver.clone()),
        Arc::new(SequentialIdSource::new()),
    );
    let scope = CapabilityScope::project(ProjectId::new("project"), vec!["/fixture".into()]);
    let mut first = None;
    for turn in 1..=2 {
        let prepared = prepare_model_context(
            input(turn),
            format!("decision-{turn}"),
            RoutingMode::CloudPreferred,
            snapshot(true, true),
            at(),
        )
        .unwrap();
        let context = ReleaseContext {
            manifest: &prepared.manifest,
            decision: &prepared.decision,
            mode: RoutingMode::CloudPreferred,
            scope: &scope,
        };
        if let Some((ref old_manifest, ref old_decision)) = first {
            let stale = ReleaseContext {
                manifest: old_manifest,
                decision: old_decision,
                ..context
            };
            assert!(matches!(
                manager.plan("stale", &prepared.request, &stale, at()),
                Err(ReleaseError::ManifestMismatch { .. })
            ));
        }
        let authorized = manager
            .authorize(format!("release-{turn}"), prepared.request, &context, at())
            .await
            .unwrap();
        assert_eq!(authorized.plan().total_bytes, prepared.manifest.total_bytes);
        first = Some((prepared.manifest, prepared.decision));
    }
    assert_eq!(approver.asked().len(), 2);
    let denied = ReleaseManager::new(
        Arc::new(InMemoryApprovalStore::new()),
        Some(Arc::new(ScriptedApprover::always(
            ApprovalDecisionKind::Deny,
        ))),
        Arc::new(SequentialIdSource::new()),
    );
    let prepared = prepare_model_context(
        input(3),
        "decision-3",
        RoutingMode::CloudPreferred,
        snapshot(true, true),
        at(),
    )
    .unwrap();
    assert!(matches!(
        denied
            .authorize(
                "release-3",
                prepared.request,
                &ReleaseContext {
                    manifest: &prepared.manifest,
                    decision: &prepared.decision,
                    mode: RoutingMode::CloudPreferred,
                    scope: &scope
                },
                at()
            )
            .await,
        Err(ReleaseError::Denied { .. })
    ));
}

#[test]
fn oversized_or_unpaired_content_is_not_silently_trimmed() {
    let mut oversized = input(2);
    oversized.request.tools[0].description = "x".repeat(MODEL_INPUT_BYTES);
    assert_eq!(
        prepare(oversized).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    let mut unpaired = input(2);
    unpaired.request.messages.pop();
    unpaired.messages.pop();
    assert_eq!(
        prepare(unpaired).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}

#[test]
fn duplicate_content_cannot_hide_a_stronger_classification() {
    let mut input = input(1);
    let mut duplicate = input.request.messages[0].clone();
    duplicate.message_id = MessageId::new("second");
    input.request.messages.push(duplicate);
    let mut stronger = source("conversation.second");
    stronger.data_class = DataClass::Sensitive;
    input.messages.push(stronger);
    assert_eq!(prepare(input).unwrap_err().code, ErrorCode::InvalidRequest);
}
