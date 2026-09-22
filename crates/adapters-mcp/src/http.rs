//! MCP's Streamable HTTP transport, client side, written here rather than
//! taken from rmcp so that what is read from a remote App is bounded, as
//! every line from a stdio App is (ADR 0050).
//!
//! Each message is POSTed to the App's one address. A reply is a single JSON
//! message, or an event stream of messages, each bounded; a notification's
//! reply is empty. The session the App names at initialization, and the
//! protocol version it answers with, are sent with every later request.
//! Redirects are not followed, so the token goes to the configured address
//! and nowhere else. Server-initiated streams (a GET) are not opened: the
//! App's replies arrive on the requests that asked for them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rmcp::RoleClient;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use tokio::sync::mpsc;

/// Why a message could not be sent.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub(crate) struct HttpError(pub(crate) String);

/// What the transport and the App share: the last HTTP failure, in words.
pub(crate) type LastFailure = Arc<Mutex<Option<String>>>;

struct Inner {
    client: reqwest::Client,
    url: reqwest::Url,
    token: Option<String>,
    session: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    incoming: mpsc::Sender<RxJsonRpcMessage<RoleClient>>,
    max_message: usize,
    max_reply: usize,
    timeout: Duration,
    oversized: Arc<AtomicBool>,
    failure: LastFailure,
}

/// The client end of one remote App's connection.
pub(crate) struct HttpTransport {
    inner: Arc<Inner>,
    incoming: mpsc::Receiver<RxJsonRpcMessage<RoleClient>>,
}

impl HttpTransport {
    #[allow(
        clippy::too_many_arguments,
        reason = "each is one bound or one piece of state the connection needs"
    )]
    pub(crate) fn new(
        url: reqwest::Url,
        token: Option<String>,
        max_message: usize,
        timeout: Duration,
        oversized: Arc<AtomicBool>,
        failure: LastFailure,
    ) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| HttpError(format!("could not build an HTTP client: {error}")))?;
        let (sender, receiver) = mpsc::channel(64);
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                url,
                token,
                session: Mutex::new(None),
                protocol: Mutex::new(None),
                incoming: sender,
                max_message,
                max_reply: max_message.saturating_mul(4),
                timeout,
                oversized,
                failure,
            }),
            incoming: receiver,
        })
    }
}

impl Transport<RoleClient> for HttpTransport {
    type Error = HttpError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let inner = Arc::clone(&self.inner);
        async move {
            let result = tokio::time::timeout(inner.timeout, inner.post(&item))
                .await
                .unwrap_or_else(|_| Err(HttpError("the App did not answer in time".to_owned())));
            if let Err(error) = &result
                && let Ok(mut failure) = inner.failure.lock()
            {
                *failure = Some(error.0.clone());
            }
            result
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.incoming.recv()
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        // Ending the session is a courtesy: the App may not support it.
        let session = self.inner.session.lock().ok().and_then(|s| s.clone());
        if let Some(session) = session {
            let mut request = self
                .inner
                .client
                .delete(self.inner.url.clone())
                .header("Mcp-Session-Id", session);
            if let Some(token) = &self.inner.token {
                request = request.bearer_auth(token);
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), request.send()).await;
        }
        Ok(())
    }
}

impl Inner {
    async fn post(&self, item: &TxJsonRpcMessage<RoleClient>) -> Result<(), HttpError> {
        let body = serde_json::to_vec(item)
            .map_err(|_| HttpError("a message could not be written".to_owned()))?;
        let mut request = self
            .client
            .post(self.url.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(session) = self.session.lock().ok().and_then(|s| s.clone()) {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(protocol) = self.protocol.lock().ok().and_then(|p| p.clone()) {
            request = request.header("MCP-Protocol-Version", protocol);
        }
        let response = request
            .send()
            .await
            .map_err(|_| HttpError("the App could not be reached".to_owned()))?;

        let status = response.status();
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic()))
            && let Ok(mut known) = self.session.lock()
        {
            *known = Some(session.to_owned());
        }
        match status.as_u16() {
            200 => {}
            202 => return Ok(()),
            401 | 403 => {
                return Err(HttpError(
                    "the App refused the token: check the one stored for it".to_owned(),
                ));
            }
            300..=399 => {
                return Err(HttpError(
                    "the App redirected elsewhere; Tramontana does not follow redirects".to_owned(),
                ));
            }
            404 => {
                return Err(HttpError(
                    "the App's address or session was not found".to_owned(),
                ));
            }
            code => return Err(HttpError(format!("the App answered with HTTP {code}"))),
        }
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            self.read_events(response).await
        } else if content_type.starts_with("application/json") {
            let body = self.read_bounded(response, self.max_message).await?;
            self.deliver(&body).await
        } else if content_type.is_empty() {
            Ok(())
        } else {
            Err(HttpError(
                "the App answered with something other than JSON".to_owned(),
            ))
        }
    }

    /// The whole body, if it is no longer than `limit`.
    async fn read_bounded(
        &self,
        response: reqwest::Response,
        limit: usize,
    ) -> Result<Vec<u8>, HttpError> {
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| HttpError("the App's answer broke off".to_owned()))?;
            if body.len() + chunk.len() > limit {
                return Err(self.too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Read an event stream, delivering each message as it completes.
    async fn read_events(&self, response: reqwest::Response) -> Result<(), HttpError> {
        let mut chunks = response.bytes_stream();
        let mut pending = Vec::new();
        let mut data = Vec::new();
        let mut total = 0usize;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| HttpError("the App's answer broke off".to_owned()))?;
            total += chunk.len();
            if total > self.max_reply {
                return Err(self.too_large());
            }
            pending.extend_from_slice(&chunk);
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let line = line.strip_suffix(b"\n").unwrap_or(&line);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.is_empty() {
                    if !data.is_empty() {
                        self.deliver(&data).await?;
                        data.clear();
                    }
                } else if let Some(value) = line.strip_prefix(b"data:") {
                    let value = value.strip_prefix(b" ").unwrap_or(value);
                    if data.len() + value.len() > self.max_message {
                        return Err(self.too_large());
                    }
                    if !data.is_empty() {
                        data.push(b'\n');
                    }
                    data.extend_from_slice(value);
                }
                // `event:`, `id:`, `retry:` and comments carry nothing needed.
            }
            if pending.len() > self.max_message {
                return Err(self.too_large());
            }
        }
        if !data.is_empty() {
            self.deliver(&data).await?;
        }
        Ok(())
    }

    /// Hand one JSON-RPC message, or a batch, to the session.
    async fn deliver(&self, body: &[u8]) -> Result<(), HttpError> {
        let value: serde_json::Value = serde_json::from_slice(body)
            .map_err(|_| HttpError("the App sent something that is not JSON".to_owned()))?;
        let messages = match value {
            serde_json::Value::Array(batch) => batch,
            single => vec![single],
        };
        for message in messages {
            if let Some(version) = message
                .pointer("/result/protocolVersion")
                .and_then(serde_json::Value::as_str)
                .filter(|version| version.len() <= 32)
                && let Ok(mut protocol) = self.protocol.lock()
            {
                *protocol = Some(version.to_owned());
            }
            let parsed: RxJsonRpcMessage<RoleClient> = serde_json::from_value(message)
                .map_err(|_| HttpError("the App sent a message that is not MCP".to_owned()))?;
            self.incoming
                .send(parsed)
                .await
                .map_err(|_| HttpError("the connection closed".to_owned()))?;
        }
        Ok(())
    }

    fn too_large(&self) -> HttpError {
        self.oversized.store(true, Ordering::SeqCst);
        HttpError("the App sent a message larger than the limit".to_owned())
    }
}
