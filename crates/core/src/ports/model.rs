//! The inference port.
//!
//! The request and stream shapes are contract types (`CORE_CONTRACTS.md` §8),
//! not port-local ones: they are serialized into the durable record and cross
//! the interface boundary, so a provider adapter and the interface must be
//! talking about the same thing.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AuthorizedModelRequest, ErrorEnvelope, ModelDescriptor, ModelEvent, ProviderId,
};

/// An inference implementation.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stable provider identifier.
    fn id(&self) -> ProviderId;

    /// Models this provider currently offers, with detected capabilities.
    async fn list_models(&self) -> Vec<ModelDescriptor>;

    /// Start a streaming request.
    ///
    /// The returned receiver yields [`ModelEvent`]s and is closed after the
    /// single terminal event. Cancelling `cancel` must converge to
    /// [`StopReason::Cancelled`] rather than dropping the stream silently.
    ///
    /// Takes an [`AuthorizedModelRequest`] rather than a bare request, and
    /// that is the point: the only way to obtain one is
    /// `services::ReleaseManager::authorize`, so a provider — a cloud provider
    /// above all — cannot be reached without a release decision having been
    /// made and recorded (`SECURITY_AND_PERMISSIONS.md` §8). It is the same
    /// construction that makes a tool unreachable without a permission
    /// decision.
    ///
    /// # Errors
    ///
    /// Returns a normalized error when the request cannot be started at all.
    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope>;
}
