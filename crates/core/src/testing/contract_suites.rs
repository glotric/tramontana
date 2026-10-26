//! Reusable contract suites.
//!
//! `TESTING_STRATEGY.md` §4 requires that every implementation of a stable port
//! obey one shared semantic suite. These are written against the trait, never
//! against a particular implementation, so an in-memory fake and a real
//! database run identical cases and any divergence is a defect in one of them
//! rather than a difference in what was tested.
//!
//! They live in the library rather than in a test file because the
//! implementations they verify live in other crates.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::contracts::{
    CheckpointId, CorrelationId, EventDraft, EventEnvelope, EventId, EventPayload, PendingAction,
    PendingActionKind, ReplaySafety, ResumeEligibility, TaskCheckpoint, TaskEventKind, TaskId,
    TaskState, Timestamp,
};
use crate::contracts::{ModelEvent, ModelRequest, SecretRef, SecretStore, StopReason};
use crate::ports::{
    CheckpointRepository, EventStore, EventStoreError, ModelProvider, RepositoryError, SecretVault,
};

/// Build an event for `task_id` at `sequence`.
#[must_use]
pub fn event_at(task_id: &TaskId, sequence: u64) -> EventEnvelope {
    EventDraft::user(
        TaskEventKind::TaskCompleted,
        EventPayload::TaskCompleted {
            summary: format!("event {sequence}"),
            artifacts: Vec::new(),
        },
    )
    .into_envelope(
        EventId::new(format!("event-{task_id}-{sequence}")),
        task_id.clone(),
        None,
        sequence,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
        CorrelationId::new("correlation-1"),
    )
}

/// Assert that `build` produces a store obeying the full `EventStore` contract.
///
/// `build` must return a *fresh, empty* store each time; several cases depend
/// on starting from nothing.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn event_store_contract<F, Fut>(build: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn EventStore>>,
{
    let task = TaskId::new("task-1");
    let other = TaskId::new("task-2");

    // An empty stream is at sequence 0, not an error.
    let store = build().await;
    assert_eq!(store.last_sequence(&task).await.unwrap(), 0);
    assert!(store.replay(&task, 0).await.unwrap().is_empty());

    // A first append expects 0 and yields sequence 1.
    let store = build().await;
    assert_eq!(append(&*store, &task, 0, &[1]).await.unwrap(), 1);
    assert_eq!(store.last_sequence(&task).await.unwrap(), 1);

    // A batch lands consecutively.
    let store = build().await;
    assert_eq!(append(&*store, &task, 0, &[1, 2]).await.unwrap(), 2);
    assert_eq!(seqs(&*store, &task, 0).await, vec![1, 2]);

    // Round trip: what comes back is what went in.
    let original = event_at(&task, 1);
    let store = build().await;
    store
        .append(&task, 0, vec![original.clone()])
        .await
        .unwrap();
    let round_tripped = store.replay(&task, 0).await.unwrap();
    assert_eq!(round_tripped, vec![original]);

    // A stale expected sequence conflicts rather than interleaving. This is
    // what stops two writers corrupting one task's order.
    let store = build().await;
    append(&*store, &task, 0, &[1]).await.unwrap();
    assert_eq!(
        append(&*store, &task, 0, &[1]).await.unwrap_err(),
        EventStoreError::SequenceConflict {
            task_id: task.clone(),
            expected: 0,
            actual: 1,
        }
    );
    assert_eq!(
        store.last_sequence(&task).await.unwrap(),
        1,
        "a conflict must leave the stream untouched"
    );

    // Misnumbered events are rejected, and nothing partial is written.
    let store = build().await;
    assert_eq!(
        append(&*store, &task, 0, &[1, 3]).await.unwrap_err(),
        EventStoreError::NonConsecutive {
            task_id: task.clone(),
            expected_first: 1,
        }
    );
    assert_eq!(
        store.last_sequence(&task).await.unwrap(),
        0,
        "a rejected batch must not write its valid prefix"
    );

    // An event addressed to another task is rejected.
    let store = build().await;
    let error = store
        .append(&task, 0, vec![event_at(&TaskId::new("other"), 1)])
        .await
        .unwrap_err();
    assert!(matches!(error, EventStoreError::NonConsecutive { .. }));

    // Replay is exclusive of `after_sequence`, so it can be called directly
    // with the last sequence an interface already rendered.
    let store = build().await;
    append(&*store, &task, 0, &[1, 2, 3]).await.unwrap();
    assert_eq!(seqs(&*store, &task, 1).await, vec![2, 3]);
    assert!(store.replay(&task, 3).await.unwrap().is_empty());

    // Streams are isolated per task.
    let store = build().await;
    append(&*store, &task, 0, &[1]).await.unwrap();
    assert_eq!(store.last_sequence(&other).await.unwrap(), 0);
    append(&*store, &other, 0, &[1]).await.unwrap();
    assert_eq!(store.replay(&task, 0).await.unwrap().len(), 1);
    assert_eq!(store.replay(&other, 0).await.unwrap().len(), 1);

    // Appending after a successful append continues the stream.
    let store = build().await;
    append(&*store, &task, 0, &[1]).await.unwrap();
    append(&*store, &task, 1, &[2]).await.unwrap();
    assert_eq!(store.last_sequence(&task).await.unwrap(), 2);

    // Deleting a stream removes that task's events and no other's, and a
    // stream that holds nothing deletes without error (ADR 0059).
    let store = build().await;
    let kept = TaskId::new("task-kept");
    append(&*store, &task, 0, &[1, 2]).await.unwrap();
    append(&*store, &kept, 0, &[1]).await.unwrap();
    store.delete_stream(&task).await.unwrap();
    assert!(store.replay(&task, 0).await.unwrap().is_empty());
    assert_eq!(store.last_sequence(&task).await.unwrap(), 0);
    assert_eq!(store.replay(&kept, 0).await.unwrap().len(), 1);
    store.delete_stream(&task).await.unwrap();
}

/// Append `task`'s events numbered `sequences`, expecting `expected`.
async fn append(
    store: &dyn EventStore,
    task: &TaskId,
    expected: u64,
    sequences: &[u64],
) -> Result<u64, EventStoreError> {
    let events = sequences.iter().map(|&at| event_at(task, at)).collect();
    store.append(task, expected, events).await
}

/// The sequences `task`'s replay after `after` returns.
#[allow(
    clippy::unwrap_used,
    reason = "this is a test suite; a panic is the failure report"
)]
async fn seqs(store: &dyn EventStore, task: &TaskId, after: u64) -> Vec<u64> {
    let events = store.replay(task, after).await.unwrap();
    events.iter().map(|event| event.sequence).collect()
}

/// The tasks [`checkpoint_repository_contract`] writes checkpoints for.
///
/// A durable store may require a checkpoint to belong to a real task, so a
/// harness for one needs to know which task rows to create first. Exported so
/// that requirement does not become a duplicated literal in every harness.
pub const CHECKPOINT_CONTRACT_TASKS: [&str; 2] = ["task-1", "task-2"];

/// Build a checkpoint for `task_id` at `sequence`.
#[must_use]
pub fn checkpoint_at(task_id: &TaskId, sequence: u64) -> TaskCheckpoint {
    let mut checkpoint = TaskCheckpoint::new(
        CheckpointId::new(format!("checkpoint-{task_id}-{sequence}")),
        task_id.clone(),
        sequence,
        Timestamp::new("2026-01-01T00:00:00.000Z"),
        TaskState::Interrupted,
        ResumeEligibility::same_worker("checkpoint.derived"),
    );
    checkpoint.completed_step_ids = vec![format!("invocation-{sequence}")];
    checkpoint.pending_action = Some(PendingAction {
        kind: PendingActionKind::Tool,
        reference_id: format!("invocation-{}", sequence + 1),
        replay_safety: ReplaySafety::Unknown,
        display_summary: "a tool was in flight".to_owned(),
    });
    checkpoint
}

/// Assert that `build` produces a repository obeying the checkpoint contract.
///
/// `build` must return a *fresh, empty* repository each time.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn checkpoint_repository_contract<F, Fut>(build: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn CheckpointRepository>>,
{
    let [task, other] = CHECKPOINT_CONTRACT_TASKS.map(TaskId::new);

    // A task with no checkpoints has no latest one, which is not an error.
    let repository = build().await;
    assert_eq!(repository.latest(&task).await.unwrap(), None);
    assert!(repository.list(&task).await.unwrap().is_empty());

    // A saved checkpoint round-trips whole, pending action included.
    let repository = build().await;
    let saved = checkpoint_at(&task, 4);
    repository.save(&saved).await.unwrap();
    let read = repository.get(&saved.checkpoint_id).await.unwrap();
    assert_eq!(read, saved, "a checkpoint must survive storage unchanged");

    // The latest is the highest sequence, not the most recently written.
    let repository = build().await;
    for sequence in [9, 2, 6] {
        repository
            .save(&checkpoint_at(&task, sequence))
            .await
            .unwrap();
    }
    assert_eq!(
        repository
            .latest(&task)
            .await
            .unwrap()
            .unwrap()
            .event_sequence,
        9
    );
    let sequences: Vec<u64> = repository
        .list(&task)
        .await
        .unwrap()
        .iter()
        .map(|checkpoint| checkpoint.event_sequence)
        .collect();
    assert_eq!(sequences, vec![2, 6, 9], "list is oldest first");

    // Checkpoints do not leak between tasks.
    let repository = build().await;
    repository.save(&checkpoint_at(&task, 1)).await.unwrap();
    repository.save(&checkpoint_at(&other, 5)).await.unwrap();
    assert_eq!(repository.list(&task).await.unwrap().len(), 1);
    assert_eq!(
        repository
            .latest(&other)
            .await
            .unwrap()
            .unwrap()
            .event_sequence,
        5
    );

    // One point in a stream has one checkpoint.
    let repository = build().await;
    repository.save(&checkpoint_at(&task, 3)).await.unwrap();
    let mut duplicate = checkpoint_at(&task, 3);
    duplicate.checkpoint_id = CheckpointId::new("checkpoint-duplicate");
    match repository.save(&duplicate).await {
        Err(RepositoryError::Conflict { kind, .. }) => assert_eq!(kind, "checkpoint"),
        other => panic!("a second checkpoint at one sequence must conflict, got {other:?}"),
    }

    // An unknown identifier is not found rather than empty.
    let repository = build().await;
    match repository.get(&CheckpointId::new("absent")).await {
        Err(RepositoryError::NotFound { kind, .. }) => assert_eq!(kind, "checkpoint"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// What a provider should be configured to do for one contract case.
///
/// The suite has to drive an adapter that talks to a real endpoint as well as
/// an in-process fake, so it asks for a *behavior* rather than constructing a
/// provider itself. A harness translates each scenario into whatever its
/// implementation needs — a script for a fake, a stubbed response for an HTTP
/// adapter.
///
/// Deliberately *not* `#[non_exhaustive]`: adding a case should break every
/// harness until it says what its implementation does, because a scenario
/// silently unhandled is a provider silently untested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderScenario {
    /// Stream these chunks in order, then finish normally.
    Streams(Vec<String>),
    /// Fail without producing any content.
    Fails,
    /// Produce content slowly enough that a cancellation lands mid-stream.
    Slow,
}

/// The request the suite sends. Providers must not depend on its content.
#[must_use]
pub fn model_request(request_id: &str) -> ModelRequest {
    use crate::contracts::{MessageId, MessageRole, NormalizedMessage, TaskId, Timestamp};

    ModelRequest::new(
        request_id,
        TaskId::new("task-1"),
        vec![NormalizedMessage::text(
            MessageId::new("message-1"),
            None,
            MessageRole::User,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
            "Say hello",
        )],
        "context-1",
    )
}

/// Drain a stream, returning everything it produced.
async fn drain(receiver: &mut tokio::sync::mpsc::Receiver<ModelEvent>) -> Vec<ModelEvent> {
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }
    events
}

/// Assert that `build` produces a provider obeying the `ModelProvider` contract.
///
/// `CORE_CONTRACTS.md` §8. The property that matters most is the last one:
/// **exactly one terminal event ends a stream**, including when cancellation
/// arrives while the underlying request is still in flight. A provider that
/// drops its stream on cancel, or emits both a `completed` and a `failed`,
/// leaves the core unable to say what happened — and the core commits the
/// coalesced answer on that signal.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn model_provider_contract<F, Fut>(build: F)
where
    F: Fn(ProviderScenario) -> Fut,
    Fut: Future<Output = Arc<dyn ModelProvider>>,
{
    // A provider reports models under its own identity, so a routing decision
    // naming a provider can be resolved back to one.
    let provider = build(ProviderScenario::Streams(vec!["hi".into()])).await;
    let models = provider.list_models().await;
    assert!(
        !models.is_empty(),
        "a provider must offer at least one model"
    );
    for model in &models {
        assert_eq!(
            model.provider_id,
            provider.id().to_string(),
            "a model claims a provider other than the one offering it"
        );
        assert!(!model.model_id.is_empty());
    }

    // A successful stream: content, then exactly one terminal event, then the
    // receiver closes.
    let provider = build(ProviderScenario::Streams(vec![
        "Hello".into(),
        ", ".into(),
        "world".into(),
    ]))
    .await;
    let mut stream = provider
        .stream(
            crate::testing::release::locally_authorized(model_request("request-1")),
            CancellationToken::new(),
        )
        .await
        .expect("a scripted success must start");
    let events = drain(&mut stream).await;

    let terminal: Vec<&ModelEvent> = events.iter().filter(|event| event.is_terminal()).collect();
    assert_eq!(
        terminal.len(),
        1,
        "a stream must end with exactly one terminal event, got {events:?}"
    );
    assert!(
        events.last().is_some_and(ModelEvent::is_terminal),
        "the terminal event must be last"
    );
    assert!(
        matches!(
            terminal[0],
            ModelEvent::Completed {
                stop_reason: StopReason::Stop
            }
        ),
        "a scripted success must complete with `stop`, got {:?}",
        terminal[0]
    );

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "Hello, world",
        "the content did not survive streaming"
    );

    // Every delta names the stream it belongs to, and offsets never go
    // backwards — a subscriber orders on them.
    let mut previous: Option<u64> = None;
    for event in &events {
        if let ModelEvent::TextDelta {
            stream_id, offset, ..
        } = event
        {
            assert_eq!(stream_id, "request-1", "a delta named a different stream");
            if let Some(previous) = previous {
                assert!(*offset > previous, "offsets went backwards: {events:?}");
            }
            previous = Some(*offset);
        }
    }

    // A failure is a normalized envelope, either as a terminal event or as a
    // refusal to start. Both are legitimate; producing neither is not.
    let provider = build(ProviderScenario::Fails).await;
    match provider
        .stream(
            crate::testing::release::locally_authorized(model_request("request-2")),
            CancellationToken::new(),
        )
        .await
    {
        Ok(mut stream) => {
            let events = drain(&mut stream).await;
            let terminal: Vec<&ModelEvent> =
                events.iter().filter(|event| event.is_terminal()).collect();
            assert_eq!(terminal.len(), 1, "a failing stream must still end once");
            assert!(
                matches!(terminal[0], ModelEvent::Failed { .. }),
                "a scripted failure ended as {:?}",
                terminal[0]
            );
        }
        Err(error) => assert!(
            !error.message.is_empty(),
            "a refusal must say something a person can act on"
        ),
    }

    // Cancellation mid-stream converges to exactly one terminal event rather
    // than the stream simply stopping.
    let provider = build(ProviderScenario::Slow).await;
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(
            crate::testing::release::locally_authorized(model_request("request-3")),
            cancel.clone(),
        )
        .await
        .expect("a slow stream must start");
    cancel.cancel();

    let events = tokio::time::timeout(std::time::Duration::from_secs(10), drain(&mut stream))
        .await
        .expect("a cancelled stream must converge, not hang");
    let terminal: Vec<&ModelEvent> = events.iter().filter(|event| event.is_terminal()).collect();
    assert_eq!(
        terminal.len(),
        1,
        "a cancelled stream must end with exactly one terminal event, got {events:?}"
    );

    // Cancelled before anything was requested: still one terminal event.
    let provider = build(ProviderScenario::Slow).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    if let Ok(mut stream) = provider
        .stream(
            crate::testing::release::locally_authorized(model_request("request-4")),
            cancel,
        )
        .await
    {
        let events = tokio::time::timeout(std::time::Duration::from_secs(10), drain(&mut stream))
            .await
            .expect("a pre-cancelled stream must converge, not hang");
        assert_eq!(
            events.iter().filter(|event| event.is_terminal()).count(),
            1,
            "a pre-cancelled stream must still end exactly once, got {events:?}"
        );
    }
}

/// A reference a contract run uses, namespaced so a real store can be swept.
///
/// Every key a test writes begins with the same prefix, so a run against a
/// developer's actual Keychain leaves nothing that cannot be identified and
/// removed — and cannot collide with a credential the user put there.
#[must_use]
pub fn test_secret_ref(name: &str) -> SecretRef {
    SecretRef {
        key: format!("tramontana.test.{name}"),
        store: SecretStore::Platform,
    }
}

/// Assert that `vault` obeys the full [`SecretVault`] contract.
///
/// Run against the in-memory fake and against the real platform store, so a
/// divergence is a defect in one of them rather than a difference in what was
/// tested (`TESTING_STRATEGY.md` §4).
///
/// The vault must start with none of the contract's keys present. Every key it
/// writes is removed before returning, including on the paths that assert
/// failure, so a run against a real credential store leaves the machine as it
/// found it.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn secret_vault_contract(vault: &dyn SecretVault) {
    let reference = test_secret_ref("contract");
    let other = test_secret_ref("contract.other");

    // Start clean, whatever an earlier run left behind.
    vault.delete(&reference).await.unwrap();
    vault.delete(&other).await.unwrap();

    // An absent credential is missing, not empty. A store that answered with
    // an empty string would be indistinguishable from one holding "".
    assert!(!vault.contains(&reference).await.unwrap());
    let missing = vault.resolve(&reference).await.unwrap_err();
    assert_eq!(
        missing.code,
        crate::contracts::ErrorCode::CredentialMissing,
        "an absent credential reported {:?}",
        missing.code
    );

    // Stored, then resolved as itself.
    vault.store(&reference, "value-one").await.unwrap();
    assert!(vault.contains(&reference).await.unwrap());
    assert_eq!(vault.resolve(&reference).await.unwrap(), "value-one");

    // Replacement, which is what a rotated key is. No delete-then-store
    // window where the credential is absent.
    vault.store(&reference, "value-two").await.unwrap();
    assert_eq!(vault.resolve(&reference).await.unwrap(), "value-two");

    // References are independent: one key's value is not another's.
    vault.store(&other, "different").await.unwrap();
    assert_eq!(vault.resolve(&reference).await.unwrap(), "value-two");
    assert_eq!(vault.resolve(&other).await.unwrap(), "different");

    // A value that is not ASCII, and one that is long, both survive intact.
    // A store that silently truncated or re-encoded would hand back a
    // credential that fails to authenticate for no visible reason.
    let awkward = format!("ключ-{}-🔑", "x".repeat(512));
    vault.store(&other, &awkward).await.unwrap();
    assert_eq!(vault.resolve(&other).await.unwrap(), awkward);

    // Deleting reports whether anything was there, and deleting again is not
    // an error: the caller asked for it to be gone, and it is gone.
    assert!(vault.delete(&reference).await.unwrap());
    assert!(!vault.delete(&reference).await.unwrap());
    assert!(!vault.contains(&reference).await.unwrap());

    // The error for a deleted credential is the same as for one never stored.
    let after_delete = vault.resolve(&reference).await.unwrap_err();
    assert_eq!(
        after_delete.code,
        crate::contracts::ErrorCode::CredentialMissing
    );

    // Nothing this suite wrote is left behind.
    vault.delete(&other).await.unwrap();
    assert!(!vault.contains(&other).await.unwrap());
}

/// Assert that an unreachable store is distinguishable from an empty one.
///
/// A store that cannot be reached reporting "no credential" is the failure
/// that makes a user re-enter a key that was already there
/// (`SECURITY_AND_PERMISSIONS.md` §7).
///
/// # Panics
///
/// Panics with the failing expectation.
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn unavailable_store_contract(vault: &dyn SecretVault) {
    let reference = test_secret_ref("unavailable");
    for code in [
        vault.resolve(&reference).await.unwrap_err().code,
        vault.contains(&reference).await.unwrap_err().code,
        vault.store(&reference, "x").await.unwrap_err().code,
        vault.delete(&reference).await.unwrap_err().code,
    ] {
        assert_ne!(
            code,
            crate::contracts::ErrorCode::CredentialMissing,
            "an unreachable store reported a missing credential"
        );
    }
}

/// Assert that a tool declaring `safe` replay can in fact be replayed.
///
/// Recovery re-invokes a step whose tool declared `replaySafety: safe`, with
/// the same arguments, after an interruption (`CORE_CONTRACTS.md` §11). So
/// "safe" is a promise about the world rather than the tool's intentions: a
/// second identical invocation must succeed, leave things as the first left
/// them, and report that it changed nothing. A write tool that reports safe
/// when it is not breaks the resume promise silently (`BUILD_ORDER.md` §16),
/// so every tool making that claim with a side effect is held to this.
///
/// `arguments` must describe a change the tool has not yet made under
/// `roots`. Every invocation carries a *different* invocation id, so
/// idempotence has to come from the state of the world rather than from the
/// tool recognising a retry.
///
/// # Panics
///
/// Panics with the failing expectation, which is how a test reports.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "this is a test suite; a panic is the failure report"
)]
pub async fn idempotent_tool_contract(
    provider: &dyn crate::ports::ToolProvider,
    tool_id: &str,
    arguments: serde_json::Value,
    roots: Vec<String>,
) {
    use crate::contracts::{
        AuthorizedTarget, AuthorizedToolRequest, Capability, CapabilityScope, GrantKind,
        GrantedAuthority, InvocationId, ProjectId, SideEffect,
    };

    let descriptor = provider
        .list_tools()
        .into_iter()
        .find(|tool| tool.tool_id == tool_id)
        .expect("the provider offers the tool under test");
    assert_eq!(
        descriptor.replay_safety,
        ReplaySafety::Safe,
        "{tool_id} does not declare safe replay, so this contract does not apply to it"
    );

    let attempt = |label: &str| {
        AuthorizedToolRequest::new(
            AuthorizedTarget {
                invocation_id: InvocationId::new(format!("contract-{label}")),
                task_id: Some(TaskId::new("task-contract")),
                tool_id: tool_id.to_owned(),
                arguments: arguments.clone(),
            },
            GrantedAuthority {
                capability: Capability::new(descriptor.capability.clone()),
                scope: CapabilityScope::project(ProjectId::new("project-contract"), roots.clone()),
                kind: GrantKind::Once,
                approval_id: None,
                site: None,
            },
        )
    };

    let first = provider
        .invoke(attempt("first"), CancellationToken::new())
        .await
        .expect("the first invocation succeeds");
    if descriptor.side_effect != SideEffect::None {
        assert_ne!(
            first.side_effect,
            SideEffect::None,
            "{tool_id}: the first invocation did not make the change its arguments describe"
        );
    }

    // Twice, so a tool that merely toggles state cannot pass by accident.
    for label in ["replay", "second-replay"] {
        let replay = provider
            .invoke(attempt(label), CancellationToken::new())
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{tool_id}: replaying a safe step failed on its own earlier effect: {error:?}"
                )
            });
        assert_eq!(
            replay.side_effect,
            SideEffect::None,
            "{tool_id}: a replay changed something again ({})",
            replay.display_summary
        );
    }
}
