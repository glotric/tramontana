//! Checkpoints and replay safety.
//!
//! `CORE_CONTRACTS.md` §11 and `PROJECT_FORMAT.md` §4 are normative. One
//! property matters more than the fields:
//!
//! **A checkpoint is derived from the durable event stream, never from a
//! worker's private state.** That is what makes it portable. An opaque
//! continuation token may improve fidelity when the same worker resumes, but
//! it is carried as an optional reference and is never the record.
//!
//! The second property is the one that protects the user: after an unclean
//! stop, an action whose effects are [`ReplaySafety::Unsafe`] or
//! [`ReplaySafety::Unknown`] is **never replayed automatically**. The task
//! becomes [`TaskState::Interrupted`] and a person decides.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{AgentDescriptor, CheckpointId, SCHEMA_VERSION, TaskId, TaskState, Timestamp};

/// What kind of work was in flight when the task stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum PendingActionKind {
    /// A model request had started and not returned.
    Model,
    /// A tool invocation had started and not reported an outcome.
    Tool,
    /// A question had been put to the user and not answered.
    Approval,
    /// A worker session had started and not stopped.
    Worker,
}

/// Whether repeating an action is safe.
///
/// This is a statement about *effects*, not about whether the action would
/// succeed. Reading a file twice is safe; writing one twice may not be, and a
/// tool whose effects are not known is treated as though they are not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ReplaySafety {
    /// Repeating it changes nothing outside Tramontana.
    Safe,
    /// Repeating it could duplicate an effect.
    Unsafe,
    /// Not determinable from the record. Treated as [`ReplaySafety::Unsafe`].
    #[default]
    Unknown,
}

impl ReplaySafety {
    /// Whether the core may repeat this action without asking.
    ///
    /// `Unknown` answers `false`. An unclassifiable action is the case this
    /// exists for: the alternative is guessing on the user's behalf about an
    /// effect nobody recorded.
    #[must_use]
    pub fn is_automatically_replayable(self) -> bool {
        matches!(self, Self::Safe)
    }
}

/// The one action that had started and not finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingAction {
    /// What kind of action it was.
    pub kind: PendingActionKind,
    /// The invocation, request, or approval it refers to.
    pub reference_id: String,
    /// Whether repeating it is safe.
    pub replay_safety: ReplaySafety,
    /// Short, secret-free description for a person deciding what to do.
    pub display_summary: String,
}

/// How a task may be picked up again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ResumeMode {
    /// The worker that was running it continues.
    SameWorker,
    /// A different worker continues from canonical Tramontana state.
    CanonicalHandoff,
}

/// Whether, and how, this checkpoint can be resumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResumeEligibility {
    /// Whether resuming is offered at all.
    pub eligible: bool,
    /// How, when it is.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<ResumeMode>,
    /// Stable reason, present whenever the answer needs explaining.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason_code: Option<String>,
}

impl ResumeEligibility {
    /// Resumable by the worker that was running it.
    #[must_use]
    pub fn same_worker(reason_code: impl Into<String>) -> Self {
        Self {
            eligible: true,
            mode: Some(ResumeMode::SameWorker),
            reason_code: Some(reason_code.into()),
        }
    }

    /// Resumable by a different worker from canonical state.
    #[must_use]
    pub fn canonical_handoff(reason_code: impl Into<String>) -> Self {
        Self {
            eligible: true,
            mode: Some(ResumeMode::CanonicalHandoff),
            reason_code: Some(reason_code.into()),
        }
    }

    /// Not resumable, with the reason a person needs.
    #[must_use]
    pub fn refused(reason_code: impl Into<String>) -> Self {
        Self {
            eligible: false,
            mode: None,
            reason_code: Some(reason_code.into()),
        }
    }
}

/// The worker a checkpoint was taken against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointWorker {
    /// Capability facts, snapshotted when the worker was selected.
    pub descriptor: AgentDescriptor,
    /// Opaque continuation reference, when the worker offered one.
    ///
    /// Adapter data, never the record: a handoff to a different worker ignores
    /// it entirely (`PROJECT_FORMAT.md` §4).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_state_ref: Option<String>,
}

/// A point a task can be understood from and continued at.
///
/// Two fields deviate from the shape written in `CORE_CONTRACTS.md` §11, and
/// the document has been updated to match: `worker`, `contextManifestId`, and
/// `configurationManifestHash` are optional here. A task interrupted while
/// `preparing` has not selected a worker or prepared a context, and a
/// projectless task never has a configuration manifest
/// (`CORE_CONTRACTS.md` §5.2). Requiring them would mean either inventing
/// values or refusing to checkpoint exactly the tasks a checkpoint is most
/// needed for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskCheckpoint {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier.
    pub checkpoint_id: CheckpointId,
    /// The task this describes.
    pub task_id: TaskId,
    /// The durable sequence this checkpoint was taken at.
    ///
    /// Everything at or below this sequence is accounted for by the fields
    /// here; anything above it happened afterwards.
    pub event_sequence: u64,
    /// When it was written.
    pub created_at: Timestamp,
    /// The task's state at that sequence.
    pub task_state: TaskState,
    /// The worker, when one had been selected.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub worker: Option<CheckpointWorker>,
    /// Steps already done, which resume must not repeat.
    ///
    /// The watermark, not a log: a step named here has a durable completion
    /// event behind it.
    pub completed_step_ids: Vec<String>,
    /// The action that had started and not finished, when there was one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pending_action: Option<PendingAction>,
    /// The context bundle prepared for this task, when one was.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context_manifest_id: Option<String>,
    /// The effective configuration in force, for a project task.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub configuration_manifest_hash: Option<String>,
    /// Whether and how this can be picked up again.
    pub resume: ResumeEligibility,
}

impl TaskCheckpoint {
    /// Build a checkpoint with the current schema version.
    #[must_use]
    pub fn new(
        checkpoint_id: CheckpointId,
        task_id: TaskId,
        event_sequence: u64,
        created_at: Timestamp,
        task_state: TaskState,
        resume: ResumeEligibility,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            checkpoint_id,
            task_id,
            event_sequence,
            created_at,
            task_state,
            worker: None,
            completed_step_ids: Vec::new(),
            pending_action: None,
            context_manifest_id: None,
            configuration_manifest_hash: None,
            resume,
        }
    }

    /// Whether the core may continue without asking a person first.
    ///
    /// A checkpoint with no pending action is safe by construction: nothing
    /// was in flight. One with a pending action is safe only if repeating that
    /// action repeats no effect.
    #[must_use]
    pub fn can_resume_without_asking(&self) -> bool {
        self.resume.eligible
            && self
                .pending_action
                .as_ref()
                .is_none_or(|action| action.replay_safety.is_automatically_replayable())
    }

    /// Whether this step has already been done.
    #[must_use]
    pub fn has_completed(&self, step_id: &str) -> bool {
        self.completed_step_ids.iter().any(|id| id == step_id)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn checkpoint(state: TaskState) -> TaskCheckpoint {
        TaskCheckpoint::new(
            CheckpointId::new("checkpoint-1"),
            TaskId::new("task-1"),
            7,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
            state,
            ResumeEligibility::same_worker("checkpoint.clean"),
        )
    }

    #[test]
    fn a_checkpoint_with_nothing_in_flight_resumes_without_asking() {
        let checkpoint = checkpoint(TaskState::Interrupted);
        assert!(checkpoint.pending_action.is_none());
        assert!(checkpoint.can_resume_without_asking());
    }

    #[test]
    fn an_unsafe_or_unknown_pending_action_is_never_replayed_automatically() {
        // `CORE_CONTRACTS.md` §11: after an unclean stop a pending unsafe or
        // unknown action is never replayed; the user reconciles it.
        for safety in [ReplaySafety::Unsafe, ReplaySafety::Unknown] {
            let mut checkpoint = checkpoint(TaskState::Interrupted);
            checkpoint.pending_action = Some(PendingAction {
                kind: PendingActionKind::Tool,
                reference_id: "invocation-1".into(),
                replay_safety: safety,
                display_summary: "wrote a file".into(),
            });
            assert!(
                !checkpoint.can_resume_without_asking(),
                "{safety:?} must not auto-replay"
            );
            assert!(!safety.is_automatically_replayable());
        }
    }

    #[test]
    fn a_safe_pending_action_may_be_repeated() {
        let mut checkpoint = checkpoint(TaskState::Interrupted);
        checkpoint.pending_action = Some(PendingAction {
            kind: PendingActionKind::Tool,
            reference_id: "invocation-1".into(),
            replay_safety: ReplaySafety::Safe,
            display_summary: "read a file".into(),
        });
        assert!(checkpoint.can_resume_without_asking());
    }

    #[test]
    fn an_ineligible_checkpoint_never_resumes_however_safe_the_action() {
        let mut checkpoint = checkpoint(TaskState::Completed);
        checkpoint.resume = ResumeEligibility::refused("task.terminal");
        checkpoint.pending_action = Some(PendingAction {
            kind: PendingActionKind::Model,
            reference_id: "request-1".into(),
            replay_safety: ReplaySafety::Safe,
            display_summary: "asked the model".into(),
        });
        assert!(!checkpoint.can_resume_without_asking());
    }

    #[test]
    fn the_completed_watermark_answers_whether_a_step_ran() {
        let mut checkpoint = checkpoint(TaskState::Interrupted);
        checkpoint.completed_step_ids = vec!["invocation-1".into(), "invocation-2".into()];
        assert!(checkpoint.has_completed("invocation-1"));
        assert!(!checkpoint.has_completed("invocation-3"));
    }

    #[test]
    fn checkpoints_serialize_as_camel_case_and_omit_absent_fields() {
        let json = serde_json::to_value(checkpoint(TaskState::Interrupted)).unwrap();
        assert_eq!(json["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(json["checkpointId"], "checkpoint-1");
        assert_eq!(json["eventSequence"], 7);
        assert_eq!(json["taskState"], "interrupted");
        assert_eq!(json["resume"]["mode"], "sameWorker");

        let fields = json.as_object().unwrap();
        for absent in [
            "worker",
            "pendingAction",
            "contextManifestId",
            "configurationManifestHash",
        ] {
            assert!(!fields.contains_key(absent), "{absent} should be omitted");
        }
    }

    #[test]
    fn a_refusal_carries_its_reason_and_no_mode() {
        let refused = ResumeEligibility::refused("checkpoint.unsafePendingAction");
        assert!(!refused.eligible);
        assert!(refused.mode.is_none());
        let json = serde_json::to_value(&refused).unwrap();
        assert!(!json.as_object().unwrap().contains_key("mode"));
        assert_eq!(json["reasonCode"], "checkpoint.unsafePendingAction");
    }
}
