//! Small reference strategy; all durable work and authority stay in the host.
use crate::contracts::{
    AgentDescriptor, CheckpointSupport, ErrorEnvelope, IntegrationMode, ModelControl,
    NetworkControl, ToolControl,
};
use crate::ports::{
    AgentProvider, ReadOnlyLimits, ReadOnlyTurn, ToolAdmission, WorkerHost, WorkerOutcome,
    WorkerRequest,
};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Stable identity of the embedded reference worker.
pub const REFERENCE_AGENT_ID: &str = "builtin.reference";

/// Sequential model/observation loop, with no private transcript or provider handles.
#[derive(Debug)]
pub struct ReferenceWorker {
    tools: Vec<String>,
    limits: ReadOnlyLimits,
    admission: ToolAdmission,
}
impl ReferenceWorker {
    /// Choose explicit registered read-only tools and bounded session limits.
    #[must_use]
    pub fn new(tools: Vec<String>, limits: ReadOnlyLimits) -> Self {
        Self {
            tools,
            limits,
            admission: ToolAdmission::READ_ONLY,
        }
    }

    /// Offer the selected tools at `admission` instead of read-only. Writes
    /// and recipes are still approved one at a time; this only lets the model
    /// ask.
    #[must_use]
    pub fn admitting(mut self, admission: ToolAdmission) -> Self {
        self.admission = admission;
        self
    }

    /// The worker reaches the network only through the browser, whose every
    /// site passes the host (ADR 0047).
    fn network_control(&self) -> NetworkControl {
        if self.admission.browse {
            NetworkControl::Brokered
        } else {
            NetworkControl::Denied
        }
    }
}

#[async_trait]
impl AgentProvider for ReferenceWorker {
    fn describe(&self) -> AgentDescriptor {
        AgentDescriptor {
            agent_provider_id: "builtin".into(),
            agent_id: REFERENCE_AGENT_ID.into(),
            display_name: match (self.admission.writes, self.admission.recipes) {
                (false, false) => "Embedded read-only worker",
                (true, false) => "Embedded read-write worker",
                (false, true) => "Embedded read-only worker that runs recipes",
                (true, true) => "Embedded read-write worker that runs recipes",
            }
            .into(),
            integration_mode: IntegrationMode::Embedded,
            model_control: ModelControl::TramontanaBrokered,
            tool_control: ToolControl::TramontanaBrokered,
            network_control: self.network_control(),
            checkpoint_support: CheckpointSupport::Portable,
            local_only_eligible: AgentDescriptor::compute_local_only_eligibility(
                ModelControl::TramontanaBrokered,
                self.network_control(),
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
                .governed_turn(&self.tools, self.admission, &self.limits, &cancel)
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
