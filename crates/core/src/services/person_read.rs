//! Reads a person asks for themselves, outside any task (ADR 0032).
//!
//! A task's tools run through its broker, which journals every request,
//! decision and result. A person looking at their own project's diff is not a
//! task: nothing asked for it on a task's behalf, and it changes nothing. So it
//! goes through the same registry and the same permission manager, under the
//! same run limit, but is recorded in no task's stream.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::task_service::TaskService;
use crate::contracts::{
    Capability, CapabilityScope, ErrorCode, ErrorEnvelope, InvocationId, ProjectId, RiskClass,
    SideEffect,
};
use crate::ports::{PermissionQuery, ToolOutcome};

impl TaskService {
    /// Run one of a project's reads for the person, outside any task.
    ///
    /// Only a registered tool that declares a read with no side effect and
    /// takes no arguments, within the project's approved roots, and only when
    /// the permission manager allows it without asking (ADR 0032).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] for an unknown project or tool,
    /// [`ErrorCode::InvalidRequest`] for a tool that is more than a
    /// no-argument read, [`ErrorCode::PermissionDenied`] when the policy does
    /// not allow it, [`ErrorCode::Timeout`] past the run limit, or the tool's
    /// own failure.
    pub async fn read_project(
        &self,
        project_id: &ProjectId,
        tool_id: &str,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let project = self.get_project(project_id).await?;
        let registered = self
            .providers
            .tools
            .get(tool_id)
            .ok_or_else(|| self.error(ErrorCode::NotFound, "No such tool is registered"))?;
        let descriptor = &registered.descriptor;
        // Checked here from the tool's own declaration, before the permission
        // manager is consulted: a tool that could change something, or that
        // would need arguments someone typed, is not a person's quiet read.
        if descriptor.risk_class != RiskClass::Read
            || descriptor.side_effect != SideEffect::None
            || !takes_no_arguments(&descriptor.input_schema)
        {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                "Only a read that takes no arguments can be run outside a task",
            ));
        }

        let query = PermissionQuery {
            tool_id: descriptor.tool_id.clone(),
            capability: Capability::new(&descriptor.capability),
            risk_class: descriptor.risk_class,
            scope: CapabilityScope::project(project.project_id.clone(), project.roots.clone()),
            persistable: false,
            details: Vec::new(),
        };
        let invocation = InvocationId::new(self.ids.next("invocation"));
        let (_, authorized) = self.providers.permissions.authorize_person_read(
            &query,
            &invocation,
            serde_json::json!({}),
        );
        let authorized = authorized.ok_or_else(|| {
            self.error(
                ErrorCode::PermissionDenied,
                "The permission policy does not allow this read",
            )
        })?;

        // The same backstop a task's tool runs under (ADR 0027). A Git read
        // bounds itself well within it.
        let run = CancellationToken::new();
        let _stop_on_exit = run.clone().drop_guard();
        tokio::select! {
            result = registered.provider.invoke(authorized, run.clone()) => result,
            () = tokio::time::sleep(crate::ports::TOOL_RUN_LIMIT) => Err(self.error(
                ErrorCode::Timeout,
                "The read did not finish within the run limit",
            )),
        }
    }
}

/// Whether a tool's input schema admits only an empty object.
fn takes_no_arguments(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_none_or(serde_json::Map::is_empty)
        && schema
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
}
