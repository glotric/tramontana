//! Small reference strategy; all durable work and authority stay in the host.
use crate::contracts::{
    AgentDescriptor, CheckpointSupport, ErrorEnvelope, IntegrationMode, ModelControl,
    NetworkControl, ToolControl,
};
use crate::ports::{
    AgentProvider, ReadOnlyLimits, ReadOnlyTurn, WorkerHost, WorkerOutcome, WorkerRequest,
};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Stable identity of the embedded read-only worker.
pub const REFERENCE_AGENT_ID: &str = "builtin.reference";

/// Sequential model/observation loop, with no private transcript or provider handles.
#[derive(Debug)]
pub struct ReferenceWorker {
    tools: Vec<String>,
    limits: ReadOnlyLimits,
}
impl ReferenceWorker {
    /// Choose explicit registered read-only tools and bounded session limits.
    #[must_use]
    pub fn new(tools: Vec<String>, limits: ReadOnlyLimits) -> Self {
        Self { tools, limits }
    }
}
#[async_trait]
impl AgentProvider for ReferenceWorker {
    fn describe(&self) -> AgentDescriptor {
        AgentDescriptor {
            agent_provider_id: "builtin".into(),
            agent_id: REFERENCE_AGENT_ID.into(),
            display_name: "Embedded read-only worker".into(),
            integration_mode: IntegrationMode::Embedded,
            model_control: ModelControl::TramontanaBrokered,
            tool_control: ToolControl::TramontanaBrokered,
            network_control: NetworkControl::Denied,
            checkpoint_support: CheckpointSupport::Portable,
            local_only_eligible: AgentDescriptor::compute_local_only_eligibility(
                ModelControl::TramontanaBrokered,
                NetworkControl::Denied,
            ),
            unavailable_reason: None,
        }
    }
    async fn run(
        &self,
        _request: WorkerRequest,
        host: &dyn WorkerHost,
        cancel: CancellationToken,
    ) -> Result<WorkerOutcome, ErrorEnvelope> {
        loop {
            if cancel.is_cancelled() {
                return Ok(WorkerOutcome::Cancelled);
            }
            match host
                .read_only_turn(&self.tools, &self.limits, &cancel)
                .await?
            {
                ReadOnlyTurn::Observed => {}
                ReadOnlyTurn::Completed { summary } => {
                    return Ok(WorkerOutcome::Completed { summary });
                }
            }
        }
    }
}
