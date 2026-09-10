//! Configured endpoints become live providers, or legible failures.
//!
//! The property that matters most is the one about partial failure: a profile
//! with one stale runtime path must still be able to use its other models.
//! An assembly that refused to start would make a moved binary look like a
//! broken application.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_composition::{Supervision, assemble_models};
use tramontana_core::contracts::{
    ModelLocation, ProviderConfig, ProviderEndpoint, SecretRef, SecretStore, Timestamp,
};
use tramontana_core::ports::SecretResolver;
use tramontana_core::testing::InMemorySecretVault;

fn at() -> Timestamp {
    Timestamp::new("2026-01-01T00:00:00.000Z")
}

fn resolver() -> Arc<dyn SecretResolver> {
    Arc::new(InMemorySecretVault::new())
}

fn openai(id: &str, base_url: &str) -> ProviderEndpoint {
    ProviderEndpoint::new(
        id,
        format!("Endpoint {id}"),
        ProviderConfig::OpenAiCompatible {
            base_url: base_url.to_owned(),
            model_id: "test-model".to_owned(),
            credential: None,
        },
        at(),
    )
}

#[tokio::test]
async fn a_configured_endpoint_becomes_a_provider_that_claims_its_own_identity() {
    // The identifier the user edited is the identifier a routing decision
    // records, so a decision can be traced back to the row that caused it.
    let plane = assemble_models(
        &[openai("my-llama", "http://localhost:8080")],
        &resolver(),
        Supervision::Start,
    )
    .await;

    assert_eq!(plane.providers.len(), 1);
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
    assert_eq!(plane.providers[0].id().to_string(), "my-llama");

    let models = plane.providers[0].list_models().await;
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].provider_id, "my-llama");
    assert_eq!(models[0].model_id, "test-model");
    assert_eq!(models[0].location, ModelLocation::Local);
}

#[tokio::test]
async fn location_follows_the_url_rather_than_anything_declared() {
    // A settings row cannot mark itself local and slip past Local Only.
    let plane = assemble_models(
        &[
            openai("local", "http://127.0.0.1:8080"),
            openai("remote", "https://api.example.test"),
        ],
        &resolver(),
        Supervision::Start,
    )
    .await;

    let located: Vec<(String, ModelLocation)> = {
        let mut pairs = Vec::new();
        for provider in &plane.providers {
            for model in provider.list_models().await {
                pairs.push((model.provider_id, model.location));
            }
        }
        pairs
    };
    assert_eq!(
        located,
        vec![
            ("local".to_owned(), ModelLocation::Local),
            ("remote".to_owned(), ModelLocation::Cloud),
        ]
    );
}

#[tokio::test]
async fn a_disabled_endpoint_is_not_brought_up_and_is_not_a_failure() {
    // Turning a cloud provider off for a while is a normal thing to want, and
    // it should cost neither the configuration nor a spurious error.
    let mut endpoint = openai("off", "https://api.example.test");
    endpoint.enabled = false;
    let plane = assemble_models(&[endpoint], &resolver(), Supervision::Start).await;

    assert!(plane.providers.is_empty());
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
}

#[tokio::test]
async fn one_broken_endpoint_does_not_take_the_others_with_it() {
    // The property this whole shape exists for. `http://` to a remote host is
    // refused by the endpoint policy (ADR 0009), which stands in here for any
    // endpoint that cannot be brought up.
    let plane = assemble_models(
        &[
            openai("good", "http://localhost:8080"),
            openai("broken", "http://api.example.test"),
            openai("also-good", "https://api.example.test"),
        ],
        &resolver(),
        Supervision::Start,
    )
    .await;

    assert_eq!(plane.providers.len(), 2, "a working endpoint was dropped");
    assert_eq!(plane.failures.len(), 1);
    assert_eq!(plane.failures[0].endpoint_id, "broken");
    // Named, so the message can say which endpoint rather than "a provider".
    assert_eq!(plane.failures[0].display_name, "Endpoint broken");
    assert!(
        plane.failures[0].error.message.contains("loopback"),
        "{}",
        plane.failures[0].error.message
    );
}

#[tokio::test]
async fn an_anthropic_endpoint_carries_its_credential_by_reference_only() {
    let endpoint = ProviderEndpoint::new(
        "anthropic",
        "Anthropic",
        ProviderConfig::Anthropic {
            base_url: "https://api.anthropic.com".to_owned(),
            model_id: "claude-test".to_owned(),
            credential: SecretRef {
                key: "anthropic.apiKey".to_owned(),
                store: SecretStore::Platform,
            },
        },
        at(),
    );
    let plane = assemble_models(&[endpoint], &resolver(), Supervision::Start).await;

    assert_eq!(plane.providers.len(), 1, "{:?}", plane.failures);
    let models = plane.providers[0].list_models().await;
    assert_eq!(models[0].location, ModelLocation::Cloud);
    // The provider holds a reference and a resolver, never a value.
    let rendered = format!("{:?}", plane.providers[0].id());
    assert!(rendered.contains("anthropic"), "{rendered}");
}

#[tokio::test]
async fn a_supervised_runtime_whose_binary_is_missing_fails_without_stopping_the_rest() {
    let directory = tempfile::tempdir().unwrap();
    let model = directory.path().join("model.gguf");
    std::fs::write(&model, b"not a real model").unwrap();

    let broken = ProviderEndpoint::new(
        "llama",
        "Local llama",
        ProviderConfig::LlamaServer {
            binary_path: directory.path().join("absent").display().to_string(),
            model_path: model.display().to_string(),
            model_id: "m".to_owned(),
            extra_args: Vec::new(),
        },
        at(),
    );

    let mut plane = assemble_models(
        &[broken, openai("good", "http://localhost:8080")],
        &resolver(),
        Supervision::Start,
    )
    .await;

    assert_eq!(plane.providers.len(), 1);
    assert_eq!(plane.failures.len(), 1);
    assert_eq!(plane.failures[0].endpoint_id, "llama");
    plane.shutdown().await;
}
