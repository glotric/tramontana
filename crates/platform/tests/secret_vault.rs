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

use tramontana_core::contracts::{SecretRef, SecretStore};
use tramontana_core::ports::SecretVault;
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

/// Whether `vault` can actually hold a credential.
///
/// A write, not a read. `is_available` answers whether the store *answers*,
/// and that is not the same question: a Secret Service with no unlocked
/// collection answers reads with "no such credential" and refuses every
/// write. Gating this suite on the read probe let it run against a store it
/// could not use, and the first `store` in the contract failed with
/// `providerUnavailable` — a real assertion failure reported as a defect in
/// the adapter, when the adapter was right and the environment was broken.
///
/// The probe writes under the suite's own service namespace, which is
/// disposable, and removes what it wrote.
async fn is_usable(vault: &PlatformSecretVault) -> bool {
    let probe = SecretRef {
        key: "tramontana.test.usability-probe".to_owned(),
        store: SecretStore::Platform,
    };
    let usable = vault.store(&probe, "probe").await.is_ok();
    let _ = vault.delete(&probe).await;
    usable
}

#[tokio::test]
async fn the_platform_vault_obeys_the_same_contract() {
    let vault = PlatformSecretVault::under_service(TEST_SERVICE);
    if !is_usable(&vault).await {
        assert!(
            std::env::var_os(REQUIRE).is_none(),
            "{REQUIRE} is set, but no platform credential store will accept a \
             credential here. On Linux this usually means the Secret Service \
             has no unlocked collection — `gnome-keyring-daemon --unlock` \
             cannot create one from an empty password, so it must be given a \
             non-empty one. On macOS it usually means no keychain is unlocked. \
             Note that the store may still answer reads in this state, which \
             is why this is gated on a write."
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
