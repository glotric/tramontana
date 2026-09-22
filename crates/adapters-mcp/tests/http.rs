//! A remote App over Streamable HTTP (ADR 0050), against a server written
//! here by hand so it can answer as JSON or as an event stream, and
//! misbehave in ways a real SDK would not let it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tramontana_adapters_mcp::{AppHealth, McpApp, McpLimits, McpRemoteSpec, StartError};
use tramontana_core::contracts::{
    ApprovalDecisionKind, Capability, CapabilityScope, InvocationId, ProjectId, TaskId, Timestamp,
};
use tramontana_core::ports::{ApprovalStore, PermissionQuery, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

/// How the fake server behaves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ok,
    /// Every request is answered with a redirect to another server.
    Redirect,
    /// The tool list is valid, but larger than the limit.
    Huge,
}

/// One request as the server saw it.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    authorization: Option<String>,
    session: Option<String>,
    protocol: Option<String>,
}

struct Server {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

async fn serve(mode: Mode, token: &'static str, elsewhere: Option<String>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            let elsewhere = elsewhere.clone();
            tokio::spawn(async move {
                let Some((head, body)) = read_request(&mut socket).await else {
                    return;
                };
                let header = |name: &str| {
                    head.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case(name)
                            .then(|| value.trim().to_owned())
                    })
                };
                let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let method = message["method"].as_str().unwrap_or("").to_owned();
                log.lock().unwrap().push(Seen {
                    method: if head.starts_with("DELETE") {
                        "DELETE".to_owned()
                    } else {
                        method.clone()
                    },
                    authorization: header("Authorization"),
                    session: header("Mcp-Session-Id"),
                    protocol: header("MCP-Protocol-Version"),
                });
                let response = respond(mode, token, elsewhere.as_deref(), &header, &message);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Server { url, seen }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let split = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..split]).to_string();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = buffer[split + 4..].to_vec();
    while body.len() < length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some((head, body))
}

fn respond(
    mode: Mode,
    token: &str,
    elsewhere: Option<&str>,
    header: &dyn Fn(&str) -> Option<String>,
    message: &Value,
) -> String {
    let reply = |status: &str, extra: &str, kind: &str, body: String| {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    if mode == Mode::Redirect {
        return reply(
            "307 Temporary Redirect",
            &format!(
                "Location: {}\r\n",
                elsewhere.unwrap_or("http://127.0.0.1:9/")
            ),
            "text/plain",
            String::new(),
        );
    }
    if header("Authorization").as_deref() != Some(&format!("Bearer {token}")) {
        return reply("401 Unauthorized", "", "text/plain", "no".to_owned());
    }
    let id = message["id"].clone();
    match message["method"].as_str().unwrap_or("") {
        "initialize" => reply(
            "200 OK",
            "Mcp-Session-Id: session-7\r\n",
            "application/json",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fake-http", "version": "1" }
                }
            })
            .to_string(),
        ),
        "tools/list" if mode == Mode::Huge => {
            // A valid answer, only too large: 300 KB of description.
            let answer = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [{
                    "name": "lookup",
                    "description": "x".repeat(300 * 1024),
                    "inputSchema": { "type": "object" },
                    "annotations": { "readOnlyHint": true }
                }] }
            });
            reply(
                "200 OK",
                "",
                "text/event-stream",
                format!("data: {answer}\n\n"),
            )
        }
        "tools/list" => {
            let answer = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [{
                    "name": "lookup",
                    "description": "Looks a word up.",
                    "inputSchema": { "type": "object", "properties": { "word": { "type": "string" } } },
                    "annotations": { "readOnlyHint": true }
                }] }
            });
            // An event stream: a comment, an unrelated notification, then
            // the answer.
            let body = format!(
                ": hello\n\nevent: message\ndata: {}\n\ndata: {}\n\n",
                json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": { "level": "info", "data": "listing" } }),
                answer
            );
            reply("200 OK", "", "text/event-stream", body)
        }
        "tools/call" => reply(
            "200 OK",
            "",
            "application/json",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "content": [{ "type": "text", "text": format!(
                    "found: {}", message["params"]["arguments"]["word"].as_str().unwrap_or("")
                ) }] }
            })
            .to_string(),
        ),
        _ => reply("202 Accepted", "", "text/plain", String::new()),
    }
}

fn limits() -> McpLimits {
    McpLimits {
        start_timeout: Duration::from_secs(5),
        call_timeout: Duration::from_secs(5),
        ..McpLimits::default()
    }
}

fn spec(url: &str, token: Option<&str>) -> McpRemoteSpec {
    McpRemoteSpec {
        app_id: "remote".to_owned(),
        display_name: "Remote".to_owned(),
        url: url.to_owned(),
        token: token.map(str::to_owned),
        limits: limits(),
    }
}

#[tokio::test]
async fn a_remote_app_lists_and_answers_with_its_token_and_session_on_every_request() {
    let server = serve(Mode::Ok, "t0ken", None).await;
    let app = McpApp::connect(spec(&server.url, Some("t0ken")))
        .await
        .unwrap();
    assert_eq!(app.protocol_version(), "2025-06-18");
    assert_eq!(app.health().await, AppHealth::Running);
    let tools = app.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_id, "mcp.remote.lookup");
    let origin = server.url.trim_end_matches("/mcp").to_owned();
    assert_eq!(app.remote(), Some(origin.as_str()));

    // Every call names where it goes, and is never a standing allowance.
    let target = app
        .target(&TaskId::new("task-1"), "mcp.remote.lookup", &json!({}))
        .await
        .unwrap();
    let site = target.site.unwrap();
    assert_eq!(site.origin, origin);
    assert!(!site.grantable);
    assert!(
        target
            .details
            .iter()
            .any(|detail| detail.label == "Sends to" && detail.value == origin)
    );

    let manager = PermissionManager::new(
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        Arc::new(SequentialIdSource::new()),
    );
    let (_, authorized) = manager
        .authorize(
            &PermissionQuery {
                tool_id: tools[0].tool_id.clone(),
                capability: Capability::new(&tools[0].capability),
                risk_class: tools[0].risk_class,
                scope: CapabilityScope::project(ProjectId::new("p"), Vec::new()),
                persistable: false,
                details: target.details,
                site: Some(site),
            },
            &TaskId::new("task-1"),
            &InvocationId::new("invocation-1"),
            json!({ "word": "bora" }),
            &Timestamp::new("2026-09-22T12:00:00.000Z"),
        )
        .await
        .unwrap();
    let outcome = app
        .invoke(authorized.unwrap(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.content["text"], "found: bora");

    app.shutdown().await;
    let seen = server.seen.lock().unwrap().clone();
    let methods: Vec<&str> = seen.iter().map(|seen| seen.method.as_str()).collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
            "DELETE"
        ]
    );
    assert!(
        seen.iter()
            .all(|seen| seen.authorization.as_deref() == Some("Bearer t0ken"))
    );
    // After initialize, the session and the protocol it answered with.
    for later in &seen[1..] {
        assert_eq!(later.session.as_deref(), Some("session-7"), "{later:?}");
    }
    for later in &seen[2..4] {
        assert_eq!(later.protocol.as_deref(), Some("2025-06-18"), "{later:?}");
    }
}

#[tokio::test]
async fn a_refused_token_says_so() {
    let server = serve(Mode::Ok, "right", None).await;
    let refused = McpApp::connect(spec(&server.url, Some("wrong")))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, StartError::Initialize(why) if why.contains("refused the token")),
        "{refused}"
    );
}

#[tokio::test]
async fn a_redirect_is_not_followed_so_the_token_goes_nowhere_else() {
    let elsewhere = serve(Mode::Ok, "t0ken", None).await;
    let server = serve(Mode::Redirect, "t0ken", Some(elsewhere.url.clone())).await;
    let refused = McpApp::connect(spec(&server.url, Some("t0ken")))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, StartError::Initialize(why) if why.contains("redirect")),
        "{refused}"
    );
    assert!(elsewhere.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_message_past_the_limit_is_not_read() {
    let server = serve(Mode::Huge, "t0ken", None).await;
    let mut small = spec(&server.url, Some("t0ken"));
    small.limits.max_line = 64 * 1024;
    let refused = McpApp::connect(small).await.unwrap_err();
    assert!(
        refused.to_string().contains("larger than the limit"),
        "{refused}"
    );
}

#[tokio::test]
async fn only_https_or_this_computer_and_no_credentials_in_the_address() {
    for (url, why) in [
        ("http://example.com/mcp", "https only"),
        ("ftp://127.0.0.1/mcp", "http or https"),
        ("https://user:pw@example.com/mcp", "name or password"),
        ("not a url", "not a web address"),
    ] {
        let refused = McpApp::connect(spec(url, None)).await.unwrap_err();
        assert!(
            matches!(&refused, StartError::Invalid(message) if message.contains(why)),
            "{url}: {refused}"
        );
    }
}
