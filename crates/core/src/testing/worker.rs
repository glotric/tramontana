use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AgentDescriptor, CheckpointSupport, ErrorEnvelope, EventDraft, EventPayload, IntegrationMode,
    ModelControl, NetworkControl, TaskEventKind, ToolControl,
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
#[derive(Debug, Default)]
pub struct FakeWorker;

impl FakeWorker {
    /// A reference worker.
    #[must_use]
    pub fn new() -> Self {
        Self
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
            checkpoint_support: CheckpointSupport::None,
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

        host.emit(EventDraft::user(
            TaskEventKind::ModelStarted,
            EventPayload::ModelStarted {
                stream_id: format!("stream-{}", request.task_id),
                model_id: "fake.deterministic".to_owned(),
            },
        ))
        .await?;

        let answer = host.call_model(&request.goal, &cancel).await?;
        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        // Deliberately no frame here. `call_model` already published one frame
        // per delta as the answer arrived; republishing the assembled answer
        // would make the interface render it twice, and the interface has no
        // way to tell a duplicate from genuinely repeated output.

        let echoed = host
            .call_tool(
                FAKE_TOOL_ID,
                serde_json::json!({ "value": answer }),
                &cancel,
            )
            .await?;
        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        let summary = echoed
            .get("echoed")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();

        Ok(WorkerOutcome::Completed { summary })
    }
}
