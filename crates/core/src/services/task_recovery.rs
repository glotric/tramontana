//! Explicit recovery commands shared by every shell.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::checkpoint::{ResumePlan, derive_checkpoint, derive_state};
use super::journal::{JournalPorts, TaskJournal};
use super::projection::TaskProjectionWriter;
use super::runtime::TaskDriver;
use super::task_service::{HandoffOffer, LiveTask, TaskService, WorkerOption};
use crate::config::ConfigurationSelection;
use crate::contracts::{
    AgentDescriptor, CancellationOrigin, CapabilityScope, CheckpointId, CorrelationId, ErrorCode,
    ErrorEnvelope, EventDraft, EventEnvelope, EventPayload, HandoffBundle, ResumeMode, RoutingMode,
    TaskCheckpoint, TaskEventKind, TaskId, TaskRecord, TaskState,
};
use crate::ports::{AgentProvider, ToolAdmission};
use crate::services::ProjectInstructions;
use crate::services::handoff::build_handoff;

/// Every admission a task can start with, to find the worker one recorded.
/// Every admission a task could have been started with.
fn admissions() -> impl Iterator<Item = ToolAdmission> {
    (0u8..16).map(|bits| ToolAdmission {
        writes: bits & 1 != 0,
        recipes: bits & 2 != 0,
        apps: bits & 4 != 0,
        browse: bits & 8 != 0,
    })
}

/// The routing mode a task was created under, when its record says.
///
/// Tasks created before the mode was recorded say nothing.
fn created_mode(events: &[EventEnvelope]) -> Option<RoutingMode> {
    events.iter().find_map(|event| match &event.payload {
        EventPayload::TaskCreated { routing_mode, .. } => *routing_mode,
        _ => None,
    })
}

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
    /// The project's own instructions as they are on disk, already known to
    /// hash to what the task recorded at start (ADR 0052).
    instructions: ProjectInstructions,
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
        let mut instructions = ProjectInstructions::default();
        let scope = if let Some(project_id) = &record.project_id {
            // From the files themselves where they can be read, because that
            // is what a continued run would be given. Comparing only the last
            // stored inventory let an edited `AGENTS.md` pass this check and
            // then reach the model, under a hash that no longer described it
            // (ADR 0052).
            let manifest_hash = match &self.project_sources {
                Some(reader) => {
                    // Resolved with the choices the task ran under, recovered
                    // from its own record. Resolving with none of them would
                    // make every task that named a skill look changed, and
                    // refuse a resume that nothing had touched (ADR 0053).
                    let selection = match &record.configuration_manifest_hash {
                        Some(hash) => self
                            .repositories
                            .configuration
                            .get(project_id, hash)
                            .await
                            .ok()
                            .map(|recorded| ConfigurationSelection::recorded_in(&recorded))
                            .unwrap_or_default(),
                        None => ConfigurationSelection::default(),
                    };
                    let (configuration, text) = crate::config::resolve_with_bodies(
                        reader.read(project_id).await?,
                        &selection,
                        self.clock.now(),
                    );
                    instructions = text;
                    Some(configuration.manifest_hash)
                }
                None => self
                    .get_effective_configuration(project_id)
                    .await?
                    .map(|configuration| configuration.manifest_hash),
            };
            if manifest_hash.as_ref() != record.configuration_manifest_hash.as_ref() {
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
            instructions,
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

        let worker = self.recorded_worker(
            continuation
                .derived
                .worker
                .as_ref()
                .map(|worker| &worker.descriptor),
            crate::services::runtime::recorded_admission(&continuation.events),
        )?;
        self.launch(continuation, worker, None).await
    }

    /// The workers this interrupted task can be handed to, best first.
    ///
    /// The ordering is a decision, so it is made here: normally another model
    /// on this device, then the one the routing mode chooses, then a model
    /// off it, because work leaves the device last (ADR 0051). When the task
    /// stopped because the work was beyond its model — its budget spent, or a
    /// capability it does not have — that order is reversed: the local model
    /// has just shown it cannot carry this, and escalating is the point
    /// (ADR 0055).
    ///
    /// Left out: the worker that already has the task, because continuing
    /// with it is a resume; and, for a task that runs Local Only, every model
    /// off this device, because [`Self::hand_off_to`] would refuse it.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task does not exist.
    pub async fn handoff_options(&self, task_id: &TaskId) -> Result<HandoffOffer, ErrorEnvelope> {
        let events = self.replay_task_events(task_id, 0).await?;
        let local_only =
            created_mode(&events).unwrap_or_else(|| self.routing_mode()) == RoutingMode::LocalOnly;
        let escalating =
            crate::services::stopped_on(&events).is_some_and(crate::services::beyond_the_model);
        let previous = events.iter().rev().find_map(|event| match &event.payload {
            EventPayload::WorkerSelected { descriptor } => Some(descriptor.agent_id.clone()),
            _ => None,
        });
        // The model that actually ran, which is not the same thing as the
        // worker: the routing mode's worker ran *some* model, and offering
        // that same model under its pinned name would suggest the one that
        // has just failed (ADR 0055).
        let ran_on = events.iter().rev().find_map(|event| match &event.payload {
            EventPayload::RoutingDecided { decision } => Some(decision.model.clone()),
            _ => None,
        });

        let mut workers: Vec<WorkerOption> = self
            .workers()
            .into_iter()
            .filter(|worker| Some(&worker.agent_id) != previous.as_ref())
            .filter(|worker| !(local_only && self.is_cloud_worker(&worker.agent_id)))
            .collect();
        // A stable sort, so configured models keep their own order within a
        // rank and the same task always offers the same first suggestion.
        workers.sort_by_key(|worker| {
            let pinned = worker.model.as_ref();
            // Last, whatever else it is: it has already been tried, and
            // nothing about this stop suggests it would do better.
            if pinned.is_some_and(|pinned| ran_on.as_ref() == Some(&pinned.model)) {
                return 3_u8;
            }
            let cloud =
                pinned.map(|model| model.location == crate::contracts::ModelLocation::Cloud);
            match (escalating, cloud) {
                (true, Some(true)) | (false, Some(false)) => 0,
                (_, None) => 1,
                (true, Some(false)) | (false, Some(true)) => 2,
            }
        });
        Ok(HandoffOffer {
            workers,
            escalating,
        })
    }

    /// Hand an interrupted task to the worker a person chose by name, from
    /// [`TaskService::workers`] (ADR 0048).
    ///
    /// The receiving worker offers its model what the task was allowed to
    /// offer, taken from the task's own record: handing a task on is not a
    /// way to gain writes, recipes, Apps or the browser nobody gave it.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when no such worker is offered, and every
    /// refusal [`TaskService::hand_off_task`] makes.
    pub async fn hand_off_to(
        &self,
        task_id: &TaskId,
        checkpoint_id: &CheckpointId,
        agent_id: &str,
    ) -> Result<HandoffBundle, ErrorEnvelope> {
        let events = self.replay_task_events(task_id, 0).await?;
        // A task that runs Local Only is refused a cloud model here, before
        // it moves, rather than failing once it has.
        let local_only =
            created_mode(&events).unwrap_or_else(|| self.routing_mode()) == RoutingMode::LocalOnly;
        if local_only && self.is_cloud_worker(agent_id) {
            return Err(self.error(
                ErrorCode::Conflict,
                "This task runs Local Only, so it cannot be handed to a model off this device",
            ));
        }
        let admission = crate::services::runtime::recorded_admission(&events)
            .unwrap_or(ToolAdmission::READ_ONLY);
        let receiving = self
            .build_worker(agent_id, admission)
            .ok_or_else(|| self.error(ErrorCode::NotFound, "No such worker is offered"))?;
        self.hand_off_task(task_id, checkpoint_id, receiving).await
    }

    /// Whether the worker named `agent_id` is pinned to a model off this
    /// device (ADR 0048).
    pub(super) fn is_cloud_worker(&self, agent_id: &str) -> bool {
        self.pinned.as_ref().is_some_and(|(models, _)| {
            models.iter().any(|model| {
                model.agent_id() == agent_id
                    && model.location == crate::contracts::ModelLocation::Cloud
            })
        })
    }

    /// The worker named `agent_id` at `admission`, if this service offers it.
    pub(super) fn build_worker(
        &self,
        agent_id: &str,
        admission: ToolAdmission,
    ) -> Option<Arc<dyn AgentProvider>> {
        if agent_id == crate::services::REFERENCE_AGENT_ID {
            return self.workers.as_ref().map(|choose| choose(admission));
        }
        let (models, choose) = self.pinned.as_ref()?;
        models
            .iter()
            .find(|model| model.agent_id() == agent_id)
            .map(|model| choose(admission, model))
    }

    /// The worker that recorded a task, to continue it.
    ///
    /// A task may have chosen its admission when it started (ADR 0031), so
    /// the service's own worker need not be the one. Each worker this service
    /// could have run is compared with the recorded descriptor. None matching
    /// is a refusal: a resume cannot add to or take from what the model may
    /// be offered.
    ///
    /// The admission the task recorded is tried first. Two admissions can
    /// describe themselves alike (Apps change no descriptor field), and the
    /// wrong one would interrupt the resumed task at its first turn.
    fn recorded_worker(
        &self,
        recorded: Option<&AgentDescriptor>,
        admission: Option<ToolAdmission>,
    ) -> Result<Arc<dyn AgentProvider>, ErrorEnvelope> {
        let Some(recorded) = recorded else {
            return Ok(Arc::clone(&self.providers.worker));
        };
        let chosen = admission
            .into_iter()
            .chain(admissions())
            .flat_map(|admission| {
                let general = self.workers.as_ref().map(|choose| choose(admission));
                let pinned = self.pinned.as_ref().and_then(|(models, choose)| {
                    models
                        .iter()
                        .find(|model| model.agent_id() == recorded.agent_id)
                        .map(|model| choose(admission, model))
                });
                general.into_iter().chain(pinned)
            });
        chosen
            .chain(std::iter::once(Arc::clone(&self.providers.worker)))
            .find(|worker| worker.describe() == *recorded)
            .ok_or_else(|| {
                self.error(
                    ErrorCode::Conflict,
                    "Same-worker resume requires the recorded worker and capabilities",
                )
            })
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
        self.launch(continuation, receiving, Some(handoff.clone()))
            .await?;
        Ok(handoff)
    }

    /// Move an interrupted task back into `preparing` and run it.
    ///
    /// `handoff` is absent for a same-worker resume and present for a
    /// handoff to `worker`. Everything else — the live registry entry, the
    /// journal, the configuration read back by hash, the replay guard built
    /// from the events, the routing mode — is identical, because the
    /// difference between the two paths is only who continues.
    async fn launch(
        &self,
        continuation: Continuation,
        worker: Arc<dyn AgentProvider>,
        handoff: Option<HandoffBundle>,
    ) -> Result<(), ErrorEnvelope> {
        let Continuation {
            record,
            checkpoint,
            events,
            scope,
            instructions,
            ..
        } = continuation;
        let task_id = record.task_id.clone();

        let cancel = CancellationToken::new();
        let interrupt = CancellationToken::new();
        let (finished, receiver) = watch::channel(false);
        let (updates, _) = broadcast::channel(256);

        let reason = if handoff.is_some() {
            super::HANDED_OVER
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

        // The mode the task was created under, not the profile's now: a task
        // started Local Only must not continue somewhere it forbade (ADR
        // 0031). Only a task recorded before modes were has none, and takes
        // the profile's, as it always did.
        let routing_mode = created_mode(&events).unwrap_or_else(|| self.routing_mode());
        let plan = ResumePlan::from_events(&events);
        let driver = TaskDriver::new(journal, self.providers.clone(), scope, self.clock.clone())
            .with_routing_mode(routing_mode)
            .with_configuration(configuration)
            .with_instructions(instructions)
            .with_checkpoints(Arc::clone(&self.repositories.checkpoints));
        let driver = match handoff {
            Some(handoff) => driver.handing_over(worker, handoff, plan, interrupt),
            None => driver
                .with_worker(worker)
                .continuing(Some(checkpoint), plan, interrupt),
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
