//! Checkpoints and replay guards derived only from canonical task events.

use crate::contracts::{
    AgentDescriptor, CheckpointId, CheckpointSupport, CheckpointWorker, EventEnvelope,
    EventPayload, PendingAction, PendingActionKind, ReplaySafety, ResumeEligibility,
    TaskCheckpoint, TaskId, TaskState, Timestamp,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Stable checkpoint refusal and eligibility reasons.
pub mod reason {
    pub const CLEAN: &str = "checkpoint.clean";
    pub const SAFE_PENDING_ACTION: &str = "checkpoint.safePendingAction";
    pub const UNSAFE_PENDING_ACTION: &str = "checkpoint.unsafePendingAction";
    pub const UNKNOWN_PENDING_ACTION: &str = "checkpoint.unknownPendingAction";
    pub const TASK_TERMINAL: &str = "checkpoint.taskTerminal";
    pub const WORKER_NOT_RESUMABLE: &str = "checkpoint.workerNotResumable";
    pub const NO_WORKER_SELECTED: &str = "checkpoint.noWorkerSelected";
    pub const MULTIPLE_PENDING: &str = "checkpoint.multiplePendingActions";
    pub const LEGACY_STEPS: &str = "checkpoint.stepsWithoutStableIdentity";
    pub const UNSUPPORTED_HISTORY: &str = "checkpoint.unsupportedHistory";
}

/// A completed step's identity and safe replacement result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedStep {
    pub request_hash: String,
    pub summary: String,
}

/// Broker-enforced resume state. An ordinal alone is never a step identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumePlan {
    pub(crate) completed: HashMap<String, CompletedStep>,
    pub(crate) requests: HashMap<String, String>,
    pub(crate) uncertain: HashSet<String>,
}

impl ResumePlan {
    /// Recover immutable request fingerprints and the completed ledger.
    #[must_use]
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        derive_state(events).resume_plan
    }

    /// Recorded outcome for an already completed step.
    #[must_use]
    pub fn already_done(&self, step_id: &str) -> Option<&CompletedStep> {
        self.completed.get(step_id)
    }
}

/// Deterministic fingerprint that excludes argument values from storage.
/// serde_json's default map representation sorts keys.
pub(crate) fn request_hash(tool_id: &str, arguments: &serde_json::Value) -> String {
    let bytes = format!("{tool_id}\n{arguments}");
    Sha256::digest(bytes.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Canonical facts used to construct a checkpoint and resume guard.
#[derive(Debug, Clone, Default)]
pub struct DerivedState {
    pub task_state: Option<TaskState>,
    pub worker: Option<AgentDescriptor>,
    pub completed_step_ids: Vec<String>,
    pub pending_action: Option<PendingAction>,
    pub context_manifest_id: Option<String>,
    pub last_sequence: u64,
    pub resume_plan: ResumePlan,
    pub(crate) multiple_pending: bool,
    pub(crate) legacy_steps: bool,
    pub(crate) unsupported_history: bool,
}

/// Replay observations without consulting any worker session or private state.
#[must_use]
pub fn derive_state(events: &[EventEnvelope]) -> DerivedState {
    let mut state = DerivedState::default();
    let mut pending = BTreeMap::new();
    let mut steps: HashMap<String, String> = HashMap::new();
    for event in events {
        state.last_sequence = event.sequence;
        match &event.payload {
            EventPayload::TaskStateChanged { current, .. } => state.task_state = Some(*current),
            EventPayload::WorkerSelected { descriptor } => state.worker = Some(descriptor.clone()),
            // The latest manifest wins: a resumed run prepares context again,
            // and a checkpoint that named the first attempt's context would
            // describe a release that is no longer the one in force.
            EventPayload::ContextPrepared { manifest } => {
                state.context_manifest_id = Some(manifest.context_manifest_id.clone());
            }
            EventPayload::ToolRequested {
                invocation_id,
                tool_id,
                step_id,
                request_hash,
                ..
            } => {
                if let (Some(step), Some(hash)) = (step_id, request_hash) {
                    steps.insert(invocation_id.to_string(), step.clone());
                    state
                        .resume_plan
                        .requests
                        .insert(step.clone(), hash.clone());
                } else {
                    state.legacy_steps = true;
                }
                pending.insert(
                    format!("tool:{invocation_id}"),
                    PendingAction {
                        kind: PendingActionKind::Approval,
                        reference_id: invocation_id.to_string(),
                        replay_safety: ReplaySafety::Safe,
                        display_summary: format!(
                            "{tool_id} was requested; execution had not started"
                        ),
                    },
                );
            }
            EventPayload::ToolStarted {
                invocation_id,
                tool_id,
                replay_safety,
            } => {
                if let Some(step) = steps.get(invocation_id.as_str()) {
                    if *replay_safety != ReplaySafety::Safe {
                        state.resume_plan.uncertain.insert(step.clone());
                    }
                } else {
                    state.legacy_steps = true;
                }
                pending.insert(
                    format!("tool:{invocation_id}"),
                    PendingAction {
                        kind: PendingActionKind::Tool,
                        reference_id: invocation_id.to_string(),
                        replay_safety: *replay_safety,
                        display_summary: format!("{tool_id} started without a recorded result"),
                    },
                );
            }
            EventPayload::ToolCompleted {
                invocation_id,
                display_summary,
                ..
            } => {
                if let Some(step) = steps.get(invocation_id.as_str()) {
                    state.resume_plan.uncertain.remove(step);
                    if !state.completed_step_ids.contains(step) {
                        state.completed_step_ids.push(step.clone());
                    }
                    if let Some(hash) = state.resume_plan.requests.get(step) {
                        state.resume_plan.completed.insert(
                            step.clone(),
                            CompletedStep {
                                request_hash: hash.clone(),
                                summary: display_summary.clone(),
                            },
                        );
                    }
                }
                pending.remove(&format!("tool:{invocation_id}"));
            }
            EventPayload::ToolFailed { invocation_id, .. } => {
                // Failure does not prove a side effect did not happen.
                if pending
                    .get(&format!("tool:{invocation_id}"))
                    .is_some_and(|action| action.replay_safety == ReplaySafety::Safe)
                {
                    pending.remove(&format!("tool:{invocation_id}"));
                }
            }
            EventPayload::ModelStarted { stream_id, .. } => {
                // Model calls are serialized by the host. A new model start
                // supersedes a safely interrupted prior attempt, which must
                // not accumulate phantom concurrent actions after retries.
                pending.retain(|_, action: &mut PendingAction| {
                    action.kind != PendingActionKind::Model
                });
                pending.insert(
                    format!("model:{stream_id}"),
                    PendingAction {
                        kind: PendingActionKind::Model,
                        reference_id: stream_id.clone(),
                        replay_safety: ReplaySafety::Safe,
                        display_summary: "A model request had not returned a complete answer"
                            .into(),
                    },
                );
            }
            EventPayload::MessageCommitted { message }
                if message.status == crate::contracts::MessageStatus::Committed
                    && message.role == crate::contracts::MessageRole::Assistant
                    && message
                        .parts
                        .iter()
                        .any(|part| matches!(part, crate::contracts::MessagePart::Text { .. })) =>
            {
                pending.retain(|_, action| action.kind != PendingActionKind::Model);
            }
            EventPayload::Unsupported => state.unsupported_history = true,
            _ => {}
        }
    }
    state.multiple_pending = pending.len() > 1;
    state.pending_action = pending.into_values().next();
    state
}

/// Construct an inspectable snapshot. Resume is always explicit; histories
/// with unsafe/unknown effects remain inspectable but cannot be replayed.
#[must_use]
pub fn derive_checkpoint(
    checkpoint_id: CheckpointId,
    task_id: TaskId,
    events: &[EventEnvelope],
    configuration_manifest_hash: Option<String>,
    created_at: Timestamp,
) -> TaskCheckpoint {
    let state = derive_state(events);
    let task_state = state.task_state.unwrap_or(TaskState::Queued);
    let resume = if task_state.is_terminal() {
        ResumeEligibility::refused(reason::TASK_TERMINAL)
    } else if state.unsupported_history {
        ResumeEligibility::refused(reason::UNSUPPORTED_HISTORY)
    } else if state.multiple_pending {
        ResumeEligibility::refused(reason::MULTIPLE_PENDING)
    } else if state.legacy_steps {
        ResumeEligibility::refused(reason::LEGACY_STEPS)
    } else if let Some(action) = &state.pending_action
        && action.replay_safety != ReplaySafety::Safe
    {
        ResumeEligibility::refused(if action.replay_safety == ReplaySafety::Unsafe {
            reason::UNSAFE_PENDING_ACTION
        } else {
            reason::UNKNOWN_PENDING_ACTION
        })
    } else {
        let code = if state.pending_action.is_some() {
            reason::SAFE_PENDING_ACTION
        } else {
            reason::CLEAN
        };
        match state
            .worker
            .as_ref()
            .map(|worker| worker.checkpoint_support)
        {
            Some(CheckpointSupport::None) => {
                ResumeEligibility::refused(reason::WORKER_NOT_RESUMABLE)
            }
            Some(CheckpointSupport::SameWorker) => ResumeEligibility::same_worker(code),
            Some(CheckpointSupport::Portable) => ResumeEligibility::canonical_handoff(code),
            None => ResumeEligibility::canonical_handoff(reason::NO_WORKER_SELECTED),
        }
    };
    let mut checkpoint = TaskCheckpoint::new(
        checkpoint_id,
        task_id,
        state.last_sequence,
        created_at,
        task_state,
        resume,
    );
    checkpoint.worker = state.worker.map(|descriptor| CheckpointWorker {
        descriptor,
        session_state_ref: None,
    });
    checkpoint.completed_step_ids = state.completed_step_ids;
    checkpoint.pending_action = state.pending_action;
    checkpoint.context_manifest_id = state.context_manifest_id;
    checkpoint.configuration_manifest_hash = configuration_manifest_hash;
    checkpoint
}
