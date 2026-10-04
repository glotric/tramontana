//! The OpenAI-compatible adapter, over a real socket.

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
use tramontana_adapters_model::{OpenAiCompatibleProvider, ProviderAuth};
use tramontana_core::contracts::{
    ErrorCode, ErrorEnvelope, ModelEvent, SecretRef, SecretStore, StopReason,
};
use tramontana_core::ports::{ModelProvider, SecretResolver};
use tramontana_core::testing::contract_suites::{
    ProviderScenario, model_provider_contract, model_request,
};
use tramontana_core::testing::release::locally_authorized;

/// The one place a test's secret value exists.
#[derive(Debug)]
struct FixedSecret(String);

#[async_trait]
impl SecretResolver for FixedSecret {
    async fn resolve(&self, _reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        Ok(self.0.clone())
    }
}

fn provider(endpoint: &FakeEndpoint, auth: ProviderAuth) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        "local",
        &endpoint.base_url,
        OpenAiCompatibleProvider::local_descriptor("local", "test-model", "Test model"),
        auth,
    )
    .unwrap()
}

async fn drain(provider: &OpenAiCompatibleProvider, cancel: CancellationToken) -> Vec<ModelEvent> {
    let mut stream = provider
        .stream(locally_authorized(model_request("request-1")), cancel)
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
    // The same cases the in-process fake passes. A divergence between the two
    // is a defect in one of them, not a difference in what was tested.
    model_provider_contract(|scenario| async move {
        let behavior = match scenario {
            ProviderScenario::Streams(chunks) => Behavior::Streams(chunks),
            ProviderScenario::Fails => Behavior::Status(503),
            // Headers arrive and content never does, so a cancel always lands
            // inside the stream rather than after it.
            ProviderScenario::Slow => Behavior::Silent,
        };
        let endpoint = FakeEndpoint::start(behavior).await;
        let provider = provider(&endpoint, ProviderAuth::None);
        // The suite owns only the provider, so the endpoint has to outlive
        // this scope or its listener closes before the suite connects. Leaked
        // deliberately: the process is a test binary that is about to exit,
        // and threading a lifetime through the suite would make every harness
        // carry a detail that only this one needs.
        Box::leak(Box::new(endpoint));
        Arc::new(provider) as Arc<dyn ModelProvider>
    })
    .await;
}

#[tokio::test]
async fn a_credential_travels_in_a_header_and_appears_in_no_url() {
    // `BUILD_ORDER.md` §14: secrets absent from rows, events, diagnostics,
    // process arguments, and URLs.
    const TOKEN: &str = "sk-test-do-not-log-me";
    let endpoint = FakeEndpoint::start(Behavior::Streams(vec!["ok".into()])).await;
    let provider = provider(
        &endpoint,
        ProviderAuth::Bearer {
            reference: SecretRef {
                key: "openai.default".into(),
                store: SecretStore::Platform,
            },
            resolver: Arc::new(FixedSecret(TOKEN.to_owned())),
        },
    );

    drain(&provider, CancellationToken::new()).await;

    let received = endpoint.received();
    let request = received.first().expect("the endpoint was never called");
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str()),
        "the credential did not travel as a bearer header"
    );
    assert!(
        !request.request_line.contains(TOKEN),
        "the credential appeared in the request line: {}",
        request.request_line
    );
    assert!(
        !request.body.contains(TOKEN),
        "the credential appeared in the request body"
    );

    // And it is not reachable from the provider itself, which is what ends up
    // in a log line or a panic message.
    let rendered = format!("{provider:?}");
    assert!(
        !rendered.contains(TOKEN),
        "debug output leaked the credential"
    );
    assert!(
        !format!(
            "{:?}",
            ProviderAuth::Bearer {
                reference: SecretRef {
                    key: "k".into(),
                    store: SecretStore::Platform
                },
                resolver: Arc::new(FixedSecret(TOKEN.to_owned())),
            }
        )
        .contains(TOKEN)
    );
}

#[tokio::test]
async fn no_credential_is_sent_when_none_is_configured() {
    // A local endpoint needs none, and sending an empty authorization header
    // would make some servers reject the request outright.
    let endpoint = FakeEndpoint::start(Behavior::Streams(vec!["ok".into()])).await;
    drain(
        &provider(&endpoint, ProviderAuth::None),
        CancellationToken::new(),
    )
    .await;

    let received = endpoint.received();
    assert!(received[0].header("authorization").is_none());
}

#[tokio::test]
async fn usage_and_finish_reason_are_reported() {
    let endpoint = FakeEndpoint::start(Behavior::Streams(vec!["a".into(), "b".into()])).await;
    let events = drain(
        &provider(&endpoint, ProviderAuth::None),
        CancellationToken::new(),
    )
    .await;

    let usage = events
        .iter()
        .find_map(|event| match event {
            ModelEvent::Usage { usage } => Some(*usage),
            _ => None,
        })
        .expect("usage was not reported");
    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(3));
    assert!(
        !usage.estimated,
        "counted usage must not be marked estimated"
    );

    assert!(matches!(
        events.last(),
        Some(ModelEvent::Completed {
            stop_reason: StopReason::Stop
        })
    ));
}

#[tokio::test]
async fn a_length_stop_and_a_tool_call_stop_are_distinguished() {
    // A caller has to tell "the model finished" from "the model ran out of
    // room" from "the model wants a tool", and only the first is an answer.
    for (wire, expected) in [
        ("length", StopReason::Length),
        ("tool_calls", StopReason::ToolCall),
        ("stop", StopReason::Stop),
    ] {
        let endpoint = FakeEndpoint::start(Behavior::Raw(vec![
            serde_json::json!({ "choices": [{ "delta": { "content": "x" } }] }).to_string(),
            serde_json::json!({ "choices": [{ "delta": {}, "finish_reason": wire }] }).to_string(),
            "[DONE]".to_owned(),
        ]))
        .await;
        let events = drain(
            &provider(&endpoint, ProviderAuth::None),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(
            events.last(),
            Some(&ModelEvent::Completed {
                stop_reason: expected
            }),
            "{wire} was not mapped to {expected:?}"
        );
    }
}

#[tokio::test]
async fn a_tool_call_arrives_as_deltas_with_a_stable_call_identity() {
    // Providers send the id once and identify continuations by index, so a
    // naive reader assembles two calls where there was one.
    let endpoint = FakeEndpoint::start(Behavior::Raw(vec![
        serde_json::json!({ "choices": [{ "delta": { "tool_calls": [
            { "id": "call-abc", "index": 0, "function": { "name": "builtin.fs.read", "arguments": "{\"pa" } }
        ] } }] })
        .to_string(),
        serde_json::json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "function": { "arguments": "th\":\".\"}" } }
        ] } }] })
        .to_string(),
        serde_json::json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] })
            .to_string(),
        "[DONE]".to_owned(),
    ]))
    .await;

    let events = drain(
        &provider(&endpoint, ProviderAuth::None),
        CancellationToken::new(),
    )
    .await;
    let deltas: Vec<(u64, Option<String>, Option<String>, String)> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ToolCallDelta {
                call_index,
                call_id,
                name,
                delta,
                ..
            } => Some((*call_index, call_id.clone(), name.clone(), delta.clone())),
            _ => None,
        })
        .collect();

    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].0, deltas[1].0);
    assert_eq!(deltas[0].1.as_deref(), Some("call-abc"));
    assert_eq!(deltas[0].2.as_deref(), Some("builtin.fs.read"));
    assert_eq!(deltas[1].1, None);
    assert_eq!(deltas[1].2, None);
    let assembled: String = deltas
        .iter()
        .map(|(_, _, _, delta)| delta.as_str())
        .collect();
    assert_eq!(assembled, "{\"path\":\".\"}");
    let mut turn = tramontana_core::services::ModelTurnAccumulator::new(
        "request-1".into(),
        tramontana_core::contracts::CorrelationId::new("test"),
        Default::default(),
    );
    for event in &events {
        turn.push(event).unwrap();
    }
    let calls = turn.finish().unwrap().tool_calls;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call-abc");
    assert_eq!(calls[0].name, "builtin.fs.read");
    assert_eq!(calls[0].arguments, serde_json::json!({"path":"."}));
}

#[tokio::test]
async fn malformed_tool_indices_and_fragment_types_fail_closed() {
    for call in [
        serde_json::json!({"id":"id","function":{"name":"tool","arguments":"{}"}}),
        serde_json::json!({"index":-1,"id":"id","function":{"name":"tool","arguments":"{}"}}),
        serde_json::json!({"index":0,"id":7,"function":{"name":"tool","arguments":"{}"}}),
        serde_json::json!({"index":0,"id":"id","function":{"name":"tool","arguments":{}}}),
    ] {
        let endpoint = FakeEndpoint::start(Behavior::Raw(vec![
            serde_json::json!({"choices":[{"delta":{"tool_calls":[call]}}]}).to_string(),
            "[DONE]".into(),
        ]))
        .await;
        let events = drain(
            &provider(&endpoint, ProviderAuth::None),
            CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(events.last(), Some(ModelEvent::Failed {error}) if error.code == tramontana_core::contracts::ErrorCode::ProtocolError)
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ModelEvent::Completed { .. }))
        );
    }
}

#[tokio::test]
async fn each_http_failure_maps_to_the_error_a_caller_can_act_on() {
    // The distinction that matters is whether retrying helps and whether the
    // user has to do something.
    for (status, expected) in [
        (401, ErrorCode::ProviderAuthentication),
        (403, ErrorCode::ProviderAuthentication),
        (429, ErrorCode::ProviderRateLimited),
        (400, ErrorCode::InvalidRequest),
        (500, ErrorCode::ProviderUnavailable),
        (503, ErrorCode::ProviderUnavailable),
    ] {
        let endpoint = FakeEndpoint::start(Behavior::Status(status)).await;
        let events = drain(
            &provider(&endpoint, ProviderAuth::None),
            CancellationToken::new(),
        )
        .await;

        match events.last() {
            Some(ModelEvent::Failed { error }) => {
                assert_eq!(error.code, expected, "{status} mapped to {:?}", error.code);
            }
            other => panic!("{status} did not end in a failure: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_truncated_stream_ends_once_and_keeps_what_arrived() {
    // The connection drops mid-object. What was received is still the model's
    // output and is kept; the stream still ends exactly once.
    let endpoint = FakeEndpoint::start(Behavior::Truncated).await;
    let events = drain(
        &provider(&endpoint, ProviderAuth::None),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        events.iter().filter(|event| event.is_terminal()).count(),
        1,
        "a truncated stream must still end exactly once: {events:?}"
    );
}

#[tokio::test]
async fn a_deadline_ends_a_silent_endpoint_rather_than_hanging() {
    let endpoint = FakeEndpoint::start(Behavior::Silent).await;
    let provider = provider(&endpoint, ProviderAuth::None);

    let mut request = model_request("request-1");
    request.limits.deadline_ms = 150;
    let mut stream = provider
        .stream(locally_authorized(request), CancellationToken::new())
        .await
        .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
        .await
        .expect("the deadline did not end the stream");
    match event {
        Some(ModelEvent::Failed { error }) => assert_eq!(error.code, ErrorCode::Timeout),
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn an_endpoint_that_is_not_a_url_is_refused_at_construction() {
    // Failing here rather than at the first request means a misconfiguration
    // is visible when it is made, not when a task is already running.
    let error = OpenAiCompatibleProvider::new(
        "local",
        "127.0.0.1:8080",
        OpenAiCompatibleProvider::local_descriptor("local", "m", "M"),
        ProviderAuth::None,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn an_https_endpoint_is_accepted_now_that_there_is_a_tls_stack() {
    // Until `docs/decisions/0009-tls-for-cloud-adapters.md` this was refused
    // with `capabilityUnsupported`, because the build carried no TLS at all.
    OpenAiCompatibleProvider::new(
        "openai",
        "https://api.openai.com",
        OpenAiCompatibleProvider::local_descriptor("openai", "m", "M"),
        ProviderAuth::None,
    )
    .expect("an https endpoint was refused");
}

#[tokio::test]
async fn a_remote_endpoint_over_plain_http_is_refused_before_a_key_can_leave() {
    // The restriction used to be implicit: with no TLS, no remote host was
    // reachable at all. Now that one is, plain http to anything but loopback
    // has to be refused, or a mistyped scheme sends the key in the clear.
    let error = OpenAiCompatibleProvider::new(
        "openai",
        "http://api.openai.com",
        OpenAiCompatibleProvider::local_descriptor("openai", "m", "M"),
        ProviderAuth::None,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(error.message.contains("loopback"), "{}", error.message);
}

#[tokio::test]
async fn an_unreachable_endpoint_is_a_provider_failure_without_echoing_the_url() {
    // A base URL can carry a query string pasted from a provider console, and
    // the transport error's own `Display` includes the URL.
    let provider = OpenAiCompatibleProvider::new(
        "local",
        "http://127.0.0.1:1/secret-path?token=leak-me",
        OpenAiCompatibleProvider::local_descriptor("local", "m", "M"),
        ProviderAuth::None,
    )
    .unwrap();

    let events = drain(&provider, CancellationToken::new()).await;
    match events.last() {
        Some(ModelEvent::Failed { error }) => {
            assert_eq!(error.code, ErrorCode::ProviderUnavailable);
            assert!(
                !error.message.contains("leak-me"),
                "the error echoed the endpoint URL: {}",
                error.message
            );
        }
        other => panic!("expected a provider failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_server_that_needs_another_name_for_the_model_is_sent_that_name() {
    // `mlx_lm.server` loads whatever `model` names — from the network when
    // it is not a local path — and calls the model it was started with
    // `default_model`. The model keeps its own identifier everywhere else.
    let endpoint = FakeEndpoint::start(Behavior::Streams(vec!["ok".into()])).await;
    let provider = provider(&endpoint, ProviderAuth::None).with_wire_model("default_model");
    drain(&provider, CancellationToken::new()).await;
    let body: serde_json::Value = serde_json::from_str(&endpoint.received()[0].body).unwrap();
    assert_eq!(body["model"], "default_model");
    assert_eq!(provider.list_models().await[0].model_id, "test-model");

    // Without it, the model's own identifier.
    let endpoint = FakeEndpoint::start(Behavior::Streams(vec!["ok".into()])).await;
    drain(&provider_of(&endpoint), CancellationToken::new()).await;
    let body: serde_json::Value = serde_json::from_str(&endpoint.received()[0].body).unwrap();
    assert_eq!(body["model"], "test-model");
}

fn provider_of(endpoint: &FakeEndpoint) -> OpenAiCompatibleProvider {
    provider(endpoint, ProviderAuth::None)
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    // The approved endpoint answers 307 to another host. Following it would
    // send the credential and the prompt somewhere nobody approved (ADR 0066).
    let elsewhere = FakeEndpoint::start(Behavior::Streams(vec!["leaked".into()])).await;
    let endpoint = FakeEndpoint::start(Behavior::Redirect(format!(
        "{}/v1/chat/completions",
        elsewhere.base_url
    )))
    .await;
    let provider = provider(&endpoint, ProviderAuth::None);

    let events = match provider
        .stream(
            locally_authorized(model_request("request-1")),
            CancellationToken::new(),
        )
        .await
    {
        Ok(mut stream) => {
            let mut events = Vec::new();
            while let Some(event) = stream.recv().await {
                events.push(event);
            }
            events
        }
        Err(_) => Vec::new(),
    };

    assert_eq!(
        endpoint.received().len(),
        1,
        "the approved endpoint was asked once"
    );
    assert!(
        elsewhere.received().is_empty(),
        "the request followed the redirect to another host"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::Completed { .. })),
        "a redirect was reported as an answer: {events:?}"
    );
}
