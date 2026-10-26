//! Serving paired desktops (ADR 0074).
//!
//! One connection per desktop: TLS, then a WebSocket, then a first frame
//! that names a paired device or spends an invite. Everything the host sends
//! goes through one bounded queue per connection. A desktop that lets it
//! fill is disconnected and reconnects from the last sequence it saw — which
//! loses nothing (spike SP0-A) and keeps a slow network from costing the
//! host memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    ApprovalDecisionKind, ApprovalRequest, CorrelationId, ErrorCode, ErrorEnvelope, TaskId,
};
use tramontana_core::ports::{Approver, PermissionError};
use tramontana_core::services::{TaskService, forward_task_updates};

use crate::devices::{Devices, PairError};
use crate::dispatch::{dispatch, invalid, parse};
use crate::protocol::{ClientFrame, ServerFrame};

/// How many frames may wait for one slow desktop before it is cut off.
const QUEUE: usize = 1_024;

/// Answers every approval question with "nobody can be asked".
///
/// SP1's host has no way to reach a person yet, so a task that needs an
/// approval fails closed, as a headless CLI's does.
// shortcut: replaced by an approver that asks the desktop driving the task,
// in SP1 step 3.
#[derive(Debug, Default)]
pub struct NobodyToAsk;

#[async_trait]
impl Approver for NobodyToAsk {
    async fn ask(
        &self,
        _request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        Err(PermissionError::NoApprover)
    }
}

/// A host: the service, the paired devices, and how to serve them.
pub struct Host {
    service: Arc<TaskService>,
    devices: Arc<Devices>,
    queue: usize,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("queue", &self.queue)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Watch {
    task_id: TaskId,
    #[serde(default)]
    after_sequence: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Unwatch {
    watch_id: u64,
}

impl Host {
    /// A host over `service`, admitting the devices in `devices`.
    #[must_use]
    pub fn new(service: Arc<TaskService>, devices: Arc<Devices>) -> Self {
        Self {
            service,
            devices,
            queue: QUEUE,
        }
    }

    /// Override the per-connection queue, so a test can make a slow desktop.
    #[must_use]
    pub fn with_queue(mut self, queue: usize) -> Self {
        self.queue = queue.max(1);
        self
    }

    /// Accept desktops on `listener` until the task is dropped.
    ///
    /// # Errors
    ///
    /// When the listener itself fails; one bad connection never ends it.
    pub async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        acceptor: tokio_native_tls::TlsAcceptor,
    ) -> std::io::Result<()> {
        let acceptor = Arc::new(acceptor);
        loop {
            let (tcp, _) = listener.accept().await?;
            let host = Arc::clone(&self);
            let acceptor = Arc::clone(&acceptor);
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let Ok(socket) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                host.connection(socket).await;
            });
        }
    }

    async fn connection<S>(&self, socket: tokio_tungstenite::WebSocketStream<S>)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut source) = socket.split();
        let (outgoing, mut pending) = mpsc::channel::<ServerFrame>(self.queue);
        let closing = CancellationToken::new();

        // The one writer. It stops when told to close or when every sender
        // is gone, and closes the socket either way.
        let writer_closing = closing.clone();
        let writer = tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    frame = pending.recv() => frame,
                    () = writer_closing.cancelled() => None,
                };
                let Some(frame) = frame else { break };
                let Ok(text) = serde_json::to_string(&frame) else {
                    break;
                };
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let send = |frame: ServerFrame| send_or_close(&outgoing, &closing, frame);

        // First frame: a paired device, or an invite being spent.
        let first = next_frame(&mut source).await;
        let admitted = match first {
            Some(ClientFrame::Hello {
                schema_version,
                token,
            }) if schema_version == tramontana_core::SCHEMA_VERSION => self
                .devices
                .authenticate(&token)
                .map(|principal| ServerFrame::Welcome {
                    schema_version,
                    principal,
                }),
            Some(ClientFrame::Pair {
                schema_version,
                code,
                device_name,
            }) if schema_version == tramontana_core::SCHEMA_VERSION => {
                match self.devices.redeem(&code, &device_name, now()) {
                    Ok((principal, token)) => Some(ServerFrame::Paired {
                        schema_version,
                        principal,
                        token,
                    }),
                    Err(PairError::InvalidInvite | PairError::Unsaved) => None,
                }
            }
            _ => None,
        };
        let Some(admitted) = admitted else {
            // One answer for every refusal: wrong version, unknown device,
            // spent invite, or no greeting at all.
            send(ServerFrame::Refused {
                error: ErrorEnvelope::new(
                    ErrorCode::PermissionDenied,
                    "this device is not admitted to this host",
                    CorrelationId::new("host"),
                ),
            });
            drop(outgoing);
            let _ = writer.await;
            return;
        };
        send(admitted);

        let mut watches: HashMap<u64, CancellationToken> = HashMap::new();
        let mut next_watch = 0_u64;
        loop {
            let frame = tokio::select! {
                frame = next_frame(&mut source) => frame,
                () = closing.cancelled() => None,
            };
            let Some(frame) = frame else { break };
            let ClientFrame::Command { id, name, args } = frame else {
                // A frame with no id is answered as id 0.
                send(reply(
                    0,
                    Err(invalid("only commands may follow the greeting".to_owned())),
                ));
                continue;
            };
            let result = match name.as_str() {
                "watch_task" => {
                    self.watch(args, &mut next_watch, &mut watches, &outgoing, &closing)
                        .await
                }
                "unwatch_task" => parse::<Unwatch>(&name, args).map(|Unwatch { watch_id }| {
                    if let Some(stop) = watches.remove(&watch_id) {
                        stop.cancel();
                    }
                    serde_json::Value::Null
                }),
                _ => dispatch(&self.service, &name, args).await,
            };
            if !send(reply(id, result)) {
                break;
            }
        }
        closing.cancel();
        drop(outgoing);
        let _ = writer.await;
    }

    async fn watch(
        &self,
        args: serde_json::Value,
        next_watch: &mut u64,
        watches: &mut HashMap<u64, CancellationToken>,
        outgoing: &mpsc::Sender<ServerFrame>,
        closing: &CancellationToken,
    ) -> Result<serde_json::Value, ErrorEnvelope> {
        let Watch {
            task_id,
            after_sequence,
        } = parse("watch_task", args)?;
        let (replayed, updates) = self
            .service
            .subscribe_task_events(&task_id, after_sequence)
            .await?;
        *next_watch += 1;
        let watch = *next_watch;
        let stop = closing.child_token();
        watches.insert(watch, stop.clone());
        let outgoing = outgoing.clone();
        let closing = closing.clone();
        let service = Arc::clone(&self.service);
        tokio::spawn(async move {
            tokio::select! {
                () = stop.cancelled() => {}
                () = forward_task_updates(
                    service, task_id, after_sequence, replayed, updates,
                    move |update| send_or_close(
                        &outgoing,
                        &closing,
                        ServerFrame::Update { watch, update: Box::new(update) },
                    ),
                ) => {}
            }
        });
        Ok(serde_json::json!({ "watchId": watch }))
    }
}

/// Queue a frame, or close the connection if the desktop has fallen so far
/// behind that the queue is full. Returns whether the frame was queued.
fn send_or_close(
    outgoing: &mpsc::Sender<ServerFrame>,
    closing: &CancellationToken,
    frame: ServerFrame,
) -> bool {
    match outgoing.try_send(frame) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            closing.cancel();
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn reply(id: u64, result: Result<serde_json::Value, ErrorEnvelope>) -> ServerFrame {
    match result {
        Ok(value) => ServerFrame::Reply {
            id,
            result: Some(value),
            error: None,
        },
        Err(error) => ServerFrame::Reply {
            id,
            result: None,
            error: Some(error),
        },
    }
}

/// The next frame the desktop sent, or `None` when it closed or sent
/// something that is not a frame. Pings and pongs are the library's.
async fn next_frame<S>(
    source: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
) -> Option<ClientFrame>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match source.next().await? {
            Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).ok(),
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(_) | Err(_) => return None,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
