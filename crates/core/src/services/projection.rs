use std::sync::Arc;

use crate::contracts::{EventEnvelope, EventPayload};
use crate::ports::{Clock, TaskRepository};

/// Keeps a task's cached row in step with its events.
///
/// The event stream is the record; the task row is a cache so that reading a
/// task does not require replaying it. A write failure here is deliberately
/// **not** propagated: the durable event is already committed, and startup
/// recovery rebuilds the cache from the events. Failing the task because a
/// cache update did not land would turn a recoverable inconsistency into a
/// lost task.
#[derive(Clone)]
pub struct TaskProjectionWriter {
    /// Where task rows live.
    pub tasks: Arc<dyn TaskRepository>,
    /// Source of the update timestamp.
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for TaskProjectionWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskProjectionWriter")
            .finish_non_exhaustive()
    }
}

impl TaskProjectionWriter {
    /// Update the cached row from a freshly written event.
    pub async fn record(&self, event: &EventEnvelope) {
        let state = match &event.payload {
            EventPayload::TaskStateChanged { current, .. } => Some(*current),
            _ => None,
        };

        let current = match state {
            Some(state) => state,
            None => match self.tasks.get(&event.task_id).await {
                Ok(record) => record.state,
                Err(error) => {
                    tracing::warn!(
                        task_id = %event.task_id,
                        %error,
                        "could not read the task row to update its projection"
                    );
                    return;
                }
            },
        };

        if let Err(error) = self
            .tasks
            .update_projection(&event.task_id, current, event.sequence, &self.clock.now())
            .await
        {
            // Recoverable: the events are the record, and startup rebuilds
            // this cache from them.
            tracing::warn!(
                task_id = %event.task_id,
                sequence = event.sequence,
                %error,
                "task projection is behind its events; startup recovery will repair it"
            );
        }
    }
}
