//! SP0-B spike: the encrypted-file vault for a host with no credential store.
//!
//! Runs the same contract as the in-memory and platform vaults, then what
//! only a file can get wrong: plaintext on disk, a wrong key, an altered file.
//! Needs `--features file-vault`.

#![cfg(feature = "file-vault")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::Path;

use tramontana_core::contracts::ErrorCode;
use tramontana_core::ports::{SecretResolver, SecretVault};
use tramontana_core::testing::contract_suites::{
    secret_vault_contract, test_secret_ref, unavailable_store_contract,
};
use tramontana_platform::FileSecretVault;

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const OTHER_KEY: &str = "ff0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn vault_in(dir: &Path, key: &str) -> FileSecretVault {
    std::fs::write(dir.join("key"), format!("{key}\n")).unwrap();
    FileSecretVault::new(dir.join("vault"), dir.join("key"))
}

#[tokio::test]
async fn the_file_vault_obeys_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    secret_vault_contract(&vault_in(dir.path(), KEY)).await;
}

#[tokio::test]
async fn a_missing_key_is_an_unavailable_store_not_a_missing_credential() {
    let dir = tempfile::tempdir().unwrap();
    let vault = FileSecretVault::new(dir.path().join("vault"), dir.path().join("no-key"));
    unavailable_store_contract(&vault).await;
}

#[tokio::test]
async fn neither_the_value_nor_its_reference_is_on_disk_in_the_clear() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(dir.path(), KEY);
    let reference = test_secret_ref("on-disk");
    vault
        .store(&reference, "sk-live-very-secret")
        .await
        .unwrap();

    let on_disk = std::fs::read(dir.path().join("vault")).unwrap();
    let text = String::from_utf8_lossy(&on_disk);
    assert!(!text.contains("sk-live-very-secret"));
    assert!(!text.contains(&reference.key));
    assert!(!dir.path().join("vault.staging").exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("vault"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "readable by others: {mode:o}");
    }
}

#[tokio::test]
async fn a_wrong_key_or_an_altered_file_is_refused_never_read_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let reference = test_secret_ref("sealed");
    vault_in(dir.path(), KEY)
        .store(&reference, "value")
        .await
        .unwrap();

    // Another key: refused, and not "no credential" — that would make a
    // person re-enter keys that are still there.
    let wrong = vault_in(dir.path(), OTHER_KEY);
    assert_eq!(
        wrong.resolve(&reference).await.unwrap_err().code,
        ErrorCode::PersistenceError
    );
    assert_eq!(
        wrong.contains(&reference).await.unwrap_err().code,
        ErrorCode::PersistenceError
    );

    // The right key, one flipped byte: refused the same way.
    let right = vault_in(dir.path(), KEY);
    let mut sealed = std::fs::read(dir.path().join("vault")).unwrap();
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    std::fs::write(dir.path().join("vault"), sealed).unwrap();
    assert_eq!(
        right.resolve(&reference).await.unwrap_err().code,
        ErrorCode::PersistenceError
    );
}

#[tokio::test]
async fn a_malformed_key_is_an_unavailable_store() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(dir.path(), "not-a-key");
    assert_eq!(
        vault
            .resolve(&test_secret_ref("any"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProviderUnavailable
    );
}

#[tokio::test]
async fn concurrent_stores_keep_every_write() {
    let dir = tempfile::tempdir().unwrap();
    let vault = std::sync::Arc::new(vault_in(dir.path(), KEY));
    let writes = (0..16).map(|n| {
        let vault = std::sync::Arc::clone(&vault);
        tokio::spawn(async move {
            vault
                .store(&test_secret_ref(&format!("n{n}")), &format!("v{n}"))
                .await
                .unwrap();
        })
    });
    for write in writes.collect::<Vec<_>>() {
        write.await.unwrap();
    }
    for n in 0..16 {
        assert_eq!(
            vault
                .resolve(&test_secret_ref(&format!("n{n}")))
                .await
                .unwrap(),
            format!("v{n}")
        );
    }
}
