//! The fake `ModelProvider` runs the shared provider contract suite.
//!
//! The suite lives in `tramontana_core::testing::contract_suites` so that every
//! real adapter runs the identical cases. A divergence between the fake every
//! other test uses and a real provider is a defect in one of them, never a
//! difference in what was tested.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::ports::ModelProvider;
use tramontana_core::testing::FakeModelProvider;
use tramontana_core::testing::contract_suites::{ProviderScenario, model_provider_contract};

#[tokio::test]
async fn the_fake_provider_obeys_the_model_provider_contract() {
    model_provider_contract(|scenario| async move {
        match scenario {
            ProviderScenario::Streams(chunks) => {
                Arc::new(FakeModelProvider::streaming(chunks)) as Arc<dyn ModelProvider>
            }
            ProviderScenario::Fails => Arc::new(FakeModelProvider::failing(
                tramontana_core::contracts::ErrorCode::ProviderUnavailable,
            )) as Arc<dyn ModelProvider>,
            // Slow enough that a cancel always lands inside the stream rather
            // than after it, without anyone waiting out the pause: the fake
            // races the sleep against cancellation.
            ProviderScenario::Slow => Arc::new(FakeModelProvider::streaming_paced(
                ["one", "two", "three"],
                60_000,
            )) as Arc<dyn ModelProvider>,
        }
    })
    .await;
}
