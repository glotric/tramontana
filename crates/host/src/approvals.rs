//! Approval questions, put to the desktops connected to a host.
//!
//! The host's half of the desktop's approval card (`apps/desktop/src-tauri/
//! src/approval.rs`), with the window replaced by every admitted desktop:
//!
//! - a question is sent to each connected desktop as it is asked, and to each
//!   desktop that connects while it is still waiting;
//! - the first allowed answer wins, and every desktop is told the question
//!   is settled, so its card goes away;
//! - an answer that was never offered is refused, here at the boundary and
//!   again by the permission manager;
//! - with nobody connected the question waits, and after five minutes it
//!   expires and is refused. Never allowed by default.
//
// shortcut: every admitted desktop belongs to SP1's one person, so all of
// them are asked. SP5 routes a question to the driver, then the launcher,
// then the owner, and never lets a contributor answer a command (ADR 0077).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ApprovalId, ApprovalRequest, CorrelationId, ErrorCode, ErrorEnvelope,
};
use tramontana_core::ports::{Approver, PermissionError};

use crate::protocol::ServerFrame;
use crate::server::send_or_close;

/// How long a question waits before it is treated as unanswered: the same
/// five minutes as the desktop's card.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// Puts approval questions to the desktops connected to this host.
pub struct RemoteApprover {
    timeout: Duration,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// In the order asked, which is the order a person should see them.
    pending: Vec<Waiting>,
    desktops: HashMap<u64, Desktop>,
    next_desktop: u64,
}

struct Waiting {
    request: ApprovalRequest,
    answer: oneshot::Sender<ApprovalDecisionKind>,
}

struct Desktop {
    outgoing: mpsc::Sender<ServerFrame>,
    closing: CancellationToken,
}

impl std::fmt::Debug for RemoteApprover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteApprover")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Default for RemoteApprover {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteApprover {
    /// An approver with the desktop's five-minute timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::with_timeout(ANSWER_TIMEOUT)
    }

    /// An approver whose questions expire after `timeout`.
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// A desktop was admitted: ask it everything still waiting, and every
    /// later question until [`detach`](Self::detach). Returns its handle.
    pub(crate) fn attach(
        &self,
        outgoing: mpsc::Sender<ServerFrame>,
        closing: CancellationToken,
    ) -> u64 {
        let mut inner = self.lock();
        for waiting in &inner.pending {
            send_or_close(
                &outgoing,
                &closing,
                ServerFrame::ApprovalRequested {
                    request: Box::new(waiting.request.clone()),
                },
            );
        }
        inner.next_desktop += 1;
        let id = inner.next_desktop;
        inner.desktops.insert(id, Desktop { outgoing, closing });
        id
    }

    /// The desktop has gone. Its questions keep waiting for another.
    pub(crate) fn detach(&self, desktop: u64) {
        self.lock().desktops.remove(&desktop);
    }

    /// Every question still waiting, oldest first.
    pub(crate) fn pending(&self) -> Vec<ApprovalRequest> {
        self.lock()
            .pending
            .iter()
            .map(|waiting| waiting.request.clone())
            .collect()
    }

    /// A desktop's answer.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when nothing is waiting under that id — answered
    /// already, or expired; [`ErrorCode::InvalidRequest`] for an answer that
    /// was not offered, which leaves the question waiting.
    pub(crate) fn resolve(
        &self,
        approval_id: &ApprovalId,
        decision: ApprovalDecisionKind,
    ) -> Result<(), ErrorEnvelope> {
        let mut inner = self.lock();
        let Some(at) = inner
            .pending
            .iter()
            .position(|waiting| waiting.request.approval_id == *approval_id)
        else {
            return Err(refused(
                ErrorCode::NotFound,
                "that approval is no longer waiting for an answer",
            ));
        };
        if !inner.pending[at]
            .request
            .eligible_decisions
            .contains(&decision)
        {
            return Err(refused(
                ErrorCode::InvalidRequest,
                "that answer was not offered for this approval",
            ));
        }
        let waiting = inner.pending.remove(at);
        let _ = waiting.answer.send(decision);
        settle(&inner, approval_id);
        Ok(())
    }

    /// Take a question out of the queue, if it is still there, and tell every
    /// desktop it is settled.
    fn withdraw(&self, approval_id: &ApprovalId) {
        let mut inner = self.lock();
        inner
            .pending
            .retain(|waiting| waiting.request.approval_id != *approval_id);
        settle(&inner, approval_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Nothing in here is left half-changed by a panic: every change is
        // one push, insert or remove.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn settle(inner: &Inner, approval_id: &ApprovalId) {
    for desktop in inner.desktops.values() {
        send_or_close(
            &desktop.outgoing,
            &desktop.closing,
            ServerFrame::ApprovalSettled {
                approval_id: approval_id.clone(),
            },
        );
    }
}

fn refused(code: ErrorCode, message: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("host"))
}

#[async_trait]
impl Approver for RemoteApprover {
    async fn ask(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        let (answer, receiver) = oneshot::channel();
        {
            // Queued and announced under one lock, so a desktop attaching at
            // the same moment sees the question exactly once.
            let mut inner = self.lock();
            inner.pending.push(Waiting {
                request: request.clone(),
                answer,
            });
            for desktop in inner.desktops.values() {
                send_or_close(
                    &desktop.outgoing,
                    &desktop.closing,
                    ServerFrame::ApprovalRequested {
                        request: Box::new(request.clone()),
                    },
                );
            }
        }
        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(_)) => Err(PermissionError::NoApprover),
            Err(_) => {
                self.withdraw(&request.approval_id);
                Err(PermissionError::Expired)
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use tramontana_core::contracts::{
        Capability, CapabilityScope, RiskClass, SCHEMA_VERSION, SideEffect, TaskId,
    };

    use super::*;

    fn question(id: &str) -> ApprovalRequest {
        ApprovalRequest {
            schema_version: SCHEMA_VERSION,
            approval_id: ApprovalId::new(id),
            task_id: TaskId::new("task-1"),
            invocation_id: None,
            capability: Capability::new("fs.write"),
            tool_id: "builtin.fs.patch".to_owned(),
            requested_scope: CapabilityScope {
                project_id: None,
                roots: Vec::new(),
                releases_data: false,
            },
            risk_class: RiskClass::Modify,
            side_effect: SideEffect::LocalReversible,
            summary: "patch a file".to_owned(),
            details: Vec::new(),
            eligible_decisions: vec![ApprovalDecisionKind::AllowOnce, ApprovalDecisionKind::Deny],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_nobody_answers_expires_and_is_refused() {
        let approver = RemoteApprover::new();
        assert!(matches!(
            approver.ask(&question("a-1")).await,
            Err(PermissionError::Expired)
        ));
        assert!(approver.pending().is_empty());
    }

    #[tokio::test]
    async fn an_answer_not_offered_is_refused_and_the_question_keeps_waiting() {
        let approver = std::sync::Arc::new(RemoteApprover::new());
        let asking = {
            let approver = std::sync::Arc::clone(&approver);
            tokio::spawn(async move { approver.ask(&question("a-1")).await })
        };
        while approver.pending().is_empty() {
            tokio::task::yield_now().await;
        }
        let id = ApprovalId::new("a-1");
        assert_eq!(
            approver
                .resolve(&id, ApprovalDecisionKind::AllowProject)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(approver.pending().len(), 1);
        approver.resolve(&id, ApprovalDecisionKind::Deny).unwrap();
        assert_eq!(asking.await.unwrap().unwrap(), ApprovalDecisionKind::Deny);
        assert_eq!(
            approver
                .resolve(&id, ApprovalDecisionKind::Deny)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
}
