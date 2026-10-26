//! The approval card's Rust half.
//!
//! A question raised anywhere in the core has to reach the window and wait for
//! an answer. This holds the pending questions and the channels their answers
//! come back on; the interface renders them and calls `resolve_approval`.
//!
//! Presentation only: which answers are offered comes from the core, and one
//! that was never offered is refused rather than honored.

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

/// Where a question is announced once it is waiting.
#[derive(Clone)]
enum Announce {
    /// The window this process opened.
    Window(AppHandle),
    /// No window at all, so nobody can be asked and every question is
    /// refused — the same fail-closed answer a headless CLI gives.
    ///
    /// Only a test can build one: the window's handle is the only way to
    /// construct an approver in a shipping build, so this is what that rule
    /// looks like when it is exercised rather than a state the product can
    /// reach.
    #[cfg(test)]
    Nobody,
    /// A test in the window's place. The question is queued and waited on
    /// exactly as it would be, and the test answers it through [`resolve`],
    /// as `resolve_approval` does for the window. Only the announcement is
    /// skipped, because there is no webview to hear it.
    ///
    /// [`resolve`]: DesktopApprover::resolve
    #[cfg(test)]
    Test,
}

/// Raises approval questions in the desktop window.
#[derive(Clone)]
pub(crate) struct DesktopApprover {
    /// Where a question is announced, and whether there is anyone to ask.
    announce: Announce,
    /// In the order the questions were asked, which is the order a person
    /// should be shown them.
    pending: Arc<Mutex<Vec<Waiting>>>,
}

/// One question waiting for an answer, and where the answer is sent.
///
/// The request is kept, not only the channel: a window that reloads loses the
/// event that raised its card, while the task goes on waiting here. Keeping it
/// is what lets the card come back instead of the task timing out against
/// nobody.
struct Waiting {
    request: ApprovalRequest,
    answer: tokio::sync::oneshot::Sender<ApprovalDecisionKind>,
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
            announce: Announce::Window(app),
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// An approver with no window, which refuses every question.
    #[cfg(test)]
    pub(crate) fn headless() -> Self {
        Self {
            announce: Announce::Nobody,
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// An approver that queues and waits, for a test answering in a person's
    /// place. See [`Announce::Test`].
    #[cfg(test)]
    pub(crate) fn waiting() -> Self {
        Self {
            announce: Announce::Test,
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Deliver a decision from the interface.
    ///
    /// Returns whether a question was waiting. A decision for an unknown or
    /// already-answered approval is reported rather than silently dropped, so
    /// a double-click cannot look like it worked twice.
    pub(crate) fn resolve(&self, approval_id: &ApprovalId, decision: ApprovalDecisionKind) -> bool {
        let Some(waiting) = self.take(approval_id) else {
            return false;
        };
        waiting.answer.send(decision).is_ok()
    }

    /// Every question still waiting, in the order they were asked.
    ///
    /// What a window asks for when it opens, so a question raised before it
    /// existed — or before it was reloaded — is still put to the person.
    pub(crate) fn pending(&self) -> Vec<ApprovalRequest> {
        self.pending.lock().map_or_else(
            |_| Vec::new(),
            |pending| {
                pending
                    .iter()
                    .map(|waiting| waiting.request.clone())
                    .collect()
            },
        )
    }

    /// Take one question out of the queue, if it is still in it.
    fn take(&self, approval_id: &ApprovalId) -> Option<Waiting> {
        let mut pending = self.pending.lock().ok()?;
        let at = pending
            .iter()
            .position(|waiting| waiting.request.approval_id == *approval_id)?;
        Some(pending.remove(at))
    }
}

/// Takes a question out of the queue however `ask` ends: answered, expired,
/// refused, or dropped because its task was cancelled.
struct Withdraw<'a>(&'a DesktopApprover, &'a ApprovalId);

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        self.0.take(self.1);
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
        #[cfg(test)]
        if matches!(self.announce, Announce::Nobody) {
            return Err(PermissionError::NoApprover);
        }

        let (sender, receiver) = tokio::sync::oneshot::channel();

        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| PermissionError::Unavailable("approval registry poisoned".into()))?;
            pending.push(Waiting {
                request: request.clone(),
                answer: sender,
            });
        }
        let _withdraw = Withdraw(self, &request.approval_id);

        // Registered before emitting, so an answer that arrives immediately
        // still finds the question waiting for it.
        if let Announce::Window(app) = &self.announce
            && app.emit(APPROVAL_REQUESTED, request).is_err()
        {
            return Err(PermissionError::NoApprover);
        }

        match tokio::time::timeout(ANSWER_TIMEOUT, receiver).await {
            Ok(Ok(decision)) => Ok(decision),
            // The window went away mid-question.
            Ok(Err(_)) => Err(PermissionError::NoApprover),
            Err(_) => Err(PermissionError::Expired),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
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

    /// A question waiting, as `ask` leaves one, without needing a window.
    fn asked(
        approver: &DesktopApprover,
        id: &str,
    ) -> tokio::sync::oneshot::Receiver<ApprovalDecisionKind> {
        let (answer, receiver) = tokio::sync::oneshot::channel();
        approver.pending.lock().unwrap().push(Waiting {
            request: question(id),
            answer,
        });
        receiver
    }

    #[test]
    fn a_window_that_opens_late_is_given_every_question_still_waiting() {
        let approver = DesktopApprover::headless();
        let _first = asked(&approver, "approval-1");
        let _second = asked(&approver, "approval-2");

        let waiting: Vec<String> = approver
            .pending()
            .iter()
            .map(|request| request.approval_id.to_string())
            .collect();
        assert_eq!(waiting, ["approval-1", "approval-2"]);
    }

    #[test]
    fn a_question_is_answered_once_and_is_then_no_longer_waiting() {
        let approver = DesktopApprover::headless();
        let mut receiver = asked(&approver, "approval-1");
        let id = ApprovalId::new("approval-1");

        assert!(approver.resolve(&id, ApprovalDecisionKind::AllowOnce));
        assert_eq!(receiver.try_recv(), Ok(ApprovalDecisionKind::AllowOnce));
        assert!(approver.pending().is_empty());
        // A second answer — a double click, or a card answered after it timed
        // out — is reported rather than silently accepted.
        assert!(!approver.resolve(&id, ApprovalDecisionKind::Deny));
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_nobody_answers_expires_and_is_refused() {
        // Fail closed: a task waiting on a card nobody looked at stops rather
        // than holding its worker forever, and the card does not linger to be
        // answered after the task has moved on.
        let approver = DesktopApprover::waiting();
        let asking = tokio::spawn({
            let approver = approver.clone();
            async move { approver.ask(&question("approval-1")).await }
        });
        tokio::task::yield_now().await;
        assert_eq!(approver.pending().len(), 1);

        tokio::time::advance(ANSWER_TIMEOUT + std::time::Duration::from_secs(1)).await;
        // Checked before awaiting: a paused clock jumps to the next timer
        // whenever nothing else can run, so awaiting first would expire any
        // timeout, however long.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(asking.is_finished(), "still waiting past the timeout");
        let answer = asking.await.unwrap();

        assert!(
            matches!(answer, Err(PermissionError::Expired)),
            "{answer:?}"
        );
        assert!(approver.pending().is_empty());
        assert!(!approver.resolve(
            &ApprovalId::new("approval-1"),
            ApprovalDecisionKind::AllowOnce
        ));
    }

    #[tokio::test]
    async fn a_question_whose_task_was_cancelled_is_withdrawn() {
        let approver = DesktopApprover::waiting();
        let asking = tokio::spawn({
            let approver = approver.clone();
            async move { approver.ask(&question("approval-1")).await }
        });
        while approver.pending().is_empty() {
            tokio::task::yield_now().await;
        }
        asking.abort();
        let _ = asking.await;
        assert!(approver.pending().is_empty());
        assert!(!approver.resolve(
            &ApprovalId::new("approval-1"),
            ApprovalDecisionKind::AllowOnce
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_answered_in_time_is_not_expired() {
        let approver = DesktopApprover::waiting();
        let asking = tokio::spawn({
            let approver = approver.clone();
            async move { approver.ask(&question("approval-1")).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(ANSWER_TIMEOUT - std::time::Duration::from_secs(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!asking.is_finished(), "gave up before the timeout");
        assert!(approver.resolve(
            &ApprovalId::new("approval-1"),
            ApprovalDecisionKind::AllowOnce
        ));
        assert_eq!(
            asking.await.unwrap().ok(),
            Some(ApprovalDecisionKind::AllowOnce)
        );
    }
}
