//! Authorized requests for tests that are not testing authorization.
//!
//! A provider adapter can only be called with an [`AuthorizedModelRequest`],
//! and the only production path to one is
//! `services::ReleaseManager::authorize`. A contract suite for a provider is
//! not testing the release boundary, though, and an adapter crate cannot reach
//! the private constructor — so the boundary would be untestable from outside
//! the core without a deliberate opening.
//!
//! This is that opening, and it is deliberately narrow: it mints an
//! authorization for a **local** destination only, where nothing is disclosed
//! and no approval is required. There is no test helper that fabricates a
//! *cloud* authorization, because that is exactly the decision the boundary
//! exists to force.

use crate::contracts::{
    AuthorizedModelRequest, ContextReleasePlan, ExecutionLocation, ModelRequest,
    ReleaseAuthorization, RoutedModel, RoutingMode, SCHEMA_VERSION, Timestamp, release_reason,
};

/// An authorization for a local destination, for tests of something else.
///
/// The plan it carries describes the request truthfully in every field a test
/// is likely to read, except that it was never matched against an assembled
/// context — which is the part `services::release` exists to do.
#[must_use]
pub fn locally_authorized(request: ModelRequest) -> AuthorizedModelRequest {
    let plan = ContextReleasePlan {
        schema_version: SCHEMA_VERSION,
        release_id: format!("release-{}", request.request_id),
        task_id: request.task_id.clone(),
        request_id: request.request_id.clone(),
        decision_id: "decision.test".to_owned(),
        context_manifest_id: request.context_manifest_id.clone(),
        destination: RoutedModel {
            provider_id: "test".to_owned(),
            model_id: "test".to_owned(),
        },
        location: ExecutionLocation::Local {
            target_id: "this-device".to_owned(),
            display_name: "This device".to_owned(),
        },
        requires_disclosure: false,
        reason_codes: Vec::new(),
        message_ids: request
            .messages
            .iter()
            .map(|message| message.message_id.clone())
            .collect(),
        released: Vec::new(),
        data_classes: request.data_classes.clone(),
        total_bytes: 0,
        total_token_estimate: None,
        redactions: Vec::new(),
        exclusions: Vec::new(),
        tools_offered: request.tools.iter().map(|tool| tool.name.clone()).collect(),
        authorization: Some(ReleaseAuthorization {
            reason_code: release_reason::LOCAL_DESTINATION.to_owned(),
            approval_id: None,
            mode: RoutingMode::LocalOnly,
        }),
        created_at: Timestamp::new("2026-01-01T00:00:00.000Z"),
    };
    AuthorizedModelRequest::new(request, plan)
}
