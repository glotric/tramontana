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

/// The model a worker is pinned to, and what a person calls it (ADR 0048).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedModel {
    /// The endpoint and model.
    pub model: crate::contracts::RoutedModel,
    /// What the person calls it, such as "Qwen 2.5 1.5B" or "Claude".
    pub display_name: String,
    /// Whether it runs on this device. A task that runs Local Only is never
    /// handed to one that does not.
    pub location: crate::contracts::ModelLocation,
}

impl PinnedModel {
    /// The identity of the built-in worker pinned to this model: stable, so
    /// a resumed task finds the same worker, and distinct from every other
    /// pin, so a task can be handed from one to another.
    #[must_use]
    pub fn agent_id(&self) -> String {
        format!(
            "{REFERENCE_AGENT_ID}@{}/{}",
            self.model.provider_id, self.model.model_id
        )
    }
}

/// Sequential model/observation loop, with no private transcript or provider handles.
#[derive(Debug)]
pub struct ReferenceWorker {
    tools: Vec<String>,
    limits: ReadOnlyLimits,
    admission: ToolAdmission,
    pinned: Option<PinnedModel>,
}
impl ReferenceWorker {
    /// Choose explicit registered read-only tools and bounded session limits.
    #[must_use]
    pub fn new(tools: Vec<String>, limits: ReadOnlyLimits) -> Self {
        Self {
            tools,
            limits,
            admission: ToolAdmission::READ_ONLY,
            pinned: None,
        }
    }

    /// Use this model and no other, rather than the one the router would
    /// choose (ADR 0048). The worker becomes a different worker: its own
    /// identity, to which a task can be handed.
    #[must_use]
    pub fn pinned(mut self, model: PinnedModel) -> Self {
        self.pinned = Some(model);
        self
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
            agent_id: self
                .pinned
                .as_ref()
                .map_or_else(|| REFERENCE_AGENT_ID.into(), PinnedModel::agent_id),
            display_name: {
                let kind = match (self.admission.writes, self.admission.recipes) {
                    (false, false) => "Embedded read-only worker",
                    (true, false) => "Embedded read-write worker",
                    (false, true) => "Embedded read-only worker that runs recipes",
                    (true, true) => "Embedded read-write worker that runs recipes",
                };
                match &self.pinned {
                    Some(pinned) => format!("{kind} on {}", pinned.display_name),
                    None => kind.to_owned(),
                }
            },
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
    fn pinned_model(&self) -> Option<crate::contracts::RoutedModel> {
        self.pinned.as_ref().map(|pinned| pinned.model.clone())
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
            let turn = match &self.pinned {
                Some(pinned) => {
                    host.pinned_turn(
                        &self.tools,
                        self.admission,
                        &self.limits,
                        &pinned.model,
                        &cancel,
                    )
                    .await?
                }
                None => {
                    host.governed_turn(&self.tools, self.admission, &self.limits, &cancel)
                        .await?
                }
            };
            match turn {
                ReadOnlyTurn::Observed => {}
                ReadOnlyTurn::Completed { summary } => {
                    return Ok(WorkerOutcome::Completed { summary });
                }
            }
        }
    }
}
