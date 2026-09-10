//! An in-memory credential store.
//!
//! Holds values in a map rather than in the operating system, which is what
//! makes every other test able to exercise the credential path without
//! touching the developer's real Keychain — and without a test run leaving
//! anything behind on the machine it ran on.
//!
//! It is a fake, not a shortcut: it passes the same contract suite as the
//! platform adapter, so a divergence between them is a defect in one rather
//! than a difference in what was tested.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, SecretRef};
use crate::ports::{SecretResolver, SecretVault};

/// A credential store that lives for as long as the process.
#[derive(Default)]
pub struct InMemorySecretVault {
    values: Mutex<HashMap<String, String>>,
    /// Whether the store reports itself unreachable.
    ///
    /// A locked Keychain and a headless Linux session are the interesting
    /// failure, and they are not reproducible from a test otherwise.
    unavailable: bool,
}

impl std::fmt::Debug for InMemorySecretVault {
    /// Prints no keys and no values.
    ///
    /// A debug format that listed the keys would put credential *names* in
    /// logs, and one that listed values would be the leak itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemorySecretVault")
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

impl InMemorySecretVault {
    /// An empty, working store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that reports itself unreachable, as a locked one would.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            values: Mutex::new(HashMap::new()),
            unavailable: true,
        }
    }

    /// The normalized error an unreachable store produces.
    fn unreachable(&self) -> ErrorEnvelope {
        ErrorEnvelope::new(
            ErrorCode::ProviderUnavailable,
            "the credential store is unavailable",
            CorrelationId::new("secret"),
        )
    }
}

#[async_trait]
impl SecretResolver for InMemorySecretVault {
    async fn resolve(&self, reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        if self.unavailable {
            return Err(self.unreachable());
        }
        self.values
            .lock()
            .ok()
            .and_then(|values| values.get(&reference.key).cloned())
            .ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::CredentialMissing,
                    format!("no credential is stored for {}", reference.key),
                    CorrelationId::new("secret"),
                )
            })
    }
}

#[async_trait]
impl SecretVault for InMemorySecretVault {
    async fn store(&self, reference: &SecretRef, value: &str) -> Result<(), ErrorEnvelope> {
        if self.unavailable {
            return Err(self.unreachable());
        }
        if let Ok(mut values) = self.values.lock() {
            values.insert(reference.key.clone(), value.to_owned());
        }
        Ok(())
    }

    async fn delete(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope> {
        if self.unavailable {
            return Err(self.unreachable());
        }
        Ok(self
            .values
            .lock()
            .is_ok_and(|mut values| values.remove(&reference.key).is_some()))
    }

    async fn contains(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope> {
        if self.unavailable {
            return Err(self.unreachable());
        }
        Ok(self
            .values
            .lock()
            .is_ok_and(|values| values.contains_key(&reference.key)))
    }
}
