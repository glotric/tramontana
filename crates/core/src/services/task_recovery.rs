//! Explicit recovery commands shared by every shell.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::checkpoint::{ResumePlan, derive_checkpoint, derive_state};
use super::journal::{JournalPorts, TaskJournal};
use super::projection::TaskProjectionWriter;
use super::runtime::TaskDriver;
use super::task_service::{LiveTask, TaskService};
use crate::contracts::{
    CancellationOrigin, CapabilityScope, CheckpointId, CorrelationId, ErrorCode, ErrorEnvelope,
    EventDraft, EventEnvelope, EventPayload, HandoffBundle, ResumeMode, TaskCheckpoint,
    TaskEventKind, TaskId, TaskRecord, TaskState,
};
use crate::services::handoff::build_handoff;

/// A validated interrupted task, ready for someone to continue.
struct Continuation {
    record: TaskRecord,
    /// The checkpoint the user inspected.
    checkpoint: TaskCheckpoint,
    /// The same point, recomputed from canonical events.
    derived: TaskCheckpoint,
    /// The task's durable history.
    events: Vec<EventEnvelope>,
    /// The limits the continued run inherits.
    scope: CapabilityScope,
}

impl TaskService {
    pub(super) fn require_execution(&self) -> Result<(), ErrorEnvelope> {
        if self.execution_allowed {
            Ok(())
        } else {
            Err(self.error(
                ErrorCode::Conflict,
                "Another runtime owns this profile; close it before changing task execution",
            ))
        }
    }

    /// Startup recovery never launches a worker or repeats an action.
    pub(super) async fn recover_interrupted(&self) -> Result<usize, ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        if !self.execution_allowed
            || self
                .lock_live()?
                .values()
                .any(|task| !*task.finished.borrow())
        {
            return Ok(0);
        }
        let mut changed = self
            .repositories
            .tasks
            .rebuild_from_events()
            .await
            .map_err(|error| self.repository_error(&error))?;
        for record in self.list_tasks(None).await? {
            if record.state.is_terminal() {
                continue;
            }
            if record.state != TaskState::Interrupted {
                self.append_state(&record, TaskState::Interrupted, "recovery.uncleanStop")
                    .await?;
                changed += 1;
            }
            self.save_checkpoint(&record.task_id).await?;
        }
        Ok(changed)
    }

    /// Pause a live worker cooperatively, then persist an inspectable checkpoint.
    /// A timeout does not claim that the worker stopped or permit a second run.
    pub async fn create_checkpoint(
        &self,
        task_id: &TaskId,
    ) -> Result<TaskCheckpoint, ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_execution()?;
        self.get_task_record(task_id).await?;
        let finished = {
            let live = self.lock_live()?;
            live.get(task_id)
                .filter(|task| !*task.finished.borrow())
                .map(|task| {
                    task.interrupt.cancel();
                    task.cancel.cancel();
                    task.finished.clone()
                })
        };
        if let Some(mut finished) = finished {
            tokio::time::timeout(Duration::from_secs(10), finished.wait_for(|done| *done))
                .await
                .map_err(|_| {
                    self.error(
                        ErrorCode::Conflict,
                        "Worker has not stopped; checkpoint is not yet available",
                    )
                })?
                .map_err(|_| {
                    self.error(
                        ErrorCode::InterruptedUnsafe,
                        "Worker exited unexpectedly; reopen the profile to recover",
                    )
                })?;
        }
        let record = self.canonical_record(task_id).await?;
        if !record.state.is_terminal() && record.state != TaskState::Interrupted {
            self.append_state(&record, TaskState::Interrupted, "user.checkpoint")
                .await?;
        }
        self.save_checkpoint(task_id).await
    }

    /// Latest stored checkpoint; inspection does not mutate or start work.
    pub async fn latest_checkpoint(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<TaskCheckpoint>, ErrorEnvelope> {
        self.get_task_record(task_id).await?;
        self.repositories
            .checkpoints
            .latest(task_id)
            .await
            .map_err(|error| self.repository_error(&error))
    }

    /// Everything both continuation paths need, validated against canonical
    /// events rather than trusted from a stored row.
    ///
    /// Resuming and handing over differ only in *who* continues; every reason
    /// a continuation can be refused — the task is running, is not
    /// interrupted, the checkpoint is stale or was never inspected, the
    /// pending action cannot be repeated, the configuration moved — is the
    /// same for both. Sharing the check is what keeps the newer path from
    /// quietly having weaker rules than the older one.
    async fn validated_continuation(
        &self,
        task_id: &TaskId,
        checkpoint_id: &CheckpointId,
    ) -> Result<Continuation, ErrorEnvelope> {
        self.require_execution()?;
        if self
            .lock_live()?
            .get(task_id)
            .is_some_and(|task| !*task.finished.borrow())
        {
            return Err(self.error(ErrorCode::Conflict, "Task is already running"));
        }
        let record = self.canonical_record(task_id).await?;
        if record.state != TaskState::Interrupted {
            return Err(self.error(
                ErrorCode::Conflict,
                "Only interrupted tasks can be continued",
            ));
        }
        let checkpoint = self
            .latest_checkpoint(task_id)
            .await?
            .filter(|checkpoint| &checkpoint.checkpoint_id == checkpoint_id)
            .ok_or_else(|| {
                self.error(
                    ErrorCode::Conflict,
                    "Inspect the latest checkpoint before continuing",
                )
            })?;
        let events = self.replay_task_events(task_id, 0).await?;
        if events.iter().any(|event| event.sequence > checkpoint.event_sequence && !matches!(
            &event.payload, EventPayload::CheckpointSaved { checkpoint_id: saved, .. } if saved == checkpoint_id
        )) {
            return Err(self.error(ErrorCode::Conflict, "Checkpoint is stale; create and inspect a new checkpoint"));
        }
        let derived = derive_checkpoint(
            checkpoint.checkpoint_id.clone(),
            task_id.clone(),
            &events,
            record.configuration_manifest_hash.clone(),
            self.clock.now(),
        );
        if !derived.resume.eligible {
            return Err(self.error(
                ErrorCode::InterruptedUnsafe,
                derived
                    .resume
                    .reason_code
                    .clone()
                    .unwrap_or_else(|| "Checkpoint is not resumable".into()),
            ));
        }
        let scope = if let Some(project_id) = &record.project_id {
            let configuration = self.get_effective_configuration(project_id).await?;
            if configuration.as_ref().map(|config| &config.manifest_hash)
                != record.configuration_manifest_hash.as_ref()
            {
                return Err(self.error(
                    ErrorCode::Conflict,
                    "Configuration changed; this checkpoint cannot be continued unchanged",
                ));
            }
            CapabilityScope::project(
                project_id.clone(),
                self.get_project(project_id).await?.roots,
            )
        } else {
            CapabilityScope {
                project_id: None,
                roots: Vec::new(),
                releases_data: false,
            }
        };

        Ok(Continuation {
            record,
            checkpoint,
            derived,
            events,
            scope,
        })
    }

    /// Resume the exact inspected checkpoint with the same worker.
    /// Eligibility is recomputed from canonical events, never trusted from a row.
    pub async fn resume_task(
        &self,
        task_id: &TaskId,
        checkpoint_id: &CheckpointId,
    ) -> Result<(), ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        let continuation = self.validated_continuation(task_id, checkpoint_id).await?;

        if continuation
            .derived
            .worker
            .as_ref()
            .is_some_and(|worker| worker.descriptor != self.providers.worker.describe())
        {
            return Err(self.error(
                ErrorCode::Conflict,
                "Same-worker resume requires the recorded worker and capabilities",
            ));
        }
        self.launch(continuation, None).await
    }

    /// Hand an interrupted task to a *different* worker, from canonical state.
    ///
    /// The receiving worker is given a [`HandoffBundle`] and nothing else: no
    /// checkpoint belonging to the previous worker, and no opaque continuation
    /// data. That is the claim `PROJECT_FORMAT.md` §4 makes and validation
    /// gate V2 tests — if the bundle is not enough, the handed-over task fails
    /// here rather than the claim going unchallenged until M10 depends on it.
    ///
    /// Returns the bundle that was handed over, so a caller can show a person
    /// exactly what the second worker was given.
    ///
    /// # Errors
    ///
    /// Every refusal [`TaskService::resume_task`] can produce, plus
    /// [`ErrorCode::Conflict`] when the checkpoint is not portable or when the
    /// receiving worker is the one that was already running.
    pub async fn hand_off_task(
        &self,
        task_id: &TaskId,
        checkpoint_id: &CheckpointId,
        receiving: Arc<dyn crate::ports::AgentProvider>,
    ) -> Result<HandoffBundle, ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        let continuation = self.validated_continuation(task_id, checkpoint_id).await?;

        if continuation.derived.resume.mode != Some(ResumeMode::CanonicalHandoff) {
            // A worker that needs its own session state cannot be replaced by
            // one reading the canonical record, and pretending otherwise would
            // hand the second worker a record it cannot act on.
            return Err(self.error(
                ErrorCode::Conflict,
                "This checkpoint is not portable; only the recorded worker can continue it",
            ));
        }

        let receiving_descriptor = receiving.describe();
        if continuation
            .derived
            .worker
            .as_ref()
            .is_some_and(|worker| worker.descriptor.agent_id == receiving_descriptor.agent_id)
        {
            return Err(self.error(
                ErrorCode::Conflict,
                "That worker already had this task; resume it instead of handing it over",
            ));
        }

        let handoff = build_handoff(
            &continuation.record,
            &continuation.events,
            &continuation.derived,
            self.clock.now(),
        );
        self.launch(continuation, Some((receiving, handoff.clone())))
            .await?;
        Ok(handoff)
    }

    /// Move an interrupted task back into `preparing` and run it.
    ///
    /// `taking_over` is absent for a same-worker resume and present for a
    /// handoff. Everything else — the live registry entry, the journal, the
    /// configuration read back by hash, the replay guard built from the
    /// events — is identical, because the difference between the two paths is
    /// only who continues.
    async fn launch(
        &self,
        continuation: Continuation,
        taking_over: Option<(Arc<dyn crate::ports::AgentProvider>, HandoffBundle)>,
    ) -> Result<(), ErrorEnvelope> {
        let Continuation {
            record,
            checkpoint,
            events,
            scope,
            ..
        } = continuation;
        let task_id = record.task_id.clone();

        let cancel = CancellationToken::new();
        let interrupt = CancellationToken::new();
        let (finished, receiver) = watch::channel(false);
        let (updates, _) = broadcast::channel(256);

        let reason = if taking_over.is_some() {
            "user.handedOver"
        } else {
            "user.resumedCheckpoint"
        };
        self.append_state(&record, TaskState::Preparing, reason)
            .await?;
        let record = self.canonical_record(&task_id).await?;
        let journal = self.journal_for(&record, updates.clone());
        self.lock_live()?.insert(
            task_id.clone(),
            LiveTask {
                cancel: cancel.clone(),
                interrupt: interrupt.clone(),
                updates,
                finished: receiver,
            },
        );

        // The manifest the task started under, not whatever the project holds
        // now: the validation above already refused if the configuration
        // moved, so this is the same one, read back by hash rather than
        // assumed.
        let configuration = match (&record.project_id, &record.configuration_manifest_hash) {
            (Some(project_id), Some(hash)) => self
                .repositories
                .configuration
                .get(project_id, hash)
                .await
                .ok(),
            _ => None,
        };

        let plan = ResumePlan::from_events(&events);
        let driver = TaskDriver::new(journal, self.providers.clone(), scope, self.clock.clone())
            .with_routing_mode(self.routing_mode())
            .with_configuration(configuration);
        let driver = match taking_over {
            Some((receiving, handoff)) => driver.handing_over(receiving, handoff, plan, interrupt),
            None => driver.continuing(Some(checkpoint), plan, interrupt),
        };

        let guard = self.execution_guard.clone();
        tokio::spawn(async move {
            let _guard = guard;
            driver.run(record.goal_summary, cancel).await;
            let _ = finished.send(true);
        });
        Ok(())
    }

    /// Discard continuation without deleting the record or undoing any effect.
    pub async fn discard_checkpoint(&self, task_id: &TaskId) -> Result<(), ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_execution()?;
        if self.canonical_record(task_id).await?.state != TaskState::Interrupted {
            return Err(self.error(
                ErrorCode::Conflict,
                "Only interrupted work can be discarded",
            ));
        }
        self.discard_interrupted(task_id).await
    }

    pub(super) async fn discard_interrupted(&self, task_id: &TaskId) -> Result<(), ErrorEnvelope> {
        if self
            .lock_live()?
            .get(task_id)
            .is_some_and(|task| !*task.finished.borrow())
        {
            return Err(self.error(ErrorCode::Conflict, "Worker has not stopped"));
        }
        let record = self.canonical_record(task_id).await?;
        self.append_state(&record, TaskState::Cancelled, "user.discardedCheckpoint")
            .await?;
        let record = self.canonical_record(task_id).await?;
        self.journal_for(&record, broadcast::channel(1).0)
            .append(EventDraft::user(
                TaskEventKind::TaskCancelled,
                EventPayload::TaskCancelled {
                    origin: CancellationOrigin::User,
                },
            ))
            .await?;
        Ok(())
    }

    async fn canonical_record(&self, task_id: &TaskId) -> Result<TaskRecord, ErrorEnvelope> {
        let mut record = self.get_task_record(task_id).await?;
        let events = self.replay_task_events(task_id, 0).await?;
        let state = derive_state(&events);
        record.state = state.task_state.unwrap_or(TaskState::Queued);
        record.last_sequence = state.last_sequence;
        Ok(record)
    }

    async fn append_state(
        &self,
        record: &TaskRecord,
        next: TaskState,
        reason: &str,
    ) -> Result<(), ErrorEnvelope> {
        record
            .state
            .transition_to(next)
            .map_err(|_| self.error(ErrorCode::Conflict, "Invalid recovery transition"))?;
        self.journal_for(record, broadcast::channel(1).0)
            .append(EventDraft::user(
                TaskEventKind::TaskStateChanged,
                EventPayload::TaskStateChanged {
                    previous: record.state,
                    current: next,
                    reason_code: reason.into(),
                },
            ))
            .await?;
        Ok(())
    }

    async fn save_checkpoint(&self, task_id: &TaskId) -> Result<TaskCheckpoint, ErrorEnvelope> {
        let record = self.canonical_record(task_id).await?;
        let events = self.replay_task_events(task_id, 0).await?;
        let latest = self.latest_checkpoint(task_id).await?;
        // Reuse an unchanged snapshot, including recovery from a crash between
        // saving its row and appending its marker. Never insert a duplicate sequence.
        let checkpoint = if let Some(latest) = latest.filter(|checkpoint| events.iter().all(|event|
            event.sequence <= checkpoint.event_sequence || matches!(&event.payload,
                EventPayload::CheckpointSaved { checkpoint_id, .. } if checkpoint_id == &checkpoint.checkpoint_id))) {
            latest
        } else {
            let checkpoint = derive_checkpoint(CheckpointId::new(self.ids.next("checkpoint")),
                task_id.clone(), &events, record.configuration_manifest_hash.clone(), self.clock.now());
            self.repositories.checkpoints.save(&checkpoint).await.map_err(|error| self.repository_error(&error))?;
            checkpoint
        };
        if !events.iter().any(|event| matches!(&event.payload,
            EventPayload::CheckpointSaved { checkpoint_id, .. } if checkpoint_id == &checkpoint.checkpoint_id)) {
            self.journal_for(&record, broadcast::channel(1).0).append(EventDraft::user(
                TaskEventKind::CheckpointSaved,
                EventPayload::CheckpointSaved {
                    checkpoint_id: checkpoint.checkpoint_id.clone(), event_sequence: checkpoint.event_sequence,
                    completed_step_count: checkpoint.completed_step_ids.len() as u64,
                    pending_action: checkpoint.pending_action.clone(), resume: checkpoint.resume.clone(),
                },
            )).await?;
        }
        Ok(checkpoint)
    }

    fn journal_for(
        &self,
        record: &TaskRecord,
        updates: broadcast::Sender<crate::contracts::TaskUpdate>,
    ) -> TaskJournal {
        TaskJournal::new(
            record.task_id.clone(),
            record.conversation_id.clone(),
            CorrelationId::new(self.ids.next("correlation")),
            JournalPorts {
                store: self.repositories.events.clone(),
                clock: self.clock.clone(),
                ids: self.ids.clone(),
                projection: TaskProjectionWriter {
                    tasks: Arc::clone(&self.repositories.tasks),
                },
            },
            updates,
            record.last_sequence,
        )
    }
}
