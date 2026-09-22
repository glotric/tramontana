//! The managed local adapter obeys the model provider contract.
//!
//! `BUILD_ORDER.md` §17 asks for the provider contract suites across the fake,
//! OpenAI-compatible and managed local adapters. The managed adapter is a
//! supervised runtime reached through the same chat-completions adapter every
//! other endpoint uses, which is how composition assembles it
//! (`assemble_models`). Here the runtime is the stand-in `llama-server`: a real
//! process on a real port, started and stopped by the real supervisor.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tramontana_adapters_local::{LlamaServerSpec, SupervisedLlamaServer};
use tramontana_adapters_model::{OpenAiCompatibleProvider, ProviderAuth};
use tramontana_core::ports::ModelProvider;
use tramontana_core::testing::contract_suites::{ProviderScenario, model_provider_contract};

fn fake_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tramontana-fake-llama-server"))
}

#[tokio::test]
async fn a_supervised_runtime_obeys_the_model_provider_contract() {
    let directory = tempfile::tempdir().unwrap();
    let model = directory.path().join("model.gguf");
    std::fs::write(&model, b"not a real model").unwrap();
    // Every runtime the suite starts, stopped when the test ends rather than
    // leaked with the provider that uses it.
    let runtimes: Arc<Mutex<Vec<SupervisedLlamaServer>>> = Arc::new(Mutex::new(Vec::new()));

    model_provider_contract(|scenario| {
        let model = model.clone();
        let runtimes = Arc::clone(&runtimes);
        async move {
            let mut spec = LlamaServerSpec::new(fake_binary(), model);
            spec.readiness_timeout = Duration::from_secs(10);
            spec.extra_args = match scenario {
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
            let runtime = SupervisedLlamaServer::start(&spec).await.unwrap();
            let provider = OpenAiCompatibleProvider::new(
                "local",
                runtime.base_url(),
                OpenAiCompatibleProvider::local_descriptor("local", "test-model", "Test model"),
                // As composition connects: with the key the runtime was started
                // with, which it now requires of every model request.
                ProviderAuth::Session(runtime.api_key().unwrap().to_owned()),
            )
            .unwrap();
            runtimes.lock().unwrap().push(runtime);
            Arc::new(provider) as Arc<dyn ModelProvider>
        }
    })
    .await;

    // Dropping a runtime stops its whole process group.
    drop(std::mem::take(&mut *runtimes.lock().unwrap()));
}
