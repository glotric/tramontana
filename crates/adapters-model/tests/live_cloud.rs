//! The cloud adapters against the real services, when a developer asks.
//!
//! Every other test of these adapters runs against a fake endpoint in this
//! process, which is what keeps the suite offline and free
//! (`TESTING_STRATEGY.md`). That leaves one thing untested: whether the real
//! services behave as the fakes were written to. Until this file ran, no
//! request had ever been made to either vendor.
//!
//! `TESTING_STRATEGY.md` already settles the policy: "cloud adapter tests use
//! fakes/recorded fixtures unless explicitly enabled by a developer". This is
//! that explicit enabling, so it needs no decision of its own.
//!
//! Runs only when a key is in the environment:
//!
//! ```text
//! TRAMONTANA_LIVE_OPENAI_KEY=sk-…    [TRAMONTANA_LIVE_OPENAI_MODEL=gpt-…]
//! TRAMONTANA_LIVE_ANTHROPIC_KEY=sk-… [TRAMONTANA_LIVE_ANTHROPIC_MODEL=claude-…]
//! ```
//!
//! `TRAMONTANA_LIVE_<VENDOR>_URL` points the same checks at another
//! OpenAI-compatible or Anthropic-compatible address — a gateway, a proxy, or
//! the fake endpoint this crate already has. The last of those is how the
//! checks themselves are tested: `the_checks_hold_against_the_fake_endpoint`
//! runs the same helpers over a known answer, every time the suite runs, so a
//! mistake here is not discovered by spending money on it.
//!
//! Otherwise each test says it was skipped and passes, as the other live
//! variants do. The key is the developer's own, read from the environment and
//! written nowhere: not to a profile, not to a log, not into an assertion
//! message.
//!
//! **These cost money.** Each test sends a sentence and caps the answer, so a
//! whole run is a fraction of a cent, but it is not nothing, and nothing here
//! runs by accident.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

mod fake_endpoint;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_adapters_model::{AnthropicProvider, OpenAiCompatibleProvider, ProviderAuth};
use tramontana_core::contracts::{
    ErrorEnvelope, Health, MessageId, MessageRole, ModelCapability, ModelDescriptor, ModelEvent,
    ModelLimits, ModelLocation, ModelRequest, NormalizedMessage, NormalizedToolDefinition,
    SecretRef, SecretStore, StopReason, TaskId, Timestamp,
};
use tramontana_core::ports::{ModelProvider, SecretResolver};
use tramontana_core::testing::release::locally_authorized;

/// The key, straight from the environment and into the one request.
#[derive(Debug)]
struct FromEnvironment(String);

#[async_trait]
impl SecretResolver for FromEnvironment {
    async fn resolve(&self, _reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        Ok(self.0.clone())
    }
}

fn reference(key: &str) -> SecretRef {
    SecretRef {
        key: key.to_owned(),
        store: SecretStore::Platform,
    }
}

fn descriptor(provider_id: &str, model_id: &str, display_name: &str) -> ModelDescriptor {
    ModelDescriptor {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        display_name: display_name.to_owned(),
        location: ModelLocation::Cloud,
        capabilities: vec![
            ModelCapability::Text,
            ModelCapability::Streaming,
            ModelCapability::Cancellation,
            ModelCapability::ToolCalls,
        ],
        context_limit: None,
        health: Health::Available,
        health_reason_code: None,
    }
}

/// A key, the model to spend it on and where to send it, or nothing to do.
fn configured(vendor: &str, default_model: &str, default_url: &str) -> Option<Live> {
    let key = std::env::var(format!("TRAMONTANA_LIVE_{vendor}_KEY")).ok()?;
    if key.trim().is_empty() {
        return None;
    }
    Some(Live {
        key,
        model: std::env::var(format!("TRAMONTANA_LIVE_{vendor}_MODEL"))
            .unwrap_or_else(|_| default_model.to_owned()),
        url: std::env::var(format!("TRAMONTANA_LIVE_{vendor}_URL"))
            .unwrap_or_else(|_| default_url.to_owned()),
    })
}

/// What a configured vendor amounts to.
struct Live {
    key: String,
    model: String,
    url: String,
}

fn skip(vendor: &str) {
    eprintln!("skipped: set TRAMONTANA_LIVE_{vendor}_KEY to run this against the real service");
}

/// One short question, with the answer capped so a run stays cheap.
fn ask(request_id: &str, question: &str) -> ModelRequest {
    let mut request = ModelRequest::new(
        request_id,
        TaskId::new("task-live"),
        vec![NormalizedMessage::text(
            MessageId::new("message-1"),
            None,
            MessageRole::User,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
            question,
        )],
        "context-live",
    );
    request.limits = ModelLimits {
        max_output_tokens: Some(64),
        deadline_ms: 60_000,
    };
    request
}

/// A tool worth calling, and impossible to answer without.
fn tool() -> NormalizedToolDefinition {
    NormalizedToolDefinition {
        name: "builtin.fs.read".to_owned(),
        description: "Read a UTF-8 text file inside the project.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false,
        }),
    }
}

/// Everything a stream produced, with the answer's text joined.
struct Answer {
    events: Vec<ModelEvent>,
    text: String,
}

impl Answer {
    fn of(events: Vec<ModelEvent>) -> Self {
        let text = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::TextDelta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        Self { events, text }
    }

    fn stop_reason(&self) -> Option<StopReason> {
        self.events.iter().find_map(|event| match event {
            ModelEvent::Completed { stop_reason } => Some(*stop_reason),
            _ => None,
        })
    }

    fn tool_calls(&self) -> Vec<(String, serde_json::Value)> {
        // Arguments arrive in fragments; the adapter is what joins them, so
        // what is asserted is the joined form a worker would act on.
        let mut calls: std::collections::BTreeMap<u64, (String, String)> =
            std::collections::BTreeMap::new();
        for event in &self.events {
            if let ModelEvent::ToolCallDelta {
                call_index,
                name,
                delta,
                ..
            } = event
            {
                let entry = calls.entry(*call_index).or_default();
                // The name arrives on the opening fragment only.
                if let Some(name) = name {
                    entry.0 = name.clone();
                }
                entry.1.push_str(delta);
            }
        }
        calls
            .into_values()
            .map(|(name, arguments)| {
                let parsed = serde_json::from_str(&arguments).unwrap_or(serde_json::Value::Null);
                (name, parsed)
            })
            .collect()
    }

    fn failed(&self) -> Option<&ErrorEnvelope> {
        self.events.iter().find_map(|event| match event {
            ModelEvent::Failed { error } => Some(error),
            _ => None,
        })
    }
}

fn openai(live: &Live) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        "openai",
        &live.url,
        descriptor("openai", &live.model, "OpenAI"),
        ProviderAuth::Bearer {
            reference: reference("openai.apiKey"),
            resolver: Arc::new(FromEnvironment(live.key.clone())),
        },
    )
    .unwrap()
}

fn anthropic(live: &Live) -> AnthropicProvider {
    AnthropicProvider::new(
        "anthropic",
        &live.url,
        descriptor("anthropic", &live.model, "Claude"),
        reference("anthropic.apiKey"),
        Arc::new(FromEnvironment(live.key.clone())),
    )
    .unwrap()
}

async fn run(provider: &dyn ModelProvider, request: ModelRequest) -> Answer {
    let mut stream = provider
        .stream(locally_authorized(request), CancellationToken::new())
        .await
        .expect("the provider accepted the request");
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    Answer::of(events)
}

/// Assert the contract every provider owes, against a real answer.
fn assert_answered(answer: &Answer, label: &str) {
    assert!(
        answer.failed().is_none(),
        "{label} failed: {:?}",
        answer.failed()
    );
    assert!(
        !answer.text.trim().is_empty(),
        "{label} said nothing: {:?}",
        answer.events
    );
    assert!(
        answer.stop_reason().is_some(),
        "{label} ended without exactly one terminal event: {:?}",
        answer.events
    );
    let terminal = answer
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                ModelEvent::Completed { .. } | ModelEvent::Failed { .. }
            )
        })
        .count();
    assert_eq!(terminal, 1, "{label} ended more than once");
    eprintln!("{label}: {}", answer.text.trim());
}

#[tokio::test]
async fn openai_answers_a_question_and_calls_a_tool() {
    let Some(live) = configured("OPENAI", "gpt-4o-mini", "https://api.openai.com") else {
        skip("OPENAI");
        return;
    };
    let model = live.model.clone();
    let provider = openai(&live);

    // What the adapter says it offers must survive contact with the service.
    let listed = provider.list_models().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].model_id, model);

    let answer = run(
        &provider,
        ask("live-openai-1", "Reply with the word ready."),
    )
    .await;
    assert_answered(&answer, "openai");

    // The part the worker depends on: a native tool call, with arguments that
    // parse. A model that cannot do this cannot edit a file (ADR 0020).
    let mut asking = ask(
        "live-openai-2",
        "Read the file README.md. Use the tool; do not answer from memory.",
    );
    asking.tools = vec![tool()];
    let calling = run(&provider, asking).await;
    assert!(calling.failed().is_none(), "{:?}", calling.failed());
    let calls = calling.tool_calls();
    assert!(
        calls
            .iter()
            .any(|(name, arguments)| name == "builtin.fs.read" && arguments.get("path").is_some()),
        "openai made no usable tool call: {:?}",
        calling.events
    );
    eprintln!("openai tool calls: {calls:?}");
}

#[tokio::test]
async fn anthropic_answers_a_question_and_calls_a_tool() {
    let Some(live) = configured(
        "ANTHROPIC",
        "claude-sonnet-4-5",
        "https://api.anthropic.com",
    ) else {
        skip("ANTHROPIC");
        return;
    };
    let model = live.model.clone();
    let provider = anthropic(&live);

    let listed = provider.list_models().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].model_id, model);

    let answer = run(
        &provider,
        ask("live-anthropic-1", "Reply with the word ready."),
    )
    .await;
    assert_answered(&answer, "anthropic");

    let mut asking = ask(
        "live-anthropic-2",
        "Read the file README.md. Use the tool; do not answer from memory.",
    );
    asking.tools = vec![tool()];
    let calling = run(&provider, asking).await;
    assert!(calling.failed().is_none(), "{:?}", calling.failed());
    let calls = calling.tool_calls();
    assert!(
        calls
            .iter()
            .any(|(name, arguments)| name == "builtin.fs.read" && arguments.get("path").is_some()),
        "anthropic made no usable tool call: {:?}",
        calling.events
    );
    eprintln!("anthropic tool calls: {calls:?}");
}

#[tokio::test]
async fn a_cancelled_cloud_turn_ends_at_once_and_says_so() {
    // Whichever vendor is configured; the behaviour is the contract's, not
    // the vendor's, and one is enough to prove it against a real socket.
    let with_openai = configured("OPENAI", "gpt-4o-mini", "https://api.openai.com");
    let with_anthropic = configured(
        "ANTHROPIC",
        "claude-sonnet-4-5",
        "https://api.anthropic.com",
    );
    let provider: Box<dyn ModelProvider> = match (&with_openai, &with_anthropic) {
        (Some(live), _) => Box::new(openai(live)),
        (None, Some(live)) => Box::new(anthropic(live)),
        (None, None) => {
            skip("OPENAI (or TRAMONTANA_LIVE_ANTHROPIC_KEY)");
            return;
        }
    };

    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(
            locally_authorized(ask("live-cancel", "Count slowly from one to twenty.")),
            cancel.clone(),
        )
        .await
        .unwrap();
    // After the first frame, so there is a real request in flight to cancel.
    let first = tokio::time::timeout(Duration::from_secs(30), stream.recv())
        .await
        .expect("a first frame arrived")
        .expect("the stream was open");
    cancel.cancel();

    let mut events = vec![first];
    while let Some(event) = tokio::time::timeout(Duration::from_secs(10), stream.recv())
        .await
        .expect("a cancelled stream ends promptly")
    {
        events.push(event);
    }
    let answer = Answer::of(events);
    let terminal = answer
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                ModelEvent::Completed { .. } | ModelEvent::Failed { .. }
            )
        })
        .count();
    assert_eq!(
        terminal, 1,
        "a cancelled turn ends exactly once: {:?}",
        answer.events
    );
    eprintln!("cancelled after {} event(s)", answer.events.len());
}

#[tokio::test]
async fn a_key_the_service_rejects_is_a_normalized_error() {
    // Needs no real key, and costs nothing: it proves the adapter turns a
    // refusal into the error the router and the interface expect, against the
    // real service rather than a fake's idea of it.
    if std::env::var_os("TRAMONTANA_LIVE_CLOUD_REFUSAL").is_none() {
        eprintln!("skipped: set TRAMONTANA_LIVE_CLOUD_REFUSAL=1 to reach the real services");
        return;
    }
    let provider = anthropic(&Live {
        key: "sk-ant-not-a-real-key".to_owned(),
        model: "claude-sonnet-4-5".to_owned(),
        url: "https://api.anthropic.com".to_owned(),
    });

    let answer = run(&provider, ask("live-refusal", "Say hello.")).await;

    let failure = answer
        .failed()
        .expect("a rejected key ends the stream with a failure");
    eprintln!("refusal: {:?} {}", failure.code, failure.message);
    assert!(
        !failure.message.contains("sk-ant-not-a-real-key"),
        "the error must not echo the credential: {}",
        failure.message
    );
}

#[tokio::test]
async fn the_checks_hold_against_the_fake_endpoint() {
    // The live checks above only run when someone pays for them, so their own
    // logic is exercised here every time, against a known answer: a text turn
    // that must pass `assert_answered`, and a tool call that must be joined
    // into arguments a worker could act on.
    let endpoint = fake_endpoint::FakeEndpoint::start(fake_endpoint::Behavior::Streams(vec![
        "rea".to_owned(),
        "dy".to_owned(),
    ]))
    .await;
    let live = Live {
        key: "not-a-real-key".to_owned(),
        model: "fake-model".to_owned(),
        url: endpoint.base_url.clone(),
    };

    let answer = run(&openai(&live), ask("fake-1", "Reply with the word ready.")).await;
    assert_answered(&answer, "fake");
    assert_eq!(answer.text, "ready");

    // Arguments split across fragments, as every real provider sends them.
    let calling = fake_endpoint::FakeEndpoint::start(fake_endpoint::Behavior::Raw(vec![
        serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"builtin.fs.read","arguments":"{\"pa"}}]}}]}).to_string(),
        serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"README.md\"}"}}]}}]}).to_string(),
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}).to_string(),
        "[DONE]".to_owned(),
    ]))
    .await;
    let mut asking = ask("fake-2", "Read README.md.");
    asking.tools = vec![tool()];
    let joined = run(
        &openai(&Live {
            url: calling.base_url.clone(),
            ..live
        }),
        asking,
    )
    .await;

    assert!(joined.failed().is_none(), "{:?}", joined.failed());
    assert_eq!(
        joined.tool_calls(),
        vec![(
            "builtin.fs.read".to_owned(),
            serde_json::json!({"path": "README.md"})
        )],
        "the fragments must join into one usable call: {:?}",
        joined.events
    );
}
