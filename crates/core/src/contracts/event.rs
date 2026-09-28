//! The durable event stream.
//!
//! `CORE_CONTRACTS.md` §5 is normative. Every fact about a task is an event;
//! the task's current state is a projection of its events, never a separate
//! source of truth.
//!
//! Three rules shape this module:
//!
//! - **`sequence` starts at 1 and increases by exactly one per task.** A gap
//!   means loss, and callers can detect it without consulting a clock.
//! - **Streaming deltas are not events.** Tokens arrive as
//!   [`StreamFrame`]s, which are transport-only; accepted content is coalesced
//!   into one durable `messageCommitted` when the stream ends.
//! - **Redaction happens before an event exists**, not before it is displayed.
//!   The interface is not the last line of defence.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    ConversationId, CorrelationId, ErrorEnvelope, EventId, RoutingMode, SCHEMA_VERSION, TaskId,
    TaskState, Timestamp, Visibility,
};

/// A file the person attached to a message (ADR 0058).
///
/// Held in the task's own record, beside its goal: the text is what the model
/// is given, already redacted, and the hash names that text rather than the
/// original bytes, so the record describes only what may be released.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskAttachment {
    /// The file's name, without any folder.
    pub name: String,
    /// What kind of text it is, such as `text/markdown`.
    pub media_type: String,
    /// Its redacted text.
    pub text: String,
    /// The content hash of `text`.
    pub content_hash: String,
    /// The length of `text` in bytes.
    pub byte_count: u64,
    /// What was removed from it at ingestion.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub redactions: Vec<String>,
}

/// What a durable event is about.
///
/// `#[non_exhaustive]` and the `Unsupported` fallback in
/// [`EventPayload`] together implement the compatibility rule in
/// `CORE_CONTRACTS.md` §2: an unknown event kind is stored and shown as an
/// unsupported redacted event, never a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum TaskEventKind {
    /// The task was created.
    TaskCreated,
    /// The task moved between states.
    TaskStateChanged,
    /// Effective configuration was resolved.
    ConfigurationResolved,
    /// A context bundle was prepared.
    ContextPrepared,
    /// A model and location were chosen.
    RoutingDecided,
    /// A payload was released to a destination.
    ContextReleased,
    /// A worker was selected.
    WorkerSelected,
    /// A worker session started.
    WorkerStarted,
    /// A model request began.
    ModelStarted,
    /// A durable normalized message was committed.
    MessageCommitted,
    /// A worker asked to run a tool.
    ToolRequested,
    /// An approval was put to the user.
    ApprovalRequested,
    /// The user decided an approval.
    ApprovalResolved,
    /// An authorized tool invocation began.
    ToolStarted,
    /// A tool invocation finished.
    ToolCompleted,
    /// A tool invocation failed.
    ToolFailed,
    /// A checkpoint was written.
    CheckpointSaved,
    /// An artifact was recorded.
    ArtifactRecorded,
    /// A worker session stopped.
    WorkerStopped,
    /// The task completed.
    TaskCompleted,
    /// The task failed.
    TaskFailed,
    /// The task was cancelled.
    TaskCancelled,
    /// A bounded, redacted diagnostic was recorded.
    DiagnosticRecorded,
}

/// The body of a durable event.
///
/// Tagged by `kind` so the discriminator is one field in both Rust and
/// TypeScript. M1 defines the payloads the walking skeleton emits; the rest
/// arrive with the milestones that produce them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum EventPayload {
    /// A bounded explanation of a core lifecycle decision.
    #[serde(rename_all = "camelCase")]
    DiagnosticRecorded {
        /// Stable machine-readable code.
        code: String,
        /// Secret-free description.
        summary: String,
    },
    /// The task was created.
    #[serde(rename_all = "camelCase")]
    TaskCreated {
        /// One-line, secret-free summary of what was asked for.
        goal_summary: String,
        /// Conversation this task belongs to, when it has one.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        conversation_id: Option<ConversationId>,
        /// Where this task's models may run, chosen when it was created and
        /// kept if it is resumed.
        ///
        /// Absent from tasks recorded before the mode was, which resume under
        /// the profile's mode (ADR 0031).
        #[serde(skip_serializing_if = "Option::is_none", default)]
        routing_mode: Option<RoutingMode>,
        /// Files the person attached to this message, as their redacted text
        /// (ADR 0058). Absent when there were none, and from every task
        /// recorded before attachments were.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        attachments: Vec<TaskAttachment>,
        /// What the person's profile told this task's model about them, as
        /// it was when the task started (ADR 0061). Absent when nothing was
        /// told: every task with a project, and every chat before profiles.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        personal: Option<String>,
    },
    /// The task moved between states.
    #[serde(rename_all = "camelCase")]
    TaskStateChanged {
        /// State before the move.
        previous: TaskState,
        /// State after the move.
        current: TaskState,
        /// Stable, machine-readable reason.
        reason_code: String,
    },
    /// Effective configuration was resolved before execution began.
    #[serde(rename_all = "camelCase")]
    ConfigurationResolved {
        /// Secret-free manifest metadata. Markdown bodies are not copied.
        configuration: super::EffectiveConfiguration,
    },
    /// A context bundle was assembled for a destination.
    ///
    /// Carries the manifest whole rather than an identifier, because the
    /// record has to explain a data release without a second lookup — and
    /// because an exported bundle must still explain it on a machine that has
    /// none of the original storage.
    #[serde(rename_all = "camelCase")]
    ContextPrepared {
        /// What was assembled, and what was withheld.
        manifest: super::ContextManifest,
    },
    /// A payload was released to a destination, on a recorded authority.
    ///
    /// Separate from `contextPrepared` because assembling is not sending. One
    /// assembly can produce several releases — a fallback that crosses to a
    /// different destination is a second one — and each has its own
    /// disclosure and its own approval (`SECURITY_AND_PERMISSIONS.md` §8).
    #[serde(rename_all = "camelCase")]
    ContextReleased {
        /// What went, where, and on whose authority.
        ///
        /// Boxed because it is by far the largest payload, and every event in
        /// the system is moved by value: without the indirection each of the
        /// twenty-odd other variants would carry its footprint.
        plan: Box<super::ContextReleasePlan>,
    },
    /// A model and a location were chosen.
    #[serde(rename_all = "camelCase")]
    RoutingDecided {
        /// The decision, its reasons, and the context it released.
        decision: super::RoutingDecision,
    },
    /// A worker was selected for this task.
    ///
    /// Carries the whole descriptor rather than a name, because a checkpoint
    /// is derived from this stream and a handoff needs the capability facts
    /// (`CORE_CONTRACTS.md` §5.1, `PROJECT_FORMAT.md` §4). Recording only an
    /// identifier would mean a resumed task had to ask a provider what it once
    /// was, which is exactly the dependency a portable record must not have.
    #[serde(rename_all = "camelCase")]
    WorkerSelected {
        /// Capability snapshot taken at selection.
        descriptor: super::AgentDescriptor,
    },
    /// A model request began.
    #[serde(rename_all = "camelCase")]
    ModelStarted {
        /// Correlates the ephemeral stream frames with this durable event.
        stream_id: String,
        /// Namespaced model identifier.
        model_id: String,
    },
    /// A durable normalized message was committed.
    #[serde(rename_all = "camelCase")]
    MessageCommitted {
        /// The message.
        message: super::NormalizedMessage,
    },
    /// A worker asked to run a tool.
    ///
    /// Carries a *summary* of the arguments, never the arguments themselves:
    /// they can hold file content or other project data, and the durable
    /// stream is displayed and exported (`CORE_CONTRACTS.md` §5).
    #[serde(rename_all = "camelCase")]
    ToolRequested {
        /// Identifies this invocation across its request and result.
        invocation_id: super::InvocationId,
        /// Namespaced tool identifier.
        tool_id: String,
        /// Short, safe description of what was asked for.
        arguments_summary: String,
        /// How much damage the tool could do.
        risk_class: super::RiskClass,
        /// Worker-supplied stable step identity, scoped to this task.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        step_id: Option<String>,
        /// Hash of the tool identity and normalized arguments, never raw values.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        request_hash: Option<String>,
    },
    /// A permission decision was reached.
    ///
    /// Emitted for *every* decision, including the ones nobody was asked
    /// about. The exit criterion for M3 is that every action carries an
    /// origin, a scope, and a decision — a silently-permitted action would
    /// satisfy the permission system and fail the audit requirement.
    #[serde(rename_all = "camelCase")]
    PermissionDecided {
        /// The invocation the decision covers.
        invocation_id: super::InvocationId,
        /// Who asked.
        origin: PermissionOrigin,
        /// What was asked for.
        capability: String,
        /// What it was limited to.
        scope: super::CapabilityScope,
        /// Whether it may proceed.
        allowed: bool,
        /// Stable reason, matching `ports::reason`.
        reason_code: String,
        /// How the authorization was obtained, when it was allowed.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        grant_kind: Option<super::GrantKind>,
    },
    /// An authorized tool invocation began.
    #[serde(rename_all = "camelCase")]
    ToolStarted {
        /// The invocation.
        invocation_id: super::InvocationId,
        /// The tool.
        tool_id: String,
        /// Effect classification recorded before execution. Legacy events are unknown.
        #[serde(default)]
        replay_safety: super::ReplaySafety,
    },
    /// A tool invocation failed.
    #[serde(rename_all = "camelCase")]
    ToolFailed {
        /// The invocation.
        invocation_id: super::InvocationId,
        /// Normalized, already-redacted error.
        error: ErrorEnvelope,
    },
    /// A tool invocation finished.
    #[serde(rename_all = "camelCase")]
    ToolCompleted {
        /// The invocation this closes.
        invocation_id: super::InvocationId,
        /// Short, safe description of the result.
        display_summary: String,
        /// What the invocation actually changed.
        side_effect: super::SideEffect,
        /// Whether output was cut short by a limit.
        truncated: bool,
        /// The files this invocation changed, as the tool reported them.
        /// Absent from records written before tools reported changes
        /// (ADR 0026).
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        changes: Vec<super::FileChange>,
    },
    /// Something the task produced, recorded as Tramontana's own fact.
    #[serde(rename_all = "camelCase")]
    ArtifactRecorded {
        /// The artifact.
        artifact: super::ArtifactRecord,
    },
    /// A checkpoint was written.
    ///
    /// The checkpoint itself lives in its own repository; this event records
    /// that one exists at this point in the stream, and enough of it to be
    /// read without a second lookup.
    #[serde(rename_all = "camelCase")]
    CheckpointSaved {
        /// The checkpoint.
        checkpoint_id: super::CheckpointId,
        /// The durable sequence it was taken at.
        event_sequence: u64,
        /// How many steps it records as already done.
        completed_step_count: u64,
        /// What was in flight, when anything was.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pending_action: Option<super::PendingAction>,
        /// Whether the task can be picked up again from here.
        resume: super::ResumeEligibility,
    },
    /// The task completed successfully.
    #[serde(rename_all = "camelCase")]
    TaskCompleted {
        /// Secret-free summary of the outcome.
        summary: String,
        /// Artifacts the task recorded, such as the files it changed.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        artifacts: Vec<super::ArtifactId>,
    },
    /// The task failed.
    #[serde(rename_all = "camelCase")]
    TaskFailed {
        /// Normalized, already-redacted error.
        error: ErrorEnvelope,
    },
    /// The task was cancelled.
    #[serde(rename_all = "camelCase")]
    TaskCancelled {
        /// Who or what asked for cancellation.
        origin: CancellationOrigin,
    },
    /// An event kind this build does not understand.
    ///
    /// Reading one is not an error: a newer core may write kinds an older
    /// interface has never heard of, and the contract requires storing and
    /// displaying them rather than failing (`CORE_CONTRACTS.md` §2).
    #[serde(other)]
    Unsupported,
}

/// Who asked for a capability.
///
/// An audit record that cannot say *who* asked is not an audit record. A
/// built-in tool invoked by the user and the same tool invoked by an external
/// worker are different events, even when the arguments match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum PermissionOrigin {
    /// The person, directly.
    User,
    /// A worker acting on the task.
    #[serde(rename_all = "camelCase")]
    Worker {
        /// Namespaced worker identifier.
        agent_id: String,
    },
    /// A named workflow.
    #[serde(rename_all = "camelCase")]
    Workflow {
        /// Workflow identifier.
        workflow_id: String,
    },
}

/// Who requested a cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CancellationOrigin {
    /// The user asked for it.
    User,
    /// A budget, deadline, or policy stopped the work.
    System,
}

/// A durable event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    /// Contract schema version.
    pub schema_version: u32,
    /// Unique identifier for this event.
    pub event_id: EventId,
    /// Task this event belongs to.
    pub task_id: TaskId,
    /// Conversation, when the task has one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub conversation_id: Option<ConversationId>,
    /// Position in the task's stream. Starts at 1, increases by exactly one.
    pub sequence: u64,
    /// When the event happened.
    pub occurred_at: Timestamp,
    /// The event kind, duplicated outside the payload so a reader can route on
    /// it without parsing a body it may not understand.
    pub kind: TaskEventKind,
    /// Who this event is for.
    pub visibility: Visibility,
    /// Shared by everything caused by one originating request.
    pub correlation_id: CorrelationId,
    /// The command, event, invocation, or approval that caused this one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub causation_id: Option<String>,
    /// The body.
    pub payload: EventPayload,
}

impl EventEnvelope {
    /// Whether this event may be delivered to a webview.
    ///
    /// `CORE_CONTRACTS.md` §5: `secret` events are internal audit markers
    /// holding references and hashes only, and never reach the interface.
    #[must_use]
    pub fn is_deliverable_to_ui(&self) -> bool {
        !matches!(self.visibility, Visibility::Secret)
    }

    /// Whether this event records the final outcome of a task.
    ///
    /// A terminal `taskStateChanged` is deliberately not enough: the outcome
    /// event that follows it carries the result, failure, or cancellation
    /// detail and closes the durable stream.
    #[must_use]
    pub fn is_terminal_outcome(&self) -> bool {
        matches!(
            &self.payload,
            EventPayload::TaskCompleted { .. }
                | EventPayload::TaskFailed { .. }
                | EventPayload::TaskCancelled { .. }
        )
    }
}

/// One frame of an in-flight model stream.
///
/// Explicitly **not** an [`EventEnvelope`]: frames are transport, are never
/// persisted, and carry no sequence in the durable stream. A reconnecting
/// interface rebuilds from durable events plus, optionally, the current
/// stream snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamFrame {
    /// Correlates with the `modelStarted` event that opened this stream.
    pub stream_id: String,
    /// Monotonically increasing position within this stream.
    pub offset: u64,
    /// The text produced.
    pub text: String,
}

/// One item delivered to a task subscriber.
///
/// Durable events and ephemeral frames share a channel so a subscriber sees
/// them interleaved in the order they happened, but they stay distinct
/// variants: only [`TaskUpdate::Event`] carries a sequence, and only it
/// survives a restart. An interface that treats a frame as durable will lose
/// it on reconnect and be surprised; the type makes that hard to do by
/// accident.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum TaskUpdate {
    /// A durable event.
    ///
    /// Boxed because an envelope is far larger than a frame, and frames are
    /// the high-frequency variant: without the indirection every frame on the
    /// channel would carry the envelope's footprint. The serialized shape is
    /// unaffected.
    #[serde(rename_all = "camelCase")]
    Event {
        /// The event.
        event: Box<EventEnvelope>,
    },
    /// An ephemeral model stream frame. Never persisted.
    #[serde(rename_all = "camelCase")]
    Frame {
        /// The frame.
        frame: StreamFrame,
    },
}

/// A builder for events that keeps sequence assignment in one place.
///
/// Sequence numbers are assigned by the event store on append, not by callers,
/// so this produces a draft that the store completes.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    /// The event kind.
    pub kind: TaskEventKind,
    /// Who the event is for.
    pub visibility: Visibility,
    /// The body.
    pub payload: EventPayload,
}

impl EventDraft {
    /// A user-visible event.
    #[must_use]
    pub fn user(kind: TaskEventKind, payload: EventPayload) -> Self {
        Self {
            kind,
            visibility: Visibility::User,
            payload,
        }
    }

    /// Complete this draft into a durable event.
    #[must_use]
    pub fn into_envelope(
        self,
        event_id: EventId,
        task_id: TaskId,
        conversation_id: Option<ConversationId>,
        sequence: u64,
        occurred_at: Timestamp,
        correlation_id: CorrelationId,
    ) -> EventEnvelope {
        EventEnvelope {
            schema_version: SCHEMA_VERSION,
            event_id,
            task_id,
            conversation_id,
            sequence,
            occurred_at,
            kind: self.kind,
            visibility: self.visibility,
            correlation_id,
            causation_id: None,
            payload: self.payload,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn envelope(payload: EventPayload, kind: TaskEventKind) -> EventEnvelope {
        EventDraft::user(kind, payload).into_envelope(
            EventId::new("e1"),
            TaskId::new("t1"),
            None,
            1,
            Timestamp::new("2026-09-07T12:00:00.000Z"),
            CorrelationId::new("c1"),
        )
    }

    #[test]
    fn events_serialize_with_a_kind_tagged_payload() {
        let event = envelope(
            EventPayload::TaskStateChanged {
                previous: TaskState::Queued,
                current: TaskState::Preparing,
                reason_code: "task.started".into(),
            },
            TaskEventKind::TaskStateChanged,
        );
        let json = serde_json::to_value(&event).unwrap();

        assert_eq!(json["kind"], "taskStateChanged");
        assert_eq!(json["payload"]["kind"], "taskStateChanged");
        assert_eq!(json["payload"]["previous"], "queued");
        assert_eq!(json["payload"]["current"], "preparing");
        assert_eq!(json["sequence"], 1);
        assert_eq!(json["schemaVersion"], SCHEMA_VERSION);
        // A task without a conversation omits the field rather than nulling it.
        assert!(!json.as_object().unwrap().contains_key("conversationId"));
    }

    #[test]
    fn only_outcome_events_close_a_task_stream() {
        let state_change = envelope(
            EventPayload::TaskStateChanged {
                previous: TaskState::Running,
                current: TaskState::Completed,
                reason_code: "task.completed".into(),
            },
            TaskEventKind::TaskStateChanged,
        );
        let completed = envelope(
            EventPayload::TaskCompleted {
                summary: "done".into(),
                artifacts: Vec::new(),
            },
            TaskEventKind::TaskCompleted,
        );
        let failed = envelope(
            EventPayload::TaskFailed {
                error: ErrorEnvelope::new(
                    super::super::ErrorCode::Internal,
                    "failed",
                    CorrelationId::new("failure"),
                ),
            },
            TaskEventKind::TaskFailed,
        );
        let cancelled = envelope(
            EventPayload::TaskCancelled {
                origin: CancellationOrigin::User,
            },
            TaskEventKind::TaskCancelled,
        );

        assert!(!state_change.is_terminal_outcome());
        assert!(completed.is_terminal_outcome());
        assert!(failed.is_terminal_outcome());
        assert!(cancelled.is_terminal_outcome());
    }

    #[test]
    fn an_unknown_event_kind_is_stored_rather_than_rejected() {
        // A newer core may emit kinds this build has never heard of. The
        // contract requires reading them as an unsupported event, not failing
        // to parse the stream (`CORE_CONTRACTS.md` §2).
        let future = serde_json::json!({
            "schemaVersion": 1,
            "eventId": "e9",
            "taskId": "t1",
            "sequence": 9,
            "occurredAt": "2026-09-07T12:00:00.000Z",
            "kind": "taskStateChanged",
            "visibility": "user",
            "correlationId": "c1",
            "payload": { "kind": "quantumEntangled", "spookiness": 11 },
        });

        let parsed: EventEnvelope = serde_json::from_value(future).unwrap();
        assert_eq!(parsed.payload, EventPayload::Unsupported);
        assert_eq!(parsed.sequence, 9);
    }

    #[test]
    fn an_unknown_additive_field_does_not_break_parsing() {
        let future = serde_json::json!({
            "schemaVersion": 1,
            "eventId": "e1",
            "taskId": "t1",
            "sequence": 1,
            "occurredAt": "2026-09-07T12:00:00.000Z",
            "kind": "taskCompleted",
            "visibility": "user",
            "correlationId": "c1",
            "tenantId": "added-in-a-later-version",
            "payload": { "kind": "taskCompleted", "summary": "done" },
        });

        let parsed: EventEnvelope = serde_json::from_value(future).unwrap();
        assert_eq!(parsed.kind, TaskEventKind::TaskCompleted);
    }

    #[test]
    fn secret_events_are_never_deliverable_to_the_interface() {
        let mut event = envelope(
            EventPayload::TaskCompleted {
                summary: "done".into(),
                artifacts: Vec::new(),
            },
            TaskEventKind::TaskCompleted,
        );
        assert!(event.is_deliverable_to_ui());

        event.visibility = Visibility::Secret;
        assert!(!event.is_deliverable_to_ui());

        event.visibility = Visibility::Diagnostic;
        assert!(
            event.is_deliverable_to_ui(),
            "diagnostics are shown, not hidden"
        );
    }

    #[test]
    fn stream_frames_are_transport_and_carry_no_durable_sequence() {
        let frame = StreamFrame {
            stream_id: "s1".into(),
            offset: 3,
            text: "hello".into(),
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["offset"], 3);
        assert!(!json.as_object().unwrap().contains_key("sequence"));
    }
}
