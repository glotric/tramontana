use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AgentDescriptor, CheckpointSupport, ErrorEnvelope, IntegrationMode, ModelControl,
    NetworkControl, ToolControl,
};
use crate::ports::{AgentProvider, WorkerHost, WorkerOutcome, WorkerRequest};
use crate::testing::tool::FAKE_TOOL_ID;

/// The identifier of the reference fake worker.
pub const FAKE_AGENT_ID: &str = "fake.reference";

/// A worker that performs one scripted, fully brokered pass.
///
/// It exists to exercise the contracts, not to be clever: ask the model, echo
/// through a tool, report. Every effect goes through [`WorkerHost`], so the
/// same path a real external worker will take is the one under test from the
/// first milestone.
#[derive(Debug)]
pub struct FakeWorker {
    /// Which tool the worker asks the host to run.
    ///
    /// Configurable because the worker lives in `core` and the real read-only
    /// tools live in an adapter crate that `core` must not depend on. A
    /// deployment points it at a genuine tool; a core test points it at the
    /// echo fake.
    tool_id: String,
    /// The arguments it sends.
    arguments: serde_json::Value,
}

impl Default for FakeWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWorker {
    /// A reference worker that echoes through the fake tool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_id: FAKE_TOOL_ID.to_owned(),
            arguments: serde_json::json!({ "value": "" }),
        }
    }

    /// A reference worker that calls a named tool.
    ///
    /// How the walking skeleton is pointed at real filesystem tools without
    /// `core` learning about them.
    #[must_use]
    pub fn calling(tool_id: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            tool_id: tool_id.into(),
            arguments,
        }
    }
}

#[async_trait]
impl AgentProvider for FakeWorker {
    fn describe(&self) -> AgentDescriptor {
        let model_control = ModelControl::TramontanaBrokered;
        let network_control = NetworkControl::Denied;
        AgentDescriptor {
            agent_provider_id: "fake".to_owned(),
            agent_id: FAKE_AGENT_ID.to_owned(),
            display_name: "Reference fake worker".to_owned(),
            integration_mode: IntegrationMode::Embedded,
            model_control,
            tool_control: ToolControl::TramontanaBrokered,
            network_control,
            // Portable, not merely same-worker: every effect goes through
            // the host, so this worker keeps no private state and a resumed
            // run is a pure function of the goal and the completed-step
            // watermark. Understating it would make canonical handoff
            // untestable against the only worker that exists.
            checkpoint_support: CheckpointSupport::Portable,
            // Computed, never asserted: the core decides eligibility from the
            // control facts above (`CORE_CONTRACTS.md` §7.2).
            local_only_eligible: AgentDescriptor::compute_local_only_eligibility(
                model_control,
                network_control,
            ),
            unavailable_reason: None,
        }
    }

    async fn run(
        &self,
        request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        // Cancellation is checked before each brokered step rather than only at
        // the top, so a cancel arriving mid-run stops at the next boundary
        // instead of after everything has already happened.
        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        // No `modelStarted` here. A worker announcing that a model started is
        // announcing something it does not yet know: the request has not been
        // released, and it may be refused. The host emits it after the release
        // decision, naming the model the router actually chose rather than one
        // the worker hardcoded.
        let answer = host.call_model(&request.goal, &cancel).await?;
        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        // Deliberately no frame here. `call_model` already published one frame
        // per delta as the answer arrived; republishing the assembled answer
        // would make the interface render it twice, and the interface has no
        // way to tell a duplicate from genuinely repeated output.

        // The arguments are the worker's own, except that the echo fake wants
        // the model's answer back. A real tool gets exactly what it was
        // configured with.
        let arguments = if self.tool_id == FAKE_TOOL_ID {
            serde_json::json!({ "value": answer })
        } else {
            self.arguments.clone()
        };

        let result = host
            .call_tool_step("reference-tool", &self.tool_id, arguments, &cancel)
            .await?;
        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        // Summarize whatever the tool returned. A worker reports what it
        // learned; it does not copy the payload into the task summary, which
        // is displayed and exported.
        let summary = result
            .get("echoed")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                result
                    .get("entries")
                    .and_then(serde_json::Value::as_array)
                    .map(|entries| {
                        format!(
                            "{} entr{} listed",
                            entries.len(),
                            if entries.len() == 1 { "y" } else { "ies" }
                        )
                    })
            })
            .unwrap_or_else(|| format!("{} completed", self.tool_id));

        Ok(WorkerOutcome::Completed { summary })
    }
}
