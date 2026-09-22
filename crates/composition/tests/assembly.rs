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
            provenance: None,
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

/// An MLX endpoint whose runtime is the stand-in `llama-server`, which
/// answers the same `/health` and chat-completions requests.
fn mlx(directory: &std::path::Path, binary: &std::path::Path) -> ProviderEndpoint {
    let folder = directory.join("mlx-model");
    std::fs::create_dir_all(&folder).unwrap();
    ProviderEndpoint::new(
        "mlx",
        "Local MLX",
        ProviderConfig::MlxServer {
            binary_path: binary.display().to_string(),
            model_path: folder.display().to_string(),
            model_id: "qwen-mlx".to_owned(),
            extra_args: Vec::new(),
            provenance: None,
        },
        at(),
    )
}

/// The stand-in runtime, built beside this test by a workspace build.
fn fake_llama_server() -> std::path::PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the test binary lives under the target directory")
        .join(format!(
            "tramontana-fake-llama-server{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        path.exists(),
        "{} is missing; build the workspace (`cargo test --workspace`) first",
        path.display()
    );
    path
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn an_mlx_endpoint_becomes_a_local_provider_on_apple_silicon() {
    let directory = tempfile::tempdir().unwrap();
    let mut plane = assemble_models(
        &[mlx(directory.path(), &fake_llama_server())],
        &resolver(),
        Supervision::Start,
    )
    .await;

    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
    assert_eq!(plane.providers.len(), 1);
    let models = plane.providers[0].list_models().await;
    assert_eq!(models[0].model_id, "qwen-mlx");
    assert_eq!(models[0].location, ModelLocation::Local);
    plane.shutdown().await;
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[tokio::test]
async fn an_mlx_endpoint_is_unavailable_elsewhere_and_the_rest_still_start() {
    let directory = tempfile::tempdir().unwrap();
    let mut plane = assemble_models(
        &[
            mlx(directory.path(), &fake_llama_server()),
            openai("good", "http://localhost:8080"),
        ],
        &resolver(),
        Supervision::Start,
    )
    .await;

    assert_eq!(plane.providers.len(), 1);
    assert_eq!(plane.failures.len(), 1);
    assert_eq!(plane.failures[0].endpoint_id, "mlx");
    assert!(
        plane.failures[0].error.message.contains("Apple Silicon"),
        "{:?}",
        plane.failures[0]
    );
    plane.shutdown().await;
}

#[tokio::test]
async fn an_mlx_endpoint_is_skipped_like_any_supervised_runtime_when_asked() {
    let directory = tempfile::tempdir().unwrap();
    let plane = assemble_models(
        &[mlx(directory.path(), &fake_llama_server())],
        &resolver(),
        Supervision::Skip,
    )
    .await;
    assert!(plane.providers.is_empty());
    assert_eq!(plane.skipped, ["mlx"]);
}

/// A copy of the stand-in runtime this test may change, as an upgrade would.
fn own_copy_of_the_runtime(directory: &std::path::Path) -> std::path::PathBuf {
    let copy = directory.join(format!("llama-server{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(fake_llama_server(), &copy).unwrap();
    copy
}

fn recorded_llama(
    directory: &std::path::Path,
    binary: &std::path::Path,
    provenance: Option<tramontana_core::contracts::RuntimeProvenance>,
) -> ProviderEndpoint {
    let model = directory.join("model.gguf");
    std::fs::write(&model, b"not a real model").unwrap();
    ProviderEndpoint::new(
        "llama",
        "Local llama",
        ProviderConfig::LlamaServer {
            binary_path: binary.display().to_string(),
            model_path: model.display().to_string(),
            model_id: "m".to_owned(),
            extra_args: Vec::new(),
            provenance,
        },
        at(),
    )
}

#[tokio::test]
async fn a_runtime_whose_executable_changed_is_held_until_confirmed() {
    let directory = tempfile::tempdir().unwrap();
    let binary = own_copy_of_the_runtime(directory.path());
    let model = directory.path().join("model.gguf");
    std::fs::write(&model, b"not a real model").unwrap();
    let recorded = tramontana_composition::record_provenance(&binary, &model, at())
        .await
        .unwrap();

    // As recorded: it starts.
    let mut plane = assemble_models(
        &[recorded_llama(
            directory.path(),
            &binary,
            Some(recorded.clone()),
        )],
        &resolver(),
        Supervision::Start,
    )
    .await;
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
    plane.shutdown().await;

    // Upgraded, or replaced: it is held, the others still start, and the
    // reason says what to do.
    std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
    let mut plane = assemble_models(
        &[
            recorded_llama(directory.path(), &binary, Some(recorded)),
            openai("good", "http://localhost:8080"),
        ],
        &resolver(),
        Supervision::Start,
    )
    .await;
    assert_eq!(plane.providers.len(), 1);
    assert_eq!(plane.failures.len(), 1);
    let error = &plane.failures[0].error;
    assert_eq!(
        error.code,
        tramontana_core::contracts::ErrorCode::PermissionDenied
    );
    assert!(
        error.message.contains("changed since you added"),
        "{error:?}"
    );
    assert!(error.message.contains("provider trust llama"), "{error:?}");
    plane.shutdown().await;
}

#[tokio::test]
async fn a_runtime_added_before_records_were_kept_still_starts() {
    let directory = tempfile::tempdir().unwrap();
    let binary = own_copy_of_the_runtime(directory.path());
    let mut plane = assemble_models(
        &[recorded_llama(directory.path(), &binary, None)],
        &resolver(),
        Supervision::Start,
    )
    .await;
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
    plane.shutdown().await;
}

#[tokio::test]
async fn files_are_checked_against_their_record() {
    use tramontana_composition::{FileCheck, check_executable, check_model, record_provenance};
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("server");
    let model = directory.path().join("model.gguf");
    std::fs::write(&binary, b"one").unwrap();
    std::fs::write(&model, b"weights").unwrap();
    let recorded = record_provenance(&binary, &model, at()).await.unwrap();
    assert_eq!(recorded.executable_bytes, 3);
    assert_eq!(recorded.model_bytes, 7);

    assert_eq!(
        check_executable(&binary, None).await,
        FileCheck::NotRecorded
    );
    assert_eq!(
        check_executable(&binary, Some(&recorded)).await,
        FileCheck::Unchanged
    );
    assert_eq!(
        check_model(&model, Some(&recorded)).await,
        FileCheck::Unchanged
    );
    std::fs::write(&model, b"other weights").unwrap();
    assert_eq!(
        check_model(&model, Some(&recorded)).await,
        FileCheck::Changed
    );
    std::fs::remove_file(&binary).unwrap();
    assert_eq!(
        check_executable(&binary, Some(&recorded)).await,
        FileCheck::Unreadable
    );
    assert!(record_provenance(&binary, &model, at()).await.is_err());
}

/// A supervised runtime, assembled as both applications assemble it, obeys
/// the model provider contract. The runtime takes only its own key, so this
/// is also what shows composition hands the provider that key.
#[tokio::test]
async fn a_supervised_runtime_as_assembled_obeys_the_provider_contract() {
    use std::sync::{Arc, Mutex};
    use tramontana_core::testing::contract_suites::{ProviderScenario, model_provider_contract};

    let directory = tempfile::tempdir().unwrap();
    let model = directory.path().join("model.gguf");
    std::fs::write(&model, b"not a real model").unwrap();
    let planes = Arc::new(Mutex::new(Vec::new()));

    model_provider_contract(|scenario| {
        let model = model.clone();
        let planes = Arc::clone(&planes);
        async move {
            let extra_args = match scenario {
                ProviderScenario::Streams(chunks) => {
                    let mut args = vec!["--fake-chat".to_owned(), "streams".to_owned()];
                    for chunk in chunks {
                        args.extend(["--fake-chunk".to_owned(), chunk]);
                    }
                    args
                }
                ProviderScenario::Fails => vec!["--fake-chat".to_owned(), "fails".to_owned()],
                ProviderScenario::Slow => vec!["--fake-chat".to_owned(), "silent".to_owned()],
            };
            let endpoint = ProviderEndpoint::new(
                "local",
                "Local",
                ProviderConfig::LlamaServer {
                    binary_path: fake_llama_server().display().to_string(),
                    model_path: model.display().to_string(),
                    model_id: "test-model".to_owned(),
                    extra_args,
                    provenance: None,
                },
                at(),
            );
            let plane = assemble_models(&[endpoint], &resolver(), Supervision::Start).await;
            assert!(plane.failures.is_empty(), "{:?}", plane.failures);
            let provider = Arc::clone(&plane.providers[0]);
            planes.lock().unwrap().push(plane);
            provider
        }
    })
    .await;

    let planes = std::mem::take(&mut *planes.lock().unwrap());
    for mut plane in planes {
        plane.shutdown().await;
    }
}

#[tokio::test]
async fn a_started_runtimes_accelerators_are_kept_for_its_endpoint() {
    let directory = tempfile::tempdir().unwrap();
    let binary = own_copy_of_the_runtime(directory.path());
    let mut plane = assemble_models(
        &[
            recorded_llama(directory.path(), &binary, None),
            openai("good", "http://localhost:8080"),
        ],
        &resolver(),
        Supervision::Start,
    )
    .await;
    assert_eq!(
        plane.accelerators("llama"),
        Some(&["Stand-in GPU (FAKE0, 1 GB)".to_owned()][..])
    );
    // An endpoint Tramontana only points at reports nothing of the kind.
    assert_eq!(plane.accelerators("good"), None);
    plane.shutdown().await;
}

/// A real `mlx_lm.server`, assembled as both applications assemble it.
///
/// Runs only when `TRAMONTANA_LIVE_MLX_SERVER` names an `mlx_lm.server` and
/// `TRAMONTANA_LIVE_MLX_MODEL` an MLX model folder; otherwise it says so and
/// passes, as the other live variants in this repository do.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn a_real_mlx_runtime_answers_and_stops() {
    use tramontana_core::contracts::{ModelEvent, StopReason};
    use tramontana_core::testing::contract_suites::model_request;
    use tramontana_core::testing::release::locally_authorized;

    let (Ok(server), Ok(model)) = (
        std::env::var("TRAMONTANA_LIVE_MLX_SERVER"),
        std::env::var("TRAMONTANA_LIVE_MLX_MODEL"),
    ) else {
        eprintln!("skipped: set TRAMONTANA_LIVE_MLX_SERVER and TRAMONTANA_LIVE_MLX_MODEL");
        return;
    };
    let endpoint = ProviderEndpoint::new(
        "mlx",
        "Local MLX",
        ProviderConfig::MlxServer {
            binary_path: server,
            model_path: model,
            model_id: "qwen-mlx".to_owned(),
            extra_args: Vec::new(),
            provenance: None,
        },
        at(),
    );
    let mut plane = assemble_models(&[endpoint], &resolver(), Supervision::Start).await;
    assert!(plane.failures.is_empty(), "{:?}", plane.failures);
    assert_eq!(
        plane.accelerators("mlx"),
        Some(&["Apple silicon GPU (MLX)".to_owned()][..])
    );

    // It answers, through the same adapter as every other endpoint, asked
    // for its model as `default_model` rather than by a name it would try to
    // download.
    let mut stream = plane.providers[0]
        .stream(
            locally_authorized(model_request("live-1")),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut text = String::new();
    let mut stopped = None;
    while let Some(event) = stream.recv().await {
        match event {
            ModelEvent::TextDelta { text: delta, .. } => text.push_str(&delta),
            ModelEvent::Completed { stop_reason } => stopped = Some(stop_reason),
            ModelEvent::Failed { error } => panic!("the model failed: {error:?}"),
            _ => {}
        }
    }
    assert!(!text.trim().is_empty(), "no text came back");
    assert!(
        matches!(stopped, Some(StopReason::Stop | StopReason::Length)),
        "{stopped:?}"
    );

    // Cancelled mid-answer, it ends promptly and once.
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = plane.providers[0]
        .stream(locally_authorized(model_request("live-2")), cancel.clone())
        .await
        .unwrap();
    let _first = stream.recv().await;
    cancel.cancel();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut terminal = 0;
        while let Some(event) = stream.recv().await {
            if matches!(
                event,
                ModelEvent::Completed { .. } | ModelEvent::Failed { .. }
            ) {
                terminal += 1;
            }
        }
        terminal
    })
    .await
    .expect("a cancelled request did not end");
    assert_eq!(ended, 1);

    plane.shutdown().await;
}
