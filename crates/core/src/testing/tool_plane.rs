use std::sync::Arc;

use crate::ports::{ApprovalStore, Approver, IdSource, ToolProvider};
use crate::services::{PermissionManager, RegistryError, ToolRegistry};

/// Assemble a registry and permission manager from a set of providers.
///
/// Both adapters and every integration test need the same five-line
/// incantation. Sharing it means the CLI and the desktop shell cannot drift
/// into registering different tools or evaluating them under different policy
/// — which would make the CLI a misleading way to inspect what the application
/// actually did.
///
/// # Errors
///
/// [`RegistryError`] when a provider offers a tool the registry refuses.
pub fn assemble_tool_plane(
    providers: Vec<Arc<dyn ToolProvider>>,
    approvals: Arc<dyn ApprovalStore>,
    approver: Arc<dyn Approver>,
    ids: Arc<dyn IdSource>,
) -> Result<(Arc<ToolRegistry>, Arc<PermissionManager>), RegistryError> {
    let mut registry = ToolRegistry::new();
    for provider in providers {
        registry.register(provider)?;
    }
    Ok((
        Arc::new(registry),
        Arc::new(PermissionManager::new(approvals, approver, ids)),
    ))
}
