//! How a user configured a model provider.
//!
//! `ARCHITECTURE.md` §416 expects the interface to "list, health, configure
//! non-secret settings, manage credentials by reference" for models and
//! providers. This is the record behind that: what a profile knows about
//! where models can be reached.
//!
//! Two properties are load-bearing.
//!
//! **A credential is never here.** Only a [`SecretRef`] — an opaque key and a
//! store name — which is the only representation of a secret this project
//! persists at all (`SECURITY_AND_PERMISSIONS.md` §7, ADR 0008). The type
//! makes the alternative unrepresentable rather than discouraged.
//!
//! **Trust location is derived, never declared.** There is no field for "this
//! is a local endpoint". A configuration that could *assert* its location
//! would be able to lie to the router, and Local Only — a hard egress
//! prohibition — would be defeated by a settings row. Where an endpoint is
//! follows from what it actually points at.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ModelLocation, SCHEMA_VERSION, SecretRef, Timestamp};

/// What kind of endpoint this is, and what it needs to be reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ProviderConfig {
    /// Anything speaking the OpenAI chat-completions format.
    ///
    /// `llama-server`, Ollama, LM Studio, vLLM, and OpenAI itself. One shape
    /// covers the local path and the optional cloud path, because the
    /// difference between them is a URL and whether a credential is attached
    /// (ADR 0007).
    #[serde(rename_all = "camelCase")]
    OpenAiCompatible {
        /// Where it is. `http://` only for loopback (ADR 0009).
        base_url: String,
        /// The model to ask for.
        model_id: String,
        /// The credential, by reference. Absent for a local endpoint.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        credential: Option<SecretRef>,
    },
    /// Anthropic's Messages API.
    #[serde(rename_all = "camelCase")]
    Anthropic {
        /// Where it is.
        base_url: String,
        /// The model to ask for.
        model_id: String,
        /// The credential, by reference. Not optional: the API refuses
        /// unauthenticated requests, so an endpoint without one is a
        /// configuration that cannot work.
        credential: SecretRef,
    },
    /// A `llama-server` this application starts and stops.
    ///
    /// The binary and the model are paths the user chose. Nothing is
    /// downloaded and `PATH` is not searched (`BUILD_ORDER.md` §14).
    #[serde(rename_all = "camelCase")]
    LlamaServer {
        /// The executable.
        binary_path: String,
        /// The GGUF model file.
        model_path: String,
        /// The name the model is reported under.
        model_id: String,
        /// Extra arguments, passed through verbatim. Never a credential.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        extra_args: Vec<String>,
    },
    /// An `mlx_lm.server` this application starts and stops (ADR 0010).
    ///
    /// Apple Silicon only; elsewhere it is configured but unavailable. The
    /// executable and the model folder are paths the user chose: nothing is
    /// downloaded, no Python is bundled, and `PATH` is not searched.
    #[serde(rename_all = "camelCase")]
    MlxServer {
        /// The `mlx_lm.server` executable, usually inside a virtual
        /// environment the user made.
        binary_path: String,
        /// The MLX model folder: weights, tokenizer and config.
        model_path: String,
        /// The name the model is reported under.
        model_id: String,
        /// Extra arguments, passed through verbatim. Never a credential.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        extra_args: Vec<String>,
    },
}

impl ProviderConfig {
    /// A stable discriminator, for storage and display.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible { .. } => "openAiCompatible",
            Self::Anthropic { .. } => "anthropic",
            Self::LlamaServer { .. } => "llamaServer",
            Self::MlxServer { .. } => "mlxServer",
        }
    }

    /// The model this endpoint serves.
    #[must_use]
    pub fn model_id(&self) -> &str {
        match self {
            Self::OpenAiCompatible { model_id, .. }
            | Self::Anthropic { model_id, .. }
            | Self::LlamaServer { model_id, .. }
            | Self::MlxServer { model_id, .. } => model_id,
        }
    }

    /// The credential this endpoint uses, if any.
    #[must_use]
    pub fn credential(&self) -> Option<&SecretRef> {
        match self {
            Self::OpenAiCompatible { credential, .. } => credential.as_ref(),
            Self::Anthropic { credential, .. } => Some(credential),
            Self::LlamaServer { .. } | Self::MlxServer { .. } => None,
        }
    }

    /// Where work sent here actually runs.
    ///
    /// **Derived, never declared.** A settings row that could claim to be
    /// local would defeat Local Only, which is a hard egress prohibition and
    /// not a preference (`SECURITY_AND_PERMISSIONS.md` §8). A supervised
    /// process is on this device by construction; an HTTP endpoint is local
    /// exactly when it is on loopback.
    #[must_use]
    pub fn location(&self) -> ModelLocation {
        match self {
            // A supervised process is on this device by construction.
            Self::LlamaServer { .. } | Self::MlxServer { .. } => ModelLocation::Local,
            // Anthropic is a third party wherever its URL points. A loopback
            // proxy in front of it is still a route off this device.
            Self::Anthropic { .. } => ModelLocation::Cloud,
            Self::OpenAiCompatible { base_url, .. } => {
                if is_loopback(base_url) {
                    ModelLocation::Local
                } else {
                    ModelLocation::Cloud
                }
            }
        }
    }
}

/// Whether a URL addresses this device.
///
/// Parsed rather than searched: a URL merely *containing* `localhost` — in a
/// path, a query, or userinfo before an `@` — addresses whatever host follows
/// it. Getting this wrong would mark a third-party endpoint as local and
/// exempt it from disclosure.
fn is_loopback(base_url: &str) -> bool {
    let Some(rest) = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// One configured way to reach a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEndpoint {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier, chosen by the user or generated.
    pub endpoint_id: String,
    /// What to call it in an interface.
    pub display_name: String,
    /// Whether the router may consider it.
    ///
    /// Separate from deleting it: turning a cloud provider off for a while is
    /// a normal thing to want, and it should not cost the configuration.
    pub enabled: bool,
    /// Explicit model/endpoint capability configured by the user after verification.
    /// Unknown defaults to false; protocol/provider names never imply support.
    #[serde(default)]
    pub supports_tool_calls: bool,
    /// What it is and how to reach it.
    pub config: ProviderConfig,
    /// When it was configured.
    pub created_at: Timestamp,
}

impl ProviderEndpoint {
    /// Configure an endpoint.
    #[must_use]
    pub fn new(
        endpoint_id: impl Into<String>,
        display_name: impl Into<String>,
        config: ProviderConfig,
        created_at: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            endpoint_id: endpoint_id.into(),
            display_name: display_name.into(),
            enabled: true,
            supports_tool_calls: false,
            config,
            created_at,
        }
    }

    /// Where work sent to this endpoint runs.
    #[must_use]
    pub fn location(&self) -> ModelLocation {
        self.config.location()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::SecretStore;

    fn reference() -> SecretRef {
        SecretRef {
            key: "openai.apiKey".to_owned(),
            store: SecretStore::Platform,
        }
    }

    fn openai(base_url: &str) -> ProviderConfig {
        ProviderConfig::OpenAiCompatible {
            base_url: base_url.to_owned(),
            model_id: "m".to_owned(),
            credential: None,
        }
    }

    #[test]
    fn a_loopback_endpoint_is_local_and_a_remote_one_is_cloud() {
        for local in [
            "http://localhost:8080",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:1234",
            "https://localhost:8443",
        ] {
            assert_eq!(openai(local).location(), ModelLocation::Local, "{local}");
        }
        for cloud in ["https://api.openai.com", "http://192.168.1.5:8080"] {
            assert_eq!(openai(cloud).location(), ModelLocation::Cloud, "{cloud}");
        }
    }

    #[test]
    fn a_url_that_merely_mentions_loopback_is_not_local() {
        // Marking a third-party endpoint local would exempt it from
        // disclosure and let Local Only send work to it.
        for cloud in [
            "https://localhost.evil.test",
            "https://evil.test/localhost",
            "https://localhost@evil.test/",
            "https://evil.test?h=127.0.0.1",
            "https://notlocalhost",
        ] {
            assert_eq!(openai(cloud).location(), ModelLocation::Cloud, "{cloud}");
        }
    }

    #[test]
    fn anthropic_is_cloud_wherever_it_is_pointed() {
        // A loopback proxy in front of a third party is still a route off
        // this device, and the disclosure has to say so.
        let config = ProviderConfig::Anthropic {
            base_url: "http://localhost:9000".to_owned(),
            model_id: "claude".to_owned(),
            credential: reference(),
        };
        assert_eq!(config.location(), ModelLocation::Cloud);
    }

    #[test]
    fn a_supervised_runtime_is_local_by_construction() {
        let config = ProviderConfig::LlamaServer {
            binary_path: "/opt/llama-server".to_owned(),
            model_path: "/models/m.gguf".to_owned(),
            model_id: "m".to_owned(),
            extra_args: Vec::new(),
        };
        assert_eq!(config.location(), ModelLocation::Local);
        assert!(config.credential().is_none());
    }

    #[test]
    fn an_endpoint_serializes_its_reference_and_never_a_value() {
        let endpoint = ProviderEndpoint::new(
            "openai",
            "OpenAI",
            ProviderConfig::OpenAiCompatible {
                base_url: "https://api.openai.com".to_owned(),
                model_id: "gpt-oss".to_owned(),
                credential: Some(reference()),
            },
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let json = serde_json::to_value(&endpoint).unwrap();
        assert_eq!(json["config"]["kind"], "openAiCompatible");
        assert_eq!(json["config"]["credential"]["key"], "openai.apiKey");
        assert_eq!(json["config"]["credential"]["store"], "platform");
        // There is nowhere in the type for a value to go.
        assert!(
            !serde_json::to_string(&endpoint).unwrap().contains("sk-"),
            "a credential value reached the record"
        );
        // And no declared location to disagree with the derived one.
        assert!(!json.as_object().unwrap().contains_key("location"));
        assert_eq!(endpoint.location(), ModelLocation::Cloud);
    }
}
