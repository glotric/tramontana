//! A model endpoint that exists for the length of one test.
//!
//! Hand-written rather than a framework dependency, for two reasons. The
//! protocol under test is narrow — one POST, one streamed response — and the
//! interesting cases are *misbehavior*: a connection that opens and then says
//! nothing, a body that stops mid-object, a status with no body at all. Those
//! are awkward to arrange through a framework designed to behave correctly.
//!
//! It also records what it received, which is how the tests check that a
//! credential travelled in a header and appeared in no URL.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a panic is the failure report, and a helper unused by one test file is still part of the harness"
)]

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

/// What the endpoint should do when asked.
#[derive(Debug, Clone)]
pub enum Behavior {
    /// Stream these chunks as content deltas, then finish cleanly.
    Streams(Vec<String>),
    /// Answer with this status and no useful body.
    Status(u16),
    /// Accept the connection, send headers, then never send content.
    Silent,
    /// Send some content and then close mid-object without finishing.
    Truncated,
    /// Send exactly these raw event payloads, verbatim.
    Raw(Vec<String>),
}

/// What one request carried.
#[derive(Debug, Clone, Default)]
pub struct Received {
    /// The request line, e.g. `POST /v1/chat/completions HTTP/1.1`.
    pub request_line: String,
    /// Header lines, lowercased names.
    pub headers: Vec<(String, String)>,
    /// The body, as sent.
    pub body: String,
}

impl Received {
    /// The value of a header, if it was sent.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A running endpoint. Dropping it stops the listener.
pub struct FakeEndpoint {
    pub base_url: String,
    received: Arc<Mutex<Vec<Received>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeEndpoint {
    /// Start an endpoint on a free loopback port.
    pub async fn start(behavior: Behavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));

        let task = tokio::spawn({
            let received = Arc::clone(&received);
            async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let behavior = behavior.clone();
                    let received = Arc::clone(&received);
                    tokio::spawn(async move {
                        if let Some(request) = read_request(&mut socket).await {
                            received.lock().unwrap().push(request);
                        }
                        respond(&mut socket, &behavior).await;
                    });
                }
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            received,
            task,
        }
    }

    /// Everything this endpoint was sent.
    #[must_use]
    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

/// Read one HTTP/1.1 request, headers and body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Received> {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];

    // Headers first: read until the blank line that ends them.
    let header_end = loop {
        if let Some(position) = find(&raw, b"\r\n\r\n") {
            break position;
        }
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..read]);
    };

    let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_lowercase(), value.trim().to_owned()))
        })
        .collect();

    // Then exactly as many body bytes as were declared.
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = raw[header_end + 4..].to_vec();
    while body.len() < length {
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }

    Some(Received {
        request_line,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Write the response the behavior calls for.
async fn respond(socket: &mut tokio::net::TcpStream, behavior: &Behavior) {
    if let Behavior::Status(status) = behavior {
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 {status} Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.shutdown().await;
        return;
    }

    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let _ = socket.flush().await;

    match behavior {
        Behavior::Silent => {
            // Headers and then nothing, forever. The client's deadline or its
            // cancellation has to be what ends this.
            std::future::pending::<()>().await;
        }
        Behavior::Truncated => {
            let _ = socket
                .write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"par")
                .await;
            let _ = socket.shutdown().await;
        }
        Behavior::Raw(payloads) => {
            for payload in payloads {
                let _ = socket
                    .write_all(format!("data: {payload}\n\n").as_bytes())
                    .await;
                let _ = socket.flush().await;
            }
            let _ = socket.shutdown().await;
        }
        Behavior::Streams(chunks) => {
            for chunk in chunks {
                let frame = serde_json::json!({
                    "choices": [{ "delta": { "content": chunk } }]
                });
                if socket
                    .write_all(format!("data: {frame}\n\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = socket.flush().await;
            }
            let finish = serde_json::json!({
                "choices": [{ "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 11, "completion_tokens": 3 }
            });
            let _ = socket
                .write_all(format!("data: {finish}\n\ndata: [DONE]\n\n").as_bytes())
                .await;
            let _ = socket.shutdown().await;
        }
        Behavior::Status(_) => unreachable!("handled above"),
    }
}
