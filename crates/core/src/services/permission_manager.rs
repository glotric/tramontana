use std::sync::Arc;

use async_trait::async_trait;

use crate::contracts::{
    ApprovalDecision, ApprovalDecisionKind, ApprovalDetail, ApprovalId, ApprovalRequest,
    AuthorizedToolRequest, GrantKind, InvocationId, ProjectGrant, RiskClass, SCHEMA_VERSION,
    SideEffect, TaskId, Timestamp,
};
use crate::ports::{
    ApprovalStore, Approver, IdSource, PermissionError, PermissionOutcome, PermissionPolicy,
    PermissionQuery, reason,
};
use crate::services::is_app_tool;

/// Evaluates tool requests against policy, and is the only source of
/// [`AuthorizedToolRequest`].
///
/// Implements the fail-closed order in `SECURITY_AND_PERMISSIONS.md` §4:
///
/// 1. hard product denial;
/// 2. explicit user or project denial;
/// 3. *(organization policy — later phases)*;
/// 4. a valid narrow project grant;
/// 5. a valid once or task grant;
/// 6. the default for the risk class;
/// 7. approval required, or denied when nobody can be asked.
///
/// Every path that is not an explicit allow ends in a denial. There is no
/// branch that falls through to permitted.
pub struct PermissionManager {
    approvals: Arc<dyn ApprovalStore>,
    approver: Arc<dyn Approver>,
    ids: Arc<dyn IdSource>,
}

impl std::fmt::Debug for PermissionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionManager").finish_non_exhaustive()
    }
}

impl PermissionManager {
    /// Assemble a manager.
    #[must_use]
    pub fn new(
        approvals: Arc<dyn ApprovalStore>,
        approver: Arc<dyn Approver>,
        ids: Arc<dyn IdSource>,
    ) -> Self {
        Self {
            approvals,
            approver,
            ids,
        }
    }

    /// Evaluate a request and, if it passes, mint the authorization.
    ///
    /// This is the only way an [`AuthorizedToolRequest`] comes into existence,
    /// which is what makes "a tool provider cannot be called without a
    /// decision" true by construction rather than by discipline.
    ///
    /// # Errors
    ///
    /// Only when the decision could not be reached. A refusal is `Ok(Err(..))`
    /// in spirit — it returns the outcome, and the caller sees `Denied`.
    pub async fn authorize(
        &self,
        query: &PermissionQuery,
        task_id: &TaskId,
        invocation_id: &InvocationId,
        arguments: serde_json::Value,
        now: &Timestamp,
    ) -> Result<(PermissionOutcome, Option<AuthorizedToolRequest>), PermissionError> {
        let outcome = self.evaluate(query, task_id, invocation_id, now).await?;

        let PermissionOutcome::Allowed {
            reason_code,
            approval_id,
        } = &outcome
        else {
            return Ok((outcome, None));
        };

        let grant_kind = match *reason_code {
            reason::SAFE_DEFAULT => GrantKind::ImplicitSafeDefault,
            reason::APPROVED_ONCE => GrantKind::Once,
            _ => GrantKind::Project,
        };

        let authorized = AuthorizedToolRequest::new(
            crate::contracts::AuthorizedTarget {
                invocation_id: invocation_id.clone(),
                task_id: Some(task_id.clone()),
                tool_id: query.tool_id.clone(),
                arguments,
            },
            crate::contracts::GrantedAuthority {
                capability: query.capability.clone(),
                scope: query.scope.clone(),
                kind: grant_kind,
                approval_id: approval_id.clone(),
            },
        );

        Ok((outcome, Some(authorized)))
    }

    /// Authorize a read the person asked for themselves, outside any task
    /// (ADR 0032).
    ///
    /// Only what the policy already allows a task without asking: a read
    /// inside an opened project that releases nothing off the device.
    /// Anything else is refused, never asked. A person is not asked to
    /// approve their own read, and nothing they start this way may do more
    /// than read.
    #[must_use]
    pub fn authorize_person_read(
        &self,
        query: &PermissionQuery,
        invocation_id: &InvocationId,
        arguments: serde_json::Value,
    ) -> (PermissionOutcome, Option<AuthorizedToolRequest>) {
        let quiet_read = query.risk_class == RiskClass::Read
            && query.scope.project_id.is_some()
            && !query.scope.releases_data
            && !is_app_tool(&query.tool_id);
        if !quiet_read {
            return (
                PermissionOutcome::Denied {
                    reason_code: reason::PERSON_READ_ONLY,
                },
                None,
            );
        }
        let authorized = AuthorizedToolRequest::new(
            crate::contracts::AuthorizedTarget {
                invocation_id: invocation_id.clone(),
                task_id: None,
                tool_id: query.tool_id.clone(),
                arguments,
            },
            crate::contracts::GrantedAuthority {
                capability: query.capability.clone(),
                scope: query.scope.clone(),
                kind: GrantKind::ImplicitSafeDefault,
                approval_id: None,
            },
        );
        (
            PermissionOutcome::Allowed {
                reason_code: reason::SAFE_DEFAULT,
                approval_id: None,
            },
            Some(authorized),
        )
    }

    /// Whether a project grant already covers this request.
    async fn covered_by_grant(&self, query: &PermissionQuery) -> Result<bool, PermissionError> {
        let Some(project_id) = &query.scope.project_id else {
            return Ok(false);
        };
        let grants = self.approvals.list_grants(project_id).await?;

        Ok(grants.iter().any(|grant| {
            grant.capability == query.capability
                && grant.tool_id == query.tool_id
                && query.scope.is_covered_by(&grant.scope)
                // A grant never escalates: one issued for a read must not
                // later cover a modifying or dangerous use of the same tool.
                && risk_rank(query.risk_class) <= risk_rank(grant.risk_ceiling)
        }))
    }

    /// Ask the user, and honor the answer.
    async fn ask(
        &self,
        query: &PermissionQuery,
        task_id: &TaskId,
        invocation_id: &InvocationId,
        now: &Timestamp,
    ) -> Result<PermissionOutcome, PermissionError> {
        let approval_id = ApprovalId::new(self.ids.next("approval"));

        // `AllowProject` is offered only where the policy permits persistence,
        // so the interface cannot present a choice the core would refuse.
        let mut eligible = vec![ApprovalDecisionKind::AllowOnce, ApprovalDecisionKind::Deny];
        if query.persistable && query.scope.project_id.is_some() {
            eligible.insert(1, ApprovalDecisionKind::AllowProject);
        }

        let request = ApprovalRequest {
            schema_version: SCHEMA_VERSION,
            approval_id: approval_id.clone(),
            task_id: task_id.clone(),
            invocation_id: Some(invocation_id.clone()),
            capability: query.capability.clone(),
            tool_id: query.tool_id.clone(),
            requested_scope: query.scope.clone(),
            risk_class: query.risk_class,
            side_effect: side_effect_for(query.risk_class),
            summary: format!("{} wants to {}", query.tool_id, query.capability),
            details: describe(query),
            eligible_decisions: eligible,
        };

        self.approvals.record_request(&request).await?;

        let answer = match self.approver.ask(&request).await {
            Ok(answer) => answer,
            // Nobody to ask, or nobody answered. Neither is consent.
            Err(PermissionError::NoApprover) => {
                return Ok(PermissionOutcome::Denied {
                    reason_code: reason::NO_APPROVER,
                });
            }
            Err(PermissionError::Expired) => {
                return Ok(PermissionOutcome::Denied {
                    reason_code: reason::USER_DENIED,
                });
            }
            Err(other) => return Err(other),
        };

        self.approvals
            .record_decision(&ApprovalDecision::new(
                approval_id.clone(),
                answer,
                now.clone(),
            ))
            .await?;

        match answer {
            ApprovalDecisionKind::Deny => Ok(PermissionOutcome::Denied {
                reason_code: reason::USER_DENIED,
            }),
            ApprovalDecisionKind::AllowOnce => {
                // Claimed immediately and atomically. If this loses the race,
                // some other caller already spent the approval.
                if self.approvals.consume_once(&approval_id).await? {
                    Ok(PermissionOutcome::Allowed {
                        reason_code: reason::APPROVED_ONCE,
                        approval_id: Some(approval_id),
                    })
                } else {
                    Ok(PermissionOutcome::Denied {
                        reason_code: reason::ONCE_ALREADY_CONSUMED,
                    })
                }
            }
            ApprovalDecisionKind::AllowProject => {
                // Guard against an approver returning a choice that was never
                // offered; honoring it would persist a grant the policy said
                // must not be persistable.
                let Some(project_id) = query.scope.project_id.clone() else {
                    return Ok(PermissionOutcome::Denied {
                        reason_code: reason::NO_PROJECT,
                    });
                };
                if !query.persistable {
                    return Ok(PermissionOutcome::Denied {
                        reason_code: reason::USER_DENIED,
                    });
                }

                self.approvals
                    .save_grant(&ProjectGrant {
                        schema_version: SCHEMA_VERSION,
                        grant_id: self.ids.next("grant"),
                        project_id,
                        capability: query.capability.clone(),
                        tool_id: query.tool_id.clone(),
                        scope: query.scope.clone(),
                        risk_ceiling: query.risk_class,
                        granted_at: now.clone(),
                    })
                    .await?;

                Ok(PermissionOutcome::Allowed {
                    reason_code: reason::APPROVED_PROJECT,
                    approval_id: Some(approval_id),
                })
            }
        }
    }
}

#[async_trait]
impl PermissionPolicy for PermissionManager {
    async fn evaluate(
        &self,
        query: &PermissionQuery,
        task_id: &TaskId,
        invocation_id: &InvocationId,
        now: &Timestamp,
    ) -> Result<PermissionOutcome, PermissionError> {
        // 1. Hard denial: a projectless task has no filesystem scope at all.
        //    "Projectless tasks have no implicit filesystem or terminal
        //    access" (`PHASE_1_ACCEPTANCE_CRITERIA.md` §8).
        if query.scope.project_id.is_none() && !query.scope.roots.is_empty() {
            return Ok(PermissionOutcome::Denied {
                reason_code: reason::NO_PROJECT,
            });
        }

        // 4. A live project grant.
        match self.covered_by_grant(query).await {
            Ok(true) => {
                return Ok(PermissionOutcome::Allowed {
                    reason_code: reason::PROJECT_GRANT,
                    approval_id: None,
                });
            }
            Ok(false) => {}
            // 7. Policy could not be read. Unknown is not permitted.
            Err(PermissionError::Unavailable(_)) => {
                return Ok(PermissionOutcome::Denied {
                    reason_code: reason::POLICY_UNAVAILABLE,
                });
            }
            Err(other) => return Err(other),
        }

        // 6. The default for the risk class. A read inside an already-opened
        //    project is allowed without asking; anything else is a question.
        if query.risk_class == RiskClass::Read
            && query.scope.project_id.is_some()
            && !query.scope.releases_data
            // An App's claim to read only is its own (ADR 0035).
            && !is_app_tool(&query.tool_id)
        {
            return Ok(PermissionOutcome::Allowed {
                reason_code: reason::SAFE_DEFAULT,
                approval_id: None,
            });
        }

        // 7. Ask.
        self.ask(query, task_id, invocation_id, now).await
    }
}

/// Order the risk classes so a grant cannot cover something worse than itself.
fn risk_rank(risk: RiskClass) -> u8 {
    match risk {
        RiskClass::Read => 0,
        RiskClass::Modify => 1,
        RiskClass::Dangerous => 2,
    }
}

/// The side effect implied by a risk class, for the approval prompt.
fn side_effect_for(risk: RiskClass) -> SideEffect {
    match risk {
        RiskClass::Read => SideEffect::None,
        RiskClass::Modify => SideEffect::LocalReversible,
        RiskClass::Dangerous => SideEffect::LocalIrreversible,
    }
}

/// Describe a request for the prompt, without leaking anything.
fn describe(query: &PermissionQuery) -> Vec<ApprovalDetail> {
    let mut details = vec![
        ApprovalDetail {
            label: "Capability".to_owned(),
            value: query.capability.to_string(),
        },
        ApprovalDetail {
            label: "Risk".to_owned(),
            value: format!("{:?}", query.risk_class),
        },
    ];
    if let Some(project) = &query.scope.project_id {
        details.push(ApprovalDetail {
            label: "Project".to_owned(),
            value: project.to_string(),
        });
    }
    if !query.scope.roots.is_empty() {
        details.push(ApprovalDetail {
            label: "Within".to_owned(),
            value: query.scope.roots.join(", "),
        });
    }
    if is_app_tool(&query.tool_id) {
        // Whose tool this is, and that what it says about itself is its own
        // word (ADR 0035).
        details.push(ApprovalDetail {
            label: "App".to_owned(),
            value: query
                .tool_id
                .split('.')
                .nth(1)
                .unwrap_or_default()
                .to_owned(),
        });
        details.push(ApprovalDetail {
            label: "The App says".to_owned(),
            value: if query.risk_class == RiskClass::Read {
                "it only reads (not verified)"
            } else {
                "nothing about what it does"
            }
            .to_owned(),
        });
    }
    if query.scope.releases_data {
        details.push(ApprovalDetail {
            label: "Leaves this device".to_owned(),
            value: "yes".to_owned(),
        });
    }
    // What the core resolved for this request, such as a recipe's exact
    // command: a person approves what will run, not a tool's name.
    details.extend(query.details.iter().cloned());
    details
}
