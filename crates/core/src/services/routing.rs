//! Choosing where a request runs, and being able to explain it afterwards.
//!
//! `CORE_CONTRACTS.md` §10 and `SECURITY_AND_PERMISSIONS.md` §8. The router is
//! a pure function so that "identical input yields an identical decision and
//! reason order" is a property that can be tested rather than hoped for
//! (`BUILD_ORDER.md` §14).
//!
//! Three rules are load-bearing:
//!
//! - **Local Only is a prohibition, not a preference.** A cloud model under
//!   Local Only is not ranked last; it is not eligible, and it does not appear
//!   in the fallback order either. A fallback list containing an endpoint the
//!   policy forbids is an invitation to reach it.
//! - **Crossing trust locations is a new decision.** A fallback is not an
//!   amendment to the record; it is another decision, with its own reasons and
//!   its own disclosure. The history therefore shows every place work actually
//!   went.
//! - **A decision names the availability snapshot it was made against.**
//!   Otherwise "why not the other model?" is unanswerable an hour later, when
//!   availability has moved on.

use crate::contracts::{
    ExecutionLocation, Health, ModelCapability, ModelDescriptor, ModelLocation, RoutedModel,
    RoutingDecision, RoutingFallback, RoutingMode, Timestamp, routing_reason,
};

/// What was available when a decision was made.
///
/// Snapshotted rather than queried at decision time so the decision can be
/// re-read later against the world it was actually made in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilitySnapshot {
    /// Stable identifier, recorded on the decision.
    pub snapshot_id: String,
    /// Every model every configured provider offered.
    pub models: Vec<ModelDescriptor>,
    /// When it was taken.
    pub taken_at: Timestamp,
}

/// Everything a routing decision is made from.
#[derive(Debug, Clone)]
pub struct RoutingInput {
    /// The mode in force.
    pub mode: RoutingMode,
    /// What was available.
    pub snapshot: AvailabilitySnapshot,
    /// Capabilities the request cannot be served without.
    pub required_capabilities: Vec<ModelCapability>,
    /// The context release this decision is for.
    pub context_manifest_id: String,
    /// Whether the request carries anything more sensitive than public data.
    pub carries_private_data: bool,
    /// Approximate size of the assembled context, for a context-window check.
    pub context_tokens: Option<u64>,
    /// A model the user chose explicitly, if any.
    pub preferred: Option<RoutedModel>,
    /// Models already tried and failed, which must not be chosen again.
    pub exhausted: Vec<RoutedModel>,
}

impl RoutingInput {
    /// A decision with no user preference and nothing yet tried.
    #[must_use]
    pub fn new(
        mode: RoutingMode,
        snapshot: AvailabilitySnapshot,
        context_manifest_id: impl Into<String>,
    ) -> Self {
        Self {
            mode,
            snapshot,
            required_capabilities: Vec::new(),
            context_manifest_id: context_manifest_id.into(),
            carries_private_data: false,
            context_tokens: None,
            preferred: None,
            exhausted: Vec::new(),
        }
    }
}

/// Why no route could be chosen.
///
/// Carries the reasons rather than a message, so an interface can explain the
/// refusal in the same vocabulary a successful decision uses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no eligible model: {}", reason_codes.join(", "))]
pub struct NoEligibleRoute {
    /// Stable reasons every candidate was ruled out for, in order.
    pub reason_codes: Vec<String>,
}

/// One candidate and why it was kept or dropped.
struct Candidate<'a> {
    model: &'a ModelDescriptor,
    /// `None` when eligible; otherwise the reason it is not.
    excluded_for: Option<&'static str>,
}

/// Choose a route, or explain why there is none.
///
/// # Errors
///
/// [`NoEligibleRoute`] listing the reasons, deduplicated and ordered, when no
/// candidate survives. A refusal is as much a decision as a choice, and it has
/// to be as explainable.
pub fn route(
    decision_id: impl Into<String>,
    input: &RoutingInput,
) -> Result<RoutingDecision, NoEligibleRoute> {
    let candidates: Vec<Candidate<'_>> = input
        .snapshot
        .models
        .iter()
        .map(|model| Candidate {
            model,
            excluded_for: exclusion(model, input),
        })
        .collect();

    let mut eligible: Vec<&ModelDescriptor> = candidates
        .iter()
        .filter(|candidate| candidate.excluded_for.is_none())
        .map(|candidate| candidate.model)
        .collect();

    if eligible.is_empty() {
        let mut reason_codes: Vec<String> = candidates
            .iter()
            .filter_map(|candidate| candidate.excluded_for.map(str::to_owned))
            .collect();
        if reason_codes.is_empty() {
            // No models at all, rather than models that were all ruled out.
            reason_codes.push(routing_reason::MODEL_UNAVAILABLE.to_owned());
        }
        reason_codes.sort_unstable();
        reason_codes.dedup();
        return Err(NoEligibleRoute { reason_codes });
    }

    // Deterministic before preference is applied, so two runs over the same
    // snapshot rank identically whatever order the providers were registered.
    eligible.sort_by(|left, right| {
        rank(left, input.mode)
            .cmp(&rank(right, input.mode))
            .then_with(|| left.provider_id.cmp(&right.provider_id))
            .then_with(|| left.model_id.cmp(&right.model_id))
    });

    let mut reasons = Vec::new();
    let chosen = match input
        .preferred
        .as_ref()
        .and_then(|wanted| position_of(&eligible, wanted))
    {
        Some(index) => {
            reasons.push(routing_reason::USER_SELECTED.to_owned());
            eligible.remove(index)
        }
        None => {
            reasons.push(preference_reason(input.mode, eligible[0]));
            eligible.remove(0)
        }
    };

    if input.mode == RoutingMode::LocalOnly {
        // Recorded even when it changed nothing: the reason a request stayed
        // on the device is a fact the user should see, not an absence.
        reasons.push(routing_reason::LOCAL_ONLY_REQUIRED.to_owned());
    }
    if !input.exhausted.is_empty() {
        reasons.push(routing_reason::FALLBACK_AFTER_FAILURE.to_owned());
    }
    if eligible.is_empty() && reasons.len() == 1 {
        reasons.push(routing_reason::SOLE_ELIGIBLE_ROUTE.to_owned());
    }

    let mut decision = RoutingDecision::new(
        decision_id,
        input.mode,
        RoutedModel {
            provider_id: chosen.provider_id.clone(),
            model_id: chosen.model_id.clone(),
        },
        location_of(chosen),
        input.snapshot.snapshot_id.clone(),
        reasons,
        input.context_manifest_id.clone(),
    );
    decision.fallback_order = eligible
        .iter()
        .map(|model| RoutingFallback {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            location: location_of(model),
        })
        .collect();

    Ok(decision)
}

/// Make a *new* decision after `failed` did not work.
///
/// Not an amendment to the previous decision. Crossing from local to cloud on
/// a retry is a data release that the earlier decision never authorized, so it
/// gets its own record, its own reasons, and its own disclosure
/// (`CORE_CONTRACTS.md` §10).
///
/// # Errors
///
/// [`NoEligibleRoute`] when nothing is left to try.
pub fn route_after_failure(
    decision_id: impl Into<String>,
    input: &RoutingInput,
    failed: &RoutedModel,
) -> Result<RoutingDecision, NoEligibleRoute> {
    let mut next = input.clone();
    if !next.exhausted.contains(failed) {
        next.exhausted.push(failed.clone());
    }
    // A user preference that has already failed is not a preference any more.
    if next.preferred.as_ref() == Some(failed) {
        next.preferred = None;
    }
    route(decision_id, &next)
}

/// Why this model cannot serve the request, if it cannot.
///
/// Ordered from the strongest prohibition down, so a cloud model refused under
/// Local Only reports that rather than an incidental capability gap.
fn exclusion(model: &ModelDescriptor, input: &RoutingInput) -> Option<&'static str> {
    let is_cloud = model.location == ModelLocation::Cloud;

    // A hard egress prohibition, checked before anything that could be read as
    // a preference (`SECURITY_AND_PERMISSIONS.md` §8).
    if is_cloud && input.mode == RoutingMode::LocalOnly {
        return Some(routing_reason::LOCAL_ONLY_REQUIRED);
    }
    if is_cloud && input.carries_private_data && input.mode == RoutingMode::Automatic {
        // Automatic does not send a user's project off the device on its own.
        // Choosing that is what Cloud Preferred means.
        return Some(routing_reason::SENSITIVITY_BLOCKED);
    }
    if input
        .exhausted
        .iter()
        .any(|tried| tried.provider_id == model.provider_id && tried.model_id == model.model_id)
    {
        return Some(routing_reason::FALLBACK_AFTER_FAILURE);
    }
    match model.health {
        Health::Available | Health::Degraded => {}
        // Impaired is still usable; unknown is not yet checked and is not
        // something to send a user's work to.
        Health::Unavailable | Health::Unknown => return Some(routing_reason::MODEL_UNAVAILABLE),
    }
    if !input
        .required_capabilities
        .iter()
        .all(|needed| model.capabilities.contains(needed))
    {
        return Some(routing_reason::CAPABILITY_REQUIRED);
    }
    if let (Some(tokens), Some(limit)) = (input.context_tokens, model.context_limit)
        && tokens > limit
    {
        return Some(routing_reason::CONTEXT_TOO_LARGE);
    }
    None
}

/// Sort key within a mode. Lower is preferred.
fn rank(model: &ModelDescriptor, mode: RoutingMode) -> u8 {
    let is_local = model.location == ModelLocation::Local;
    match mode {
        // Local-first by default: work stays on the device unless the user
        // asked otherwise.
        RoutingMode::LocalOnly | RoutingMode::Automatic => u8::from(!is_local),
        RoutingMode::CloudPreferred => u8::from(is_local),
    }
}

/// The reason a mode's default ranking produced this choice.
fn preference_reason(mode: RoutingMode, chosen: &ModelDescriptor) -> String {
    match mode {
        RoutingMode::CloudPreferred if chosen.location == ModelLocation::Cloud => {
            routing_reason::CLOUD_PREFERRED.to_owned()
        }
        // Cloud Preferred that lands on a local model did so because no cloud
        // model was eligible, which is a different fact and says so.
        RoutingMode::CloudPreferred => routing_reason::PROVIDER_UNAVAILABLE.to_owned(),
        _ => routing_reason::SOLE_ELIGIBLE_ROUTE.to_owned(),
    }
}

/// Where a model runs, in the form a routing decision records.
fn location_of(model: &ModelDescriptor) -> ExecutionLocation {
    match model.location {
        ModelLocation::Local => ExecutionLocation::Local {
            target_id: "this-device".to_owned(),
            display_name: "This device".to_owned(),
        },
        ModelLocation::Cloud => ExecutionLocation::Cloud {
            provider_id: model.provider_id.clone(),
            region: None,
        },
    }
}

/// Where `wanted` sits among the eligible models, if it is one of them.
fn position_of(eligible: &[&ModelDescriptor], wanted: &RoutedModel) -> Option<usize> {
    eligible.iter().position(|model| {
        model.provider_id == wanted.provider_id && model.model_id == wanted.model_id
    })
}
