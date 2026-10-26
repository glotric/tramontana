//! Renting compute by the minute (ADR 0072).
//!
//! `ComputeRouter`'s first non-local target. An adapter talks to one
//! provider's control API; whether a lease *should* be started, and when it
//! must stop, is decided in [`crate::services::ComputeLeases`].

use async_trait::async_trait;

use crate::contracts::{
    ComputeLease, ComputeLeaseEvent, ComputeSpec, ErrorEnvelope, LeaseCost, LeaseId, LeaseStatus,
    ProviderId,
};

/// Starts, watches, prices and terminates leases at one provider.
///
/// Contract, checked by `tests/compute_leases.rs` against the fake:
///
/// - `provision` creates nothing that bills while idle: zero always-on
///   workers, and the spec's idle limit enforced by the provider itself;
/// - `stop` **terminates** (no stopped machine whose disk keeps billing) and
///   is idempotent: stopping a lease that is already gone is `Ok`;
/// - `cost` comes from the provider. An adapter that cannot read it returns
///   an error rather than an estimate.
#[async_trait]
pub trait ComputeProvisioner: Send + Sync {
    /// Who this provider is.
    fn id(&self) -> ProviderId;

    /// Start a lease.
    ///
    /// # Errors
    ///
    /// A normalized provider error; nothing is left running on failure.
    async fn provision(&self, spec: &ComputeSpec) -> Result<ComputeLease, ErrorEnvelope>;

    /// Where the lease is.
    ///
    /// # Errors
    ///
    /// A normalized provider error when the provider cannot be asked.
    async fn status(&self, lease: &LeaseId) -> Result<LeaseStatus, ErrorEnvelope>;

    /// Terminate the lease. Idempotent.
    ///
    /// # Errors
    ///
    /// A normalized provider error when the provider could not be told.
    async fn stop(&self, lease: &LeaseId) -> Result<(), ErrorEnvelope>;

    /// What the lease has cost, from the provider.
    ///
    /// # Errors
    ///
    /// Any failure to read it; callers fail closed.
    async fn cost(&self, lease: &LeaseId) -> Result<LeaseCost, ErrorEnvelope>;
}

/// The durable record of leases: append-only, in order.
#[async_trait]
pub trait ComputeLeaseJournal: Send + Sync {
    /// Record one event.
    ///
    /// # Errors
    ///
    /// A normalized error when storage failed.
    async fn append(&self, event: &ComputeLeaseEvent) -> Result<(), ErrorEnvelope>;

    /// Every event, oldest first.
    ///
    /// # Errors
    ///
    /// A normalized error when storage failed.
    async fn list(&self) -> Result<Vec<ComputeLeaseEvent>, ErrorEnvelope>;
}
