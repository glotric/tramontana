//! The Anthropic adapter, against a real socket.
//!
//! It passes the same `model_provider_contract` suite as the in-process fake
//! and the chat-completions adapter, so a divergence between the three is a
//! defect in one of them rather than a difference in what was tested.
//!
//! Beyond the shared contract, what is tested here is the part that is
//! genuinely Anthropic's and would be a silent bug if copied from the OpenAI
//! adapter: the system prompt hoisted out of the message list, the required
//! `max_tokens`, and `x-api-key` rather than a bearer token.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

mod fake_endpoint;

use std::sync::Arc;

use async_trait::async_trait;
use fake_endpoint::{Behavior, FakeEndpoint};
use tokio_util::sync::CancellationToken;
use tramontana_adapters_model::AnthropicProvider;
use tramontana_core::contracts::{
    ErrorEnvelope, MessageId, MessageRole, ModelEvent, ModelRequest, NormalizedMessage, SecretRef,
    SecretStore, StopReason, TaskId, Timestamp,
};
use tramontana_core::ports::{ModelProvider, SecretResolver};
use tramontana_core::testing::contract_suites::{
    ProviderScenario, model_provider_contract, model_request,
};
use tramontana_core::testing::release::locally_authorized;

/// A resolver that hands back one synthetic value.
#[derive(Debug)]
struct FixedSecret(String);

#[async_trait]
impl SecretResolver for FixedSecret {
    async fn resolve(&self, _reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        Ok(self.0.clone())
    }
}

fn credential() -> SecretRef {
    SecretRef {
        key: "anthropic.apiKey".to_owned(),
        store: SecretStore::Platform,
    }
}

fn provider(endpoint: &FakeEndpoint, key: &str) -> AnthropicProvider {
    AnthropicProvider::new(
        &endpoint.base_url,
        AnthropicProvider::cloud_descriptor("claude-test", "Claude test", Some(200_000)),
        credential(),
        Arc::new(FixedSecret(key.to_owned())),
    )
    .unwrap()
}

async fn drain(provider: &AnthropicProvider) -> Vec<ModelEvent> {
    let mut stream = provider
        .stream(
            locally_authorized(model_request("request-1")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn the_adapter_obeys_the_model_provider_contract_over_a_socket() {
    model_provider_contract(|scenario| async move {
        let behavior = match scenario {
            ProviderScenario::Streams(chunks) => Behavior::StreamsAnthropic(chunks),
            ProviderScenario::Fails => Behavior::Status(503),
            // Headers arrive and content never does, so a cancel always lands
            // inside the stream rather than after it.
            ProviderScenario::Slow => Behavior::Silent,
        };
        let endpoint = FakeEndpoint::start(behavior).await;
        let provider = AnthropicProvider::new(
            &endpoint.base_url,
            AnthropicProvider::cloud_descriptor("claude-test", "Claude test", None),
            credential(),
            Arc::new(FixedSecret("synthetic-key".to_owned())),
        )
        .unwrap();
        // The endpoint outlives the provider for the duration of the case.
        Box::leak(Box::new(endpoint));
        Arc::new(provider) as Arc<dyn ModelProvider>
    })
    .await;
}

#[tokio::test]
async fn a_system_message_is_hoisted_out_of_the_conversation() {
    // Anthropic rejects a `system`-role message outright, and relabelling it
    // `user` would change what the model was told rather than fail loudly.
    let endpoint = FakeEndpoint::start(Behavior::StreamsAnthropic(vec!["ok".into()])).await;
    let provider = provider(&endpoint, "synthetic-key");

    let request = ModelRequest::new(
        "request-1",
        TaskId::new("task-1"),
        vec![
            NormalizedMessage::text(
                MessageId::new("m1"),
                None,
                MessageRole::System,
                Timestamp::new("2026-01-01T00:00:00.000Z"),
                "Be brief.",
            ),
            NormalizedMessage::text(
                MessageId::new("m2"),
                None,
                MessageRole::User,
                Timestamp::new("2026-01-01T00:00:00.000Z"),
                "Say hello",
            ),
        ],
        "context-1",
    );
    let mut stream = provider
        .stream(locally_authorized(request), CancellationToken::new())
        .await
        .unwrap();
    while stream.recv().await.is_some() {}

    let received = endpoint
        .received()
        .pop()
        .expect("the endpoint received nothing");
    let body: serde_json::Value = serde_json::from_str(&received.body).unwrap();
    assert_eq!(body["system"], "Be brief.");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "Say hello");
}

#[tokio::test]
async fn every_request_carries_max_tokens_because_the_api_requires_it() {
    // There is no server-side default. A request without it is rejected, and
    // the failure would look like a provider outage rather than a bug here.
    let endpoint = FakeEndpoint::start(Behavior::StreamsAnthropic(vec!["ok".into()])).await;
    let provider = provider(&endpoint, "synthetic-key");
    drain(&provider).await;

    let body: serde_json::Value = serde_json::from_str(
        &endpoint
            .received()
            .pop()
            .expect("the endpoint received nothing")
            .body,
    )
    .unwrap();
    assert!(
        body["max_tokens"].as_u64().is_some_and(|limit| limit > 0),
        "no usable max_tokens in {body}"
    );
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn the_credential_travels_in_x_api_key_and_nowhere_else() {
    let endpoint = FakeEndpoint::start(Behavior::StreamsAnthropic(vec!["ok".into()])).await;
    let provider = provider(&endpoint, "synthetic-key-value");
    drain(&provider).await;

    let received = endpoint
        .received()
        .pop()
        .expect("the endpoint received nothing");
    assert_eq!(received.header("x-api-key"), Some("synthetic-key-value"));
    assert_eq!(received.header("anthropic-version"), Some("2023-06-01"));
    assert!(
        received.header("authorization").is_none(),
        "a bearer token was sent to an endpoint that authenticates with x-api-key"
    );
    // Not in the URL, and not in the body.
    assert!(!received.request_line.contains("synthetic-key-value"));
    assert!(!received.body.contains("synthetic-key-value"));
}

#[tokio::test]
async fn the_debug_format_never_prints_the_credential() {
    let endpoint = FakeEndpoint::start(Behavior::StreamsAnthropic(vec!["ok".into()])).await;
    let rendered = format!("{:?}", provider(&endpoint, "synthetic-key-value"));
    assert!(!rendered.contains("synthetic-key-value"), "{rendered}");
    // The reference is fine to print; it is what the database holds anyway.
    assert!(rendered.contains("anthropic.apiKey"), "{rendered}");
}

#[tokio::test]
async fn usage_arrives_even_though_it_is_split_across_two_frames() {
    // Input tokens come on `message_start` and output tokens on
    // `message_delta`. Reading only one frame would report half the cost.
    let endpoint = FakeEndpoint::start(Behavior::StreamsAnthropic(vec!["ok".into()])).await;
    let events = drain(&provider(&endpoint, "synthetic-key")).await;

    let input: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::Usage { usage } => usage.input_tokens,
            _ => None,
        })
        .collect();
    let output: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::Usage { usage } => usage.output_tokens,
            _ => None,
        })
        .collect();
    assert_eq!(input, vec![11], "input tokens were not reported");
    assert_eq!(output, vec![3], "output tokens were not reported");
}

#[tokio::test]
async fn an_error_frame_inside_the_stream_becomes_the_terminal_failure() {
    // Anthropic can report an error mid-stream rather than as a status. A
    // decoder that ignored it would hand back a truncated answer reported as
    // a success.
    let endpoint = FakeEndpoint::start(Behavior::Raw(vec![
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"par"}}"#
            .to_owned(),
        r#"{"type":"error","error":{"type":"overloaded_error"}}"#.to_owned(),
    ]))
    .await;
    let events = drain(&provider(&endpoint, "synthetic-key")).await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelEvent::Failed { .. })),
        "an in-stream error did not produce a failure: {events:?}"
    );
    let terminal = events.iter().filter(|event| event.is_terminal()).count();
    assert_eq!(
        terminal, 1,
        "expected exactly one terminal event: {events:?}"
    );
}

#[tokio::test]
async fn a_plain_http_endpoint_that_is_not_loopback_is_refused() {
    let error = AnthropicProvider::new(
        "http://api.anthropic.com",
        AnthropicProvider::cloud_descriptor("claude-test", "Claude test", None),
        credential(),
        Arc::new(FixedSecret("synthetic-key".to_owned())),
    )
    .unwrap_err();
    assert!(error.message.contains("loopback"), "{}", error.message);
}

#[tokio::test]
async fn a_cloud_descriptor_says_it_is_cloud() {
    // Not cosmetic: the router treats a cloud location as a prohibition under
    // Local Only, and the release boundary requires a disclosure for it.
    let descriptor = AnthropicProvider::cloud_descriptor("claude-test", "Claude test", None);
    assert_eq!(
        descriptor.location,
        tramontana_core::contracts::ModelLocation::Cloud
    );
    let _ = StopReason::Stop;
}
