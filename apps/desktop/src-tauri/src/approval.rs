//! The approval card's Rust half.
//!
//! A question raised anywhere in the core has to reach the window and wait for
//! an answer. This holds the pending questions and the channels their answers
//! come back on; the interface renders them and calls `resolve_approval`.
//!
//! Presentation only: which answers are offered comes from the core, and one
//! that was never offered is refused rather than honored.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tauri::{AppHandle, Emitter};
use tramontana_core::contracts::{ApprovalDecisionKind, ApprovalId, ApprovalRequest};
use tramontana_core::ports::{Approver, PermissionError};

/// How long a question waits before it is treated as unanswered.
///
/// A task blocked forever on a card nobody looked at is worse than one that
/// stopped: it holds its worker, and the user has no way to see why. Timing
/// out fails closed.
const ANSWER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The event the interface listens for.
pub(crate) const APPROVAL_REQUESTED: &str = "tramontana://approval-requested";

/// Raises approval questions in the desktop window.
#[derive(Clone)]
pub(crate) struct DesktopApprover {
    /// The window to raise questions in.
    ///
    /// Absent when there is no window — a test, or a profile opened before the
    /// interface exists. `ask` then refuses, which is the same fail-closed
    /// answer a headless CLI gives.
    app: Option<AppHandle>,
    pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApprovalDecisionKind>>>>,
}

impl std::fmt::Debug for DesktopApprover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopApprover").finish_non_exhaustive()
    }
}

impl DesktopApprover {
    /// An approver bound to a window.
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            app: Some(app),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// An approver with no window, which refuses every question.
    #[cfg(test)]
    pub(crate) fn headless() -> Self {
        Self {
            app: None,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Deliver a decision from the interface.
    ///
    /// Returns whether a question was waiting. A decision for an unknown or
    /// already-answered approval is reported rather than silently dropped, so
    /// a double-click cannot look like it worked twice.
    pub(crate) fn resolve(&self, approval_id: &ApprovalId, decision: ApprovalDecisionKind) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        let Some(sender) = pending.remove(approval_id.as_str()) else {
            return false;
        };
        sender.send(decision).is_ok()
    }
}

#[async_trait]
impl Approver for DesktopApprover {
    async fn ask(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        // No window means no one to ask. Refusing is not a denial by the
        // user; it is the absence of a user, and both fail closed.
        let Some(app) = self.app.clone() else {
            return Err(PermissionError::NoApprover);
        };

        let (sender, receiver) = tokio::sync::oneshot::channel();

        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| PermissionError::Unavailable("approval registry poisoned".into()))?;
            pending.insert(request.approval_id.to_string(), sender);
        }

        // Registered before emitting, so an answer that arrives immediately
        // still finds the question waiting for it.
        if app.emit(APPROVAL_REQUESTED, request).is_err() {
            self.pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(request.approval_id.as_str()));
            return Err(PermissionError::NoApprover);
        }

        match tokio::time::timeout(ANSWER_TIMEOUT, receiver).await {
            Ok(Ok(decision)) => Ok(decision),
            // The window went away mid-question.
            Ok(Err(_)) => Err(PermissionError::NoApprover),
            Err(_) => {
                self.pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(request.approval_id.as_str()));
                Err(PermissionError::Expired)
            }
        }
    }
}
