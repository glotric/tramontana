use std::sync::Arc;

use crate::ports::{ApprovalStore, Approver, IdSource, ToolProvider};
use crate::services::{PermissionManager, RegistryError, ReleaseManager, ToolRegistry};

/// Assemble a registry, a permission manager, and a release manager.
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
/// A registry, a permission manager, and a release manager.
pub type AssembledPlane = (
    Arc<ToolRegistry>,
    Arc<PermissionManager>,
    Arc<ReleaseManager>,
);

pub fn assemble_tool_plane(
    providers: Vec<Arc<dyn ToolProvider>>,
    approvals: Arc<dyn ApprovalStore>,
    approver: Arc<dyn Approver>,
    ids: Arc<dyn IdSource>,
) -> Result<AssembledPlane, RegistryError> {
    let mut registry = ToolRegistry::new();
    for provider in providers {
        registry.register(provider)?;
    }
    Ok((
        Arc::new(registry),
        Arc::new(PermissionManager::new(
            Arc::clone(&approvals),
            Arc::clone(&approver),
            Arc::clone(&ids),
        )),
        // The same approver answers both kinds of question, so a release
        // prompt and a tool prompt reach the user by the same path and land in
        // the same approval ledger.
        Arc::new(ReleaseManager::new(approvals, Some(approver), ids)),
    ))
}
