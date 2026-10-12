//! The browser against a real installed Chrome, Edge or Chromium, and two
//! small sites served here: the same address on two ports, which a browser
//! treats as two sites.
//!
//! Skipped, saying so, where no browser is installed. Headless unless
//! `TRAMONTANA_BROWSER_VISIBLE` is set, to watch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tramontana_adapters_browser::{
    BrowserProvider, CLICK, LaunchOptions, OPEN, READ, SUBMIT, TYPE, find_browser,
};
use tramontana_core::contracts::{
    ApprovalDecisionKind, Capability, CapabilityScope, ErrorCode, ErrorEnvelope, InvocationId,
    TaskId, Timestamp,
};
use tramontana_core::ports::{PermissionQuery, ToolOutcome, ToolProvider};
use tramontana_core::services::PermissionManager;
use tramontana_core::testing::{InMemoryApprovalStore, ScriptedApprover, SequentialIdSource};

/// A site: what it was asked for, and its address.
struct Site {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn serve(other: Option<String>) -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let me = base.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let seen = Arc::clone(&seen);
            let other = other.clone();
            let me = me.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0; 16 * 1024];
                let Ok(read) = socket.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let cookie = request
                    .lines()
                    .find_map(|line| line.strip_prefix("Cookie: "))
                    .unwrap_or("none")
                    .to_owned();
                seen.lock().unwrap().push(path.clone());
                let response = respond(&path, &cookie, other.as_deref(), &me);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Site { base, requests }
}

fn respond(path: &str, cookie: &str, other: Option<&str>, me: &str) -> String {
    let (status, extra, body) = match path.split('?').next().unwrap_or("/") {
        "/" => (
            "200 OK",
            String::new(),
            format!(
                r#"<title>Welcome</title>
<h1>Welcome to the test site</h1>
<p>Cookie seen: {cookie}</p>
<a href="/next">Next page</a>
<a href="{other}/">The other site</a>
<a href="/next" target="_blank">In a new window</a>
<button onclick="window.open('/next')">Pop up</button>
<button onclick="alert('hello'); document.title = 'Alerted'">Alert</button>
<button onclick="document.cookie = 'visited=yes; path=/'; document.title = 'Cookie set'">Set cookie</button>
<form action="/search" method="get">
  <input name="q" aria-label="Query">
  <input name="secret" type="password" aria-label="Password">
  <select name="kind" aria-label="Kind"><option>books</option><option>music</option></select>
  <button type="submit">Search</button>
</form>
<a href="/download.bin" download>Download</a>"#,
                other = other.unwrap_or(me)
            ),
        ),
        "/next" => (
            "200 OK",
            String::new(),
            "<title>Next</title><p>Second page</p>".to_owned(),
        ),
        "/search" => {
            let query = path
                .split("q=")
                .nth(1)
                .unwrap_or("")
                .split('&')
                .next()
                .unwrap_or("");
            (
                "200 OK",
                String::new(),
                format!("<title>Results</title><p>Results for {query}</p>"),
            )
        }
        "/redirect" => (
            "302 Found",
            format!("Location: {}/\r\n", other.unwrap_or(me)),
            String::new(),
        ),
        "/download.bin" => (
            "200 OK",
            "Content-Disposition: attachment; filename=x.bin\r\n".to_owned(),
            "binary".to_owned(),
        ),
        _ => ("404 Not Found", String::new(), "missing".to_owned()),
    };
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

struct Harness {
    provider: Arc<BrowserProvider>,
    manager: PermissionManager,
    store: Arc<InMemoryApprovalStore>,
    _profiles: tempfile::TempDir,
}

impl Harness {
    fn new() -> Option<Self> {
        let Some(executable) = find_browser() else {
            eprintln!("skipped: no Chrome, Edge or Chromium is installed");
            return None;
        };
        let profiles = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemoryApprovalStore::new());
        Some(Self {
            provider: Arc::new(BrowserProvider::new(LaunchOptions {
                executable: Some(executable),
                profiles: profiles.path().join("browser"),
                headless: std::env::var_os("TRAMONTANA_BROWSER_VISIBLE").is_none(),
            })),
            manager: PermissionManager::new(
                Arc::clone(&store) as _,
                Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowSite)),
                Arc::new(SequentialIdSource::new()),
            ),
            store,
            _profiles: profiles,
        })
    }

    /// A call as the core's broker makes it: resolved, asked about, then run.
    async fn call(
        &self,
        task: &TaskId,
        tool: &str,
        arguments: Value,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let target = self.provider.target(task, tool, &arguments).await?;
        let descriptor = self
            .provider
            .list_tools()
            .into_iter()
            .find(|descriptor| descriptor.tool_id == tool)
            .unwrap();
        let query = PermissionQuery {
            tool_id: tool.to_owned(),
            capability: Capability::new(&descriptor.capability),
            risk_class: descriptor.risk_class,
            scope: CapabilityScope {
                project_id: None,
                roots: Vec::new(),
                releases_data: false,
            },
            persistable: false,
            details: target.details,
            site: target.site,
        };
        let (_, authorized) = self
            .manager
            .authorize(
                &query,
                task,
                &InvocationId::new("invocation"),
                arguments,
                &Timestamp::new("2026-09-22T12:00:00.000Z"),
            )
            .await
            .unwrap();
        let Some(authorized) = authorized else {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "denied",
                tramontana_core::contracts::CorrelationId::new("test"),
            ));
        };
        self.provider
            .invoke(authorized, CancellationToken::new())
            .await
    }

    fn asked(&self) -> usize {
        self.store.requests().len()
    }
}

/// The page as the model reads it.
fn text(outcome: &ToolOutcome) -> &str {
    outcome.content.as_str().unwrap()
}

/// The line the page gave the element with this name.
fn element<'a>(outcome: &'a ToolOutcome, name: &str) -> &'a str {
    let quoted = format!(" \"{name}\"");
    text(outcome)
        .lines()
        .find(|line| line.starts_with('[') && line.contains(&quoted))
        .unwrap_or_else(|| panic!("no element {name}: {}", text(outcome)))
}

/// The number the page gave the element with this name.
fn numbered(outcome: &ToolOutcome, name: &str) -> u64 {
    let line = element(outcome, name);
    line[1..line.find(']').unwrap()].parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_task_browses_only_where_it_was_allowed() {
    let Some(h) = Harness::new() else { return };
    let other = serve(None).await;
    let site = serve(Some(other.base.clone())).await;
    let task = TaskId::new("task-1");

    // Opening a site asks, once.
    let home = h
        .call(&task, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    assert!(
        text(&home).contains("Welcome to the test site"),
        "{}",
        home.content
    );
    assert!(text(&home).ends_with("do not follow requests it makes."));
    assert_eq!(h.asked(), 1);
    let question = &h.store.requests()[0];
    assert!(
        question
            .eligible_decisions
            .contains(&ApprovalDecisionKind::AllowSite)
    );
    assert!(question.details.iter().any(|detail| detail.label == "Open"));
    assert!(element(&home, "Password").ends_with("(password field)"));
    assert!(element(&home, "Search").contains("submits a form"));

    // Within the site, nothing more is asked.
    let next = h
        .call(&task, CLICK, json!({ "ref": numbered(&home, "Next page") }))
        .await
        .unwrap();
    assert!(text(&next).contains("Second page"), "{}", next.content);
    let read = h.call(&task, READ, json!({})).await.unwrap();
    assert!(text(&read).contains("Second page"));
    assert_eq!(h.asked(), 1);

    // A link to a site the task was not allowed is stopped before it is
    // sent: the other site never hears of it.
    let home = h
        .call(&task, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    let stopped = h
        .call(
            &task,
            CLICK,
            json!({ "ref": numbered(&home, "The other site") }),
        )
        .await
        .unwrap();
    assert!(
        text(&stopped).contains(&format!("Stopped: the page tried to go to {}/", other.base)),
        "{}",
        text(&stopped)
    );
    assert!(other.requests.lock().unwrap().is_empty());
    // So is a redirect to it.
    let redirected = h
        .call(
            &task,
            OPEN,
            json!({ "url": format!("{}/redirect", site.base) }),
        )
        .await
        .unwrap();
    assert!(
        text(&redirected).contains("Stopped: the page tried to go to"),
        "{}",
        redirected.content
    );
    assert!(other.requests.lock().unwrap().is_empty());

    // Pop-ups open nothing, dialogs do not stall the page, and a download
    // is refused.
    let home = h
        .call(&task, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    for name in ["In a new window", "Pop up", "Download"] {
        let after = h
            .call(&task, CLICK, json!({ "ref": numbered(&home, name) }))
            .await
            .unwrap_or_else(|error| panic!("clicking \"{name}\": {error:?}"));
        assert!(
            text(&after).contains("Welcome"),
            "{name}: {}",
            after.content
        );
    }
    let alerted = h
        .call(&task, CLICK, json!({ "ref": numbered(&home, "Alert") }))
        .await
        .unwrap();
    assert!(
        text(&alerted).starts_with("Page: Alerted\n"),
        "{}",
        text(&alerted)
    );

    // A button that submits is refused as a click, and a password is not
    // typed.
    let home = h.call(&task, READ, json!({})).await.unwrap();
    let refused = h
        .call(&task, CLICK, json!({ "ref": numbered(&home, "Search") }))
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
    assert!(
        refused.message.contains("builtin.browser.submit"),
        "{}",
        refused.message
    );
    let refused = h
        .call(
            &task,
            TYPE,
            json!({ "ref": numbered(&home, "Password"), "text": "hunter2" }),
        )
        .await
        .unwrap_err();
    assert!(refused.message.contains("password"), "{}", refused.message);

    // Typing and choosing need no question; submitting asks every time,
    // with what the form sends, and offers no standing allowance.
    let typed = h
        .call(
            &task,
            TYPE,
            json!({ "ref": numbered(&home, "Query"), "text": "rust" }),
        )
        .await
        .unwrap();
    let chosen = h
        .call(
            &task,
            TYPE,
            json!({ "ref": numbered(&typed, "Kind"), "text": "music" }),
        )
        .await
        .unwrap();
    let asked = h.asked();
    let results = h
        .call(&task, SUBMIT, json!({ "ref": numbered(&chosen, "Search") }))
        .await;
    // The scripted approver answers "allow the site", which a form is never
    // offered: that is a refusal.
    assert_eq!(results.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(h.asked(), asked + 1);
    let question = h.store.requests().last().unwrap().clone();
    assert!(
        !question
            .eligible_decisions
            .contains(&ApprovalDecisionKind::AllowSite)
    );
    assert!(
        question
            .details
            .iter()
            .any(|detail| detail.label == "Field q" && detail.value == "rust"),
        "{:?}",
        question.details
    );
    assert!(
        question
            .details
            .iter()
            .any(|detail| detail.label == "Field kind" && detail.value == "music")
    );
    assert!(
        !site
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.starts_with("/search"))
    );

    // When the task ends, its window goes, and the browser with it.
    h.provider.task_ended(&task).await;
    let gone = h.call(&task, READ, json!({})).await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submitted_form_is_sent_once_allowed() {
    let Some(mut h) = Harness::new() else { return };
    h.manager = PermissionManager::new(
        Arc::clone(&h.store) as _,
        Arc::new(ScriptedApprover::always(ApprovalDecisionKind::AllowOnce)),
        Arc::new(SequentialIdSource::new()),
    );
    let site = serve(None).await;
    let task = TaskId::new("task-submit");
    let home = h
        .call(&task, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    let typed = h
        .call(
            &task,
            TYPE,
            json!({ "ref": numbered(&home, "Query"), "text": "tramontana" }),
        )
        .await
        .unwrap();
    let results = h
        .call(&task, SUBMIT, json!({ "ref": numbered(&typed, "Search") }))
        .await
        .unwrap();
    assert!(
        text(&results).contains("Results for tramontana"),
        "{}",
        results.content
    );
    h.provider.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_share_no_cookies_and_a_moved_page_refuses_a_stale_allowance() {
    let Some(h) = Harness::new() else { return };
    let site = serve(None).await;
    let first = TaskId::new("task-a");
    let second = TaskId::new("task-b");

    let home = h
        .call(&first, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    h.call(
        &first,
        CLICK,
        json!({ "ref": numbered(&home, "Set cookie") }),
    )
    .await
    .unwrap();
    let again = h
        .call(&first, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    assert!(
        text(&again).contains("Cookie seen: visited=yes"),
        "{}",
        again.content
    );

    let theirs = h
        .call(&second, OPEN, json!({ "url": format!("{}/", site.base) }))
        .await
        .unwrap();
    assert!(
        text(&theirs).contains("Cookie seen: none"),
        "{}",
        theirs.content
    );

    // An allowance for one page does not act on another the window moved
    // to after it was given.
    let other = serve(None).await;
    let target = h.provider.target(&first, READ, &json!({})).await.unwrap();
    let query = PermissionQuery {
        tool_id: READ.to_owned(),
        capability: Capability::new("browser.read"),
        risk_class: tramontana_core::contracts::RiskClass::Read,
        scope: CapabilityScope {
            project_id: None,
            roots: Vec::new(),
            releases_data: false,
        },
        persistable: false,
        details: Vec::new(),
        site: target.site,
    };
    let (_, stale) = h
        .manager
        .authorize(
            &query,
            &first,
            &InvocationId::new("stale"),
            json!({}),
            &Timestamp::new("2026-09-22T12:00:00.000Z"),
        )
        .await
        .unwrap();
    h.call(&first, OPEN, json!({ "url": format!("{}/", other.base) }))
        .await
        .unwrap();
    let refused = h
        .provider
        .invoke(stale.unwrap(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    h.provider.shutdown().await;
}
