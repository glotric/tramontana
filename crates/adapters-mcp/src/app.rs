//! One App: a server process Tramontana owns, and the tools it offered.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, CancelledNotificationParam,
    ClientRequest, ContentBlock, PaginatedRequestParams, ServerResult,
};
use rmcp::service::{PeerRequestOptions, RunningService};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{Peer, RoleClient, ServiceError, ServiceExt};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, ToolDescriptor,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};

use crate::bounded::{BoundedLines, StderrTail};
use crate::descriptor::{AppTool, Refusal, discover, shown};

/// How much an App may send, and how long it may take.
#[derive(Debug, Clone, Copy)]
pub struct McpLimits {
    /// The longest single message, in bytes. Past it the connection closes.
    pub max_line: usize,
    /// The most tools registered from one App.
    pub max_tools: usize,
    /// The most pages of tools read before listing stops.
    pub max_pages: usize,
    /// How long starting, and each page of the tool list, may take.
    pub start_timeout: Duration,
    /// How long one call may take.
    pub call_timeout: Duration,
    /// The most result text kept from one call, in bytes.
    pub max_result_text: usize,
    /// How much of the App's stderr is kept for diagnostics, in bytes.
    pub stderr_tail: usize,
}

impl Default for McpLimits {
    fn default() -> Self {
        Self {
            max_line: 1024 * 1024,
            max_tools: 64,
            max_pages: 8,
            start_timeout: Duration::from_secs(10),
            call_timeout: Duration::from_secs(60),
            max_result_text: 64 * 1024,
            stderr_tail: 8 * 1024,
        }
    }
}

/// What to start, and exactly what it is given.
///
/// Nothing is inherited. The environment is cleared and holds only
/// `environment`, in which a secret reference has already been resolved by
/// the caller; `PATH` is not searched, so `program` is a path to the file.
#[derive(Clone)]
pub struct McpServerSpec {
    /// The App's identifier, which namespaces its tools as `mcp.<app_id>.`.
    pub app_id: String,
    /// What to call it.
    pub display_name: String,
    /// The server executable.
    pub program: PathBuf,
    /// Its arguments.
    pub arguments: Vec<String>,
    /// Its working directory.
    pub working_directory: PathBuf,
    /// Every environment variable it is given, and nothing else.
    pub environment: Vec<(String, String)>,
    /// Its limits.
    pub limits: McpLimits,
}

impl std::fmt::Debug for McpServerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Values can be secrets; names are not.
        f.debug_struct("McpServerSpec")
            .field("app_id", &self.app_id)
            .field("program", &self.program)
            .field("arguments", &self.arguments.len())
            .field("working_directory", &self.working_directory)
            .field(
                "environment",
                &self
                    .environment
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Why an App could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The configuration cannot be run as given.
    Invalid(&'static str),
    /// The program could not be started.
    Spawn(std::io::ErrorKind),
    /// It did not complete MCP's initialize, in time or at all.
    Initialize(String),
    /// It could not list its tools.
    ListTools(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "{why}"),
            Self::Spawn(kind) => write!(f, "the App's program could not be started ({kind})"),
            Self::Initialize(why) => write!(f, "the App did not start: {why}"),
            Self::ListTools(why) => write!(f, "the App did not list its tools: {why}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Whether an App's process is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppHealth {
    /// Running and connected.
    Running,
    /// The connection closed or the process ended.
    Stopped {
        /// Why, in words.
        reason: String,
    },
}

/// The server's process tree, stopped when dropped unless already stopped.
struct OwnedServer {
    child: Box<dyn ChildWrapper>,
    stopped: bool,
}

impl OwnedServer {
    async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        self.stopped = true;
    }
}

impl Drop for OwnedServer {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.start_kill();
        }
    }
}

/// A started App: its connection, its process and the tools it offered.
pub struct McpApp {
    spec: McpServerSpec,
    peer: Peer<RoleClient>,
    service: Mutex<Option<RunningService<RoleClient, ()>>>,
    server: Mutex<OwnedServer>,
    tools: Vec<AppTool>,
    refused: Vec<Refusal>,
    protocol_version: String,
    stderr: StderrTail,
    oversized: Arc<AtomicBool>,
}

impl std::fmt::Debug for McpApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpApp")
            .field("app_id", &self.spec.app_id)
            .field("tools", &self.tools.len())
            .field("refused", &self.refused.len())
            .field("protocol_version", &self.protocol_version)
            .finish_non_exhaustive()
    }
}

impl McpApp {
    /// Start the server, complete MCP's initialize, and list its tools.
    ///
    /// # Errors
    ///
    /// [`StartError`] when the configuration cannot run, the program does
    /// not start, or it does not initialize or list its tools in time. No
    /// process is left running after an error.
    pub async fn start(spec: McpServerSpec) -> Result<Self, StartError> {
        validate(&spec)?;
        let mut command = tokio::process::Command::new(&spec.program);
        command
            .args(&spec.arguments)
            .current_dir(&spec.working_directory)
            .env_clear()
            .envs(spec.environment.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut wrapped = CommandWrap::from(command);
        #[cfg(unix)]
        wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        {
            wrapped.wrap(process_wrap::tokio::JobObject);
            wrapped.wrap(process_wrap::tokio::KillOnDrop);
        }
        let child = wrapped
            .spawn()
            .map_err(|error| StartError::Spawn(error.kind()))?;
        let mut server = OwnedServer {
            child,
            stopped: false,
        };
        let (Some(stdin), Some(stdout)) =
            (server.child.stdin().take(), server.child.stdout().take())
        else {
            server.stop().await;
            return Err(StartError::Invalid("the App's streams could not be opened"));
        };
        let stderr = StderrTail::start(server.child.stderr().take(), spec.limits.stderr_tail);

        let oversized = Arc::new(AtomicBool::new(false));
        let transport = AsyncRwTransport::new_client(
            BoundedLines::new(stdout, spec.limits.max_line, Arc::clone(&oversized)),
            stdin,
        );
        let service =
            match tokio::time::timeout(spec.limits.start_timeout, ().serve(transport)).await {
                Ok(Ok(service)) => service,
                Ok(Err(error)) => {
                    server.stop().await;
                    return Err(StartError::Initialize(shown(&error.to_string(), 200)));
                }
                Err(_) => {
                    server.stop().await;
                    return Err(StartError::Initialize(
                        "it did not answer in time".to_owned(),
                    ));
                }
            };
        let protocol_version = service
            .peer_info()
            .map(|info| info.protocol_version.to_string())
            .unwrap_or_default();
        let peer = service.peer().clone();

        let listed = match list_tools(&peer, &spec.limits).await {
            Ok(listed) => listed,
            Err(error) => {
                let _ = service.cancel().await;
                server.stop().await;
                return Err(StartError::ListTools(error));
            }
        };
        let mut discovered = discover(&spec.app_id, listed);
        if discovered.tools.len() > spec.limits.max_tools {
            for tool in discovered.tools.drain(spec.limits.max_tools..) {
                discovered.refused.push(Refusal {
                    name: shown(&tool.name, crate::descriptor::MAX_NAME),
                    reason: "the App offered more tools than the limit",
                });
            }
        }

        Ok(Self {
            spec,
            peer,
            service: Mutex::new(Some(service)),
            server: Mutex::new(server),
            tools: discovered.tools,
            refused: discovered.refused,
            protocol_version,
            stderr,
            oversized,
        })
    }

    /// The App's identifier.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.spec.app_id
    }

    /// The MCP protocol version the App answered with.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// The tools the App offered that were refused, and why.
    #[must_use]
    pub fn refused(&self) -> &[Refusal] {
        &self.refused
    }

    /// The last of what the App wrote to stderr, redacted.
    #[must_use]
    pub fn diagnostics(&self) -> String {
        shown(&self.stderr.text(), self.spec.limits.stderr_tail)
    }

    /// Whether the App is still running and connected.
    pub async fn health(&self) -> AppHealth {
        if self.oversized.load(Ordering::SeqCst) {
            return AppHealth::Stopped {
                reason: "the App sent a message larger than the limit".to_owned(),
            };
        }
        let mut server = self.server.lock().await;
        match server.child.try_wait() {
            Ok(None) if !server.stopped && !self.peer.is_transport_closed() => AppHealth::Running,
            Ok(Some(status)) => AppHealth::Stopped {
                reason: format!("the App's process ended ({status})"),
            },
            _ => AppHealth::Stopped {
                reason: "the App's connection closed".to_owned(),
            },
        }
    }

    /// Close the connection and stop the process tree.
    pub async fn shutdown(&self) {
        if let Some(service) = self.service.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), service.cancel()).await;
        }
        self.server.lock().await.stop().await;
    }

    fn error(&self, code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
        ErrorEnvelope::new(
            code,
            message,
            CorrelationId::new(format!("mcp.{}", self.spec.app_id)),
        )
    }

    fn call_failed(&self, error: &ServiceError) -> ErrorEnvelope {
        if self.oversized.load(Ordering::SeqCst) {
            return self.error(
                ErrorCode::LimitExceeded,
                "The App sent a message larger than the limit, and was disconnected",
            );
        }
        match error {
            ServiceError::Timeout { .. } => self.error(
                ErrorCode::Timeout,
                "The App did not answer in time; the call was cancelled",
            ),
            ServiceError::TransportClosed | ServiceError::TransportSend(_) => {
                self.error(ErrorCode::ToolFailed, "The App stopped during the call")
            }
            ServiceError::McpError(data) => self.error(
                ErrorCode::ToolFailed,
                format!("The App refused the call: {}", shown(&data.message, 200)),
            ),
            ServiceError::UnexpectedResponse => self.error(
                ErrorCode::ToolFailed,
                "The App answered with something other than a tool result",
            ),
            _ => self.error(ErrorCode::ToolFailed, "The call to the App failed"),
        }
    }

    /// A result as the tool plane records it: text only, redacted and cut.
    fn outcome(
        &self,
        tool: &AppTool,
        result: CallToolResult,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let limit = self.spec.limits.max_result_text;
        let mut text = String::new();
        let mut omitted = 0_usize;
        for block in &result.content {
            match block {
                ContentBlock::Text(content) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&content.text);
                }
                _ => omitted += 1,
            }
        }
        if result.is_error == Some(true) {
            return Err(self.error(
                ErrorCode::ToolFailed,
                format!("The App reported a failure: {}", shown(&text, 200)),
            ));
        }
        if let (Some(schema), Some(structured)) = (&tool.output, &result.structured_content) {
            schema.validate(structured).map_err(|_| {
                self.error(
                    ErrorCode::ToolFailed,
                    "The App's result did not match the shape it declared",
                )
            })?;
        }

        let redacted = tramontana_core::services::redact(&text).text;
        let truncated = redacted.len() > limit;
        let kept = if truncated {
            let mut end = limit;
            while !redacted.is_char_boundary(end) {
                end -= 1;
            }
            redacted[..end].to_owned()
        } else {
            redacted
        };
        let summary = format!(
            "{} returned {} byte(s){}{}",
            tool.descriptor.display_name,
            kept.len(),
            if truncated { ", cut to the limit" } else { "" },
            if omitted > 0 {
                format!(", {omitted} non-text part(s) left out")
            } else {
                String::new()
            },
        );
        Ok(ToolOutcome {
            content: serde_json::json!({
                "text": kept,
                "truncated": truncated,
                "omittedParts": omitted,
            }),
            display_summary: summary,
            side_effect: tool.descriptor.side_effect,
            truncated,
            changes: Vec::new(),
        })
    }
}

/// Read the tool list a page at a time, stopping at the page limit.
async fn list_tools(
    peer: &Peer<RoleClient>,
    limits: &McpLimits,
) -> Result<Vec<rmcp::model::Tool>, String> {
    let mut tools = Vec::new();
    let mut cursor = None;
    for _ in 0..limits.max_pages {
        let page = tokio::time::timeout(
            limits.start_timeout,
            peer.list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor))),
        )
        .await
        .map_err(|_| "it did not answer in time".to_owned())?
        .map_err(|error| shown(&error.to_string(), 200))?;
        tools.extend(page.tools);
        cursor = page.next_cursor;
        if cursor.is_none() || tools.len() > limits.max_tools {
            break;
        }
    }
    Ok(tools)
}

fn validate(spec: &McpServerSpec) -> Result<(), StartError> {
    let id = &spec.app_id;
    let first_ok = id
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !first_ok
        || id.len() > 32
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(StartError::Invalid(
            "an App's identifier is lowercase letters, digits, `_` and `-`, at most 32",
        ));
    }
    if !spec.program.is_absolute() {
        return Err(StartError::Invalid(
            "an App's program is a full path; PATH is not searched",
        ));
    }
    if !spec.working_directory.is_dir() {
        return Err(StartError::Invalid(
            "an App's working directory must be an existing folder",
        ));
    }
    if spec
        .environment
        .iter()
        .any(|(name, _)| name.is_empty() || name.contains(['=', '\0']))
    {
        return Err(StartError::Invalid(
            "an environment variable name is not valid",
        ));
    }
    Ok(())
}

#[async_trait]
impl ToolProvider for McpApp {
    fn id(&self) -> ProviderId {
        ProviderId::new(format!("mcp.{}", self.spec.app_id))
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.tools
            .iter()
            .map(|tool| tool.descriptor.clone())
            .collect()
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.descriptor.tool_id == request.tool_id())
            .ok_or_else(|| self.error(ErrorCode::NotFound, "The App offers no such tool"))?;
        // Checked again here, whatever the caller checked: the arguments are
        // the model's, and what reaches the App must be what its schema
        // allows.
        tool.input.validate(request.arguments())?;
        let arguments =
            request.arguments().as_object().cloned().ok_or_else(|| {
                self.error(ErrorCode::InvalidRequest, "Arguments must be an object")
            })?;
        if cancel.is_cancelled() {
            return Err(self.error(ErrorCode::Cancelled, "Cancelled before the call"));
        }

        let mut options = PeerRequestOptions::no_options();
        options.timeout = Some(self.spec.limits.call_timeout);
        let handle = self
            .peer
            .send_request_with_option(
                ClientRequest::CallToolRequest(CallToolRequest::new(
                    CallToolRequestParams::new(tool.name.clone()).with_arguments(arguments),
                )),
                options,
            )
            .await
            .map_err(|error| self.call_failed(&error))?;
        let id = handle.id.clone();
        let answer = tokio::select! {
            answer = handle.await_response() => answer,
            () = cancel.cancelled() => {
                // Tell the App the call is no longer wanted; what it does
                // with that is up to it, but the task does not wait.
                let _ = self
                    .peer
                    .notify_cancelled(CancelledNotificationParam::new(
                        Some(id),
                        Some("cancelled by Tramontana".to_owned()),
                    ))
                    .await;
                return Err(self.error(ErrorCode::Cancelled, "The call was cancelled"));
            }
        };
        match answer {
            Ok(ServerResult::CallToolResult(result)) => self.outcome(tool, result),
            Ok(_) => Err(self.call_failed(&ServiceError::UnexpectedResponse)),
            Err(error) => Err(self.call_failed(&error)),
        }
    }
}
