//! SP0-A spike (`docs/SHARED_PROJECTS_PLAN.md`): a task's events delivered to
//! a client over a real WebSocket on loopback, through the same
//! `forward_task_updates` the desktop uses for its window.
//!
//! The question: does replay-then-live, resumed from the last sequence a
//! client saw, give every event exactly once across disconnects — including
//! when the server cuts off a client that reads too slowly? Throwaway server
//! code; the delivery rules under test are the production ones.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tramontana_core::contracts::{TaskId, TaskUpdate};
use tramontana_core::ports::EventStore;
use tramontana_core::services::{TaskService, forward_task_updates};
use tramontana_core::testing::{
    FakeModelProvider, FakeToolProvider, FakeWorker, InMemoryEventStore, ScriptedApprover,
    conversation_in_a_project, start_request, test_service, wait_state,
};

fn service_with_store() -> (Arc<TaskService>, Arc<InMemoryEventStore>) {
    let store = Arc::new(InMemoryEventStore::new());
    let service = test_service(
        Arc::clone(&store) as Arc<dyn EventStore>,
        vec![Arc::new(FakeModelProvider::streaming([
            "Hello", ", ", "world",
        ]))],
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(ScriptedApprover::unavailable()),
        Arc::new(FakeWorker::new()),
    );
    (Arc::new(service), store)
}

async fn start_task(service: &TaskService) -> TaskId {
    let conversation = conversation_in_a_project(service).await;
    service
        .start_task(start_request(&conversation, "say hello"))
        .await
        .unwrap()
}

async fn await_terminal(service: &TaskService, task_id: &TaskId) {
    let every = Duration::from_millis(1);
    wait_state(service, task_id, every * 1_000, every, |s| s.is_terminal()).await;
}

/// A host in miniature: each connection names a task and the last sequence
/// it saw, and gets the rest. A client whose `queue` fills is cut off, as a
/// host must do rather than buffer without bound; it reconnects to catch up.
async fn serve(service: Arc<TaskService>, queue: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, mut source) = socket.split();
                let Some(Ok(Message::Text(request))) = source.next().await else {
                    return;
                };
                let request: serde_json::Value = serde_json::from_str(request.as_str()).unwrap();
                let task_id = TaskId::new(request["taskId"].as_str().unwrap());
                let after = request["after"].as_u64().unwrap();

                let (replayed, updates) = service
                    .subscribe_task_events(&task_id, after)
                    .await
                    .unwrap();
                let (outgoing, mut pending) = mpsc::channel::<String>(queue);
                tokio::spawn(forward_task_updates(
                    Arc::clone(&service),
                    task_id,
                    after,
                    replayed,
                    updates,
                    move |update| {
                        outgoing
                            .try_send(serde_json::to_string(&update).unwrap())
                            .is_ok()
                    },
                ));
                while let Some(text) = pending.recv().await {
                    if sink.send(Message::text(text)).await.is_err() {
                        return;
                    }
                }
                let _ = sink.close().await;
            });
        }
    });
    addr
}

/// One connection: the sequences received, and whether the terminal event
/// was among them. `give_up_after` drops the connection early, as a client
/// whose network fails would.
async fn watch_once(
    addr: SocketAddr,
    task_id: &TaskId,
    after: u64,
    give_up_after: Option<usize>,
) -> (Vec<u64>, bool) {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();
    let request = serde_json::json!({ "taskId": task_id.as_str(), "after": after });
    socket
        .send(Message::text(request.to_string()))
        .await
        .unwrap();

    let mut sequences = Vec::new();
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        if let TaskUpdate::Event { event } = serde_json::from_str(text.as_str()).unwrap() {
            sequences.push(event.sequence);
            if event.is_terminal_outcome() {
                return (sequences, true);
            }
            if give_up_after == Some(sequences.len()) {
                return (sequences, false);
            }
        }
    }
    (sequences, false)
}

/// Reconnect from the last sequence seen until the terminal event arrives.
/// Returns every sequence in arrival order and how many connections it took.
async fn watch_to_the_end(
    addr: SocketAddr,
    task_id: &TaskId,
    first_give_up_after: Option<usize>,
) -> (Vec<u64>, usize) {
    let mut seen = Vec::new();
    for connection in 1..=1_000 {
        let after = seen.last().copied().unwrap_or(0);
        let give_up = if connection == 1 {
            first_give_up_after
        } else {
            None
        };
        let (sequences, terminal) = watch_once(addr, task_id, after, give_up).await;
        seen.extend(sequences);
        if terminal {
            return (seen, connection);
        }
    }
    panic!("never saw the terminal event");
}

#[tokio::test]
async fn a_client_that_drops_and_reconnects_sees_every_event_exactly_once() {
    let (service, store) = service_with_store();
    let addr = serve(Arc::clone(&service), 1_024).await;
    let task_id = start_task(&service).await;

    let (seen, connections) = watch_to_the_end(addr, &task_id, Some(2)).await;

    // Not deduplicated before comparing: a duplicate would fail this too.
    let last = store.last_sequence(&task_id).await.unwrap();
    assert_eq!(seen, (1..=last).collect::<Vec<_>>());
    assert!(connections >= 2, "the first connection was dropped early");
}

#[tokio::test]
async fn a_client_too_slow_for_its_queue_is_cut_off_and_catches_up() {
    let (service, store) = service_with_store();
    // A queue of one: every event after the first overflows it.
    let addr = serve(Arc::clone(&service), 1).await;
    let task_id = start_task(&service).await;
    await_terminal(&service, &task_id).await;

    let (seen, connections) = watch_to_the_end(addr, &task_id, None).await;

    let last = store.last_sequence(&task_id).await.unwrap();
    assert_eq!(seen, (1..=last).collect::<Vec<_>>());
    assert!(
        connections > 1,
        "the overflow path was exercised: {connections} connection(s) for {last} events"
    );
}
