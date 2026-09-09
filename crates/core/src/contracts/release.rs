//! What is about to be sent, where, and on whose authority.
//!
//! `SECURITY_AND_PERMISSIONS.md` §8 is normative: before cloud escalation the
//! routing and context layers must produce a *release plan* naming the
//! destination, the reason, the sources and message range included, the data
//! classes and size, the redactions and exclusions applied, the tools offered
//! to the model, and the approval or standing policy that permits it.
//!
//! The plan is not a second copy of the context manifest. They answer
//! different questions, and the difference is the point:
//!
//! - a [`ContextManifest`](super::ContextManifest) records what was
//!   *assembled* for a task;
//! - a [`ContextReleasePlan`] records what one particular request actually
//!   *carries* to one particular destination.
//!
//! Assembling is not sending. A source can be gathered and then never reach
//! the model — because nothing in the payload used it, because it was
//! redacted, or because a budget dropped it. A plan derived from the manifest
//! alone would claim it was released; a plan derived from the payload cannot.
//! That is why the plan is computed from the bytes being handed over, and why
//! `TESTING_STRATEGY.md` §5 names "release plan mismatch between disclosed and
//! transmitted source categories/bytes" as a case that must be tested.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    ApprovalId, DataClass, ExecutionLocation, MessageId, RoutedModel, RoutingMode, TaskId,
    Timestamp,
};

/// Stable reasons a release was permitted or refused.
///
/// The same vocabulary whichever way it went, so an interface can explain a
/// refusal in the terms it uses to explain a release.
pub mod reason {
    /// The destination is on this device, so nothing was disclosed.
    pub const LOCAL_DESTINATION: &str = "release.localDestination";
    /// The user permitted this one release.
    pub const APPROVED_ONCE: &str = "release.approvedOnce";
    /// The user refused.
    pub const USER_DENIED: &str = "release.userDenied";
    /// Nobody could be asked, so nothing left the device.
    pub const NO_APPROVER: &str = "release.noApprover";
    /// The user was asked and did not answer.
    pub const APPROVAL_EXPIRED: &str = "release.approvalExpired";
    /// A source was assembled but no part of the payload carried it.
    pub const NOT_TRANSMITTED: &str = "release.notTransmitted";
}

/// One category of content actually carried by a request.
///
/// Summarized by category and data class rather than listed source by source:
/// the disclosure has to be readable before a person decides, and
/// `PHASE_1_ACCEPTANCE_CRITERIA.md` §12 asks for categories, classes, and a
/// size estimate — not a file listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleasedCategory {
    /// What kind of source it is.
    pub category: super::ContextCategory,
    /// How sensitive the content is.
    pub data_class: DataClass,
    /// How many sources of this kind were carried.
    pub source_count: u32,
    /// How many bytes of them were carried.
    pub byte_count: u64,
}

/// A source that was assembled and did not go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseExclusion {
    /// Which source.
    pub source_id: String,
    /// What kind of source it is.
    pub category: super::ContextCategory,
    /// Stable reason it did not go.
    pub reason_code: String,
}

/// On whose authority a release proceeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAuthorization {
    /// Stable reason, from [`reason`].
    pub reason_code: String,
    /// The approval that permitted it, when a user was asked.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub approval_id: Option<ApprovalId>,
    /// The routing mode in force, which is the standing policy that applied.
    pub mode: RoutingMode,
}

/// Everything one request releases to one destination.
///
/// Built by `services::release`, never by hand: the totals and categories are
/// derived from the payload, so a plan cannot describe a request other than
/// the one it accompanies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextReleasePlan {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier for this release.
    pub release_id: String,
    /// The task it belongs to.
    pub task_id: TaskId,
    /// The request it accompanies.
    pub request_id: String,
    /// The routing decision that chose the destination.
    pub decision_id: String,
    /// The assembled context this payload was drawn from.
    pub context_manifest_id: String,
    /// The model it is going to.
    pub destination: RoutedModel,
    /// Where that model runs.
    pub location: ExecutionLocation,
    /// Whether this release leaves the device.
    ///
    /// Derived from `location`, like `RoutingDecision::requires_disclosure`
    /// and for the same reason: whether data left the device is a fact about
    /// where it went, not a judgement a caller should be able to get wrong.
    pub requires_disclosure: bool,
    /// Why the destination was chosen, in the router's deterministic order.
    pub reason_codes: Vec<String>,
    /// The messages carried, in payload order.
    pub message_ids: Vec<MessageId>,
    /// What was carried, by category and data class.
    pub released: Vec<ReleasedCategory>,
    /// Data classes actually carried, strongest first.
    pub data_classes: Vec<DataClass>,
    /// Total bytes carried.
    pub total_bytes: u64,
    /// Approximate total token cost, when one was estimated.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_token_estimate: Option<u64>,
    /// Stable codes for redactions applied before release.
    pub redactions: Vec<String>,
    /// Sources assembled for this task that this request did not carry.
    pub exclusions: Vec<ReleaseExclusion>,
    /// Tools the model is allowed to call, by name.
    ///
    /// Offering a tool to a cloud model discloses that the tool exists and
    /// what it takes, which is part of the release and not an implementation
    /// detail (`SECURITY_AND_PERMISSIONS.md` §8).
    pub tools_offered: Vec<String>,
    /// On whose authority it proceeded. Absent while the plan is a proposal.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub authorization: Option<ReleaseAuthorization>,
    /// When the plan was made.
    pub created_at: Timestamp,
}

impl ContextReleasePlan {
    /// Whether this plan has been authorized.
    #[must_use]
    pub fn is_authorized(&self) -> bool {
        self.authorization.is_some()
    }

    /// A one-line summary for an approval prompt.
    ///
    /// Names the destination and the size, never the content: a disclosure
    /// that quoted what it was disclosing would be a second copy of it.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "Send {} bytes of {} context to {} ({})",
            self.total_bytes,
            self.data_classes
                .iter()
                .map(class_label)
                .collect::<Vec<_>>()
                .join(", "),
            self.destination.model_id,
            self.destination.provider_id,
        )
    }
}

/// How a data class is named in a prompt shown to a person.
fn class_label(class: &DataClass) -> &'static str {
    match class {
        DataClass::Public => "public",
        DataClass::Project => "project",
        DataClass::Sensitive => "sensitive",
        DataClass::Secret => "secret",
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{ContextCategory, SCHEMA_VERSION};

    fn plan(location: ExecutionLocation) -> ContextReleasePlan {
        ContextReleasePlan {
            schema_version: SCHEMA_VERSION,
            release_id: "release-1".to_owned(),
            task_id: TaskId::new("task-1"),
            request_id: "request-1".to_owned(),
            decision_id: "decision-1".to_owned(),
            context_manifest_id: "context-1".to_owned(),
            destination: RoutedModel {
                provider_id: "openai".to_owned(),
                model_id: "gpt-oss".to_owned(),
            },
            requires_disclosure: matches!(location, ExecutionLocation::Cloud { .. }),
            location,
            reason_codes: vec!["cloudPreferred".to_owned()],
            message_ids: vec![MessageId::new("message-1")],
            released: vec![ReleasedCategory {
                category: ContextCategory::Conversation,
                data_class: DataClass::Project,
                source_count: 1,
                byte_count: 9,
            }],
            data_classes: vec![DataClass::Project],
            total_bytes: 9,
            total_token_estimate: None,
            redactions: Vec::new(),
            exclusions: Vec::new(),
            tools_offered: Vec::new(),
            authorization: None,
            created_at: Timestamp::new("2026-01-01T00:00:00.000Z"),
        }
    }

    fn cloud() -> ExecutionLocation {
        ExecutionLocation::Cloud {
            provider_id: "openai".to_owned(),
            region: None,
        }
    }

    #[test]
    fn a_proposal_is_not_authorized() {
        let proposal = plan(cloud());
        assert!(!proposal.is_authorized());
        let json = serde_json::to_value(&proposal).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("authorization"),
            "an unauthorized plan serialized an authorization field"
        );
    }

    #[test]
    fn the_prompt_summary_never_quotes_what_it_discloses() {
        // The summary is shown to a person deciding whether to release the
        // content. Putting the content in it would be the disclosure leaking
        // through the thing meant to describe it.
        let summary = plan(cloud()).summary();
        assert!(summary.contains("gpt-oss"), "{summary}");
        assert!(summary.contains("project"), "{summary}");
        assert!(summary.contains('9'), "{summary}");
    }

    #[test]
    fn plans_serialize_as_camel_case() {
        let json = serde_json::to_value(plan(cloud())).unwrap();
        assert_eq!(json["contextManifestId"], "context-1");
        assert_eq!(json["requiresDisclosure"], true);
        assert_eq!(json["released"][0]["dataClass"], "project");
        assert_eq!(json["released"][0]["sourceCount"], 1);
    }
}
