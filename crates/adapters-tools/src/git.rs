use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, RiskClass,
    SideEffect, ToolDescriptor,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};

use crate::limits::ReadLimits;
use crate::scope::primary_root;

/// Read-only Git inspection.
///
/// `git` runs as a supervised child process with a fixed argument array and
/// **no shell**, its working directory pinned to an approved root, a minimal
/// environment, a deadline, and a bounded output. M6 formalizes all of that as
/// a general command broker; until then these three invocations are the only
/// processes the application spawns, and they are hard-coded rather than
/// composed from caller input.
///
/// Read-only in the sense that matters: the subcommands cannot alter the
/// repository. `SECURITY_AND_PERMISSIONS.md` §3.1 classifies Git status and
/// diff as Read.
#[derive(Debug, Clone)]
pub struct GitTools {
    limits: ReadLimits,
}

impl Default for GitTools {
    fn default() -> Self {
        Self::new()
    }
}

impl GitTools {
    /// Tools with the default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: ReadLimits::default(),
        }
    }

    /// Tools with explicit limits.
    #[must_use]
    pub fn with_limits(limits: ReadLimits) -> Self {
        Self { limits }
    }
}

pub(crate) const STATUS: &str = "builtin.git.status";
pub(crate) const DIFF: &str = "builtin.git.diff";
pub(crate) const LOG: &str = "builtin.git.log";

/// The exact arguments each tool runs. Nothing here comes from the caller.
fn arguments_for(tool_id: &str) -> Option<&'static [&'static str]> {
    match tool_id {
        STATUS => Some(&["status", "--porcelain=v1", "--branch"]),
        // `--no-color` and `--no-ext-diff` keep the output plain and stop a
        // repository's own config from running an external diff program.
        DIFF => Some(&[
            "--no-pager",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--stat",
        ]),
        LOG => Some(&[
            "--no-pager",
            "log",
            "--max-count=20",
            "--no-color",
            "--pretty=format:%h %ad %s",
            "--date=short",
        ]),
        _ => None,
    }
}

fn descriptor(tool_id: &str, display: &str, description: &str) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_owned(),
        provider_id: "builtin".to_owned(),
        display_name: display.to_owned(),
        description: description.to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        risk_class: RiskClass::Read,
        side_effect: SideEffect::None,
        capability: "git.read".to_owned(),
        supports_cancellation: true,
    }
}

#[async_trait]
impl ToolProvider for GitTools {
    fn id(&self) -> ProviderId {
        ProviderId::new("builtin")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![
            descriptor(STATUS, "Git status", "Show the working tree status."),
            descriptor(DIFF, "Git diff", "Show a summary of uncommitted changes."),
            descriptor(LOG, "Git log", "Show recent commits."),
        ]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let correlation = CorrelationId::new(request.invocation_id().as_str());
        let fail = |code, message: String| ErrorEnvelope::new(code, message, correlation.clone());

        if cancel.is_cancelled() {
            return Err(fail(
                ErrorCode::Cancelled,
                "cancelled before running".to_owned(),
            ));
        }

        let Some(arguments) = arguments_for(request.tool_id()) else {
            return Err(fail(
                ErrorCode::InvalidRequest,
                format!("unknown tool {}", request.tool_id()),
            ));
        };
        let root = primary_root(&request)?;

        let mut command = tokio::process::Command::new("git");
        command
            .args(arguments)
            .current_dir(root.as_path())
            // A scrubbed environment. Inheriting the parent's would hand the
            // child every variable this process holds, and Git in particular
            // reads a great deal of behavior from its environment.
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", root.as_path())
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = tokio::select! {
            result = command.output() => result.map_err(|error| {
                fail(ErrorCode::ToolFailed, format!("could not run git: {}", error.kind()))
            })?,
            () = cancel.cancelled() => {
                return Err(fail(ErrorCode::Cancelled, "cancelled while running".to_owned()));
            }
            () = tokio::time::sleep(self.limits.deadline) => {
                return Err(fail(ErrorCode::Timeout, "git exceeded its deadline".to_owned()));
            }
        };

        if !output.status.success() {
            // stderr is bounded and reported as a category, not echoed: it can
            // carry absolute paths and remote URLs.
            return Err(fail(
                ErrorCode::ToolFailed,
                format!(
                    "git exited unsuccessfully ({})",
                    output
                        .status
                        .code()
                        .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                ),
            ));
        }

        let full_len = output.stdout.len();
        let truncated = full_len > self.limits.max_bytes;
        let text: String = String::from_utf8_lossy(&output.stdout)
            .chars()
            .take(self.limits.max_bytes)
            .collect();

        let lines = text.lines().count();
        Ok(ToolOutcome {
            content: serde_json::json!({
                "tool": request.tool_id(),
                "text": text,
                "byteCount": full_len,
                "truncated": truncated,
            }),
            display_summary: format!(
                "{} returned {lines} line{}",
                request.tool_id(),
                if lines == 1 { "" } else { "s" }
            ),
            side_effect: SideEffect::None,
            truncated,
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_runs_exactly_the_arguments_it_declares() {
        // No caller input reaches the command line: the arguments are
        // constants, matched here literally. That is what makes these three
        // invocations safe to ship before M6's general command broker exists,
        // and this test fails if anyone starts composing them.
        //
        // Spaces inside a single argument are fine — `--pretty=format:%h %ad
        // %s` is one argv element. What must never appear is a *shell*: the
        // arguments are passed as an array to `Command::args`, never joined.
        assert_eq!(
            arguments_for(STATUS),
            Some(&["status", "--porcelain=v1", "--branch"][..])
        );
        assert_eq!(
            arguments_for(DIFF),
            Some(
                &[
                    "--no-pager",
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "--stat"
                ][..]
            )
        );
        assert_eq!(
            arguments_for(LOG),
            Some(
                &[
                    "--no-pager",
                    "log",
                    "--max-count=20",
                    "--no-color",
                    "--pretty=format:%h %ad %s",
                    "--date=short",
                ][..]
            )
        );
    }

    #[test]
    fn no_argument_could_redirect_output_or_chain_a_command() {
        // Belt and braces. These arguments never reach a shell, so a
        // metacharacter would be inert — but one appearing here would mean
        // someone had started building a command line as text.
        for tool in [STATUS, DIFF, LOG] {
            for argument in arguments_for(tool).expect("known tool") {
                assert!(
                    !argument.contains([';', '|', '&', '>', '<', '`', '$']),
                    "{tool} argument `{argument}` looks like shell syntax"
                );
            }
        }
    }

    #[test]
    fn only_read_only_subcommands_are_offered() {
        let mutating = [
            "add", "commit", "push", "fetch", "pull", "checkout", "reset", "clean", "rm", "mv",
            "merge", "rebase", "config", "remote",
        ];
        for tool in [STATUS, DIFF, LOG] {
            let arguments = arguments_for(tool).expect("known tool");
            for argument in arguments {
                assert!(
                    !mutating.contains(argument),
                    "{tool} must not run the mutating subcommand `{argument}`"
                );
            }
        }
    }

    #[test]
    fn an_unknown_tool_has_no_arguments() {
        assert!(arguments_for("builtin.git.push").is_none());
        assert!(arguments_for("builtin.fs.read").is_none());
    }
}
