use std::sync::Arc;

use tokio::sync::broadcast;

use crate::contracts::{
    ConversationId, CorrelationId, ErrorCode, ErrorEnvelope, EventDraft, EventEnvelope, EventId,
    StreamFrame, TaskId, TaskUpdate,
};
use crate::ports::{Clock, EventStore, IdSource};
use crate::services::projection::TaskProjectionWriter;

/// The ports a journal needs.
///
/// Grouped so adding a port later does not lengthen an already-long parameter
/// list, and so the composition root assembles them in one place.
#[derive(Clone)]
pub struct JournalPorts {
    /// Durable storage.
    pub store: Arc<dyn EventStore>,
    /// Source of timestamps.
    pub clock: Arc<dyn Clock>,
    /// Source of identifiers.
    pub ids: Arc<dyn IdSource>,
    /// Cached task projection updated after durability and before publication.
    pub projection: TaskProjectionWriter,
}

impl std::fmt::Debug for JournalPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalPorts").finish_non_exhaustive()
    }
}

/// Appends events for one task and publishes them to subscribers.
///
/// Holds the sequence for its task, so numbering lives in exactly one place.
/// Every append is optimistic against the store, which is what stops two
/// writers interleaving into one stream.
#[derive(Clone)]
pub struct TaskJournal {
    task_id: TaskId,
    conversation_id: Option<ConversationId>,
    correlation_id: CorrelationId,
    ports: JournalPorts,
    updates: broadcast::Sender<TaskUpdate>,
    sequence: Arc<tokio::sync::Mutex<u64>>,
}

impl std::fmt::Debug for TaskJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskJournal")
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

impl TaskJournal {
    /// Create a journal for a task whose stream is currently at `sequence`.
    #[must_use]
    pub fn new(
        task_id: TaskId,
        conversation_id: Option<ConversationId>,
        correlation_id: CorrelationId,
        ports: JournalPorts,
        updates: broadcast::Sender<TaskUpdate>,
        sequence: u64,
    ) -> Self {
        Self {
            task_id,
            conversation_id,
            correlation_id,
            ports,
            updates,
            sequence: Arc::new(tokio::sync::Mutex::new(sequence)),
        }
    }

    /// Append one event and publish it.
    ///
    /// # Errors
    ///
    /// Returns a normalized error when the store rejects the append, including
    /// a sequence conflict caused by a concurrent writer.
    pub async fn append(&self, draft: EventDraft) -> Result<EventEnvelope, ErrorEnvelope> {
        let mut sequence = self.sequence.lock().await;
        let expected = *sequence;

        let event = draft.into_envelope(
            EventId::new(self.ports.ids.next("event")),
            self.task_id.clone(),
            self.conversation_id.clone(),
            expected + 1,
            self.ports.clock.now(),
            self.correlation_id.clone(),
        );

        self.ports
            .store
            .append(&self.task_id, expected, vec![event.clone()])
            .await
            .map_err(|error| {
                let code = match error {
                    crate::ports::EventStoreError::SequenceConflict { .. } => ErrorCode::Conflict,
                    _ => ErrorCode::PersistenceError,
                };
                ErrorEnvelope::new(code, error.to_string(), self.correlation_id.clone())
            })?;

        *sequence = event.sequence;
        drop(sequence);

        // The event is already canonical. Bring the disposable task-row cache
        // up to date before publishing so a client cannot observe a terminal
        // event and close the process in the small gap before projection.
        self.ports.projection.record(&event).await;
        self.publish_persisted(&event);

        Ok(event)
    }

    /// Publish an event which is already durable.
    ///
    /// Used for the initial batch because task creation and those events must
    /// commit in one repository transaction before anything is announced.
    pub(crate) fn publish_persisted(&self, event: &EventEnvelope) {
        // Redaction already happened upstream; secret events are audit markers
        // and never reach a subscriber (`CORE_CONTRACTS.md` §5).
        if event.is_deliverable_to_ui() {
            // A send failure only means nobody is listening right now, which is
            // normal: the durable record is already written and a subscriber
            // catches up by replaying it.
            let _ = self.updates.send(TaskUpdate::Event {
                event: Box::new(event.clone()),
            });
        }
    }

    /// Publish an ephemeral frame without persisting it.
    pub fn publish_frame(&self, frame: StreamFrame) {
        let _ = self.updates.send(TaskUpdate::Frame { frame });
    }

    /// The task this journal writes for.
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// The correlation identifier shared by everything in this task.
    #[must_use]
    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }

    /// The conversation this task belongs to, when it has one.
    #[must_use]
    pub fn conversation_id(&self) -> Option<&ConversationId> {
        self.conversation_id.as_ref()
    }

    /// Mint an identifier from the same source the journal numbers events with.
    ///
    /// Exposed so a caller building a durable record does not reach for a
    /// second source and lose determinism under the fakes.
    #[must_use]
    pub fn next_id(&self, prefix: &str) -> String {
        self.ports.ids.next(prefix)
    }

    /// The current time, from the same clock the journal stamps events with.
    #[must_use]
    pub fn now(&self) -> crate::contracts::Timestamp {
        self.ports.clock.now()
    }
}
