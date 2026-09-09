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

/// Storing, replacing, and removing credentials.
///
/// Deliberately a *different* port from [`SecretResolver`], not a superset
/// everyone gets. An adapter receives the resolver and can do exactly one
/// thing: turn the one reference it was handed into the one value it needs.
/// The application service receives this, and can manage the store. A model
/// adapter that could also write to the credential store, or enumerate it,
/// would hold capability it never needs — and "it does not use it" is a
/// convention, whereas a narrower port is a fact
/// (`SECURITY_AND_PERMISSIONS.md` §7).
///
/// There is no `list`. Which credentials exist is answered by the references
/// the database already holds; an enumeration API over the real store would be
/// a leak surface with no caller.
#[async_trait]
pub trait SecretVault: SecretResolver {
    /// Store `value` under `reference`, replacing anything already there.
    ///
    /// Replacement rather than insert-or-fail because "support credential
    /// replacement" is the normal case: a rotated key is the same reference
    /// with a new value, and making the caller delete first would leave a
    /// window with no credential at all.
    ///
    /// # Errors
    ///
    /// A normalized error when the store is unavailable. Never echoes `value`.
    async fn store(&self, reference: &SecretRef, value: &str) -> Result<(), ErrorEnvelope>;

    /// Remove `reference`. Returns whether anything was there.
    ///
    /// # Errors
    ///
    /// A normalized error when the store is unavailable. Deleting something
    /// absent is `Ok(false)`, not an error: the caller asked for it to be
    /// gone, and it is gone.
    async fn delete(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope>;

    /// Whether a credential is stored under `reference`.
    ///
    /// Answers "is this configured?" without resolving the value, so an
    /// interface can show that a credential is attached without ever holding
    /// it (`SECURITY_AND_PERMISSIONS.md` §7).
    ///
    /// # Errors
    ///
    /// A normalized error when the store is unavailable. A store that cannot
    /// be reached is not the same as a credential that is not there, and the
    /// two must not collapse into `false`.
    async fn contains(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope>;
}
