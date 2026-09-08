//! Capabilities, scopes, approvals, and authorization.
//!
//! `SECURITY_AND_PERMISSIONS.md` §3–§4 and `CORE_CONTRACTS.md` §9 are
//! normative. The load-bearing idea in this module is not any single type — it
//! is that [`AuthorizedToolRequest`] **cannot be constructed outside the
//! permission manager**. A tool provider takes one and therefore cannot be
//! called without a decision having been made, and the compiler enforces that
//! rather than a convention.
//!
//! Everything here is fail-closed. Missing, malformed, expired, or
//! mismatched authorization denies; nothing is an implicit allow.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    ApprovalId, InvocationId, ProjectId, RiskClass, SCHEMA_VERSION, SideEffect, TaskId, Timestamp,
};

/// What a tool needs permission to do.
///
/// A dotted, namespaced name such as `filesystem.read` or `git.read`. Grants
/// are matched on the exact capability; there is no wildcard and no implied
/// hierarchy, so `filesystem.read` never confers `filesystem.modify`.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    /// Name a capability.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the dotted name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an authorization is limited to.
///
/// `SECURITY_AND_PERMISSIONS.md` §3.2. A scope binds the project and, for
/// filesystem work, the canonical roots it may touch. Authorizations are
/// non-transferable: a grant naming one project authorizes nothing in another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityScope {
    /// The project this is limited to. Absent means a projectless task, which
    /// has no filesystem access at all.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// Canonical roots the operation may touch.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub roots: Vec<String>,
    /// Whether the operation sends project-derived data off the device.
    ///
    /// A separate dimension from risk: "a read action that sends content to a
    /// cloud endpoint is not treated as a harmless local read"
    /// (`SECURITY_AND_PERMISSIONS.md` §3.1).
    pub releases_data: bool,
}

impl CapabilityScope {
    /// A scope limited to one project and its roots, with no data release.
    #[must_use]
    pub fn project(project_id: ProjectId, roots: Vec<String>) -> Self {
        Self {
            project_id: Some(project_id),
            roots,
            releases_data: false,
        }
    }

    /// Whether `self` is entirely contained by `other`.
    ///
    /// Used to decide whether a stored grant covers a fresh request. The test
    /// is deliberately one-directional and strict: a grant covers a request
    /// only when it names the same project, permits every root the request
    /// asks for, and is at least as permissive about data release. Anything
    /// unclear is not covered.
    #[must_use]
    pub fn is_covered_by(&self, other: &Self) -> bool {
        if self.project_id != other.project_id {
            return false;
        }
        if self.releases_data && !other.releases_data {
            return false;
        }
        self.roots.iter().all(|root| other.roots.contains(root))
    }
}

/// How long an authorization lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum GrantKind {
    /// No approval was needed: a read inside an already-opened project.
    ImplicitSafeDefault,
    /// Approved for exactly this request, and consumed by it.
    Once,
    /// A persistent, revocable grant for this project and capability.
    Project,
}

/// What the user may choose when asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecisionKind {
    /// Allow this request only, once.
    AllowOnce,
    /// Allow this capability in this project until revoked.
    AllowProject,
    /// Refuse.
    Deny,
}

/// A question put to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    /// Contract schema version.
    pub schema_version: u32,
    /// Identifies this question and the decision that answers it.
    pub approval_id: ApprovalId,
    /// The task waiting on it.
    pub task_id: TaskId,
    /// The invocation waiting on it, when a tool call prompted the question.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub invocation_id: Option<InvocationId>,
    /// What is being asked for.
    pub capability: Capability,
    /// The exact tool.
    pub tool_id: String,
    /// What it is limited to.
    pub requested_scope: CapabilityScope,
    /// How much damage it could do.
    pub risk_class: RiskClass,
    /// What it would change.
    pub side_effect: SideEffect,
    /// One-line summary for the prompt.
    pub summary: String,
    /// Label/value pairs shown beneath the summary. Already redacted.
    pub details: Vec<ApprovalDetail>,
    /// Which answers are offered.
    ///
    /// `AllowProject` is omitted for anything the policy declares
    /// non-persistable, so the interface cannot offer a choice the core would
    /// refuse to honor.
    pub eligible_decisions: Vec<ApprovalDecisionKind>,
}

/// One labelled fact shown in an approval prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDetail {
    /// What this fact is.
    pub label: String,
    /// The value. Never a secret, never raw file content.
    pub value: String,
}

/// The user's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecision {
    /// Contract schema version.
    pub schema_version: u32,
    /// The question being answered.
    pub approval_id: ApprovalId,
    /// The answer.
    pub decision: ApprovalDecisionKind,
    /// When it was answered.
    pub decided_at: Timestamp,
}

impl ApprovalDecision {
    /// Record an answer.
    #[must_use]
    pub fn new(
        approval_id: ApprovalId,
        decision: ApprovalDecisionKind,
        decided_at: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            approval_id,
            decision,
            decided_at,
        }
    }
}

/// A persistent, revocable grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGrant {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier, so it can be shown and revoked.
    pub grant_id: String,
    /// The project it belongs to.
    pub project_id: ProjectId,
    /// What it permits.
    pub capability: Capability,
    /// The exact tool it permits.
    pub tool_id: String,
    /// What it is limited to.
    pub scope: CapabilityScope,
    /// The highest risk it covers. A grant never escalates.
    pub risk_ceiling: RiskClass,
    /// When it was granted.
    pub granted_at: Timestamp,
}

/// What is being authorized.
///
/// Split from [`GrantedAuthority`] so the two halves cannot be transposed: the
/// request being made and the permission that covers it come from different
/// places and are easy to confuse in a long parameter list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuthorizedTarget {
    /// The invocation.
    pub(crate) invocation_id: InvocationId,
    /// The task it belongs to.
    pub(crate) task_id: TaskId,
    /// The tool.
    pub(crate) tool_id: String,
    /// Validated arguments.
    pub(crate) arguments: serde_json::Value,
}

/// The permission that covers it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GrantedAuthority {
    /// What was granted.
    pub(crate) capability: Capability,
    /// The limits it carries.
    pub(crate) scope: CapabilityScope,
    /// How it was obtained.
    pub(crate) kind: GrantKind,
    /// The approval that produced it, when a user was asked.
    pub(crate) approval_id: Option<ApprovalId>,
}

/// A tool request that has passed policy.
///
/// **Only the permission manager can build one.** The inner fields are private
/// and the sole constructor is `pub(crate)`, so a tool provider that accepts
/// this type cannot be called without a decision having been made. That is the
/// difference between a permission system and a convention.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizedToolRequest {
    invocation_id: InvocationId,
    task_id: TaskId,
    tool_id: String,
    arguments: serde_json::Value,
    capability: Capability,
    granted_scope: CapabilityScope,
    grant_kind: GrantKind,
    approval_id: Option<ApprovalId>,
}

impl AuthorizedToolRequest {
    /// Build an authorization. Reachable only from within the core.
    #[must_use]
    pub(crate) fn new(target: AuthorizedTarget, grant: GrantedAuthority) -> Self {
        Self {
            invocation_id: target.invocation_id,
            task_id: target.task_id,
            tool_id: target.tool_id,
            arguments: target.arguments,
            capability: grant.capability,
            granted_scope: grant.scope,
            grant_kind: grant.kind,
            approval_id: grant.approval_id,
        }
    }

    /// The invocation this authorizes.
    #[must_use]
    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }

    /// The task it belongs to.
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// The tool it authorizes.
    #[must_use]
    pub fn tool_id(&self) -> &str {
        &self.tool_id
    }

    /// The validated arguments.
    #[must_use]
    pub fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }

    /// The capability that was granted.
    #[must_use]
    pub fn capability(&self) -> &Capability {
        &self.capability
    }

    /// The limits the grant carries. A provider must not exceed these.
    #[must_use]
    pub fn granted_scope(&self) -> &CapabilityScope {
        &self.granted_scope
    }

    /// How the authorization was obtained.
    #[must_use]
    pub fn grant_kind(&self) -> GrantKind {
        self.grant_kind
    }

    /// The approval that produced it, when a user was asked.
    #[must_use]
    pub fn approval_id(&self) -> Option<&ApprovalId> {
        self.approval_id.as_ref()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn scope(project: &str, roots: &[&str]) -> CapabilityScope {
        CapabilityScope::project(
            ProjectId::new(project),
            roots.iter().map(|r| (*r).to_owned()).collect(),
        )
    }

    #[test]
    fn a_grant_covers_a_request_within_its_own_project_and_roots() {
        let granted = scope("p1", &["/work/a", "/work/b"]);
        assert!(scope("p1", &["/work/a"]).is_covered_by(&granted));
        assert!(scope("p1", &["/work/a", "/work/b"]).is_covered_by(&granted));
    }

    #[test]
    fn a_grant_never_reaches_into_another_project() {
        // "Authorizations are non-transferable. A grant to one MCP App or one
        // project does not grant another access." (§3.2)
        let granted = scope("p1", &["/work/a"]);
        assert!(!scope("p2", &["/work/a"]).is_covered_by(&granted));
    }

    #[test]
    fn a_grant_never_reaches_a_root_it_does_not_name() {
        let granted = scope("p1", &["/work/a"]);
        assert!(!scope("p1", &["/work/a", "/work/elsewhere"]).is_covered_by(&granted));
        assert!(!scope("p1", &["/work/elsewhere"]).is_covered_by(&granted));
    }

    #[test]
    fn a_local_grant_never_authorizes_sending_data_off_the_device() {
        // Data release is a separate dimension from risk. A grant to read
        // files locally must not become permission to upload them.
        let local = scope("p1", &["/work/a"]);
        let releasing = CapabilityScope {
            releases_data: true,
            ..scope("p1", &["/work/a"])
        };
        assert!(!releasing.is_covered_by(&local));
        // The reverse is fine: a grant that permits release covers one that
        // does not need it.
        assert!(local.is_covered_by(&releasing));
    }

    #[test]
    fn a_projectless_scope_is_not_covered_by_a_project_grant() {
        let granted = scope("p1", &["/work/a"]);
        let projectless = CapabilityScope {
            project_id: None,
            roots: Vec::new(),
            releases_data: false,
        };
        assert!(!projectless.is_covered_by(&granted));
    }

    #[test]
    fn capabilities_have_no_implied_hierarchy() {
        // `filesystem.read` must not confer `filesystem.modify` by prefix.
        // Matching is on the exact name, which this asserts by construction.
        assert_ne!(
            Capability::new("filesystem.read"),
            Capability::new("filesystem.modify")
        );
        assert_ne!(
            Capability::new("filesystem"),
            Capability::new("filesystem.read")
        );
    }

    #[test]
    fn permission_types_serialize_as_camel_case() {
        let decision = ApprovalDecision::new(
            ApprovalId::new("a1"),
            ApprovalDecisionKind::AllowProject,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let json = serde_json::to_value(&decision).unwrap();
        assert_eq!(json["decision"], "allowProject");
        assert_eq!(json["approvalId"], "a1");
        assert_eq!(json["schemaVersion"], SCHEMA_VERSION);
    }

    #[test]
    fn an_empty_scope_omits_its_fields_rather_than_nulling_them() {
        let scope = CapabilityScope {
            project_id: None,
            roots: Vec::new(),
            releases_data: false,
        };
        let json = serde_json::to_value(&scope).unwrap();
        let object = json.as_object().unwrap();
        assert!(!object.contains_key("projectId"));
        assert!(!object.contains_key("roots"));
        assert_eq!(object["releasesData"], false);
    }
}
