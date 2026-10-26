//! Deciding whether a payload may leave, and recording exactly what left.
//!
//! `SECURITY_AND_PERMISSIONS.md` §8 and `BUILD_ORDER.md` §14 item 5. This is
//! the model half of the boundary M3 built for tools: a provider adapter takes
//! an [`AuthorizedModelRequest`], and the only way one exists is through
//! [`ReleaseManager::authorize`]. "A cloud endpoint cannot be reached without
//! a release decision" is therefore true by construction rather than by
//! everyone remembering.
//!
//! Two rules do the work.
//!
//! **The plan is computed from the payload, never from the manifest alone.**
//! `TESTING_STRATEGY.md` §5 asks for a test that the disclosed source
//! categories and bytes match the transmitted ones. A plan derived from the
//! assembled context would pass that test by construction and be wrong in
//! practice, because assembling is not sending: a source can be gathered and
//! never used. Deriving it from the bytes being handed over makes the mismatch
//! unrepresentable in the safe direction, and the unsafe direction — a payload
//! carrying something the manifest never accounted for — an error.
//!
//! **Local costs nothing and cloud always asks.** A local destination is
//! authorized without a question, because no data left the device and a prompt
//! that fires when nothing is at stake trains people to dismiss prompts. A
//! cloud destination is asked for every request: `CORE_CONTRACTS.md` §9 makes
//! external actions non-persistable, and the standing policy for cloud use is
//! already the routing mode. A second standing grant would be a second place
//! the same question is answered, and the more permissive of the two would win
//! without the user knowing which one they were setting.

use std::sync::Arc;

use crate::contracts::{
    ApprovalDecision, ApprovalDecisionKind, ApprovalDetail, ApprovalId, ApprovalRequest,
    AuthorizedModelRequest, Capability, CapabilityScope, ContextCategory, ContextManifest,
    ContextReleasePlan, DataClass, ErrorCode, ExecutionLocation, ModelRequest,
    ReleaseAuthorization, ReleaseExclusion, ReleasedCategory, RiskClass, RoutingDecision,
    RoutingMode, SCHEMA_VERSION, SideEffect, Timestamp, content_hash, release_reason,
};
use crate::ports::{ApprovalStore, Approver, IdSource, PermissionError};

/// The capability a cloud release asks for.
///
/// Named like a tool capability because it is one: it appears in the same
/// approval ledger, and a person reading their approval history should not
/// have to learn a second vocabulary for "something left this device".
pub const RELEASE_CAPABILITY: &str = "model.contextRelease";

/// The tool identifier a release approval is recorded against.
const RELEASE_TOOL_ID: &str = "model.stream";

/// Why a release could not be planned or authorized.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReleaseError {
    /// The request and the manifest describe different assemblies.
    #[error("the request names context {found}, but the manifest is {expected}")]
    ManifestMismatch {
        /// The manifest that was supplied.
        expected: String,
        /// The manifest the request claims.
        found: String,
    },
    /// The payload carries content the assembled context never accounted for.
    ///
    /// The dangerous direction, and the reason the plan is derived from the
    /// payload: a request that has grown a source since the context was
    /// assembled would otherwise be disclosed as the smaller thing.
    #[error("the payload carries content no assembled source accounts for: {content_hash}")]
    UndisclosedContent {
        /// Hash of the offending content, never the content.
        content_hash: String,
    },
    /// A part of the payload this build cannot describe.
    ///
    /// Refused rather than sent: releasing something the disclosure cannot
    /// name would make the record a guess.
    #[error("the payload carries a part this build cannot describe")]
    OpaquePayloadPart,
    /// The normalized request cannot be represented safely by this build.
    #[error("invalid model payload: {0:?}")]
    InvalidPayload(ErrorCode),
    /// The release was refused.
    #[error("the release was refused: {reason_code}")]
    Denied {
        /// Stable reason, from [`release_reason`].
        reason_code: &'static str,
    },
    /// The decision itself could not be reached.
    #[error("release policy unavailable: {0}")]
    Unavailable(String),
}

/// Everything about a task that a release decision turns on.
///
/// Bundled because the four are meaningless apart: a manifest with no decision
/// names no destination, and a decision with no scope authorizes nothing in
/// particular.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseContext<'a> {
    /// What was assembled for this task.
    pub manifest: &'a ContextManifest,
    /// The routing decision that chose the destination.
    pub decision: &'a RoutingDecision,
    /// The standing policy in force.
    pub mode: RoutingMode,
    /// The limits the task runs under. A release turns on `releases_data`
    /// whatever the task's own scope says, because that is what it does.
    pub scope: &'a CapabilityScope,
}

/// Plans releases and is the only source of [`AuthorizedModelRequest`].
pub struct ReleaseManager {
    approvals: Arc<dyn ApprovalStore>,
    approver: Option<Arc<dyn Approver>>,
    ids: Arc<dyn IdSource>,
}

opaque_debug!(ReleaseManager);

impl ReleaseManager {
    /// Assemble a manager.
    ///
    /// `approver` is optional because a headless run genuinely has nobody to
    /// ask. That is not the same as a refusal, and it still fails closed: a
    /// cloud release with no approver is denied with
    /// [`release_reason::NO_APPROVER`].
    #[must_use]
    pub fn new(
        approvals: Arc<dyn ApprovalStore>,
        approver: Option<Arc<dyn Approver>>,
        ids: Arc<dyn IdSource>,
    ) -> Self {
        Self {
            approvals,
            approver,
            ids,
        }
    }

    /// Describe what `request` would release, without releasing it.
    ///
    /// This is what an interface shows before anyone decides. The returned
    /// plan is a proposal: its `authorization` is absent until
    /// [`Self::authorize`] fills it in.
    ///
    /// # Errors
    ///
    /// [`ReleaseError::ManifestMismatch`] when the request and manifest
    /// disagree, [`ReleaseError::UndisclosedContent`] when the payload carries
    /// something the context never assembled (or its byte count differs), or
    /// [`ReleaseError::InvalidPayload`] when the request is malformed,
    /// unsupported or exceeds input bounds.
    pub fn plan(
        &self,
        release_id: impl Into<String>,
        request: &ModelRequest,
        context: &ReleaseContext<'_>,
        now: Timestamp,
    ) -> Result<ContextReleasePlan, ReleaseError> {
        let (manifest, decision) = (context.manifest, context.decision);
        if request.context_manifest_id != manifest.context_manifest_id {
            return Err(ReleaseError::ManifestMismatch {
                expected: manifest.context_manifest_id.clone(),
                found: request.context_manifest_id.clone(),
            });
        }
        if decision.context_manifest_id != manifest.context_manifest_id {
            return Err(ReleaseError::ManifestMismatch {
                expected: manifest.context_manifest_id.clone(),
                found: decision.context_manifest_id.clone(),
            });
        }

        // Every distinct piece of content the payload carries, by hash. A
        // source repeated across two messages is one release of one source,
        // which is why this is a set rather than a running total.
        let carried = super::model_input::model_input_content(request)
            .map_err(|error| ReleaseError::InvalidPayload(error.code))?;

        // Match the payload against what was assembled. A hash the manifest
        // does not account for is the failure this whole type exists to make
        // impossible to ship quietly.
        let mut released: Vec<ReleasedCategory> = Vec::new();
        let mut transmitted_source_ids = Vec::new();
        let mut redactions: Vec<String> = Vec::new();
        let mut released_hashes = std::collections::BTreeSet::new();
        for content in &carried {
            let hash = content_hash(content);
            if !released_hashes.insert(hash.clone()) {
                continue;
            }
            let Some(source) = manifest.sources.iter().find(|source| {
                source.included
                    && source.content_hash == hash
                    && source.byte_count == content.len() as u64
            }) else {
                return Err(ReleaseError::UndisclosedContent { content_hash: hash });
            };
            match released.iter_mut().find(|entry| {
                entry.category == source.category && entry.data_class == source.data_class
            }) {
                Some(entry) => {
                    entry.source_count += 1;
                    entry.byte_count += source.byte_count;
                }
                None => released.push(ReleasedCategory {
                    category: source.category,
                    data_class: source.data_class,
                    source_count: 1,
                    byte_count: source.byte_count,
                }),
            }
            transmitted_source_ids.push(source.source_id.clone());
            // Redactions belong to the source they were made in, so the plan
            // reports what applies to the payload rather than every redaction
            // the task ever made. A disclosure listing a redaction from a
            // source that was never sent describes a different release.
            redactions.extend(source.redactions.iter().cloned());
        }
        redactions.sort_unstable();
        redactions.dedup();
        // Ordered explicitly rather than by however the payload happened to be
        // laid out, so two identical releases produce identical records and
        // can be compared rather than only read — the same property the router
        // gives its reason codes.
        released.sort_by_key(|entry| (strength(&entry.data_class), category_key(entry.category)));

        // Assembled and not carried. Recorded rather than dropped: "gathered
        // but not sent" is the answer to "did my instructions reach the
        // model?", and a plan that listed only what went could not give it.
        let exclusions = manifest
            .sources
            .iter()
            .filter(|source| !transmitted_source_ids.contains(&source.source_id))
            .map(|source| ReleaseExclusion {
                source_id: source.source_id.clone(),
                category: source.category,
                reason_code: source
                    .exclusion_reason
                    .clone()
                    .unwrap_or_else(|| release_reason::NOT_TRANSMITTED.to_owned()),
            })
            .collect();

        let mut data_classes: Vec<DataClass> =
            released.iter().map(|entry| entry.data_class).collect();
        data_classes.sort_unstable_by_key(strength);
        data_classes.dedup();

        Ok(ContextReleasePlan {
            schema_version: SCHEMA_VERSION,
            release_id: release_id.into(),
            task_id: request.task_id.clone(),
            request_id: request.request_id.clone(),
            decision_id: decision.decision_id.clone(),
            context_manifest_id: manifest.context_manifest_id.clone(),
            destination: decision.model.clone(),
            requires_disclosure: matches!(decision.location, ExecutionLocation::Cloud { .. }),
            location: decision.location.clone(),
            reason_codes: decision.reason_codes.clone(),
            message_ids: request
                .messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect(),
            total_bytes: released.iter().map(|entry| entry.byte_count).sum(),
            released,
            data_classes,
            total_token_estimate: manifest.total_token_estimate,
            redactions,
            exclusions,
            tools_offered: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            authorization: None,
            created_at: now,
        })
    }

    /// Decide whether `request` may go, and bind the answer to it.
    ///
    /// The returned [`AuthorizedModelRequest`] owns both the payload and the
    /// finalized plan, so the thing that was disclosed and the thing that is
    /// sent cannot come apart afterwards.
    ///
    /// # Errors
    ///
    /// Anything [`Self::plan`] can return, plus [`ReleaseError::Denied`] when
    /// the user refuses, nobody can be asked, or no answer arrives.
    pub async fn authorize(
        &self,
        release_id: impl Into<String>,
        request: ModelRequest,
        context: &ReleaseContext<'_>,
        now: Timestamp,
    ) -> Result<AuthorizedModelRequest, ReleaseError> {
        let mut plan = self.plan(release_id, &request, context, now.clone())?;

        let authorization = if plan.requires_disclosure {
            self.ask(&plan, context, now).await?
        } else {
            ReleaseAuthorization {
                reason_code: release_reason::LOCAL_DESTINATION.to_owned(),
                approval_id: None,
                mode: context.mode,
            }
        };
        plan.authorization = Some(authorization);

        Ok(AuthorizedModelRequest::new(request, plan))
    }

    /// Put the release to the user and record both halves of the exchange.
    async fn ask(
        &self,
        plan: &ContextReleasePlan,
        context: &ReleaseContext<'_>,
        now: Timestamp,
    ) -> Result<ReleaseAuthorization, ReleaseError> {
        let Some(approver) = self.approver.as_ref() else {
            return Err(ReleaseError::Denied {
                reason_code: release_reason::NO_APPROVER,
            });
        };

        let approval_id = ApprovalId::new(self.ids.next("approval"));
        let request = ApprovalRequest {
            schema_version: SCHEMA_VERSION,
            approval_id: approval_id.clone(),
            task_id: plan.task_id.clone(),
            invocation_id: None,
            capability: Capability::new(RELEASE_CAPABILITY),
            tool_id: RELEASE_TOOL_ID.to_owned(),
            // The task's own scope with the release dimension turned on:
            // "a read action that sends content to a cloud endpoint is not
            // treated as a harmless local read"
            // (`SECURITY_AND_PERMISSIONS.md` §3.1).
            requested_scope: CapabilityScope {
                releases_data: true,
                ..context.scope.clone()
            },
            // Reaching a third party is external and cannot be taken back.
            risk_class: RiskClass::Dangerous,
            side_effect: SideEffect::External,
            summary: plan.summary(),
            details: details_of(plan),
            // No `AllowProject`: the standing policy for cloud use is the
            // routing mode, and a grant would be a second answer to the same
            // question (`CORE_CONTRACTS.md` §9).
            eligible_decisions: vec![ApprovalDecisionKind::AllowOnce, ApprovalDecisionKind::Deny],
        };
        self.approvals
            .record_request(&request)
            .await
            .map_err(unavailable)?;

        let answer = match approver.ask(&request).await {
            Ok(answer) => answer,
            Err(PermissionError::NoApprover) => {
                return Err(ReleaseError::Denied {
                    reason_code: release_reason::NO_APPROVER,
                });
            }
            Err(PermissionError::Expired) => {
                return Err(ReleaseError::Denied {
                    reason_code: release_reason::APPROVAL_EXPIRED,
                });
            }
            Err(error) => return Err(unavailable(error)),
        };
        self.approvals
            .record_decision(&ApprovalDecision::new(
                approval_id.clone(),
                answer,
                now.clone(),
            ))
            .await
            .map_err(unavailable)?;

        match answer {
            ApprovalDecisionKind::AllowOnce | ApprovalDecisionKind::AllowProject => {}
            // A site is never offered for a release: an answer that was not
            // offered is not consent.
            ApprovalDecisionKind::Deny | ApprovalDecisionKind::AllowSite => {
                return Err(ReleaseError::Denied {
                    reason_code: release_reason::USER_DENIED,
                });
            }
        }

        // An allow-once approval is claimed exactly once, so a plan cannot be
        // approved and then replayed against a second payload.
        if !self
            .approvals
            .consume_once(&approval_id)
            .await
            .map_err(unavailable)?
        {
            return Err(ReleaseError::Denied {
                reason_code: release_reason::USER_DENIED,
            });
        }

        Ok(ReleaseAuthorization {
            reason_code: release_reason::APPROVED_ONCE.to_owned(),
            approval_id: Some(approval_id),
            mode: context.mode,
        })
    }
}

/// The labelled facts shown beneath the prompt summary.
///
/// Every one of them is a count, a class, or a name — never content.
fn details_of(plan: &ContextReleasePlan) -> Vec<ApprovalDetail> {
    let mut details = vec![
        ApprovalDetail {
            label: "Destination".to_owned(),
            value: format!(
                "{} / {}",
                plan.destination.provider_id, plan.destination.model_id
            ),
        },
        ApprovalDetail {
            label: "Reason".to_owned(),
            value: plan.reason_codes.join(", "),
        },
        ApprovalDetail {
            label: "Messages".to_owned(),
            value: plan.message_ids.len().to_string(),
        },
        ApprovalDetail {
            label: "Sources".to_owned(),
            value: plan
                .released
                .iter()
                .map(|entry| {
                    format!(
                        "{:?} x{} ({} bytes)",
                        entry.category, entry.source_count, entry.byte_count
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        },
    ];
    if !plan.exclusions.is_empty() {
        details.push(ApprovalDetail {
            label: "Withheld".to_owned(),
            value: plan.exclusions.len().to_string(),
        });
    }
    if !plan.tools_offered.is_empty() {
        details.push(ApprovalDetail {
            label: "Tools offered".to_owned(),
            value: plan.tools_offered.join(", "),
        });
    }
    details
}

/// Storage failure, in the release vocabulary.
fn unavailable(error: PermissionError) -> ReleaseError {
    ReleaseError::Unavailable(error.to_string())
}

/// Sort key for context categories.
///
/// Explicit rather than derived from declaration order: the order a record is
/// written in should not change because someone reordered an enum.
fn category_key(category: ContextCategory) -> u8 {
    match category {
        ContextCategory::Conversation => 0,
        ContextCategory::Instruction => 1,
        ContextCategory::Skill => 2,
        ContextCategory::SelectedFile => 3,
        ContextCategory::RepositoryMetadata => 4,
        ContextCategory::ToolResult => 5,
    }
}

/// Sort key for data classes, strongest first.
fn strength(class: &DataClass) -> u8 {
    match class {
        DataClass::Secret => 0,
        DataClass::Sensitive => 1,
        DataClass::Project => 2,
        DataClass::Public => 3,
    }
}
