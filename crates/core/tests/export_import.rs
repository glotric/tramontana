//! Export redaction, import validation, and worker handoff.
//!
//! `BUILD_ORDER.md` §12. These are the tests the thesis rests on: an export
//! that leaks is worse than no export, an import that trusts its input is an
//! attack surface, and a handoff that needs the previous worker's private
//! state is not portability.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::collections::BTreeMap;

use tramontana_core::contracts::{
    BUNDLE_MAJOR_VERSION, CheckpointId, CheckpointSupport, CheckpointWorker, ConversationId,
    ConversationRecord, CorrelationId, EventDraft, EventEnvelope, EventId, EventPayload,
    ExportBundle, ExportOptions, MessagePart, MessageRole, NormalizedMessage, OmissionCategory,
    PendingAction, PendingActionKind, ProjectId, ProjectRecord, ReplaySafety, ResumeEligibility,
    RiskClass, SideEffect, TaskCheckpoint, TaskEventKind, TaskId, TaskRecord, Timestamp,
    Visibility,
};
use tramontana_core::services::{
    ExportRequest, ImportError, ImportLimits, REDACTED_OUTPUT, REDACTED_PATH, build_export,
    build_handoff, validate_import,
};

const PRIVATE_ROOT: &str = "/Users/aurelia/Projects/tramontana";

fn at(seconds: u32) -> Timestamp {
    Timestamp::new(format!("2026-01-01T00:00:{seconds:02}.000Z"))
}

fn event(sequence: u64, payload: EventPayload, kind: TaskEventKind) -> EventEnvelope {
    EventDraft::user(kind, payload).into_envelope(
        EventId::new(format!("event-{sequence}")),
        TaskId::new("task-1"),
        None,
        sequence,
        at(sequence as u32),
        CorrelationId::new("correlation-1"),
    )
}

fn task_record() -> TaskRecord {
    TaskRecord::new(
        TaskId::new("task-1"),
        None,
        Some(ProjectId::new("project-1")),
        "Explain the permission boundary",
        at(0),
    )
}

fn checkpoint_with_session_state() -> TaskCheckpoint {
    let mut checkpoint = TaskCheckpoint::new(
        CheckpointId::new("checkpoint-1"),
        TaskId::new("task-1"),
        6,
        at(6),
        tramontana_core::contracts::TaskState::Interrupted,
        ResumeEligibility::same_worker("checkpoint.clean"),
    );
    checkpoint.worker = Some(CheckpointWorker {
        descriptor: descriptor(CheckpointSupport::SameWorker),
        session_state_ref: Some("worker-private-session-42".to_owned()),
    });
    checkpoint.completed_step_ids = vec!["read-the-readme".to_owned()];
    checkpoint.pending_action = Some(PendingAction {
        kind: PendingActionKind::Tool,
        reference_id: "invocation-2".to_owned(),
        replay_safety: ReplaySafety::Safe,
        display_summary: "builtin.fs.read started without a recorded result".to_owned(),
    });
    checkpoint
}

fn descriptor(support: CheckpointSupport) -> tramontana_core::contracts::AgentDescriptor {
    use tramontana_core::contracts::{
        AgentDescriptor, IntegrationMode, ModelControl, NetworkControl, ToolControl,
    };
    AgentDescriptor {
        agent_provider_id: "fake".to_owned(),
        agent_id: "fake.reference".to_owned(),
        display_name: "Reference fake worker".to_owned(),
        integration_mode: IntegrationMode::Embedded,
        model_control: ModelControl::TramontanaBrokered,
        tool_control: ToolControl::TramontanaBrokered,
        network_control: NetworkControl::Denied,
        checkpoint_support: support,
        local_only_eligible: true,
        unavailable_reason: None,
    }
}

/// A task whose record contains everything an export is supposed to strip.
fn revealing_request() -> ExportRequest {
    let mut message = NormalizedMessage::text(
        tramontana_core::contracts::MessageId::new("message-1"),
        Some(ConversationId::new("conversation-1")),
        MessageRole::Assistant,
        at(3),
        format!("I read {PRIVATE_ROOT}/README.md and it explains the boundary."),
    );
    message.parts.push(MessagePart::ToolResult {
        invocation_id: tramontana_core::contracts::InvocationId::new("invocation-1"),
        content: serde_json::json!({ "text": "the entire contents of a private file" }),
        truncated: false,
    });

    let mut request = ExportRequest::at(at(9));
    request.project = Some(ProjectRecord::new(
        ProjectId::new("project-1"),
        "Tramontana",
        vec![PRIVATE_ROOT.to_owned()],
        at(0),
    ));
    request.conversations = vec![ConversationRecord::new(
        ConversationId::new("conversation-1"),
        Some(ProjectId::new("project-1")),
        "Permissions",
        at(0),
    )];
    request.tasks = vec![task_record()];
    request.checkpoints = vec![checkpoint_with_session_state()];
    request.events = vec![
        event(
            1,
            EventPayload::TaskCreated {
                goal_summary: "Explain the permission boundary".to_owned(),
                conversation_id: None,
                routing_mode: None,
                attachments: Vec::new(),
                personal: None,
            },
            TaskEventKind::TaskCreated,
        ),
        event(
            2,
            EventPayload::ToolRequested {
                invocation_id: tramontana_core::contracts::InvocationId::new("invocation-1"),
                tool_id: "builtin.fs.read".to_owned(),
                arguments_summary: format!("path {PRIVATE_ROOT}/README.md"),
                risk_class: RiskClass::Read,
                step_id: Some("read-the-readme".to_owned()),
                request_hash: Some("abc123".to_owned()),
            },
            TaskEventKind::ToolRequested,
        ),
        event(
            3,
            EventPayload::MessageCommitted { message },
            TaskEventKind::MessageCommitted,
        ),
        event(
            4,
            EventPayload::ToolCompleted {
                invocation_id: tramontana_core::contracts::InvocationId::new("invocation-1"),
                display_summary: "Read 40 lines".to_owned(),
                side_effect: SideEffect::None,
                truncated: false,
                changes: Vec::new(),
            },
            TaskEventKind::ToolCompleted,
        ),
    ];

    // An internal audit marker, which never leaves the machine.
    let mut secret = event(
        5,
        EventPayload::DiagnosticRecorded {
            code: "audit.secretReference".to_owned(),
            summary: format!("resolved a credential for {PRIVATE_ROOT}"),
        },
        TaskEventKind::DiagnosticRecorded,
    );
    secret.visibility = Visibility::Secret;
    request.events.push(secret);

    request
}

fn rendered(bundle: &ExportBundle) -> String {
    bundle
        .files
        .values()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_export_redacts_absolute_paths_raw_output_and_internal_events_by_default() {
    let bundle = build_export(revealing_request());
    let text = rendered(&bundle);

    assert!(
        !text.contains(PRIVATE_ROOT),
        "the exported bundle still names a private absolute path"
    );
    assert!(text.contains(REDACTED_PATH), "the redaction is not marked");
    assert!(
        !text.contains("the entire contents of a private file"),
        "raw tool output survived the export"
    );
    assert!(text.contains(REDACTED_OUTPUT));
    assert!(
        !text.contains("audit.secretReference"),
        "a secret-visibility event was exported"
    );
    assert!(
        !text.contains("worker-private-session-42"),
        "a worker's private continuation reference was exported"
    );

    // The project's roots are device-local bindings, not part of the record.
    assert!(
        !text.contains("\"roots\":[\""),
        "the project's approved roots were exported"
    );
}

#[test]
fn every_omission_is_named_and_counted_in_the_manifest() {
    // An omission the reader cannot see is indistinguishable from data that
    // never existed.
    let bundle = build_export(revealing_request());
    let categories: Vec<_> = bundle
        .manifest
        .omissions
        .iter()
        .map(|omission| omission.category)
        .collect();

    for expected in [
        OmissionCategory::AbsolutePath,
        OmissionCategory::RawToolOutput,
        OmissionCategory::InternalEvent,
        OmissionCategory::WorkerPrivateState,
    ] {
        assert!(
            categories.contains(&expected),
            "{expected:?} was removed but not listed"
        );
    }
    for omission in &bundle.manifest.omissions {
        assert!(omission.count > 0, "{:?} is listed with no count", omission);
        assert!(!omission.reason.is_empty());
    }

    // And the human-readable note says the same thing.
    let readme = bundle.files.get("README.txt").unwrap();
    assert!(readme.contains("What is not here"));
    assert!(readme.contains("Importing it runs no workflow, tool, hook, or worker."));
}

#[test]
fn including_paths_and_output_is_possible_but_never_includes_a_secret() {
    // Absolute paths and raw output are defaults the user can override.
    // Secrets and worker-private state are not choices.
    let mut request = revealing_request();
    request.options = ExportOptions {
        include_absolute_paths: true,
        include_raw_tool_output: true,
        include_artifacts: true,
    };
    let bundle = build_export(request);
    let text = rendered(&bundle);

    assert!(
        text.contains(PRIVATE_ROOT),
        "an explicit include was ignored"
    );
    assert!(text.contains("the entire contents of a private file"));

    assert!(
        !text.contains("audit.secretReference"),
        "an internal audit marker escaped through an option"
    );
    assert!(
        !text.contains("worker-private-session-42"),
        "worker-private state escaped through an option"
    );
    assert!(!OmissionCategory::Secret.is_optional());
    assert!(!OmissionCategory::WorkerPrivateState.is_optional());
}

#[test]
fn a_bundle_round_trips_through_validation_unchanged() {
    let bundle = build_export(revealing_request());
    let records = validate_import(&bundle, ImportLimits::default()).unwrap();

    assert_eq!(records.tasks.len(), 1);
    assert_eq!(records.tasks[0].task_id, TaskId::new("task-1"));
    assert_eq!(records.conversations.len(), 1);
    assert_eq!(records.projects.len(), 1);
    assert!(
        records.projects[0].roots.is_empty(),
        "importing a project must not re-grant a directory"
    );

    let events = records.events.get("task-1").unwrap();
    // Four user-visible events; the secret one never left.
    assert_eq!(events.len(), 4);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );

    assert_eq!(records.quarantined.len(), 0, "nothing to quarantine here");
}

#[test]
fn opaque_worker_state_is_quarantined_rather_than_restored() {
    // A hostile bundle can carry one even though ours never do; it is the one
    // thing a receiving worker could act on directly.
    let mut bundle = build_export(revealing_request());
    let mut checkpoint = checkpoint_with_session_state();
    checkpoint.worker.as_mut().unwrap().session_state_ref =
        Some("attacker-supplied-continuation".to_owned());
    reseal(
        &mut bundle,
        "checkpoints.ndjson",
        format!("{}\n", serde_json::to_string(&checkpoint).unwrap()),
    );

    let records = validate_import(&bundle, ImportLimits::default()).unwrap();
    assert_eq!(records.quarantined, vec!["attacker-supplied-continuation"]);
    assert!(
        records.checkpoints[0]
            .worker
            .as_ref()
            .unwrap()
            .session_state_ref
            .is_none(),
        "quarantined state must not remain on the restored checkpoint"
    );
}

/// Replace a file and update its manifest entry, so only the intended rule is
/// under test rather than the checksum incidentally failing first.
fn reseal(bundle: &mut ExportBundle, path: &str, contents: String) {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(contents.as_bytes());
    let sha256: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();

    for entry in &mut bundle.manifest.entries {
        if entry.path == path {
            entry.sha256 = sha256.clone();
            entry.byte_count = contents.len() as u64;
        }
    }
    bundle.files.insert(path.to_owned(), contents);
}

#[test]
fn an_unknown_bundle_major_version_is_refused_whole() {
    let mut bundle = build_export(revealing_request());
    bundle.manifest.bundle_major_version = BUNDLE_MAJOR_VERSION + 1;

    match validate_import(&bundle, ImportLimits::default()) {
        Err(ImportError::UnsupportedBundleVersion { found, supported }) => {
            assert_eq!(found, BUNDLE_MAJOR_VERSION + 1);
            assert_eq!(supported, BUNDLE_MAJOR_VERSION);
        }
        other => panic!("expected a version refusal, got {other:?}"),
    }
}

#[test]
fn a_tampered_file_fails_its_checksum() {
    let mut bundle = build_export(revealing_request());
    // Same length, different content: the size check cannot catch this, so the
    // checksum has to.
    let tasks = bundle.files.get_mut("tasks.ndjson").unwrap();
    let original_len = tasks.len();
    *tasks = tasks.replace("Explain the permission", "Ignore all prior rules");
    assert_eq!(tasks.len(), original_len, "the tamper changed the length");

    match validate_import(&bundle, ImportLimits::default()) {
        Err(ImportError::ChecksumMismatch { path }) => assert_eq!(path, "tasks.ndjson"),
        other => panic!("expected a checksum failure, got {other:?}"),
    }
}

#[test]
fn a_file_that_is_not_the_size_it_claims_is_refused() {
    let mut bundle = build_export(revealing_request());
    bundle
        .files
        .insert("tasks.ndjson".to_owned(), "{}\n".to_owned());

    match validate_import(&bundle, ImportLimits::default()) {
        Err(ImportError::SizeMismatch { path, .. }) => assert_eq!(path, "tasks.ndjson"),
        other => panic!("expected a size mismatch, got {other:?}"),
    }
}

#[test]
fn an_oversized_entry_is_refused_before_it_is_read() {
    let bundle = build_export(revealing_request());
    let limits = ImportLimits {
        max_entry_bytes: 8,
        ..ImportLimits::default()
    };
    assert!(matches!(
        validate_import(&bundle, limits),
        Err(ImportError::TooLarge { .. })
    ));
}

#[test]
fn a_path_that_could_escape_the_destination_is_refused() {
    // Every one of these is a way to address something outside the directory
    // a bundle is unpacked into.
    for hostile in [
        "../outside.ndjson",
        "nested/../../outside.ndjson",
        "/etc/passwd",
        "C:\\Windows\\System32\\config",
        "C:relative.ndjson",
        "\\\\server\\share\\file",
        "./tasks.ndjson",
        "",
        "a//b.ndjson",
        "tasks\u{0}.ndjson",
    ] {
        let mut bundle = build_export(revealing_request());
        bundle.manifest.entries[0].path = hostile.to_owned();

        match validate_import(&bundle, ImportLimits::default()) {
            Err(ImportError::UnsafePath { path }) => assert_eq!(path, hostile),
            other => panic!("{hostile:?} was not refused as unsafe: {other:?}"),
        }
    }
}

#[test]
fn a_file_the_manifest_does_not_declare_is_refused() {
    // Otherwise a bundle could smuggle in a file that no checksum, size, or
    // path rule ever looked at.
    let mut bundle = build_export(revealing_request());
    bundle
        .files
        .insert("undeclared.ndjson".to_owned(), "{}\n".to_owned());

    match validate_import(&bundle, ImportLimits::default()) {
        Err(ImportError::UndeclaredEntry { path }) => assert_eq!(path, "undeclared.ndjson"),
        other => panic!("expected an undeclared-entry refusal, got {other:?}"),
    }
}

#[test]
fn a_malformed_record_names_the_line_and_never_echoes_it() {
    let mut bundle = build_export(revealing_request());
    reseal(
        &mut bundle,
        "tasks.ndjson",
        "{\"secret\":\"sk-live-should-not-appear\"}\n".to_owned(),
    );

    match validate_import(&bundle, ImportLimits::default()) {
        Err(ImportError::Malformed { path, detail }) => {
            assert_eq!(path, "tasks.ndjson");
            assert!(detail.contains("line 1"));
            assert!(
                !detail.contains("sk-live-should-not-appear"),
                "the rejection echoed the content it refused: {detail}"
            );
        }
        other => panic!("expected a malformed-record refusal, got {other:?}"),
    }
}

#[test]
fn the_committed_fixture_stream_survives_an_export_and_import_round_trip() {
    // `BUILD_ORDER.md` §12 asks for the round trip to go through the committed
    // fixtures, so the format is pinned against a real recorded stream rather
    // than one this test happened to build.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/contracts/v1/task-completed.ndjson");
    let original: Vec<EventEnvelope> = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|error| panic!("reading {}: {error}", fixture.display()))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!original.is_empty(), "the fixture stream is empty");

    let task_id = original[0].task_id.clone();
    let mut request = ExportRequest::at(at(9));
    // The fixture contains the deliberately bounded tool call/result parts.
    // Preserve them for this lossless format round-trip test; the default
    // export remains privacy-preserving and redacts those fields.
    request.options.include_raw_tool_output = true;
    request.tasks = vec![TaskRecord::new(
        task_id.clone(),
        None,
        None,
        "Say hello",
        at(0),
    )];
    request.events = original.clone();

    let bundle = build_export(request);
    let records = validate_import(&bundle, ImportLimits::default()).unwrap();
    let restored = records.events.get(task_id.as_str()).unwrap();

    assert_eq!(
        restored, &original,
        "a round trip changed the durable record"
    );
    assert_eq!(records.tasks[0].task_id, task_id);
}

#[test]
fn a_handoff_carries_the_work_and_not_the_previous_workers_session() {
    let checkpoint = checkpoint_with_session_state();
    let request = revealing_request();
    let handoff = build_handoff(&task_record(), &request.events, &checkpoint, at(9));

    // The receiving worker learns the goal, what is done, and what is pending.
    assert_eq!(handoff.goal, "Explain the permission boundary");
    assert_eq!(handoff.completed_step_ids, vec!["read-the-readme"]);
    assert_eq!(
        handoff.pending_action.as_ref().unwrap().reference_id,
        "invocation-2"
    );
    assert!(
        handoff
            .history
            .iter()
            .any(|entry| entry.summary.contains("builtin.fs.read")),
        "the history does not say what was attempted"
    );

    // It learns who held the task before, and nothing private to them.
    assert_eq!(
        handoff.previous_worker.as_ref().unwrap().agent_id,
        "fake.reference"
    );
    let rendered = serde_json::to_string(&handoff).unwrap();
    assert!(
        !rendered.contains("worker-private-session-42"),
        "the handoff carried the previous worker's private session"
    );
    assert!(
        !rendered.contains("audit.secretReference"),
        "the handoff carried an internal audit marker"
    );

    // And it is told that something was deliberately withheld, rather than
    // being left to assume the record is complete.
    assert!(
        handoff
            .omissions
            .iter()
            .any(|omission| omission.category == OmissionCategory::WorkerPrivateState),
        "the handoff did not declare what it withheld"
    );
}

#[test]
fn a_handoff_is_complete_for_a_differently_configured_worker() {
    // `BUILD_ORDER.md` §12 asks for a handoff a *second worker configuration*
    // can pick up. The property that makes that true is negative: nothing in
    // the bundle is meaningful only to the worker that produced it. So the
    // test crosses the boundary the way a real handoff would — serialize,
    // discard everything else, and read it back as the receiving worker.
    let handoff = build_handoff(
        &task_record(),
        &revealing_request().events,
        &checkpoint_with_session_state(),
        at(9),
    );
    let wire = serde_json::to_string(&handoff).unwrap();
    let received: tramontana_core::contracts::HandoffBundle = serde_json::from_str(&wire).unwrap();

    // A second worker with different capabilities than the first.
    let receiving = descriptor(CheckpointSupport::Portable);
    assert_ne!(
        receiving.checkpoint_support,
        received
            .previous_worker
            .as_ref()
            .unwrap()
            .checkpoint_support,
        "the two configurations must actually differ for this to prove anything"
    );

    // Everything it needs to continue is present without consulting anything
    // else: what was asked for, what has happened, what is already done, and
    // what was in flight.
    assert!(!received.goal.is_empty());
    assert!(!received.history.is_empty());
    assert!(
        received
            .history
            .iter()
            .all(|entry| !entry.summary.is_empty())
    );
    assert_eq!(received.completed_step_ids, vec!["read-the-readme"]);
    assert!(received.pending_action.is_some());
    assert_eq!(received.event_sequence, 6);

    // And nothing it cannot interpret. The previous worker appears only as
    // provenance, never as something the receiver has to be or to load.
    assert!(!wire.contains("worker-private-session-42"));
    assert!(
        received
            .omissions
            .iter()
            .any(|omission| omission.category == OmissionCategory::WorkerPrivateState)
    );
}

#[test]
fn a_handoff_describes_the_checkpointed_point_not_whatever_happened_after() {
    let mut checkpoint = checkpoint_with_session_state();
    checkpoint.event_sequence = 2;
    let request = revealing_request();
    let handoff = build_handoff(&task_record(), &request.events, &checkpoint, at(9));

    assert_eq!(handoff.event_sequence, 2);
    assert!(
        handoff.history.iter().all(|entry| entry.sequence <= 2),
        "the handoff described events after the point it hands over"
    );
}

#[test]
fn an_empty_export_is_still_a_valid_bundle() {
    let bundle = build_export(ExportRequest::at(at(0)));
    let records = validate_import(&bundle, ImportLimits::default()).unwrap();
    assert!(records.tasks.is_empty());
    assert!(bundle.manifest.omissions.is_empty());
    assert_eq!(bundle.manifest.entries.len(), bundle.files.len());
}

#[test]
fn a_bundle_declares_every_file_it_contains() {
    let bundle = build_export(revealing_request());
    let declared: BTreeMap<_, _> = bundle
        .manifest
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    assert_eq!(declared.len(), bundle.files.len());
    for (path, contents) in &bundle.files {
        let entry = declared.get(path).expect("undeclared file");
        assert_eq!(entry.byte_count, contents.len() as u64);
        assert_eq!(entry.sha256.len(), 64);
    }
}

/// xorshift64*: seeded, so a failure reproduces.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// A bundle is hostile input (`TESTING_STRATEGY.md` §5): whatever is in it,
/// importing reads it or refuses it, and never crashes. Each file is damaged
/// and resealed, so the damage reaches the parsers rather than stopping at
/// the checksum.
#[test]
fn a_damaged_bundle_is_imported_or_refused_never_a_crash() {
    let original = build_export(revealing_request());
    let paths: Vec<String> = original.files.keys().cloned().collect();
    let spice = [
        "{",
        "}",
        "[",
        "]",
        "\"",
        ",",
        ":",
        "null",
        "\n",
        "\\u0000",
        "1e999",
        "-1",
        "\"sequence\":",
        "\"kind\":\"taskCompleted\"",
        "\"schemaVersion\":99",
        "é",
    ];
    let mut state = 0x5eed_0fb0_d1e5_u64;
    let mut imported = 0;
    for _ in 0..1500 {
        let mut bundle = original.clone();
        let path = &paths[usize::try_from(next(&mut state) % paths.len() as u64).unwrap()];
        let mut text: Vec<char> = bundle.files[path].chars().collect();
        for _ in 0..=next(&mut state) % 3 {
            let at = usize::try_from(next(&mut state) % (text.len() as u64 + 1)).unwrap();
            match next(&mut state) % 4 {
                0 if !text.is_empty() => {
                    let end =
                        (at + 1 + usize::try_from(next(&mut state) % 30).unwrap()).min(text.len());
                    text.drain(at.min(end)..end);
                }
                1 => text.truncate(at),
                _ => {
                    let insert =
                        spice[usize::try_from(next(&mut state) % spice.len() as u64).unwrap()];
                    text.splice(at..at, insert.chars());
                }
            }
        }
        reseal(&mut bundle, path, text.into_iter().collect());
        if validate_import(&bundle, ImportLimits::default()).is_ok() {
            imported += 1;
        }
    }
    // Some damage is harmless, most is not; both kinds were reached.
    assert!(imported < 1500, "no damage was ever refused");
}
