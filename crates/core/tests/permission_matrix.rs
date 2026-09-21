//! The permission security matrix.
//!
//! `TESTING_STRATEGY.md` §5 and `PHASE_1_ACCEPTANCE_CRITERIA.md` §9. These are
//! direct assertions rather than snapshots, because a snapshot of a security
//! rule tells you it changed, not whether the change was safe.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::contracts::{
    ApprovalDecisionKind, Capability, CapabilityScope, GrantKind, InvocationId, ProjectId,
    RiskClass, TaskId, Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionPolicy, PermissionQuery, reason};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

fn now() -> Timestamp {
    Timestamp::new("2026-01-01T00:00:00.000Z")
}

fn task() -> TaskId {
    TaskId::new("task-1")
}

fn invocation() -> InvocationId {
    InvocationId::new("invocation-1")
}

fn scope(roots: &[&str]) -> CapabilityScope {
    CapabilityScope::project(
        ProjectId::new("project-1"),
        roots.iter().map(|r| (*r).to_owned()).collect(),
    )
}

fn read_query() -> PermissionQuery {
    PermissionQuery {
        tool_id: "builtin.fs.read".to_owned(),
        capability: Capability::new("filesystem.read"),
        risk_class: RiskClass::Read,
        scope: scope(&["/work/project"]),
        persistable: true,
        details: Vec::new(),
    }
}

fn manager(approver: ScriptedApprover) -> (Arc<PermissionManager>, Arc<InMemoryApprovalStore>) {
    let store = Arc::new(InMemoryApprovalStore::new());
    let manager = PermissionManager::new(
        Arc::clone(&store) as Arc<dyn ApprovalStore>,
        Arc::new(approver),
        Arc::new(SequentialIdSource::new()),
    );
    (Arc::new(manager), store)
}

// --- The safe default ------------------------------------------------------

#[tokio::test]
async fn a_read_inside_an_opened_project_needs_no_approval() {
    let (manager, _) = manager(ScriptedApprover::unavailable());
    let outcome = manager
        .evaluate(&read_query(), &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(outcome.is_allowed());
    assert_eq!(outcome.reason_code(), reason::SAFE_DEFAULT);
}

#[tokio::test]
async fn a_read_that_leaves_the_device_is_not_a_safe_default() {
    // "A read action that sends content to a cloud endpoint is not treated as
    // a harmless local read." (SECURITY_AND_PERMISSIONS.md §3.1)
    let mut query = read_query();
    query.scope.releases_data = true;

    let (manager, _) = manager(ScriptedApprover::unavailable());
    let outcome = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed(), "a data release must be asked about");
    assert_eq!(outcome.reason_code(), reason::NO_APPROVER);
}

// --- Fail closed -----------------------------------------------------------

#[tokio::test]
async fn a_modifying_tool_with_nobody_to_ask_is_denied() {
    let query = PermissionQuery {
        risk_class: RiskClass::Modify,
        capability: Capability::new("filesystem.modify"),
        tool_id: "builtin.fs.write".to_owned(),
        ..read_query()
    };
    let (manager, _) = manager(ScriptedApprover::unavailable());
    let outcome = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed());
    assert_eq!(outcome.reason_code(), reason::NO_APPROVER);
}

#[tokio::test]
async fn unreadable_policy_denies_rather_than_permits() {
    // "Missing, malformed, or stale authorization is denial/approval-required,
    // never implicit allow." (§4)
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    store.break_storage();

    let outcome = manager
        .evaluate(&read_query(), &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed(), "unknown policy must not permit");
    assert_eq!(outcome.reason_code(), reason::POLICY_UNAVAILABLE);
}

#[tokio::test]
async fn a_projectless_task_gets_no_filesystem_scope() {
    // "Projectless tasks have no implicit filesystem or terminal access."
    let query = PermissionQuery {
        scope: CapabilityScope {
            project_id: None,
            roots: vec!["/work/project".to_owned()],
            releases_data: false,
        },
        ..read_query()
    };
    let (manager, _) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));
    let outcome = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed());
    assert_eq!(outcome.reason_code(), reason::NO_PROJECT);
}

#[tokio::test]
async fn a_denial_is_a_denial() {
    let query = PermissionQuery {
        risk_class: RiskClass::Dangerous,
        ..read_query()
    };
    let (manager, _) = manager(ScriptedApprover::always(ApprovalDecisionKind::Deny));
    let outcome = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed());
    assert_eq!(outcome.reason_code(), reason::USER_DENIED);
}

// --- Grants ----------------------------------------------------------------

#[tokio::test]
async fn a_project_grant_covers_later_requests_without_asking_again() {
    let query = PermissionQuery {
        risk_class: RiskClass::Modify,
        ..read_query()
    };
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));

    let first = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();
    assert_eq!(first.reason_code(), reason::APPROVED_PROJECT);

    let second = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();
    assert_eq!(second.reason_code(), reason::PROJECT_GRANT);

    // Exactly one question was asked, not two.
    assert_eq!(store.requests().len(), 1);
}

#[tokio::test]
async fn a_read_grant_cannot_authorize_a_write() {
    // The escalation that matters: a grant issued for one capability must not
    // cover a different, more dangerous one on the same tool.
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));

    let read = PermissionQuery {
        risk_class: RiskClass::Modify,
        capability: Capability::new("filesystem.read"),
        ..read_query()
    };
    manager
        .evaluate(&read, &task(), &invocation(), &now())
        .await
        .unwrap();
    assert_eq!(
        store
            .list_grants(&ProjectId::new("project-1"))
            .await
            .unwrap()
            .len(),
        1
    );

    for escalation in [
        Capability::new("filesystem.modify"),
        Capability::new("process.run"),
        Capability::new("network.request"),
    ] {
        let query = PermissionQuery {
            capability: escalation.clone(),
            risk_class: RiskClass::Dangerous,
            ..read_query()
        };
        // Deny at the prompt so the only way through is the existing grant.
        let (denying, _) = manager_with_existing_grant(&store, ApprovalDecisionKind::Deny);
        let outcome = denying
            .evaluate(&query, &task(), &invocation(), &now())
            .await
            .unwrap();
        assert!(
            !outcome.is_allowed(),
            "a filesystem.read grant must not cover {escalation}"
        );
    }
}

#[tokio::test]
async fn a_grant_never_covers_a_higher_risk_than_it_was_issued_for() {
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));

    let modest = PermissionQuery {
        risk_class: RiskClass::Modify,
        ..read_query()
    };
    manager
        .evaluate(&modest, &task(), &invocation(), &now())
        .await
        .unwrap();

    // Same tool, same capability, but now a dangerous use.
    let dangerous = PermissionQuery {
        risk_class: RiskClass::Dangerous,
        ..read_query()
    };
    let (denying, _) = manager_with_existing_grant(&store, ApprovalDecisionKind::Deny);
    let outcome = denying
        .evaluate(&dangerous, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(
        !outcome.is_allowed(),
        "a grant must not escalate its risk ceiling"
    );
}

#[tokio::test]
async fn a_grant_does_not_reach_a_root_it_was_not_given() {
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));
    let granted = PermissionQuery {
        risk_class: RiskClass::Modify,
        scope: scope(&["/work/project"]),
        ..read_query()
    };
    manager
        .evaluate(&granted, &task(), &invocation(), &now())
        .await
        .unwrap();

    let elsewhere = PermissionQuery {
        risk_class: RiskClass::Modify,
        scope: scope(&["/work/other"]),
        ..read_query()
    };
    let (denying, _) = manager_with_existing_grant(&store, ApprovalDecisionKind::Deny);
    let outcome = denying
        .evaluate(&elsewhere, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed());
}

#[tokio::test]
async fn revoking_a_grant_blocks_the_next_invocation() {
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));
    let query = PermissionQuery {
        risk_class: RiskClass::Modify,
        ..read_query()
    };
    manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    let grants = store
        .list_grants(&ProjectId::new("project-1"))
        .await
        .unwrap();
    assert!(store.revoke_grant(&grants[0].grant_id).await.unwrap());

    let (denying, _) = manager_with_existing_grant(&store, ApprovalDecisionKind::Deny);
    let outcome = denying
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();
    assert!(
        !outcome.is_allowed(),
        "a revoked grant must stop authorizing"
    );
}

#[tokio::test]
async fn a_non_persistable_operation_is_never_offered_a_project_grant() {
    let query = PermissionQuery {
        risk_class: RiskClass::Dangerous,
        persistable: false,
        ..read_query()
    };
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));
    manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    let offered = &store.requests()[0].eligible_decisions;
    assert!(!offered.contains(&ApprovalDecisionKind::AllowProject));
    assert!(offered.contains(&ApprovalDecisionKind::AllowOnce));
    assert!(offered.contains(&ApprovalDecisionKind::Deny));
}

#[tokio::test]
async fn an_approver_cannot_persist_a_grant_the_policy_forbade() {
    // Defence in depth: the choice was never offered, but an approver that
    // returns it anyway must not cause a grant to be written.
    let query = PermissionQuery {
        risk_class: RiskClass::Dangerous,
        persistable: false,
        ..read_query()
    };
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));
    let outcome = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();

    assert!(!outcome.is_allowed());
    assert!(
        store
            .list_grants(&ProjectId::new("project-1"))
            .await
            .unwrap()
            .is_empty()
    );
}

// --- Allow once ------------------------------------------------------------

#[tokio::test]
async fn allow_once_is_consumed_exactly_once_under_concurrency() {
    // Sixteen callers race for one approval. Exactly one may win, and it must
    // hold whether the store is checked once or a thousand times.
    let store = Arc::new(InMemoryApprovalStore::new());
    let approval = tramontana_core::contracts::ApprovalId::new("approval-1");

    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = Arc::clone(&store);
        let approval = approval.clone();
        handles.push(tokio::spawn(
            async move { store.consume_once(&approval).await },
        ));
    }

    let mut winners = 0;
    for handle in handles {
        if handle.await.unwrap().unwrap() {
            winners += 1;
        }
    }
    assert_eq!(
        winners, 1,
        "an allow-once approval may be spent exactly once"
    );
}

#[tokio::test]
async fn allow_once_does_not_survive_into_a_second_request() {
    let query = PermissionQuery {
        risk_class: RiskClass::Modify,
        ..read_query()
    };
    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce));

    let first = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();
    assert_eq!(first.reason_code(), reason::APPROVED_ONCE);

    let second = manager
        .evaluate(&query, &task(), &invocation(), &now())
        .await
        .unwrap();
    assert_eq!(second.reason_code(), reason::APPROVED_ONCE, "asked again");

    // Two separate questions, two separate approvals — no grant was persisted.
    assert_eq!(store.requests().len(), 2);
    assert!(
        store
            .list_grants(&ProjectId::new("project-1"))
            .await
            .unwrap()
            .is_empty()
    );
}

// --- Authorization proof ---------------------------------------------------

#[tokio::test]
async fn a_denied_request_yields_no_authorization() {
    let query = PermissionQuery {
        risk_class: RiskClass::Dangerous,
        ..read_query()
    };
    let (manager, _) = manager(ScriptedApprover::always(ApprovalDecisionKind::Deny));

    let (outcome, authorized) = manager
        .authorize(
            &query,
            &task(),
            &invocation(),
            serde_json::json!({}),
            &now(),
        )
        .await
        .unwrap();

    assert!(!outcome.is_allowed());
    assert!(
        authorized.is_none(),
        "a refusal must not mint an authorization a tool could accept"
    );
}

#[tokio::test]
async fn an_authorization_carries_the_scope_it_was_granted() {
    let (manager, _) = manager(ScriptedApprover::unavailable());
    let (outcome, authorized) = manager
        .authorize(
            &read_query(),
            &task(),
            &invocation(),
            serde_json::json!({ "path": "src/main.rs" }),
            &now(),
        )
        .await
        .unwrap();

    assert!(outcome.is_allowed());
    let authorized = authorized.unwrap();
    assert_eq!(authorized.grant_kind(), GrantKind::ImplicitSafeDefault);
    assert_eq!(
        authorized.granted_scope().roots,
        vec!["/work/project".to_owned()]
    );
    assert_eq!(authorized.capability().as_str(), "filesystem.read");
    assert_eq!(authorized.tool_id(), "builtin.fs.read");
}

#[tokio::test]
async fn an_approval_prompt_carries_no_argument_values() {
    // The prompt describes *what* is being asked for, never the payload.
    // Arguments can hold file content, paths, or a credential someone pasted
    // into a conversation, and an approval card is rendered to a screen and
    // written to the durable record.
    let mut query = PermissionQuery {
        risk_class: RiskClass::Dangerous,
        ..read_query()
    };
    query.scope.releases_data = true;

    let (manager, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::Deny));
    manager
        .authorize(
            &query,
            &task(),
            &invocation(),
            serde_json::json!({
                "path": "/home/someone/.ssh/id_rsa",
                "token": "sk-live-SHOULD-NEVER-APPEAR",
                "body": "line one\nline two",
            }),
            &now(),
        )
        .await
        .unwrap();

    let rendered = serde_json::to_string(&store.requests()[0]).unwrap();

    // It says what is being asked for...
    assert!(rendered.contains("filesystem.read"));
    assert!(rendered.contains("Leaves this device"));

    // ...and carries none of the argument values.
    for leaked in [
        "sk-live-SHOULD-NEVER-APPEAR",
        "id_rsa",
        "/home/someone",
        "line one",
    ] {
        assert!(
            !rendered.contains(leaked),
            "approval prompt leaked {leaked:?}: {rendered}"
        );
    }
}

/// A second manager over the same store, answering differently.
///
/// Lets a test prove that the *only* way a later request succeeds is the
/// existing grant — because the prompt would refuse it.
fn manager_with_existing_grant(
    store: &Arc<InMemoryApprovalStore>,
    answer: ApprovalDecisionKind,
) -> (Arc<PermissionManager>, Arc<InMemoryApprovalStore>) {
    let manager = PermissionManager::new(
        Arc::clone(store) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(answer)),
        Arc::new(SequentialIdSource::new()),
    );
    (Arc::new(manager), Arc::clone(store))
}

// --- An App's tools (ADR 0035) ---------------------------------------------

/// A read an App says its tool does: the App's claim, not a fact.
fn app_read(tool_id: &str) -> PermissionQuery {
    PermissionQuery {
        tool_id: tool_id.to_owned(),
        capability: Capability::new("mcp.notes"),
        ..read_query()
    }
}

#[tokio::test]
async fn an_app_read_is_asked_about_where_a_built_in_read_is_not() {
    let (manager, _) = manager(ScriptedApprover::unavailable());
    let built_in = manager
        .evaluate(&read_query(), &task(), &invocation(), &now())
        .await
        .unwrap();
    assert_eq!(built_in.reason_code(), reason::SAFE_DEFAULT);

    let app = manager
        .evaluate(
            &app_read("mcp.notes.search"),
            &task(),
            &invocation(),
            &now(),
        )
        .await
        .unwrap();
    assert!(!app.is_allowed(), "an App's read ran unasked");
    assert_eq!(app.reason_code(), reason::NO_APPROVER);
}

#[tokio::test]
async fn a_grant_for_an_app_tool_covers_that_tool_alone_until_it_is_revoked() {
    let (granting, store) = manager(ScriptedApprover::always(ApprovalDecisionKind::AllowProject));
    let first = granting
        .evaluate(
            &app_read("mcp.notes.search"),
            &task(),
            &invocation(),
            &now(),
        )
        .await
        .unwrap();
    assert_eq!(first.reason_code(), reason::APPROVED_PROJECT);

    // From here on every question is refused: only the grant can allow.
    let (later, _) = manager_with_existing_grant(&store, ApprovalDecisionKind::Deny);
    let covered = later
        .evaluate(
            &app_read("mcp.notes.search"),
            &task(),
            &invocation(),
            &now(),
        )
        .await
        .unwrap();
    assert_eq!(covered.reason_code(), reason::PROJECT_GRANT);
    for other in ["mcp.notes.delete_all", "mcp.other.search"] {
        let outcome = later
            .evaluate(&app_read(other), &task(), &invocation(), &now())
            .await
            .unwrap();
        assert!(!outcome.is_allowed(), "the grant reached {other}");
    }

    let grants = store
        .list_grants(&ProjectId::new("project-1"))
        .await
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert!(store.revoke_grant(&grants[0].grant_id).await.unwrap());
    let revoked = later
        .evaluate(
            &app_read("mcp.notes.search"),
            &task(),
            &invocation(),
            &now(),
        )
        .await
        .unwrap();
    assert!(
        !revoked.is_allowed(),
        "a revoked grant still allowed the App's tool"
    );
}

#[tokio::test]
async fn the_question_names_the_app_and_does_not_vouch_for_its_claim() {
    let approver = Arc::new(ScriptedApprover::always(ApprovalDecisionKind::Deny));
    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::clone(&approver) as Arc<dyn tramontana_core::ports::Approver>,
        Arc::new(SequentialIdSource::new()),
    );
    let mut silent = app_read("mcp.notes.stamp");
    silent.risk_class = RiskClass::Dangerous;
    silent.persistable = false;
    for query in [app_read("mcp.notes.search"), silent, read_query_asked()] {
        manager
            .evaluate(&query, &task(), &invocation(), &now())
            .await
            .unwrap();
    }

    let asked = approver.asked();
    let detail = |at: usize, label: &str| {
        asked[at]
            .details
            .iter()
            .find(|detail| detail.label == label)
            .map(|detail| detail.value.clone())
    };
    assert_eq!(detail(0, "App").as_deref(), Some("notes"));
    assert_eq!(
        detail(0, "The App says").as_deref(),
        Some("it only reads (not verified)")
    );
    assert_eq!(
        detail(1, "The App says").as_deref(),
        Some("nothing about what it does")
    );
    // A built-in tool's question says neither.
    assert_eq!(detail(2, "App"), None);
    assert_eq!(detail(2, "The App says"), None);
}

/// A built-in read that is asked about, because it leaves the device.
fn read_query_asked() -> PermissionQuery {
    let mut query = read_query();
    query.scope.releases_data = true;
    query
}
