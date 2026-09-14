//! The recipe runner: a project's own commands, run exactly as the core
//! resolved them and a person approved them (ADR 0025).

use std::ffi::OsString;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, ReplaySafety,
    RiskClass, SideEffect, ToolDescriptor,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};
use tramontana_core::services::RECIPE_TOOL_ID;

use crate::command::{CommandError, CommandSpec, run_command};
use crate::scope::primary_root;

/// Runs the commands a project declares in `.tramontana/project.md`.
///
/// Dangerous, whatever a recipe is called: the repository chooses the name,
/// and a project command can do whatever the user can. So every run is its own
/// approval, nothing is ever replayed, and a model is offered this tool only
/// when recipes are admitted (ADR 0025).
///
/// It runs only a command the core resolved. The model names a recipe; the
/// core looks it up in the task's configuration and hands this tool the
/// argument list the person approved. A request without that list did not
/// come through the core, and is refused.
#[derive(Debug, Clone)]
pub struct RecipeTools {
    deadline: Duration,
    output_limit: usize,
}

impl Default for RecipeTools {
    fn default() -> Self {
        Self::new()
    }
}

impl RecipeTools {
    /// Ten minutes and 32 KiB of each stream. A worker's turn deadline bounds
    /// its model calls, not this run (ADR 0027).
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(Duration::from_secs(600), 32 * 1024)
    }

    /// Explicit limits.
    #[must_use]
    pub fn with_limits(deadline: Duration, output_limit: usize) -> Self {
        Self {
            deadline,
            output_limit,
        }
    }
}

#[async_trait]
impl ToolProvider for RecipeTools {
    fn id(&self) -> ProviderId {
        ProviderId::new("builtin")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: RECIPE_TOOL_ID.to_owned(),
            provider_id: "builtin".to_owned(),
            display_name: "Run recipe".to_owned(),
            description: "Run one of the commands the project declares in `.tramontana/project.md` \
                          under `recipes:`, such as `test`, by its id. You choose the recipe, never \
                          its command or arguments. A person approves every run and sees the exact \
                          command first. A command that fails is reported with its exit code and \
                          output rather than as an error."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "recipe": { "type": "string" } },
                "required": ["recipe"],
                "additionalProperties": false,
            }),
            risk_class: RiskClass::Dangerous,
            // The most conservative honest declaration: a project command can
            // reach anything the user can.
            side_effect: SideEffect::External,
            capability: "command.recipe".to_owned(),
            supports_cancellation: true,
            // Running again a command that was cut short could repeat whatever
            // its first run did.
            replay_safety: ReplaySafety::Unsafe,
        }]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let correlation = CorrelationId::new(request.invocation_id().as_str());
        let fail = |code: ErrorCode, message: String| {
            ErrorEnvelope::new(code, message, correlation.clone())
        };
        if request.tool_id() != RECIPE_TOOL_ID {
            return Err(fail(
                ErrorCode::InvalidRequest,
                format!("unknown tool {}", request.tool_id()),
            ));
        }

        let arguments = request.arguments();
        let recipe = arguments.get("recipe").and_then(Value::as_str);
        let run = strings(arguments.get("run")).filter(|run| !run.is_empty());
        let inherit = strings(arguments.get("inherit"));
        let (Some(recipe), Some(run), Some(inherit)) = (recipe, run, inherit) else {
            return Err(fail(
                ErrorCode::InvalidRequest,
                "a recipe runs only as the core resolved it".to_owned(),
            ));
        };
        let root = primary_root(&request)?;

        let spec = CommandSpec {
            program: run[0].clone().into(),
            arguments: run[1..].iter().map(OsString::from).collect(),
            directory: root.as_path().to_path_buf(),
            inherit,
            set: Vec::new(),
            deadline: self.deadline,
            output_limit: self.output_limit,
        };
        let output = run_command(&spec, &cancel)
            .await
            .map_err(|error| match error {
                CommandError::Cancelled => fail(
                    ErrorCode::Cancelled,
                    format!("recipe `{recipe}` was cancelled; everything it started was stopped"),
                ),
                CommandError::Timeout => fail(
                    ErrorCode::Timeout,
                    format!(
                        "recipe `{recipe}` exceeded its deadline; everything it started was stopped"
                    ),
                ),
                CommandError::Spawn(std::io::ErrorKind::NotFound) => fail(
                    ErrorCode::NotFound,
                    format!("recipe `{recipe}`: its program was not found"),
                ),
                other => fail(ErrorCode::ToolFailed, format!("recipe `{recipe}`: {other}")),
            })?;

        let truncated = output.stdout.truncated() || output.stderr.truncated();
        let display_summary = match output.exit_code {
            Some(0) => format!("Recipe `{recipe}` passed"),
            Some(code) => format!("Recipe `{recipe}` failed (exit {code})"),
            None => format!("Recipe `{recipe}` was ended by a signal"),
        };
        Ok(ToolOutcome {
            content: json!({
                "recipe": recipe,
                "exitCode": output.exit_code,
                "succeeded": output.succeeded(),
                "stdout": String::from_utf8_lossy(&output.stdout.kept),
                "stderr": String::from_utf8_lossy(&output.stderr.kept),
                "stdoutBytes": output.stdout.total,
                "stderrBytes": output.stderr.total,
                "truncated": truncated,
            }),
            display_summary,
            side_effect: SideEffect::External,
            truncated,
            changes: Vec::new(),
        })
    }
}

/// A JSON array of strings, or nothing.
fn strings(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}
