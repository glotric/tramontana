//! A person's own reads of a project, outside any task (ADR 0032).
#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::*;
use tramontana_core::ports::*;
use tramontana_core::services::*;
use tramontana_core::testing::*;

fn tool(
    tool_id: &str,
    risk_class: RiskClass,
    side_effect: SideEffect,
    schema: serde_json::Value,
) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_owned(),
        provider_id: "fake".to_owned(),
        display_name: tool_id.to_owned(),
        description: "fixture".to_owned(),
        input_schema: schema,
        risk_class,
        side_effect,
        capability: "fake.capability".to_owned(),
        supports_cancellation: true,
        replay_safety: ReplaySafety::Safe,
    }
}

fn no_arguments() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
}

/// Tools that count their runs and keep the last authorization they were given.
#[derive(Debug, Default)]
struct Reads {
    runs: AtomicUsize,
    last: Mutex<Option<AuthorizedToolRequest>>,
}

#[async_trait]
impl ToolProvider for Reads {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![
            tool(
                "fake.git.diff",
                RiskClass::Read,
                SideEffect::None,
                no_arguments(),
            ),
            tool(
                "fake.fs.read",
                RiskClass::Read,
                SideEffect::None,
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                }),
            ),
            tool(
                "fake.fs.patch",
                RiskClass::Modify,
                SideEffect::LocalReversible,
                no_arguments(),
            ),
        ]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = Some(request);
        Ok(ToolOutcome {
            content: serde_json::json!({"text": "diff --git a/greeting.txt b/greeting.txt"}),
            display_summary: "fake.git.diff returned 1 line".to_owned(),
            side_effect: SideEffect::None,
            truncated: false,
            changes: Vec::new(),
        })
    }
}

/// Counts every question it is asked, and answers none.
#[derive(Debug, Default)]
struct Asked(AtomicUsize);

#[async_trait]
impl Approver for Asked {
    async fn ask(
        &self,
        _request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(PermissionError::NoApprover)
    }
}

struct Fixture {
    service: TaskService,
    permissions: Arc<PermissionManager>,
    reads: Arc<Reads>,
    asked: Arc<Asked>,
}

fn fixture() -> Fixture {
    let events: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let reads = Arc::new(Reads::default());
    let asked = Arc::new(Asked::default());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![reads.clone()],
        Arc::new(InMemoryApprovalStore::new()),
        asked.clone(),
        ids.clone(),
    )
    .unwrap();
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
        },
        Providers {
            models: vec![Arc::new(FakeModelProvider::streaming(["answer"]))],
            tools,
            permissions: permissions.clone(),
            releases,
            worker: Arc::new(FakeWorker::new()),
        },
        Arc::new(FakeClock::new()),
        ids,
    );
    Fixture {
        service,
        permissions,
        reads,
        asked,
    }
}

#[tokio::test]
async fn a_person_reads_a_projects_diff_with_no_task_and_no_question() {
    let fixture = fixture();
    let project = fixture
        .service
        .create_project("Synthetic project", vec!["/synthetic".into()])
        .await
        .unwrap();

    let outcome = fixture
        .service
        .read_project(&project.project_id, "fake.git.diff")
        .await
        .unwrap();

    assert_eq!(
        outcome.content["text"],
        "diff --git a/greeting.txt b/greeting.txt"
    );
    assert_eq!(fixture.reads.runs.load(Ordering::SeqCst), 1);
    let given = fixture.reads.last.lock().unwrap().clone().unwrap();
    assert_eq!(given.task_id(), None, "a person's read belongs to no task");
    assert_eq!(given.granted_scope().roots, ["/synthetic"]);
    assert_eq!(given.grant_kind(), GrantKind::ImplicitSafeDefault);
    assert_eq!(
        fixture.asked.0.load(Ordering::SeqCst),
        0,
        "a person's read asked a question"
    );
    assert!(
        fixture.service.list_tasks(None).await.unwrap().is_empty(),
        "a person's read was recorded as a task"
    );
}

#[tokio::test]
async fn anything_more_than_a_no_argument_read_is_refused_before_it_runs() {
    let fixture = fixture();
    let project = fixture
        .service
        .create_project("Synthetic project", vec!["/synthetic".into()])
        .await
        .unwrap();

    for (tool_id, code) in [
        ("fake.fs.patch", ErrorCode::InvalidRequest),
        ("fake.fs.read", ErrorCode::InvalidRequest),
        ("fake.nothing.here", ErrorCode::NotFound),
    ] {
        let refused = fixture
            .service
            .read_project(&project.project_id, tool_id)
            .await
            .unwrap_err();
        assert_eq!(refused.code, code, "{tool_id}");
    }
    assert!(
        fixture
            .service
            .read_project(&ProjectId::new("no-such-project"), "fake.git.diff")
            .await
            .is_err()
    );
    assert_eq!(fixture.reads.runs.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.asked.0.load(Ordering::SeqCst), 0);
}

#[test]
fn the_permission_manager_gives_a_person_only_a_quiet_read() {
    let fixture = fixture();
    let invocation = InvocationId::new("invocation-1");
    let project = CapabilityScope::project(ProjectId::new("project-1"), vec!["/synthetic".into()]);
    let query = |risk_class, scope: CapabilityScope| PermissionQuery {
        tool_id: "fake.git.diff".to_owned(),
        capability: Capability::new("fake.capability"),
        risk_class,
        scope,
        persistable: false,
        details: Vec::new(),
    };

    let (allowed, authorized) = fixture.permissions.authorize_person_read(
        &query(RiskClass::Read, project.clone()),
        &invocation,
        serde_json::json!({}),
    );
    assert!(allowed.is_allowed());
    assert!(authorized.is_some());

    let releasing = CapabilityScope {
        releases_data: true,
        ..project.clone()
    };
    let projectless = CapabilityScope {
        project_id: None,
        roots: Vec::new(),
        releases_data: false,
    };
    for (risk_class, scope) in [
        (RiskClass::Modify, project.clone()),
        (RiskClass::Dangerous, project),
        (RiskClass::Read, releasing),
        (RiskClass::Read, projectless),
    ] {
        let (outcome, authorized) = fixture.permissions.authorize_person_read(
            &query(risk_class, scope),
            &invocation,
            serde_json::json!({}),
        );
        assert_eq!(
            outcome,
            PermissionOutcome::Denied {
                reason_code: reason::PERSON_READ_ONLY
            }
        );
        assert!(authorized.is_none());
    }
}
