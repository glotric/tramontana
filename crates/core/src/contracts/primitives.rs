//! Common primitives from `docs/CORE_CONTRACTS.md` §3.
//!
//! These are vocabulary, not behavior: the classifications that permission,
//! routing, context, and audit code all agree on. They are defined at M0 so
//! the schema-generation path is proven end to end against a real tagged
//! union before M1 adds the task, event, and tool contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Sensitivity of a piece of content, used to decide what may leave the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DataClass {
    /// Not derived from the user's project or conversation.
    Public,
    /// Derived from an opened project.
    Project,
    /// Explicitly marked sensitive by the user or a policy.
    Sensitive,
    /// A credential or token. Never serialized; see [`SecretRef`].
    Secret,
}

/// Who an event is for.
///
/// Redaction happens in core before delivery, so `Secret` events are internal
/// audit markers holding references and hashes only, and never reach a webview
/// (`CORE_CONTRACTS.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Visibility {
    /// Shown in the normal activity stream.
    User,
    /// Available in diagnostics, not in the primary stream.
    Diagnostic,
    /// Internal audit marker; never delivered to the user interface.
    Secret,
}

/// How much damage a tool invocation could do if the model asked for it wrongly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RiskClass {
    /// Observes data without intended mutation.
    Read,
    /// Changes recoverable project state.
    Modify,
    /// Hard to reverse, broad, privileged, external, or destructive.
    Dangerous,
}

/// What an invocation actually changed, recorded alongside its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SideEffect {
    /// Nothing outside Tramontana's own records changed.
    None,
    /// A local change that can be undone.
    LocalReversible,
    /// A local change that cannot be undone.
    LocalIrreversible,
    /// Something outside this device changed.
    External,
}

/// The user's standing policy for where inference may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RoutingMode {
    /// Hard egress prohibition for inference and cloud-derived context.
    LocalOnly,
    /// Deterministic policy chooses an eligible route and shows its reasons.
    Automatic,
    /// Prefer a configured cloud route where policy and availability allow.
    CloudPreferred,
}

/// Where a resolvable secret lives.
///
/// Only `Platform` exists today. It is an enum rather than a bare marker so
/// that adding a store later is an additive schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SecretStore {
    /// The operating system credential store.
    Platform,
}

/// An opaque handle to a credential.
///
/// This is the only representation of a secret that may be persisted, logged,
/// exported, or sent over IPC. `key` is an application-generated identifier
/// and never carries any part of the value it refers to
/// (`SECURITY_AND_PERMISSIONS.md` §7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    /// Opaque application-generated key. Never the secret.
    pub key: String,
    /// Which store resolves this reference.
    pub store: SecretStore,
}

/// Where work ran.
///
/// Recorded on routing decisions and shown to the user, so "did this leave my
/// machine?" is answerable from the durable record alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExecutionLocation {
    /// A device the user controls.
    #[serde(rename_all = "camelCase")]
    Local {
        /// Stable identifier for the compute target.
        target_id: String,
        /// Human-readable name for the interface.
        display_name: String,
    },
    /// A third-party endpoint. Reaching one is a data-release decision.
    #[serde(rename_all = "camelCase")]
    Cloud {
        /// Stable provider identifier.
        provider_id: String,
        /// Provider region, when the provider exposes one.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        region: Option<String>,
    },
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_as_camel_case() {
        let cases: Vec<(String, &str)> = vec![
            (
                serde_json::to_string(&DataClass::Project).unwrap(),
                "\"project\"",
            ),
            (
                serde_json::to_string(&Visibility::Diagnostic).unwrap(),
                "\"diagnostic\"",
            ),
            (
                serde_json::to_string(&RiskClass::Dangerous).unwrap(),
                "\"dangerous\"",
            ),
            (
                serde_json::to_string(&SideEffect::LocalIrreversible).unwrap(),
                "\"localIrreversible\"",
            ),
            (
                serde_json::to_string(&RoutingMode::CloudPreferred).unwrap(),
                "\"cloudPreferred\"",
            ),
            (
                serde_json::to_string(&SecretStore::Platform).unwrap(),
                "\"platform\"",
            ),
        ];
        for (actual, expected) in cases {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn execution_location_is_a_kind_tagged_union() {
        let local = ExecutionLocation::Local {
            target_id: "device-1".into(),
            display_name: "This Mac".into(),
        };
        assert_eq!(
            serde_json::to_value(&local).unwrap(),
            serde_json::json!({
                "kind": "local",
                "targetId": "device-1",
                "displayName": "This Mac",
            })
        );
    }

    #[test]
    fn absent_optional_fields_are_omitted_not_null() {
        // `CORE_CONTRACTS.md` §2: absent is valid, and `null` is reserved for
        // "known empty" differing from absent.
        let cloud = ExecutionLocation::Cloud {
            provider_id: "openai".into(),
            region: None,
        };
        let json = serde_json::to_value(&cloud).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "cloud", "providerId": "openai" })
        );
        assert!(!json.as_object().unwrap().contains_key("region"));
    }

    #[test]
    fn contracts_round_trip_through_json() {
        let original = SecretRef {
            key: "provider.openai.default".into(),
            store: SecretStore::Platform,
        };
        let encoded = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<SecretRef>(&encoded).unwrap(),
            original
        );
    }
}
