use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    ApprovalDetail, AuthorizedToolRequest, ErrorEnvelope, FileChange, InvocationId, ProviderId,
    SideEffect, TaskId, ToolDescriptor,
};
use crate::ports::SiteAccess;

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
    /// The files this invocation changed, in the tool's own words. The tool
    /// is the authority on what it did; the core records it rather than
    /// guessing from the tool's output (ADR 0026). Empty for any tool that
    /// changes no file.
    pub changes: Vec<FileChange>,
}

/// What a call would act on, as its provider resolves it before anyone is
/// asked (ADR 0047).
///
/// Only the built-in browser says anything here: the site of the page a
/// click lands on is known to the browser, not to the call's arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolTarget {
    /// The web site the call acts on.
    pub site: Option<SiteAccess>,
    /// What the approval prompt shows about it, such as the page and the
    /// button. Redacted by the core before anyone sees it.
    pub details: Vec<ApprovalDetail>,
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

    /// What a call would act on, before it is authorized.
    ///
    /// Must not change anything: it only looks. Nothing by default.
    ///
    /// # Errors
    ///
    /// A normalized error the model can act on, such as a page that is not
    /// open; the call is then refused without anyone being asked.
    async fn target(
        &self,
        _task_id: &TaskId,
        _tool_id: &str,
        _arguments: &serde_json::Value,
    ) -> Result<ToolTarget, ErrorEnvelope> {
        Ok(ToolTarget::default())
    }

    /// A task has ended, however it ended, or was interrupted. A provider
    /// that holds something for the task, such as a browser window, lets it
    /// go. Nothing by default.
    async fn task_ended(&self, _task_id: &TaskId) {}
}
