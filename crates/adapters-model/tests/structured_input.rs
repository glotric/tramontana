//! Structured conversations reach both wire formats without losing identity,
//! observations, or truncation. Invalid requests reach neither credentials nor HTTP.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "assertions in tests"
)]
mod fake_endpoint;
use async_trait::async_trait;
use fake_endpoint::{Behavior, FakeEndpoint};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
use tramontana_adapters_model::{AnthropicProvider, OpenAiCompatibleProvider, ProviderAuth};
use tramontana_core::testing::release::locally_authorized;
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

#[derive(Debug, Default)]
struct CountingSecret(AtomicUsize);
#[async_trait]
impl SecretResolver for CountingSecret {
    async fn resolve(&self, _reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("synthetic-key".into())
    }
}
fn credential() -> SecretRef {
    SecretRef {
        key: "test".into(),
        store: SecretStore::Platform,
    }
}
fn provider(
    endpoint: &FakeEndpoint,
    anthropic: bool,
    tools: bool,
    secrets: Arc<CountingSecret>,
) -> Box<dyn ModelProvider> {
    let mut descriptor = OpenAiCompatibleProvider::local_descriptor("fixture", "model", "Fixture");
    if tools {
        descriptor.capabilities.push(ModelCapability::ToolCalls);
    }
    if anthropic {
        Box::new(
            AnthropicProvider::new(
                "fixture",
                &endpoint.base_url,
                descriptor,
                credential(),
                secrets,
            )
            .unwrap(),
        )
    } else {
        Box::new(
            OpenAiCompatibleProvider::new(
                "fixture",
                &endpoint.base_url,
                descriptor,
                ProviderAuth::Bearer {
                    reference: credential(),
                    resolver: secrets,
                },
            )
            .unwrap(),
        )
    }
}
fn message(id: &str, role: MessageRole, parts: Vec<MessagePart>) -> NormalizedMessage {
    let mut message = NormalizedMessage::text(
        MessageId::new(id),
        None,
        role,
        Timestamp::new("2026-01-01T00:00:00Z"),
        "",
    );
    message.parts = parts;
    message
}
fn request() -> ModelRequest {
    let mut request = ModelRequest::new(
        "request",
        TaskId::new("task"),
        vec![
            message(
                "system",
                MessageRole::System,
                vec![MessagePart::Text {
                    text: "Read only".into(),
                }],
            ),
            message(
                "user",
                MessageRole::User,
                vec![MessagePart::Text {
                    text: "Inspect".into(),
                }],
            ),
            message(
                "calls",
                MessageRole::Assistant,
                vec![
                    MessagePart::Text {
                        text: "Looking".into(),
                    },
                    MessagePart::ToolCall {
                        invocation_id: InvocationId::new("invocation:1"),
                        tool_id: "builtin.fs.read".into(),
                        arguments: json!({"path":"README.md"}),
                    },
                    MessagePart::ToolCall {
                        invocation_id: InvocationId::new("invocation:2"),
                        tool_id: "builtin.fs.list".into(),
                        arguments: json!({"path":"."}),
                    },
                ],
            ),
            message(
                "result-2",
                MessageRole::Tool,
                vec![MessagePart::ToolResult {
                    invocation_id: InvocationId::new("invocation:2"),
                    content: json!({"files":["README.md"]}),
                    truncated: false,
                }],
            ),
            message(
                "result-1",
                MessageRole::Tool,
                vec![MessagePart::ToolResult {
                    invocation_id: InvocationId::new("invocation:1"),
                    content: json!({"text":"Fixture docs"}),
                    truncated: true,
                }],
            ),
        ],
        "context",
    );
    request.tools = ["builtin.fs.read","builtin.fs.list"].into_iter().map(|name|NormalizedToolDefinition {
        name:name.into(),description:format!("Description for {name}"),input_schema:json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})}).collect();
    request.required_capabilities = vec!["toolCalls".into()];
    request
}
async fn drain(provider: &dyn ModelProvider, request: AuthorizedModelRequest) -> Vec<ModelEvent> {
    let mut receiver = provider
        .stream(request, CancellationToken::new())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn structured_content_is_released_then_sent_losslessly_to_both_protocols() {
    for anthropic in [false, true] {
        let endpoint = FakeEndpoint::start(if anthropic {
            Behavior::StreamsAnthropic(vec!["Answer".into()])
        } else {
            Behavior::Streams(vec!["Answer".into()])
        })
        .await;
        let secrets = Arc::new(CountingSecret::default());
        let provider = provider(&endpoint, anthropic, true, secrets.clone());
        let request = request();
        let content = model_input_content(&request).unwrap();
        let manifest = ContextManifest::new(
            "context",
            TaskId::new("task"),
            DestinationClass::Local,
            content
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    ContextSource::included(
                        format!("source-{i}"),
                        ContextCategory::Conversation,
                        DataClass::Project,
                        text,
                    )
                })
                .collect(),
            Timestamp::new("2026-01-01T00:00:00Z"),
        );
        let decision = RoutingDecision::new(
            "decision",
            RoutingMode::LocalOnly,
            RoutedModel {
                provider_id: "fixture".into(),
                model_id: "model".into(),
            },
            ExecutionLocation::Local {
                target_id: "fixture".into(),
                display_name: "Fixture".into(),
            },
            "snapshot",
            vec![],
            "context",
        );
        let manager = ReleaseManager::new(
            Arc::new(InMemoryApprovalStore::new()),
            None,
            Arc::new(SequentialIdSource::new()),
        );
        let authorized = manager
            .authorize(
                "release",
                request.clone(),
                &ReleaseContext {
                    manifest: &manifest,
                    decision: &decision,
                    mode: RoutingMode::LocalOnly,
                    scope: &CapabilityScope::project(
                        ProjectId::new("project"),
                        vec!["/fixture".into()],
                    ),
                },
                Timestamp::new("2026-01-01T00:00:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(
            authorized.plan().tools_offered,
            vec!["builtin.fs.read", "builtin.fs.list"]
        );
        assert_eq!(
            authorized.plan().total_bytes,
            content
                .iter()
                .map(|content| content.len() as u64)
                .sum::<u64>()
        );
        let events = drain(provider.as_ref(), authorized).await;
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Completed {
                stop_reason: StopReason::Stop
            })
        ));
        assert_eq!(secrets.0.load(Ordering::SeqCst), 1);
        let body: Value = serde_json::from_str(&endpoint.received()[0].body).unwrap();
        let (call_name, call_args, call_id, result_id, result_content) = if anthropic {
            assert_eq!(body["system"], "Read only");
            assert_eq!(body["messages"].as_array().unwrap().len(), 3);
            assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
            assert_eq!(
                body["tools"][0]["input_schema"],
                request.tools[0].input_schema
            );
            (
                body["messages"][1]["content"][1]["name"].clone(),
                body["messages"][1]["content"][1]["input"].clone(),
                body["messages"][1]["content"][1]["id"].clone(),
                body["messages"][2]["content"][1]["tool_use_id"].clone(),
                body["messages"][2]["content"][1]["content"].clone(),
            )
        } else {
            assert_eq!(body["messages"][0]["role"], "system");
            assert_eq!(body["messages"][3]["role"], "tool");
            assert_eq!(
                body["tools"][0]["function"]["parameters"],
                request.tools[0].input_schema
            );
            (
                body["messages"][2]["tool_calls"][0]["function"]["name"].clone(),
                serde_json::from_str(
                    body["messages"][2]["tool_calls"][0]["function"]["arguments"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap(),
                body["messages"][2]["tool_calls"][0]["id"].clone(),
                body["messages"][4]["tool_call_id"].clone(),
                body["messages"][4]["content"].clone(),
            )
        };
        assert_eq!(call_name, "tool_1");
        assert_eq!(call_args, json!({"path":"README.md"}));
        assert_eq!(call_id, result_id);
        assert_eq!(
            serde_json::from_str::<Value>(result_content.as_str().unwrap()).unwrap(),
            json!({"content":{"text":"Fixture docs"},"truncated":true})
        );
        assert!(!endpoint.received()[0].body.contains("invocation:1"));
    }
}

#[tokio::test]
async fn wire_aliases_are_restored_in_streamed_candidates() {
    for anthropic in [false, true] {
        let frames = if anthropic {
            vec![
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"new-call","name":"tool_1","input":{}}}).to_string(),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"README.md\"}"}}).to_string(),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}).to_string(),json!({"type":"message_stop"}).to_string(),
        ]
        } else {
            vec![
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"new-call","function":{"name":"tool_1","arguments":"{\"path\":\"README.md\"}"}}]}}]}).to_string(),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}).to_string(),"[DONE]".into(),
        ]
        };
        let endpoint = FakeEndpoint::start(Behavior::Raw(frames)).await;
        let provider = provider(
            &endpoint,
            anthropic,
            true,
            Arc::new(CountingSecret::default()),
        );
        let events = drain(provider.as_ref(), locally_authorized(request())).await;
        let mut turn = ModelTurnAccumulator::new(
            "request".into(),
            CorrelationId::new("test"),
            Default::default(),
        );
        for event in events {
            turn.push(&event).unwrap();
        }
        let turn = turn.finish().unwrap();
        assert_eq!(turn.tool_calls[0].name, "builtin.fs.read");
        assert_eq!(turn.tool_calls[0].call_id, "new-call");
    }
}

#[tokio::test]
async fn invalid_or_unsupported_requests_touch_neither_secret_store_nor_endpoint() {
    for anthropic in [false, true] {
        let endpoint = FakeEndpoint::start(Behavior::Streams(vec![])).await;
        let secrets = Arc::new(CountingSecret::default());
        for scenario in [
            "capability",
            "orphan",
            "unknown-part",
            "response-format",
            "large",
        ] {
            let provider = provider(
                &endpoint,
                anthropic,
                scenario != "capability",
                secrets.clone(),
            );
            let mut request = request();
            match scenario {
                "orphan" => {
                    request.messages.remove(2);
                }
                "unknown-part" => request.messages[0].parts.push(MessagePart::Unsupported),
                "response-format" => {
                    request.response_format = Some(ResponseFormat::JsonSchema { schema: json!({}) })
                }
                "large" => request.tools[0].description = "x".repeat(MODEL_INPUT_BYTES),
                _ => {}
            }
            assert!(
                provider
                    .stream(locally_authorized(request), CancellationToken::new())
                    .await
                    .is_err(),
                "{scenario}"
            );
        }
        assert_eq!(secrets.0.load(Ordering::SeqCst), 0);
        assert!(endpoint.received().is_empty());
    }
}
