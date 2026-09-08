use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::contracts::{EventEnvelope, TaskId};
use crate::ports::{EventStore, EventStoreError};

/// An in-memory append-only event store.
///
/// Enforces exactly the invariants a durable store must, so the same contract
/// suite runs against this and against SQLite from M2: sequences start at 1 and
/// increase by one, appends are all-or-nothing, and a stale expected sequence
/// is a conflict rather than an interleave.
#[derive(Debug, Default)]
pub struct InMemoryEventStore {
    streams: Mutex<HashMap<TaskId, Vec<EventEnvelope>>>,
}

impl InMemoryEventStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Lock the stream map, mapping a poisoned lock to a normalized error.
///
/// A poisoned mutex means another thread panicked mid-write. Treating it as
/// storage failure rather than panicking again keeps the fake honest about
/// what a real store would report.
fn lock(
    store: &InMemoryEventStore,
) -> Result<std::sync::MutexGuard<'_, HashMap<TaskId, Vec<EventEnvelope>>>, EventStoreError> {
    store
        .streams
        .lock()
        .map_err(|_| EventStoreError::Unavailable("event store lock poisoned".to_owned()))
}

#[async_trait]
impl EventStore for InMemoryEventStore {
    async fn append(
        &self,
        task_id: &TaskId,
        expected_sequence: u64,
        events: Vec<EventEnvelope>,
    ) -> Result<u64, EventStoreError> {
        let mut streams = lock(self)?;
        let stream = streams.entry(task_id.clone()).or_default();

        let actual = stream.last().map_or(0, |event| event.sequence);
        if actual != expected_sequence {
            return Err(EventStoreError::SequenceConflict {
                task_id: task_id.clone(),
                expected: expected_sequence,
                actual,
            });
        }

        // Verify before mutating so a misnumbered batch leaves no partial write.
        for (offset, event) in events.iter().enumerate() {
            let expected = expected_sequence + offset as u64 + 1;
            if event.sequence != expected || event.task_id != *task_id {
                return Err(EventStoreError::NonConsecutive {
                    task_id: task_id.clone(),
                    expected_first: expected_sequence + 1,
                });
            }
        }

        stream.extend(events);
        Ok(stream
            .last()
            .map_or(expected_sequence, |event| event.sequence))
    }

    async fn replay(
        &self,
        task_id: &TaskId,
        after_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, EventStoreError> {
        let streams = lock(self)?;
        Ok(streams
            .get(task_id)
            .map(|stream| {
                stream
                    .iter()
                    .filter(|event| event.sequence > after_sequence)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn last_sequence(&self, task_id: &TaskId) -> Result<u64, EventStoreError> {
        let streams = lock(self)?;
        Ok(streams
            .get(task_id)
            .and_then(|stream| stream.last())
            .map_or(0, |event| event.sequence))
    }
}
