//! Capability descriptors for models, workers, and tools.
//!
//! `CORE_CONTRACTS.md` §7 and §9. Descriptors are how a replaceable
//! implementation tells the core what it can actually do. Two rules:
//!
//! - **Feature-detect, never infer from a name.** A model is not assumed to
//!   support tool calls because of what it is called.
//! - **A worker's self-description is evidence, not authority.**
//!   [`AgentDescriptor::local_only_eligible`] is computed by the core from
//!   enforceable facts, never copied from what the worker claims about itself.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{RiskClass, SideEffect};

/// Something a model can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelCapability {
    /// Produces text.
    Text,
    /// Streams incrementally.
    Streaming,
    /// Requests tool calls.
    ToolCalls,
    /// Honors a response schema.
    StructuredOutput,
    /// Accepts images.
    ImageInput,
    /// Can be cancelled mid-request.
    Cancellation,
}

/// Whether a provider or worker is usable right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Health {
    /// Not yet checked.
    Unknown,
    /// Usable.
    Available,
    /// Usable but impaired.
    Degraded,
    /// Not usable.
    Unavailable,
}

/// A model a provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    /// Provider that offers it.
    pub provider_id: String,
    /// Provider-scoped model identifier.
    pub model_id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Whether inference runs on this device or elsewhere.
    pub location: ModelLocation,
    /// Detected capabilities.
    pub capabilities: Vec<ModelCapability>,
    /// Context window, when the provider reports one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context_limit: Option<u64>,
    /// Current health.
    pub health: Health,
    /// Stable reason for a non-available health, for diagnostics and tests.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub health_reason_code: Option<String>,
}

/// Whether a model runs on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelLocation {
    /// On this device.
    Local,
    /// A third-party endpoint. Reaching one is a data-release decision.
    Cloud,
}

/// How a worker is integrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationMode {
    /// Runs inside Tramontana.
    Embedded,
    /// A separate process speaking the Agent Client Protocol.
    Acp,
    /// A separate process speaking something bespoke.
    CustomProcess,
}

/// Who decides which model a worker uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelControl {
    /// Every model request passes through Tramontana's router and policy.
    TramontanaBrokered,
    /// The worker calls models itself. Not equivalent to brokered.
    WorkerManaged,
}

/// How a worker reaches tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolControl {
    /// Every effect is requested through the host and passes policy.
    TramontanaBrokered,
    /// Direct access, confined by a tested compute boundary.
    DirectConstrained,
    /// Direct access with no confinement. Cannot meet the Phase 1 contract.
    DirectUnconstrained,
}

/// What a worker can do on the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NetworkControl {
    /// No network access.
    Denied,
    /// All egress passes through Tramontana.
    Brokered,
    /// Egress is limited by a tested boundary.
    Constrained,
    /// Egress cannot be observed. Disqualifying for Local Only.
    Unobservable,
}

/// How well a worker can be stopped and resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CheckpointSupport {
    /// Cannot be resumed.
    None,
    /// Can resume itself from its own state.
    SameWorker,
    /// Can be resumed from canonical Tramontana state by another worker.
    Portable,
}

/// A worker a provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDescriptor {
    /// Provider that offers it.
    pub agent_provider_id: String,
    /// Provider-scoped worker identifier.
    pub agent_id: String,
    /// Human-readable name.
    pub display_name: String,
    /// How it is integrated.
    pub integration_mode: IntegrationMode,
    /// Who chooses its model.
    pub model_control: ModelControl,
    /// How it reaches tools.
    pub tool_control: ToolControl,
    /// What it can do on the network.
    pub network_control: NetworkControl,
    /// How well it can be stopped and resumed.
    pub checkpoint_support: CheckpointSupport,
    /// Whether it may run under Local Only.
    ///
    /// Computed by the core from the fields above, never taken from the
    /// worker's own claim (`CORE_CONTRACTS.md` §7.2).
    pub local_only_eligible: bool,
    /// Why it is unavailable, when it is.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub unavailable_reason: Option<String>,
}

impl AgentDescriptor {
    /// Whether a worker with this control profile may run under Local Only.
    ///
    /// Local Only is a hard egress prohibition
    /// (`SECURITY_AND_PERMISSIONS.md` §8). A worker qualifies only if the core
    /// can *enforce* that, which means both its model calls and its network
    /// access are observable. A worker that might reach the network unseen is
    /// ineligible however it describes itself.
    #[must_use]
    pub fn compute_local_only_eligibility(
        model_control: ModelControl,
        network_control: NetworkControl,
    ) -> bool {
        let egress_observable = matches!(
            network_control,
            NetworkControl::Denied | NetworkControl::Brokered | NetworkControl::Constrained
        );
        matches!(model_control, ModelControl::TramontanaBrokered) && egress_observable
    }
}

/// A tool the registry can offer to a worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    /// Namespaced identifier, such as `builtin.fs.read` or `mcp.github.search`.
    pub tool_id: String,
    /// Provider that implements it.
    pub provider_id: String,
    /// Human-readable name.
    pub display_name: String,
    /// What it does, for the approval prompt. Presentation, never policy.
    pub description: String,
    /// JSON Schema its arguments are validated against.
    pub input_schema: serde_json::Value,
    /// How much damage it could do.
    pub risk_class: RiskClass,
    /// What it changes.
    pub side_effect: SideEffect,
    /// Capability it requires.
    pub capability: String,
    /// Whether it honors cancellation.
    pub supports_cancellation: bool,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_fully_brokered_worker_is_local_only_eligible() {
        assert!(AgentDescriptor::compute_local_only_eligibility(
            ModelControl::TramontanaBrokered,
            NetworkControl::Denied,
        ));
        assert!(AgentDescriptor::compute_local_only_eligibility(
            ModelControl::TramontanaBrokered,
            NetworkControl::Brokered,
        ));
    }

    #[test]
    fn unobservable_egress_disqualifies_a_worker_from_local_only() {
        // The whole promise of Local Only is that nothing leaves. A worker
        // that could call out unseen cannot be part of that promise, whatever
        // it says about itself.
        assert!(!AgentDescriptor::compute_local_only_eligibility(
            ModelControl::TramontanaBrokered,
            NetworkControl::Unobservable,
        ));
    }

    #[test]
    fn a_worker_managed_model_path_disqualifies_a_worker_from_local_only() {
        // Even with no network of its own, a worker choosing its own model is
        // choosing a destination the core did not audit.
        for network in [
            NetworkControl::Denied,
            NetworkControl::Brokered,
            NetworkControl::Constrained,
        ] {
            assert!(!AgentDescriptor::compute_local_only_eligibility(
                ModelControl::WorkerManaged,
                network,
            ));
        }
    }

    #[test]
    fn descriptors_serialize_as_camel_case() {
        let descriptor = AgentDescriptor {
            agent_provider_id: "builtin".into(),
            agent_id: "builtin.reference".into(),
            display_name: "Reference worker".into(),
            integration_mode: IntegrationMode::Embedded,
            model_control: ModelControl::TramontanaBrokered,
            tool_control: ToolControl::TramontanaBrokered,
            network_control: NetworkControl::Denied,
            checkpoint_support: CheckpointSupport::Portable,
            local_only_eligible: true,
            unavailable_reason: None,
        };
        let json = serde_json::to_value(&descriptor).unwrap();

        assert_eq!(json["integrationMode"], "embedded");
        assert_eq!(json["modelControl"], "tramontanaBrokered");
        assert_eq!(json["checkpointSupport"], "portable");
        assert_eq!(json["localOnlyEligible"], true);
        assert!(!json.as_object().unwrap().contains_key("unavailableReason"));
    }
}
