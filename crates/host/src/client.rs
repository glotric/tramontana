//! A desktop's side of a host connection (SP1 step 4).
//!
//! The desktop's Rust side holds one of these per paired host and forwards
//! commands through it; the webview never sees the token or the socket
//! (ADR 0074 §1). Replies are matched to commands by id; everything else the
//! host sends — task updates, approval questions — arrives as
//! [`HostEvent`]s on one channel, in order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tramontana_core::contracts::{
    ApprovalId, ApprovalRequest, CorrelationId, ErrorCode, ErrorEnvelope, TaskId, TaskUpdate,
};

use crate::devices::Token;
use crate::protocol::{ClientFrame, ServerFrame};
use crate::tls::{ConnectError, connect};

/// What an invite link carries (ADR 0076 §2):
/// `tramontana://pair?host=<address>&fingerprint=<sha256>&code=<code>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairLink {
    /// Where the host listens, `host:port`.
    pub address: String,
    /// The pin for the host's certificate.
    pub fingerprint: String,
    /// The one-time code.
    pub code: Token,
}

impl PairLink {
    /// Read an invite link. `None` for anything that is not exactly one.
    #[must_use]
    pub fn parse(link: &str) -> Option<Self> {
        let query = link.trim().strip_prefix("tramontana://pair?")?;
        let (mut address, mut fingerprint, mut code) = (None, None, None);
        for pair in query.split('&') {
            match pair.split_once('=')? {
                ("host", value) => address = Some(value),
                ("fingerprint", value) => fingerprint = Some(value),
                ("code", value) => code = Some(value),
                _ => return None,
            }
        }
        let address = address.filter(|value| !value.is_empty() && !value.contains('/'))?;
        let is_hex =
            |value: &&str| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit());
        Some(Self {
            address: address.to_owned(),
            fingerprint: fingerprint.filter(is_hex)?.to_owned(),
            code: Token::new(code.filter(is_hex)?),
        })
    }
}

/// Why a desktop could not pair or connect.
#[derive(Debug)]
pub enum ClientError {
    /// No connection, or not to the pinned host.
    Connect(ConnectError),
    /// The host refused: an unknown device, a spent invite, another version.
    Refused(ErrorEnvelope),
    /// The connection ended, or the host sent something that is not a frame.
    Closed,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(error) => error.fmt(f),
            Self::Refused(error) => f.write_str(&error.message),
            Self::Closed => f.write_str("the host closed the connection"),
        }
    }
}

impl std::error::Error for ClientError {}

/// What the host sends that is not a reply.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// A question to show as an approval card.
    ApprovalRequested(Box<ApprovalRequest>),
    /// A card to take down.
    ApprovalSettled(ApprovalId),
    /// The connection is gone. Watches end; reconnect and watch again from
    /// the last sequence seen.
    Disconnected,
}

/// Callers waiting for a reply, by command id. `None` once the connection
/// is gone, so a command made after that fails at once instead of waiting.
type Replies = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<Value, ErrorEnvelope>>>>>>;

/// Where each watch's updates go.
///
/// The host starts sending a watch's updates as soon as it subscribes, so
/// some can arrive before the reply that names the watch. Those are held
/// here, by watch, until [`HostClient::watch`] registers where they go.
#[derive(Default)]
struct Watches {
    sinks: HashMap<u64, Box<dyn FnMut(TaskUpdate) -> bool + Send>>,
    early: HashMap<u64, Vec<TaskUpdate>>,
}

impl std::fmt::Debug for Watches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watches")
            .field("sinks", &self.sinks.len())
            .field("early", &self.early.len())
            .finish()
    }
}

/// One live connection to a host, as one paired device.
#[derive(Debug)]
pub struct HostClient {
    outgoing: mpsc::UnboundedSender<ClientFrame>,
    watches: Arc<Mutex<Watches>>,
    replies: Replies,
    next_id: std::sync::atomic::AtomicU64,
    principal: String,
}

/// Pair this desktop with the host an invite link names. Returns who the
/// device belongs to and its token, which the caller keeps in the OS
/// credential store.
///
/// # Errors
///
/// [`ClientError::Connect`] when the host is unreachable or not the pinned
/// one; [`ClientError::Refused`] for a spent, expired or unknown invite.
pub async fn pair(link: &PairLink, device_name: &str) -> Result<(String, Token), ClientError> {
    let mut socket = connect(&link.address, &link.fingerprint)
        .await
        .map_err(ClientError::Connect)?;
    send(
        &mut socket,
        &ClientFrame::Pair {
            schema_version: tramontana_core::SCHEMA_VERSION,
            code: link.code.clone(),
            device_name: device_name.to_owned(),
        },
    )
    .await?;
    let answer = next(&mut socket).await;
    let _ = socket.close(None).await;
    match answer {
        Some(ServerFrame::Paired {
            principal, token, ..
        }) => Ok((principal, token)),
        Some(ServerFrame::Refused { error }) => Err(ClientError::Refused(error)),
        _ => Err(ClientError::Closed),
    }
}

impl HostClient {
    /// Connect as a paired device. Returns the client and the channel its
    /// [`HostEvent`]s arrive on.
    ///
    /// # Errors
    ///
    /// As [`pair`], with [`ClientError::Refused`] for an unknown or revoked
    /// device.
    pub async fn connect(
        address: &str,
        fingerprint: &str,
        token: &Token,
    ) -> Result<(Self, mpsc::UnboundedReceiver<HostEvent>), ClientError> {
        let mut socket = connect(address, fingerprint)
            .await
            .map_err(ClientError::Connect)?;
        send(
            &mut socket,
            &ClientFrame::Hello {
                schema_version: tramontana_core::SCHEMA_VERSION,
                token: token.clone(),
            },
        )
        .await?;
        let principal = match next(&mut socket).await {
            Some(ServerFrame::Welcome { principal, .. }) => principal,
            Some(ServerFrame::Refused { error }) => return Err(ClientError::Refused(error)),
            _ => return Err(ClientError::Closed),
        };

        let (mut sink, mut source) = socket.split();
        let (outgoing, mut queued) = mpsc::unbounded_channel::<ClientFrame>();
        let (events, receiver) = mpsc::unbounded_channel();
        let replies: Replies = Arc::new(Mutex::new(Some(HashMap::new())));
        let watches: Arc<Mutex<Watches>> = Arc::default();

        // The writer: commands go out in the order they were made.
        tokio::spawn(async move {
            while let Some(frame) = queued.recv().await {
                let Ok(text) = serde_json::to_string(&frame) else {
                    break;
                };
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        // The reader: replies to their callers, everything else to events.
        let waiting = Arc::clone(&replies);
        let watching = Arc::clone(&watches);
        tokio::spawn(async move {
            while let Some(Ok(message)) = source.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(frame) = serde_json::from_str::<ServerFrame>(text.as_str()) else {
                    break;
                };
                let event = match frame {
                    ServerFrame::Reply { id, result, error } => {
                        let caller = lock(&waiting)
                            .as_mut()
                            .and_then(|callers| callers.remove(&id));
                        if let Some(caller) = caller {
                            let _ = caller.send(match error {
                                Some(error) => Err(error),
                                None => Ok(result.unwrap_or(Value::Null)),
                            });
                        }
                        continue;
                    }
                    ServerFrame::Update { watch, update } => {
                        let mut watching = lock(&watching);
                        match watching.sinks.get_mut(&watch) {
                            Some(sink) => {
                                if !sink(*update) {
                                    watching.sinks.remove(&watch);
                                }
                            }
                            None => watching.early.entry(watch).or_default().push(*update),
                        }
                        continue;
                    }
                    ServerFrame::ApprovalRequested { request } => {
                        HostEvent::ApprovalRequested(request)
                    }
                    ServerFrame::ApprovalSettled { approval_id } => {
                        HostEvent::ApprovalSettled(approval_id)
                    }
                    _ => break,
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            // Every caller still waiting learns the host is gone.
            *lock(&waiting) = None;
            let _ = events.send(HostEvent::Disconnected);
        });

        Ok((
            Self {
                outgoing,
                replies,
                watches,
                next_id: std::sync::atomic::AtomicU64::new(1),
                principal,
            },
            receiver,
        ))
    }

    /// Follow a task on the host from after `after_sequence`, sending each
    /// update to `sink` until the task ends, the connection does, or `sink`
    /// returns false. Returns the watch id, for `unwatch_task`.
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call).
    pub async fn watch<F>(
        &self,
        task_id: &TaskId,
        after_sequence: u64,
        mut sink: F,
    ) -> Result<u64, ErrorEnvelope>
    where
        F: FnMut(TaskUpdate) -> bool + Send + 'static,
    {
        let reply = self
            .call(
                "watch_task",
                serde_json::json!({ "taskId": task_id, "afterSequence": after_sequence }),
            )
            .await?;
        let watch = reply["watchId"].as_u64().ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::ProtocolError,
                "the host's reply to watch_task named no watch",
                CorrelationId::new("host"),
            )
        })?;
        let mut watches = lock(&self.watches);
        let mut open = true;
        for update in watches.early.remove(&watch).unwrap_or_default() {
            open = open && sink(update);
        }
        if open {
            watches.sinks.insert(watch, Box::new(sink));
        }
        Ok(watch)
    }

    /// Who this device belongs to on the host.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// Run one command on the host, named and shaped as the desktop's Tauri
    /// command, and wait for its reply.
    ///
    /// # Errors
    ///
    /// The host's error, or [`ErrorCode::ProviderUnavailable`] when the
    /// connection is gone.
    pub async fn call(&self, name: &str, args: Value) -> Result<Value, ErrorEnvelope> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        match lock(&self.replies).as_mut() {
            Some(callers) => callers.insert(id, reply),
            None => return Err(gone()),
        };
        let sent = self.outgoing.send(ClientFrame::Command {
            id,
            name: name.to_owned(),
            args,
        });
        if sent.is_err() {
            if let Some(callers) = lock(&self.replies).as_mut() {
                callers.remove(&id);
            }
            return Err(gone());
        }
        answer.await.unwrap_or_else(|_| Err(gone()))
    }
}

fn gone() -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::ProviderUnavailable,
        "the host is not connected",
        CorrelationId::new("host"),
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_native_tls::TlsStream<tokio::net::TcpStream>>;

async fn send(socket: &mut Socket, frame: &ClientFrame) -> Result<(), ClientError> {
    let text = serde_json::to_string(frame).map_err(|_| ClientError::Closed)?;
    socket
        .send(Message::text(text))
        .await
        .map_err(|_| ClientError::Closed)
}

async fn next(socket: &mut Socket) -> Option<ServerFrame> {
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).ok();
        }
    }
    None
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn an_invite_link_is_read_and_anything_else_refused() {
        let hex = "ab".repeat(32);
        let link = PairLink::parse(&format!(
            "tramontana://pair?host=nas.local:7443&fingerprint={hex}&code={hex}"
        ))
        .unwrap();
        assert_eq!(link.address, "nas.local:7443");
        assert_eq!(link.fingerprint, hex);
        assert_eq!(link.code.expose(), hex);

        for bad in [
            format!("https://pair?host=a:1&fingerprint={hex}&code={hex}"),
            format!("tramontana://pair?host=a:1&fingerprint=short&code={hex}"),
            format!("tramontana://pair?host=a:1&fingerprint={hex}"),
            format!("tramontana://pair?host=a:1/evil&fingerprint={hex}&code={hex}"),
            format!("tramontana://pair?host=a:1&fingerprint={hex}&code={hex}&extra=1"),
        ] {
            assert_eq!(PairLink::parse(&bad), None, "{bad}");
        }
    }
}
