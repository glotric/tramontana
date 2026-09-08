use async_trait::async_trait;

use crate::contracts::{
    ApprovalDecision, ApprovalDecisionKind, ApprovalId, ApprovalRequest, Capability,
    CapabilityScope, ProjectGrant, ProjectId, RiskClass, Timestamp,
};

/// Why a permission operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PermissionError {
    /// Nobody can be asked, so nothing can be approved.
    ///
    /// Distinct from a denial: the user did not refuse, there was no user. It
    /// still fails closed (`SECURITY_AND_PERMISSIONS.md` §4).
    #[error("interactive approval is not available")]
    NoApprover,
    /// The user was asked and did not answer in time.
    #[error("the approval request expired before it was answered")]
    Expired,
    /// Durable storage failed.
    #[error("permission storage unavailable: {0}")]
    Unavailable(String),
}

/// Where approvals and grants are kept.
///
/// `consume_once` is the load-bearing method. "Once grants are consumed exactly
/// once" (`PHASE_1_ACCEPTANCE_CRITERIA.md` §9) has to hold under concurrency
/// and across a restart, so consumption is a single atomic operation that
/// reports whether *this* caller was the one that claimed it — never a read
/// followed by a write.
#[async_trait]
pub trait ApprovalStore: Send + Sync {
    /// Record that a question was put to the user.
    ///
    /// # Errors
    ///
    /// [`PermissionError::Unavailable`] when storage failed.
    async fn record_request(&self, request: &ApprovalRequest) -> Result<(), PermissionError>;

    /// Record the answer.
    ///
    /// # Errors
    ///
    /// [`PermissionError::Unavailable`] when storage failed.
    async fn record_decision(&self, decision: &ApprovalDecision) -> Result<(), PermissionError>;

    /// Atomically claim an unconsumed allow-once approval.
    ///
    /// Returns `true` for exactly one caller. Every later or concurrent caller
    /// gets `false`, including after a restart.
    ///
    /// # Errors
    ///
    /// [`PermissionError::Unavailable`] when storage failed.
    async fn consume_once(&self, approval_id: &ApprovalId) -> Result<bool, PermissionError>;

    /// Persist a project grant.
    ///
    /// # Errors
    ///
    /// [`PermissionError::Unavailable`] when storage failed.
    async fn save_grant(&self, grant: &ProjectGrant) -> Result<(), PermissionError>;

    /// Every live grant for a project.
    ///
    /// # Errors
    ///
    /// [`PermissionError::Unavailable`] when storage failed.
    async fn list_grants(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<ProjectGrant>, PermissionError>;

    /// Revoke a grant. Returns whether one was removed.
    ///
    /// # Errors
    ///
    /// [`PermissionError::Unavailable`] when storage failed.
    async fn revoke_grant(&self, grant_id: &str) -> Result<bool, PermissionError>;
}

/// Who answers approval questions.
///
/// The CLI prompts on a terminal; the desktop shows a card. A headless context
/// has none, and returns [`PermissionError::NoApprover`] rather than assuming.
#[async_trait]
pub trait Approver: Send + Sync {
    /// Put a question to the user and wait for the answer.
    ///
    /// # Errors
    ///
    /// [`PermissionError::NoApprover`] when nobody can be asked, or
    /// [`PermissionError::Expired`] when no answer arrived.
    async fn ask(&self, request: &ApprovalRequest)
    -> Result<ApprovalDecisionKind, PermissionError>;
}

/// What a tool wants to do, before anyone has decided whether it may.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionQuery {
    /// The exact tool.
    pub tool_id: String,
    /// What it needs.
    pub capability: Capability,
    /// How much damage it could do.
    pub risk_class: RiskClass,
    /// What it would be limited to.
    pub scope: CapabilityScope,
    /// Whether a project grant may cover this, or every use must be approved.
    ///
    /// "Not offered for operations classified as non-persistable"
    /// (`SECURITY_AND_PERMISSIONS.md` §4).
    pub persistable: bool,
}

/// Why a request was allowed or refused.
///
/// Stable strings so the activity stream, diagnostics, and tests can all agree
/// on what happened.
pub mod reason {
    /// A read inside an already-opened project needs no approval.
    pub const SAFE_DEFAULT: &str = "policy.safe_default";
    /// A live project grant covered the request.
    pub const PROJECT_GRANT: &str = "policy.project_grant";
    /// The user allowed this one request.
    pub const APPROVED_ONCE: &str = "policy.approved_once";
    /// The user allowed this capability for the project.
    pub const APPROVED_PROJECT: &str = "policy.approved_project";
    /// The user refused.
    pub const USER_DENIED: &str = "policy.user_denied";
    /// The request named no project, so it has no filesystem scope at all.
    pub const NO_PROJECT: &str = "policy.no_project";
    /// The request reached outside the roots the project approved.
    pub const OUTSIDE_ROOTS: &str = "policy.outside_roots";
    /// Nobody could be asked.
    pub const NO_APPROVER: &str = "policy.no_approver";
    /// An allow-once approval had already been used.
    pub const ONCE_ALREADY_CONSUMED: &str = "policy.once_already_consumed";
    /// Policy could not be read, so the request fails closed.
    pub const POLICY_UNAVAILABLE: &str = "policy.unavailable";
}

/// The outcome of evaluating a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    /// Permitted. Carries how, and the approval that produced it if any.
    Allowed {
        /// Why it was allowed.
        reason_code: &'static str,
        /// The approval, when a user was asked.
        approval_id: Option<ApprovalId>,
    },
    /// Refused. The worker receives this, not a chance to rephrase.
    Denied {
        /// Why it was refused.
        reason_code: &'static str,
    },
}

impl PermissionOutcome {
    /// Whether the request may proceed.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

    /// The stable reason, whichever way it went.
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::Allowed { reason_code, .. } | Self::Denied { reason_code } => reason_code,
        }
    }
}

/// Decides whether a tool may run.
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Evaluate a request, asking the user if the policy requires it.
    ///
    /// # Errors
    ///
    /// Returns an error only when the decision itself could not be reached.
    /// A refusal is a successful evaluation with [`PermissionOutcome::Denied`].
    async fn evaluate(
        &self,
        query: &PermissionQuery,
        task_id: &crate::contracts::TaskId,
        invocation_id: &crate::contracts::InvocationId,
        now: &Timestamp,
    ) -> Result<PermissionOutcome, PermissionError>;
}
