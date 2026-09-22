//! A DevTools protocol connection: requests matched to their answers, and
//! each page's events delivered to whoever drives that page.
//!
//! One websocket to the browser, with pages attached in "flat" mode, so every
//! page's messages travel over it tagged with their session. Nothing here
//! decides anything; it only carries messages, and bounds how long it waits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// The longest one message's answer is waited for.
pub(crate) const CALL_LIMIT: Duration = Duration::from_secs(15);

/// The largest message read from the browser. A page's text is bounded
/// before it is sent, so anything near this is not a page Tramontana asked
/// for.
const MESSAGE_LIMIT: usize = 16 * 1024 * 1024;

/// Why a message went unanswered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CdpError {
    /// The browser answered with an error.
    Protocol(String),
    /// No answer in time.
    Timeout,
    /// The connection is gone: the browser quit, or was closed.
    Closed,
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(message) => write!(f, "the browser refused: {message}"),
            Self::Timeout => f.write_str("the browser did not answer in time"),
            Self::Closed => f.write_str("the browser is no longer running"),
        }
    }
}

/// An event the browser sent unasked.
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub(crate) method: String,
    pub(crate) params: Value,
}

type Pending = HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>;

/// The connection to one browser.
pub(crate) struct Connection {
    outgoing: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<Pending>>,
    /// Each attached page's events, by session.
    sessions: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Event>>>>,
    next_id: AtomicU64,
    closed: CancellationToken,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("closed", &self.closed.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Connect to a browser's DevTools endpoint on this machine.
    pub(crate) async fn open(endpoint: &str) -> Result<Arc<Self>, CdpError> {
        if !endpoint.starts_with("ws://127.0.0.1:") {
            // Only ever a port the browser Tramontana started opened here.
            return Err(CdpError::Closed);
        }
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MESSAGE_LIMIT))
            .max_frame_size(Some(MESSAGE_LIMIT));
        let (socket, _) = tokio::time::timeout(
            CALL_LIMIT,
            tokio_tungstenite::connect_async_with_config(endpoint, Some(config), false),
        )
        .await
        .map_err(|_| CdpError::Timeout)?
        .map_err(|_| CdpError::Closed)?;
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut queue) = mpsc::unbounded_channel::<String>();
        let connection = Arc::new(Self {
            outgoing,
            pending: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            closed: CancellationToken::new(),
        });

        let closed = connection.closed.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = closed.cancelled() => break,
                    message = queue.recv() => {
                        let Some(message) = message else { break };
                        if sink.send(Message::text(message)).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = sink.close().await;
            closed.cancel();
        });

        let closed = connection.closed.clone();
        let pending = Arc::clone(&connection.pending);
        let sessions = Arc::clone(&connection.sessions);
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    () = closed.cancelled() => break,
                    message = stream.next() => message,
                };
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    _ => break,
                };
                let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                dispatch(value, &pending, &sessions);
            }
            closed.cancel();
            // Nobody waits for an answer that cannot come.
            if let Ok(mut pending) = pending.lock() {
                for (_, waiting) in pending.drain() {
                    let _ = waiting.send(Err(CdpError::Closed));
                }
            }
            if let Ok(mut sessions) = sessions.lock() {
                sessions.clear();
            }
        });
        Ok(connection)
    }

    /// Whether the connection is gone.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Close the connection.
    pub(crate) fn close(&self) {
        self.closed.cancel();
    }

    /// Receive a page's events from now on.
    pub(crate) fn subscribe(&self, session: &str) -> mpsc::UnboundedReceiver<Event> {
        let (sender, receiver) = mpsc::unbounded_channel();
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(session.to_owned(), sender);
        }
        receiver
    }

    /// Stop delivering a page's events.
    pub(crate) fn unsubscribe(&self, session: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session);
        }
    }

    /// Send a command and wait for its answer.
    pub(crate) async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        if self.is_closed() {
            return Err(CdpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| CdpError::Closed)?
            .insert(id, answer);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = Value::String(session.to_owned());
        }
        if self.outgoing.send(message.to_string()).is_err() {
            self.forget(id);
            return Err(CdpError::Closed);
        }
        let result = tokio::time::timeout(CALL_LIMIT, answered).await;
        self.forget(id);
        match result {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => Err(CdpError::Timeout),
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

/// Deliver one message: an answer to whoever asked, an event to its page.
fn dispatch(
    value: Value,
    pending: &Mutex<Pending>,
    sessions: &Mutex<HashMap<String, mpsc::UnboundedSender<Event>>>,
) {
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        let waiting = pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&id));
        if let Some(waiting) = waiting {
            let answer = match value.get("error") {
                Some(error) => Err(CdpError::Protocol(
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .chars()
                        .take(200)
                        .collect(),
                )),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = waiting.send(answer);
        }
        return;
    }
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    // A page's own events carry its session. A page closing is announced at
    // the browser's level, naming the session, and goes to that page too.
    let session = value
        .get("sessionId")
        .and_then(Value::as_str)
        .or_else(|| {
            (method == "Target.detachedFromTarget")
                .then(|| params.get("sessionId").and_then(Value::as_str))
                .flatten()
        })
        .map(str::to_owned);
    let Some(session) = session else {
        return;
    };
    if let Ok(sessions) = sessions.lock()
        && let Some(sender) = sessions.get(&session)
    {
        let _ = sender.send(Event {
            method: method.to_owned(),
            params,
        });
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_reaches_whoever_asked_and_an_error_says_why() {
        let pending = Mutex::new(HashMap::new());
        let sessions = Mutex::new(HashMap::new());
        let (first, mut first_answer) = oneshot::channel();
        let (second, mut second_answer) = oneshot::channel();
        pending.lock().unwrap().insert(1, first);
        pending.lock().unwrap().insert(2, second);
        dispatch(
            json!({"id": 1, "result": {"ok": true}}),
            &pending,
            &sessions,
        );
        dispatch(
            json!({"id": 2, "error": {"code": -32000, "message": "No node"}}),
            &pending,
            &sessions,
        );
        assert_eq!(first_answer.try_recv().unwrap(), Ok(json!({"ok": true})));
        assert_eq!(
            second_answer.try_recv().unwrap(),
            Err(CdpError::Protocol("No node".into()))
        );
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn a_page_receives_its_own_events_and_its_closing_but_no_other_pages() {
        let pending = Mutex::new(HashMap::new());
        let sessions = Mutex::new(HashMap::new());
        let (mine, mut received) = mpsc::unbounded_channel();
        sessions.lock().unwrap().insert("s1".to_owned(), mine);
        dispatch(
            json!({"sessionId": "s1", "method": "Page.loadEventFired", "params": {}}),
            &pending,
            &sessions,
        );
        dispatch(
            json!({"sessionId": "s2", "method": "Page.loadEventFired", "params": {}}),
            &pending,
            &sessions,
        );
        dispatch(
            json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "s1"}}),
            &pending,
            &sessions,
        );
        assert_eq!(received.try_recv().unwrap().method, "Page.loadEventFired");
        assert_eq!(
            received.try_recv().unwrap().method,
            "Target.detachedFromTarget"
        );
        assert!(received.try_recv().is_err());
    }
}
