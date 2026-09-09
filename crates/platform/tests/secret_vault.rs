//! One credential-store contract, run against the fake and the real store.
//!
//! `TESTING_STRATEGY.md` §4 and `docs/decisions/0008-platform-credential-store.md`.
//! The in-memory vault and the platform vault run identical cases, so a
//! divergence is a defect in one of them rather than a difference in what was
//! tested.
//!
//! The real store's run needs a credential store to exist. CI provides one on
//! all three platforms — Linux starts `gnome-keyring` on a session bus — and
//! sets `TRAMONTANA_REQUIRE_PLATFORM_SECRETS`, which turns "no store here"
//! from a skip into a failure. Without that variable the run skips and says
//! so, because a developer on a machine with a locked keychain should not be
//! blocked, but a test that quietly does nothing is worse than one that is
//! absent: it looks like coverage.

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

/// Whether a skipped real-store run is a failure.
///
/// CI sets this, because a test that skips silently is worse than one that is
/// absent: it looks like coverage. Setting it turns "the keyring was not set
/// up on this runner" from a green run into a red one.
const REQUIRE: &str = "TRAMONTANA_REQUIRE_PLATFORM_SECRETS";

#[tokio::test]
async fn the_platform_vault_obeys_the_same_contract() {
    let vault = PlatformSecretVault::under_service(TEST_SERVICE);
    if !vault.is_available() {
        assert!(
            std::env::var_os(REQUIRE).is_none(),
            "{REQUIRE} is set, but no platform credential store is reachable. \
             On Linux this means no Secret Service provider is running on the \
             session bus; on macOS it usually means no keychain is unlocked."
        );
        // Said out loud rather than silently returning, so a developer running
        // locally without a store can see why this proved nothing.
        println!(
            "SKIPPED: no platform credential store is reachable here. \
             Set {REQUIRE} to make this a failure instead."
        );
        return;
    }
    secret_vault_contract(&vault).await;
}
