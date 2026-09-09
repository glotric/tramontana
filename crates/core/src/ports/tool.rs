use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AuthorizedToolRequest, ErrorEnvelope, InvocationId, ProviderId, SideEffect, ToolDescriptor,
};

/// A tool call that has passed schema validation but not yet policy.
///
/// The *request*. `PermissionManager` turns one of these into an
/// [`AuthorizedToolRequest`], which is what [`ToolProvider::invoke`] accepts —
/// so an unauthorized call is not merely forbidden, it is unrepresentable
/// (`CORE_CONTRACTS.md` §9).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    /// Identifies this invocation across its request, start, and result events.
    pub invocation_id: InvocationId,
    /// Namespaced tool identifier.
    pub tool_id: String,
    /// Arguments, already validated against the tool's input schema.
    pub arguments: serde_json::Value,
}

/// What a tool produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// Structured, redacted result content.
    pub content: serde_json::Value,
    /// Short human-readable summary safe to show in the activity stream.
    pub display_summary: String,
    /// What the invocation actually changed.
    pub side_effect: SideEffect,
    /// Whether output was cut short by a limit.
    pub truncated: bool,
}

/// A source of tools.
///
/// A provider contributes tools; it never grants permission to use them. The
/// registry and, from M3, the permission manager stand between a worker and
/// anything here.
#[async_trait]
pub trait ToolProvider: Send + Sync {
    /// Stable provider identifier.
    fn id(&self) -> ProviderId;

    /// Tools this provider offers, with their risk and side-effect metadata.
    fn list_tools(&self) -> Vec<ToolDescriptor>;

    /// Run an authorized invocation.
    ///
    /// The argument type is the proof: a provider cannot be called without a
    /// permission decision, because only the permission manager can build one.
    ///
    /// A provider must not exceed
    /// [`AuthorizedToolRequest::granted_scope`]. The scope is the authority,
    /// not a hint — and re-checking it inside the provider is what makes a
    /// mistake in the caller fail safe.
    ///
    /// # Errors
    ///
    /// Returns a normalized error on failure, timeout, or cancellation.
    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope>;
}
