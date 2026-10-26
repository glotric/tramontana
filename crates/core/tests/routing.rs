//! Deterministic, visible, permission-aware destination choice.
//!
//! `BUILD_ORDER.md` §14 asks for three properties in particular: identical
//! input yields an identical decision and reason order; Local Only is tested
//! with a network sink that fails on any attempted cloud connection; and a
//! fallback never crosses trust location without a new decision and
//! disclosure.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

fn at() -> Timestamp {
    Timestamp::new("2026-01-01T00:00:00.000Z")
}

fn model(provider: &str, id: &str, location: ModelLocation) -> ModelDescriptor {
    ModelDescriptor {
        provider_id: provider.to_owned(),
        model_id: id.to_owned(),
        display_name: id.to_owned(),
        location,
        capabilities: vec![
            ModelCapability::Text,
            ModelCapability::Streaming,
            ModelCapability::Cancellation,
        ],
        context_limit: None,
        health: Health::Available,
        health_reason_code: None,
    }
}

fn snapshot(models: Vec<ModelDescriptor>) -> AvailabilitySnapshot {
    AvailabilitySnapshot {
        snapshot_id: "availability-1".to_owned(),
        models,
        taken_at: at(),
    }
}

fn local_and_cloud() -> Vec<ModelDescriptor> {
    vec![
        model("openai", "gpt-x", ModelLocation::Cloud),
        model("local", "llama", ModelLocation::Local),
    ]
}

fn input(mode: RoutingMode, models: Vec<ModelDescriptor>) -> RoutingInput {
    RoutingInput::new(mode, snapshot(models), "context-1")
}

#[test]
fn identical_input_yields_an_identical_decision_and_reason_order() {
    // The property that makes a decision comparable with an earlier one rather
    // than merely readable. Provider registration order must not leak in.
    let forwards = route(
        "decision-1",
        &input(RoutingMode::Automatic, local_and_cloud()),
    )
    .unwrap();

    let mut reversed = local_and_cloud();
    reversed.reverse();
    let backwards = route("decision-1", &input(RoutingMode::Automatic, reversed)).unwrap();

    assert_eq!(forwards, backwards, "ranking depended on input order");
    assert_eq!(forwards.reason_codes, backwards.reason_codes);
}

#[test]
fn local_only_makes_a_cloud_model_ineligible_rather_than_last() {
    // A hard egress prohibition. A fallback list containing an endpoint the
    // policy forbids is an invitation to reach it.
    let decision = route(
        "decision-1",
        &input(RoutingMode::LocalOnly, local_and_cloud()),
    )
    .unwrap();

    assert_eq!(decision.model.model_id, "llama");
    assert!(!decision.leaves_the_device());
    assert!(!decision.requires_disclosure);
    assert!(
        decision.fallback_order.is_empty(),
        "a cloud model was offered as a fallback under Local Only: {:?}",
        decision.fallback_order
    );
    assert!(
        decision
            .reason_codes
            .contains(&"localOnlyRequired".to_owned()),
        "the reason work stayed on the device was not recorded"
    );
}

#[test]
fn local_only_with_no_local_model_refuses_rather_than_reaching_out() {
    let refusal = route(
        "decision-1",
        &input(
            RoutingMode::LocalOnly,
            vec![model("openai", "gpt-x", ModelLocation::Cloud)],
        ),
    )
    .unwrap_err();

    assert_eq!(refusal.reason_codes, vec!["localOnlyRequired"]);
}

#[test]
fn automatic_does_not_send_private_data_off_the_device_on_its_own() {
    // Choosing to release a user's project is what Cloud Preferred means.
    let mut request = input(RoutingMode::Automatic, local_and_cloud());
    request.carries_private_data = true;

    let decision = route("decision-1", &request).unwrap();
    assert_eq!(decision.model.model_id, "llama");
    assert!(
        decision.fallback_order.is_empty(),
        "a cloud fallback was offered for private data under Automatic"
    );
}

#[test]
fn cloud_preferred_chooses_cloud_and_says_the_data_leaves() {
    let mut request = input(RoutingMode::CloudPreferred, local_and_cloud());
    request.carries_private_data = true;

    let decision = route("decision-1", &request).unwrap();
    assert_eq!(decision.model.model_id, "gpt-x");
    assert!(decision.leaves_the_device());
    assert!(
        decision.requires_disclosure,
        "reaching a third party must be disclosed"
    );
    assert!(decision.reason_codes.contains(&"cloudPreferred".to_owned()));
}

#[test]
fn cloud_preferred_landing_on_a_local_model_says_why() {
    // A different fact from "cloud was preferred and chosen", and the record
    // should not read as though the preference was honored.
    let decision = route(
        "decision-1",
        &input(
            RoutingMode::CloudPreferred,
            vec![model("local", "llama", ModelLocation::Local)],
        ),
    )
    .unwrap();

    assert_eq!(decision.model.model_id, "llama");
    assert!(
        decision
            .reason_codes
            .contains(&"providerUnavailable".to_owned()),
        "got {:?}",
        decision.reason_codes
    );
    assert!(!decision.requires_disclosure);
}

#[test]
fn an_explicit_choice_wins_when_it_is_eligible_and_is_ignored_when_it_is_not() {
    let mut request = input(RoutingMode::Automatic, local_and_cloud());
    request.preferred = Some(RoutedModel {
        provider_id: "openai".into(),
        model_id: "gpt-x".into(),
    });
    let decision = route("decision-1", &request).unwrap();
    assert_eq!(decision.model.model_id, "gpt-x");
    assert!(decision.reason_codes.contains(&"userSelected".to_owned()));

    // The same choice under Local Only is not a preference the policy can
    // honor, and the policy wins.
    request.mode = RoutingMode::LocalOnly;
    let decision = route("decision-1", &request).unwrap();
    assert_eq!(decision.model.model_id, "llama");
    assert!(!decision.reason_codes.contains(&"userSelected".to_owned()));
}

#[test]
fn a_model_that_cannot_do_the_work_is_not_a_fallback() {
    let mut request = input(
        RoutingMode::Automatic,
        vec![model("local", "llama", ModelLocation::Local)],
    );
    request.required_capabilities = vec![ModelCapability::ToolCalls];

    let refusal = route("decision-1", &request).unwrap_err();
    assert_eq!(refusal.reason_codes, vec!["capabilityRequired"]);
}

#[test]
fn a_context_that_does_not_fit_rules_a_model_out() {
    let mut small = model("local", "llama", ModelLocation::Local);
    small.context_limit = Some(1_000);
    let mut request = input(RoutingMode::Automatic, vec![small]);
    request.context_tokens = Some(4_000);

    assert_eq!(
        route("decision-1", &request).unwrap_err().reason_codes,
        vec!["contextTooLarge"]
    );
}

#[test]
fn an_unhealthy_or_unchecked_model_is_not_chosen() {
    // Degraded is impaired but usable. Unknown has not been checked, and an
    // unchecked endpoint is not somewhere to send a user's work.
    for (health, eligible) in [
        (Health::Available, true),
        (Health::Degraded, true),
        (Health::Unavailable, false),
        (Health::Unknown, false),
    ] {
        let mut candidate = model("local", "llama", ModelLocation::Local);
        candidate.health = health;
        let outcome = route(
            "decision-1",
            &input(RoutingMode::Automatic, vec![candidate]),
        );
        assert_eq!(outcome.is_ok(), eligible, "{health:?}");
    }
}

#[test]
fn a_fallback_is_a_new_decision_with_its_own_disclosure() {
    // "A fallback is a new routing decision. Crossing from local to cloud
    // always creates a new release manifest/disclosure decision."
    let request = input(RoutingMode::CloudPreferred, local_and_cloud());
    let first = route("decision-1", &request).unwrap();
    assert_eq!(first.model.model_id, "gpt-x");

    let second = route_after_failure("decision-2", &request, &first.model).unwrap();

    assert_ne!(
        second.decision_id, first.decision_id,
        "a fallback amended the earlier decision instead of making a new one"
    );
    assert_eq!(second.model.model_id, "llama");
    assert!(
        second
            .reason_codes
            .contains(&"fallbackAfterFailure".to_owned())
    );
    // Disclosure is recomputed for where it actually went, not inherited.
    assert!(!second.requires_disclosure);
    assert!(first.requires_disclosure);
}

#[test]
fn a_fallback_into_the_cloud_carries_its_own_disclosure() {
    let request = input(RoutingMode::CloudPreferred, local_and_cloud());
    let local_first = RoutedModel {
        provider_id: "local".into(),
        model_id: "llama".into(),
    };
    let after = route_after_failure("decision-2", &request, &local_first).unwrap();

    assert_eq!(after.model.model_id, "gpt-x");
    assert!(
        after.requires_disclosure,
        "a fallback that crossed to a third party did not disclose it"
    );
}

#[test]
fn a_fallback_never_crosses_a_boundary_the_mode_forbids() {
    let request = input(RoutingMode::LocalOnly, local_and_cloud());
    let local = RoutedModel {
        provider_id: "local".into(),
        model_id: "llama".into(),
    };
    let refusal = route_after_failure("decision-2", &request, &local).unwrap_err();

    assert!(
        refusal
            .reason_codes
            .contains(&"localOnlyRequired".to_owned()),
        "Local Only fell back into the cloud: {refusal:?}"
    );
}

/// A provider that counts every time it is asked to run something.
///
/// Two of these stand in for a configured cloud endpoint and a configured
/// local one. The cloud instance is the network sink `BUILD_ORDER.md` §14
/// asks for: if Local Only is a preference rather than a prohibition, it
/// records a call and the test says so.
///
/// One provider offering models under two different provider ids — which an
/// earlier version of this fixture did — is exactly what the shared contract
/// suite forbids ("a model claims a provider other than the one offering
/// it"), and the driver now refuses to dispatch on it.
#[derive(Debug)]
struct CountingProvider {
    provider_id: &'static str,
    model_id: &'static str,
    location: ModelLocation,
    reached: AtomicUsize,
}

impl CountingProvider {
    fn new(provider_id: &'static str, model_id: &'static str, location: ModelLocation) -> Self {
        Self {
            provider_id,
            model_id,
            location,
            reached: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelProvider for CountingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.provider_id)
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        vec![model(self.provider_id, self.model_id, self.location)]
    }

    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        // Recorded rather than panicking: a panic inside a spawned task would
        // surface as a hung test rather than as this assertion.
        self.reached.fetch_add(1, Ordering::SeqCst);

        FakeModelProvider::streaming(["answer"])
            .stream(request, cancel)
            .await
    }
}

#[tokio::test]
async fn under_local_only_a_task_never_reaches_a_cloud_endpoint() {
    let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    // Two configured endpoints, as a profile with both would have.
    let sink = Arc::new(CountingProvider::new(
        "cloud",
        "remote",
        ModelLocation::Cloud,
    ));
    let on_device = Arc::new(CountingProvider::new(
        "local",
        "on-device",
        ModelLocation::Local,
    ));
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        ids.clone(),
    )
    .unwrap();

    let service = TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: vec![sink.clone(), on_device.clone()],
            tools,
            permissions,
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(RoutingMode::LocalOnly);

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "Say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service
                .get_task(&task_id)
                .await
                .unwrap()
                .state
                .is_terminal()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task never settled");

    // A model ran — this is not a test that nothing happened — and it was
    // the one on this device.
    assert_eq!(
        on_device.reached.load(Ordering::SeqCst),
        1,
        "the task did not run a model at all, so this proves nothing"
    );
    assert_eq!(
        sink.reached.load(Ordering::SeqCst),
        0,
        "Local Only reached a cloud endpoint"
    );

    // And the recorded decision kept it on the device, with no cloud fallback
    // offered anywhere in the stream.
    let decisions: Vec<RoutingDecision> = service
        .replay_task_events(&task_id, 0)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.payload {
            EventPayload::RoutingDecided { decision } => Some(decision),
            _ => None,
        })
        .collect();

    assert_eq!(decisions.len(), 2); // Task plan plus actual request.
    for decision in decisions {
        assert_eq!(decision.model.model_id, "on-device");
        assert!(!decision.leaves_the_device());
        assert!(!decision.requires_disclosure);
        assert!(
            decision.fallback_order.is_empty(),
            "a cloud endpoint was listed as reachable under Local Only"
        );
    }
}

#[tokio::test]
async fn a_task_with_no_eligible_model_fails_with_the_reason() {
    // A refusal is as much a decision as a choice, and the user has to be told.
    let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        ids.clone(),
    )
    .unwrap();

    /// A provider offering nothing at all.
    #[derive(Debug)]
    struct NoModels;

    #[async_trait]
    impl ModelProvider for NoModels {
        fn id(&self) -> ProviderId {
            ProviderId::new("empty")
        }
        async fn list_models(&self) -> Vec<ModelDescriptor> {
            Vec::new()
        }
        async fn stream(
            &self,
            _request: AuthorizedModelRequest,
            _cancel: CancellationToken,
        ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
            panic!("a task with no eligible model must never reach a provider");
        }
    }

    let service = TaskService::new(
        in_memory_repositories(events),
        Providers {
            models: vec![Arc::new(NoModels)],
            tools,
            permissions,
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        ids,
    );

    let task_id = service
        .start_task(StartTaskRequest {
            conversation_id: None,
            goal: "Say hello".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = service.get_task(&task_id).await.unwrap().state;
            if state.is_terminal() {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the task never settled");

    let failure = service
        .replay_task_events(&task_id, 0)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.payload {
            EventPayload::TaskFailed { error } => Some(error),
            _ => None,
        })
        .expect("the task did not record why it failed");

    assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
    assert!(
        failure.message.contains("modelUnavailable"),
        "the refusal did not carry a reason a person can act on: {}",
        failure.message
    );
}

// --- A worker pinned to one model (ADR 0048) --------------------------------

#[test]
fn a_required_model_is_the_only_one_chosen_and_nothing_else_stands_in_for_it() {
    let mut pinned = input(RoutingMode::Automatic, local_and_cloud());
    pinned.required = Some(RoutedModel {
        provider_id: "local".into(),
        model_id: "llama".into(),
    });
    let decision = route("decision-1", &pinned).unwrap();
    assert_eq!(decision.model.model_id, "llama");
    assert!(
        decision
            .reason_codes
            .contains(&routing_reason::USER_SELECTED.to_owned())
    );
    assert!(
        decision.fallback_order.is_empty(),
        "{:?}",
        decision.fallback_order
    );

    // Unavailable, it is not replaced by another.
    pinned.required = Some(RoutedModel {
        provider_id: "local".into(),
        model_id: "missing".into(),
    });
    assert!(route("decision-2", &pinned).is_err());
}

#[test]
fn a_person_may_pin_a_cloud_model_under_automatic_but_never_under_local_only() {
    let cloud = RoutedModel {
        provider_id: "openai".into(),
        model_id: "gpt-x".into(),
    };
    let mut automatic = input(RoutingMode::Automatic, local_and_cloud());
    automatic.carries_private_data = true;
    automatic.required = Some(cloud.clone());
    let decision = route("decision-1", &automatic).unwrap();
    assert_eq!(decision.model, cloud);

    // Without the pin, Automatic keeps private data on the device.
    automatic.required = None;
    assert_ne!(route("decision-2", &automatic).unwrap().model, cloud);

    let mut local_only = input(RoutingMode::LocalOnly, local_and_cloud());
    local_only.required = Some(cloud);
    let refusal = route("decision-3", &local_only).unwrap_err();
    assert!(
        refusal
            .reason_codes
            .contains(&routing_reason::LOCAL_ONLY_REQUIRED.to_owned())
    );
}
