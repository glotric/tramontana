use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::contracts::{
    ApprovalDecision, ApprovalDecisionKind, ApprovalId, ApprovalRequest, ProjectGrant, ProjectId,
};
use crate::ports::{ApprovalStore, Approver, PermissionError};

/// Approvals and grants held in memory.
///
/// `consume_once` claims under a single lock, which is what makes
/// "consumed exactly once" testable under concurrency. A read-then-write would
/// pass a single-threaded test and fail in production.
#[derive(Debug, Default)]
pub struct InMemoryApprovalStore {
    state: Mutex<ApprovalState>,
    /// When set, every call fails, so callers can be tested for fail-closed
    /// behavior when policy storage is unreadable.
    unavailable: Mutex<bool>,
}

#[derive(Debug, Default)]
struct ApprovalState {
    requests: Vec<ApprovalRequest>,
    decisions: Vec<ApprovalDecision>,
    consumed: HashSet<String>,
    grants: HashMap<ProjectId, Vec<ProjectGrant>>,
}

impl InMemoryApprovalStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent call report storage failure.
    pub fn break_storage(&self) {
        if let Ok(mut flag) = self.unavailable.lock() {
            *flag = true;
        }
    }

    /// Questions that were put to the user.
    #[must_use]
    pub fn requests(&self) -> Vec<ApprovalRequest> {
        self.state
            .lock()
            .map(|s| s.requests.clone())
            .unwrap_or_default()
    }

    /// Answers that were recorded.
    #[must_use]
    pub fn decisions(&self) -> Vec<ApprovalDecision> {
        self.state
            .lock()
            .map(|s| s.decisions.clone())
            .unwrap_or_default()
    }

    fn check(&self) -> Result<(), PermissionError> {
        let broken = self.unavailable.lock().map(|f| *f).unwrap_or(true);
        if broken {
            return Err(PermissionError::Unavailable("storage is broken".to_owned()));
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ApprovalState>, PermissionError> {
        self.state
            .lock()
            .map_err(|_| PermissionError::Unavailable("approval lock poisoned".to_owned()))
    }
}

#[async_trait]
impl ApprovalStore for InMemoryApprovalStore {
    async fn record_request(&self, request: &ApprovalRequest) -> Result<(), PermissionError> {
        self.check()?;
        self.lock()?.requests.push(request.clone());
        Ok(())
    }

    async fn record_decision(&self, decision: &ApprovalDecision) -> Result<(), PermissionError> {
        self.check()?;
        self.lock()?.decisions.push(decision.clone());
        Ok(())
    }

    async fn consume_once(&self, approval_id: &ApprovalId) -> Result<bool, PermissionError> {
        self.check()?;
        // Insert-and-report under one lock. `HashSet::insert` returns whether
        // the value was newly added, so exactly one caller can see `true`.
        Ok(self.lock()?.consumed.insert(approval_id.to_string()))
    }

    async fn save_grant(&self, grant: &ProjectGrant) -> Result<(), PermissionError> {
        self.check()?;
        self.lock()?
            .grants
            .entry(grant.project_id.clone())
            .or_default()
            .push(grant.clone());
        Ok(())
    }

    async fn list_grants(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<ProjectGrant>, PermissionError> {
        self.check()?;
        Ok(self
            .lock()?
            .grants
            .get(project_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn revoke_grant(&self, grant_id: &str) -> Result<bool, PermissionError> {
        self.check()?;
        let mut state = self.lock()?;
        let mut removed = false;
        for grants in state.grants.values_mut() {
            let before = grants.len();
            grants.retain(|grant| grant.grant_id != grant_id);
            removed |= grants.len() != before;
        }
        Ok(removed)
    }
}

/// An approver that answers the same way every time.
#[derive(Debug)]
pub struct ScriptedApprover {
    answer: Result<ApprovalDecisionKind, PermissionError>,
    asked: Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedApprover {
    /// Always answer `answer`.
    #[must_use]
    pub fn always(answer: ApprovalDecisionKind) -> Self {
        Self {
            answer: Ok(answer),
            asked: Mutex::new(Vec::new()),
        }
    }

    /// Always fail with `error`, as a headless context would.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            answer: Err(PermissionError::NoApprover),
            asked: Mutex::new(Vec::new()),
        }
    }

    /// The questions this approver was asked.
    #[must_use]
    pub fn asked(&self) -> Vec<ApprovalRequest> {
        self.asked.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Approver for ScriptedApprover {
    async fn ask(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(request.clone());
        }
        self.answer.clone()
    }
}
