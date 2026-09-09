//! Task state and its transition rules.
//!
//! `CORE_CONTRACTS.md` §4 is normative. Two properties matter more than the
//! particular states:
//!
//! 1. **A task has exactly one terminal state.** Late events cannot move a
//!    task that has already ended.
//! 2. **Generating text and running a tool are not states.** They are events
//!    emitted while the task is [`TaskState::Running`], which keeps concurrent
//!    streams from churning the state machine.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a task is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    /// Accepted, not yet started.
    Queued,
    /// Resolving project and effective configuration.
    Preparing,
    /// Choosing a model and a compute target.
    Routing,
    /// Starting the selected worker.
    StartingWorker,
    /// The worker is doing the work. Model and tool activity are events here.
    Running,
    /// Blocked on a user decision.
    AwaitingApproval,
    /// Writing a durable checkpoint.
    Checkpointing,
    /// Cancellation requested; cleaning up.
    Cancelling,
    /// Stopped uncleanly. Not automatically resumable — a checkpoint decides.
    Interrupted,
    /// Finished successfully. Terminal.
    Completed,
    /// Finished unsuccessfully. Terminal.
    Failed,
    /// Cancellation completed. Terminal.
    Cancelled,
}

impl TaskState {
    /// Whether this state can never be left.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Whether cancellation may still be requested from this state.
    #[must_use]
    pub fn is_cancellable(self) -> bool {
        self.allows(Self::Cancelling)
    }

    /// The states reachable from this one, per `CORE_CONTRACTS.md` §4.2.
    #[must_use]
    pub fn allowed_next(self) -> &'static [TaskState] {
        use TaskState::{
            AwaitingApproval, Cancelled, Cancelling, Checkpointing, Completed, Failed, Interrupted,
            Preparing, Queued, Routing, Running, StartingWorker,
        };
        match self {
            Queued => &[Preparing, Cancelling, Failed, Interrupted],
            Preparing => &[Routing, Cancelling, Failed, Interrupted],
            Routing => &[StartingWorker, Cancelling, Failed, Interrupted],
            StartingWorker => &[Running, Cancelling, Failed, Interrupted],
            Running => &[
                AwaitingApproval,
                Checkpointing,
                Cancelling,
                Completed,
                Failed,
                Interrupted,
            ],
            AwaitingApproval => &[Running, Checkpointing, Cancelling, Failed, Interrupted],
            Checkpointing => &[Running, Cancelling, Completed, Failed, Interrupted],
            Cancelling => &[Cancelled, Failed, Interrupted],
            // Only after an explicit resume or cancel decision by the user.
            Interrupted => &[Preparing, Cancelled, Failed],
            Completed | Failed | Cancelled => &[],
        }
    }

    /// Whether moving directly to `next` is legal.
    #[must_use]
    pub fn allows(self, next: TaskState) -> bool {
        self.allowed_next().contains(&next)
    }

    /// Validate a transition.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTransition`] when the move is not permitted, including
    /// every move out of a terminal state and every self-transition.
    pub fn transition_to(self, next: TaskState) -> Result<TaskState, InvalidTransition> {
        if self.allows(next) {
            Ok(next)
        } else {
            Err(InvalidTransition {
                from: self,
                to: next,
            })
        }
    }
}

/// The current view of a task.
///
/// A projection of the event stream, not a second source of truth: it exists
/// so reading a task's state does not require replaying every event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier.
    pub task_id: super::TaskId,
    /// Current state.
    pub state: TaskState,
    /// Highest durable sequence written.
    pub last_sequence: u64,
}

/// A refused task-state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid task transition from {from:?} to {to:?}")]
pub struct InvalidTransition {
    /// The state the task was in.
    pub from: TaskState,
    /// The state that was requested.
    pub to: TaskState,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::TaskState::{
        AwaitingApproval, Cancelled, Cancelling, Checkpointing, Completed, Failed, Interrupted,
        Preparing, Queued, Routing, Running, StartingWorker,
    };
    use super::*;

    /// Every state, so exhaustiveness checks below cannot silently miss one.
    const ALL: [TaskState; 12] = [
        Queued,
        Preparing,
        Routing,
        StartingWorker,
        Running,
        AwaitingApproval,
        Checkpointing,
        Cancelling,
        Interrupted,
        Completed,
        Failed,
        Cancelled,
    ];

    /// The transition table from `CORE_CONTRACTS.md` §4.2, transcribed
    /// independently of the implementation so a typo in one shows up as a
    /// disagreement rather than agreeing with itself.
    const TABLE: &[(TaskState, &[TaskState])] = &[
        (Queued, &[Preparing, Cancelling, Failed, Interrupted]),
        (Preparing, &[Routing, Cancelling, Failed, Interrupted]),
        (Routing, &[StartingWorker, Cancelling, Failed, Interrupted]),
        (StartingWorker, &[Running, Cancelling, Failed, Interrupted]),
        (
            Running,
            &[
                AwaitingApproval,
                Checkpointing,
                Cancelling,
                Completed,
                Failed,
                Interrupted,
            ],
        ),
        (
            AwaitingApproval,
            &[Running, Checkpointing, Cancelling, Failed, Interrupted],
        ),
        (
            Checkpointing,
            &[Running, Cancelling, Completed, Failed, Interrupted],
        ),
        (Cancelling, &[Cancelled, Failed, Interrupted]),
        (Interrupted, &[Preparing, Cancelled, Failed]),
        (Completed, &[]),
        (Failed, &[]),
        (Cancelled, &[]),
    ];

    #[test]
    fn every_allowed_transition_in_the_contract_is_permitted() {
        for (from, allowed) in TABLE {
            for to in *allowed {
                assert_eq!(
                    from.transition_to(*to).unwrap(),
                    *to,
                    "{from:?} -> {to:?} is in the contract table"
                );
            }
        }
    }

    #[test]
    fn every_transition_absent_from_the_contract_is_refused() {
        for (from, allowed) in TABLE {
            for to in ALL {
                if allowed.contains(&to) {
                    continue;
                }
                let error = from.transition_to(to).unwrap_err();
                assert_eq!(error, InvalidTransition { from: *from, to });
            }
        }
    }

    #[test]
    fn the_table_covers_every_state_exactly_once() {
        assert_eq!(TABLE.len(), ALL.len());
        for state in ALL {
            assert_eq!(
                TABLE.iter().filter(|(from, _)| *from == state).count(),
                1,
                "{state:?} must appear once in the table"
            );
        }
    }

    #[test]
    fn a_terminal_task_can_never_move_again() {
        for state in ALL.into_iter().filter(|s| s.is_terminal()) {
            assert!(state.allowed_next().is_empty());
            for to in ALL {
                assert!(
                    state.transition_to(to).is_err(),
                    "late event moved terminal {state:?} to {to:?}"
                );
            }
        }
    }

    #[test]
    fn no_state_transitions_to_itself() {
        // A repeated state would append an event that says nothing happened.
        for state in ALL {
            assert!(state.transition_to(state).is_err(), "{state:?} -> itself");
        }
    }

    #[test]
    fn exactly_three_states_are_terminal() {
        let terminal: Vec<_> = ALL.into_iter().filter(|s| s.is_terminal()).collect();
        assert_eq!(terminal, vec![Completed, Failed, Cancelled]);
    }

    #[test]
    fn every_active_state_can_be_cancelled() {
        // "Every long-running operation is cancellable or clearly marked
        // non-cancellable" (PHASE_1_SPEC §9). Cancelling and Interrupted are
        // not themselves cancellable: one is already stopping, the other has
        // stopped.
        for state in ALL {
            let expected = !state.is_terminal() && !matches!(state, Cancelling | Interrupted);
            assert_eq!(state.is_cancellable(), expected, "{state:?}");
        }
    }

    #[test]
    fn interrupted_is_not_terminal_but_needs_an_explicit_decision() {
        assert!(!Interrupted.is_terminal());
        assert!(Interrupted.allows(Preparing), "resume");
        assert!(Interrupted.allows(Cancelled), "discard");
        // It cannot slip straight back into running: recovery goes through
        // Preparing so context and configuration are rebuilt first.
        assert!(!Interrupted.allows(Running));
    }

    #[test]
    fn task_states_serialize_as_camel_case() {
        assert_eq!(
            serde_json::to_string(&StartingWorker).unwrap(),
            "\"startingWorker\""
        );
        assert_eq!(
            serde_json::to_string(&AwaitingApproval).unwrap(),
            "\"awaitingApproval\""
        );
    }
}
