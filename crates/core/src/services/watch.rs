//! Delivering a task's updates to one watcher: each event once, in order.
//!
//! Every adapter that shows a task live — the desktop's window today, a
//! remote client of a host later — needs the same delivery rules, so they
//! live here rather than in each adapter.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::contracts::{EventEnvelope, TaskId, TaskUpdate};
use crate::services::TaskService;

/// Send a task's updates to one watcher until the task ends.
///
/// `replayed` holds the durable events after `after_sequence`, and `updates`
/// the live stream, subscribed before the replay was read
/// ([`TaskService::subscribe_task_events`]). An event can arrive on both, so
/// anything at or below the highest sequence already sent is dropped: the
/// watcher sees each event once, in order, with no gap. A watcher that falls
/// behind catches up from the store. `send` returns false when the watcher
/// has gone, which ends the delivery; so does the task's terminal event.
pub async fn forward_task_updates<F>(
    service: Arc<TaskService>,
    task_id: TaskId,
    after_sequence: u64,
    replayed: Vec<EventEnvelope>,
    mut updates: broadcast::Receiver<TaskUpdate>,
    mut send: F,
) where
    F: FnMut(TaskUpdate) -> bool + Send + 'static,
{
    let mut highest = after_sequence;
    let mut terminal_replayed = false;
    for event in replayed {
        highest = highest.max(event.sequence);
        terminal_replayed |= event.is_terminal_outcome();
        if !send(TaskUpdate::Event {
            event: Box::new(event),
        }) {
            return; // The watcher went away.
        }
    }
    if terminal_replayed {
        return;
    }

    loop {
        match updates.recv().await {
            Ok(TaskUpdate::Event { event }) => {
                // Subscription precedes replay, so an event can arrive
                // twice. Dropping the duplicate here keeps the watcher
                // from having to know that.
                if event.sequence <= highest {
                    continue;
                }
                highest = event.sequence;
                let terminal = event.is_terminal_outcome();
                if !send(TaskUpdate::Event { event }) || terminal {
                    return;
                }
            }
            Ok(update) => {
                if !send(update) {
                    return;
                }
            }
            // Lagging is recoverable: durable events are in the store, so
            // catch up rather than reporting a failure to the user.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let Ok((missed, _)) = service.subscribe_task_events(&task_id, highest).await else {
                    return;
                };
                for event in missed {
                    highest = highest.max(event.sequence);
                    let terminal = event.is_terminal_outcome();
                    if !send(TaskUpdate::Event {
                        event: Box::new(event),
                    }) || terminal
                    {
                        return;
                    }
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
