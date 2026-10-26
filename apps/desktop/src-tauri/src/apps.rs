//! The Apps screen's Rust half (ADR 0040).
//!
//! What the profile has configured, what each running App offers and how it
//! is doing, and what was decided about its tools — shown with less than the
//! profile holds: no secret or variable value, and the program by its file
//! name only.

use std::collections::HashMap;

use serde::Serialize;
use tramontana_core::contracts::{
    AppConfig, ErrorCode, ErrorEnvelope, EventPayload, GrantKind, TaskId, Timestamp,
};
use tramontana_core::ports::{AppSettingsRepository, ToolProvider};
use tramontana_core::services::TaskService;

use crate::ipc_error;
use crate::state::AppState;

/// The most recent decisions the history shows.
pub(crate) const HISTORY_DECISIONS: usize = 50;
/// How many of the most recent tasks the history looks through.
pub(crate) const HISTORY_TASKS: usize = 200;

/// Where an App stands, in the words the screen uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum AppStatus {
    /// Started with this window and still connected.
    Running,
    /// Started with this window, and has since stopped.
    Stopped { reason: String },
    /// Enabled, but it did not start when this window opened.
    DidNotStart { reason: String },
    /// Turned off, and not running.
    Off,
    /// Turned on since this window opened; it starts next time.
    StartsOnRestart,
    /// Turned off since this window opened; it is still running until then.
    StopsOnRestart,
}

/// One tool an App registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppToolView {
    pub tool_id: String,
    /// The App's own description of it. Presentation, never policy.
    pub description: String,
    /// Whether a task that admits Apps may offer it to a model (ADR 0036).
    pub offered: bool,
}

/// One tool an App offered that Tramontana refused to register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RefusedToolView {
    pub name: String,
    pub reason: String,
}

/// One configured App, as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppView {
    pub app_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub status: AppStatus,
    /// The program's file name. Its full path names a folder, and usually a
    /// person.
    pub program: String,
    /// A remote App's address (ADR 0050); absent for one that runs here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The names of the variables it is given; never their values.
    pub variables: Vec<String>,
    /// The names of the secrets it is given; never their values.
    pub secrets: Vec<String>,
    /// The MCP version it answered with, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    pub tools: Vec<AppToolView>,
    pub refused: Vec<RefusedToolView>,
    /// The last of what it wrote to its error stream, already redacted.
    pub diagnostics: String,
}

/// The program's file name, whichever separator its path uses.
fn file_name(program: &str) -> String {
    program
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(program)
        .to_owned()
}

/// Every configured App, and how each one is doing in this window.
pub(crate) async fn list(state: &AppState) -> Result<Vec<AppView>, ErrorEnvelope> {
    let configured = state
        .plane
        .app_settings
        .list()
        .await
        .map_err(|error| failure(format!("could not read the Apps: {error}")))?;
    let mut views = Vec::with_capacity(configured.len());
    for config in configured {
        views.push(view(state, config).await);
    }
    Ok(views)
}

async fn view(state: &AppState, config: AppConfig) -> AppView {
    let running = state
        .plane
        .apps
        .apps
        .iter()
        .find(|app| app.app_id() == config.app_id);
    let failed = state
        .plane
        .apps
        .failures
        .iter()
        .find(|failure| failure.app_id == config.app_id);

    let status = match (running, failed, config.enabled) {
        (Some(_), _, false) => AppStatus::StopsOnRestart,
        (Some(app), _, true) => match app.health().await {
            tramontana_adapters_mcp::AppHealth::Running => AppStatus::Running,
            tramontana_adapters_mcp::AppHealth::Stopped { reason } => AppStatus::Stopped { reason },
        },
        (None, Some(failure), true) => AppStatus::DidNotStart {
            reason: failure.reason.clone(),
        },
        (None, _, false) => AppStatus::Off,
        (None, None, true) => AppStatus::StartsOnRestart,
    };

    let (protocol_version, tools, refused, diagnostics) = running.map_or_else(
        || (None, Vec::new(), Vec::new(), String::new()),
        |app| {
            let tools = app
                .list_tools()
                .into_iter()
                .map(|tool| AppToolView {
                    offered: tramontana_composition::offerable(&tool),
                    tool_id: tool.tool_id,
                    description: tool.description,
                })
                .collect();
            let refused = app
                .refused()
                .iter()
                .map(|refusal| RefusedToolView {
                    name: refusal.name.clone(),
                    reason: refusal.reason.to_owned(),
                })
                .collect();
            (
                Some(app.protocol_version().to_owned()),
                tools,
                refused,
                app.diagnostics(),
            )
        },
    );

    AppView {
        program: file_name(&config.launch.program),
        address: config.remote.as_ref().map(|remote| remote.url.clone()),
        variables: config
            .launch
            .environment
            .iter()
            .map(|variable| variable.name.clone())
            .collect(),
        secrets: config
            .launch
            .secrets
            .iter()
            .map(|secret| secret.name.clone())
            .collect(),
        app_id: config.app_id,
        display_name: config.display_name,
        enabled: config.enabled,
        status,
        protocol_version,
        tools,
        refused,
        diagnostics,
    }
}

/// Turn an App on or off. Takes effect the next time Tramontana opens.
pub(crate) async fn set_enabled(
    settings: &dyn AppSettingsRepository,
    app_id: &str,
    enabled: bool,
) -> Result<(), ErrorEnvelope> {
    let mut app = settings
        .list()
        .await
        .map_err(|error| failure(format!("could not read the Apps: {error}")))?
        .into_iter()
        .find(|app| app.app_id == app_id)
        .ok_or_else(|| {
            ipc_error(
                ErrorCode::NotFound,
                format!("no App is configured as `{app_id}`"),
            )
        })?;
    app.enabled = enabled;
    settings
        .save(&app)
        .await
        .map_err(|error| failure(format!("could not save the App: {error}")))
}

/// How a call to one of an App's tools was decided, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppDecision {
    pub task_id: TaskId,
    /// The task's goal, as its record summarizes it.
    pub goal: String,
    pub tool_id: String,
    pub allowed: bool,
    /// The stable reason the decision gives.
    pub reason_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_kind: Option<GrantKind>,
    /// `completed` or `failed`, once the call ended; absent when it never ran
    /// or never finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub decided_at: Timestamp,
}

/// What the history found, and whether it stopped looking before the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppHistory {
    pub decisions: Vec<AppDecision>,
    /// Whether older tasks were not looked through, or more decisions were
    /// found than are shown.
    pub partial: bool,
}

/// The most recent decisions about `app_id`'s tools, newest first, from the
/// durable record.
///
/// A decision does not name its tool, so each is paired with the request it
/// answers by invocation, and with the call's result when there was one.
pub(crate) async fn history(
    service: &TaskService,
    app_id: &str,
) -> Result<AppHistory, ErrorEnvelope> {
    let prefix = format!("{}{app_id}.", tramontana_core::services::APP_NAMESPACE);
    let mut tasks = service.list_tasks(None).await?;
    tasks.sort_by(|a, b| b.created_at.as_str().cmp(a.created_at.as_str()));
    let mut partial = tasks.len() > HISTORY_TASKS;
    tasks.truncate(HISTORY_TASKS);

    let mut decisions = Vec::new();
    for task in tasks {
        let events = service.replay_task_events(&task.task_id, 0).await?;
        let mut requested: HashMap<String, String> = HashMap::new();
        let mut outcomes: HashMap<String, &'static str> = HashMap::new();
        for event in &events {
            match &event.payload {
                EventPayload::ToolRequested {
                    invocation_id,
                    tool_id,
                    ..
                } if tool_id.starts_with(&prefix) => {
                    requested.insert(invocation_id.to_string(), tool_id.clone());
                }
                EventPayload::ToolCompleted { invocation_id, .. } => {
                    outcomes.insert(invocation_id.to_string(), "completed");
                }
                EventPayload::ToolFailed { invocation_id, .. } => {
                    outcomes.insert(invocation_id.to_string(), "failed");
                }
                _ => {}
            }
        }
        let mut found: Vec<AppDecision> = events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::PermissionDecided {
                    invocation_id,
                    allowed,
                    reason_code,
                    grant_kind,
                    ..
                } => requested
                    .get(&invocation_id.to_string())
                    .map(|tool_id| AppDecision {
                        task_id: task.task_id.clone(),
                        goal: task.goal_summary.clone(),
                        tool_id: tool_id.clone(),
                        allowed: *allowed,
                        reason_code: reason_code.clone(),
                        grant_kind: *grant_kind,
                        outcome: outcomes
                            .get(&invocation_id.to_string())
                            .map(|outcome| (*outcome).to_owned()),
                        decided_at: event.occurred_at.clone(),
                    }),
                _ => None,
            })
            .collect();
        found.reverse();
        decisions.extend(found);
        if decisions.len() > HISTORY_DECISIONS {
            partial = true;
            break;
        }
    }
    decisions.truncate(HISTORY_DECISIONS);
    Ok(AppHistory { decisions, partial })
}

fn failure(message: String) -> ErrorEnvelope {
    ipc_error(ErrorCode::PersistenceError, message)
}
