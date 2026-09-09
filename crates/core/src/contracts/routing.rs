//! Which model was chosen, where it runs, and why.
//!
//! `CORE_CONTRACTS.md` §10 is normative. A routing decision exists so that
//! "why did it use that model?" has an answer in the record rather than in
//! someone's memory of the policy.
//!
//! Three properties are load-bearing:
//!
//! - **Reason ordering is deterministic.** Two identical situations produce
//!   the same reasons in the same order, so a decision can be compared with an
//!   earlier one rather than merely read.
//! - **A fallback is a new decision.** Retrying elsewhere is not an amendment
//!   to this record; it is another one, with `fallbackAfterFailure` among its
//!   reasons. The history therefore shows every place the work actually went.
//! - **It names the context it released.** Crossing from local to cloud is a
//!   data-release decision, and `requiresDisclosure` plus `contextManifestId`
//!   are what tie the choice to what was sent.
//!
//! M4 records this shape and fills it from fakes; M5 makes the real decision
//! behind the same type (`BUILD_ORDER.md` §12).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ExecutionLocation, RoutingMode, SCHEMA_VERSION};

/// Stable reason codes a routing decision can carry.
///
/// `CORE_CONTRACTS.md` §10 lists the initial set. They are constants rather
/// than an enum because the interface matches on them and a new code must be
/// an additive change, not a breaking one.
pub mod reason {
    /// The user picked this model explicitly.
    pub const USER_SELECTED: &str = "userSelected";
    /// Local Only forbids leaving the device.
    pub const LOCAL_ONLY_REQUIRED: &str = "localOnlyRequired";
    /// Policy prefers a cloud route where one is allowed.
    pub const CLOUD_PREFERRED: &str = "cloudPreferred";
    /// The task needs a capability only this model has.
    pub const CAPABILITY_REQUIRED: &str = "capabilityRequired";
    /// The preferred provider is not available.
    pub const PROVIDER_UNAVAILABLE: &str = "providerUnavailable";
    /// The preferred model is not available.
    pub const MODEL_UNAVAILABLE: &str = "modelUnavailable";
    /// The assembled context does not fit the preferred model.
    pub const CONTEXT_TOO_LARGE: &str = "contextTooLarge";
    /// Sensitive data blocked a route that would have released it.
    pub const SENSITIVITY_BLOCKED: &str = "sensitivityBlocked";
    /// A required credential is not configured.
    pub const CREDENTIAL_MISSING: &str = "credentialMissing";
    /// The device has no network.
    pub const OFFLINE: &str = "offline";
    /// A cheaper route was chosen deliberately.
    pub const COST_PREFERENCE: &str = "costPreference";
    /// This decision follows an earlier route failing.
    pub const FALLBACK_AFTER_FAILURE: &str = "fallbackAfterFailure";
    /// Exactly one route was eligible, so there was nothing to weigh.
    ///
    /// Added to §10's initial set in M4. Every other code explains a
    /// *preference*; this one is the honest answer when the policy had no
    /// choice to make, which is the common case until M5 adds providers.
    pub const SOLE_ELIGIBLE_ROUTE: &str = "soleEligibleRoute";
}

/// One model a decision could use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutedModel {
    /// The provider offering it.
    pub provider_id: String,
    /// The model.
    pub model_id: String,
}

/// A model and where it would run, as a fallback option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutingFallback {
    /// The provider offering it.
    pub provider_id: String,
    /// The model.
    pub model_id: String,
    /// Where it would run.
    pub location: ExecutionLocation,
}

/// Where a task's inference was sent, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutingDecision {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier for this decision.
    pub decision_id: String,
    /// The routing mode in force when it was made.
    pub mode: RoutingMode,
    /// The model chosen.
    pub model: RoutedModel,
    /// Where it runs.
    pub location: ExecutionLocation,
    /// The availability snapshot the decision was made against.
    ///
    /// Recorded so a decision can be explained later even though availability
    /// has moved on. Without it, "why not the other model?" is unanswerable.
    pub availability_snapshot_id: String,
    /// Why, in deterministic order.
    pub reason_codes: Vec<String>,
    /// What would have been tried next, in order.
    pub fallback_order: Vec<RoutingFallback>,
    /// The context this decision released.
    pub context_manifest_id: String,
    /// Whether reaching this route needs the user told.
    pub requires_disclosure: bool,
}

impl RoutingDecision {
    /// Build a decision, sorting its reasons into a deterministic order.
    ///
    /// `requires_disclosure` is computed from the location rather than passed
    /// in: whether data leaves the device is a fact about where it went, not a
    /// judgement the caller should be able to get wrong.
    #[must_use]
    pub fn new(
        decision_id: impl Into<String>,
        mode: RoutingMode,
        model: RoutedModel,
        location: ExecutionLocation,
        availability_snapshot_id: impl Into<String>,
        reason_codes: Vec<String>,
        context_manifest_id: impl Into<String>,
    ) -> Self {
        let requires_disclosure = matches!(location, ExecutionLocation::Cloud { .. });
        let mut reason_codes = reason_codes;
        // "Reason ordering is deterministic" (`CORE_CONTRACTS.md` §10). Sorting
        // is the cheapest rule that holds, and it lets two decisions be
        // compared rather than only read.
        reason_codes.sort_unstable();
        reason_codes.dedup();

        Self {
            schema_version: SCHEMA_VERSION,
            decision_id: decision_id.into(),
            mode,
            model,
            location,
            availability_snapshot_id: availability_snapshot_id.into(),
            reason_codes,
            fallback_order: Vec::new(),
            context_manifest_id: context_manifest_id.into(),
            requires_disclosure,
        }
    }

    /// Whether this decision sends data off the device.
    #[must_use]
    pub fn leaves_the_device(&self) -> bool {
        matches!(self.location, ExecutionLocation::Cloud { .. })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn local() -> ExecutionLocation {
        ExecutionLocation::Local {
            target_id: "this-device".into(),
            display_name: "This device".into(),
        }
    }

    fn decision(location: ExecutionLocation, reasons: Vec<String>) -> RoutingDecision {
        RoutingDecision::new(
            "decision-1",
            RoutingMode::Automatic,
            RoutedModel {
                provider_id: "fake".into(),
                model_id: "fake.deterministic".into(),
            },
            location,
            "availability-1",
            reasons,
            "context-1",
        )
    }

    #[test]
    fn reasons_are_ordered_deterministically_whatever_order_they_arrive_in() {
        let one = decision(
            local(),
            vec![
                reason::LOCAL_ONLY_REQUIRED.into(),
                reason::USER_SELECTED.into(),
            ],
        );
        let other = decision(
            local(),
            vec![
                reason::USER_SELECTED.into(),
                reason::LOCAL_ONLY_REQUIRED.into(),
            ],
        );
        assert_eq!(
            one.reason_codes, other.reason_codes,
            "two identical situations produced different records"
        );
    }

    #[test]
    fn a_repeated_reason_is_recorded_once() {
        let decision = decision(
            local(),
            vec![reason::OFFLINE.into(), reason::OFFLINE.into()],
        );
        assert_eq!(decision.reason_codes, vec![reason::OFFLINE]);
    }

    #[test]
    fn disclosure_is_derived_from_the_destination_not_asserted() {
        // Whether data leaves the device is a fact about where it went, not a
        // judgement a caller can get wrong.
        let staying = decision(local(), vec![reason::LOCAL_ONLY_REQUIRED.into()]);
        assert!(!staying.requires_disclosure);
        assert!(!staying.leaves_the_device());

        let leaving = decision(
            ExecutionLocation::Cloud {
                provider_id: "example".into(),
                region: None,
            },
            vec![reason::CLOUD_PREFERRED.into()],
        );
        assert!(leaving.requires_disclosure);
        assert!(leaving.leaves_the_device());
    }

    #[test]
    fn a_decision_names_the_context_it_released() {
        let decision = decision(local(), vec![reason::USER_SELECTED.into()]);
        assert_eq!(decision.context_manifest_id, "context-1");
    }

    #[test]
    fn decisions_serialize_as_camel_case() {
        let json =
            serde_json::to_value(decision(local(), vec![reason::USER_SELECTED.into()])).unwrap();
        assert_eq!(json["decisionId"], "decision-1");
        assert_eq!(json["mode"], "automatic");
        assert_eq!(json["model"]["modelId"], "fake.deterministic");
        assert_eq!(json["location"]["kind"], "local");
        assert_eq!(json["reasonCodes"][0], "userSelected");
        assert_eq!(json["requiresDisclosure"], false);
    }
}
