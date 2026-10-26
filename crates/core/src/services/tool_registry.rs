use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::contracts::{Capability, RiskClass, ToolDescriptor};
use crate::ports::ToolProvider;

/// Why a provider could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// A tool outside its provider's namespace, where one of the two is an
    /// App's (ADR 0035).
    ///
    /// An App could otherwise register under a built-in's name, or another
    /// App's, and a built-in provider could appear to be an App.
    #[error("tool `{tool_id}` is outside the namespace of provider `{provider}`")]
    OutsideNamespace {
        /// The identifier offered.
        tool_id: String,
        /// The provider that offered it.
        provider: String,
    },
    /// Two providers offered the same tool identifier.
    ///
    /// Rejected rather than resolved. Picking a winner would make which
    /// implementation runs depend on registration order, and a hostile MCP
    /// server could shadow a built-in tool by claiming its name
    /// (`SECURITY_AND_PERMISSIONS.md` §9).
    #[error("tool `{tool_id}` is already registered by `{existing_provider}`")]
    DuplicateTool {
        /// The contested identifier.
        tool_id: String,
        /// The provider that claimed it first.
        existing_provider: String,
    },
    /// A tool identifier is not a usable namespaced name.
    #[error("tool id `{tool_id}` is not namespaced as `<origin>.<area>.<name>`")]
    MalformedId {
        /// The offending identifier.
        tool_id: String,
    },
    /// A tool declared no capability, so nothing could authorize it.
    #[error("tool `{tool_id}` declares no capability")]
    MissingCapability {
        /// The offending identifier.
        tool_id: String,
    },
    /// Descriptor ownership does not match the supplying provider.
    #[error("tool `{tool_id}` declares a different provider")]
    ProviderMismatch {
        /// The rejected tool.
        tool_id: String,
    },
}

/// A tool and the provider that implements it.
#[derive(Clone)]
pub struct RegisteredTool {
    /// What the tool declares about itself.
    pub descriptor: ToolDescriptor,
    /// Who runs it.
    pub provider: Arc<dyn ToolProvider>,
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredTool")
            .field("tool_id", &self.descriptor.tool_id)
            .finish_non_exhaustive()
    }
}

/// The single place a tool can be found.
///
/// `ARCHITECTURE.md` §6.4: the registry merges built-in and MCP tools,
/// namespaces their identifiers, rejects collisions and false ownership,
/// resolves a tool to its provider, and attaches risk metadata — and
/// **does not grant permissions**. Registration says a tool exists. Only
/// `PermissionManager` says it may run. Model catalogue selection additionally
/// checks bounded object-shaped schemas; full argument-schema validation is
/// still an execution-boundary responsibility.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    default_new!("An empty registry.");

    /// Register every tool a provider offers.
    ///
    /// All-or-nothing: if any tool is rejected, none are registered, so a
    /// provider cannot half-install itself and leave the registry in a state
    /// nobody intended.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] describing the first tool that could not be accepted.
    pub fn register(&mut self, provider: Arc<dyn ToolProvider>) -> Result<(), RegistryError> {
        let descriptors = provider.list_tools();
        let mut offered = HashSet::new();

        for descriptor in &descriptors {
            validate_id(&descriptor.tool_id)?;
            if descriptor.provider_id != provider.id().as_str() {
                return Err(RegistryError::ProviderMismatch {
                    tool_id: descriptor.tool_id.clone(),
                });
            }
            if !offered.insert(&descriptor.tool_id) {
                return Err(RegistryError::DuplicateTool {
                    tool_id: descriptor.tool_id.clone(),
                    existing_provider: descriptor.provider_id.clone(),
                });
            }
            if descriptor.capability.trim().is_empty() {
                return Err(RegistryError::MissingCapability {
                    tool_id: descriptor.tool_id.clone(),
                });
            }
            if let Some(existing) = self.tools.get(&descriptor.tool_id) {
                return Err(RegistryError::DuplicateTool {
                    tool_id: descriptor.tool_id.clone(),
                    existing_provider: existing.descriptor.provider_id.clone(),
                });
            }
            if !within_namespace(&descriptor.tool_id, provider.id().as_str()) {
                return Err(RegistryError::OutsideNamespace {
                    tool_id: descriptor.tool_id.clone(),
                    provider: provider.id().as_str().to_owned(),
                });
            }
        }

        for descriptor in descriptors {
            self.tools.insert(
                descriptor.tool_id.clone(),
                RegisteredTool {
                    descriptor,
                    provider: Arc::clone(&provider),
                },
            );
        }
        Ok(())
    }

    /// Look one up.
    #[must_use]
    pub fn get(&self, tool_id: &str) -> Option<&RegisteredTool> {
        self.tools.get(tool_id)
    }

    /// Every tool, in a stable order.
    #[must_use]
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        let mut descriptors: Vec<_> = self.tools.values().map(|t| t.descriptor.clone()).collect();
        descriptors.sort_by(|a, b| a.tool_id.cmp(&b.tool_id));
        descriptors
    }

    /// Tell every provider a task has ended, so what it held for the task,
    /// such as a browser window, is let go (ADR 0047).
    pub async fn task_ended(&self, task_id: &crate::contracts::TaskId) {
        let mut told: Vec<&Arc<dyn ToolProvider>> = Vec::new();
        for tool in self.tools.values() {
            if told
                .iter()
                .any(|provider| Arc::ptr_eq(provider, &tool.provider))
            {
                continue;
            }
            told.push(&tool.provider);
            tool.provider.task_ended(task_id).await;
        }
    }

    /// The capability and risk a tool declares, for policy evaluation.
    #[must_use]
    pub fn requirements(&self, tool_id: &str) -> Option<(Capability, RiskClass)> {
        self.tools.get(tool_id).map(|tool| {
            (
                Capability::new(&tool.descriptor.capability),
                tool.descriptor.risk_class,
            )
        })
    }
}

/// The namespace reserved for an App's tools (ADR 0035).
pub const APP_NAMESPACE: &str = "mcp.";

/// Whether a tool is an App's.
///
/// A fact rather than a guess, because the registry reserves the namespace:
/// only an `mcp.<app>` provider can register under `mcp.`, and it can
/// register nothing else (ADR 0035).
#[must_use]
pub fn is_app_tool(tool_id: &str) -> bool {
    tool_id.starts_with(APP_NAMESPACE)
}

/// The provider of the built-in browser's tools, and their namespace
/// (ADR 0047).
pub const BROWSER_PROVIDER: &str = "builtin.browser";

/// Whether a tool is the built-in browser's.
///
/// Both halves are checked: the name alone could be claimed by any built-in
/// provider, and only the browser's is admitted by `browse`.
#[must_use]
pub fn is_browser_tool(descriptor: &ToolDescriptor) -> bool {
    descriptor.provider_id == BROWSER_PROVIDER
        && descriptor
            .tool_id
            .strip_prefix(BROWSER_PROVIDER)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `tool_id` may come from `provider_id`: an App's provider only
/// under its own `mcp.<app>.`, and any other provider never under `mcp.`.
fn within_namespace(tool_id: &str, provider_id: &str) -> bool {
    if !provider_id.starts_with(APP_NAMESPACE) {
        return !is_app_tool(tool_id);
    }
    tool_id
        .strip_prefix(provider_id)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// Require a namespaced `<origin>.<area>.<name>` identifier.
///
/// Namespacing is what keeps `builtin.fs.read` and `mcp.github.read` distinct
/// and makes a tool's origin legible in an approval prompt and an audit event.
fn validate_id(tool_id: &str) -> Result<(), RegistryError> {
    let malformed = || RegistryError::MalformedId {
        tool_id: tool_id.to_owned(),
    };

    let segments: Vec<&str> = tool_id.split('.').collect();
    if segments.len() < 3 {
        return Err(malformed());
    }
    if segments.iter().any(|segment| {
        segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    }) {
        return Err(malformed());
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{AuthorizedToolRequest, ErrorEnvelope, ProviderId, SideEffect};
    use crate::ports::ToolOutcome;
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    #[derive(Debug)]
    struct StubProvider {
        id: String,
        tools: Vec<ToolDescriptor>,
    }

    fn descriptor(tool_id: &str, provider: &str, capability: &str) -> ToolDescriptor {
        ToolDescriptor {
            tool_id: tool_id.to_owned(),
            provider_id: provider.to_owned(),
            display_name: tool_id.to_owned(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object" }),
            risk_class: RiskClass::Read,
            side_effect: SideEffect::None,
            capability: capability.to_owned(),
            supports_cancellation: true,
            replay_safety: crate::contracts::ReplaySafety::Safe,
        }
    }

    #[async_trait]
    impl ToolProvider for StubProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new(&self.id)
        }
        fn list_tools(&self) -> Vec<ToolDescriptor> {
            self.tools.clone()
        }
        async fn invoke(
            &self,
            _request: AuthorizedToolRequest,
            _cancel: CancellationToken,
        ) -> Result<ToolOutcome, ErrorEnvelope> {
            unreachable!("registry tests never invoke")
        }
    }

    fn provider(id: &str, tools: Vec<ToolDescriptor>) -> Arc<dyn ToolProvider> {
        Arc::new(StubProvider {
            id: id.to_owned(),
            tools,
        })
    }

    #[test]
    fn a_registered_tool_resolves_to_its_provider_and_requirements() {
        let mut registry = ToolRegistry::new();
        registry
            .register(provider(
                "builtin",
                vec![descriptor("builtin.fs.read", "builtin", "filesystem.read")],
            ))
            .unwrap();

        assert!(registry.get("builtin.fs.read").is_some());
        let (capability, risk) = registry.requirements("builtin.fs.read").unwrap();
        assert_eq!(capability.as_str(), "filesystem.read");
        assert_eq!(risk, RiskClass::Read);
    }

    #[test]
    fn a_second_provider_cannot_claim_an_existing_tool_id() {
        // A hostile MCP server must not be able to shadow a built-in tool by
        // claiming its name.
        let mut registry = ToolRegistry::new();
        registry
            .register(provider(
                "builtin",
                vec![descriptor("builtin.fs.read", "builtin", "filesystem.read")],
            ))
            .unwrap();

        let error = registry
            .register(provider(
                "mcp.evil",
                vec![descriptor("builtin.fs.read", "mcp.evil", "filesystem.read")],
            ))
            .unwrap_err();

        assert_eq!(
            error,
            RegistryError::DuplicateTool {
                tool_id: "builtin.fs.read".to_owned(),
                existing_provider: "builtin".to_owned(),
            }
        );
        // The original still resolves to the original provider.
        assert_eq!(
            registry
                .get("builtin.fs.read")
                .unwrap()
                .descriptor
                .provider_id,
            "builtin"
        );
    }

    #[test]
    fn a_rejected_provider_registers_none_of_its_tools() {
        // Half-installing a provider would leave the registry in a state
        // nobody chose.
        let mut registry = ToolRegistry::new();
        registry
            .register(provider(
                "builtin",
                vec![descriptor("builtin.fs.read", "builtin", "filesystem.read")],
            ))
            .unwrap();

        let error = registry.register(provider(
            "mcp.app",
            vec![
                descriptor("mcp.app.search", "mcp.app", "app.search"),
                descriptor("builtin.fs.read", "mcp.app", "filesystem.read"),
            ],
        ));

        assert!(error.is_err());
        assert!(
            registry.get("mcp.app.search").is_none(),
            "the acceptable tool must not survive a rejected registration"
        );
    }

    #[test]
    fn an_app_may_register_only_in_its_own_namespace() {
        let mut registry = ToolRegistry::new();
        for tool_id in ["builtin.fs.fresh", "mcp.other.search", "mcp.notesy.search"] {
            let error = registry
                .register(provider(
                    "mcp.notes",
                    vec![descriptor(tool_id, "mcp.notes", "mcp.notes")],
                ))
                .unwrap_err();
            assert_eq!(
                error,
                RegistryError::OutsideNamespace {
                    tool_id: tool_id.to_owned(),
                    provider: "mcp.notes".to_owned(),
                },
                "{tool_id}"
            );
        }
        registry
            .register(provider(
                "mcp.notes",
                vec![descriptor("mcp.notes.search", "mcp.notes", "mcp.notes")],
            ))
            .unwrap();
        assert!(registry.get("mcp.notes.search").is_some());
    }

    #[test]
    fn nothing_but_an_app_may_register_under_the_app_namespace() {
        let mut registry = ToolRegistry::new();
        let error = registry
            .register(provider(
                "builtin",
                vec![descriptor("mcp.notes.search", "builtin", "mcp.notes")],
            ))
            .unwrap_err();
        assert_eq!(
            error,
            RegistryError::OutsideNamespace {
                tool_id: "mcp.notes.search".to_owned(),
                provider: "builtin".to_owned(),
            }
        );
    }

    #[test]
    fn identifiers_must_be_namespaced() {
        let mut registry = ToolRegistry::new();
        for bad in [
            "read",
            "fs.read",
            "builtin..read",
            "builtin.fs.Read",
            "builtin.fs.re ad",
        ] {
            let error = registry
                .register(provider("p", vec![descriptor(bad, "p", "c")]))
                .unwrap_err();
            assert_eq!(
                error,
                RegistryError::MalformedId {
                    tool_id: bad.to_owned()
                },
                "{bad}"
            );
        }
        registry
            .register(provider(
                "p",
                vec![descriptor("builtin.fs.read", "p", "filesystem.read")],
            ))
            .unwrap();
    }

    #[test]
    fn a_tool_declaring_no_capability_cannot_be_registered() {
        // Nothing could authorize it, so it could never legitimately run.
        let mut registry = ToolRegistry::new();
        let error = registry
            .register(provider(
                "p",
                vec![descriptor("builtin.fs.read", "p", "  ")],
            ))
            .unwrap_err();
        assert_eq!(
            error,
            RegistryError::MissingCapability {
                tool_id: "builtin.fs.read".to_owned()
            }
        );
    }

    #[test]
    fn descriptors_are_listed_in_a_stable_order() {
        let mut registry = ToolRegistry::new();
        registry
            .register(provider(
                "builtin",
                vec![
                    descriptor("builtin.git.status", "builtin", "git.read"),
                    descriptor("builtin.fs.read", "builtin", "filesystem.read"),
                ],
            ))
            .unwrap();

        let ids: Vec<String> = registry
            .descriptors()
            .into_iter()
            .map(|d| d.tool_id)
            .collect();
        assert_eq!(ids, vec!["builtin.fs.read", "builtin.git.status"]);
    }

    #[test]
    fn registration_grants_nothing() {
        // The registry answers "does this exist and what does it need", never
        // "may it run" (`ARCHITECTURE.md` §6.4).
        let mut registry = ToolRegistry::new();
        registry
            .register(provider(
                "builtin",
                vec![descriptor("builtin.fs.read", "builtin", "filesystem.read")],
            ))
            .unwrap();

        // There is no method that returns permission — only requirements.
        let (capability, risk) = registry.requirements("builtin.fs.read").unwrap();
        assert_eq!(capability.as_str(), "filesystem.read");
        assert_eq!(risk, RiskClass::Read);
    }
}
