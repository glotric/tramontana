//! One credential-store contract, run against the fake and the real store.
//!
//! `TESTING_STRATEGY.md` §4 and `docs/decisions/0008-platform-credential-store.md`.
//! The in-memory vault and the platform vault run identical cases, so a
//! divergence is a defect in one of them rather than a difference in what was
//! tested.
//!
//! The real store's run needs a credential store to exist. A CI runner has no
//! desktop session and Linux Secret Service has no provider there, so that run
//! reports that it was skipped rather than passing silently — a test that
//! quietly does nothing is worse than one that is absent, because it looks
//! like coverage.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::testing::InMemorySecretVault;
use tramontana_core::testing::contract_suites::{
    secret_vault_contract, unavailable_store_contract,
};
use tramontana_platform::PlatformSecretVault;

/// The namespace the real-store run uses.
///
/// Never `dev.tramontana`: a test must not be able to read or delete the
/// credentials a developer actually uses.
const TEST_SERVICE: &str = "dev.tramontana.contract-test";

#[tokio::test]
async fn the_in_memory_vault_obeys_the_contract() {
    secret_vault_contract(&InMemorySecretVault::new()).await;
}

#[tokio::test]
async fn an_unreachable_store_is_never_read_as_an_empty_one() {
    unavailable_store_contract(&InMemorySecretVault::unavailable()).await;
}

#[tokio::test]
async fn the_platform_vault_obeys_the_same_contract() {
    let vault = PlatformSecretVault::under_service(TEST_SERVICE);
    if !vault.is_available() {
        // Said out loud rather than silently returning. `cargo test -- --nocapture`
        // shows it, and the M5 evidence note records that CI does not run it.
        println!(
            "SKIPPED: no platform credential store is reachable here \
             (expected on CI and on a headless Linux session)"
        );
        return;
    }
    secret_vault_contract(&vault).await;
}
