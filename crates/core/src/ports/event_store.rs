use async_trait::async_trait;

use crate::contracts::{EventEnvelope, TaskId};

/// Why an append or replay failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EventStoreError {
    /// Another writer advanced the stream first.
    ///
    /// This is the optimistic-concurrency signal, not a fault: the caller
    /// re-reads and decides again. It exists so two writers can never
    /// interleave into one task's stream and produce an order neither
    /// intended.
    #[error("expected sequence {expected} for task {task_id}, but the stream is at {actual}")]
    SequenceConflict {
        /// The task whose stream conflicted.
        task_id: TaskId,
        /// The sequence the caller believed it was appending after.
        expected: u64,
        /// The sequence the stream is actually at.
        actual: u64,
    },
    /// The events offered were not numbered consecutively from `expected + 1`.
    #[error("events for task {task_id} must be numbered consecutively from {expected_first}")]
    NonConsecutive {
        /// The task whose append was rejected.
        task_id: TaskId,
        /// The sequence the first event should have carried.
        expected_first: u64,
    },
    /// Durable storage failed.
    #[error("event store unavailable: {0}")]
    Unavailable(String),
}

/// An append-only journal of task events.
///
/// The store is the durable record; a task's state is a projection of it. Two
/// invariants hold for every implementation, and the shared contract suite
/// checks both:
///
/// 1. **Sequences start at 1 and increase by exactly one per task.** A gap
///    means loss.
/// 2. **`append` is atomic and optimistic.** Either every event lands, or none
///    does and the caller learns the stream moved.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append events, expecting the stream to be at `expected_sequence`.
    ///
    /// Pass `0` for a task's first append. The events must already carry
    /// sequences `expected_sequence + 1 ..= expected_sequence + events.len()`.
    ///
    /// # Errors
    ///
    /// [`EventStoreError::SequenceConflict`] when the stream has moved,
    /// [`EventStoreError::NonConsecutive`] when the events are misnumbered,
    /// and [`EventStoreError::Unavailable`] when storage failed.
    async fn append(
        &self,
        task_id: &TaskId,
        expected_sequence: u64,
        events: Vec<EventEnvelope>,
    ) -> Result<u64, EventStoreError>;

    /// Read durable events after `after_sequence`, in order.
    ///
    /// Pass `0` to read the whole stream. This is how a reconnecting interface
    /// catches up before live delivery resumes (`CORE_CONTRACTS.md` §13).
    ///
    /// # Errors
    ///
    /// [`EventStoreError::Unavailable`] when storage failed.
    async fn replay(
        &self,
        task_id: &TaskId,
        after_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, EventStoreError>;

    /// The highest sequence written for a task, or `0` if it has no events.
    ///
    /// # Errors
    ///
    /// [`EventStoreError::Unavailable`] when storage failed.
    async fn last_sequence(&self, task_id: &TaskId) -> Result<u64, EventStoreError>;
}
