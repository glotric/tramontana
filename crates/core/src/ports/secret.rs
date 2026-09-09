//! Resolving an opaque secret reference to a value.
//!
//! `SECURITY_AND_PERMISSIONS.md` §7: a [`SecretRef`] is an opaque key and a
//! store name, never any part of the value. This is the one place a value
//! exists, and it exists as a return value rather than as a field: an adapter
//! resolves per request and lets the value drop, so a secret is never held in
//! a struct that could be logged, cloned into an event, or captured in a
//! debug format.

use async_trait::async_trait;

use crate::contracts::{ErrorEnvelope, SecretRef};

/// Turns an opaque reference into the value it names.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    /// Resolve `reference`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::CredentialMissing`](crate::contracts::ErrorCode) when the
    /// store has no such key, or a normalized error when the store itself is
    /// unavailable. The error must not echo the key's value.
    async fn resolve(&self, reference: &SecretRef) -> Result<String, ErrorEnvelope>;
}
