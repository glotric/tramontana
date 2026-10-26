//! The commands a host answers, named and shaped as the desktop's own.
//!
//! Each one is the matching Tauri command's body: parse the arguments the
//! webview would send, call the service, return what the webview would get.
//! A command not listed here is refused, never guessed at. `watch_task` and
//! `unwatch_task` stream, so they live in `server.rs`.
//
// shortcut: SP1 serves the commands one person needs to follow and start
// work remotely. The rest of the desktop's commands follow in SP1 step 4,
// when the desktop forwards to a host.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tramontana_core::contracts::{
    ConversationId, CorrelationId, ErrorCode, ErrorEnvelope, ProjectId, TaskId,
};
use tramontana_core::services::{StartTaskRequest, TaskService, checked_goal};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ByProject {
    project_id: Option<ProjectId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ByConversation {
    conversation_id: Option<ConversationId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ByTask {
    task_id: TaskId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTask {
    goal: String,
    conversation_id: Option<ConversationId>,
}

/// Run one non-streaming command.
pub(crate) async fn dispatch(
    service: &TaskService,
    name: &str,
    args: Value,
) -> Result<Value, ErrorEnvelope> {
    match name {
        "core_info" => Ok(serde_json::json!({
            "schemaVersion": tramontana_core::SCHEMA_VERSION,
            "appVersion": env!("CARGO_PKG_VERSION"),
        })),
        "list_projects" => to_value(service.list_projects().await?),
        "list_conversations" => {
            let ByProject { project_id } = parse(name, args)?;
            to_value(service.list_conversations(project_id.as_ref()).await?)
        }
        "list_tasks" => {
            let ByConversation { conversation_id } = parse(name, args)?;
            to_value(service.list_tasks(conversation_id.as_ref()).await?)
        }
        "get_task" => {
            let ByTask { task_id } = parse(name, args)?;
            to_value(service.get_task(&task_id).await?)
        }
        "replay_task_events" => {
            let ByTask { task_id } = parse(name, args)?;
            to_value(service.replay_task_events(&task_id, 0).await?)
        }
        "cancel_task" => {
            let ByTask { task_id } = parse(name, args)?;
            service.cancel_task(&task_id).await?;
            Ok(Value::Null)
        }
        // Started here, followed with `watch_task`: one stream per watch.
        "start_task" => {
            let StartTask {
                goal,
                conversation_id,
            } = parse(name, args)?;
            let task_id = service
                .start_task(StartTaskRequest {
                    conversation_id,
                    goal: checked_goal(&goal)?,
                    ..StartTaskRequest::default()
                })
                .await?;
            to_value(task_id)
        }
        _ => Err(invalid(format!("this host has no command named {name:?}"))),
    }
}

pub(crate) fn parse<T: DeserializeOwned>(name: &str, args: Value) -> Result<T, ErrorEnvelope> {
    let args = if args.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        args
    };
    serde_json::from_value(args)
        .map_err(|_| invalid(format!("the arguments for {name} are not valid")))
}

fn to_value(value: impl serde::Serialize) -> Result<Value, ErrorEnvelope> {
    serde_json::to_value(value).map_err(|_| {
        ErrorEnvelope::new(
            ErrorCode::Internal,
            "the reply could not be encoded",
            CorrelationId::new("host"),
        )
    })
}

pub(crate) fn invalid(message: String) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidRequest,
        message,
        CorrelationId::new("host"),
    )
}
