use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::contracts::{ErrorEnvelope, ModelDescriptor, ProviderId};

/// A request for inference.
///
/// M1 carries only what the walking skeleton needs. The full serialized
/// contract in `CORE_CONTRACTS.md` §8 — normalized messages, tool definitions,
/// required capabilities, data classes, and a context manifest reference —
/// lands in M5 with the first real provider, since defining it now would
/// freeze a shape nothing has exercised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    /// Correlates the resulting stream with its `modelStarted` event.
    pub request_id: String,
    /// The prompt.
    pub prompt: String,
}

/// Why a model stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// The model finished normally.
    Stop,
    /// An output limit was reached.
    Length,
    /// The model asked to call a tool.
    ToolCall,
    /// The request was cancelled.
    Cancelled,
}

/// One item from a model stream.
///
/// Exactly one `Completed` or `Failed` ends a stream, even when cancellation
/// arrives while the underlying request is still in flight
/// (`CORE_CONTRACTS.md` §8).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ModelEvent {
    /// Incremental text.
    TextDelta {
        /// Position within this stream.
        offset: u64,
        /// The text produced.
        text: String,
    },
    /// The stream ended normally.
    Completed {
        /// Why it ended.
        stop_reason: StopReason,
    },
    /// The stream ended in failure.
    Failed {
        /// Normalized, already-redacted error.
        error: ErrorEnvelope,
    },
}

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
    /// # Errors
    ///
    /// Returns a normalized error when the request cannot be started at all.
    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope>;
}
