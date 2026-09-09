use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AgentDescriptor, CheckpointSupport, ErrorEnvelope, EventDraft, EventPayload, HandoffBundle,
    IntegrationMode, ModelControl, NetworkControl, TaskEventKind, ToolControl,
};
use crate::ports::{AgentProvider, WorkerHost, WorkerOutcome, WorkerRequest};
use crate::testing::tool::FAKE_TOOL_ID;

/// The identifier of the fake worker that takes a task over.
pub const RECEIVING_AGENT_ID: &str = "fake.receiving";

/// Diagnostic code for the read-back a receiving worker records.
pub const HANDOFF_RECEIVED: &str = "handoff.received";

/// A second worker, with different capabilities, that continues someone
/// else's task from canonical state alone.
///
/// This exists to make validation gate V2's fifth step a test rather than a
/// judgement: *"configure a second fake worker with different capabilities,
/// hand the task to it from canonical state, and confirm it receives a
/// coherent goal, history, and context without the first worker's private
/// data"* (`BUILD_ORDER.md` §13).
///
/// It differs from [`FakeWorker`](crate::testing::FakeWorker) in ways the
/// record can see: a different identity, and `DirectConstrained` tool control
/// rather than fully brokered. The second difference is the interesting one —
/// it means the two workers do not have the same standing, so a handoff that
/// silently assumed the receiver was identical to the sender would show up.
///
/// What it deliberately does *not* do is read anything private to the previous
/// worker. It is handed a [`HandoffBundle`] and nothing else, and it refuses
/// to run without one, because a receiving worker that could fall back on
/// starting from scratch would make the canonical-state claim untestable: the
/// task would finish either way and nobody would learn whether the record was
/// sufficient.
#[derive(Debug)]
pub struct ReceivingWorker {
    /// The tool it continues with.
    ///
    /// Configurable for the same reason [`FakeWorker`](crate::testing::FakeWorker)
    /// is: this worker lives in `core`, and the real read-only tools live in
    /// an adapter crate `core` must not depend on. To continue a task without
    /// repeating a step, it has to reconstruct the *same* call the previous
    /// worker made — so a deployment points it at the same real tool.
    tool_id: String,
    /// The arguments it sends, which must match what the record holds.
    arguments: serde_json::Value,
}

impl Default for ReceivingWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceivingWorker {
    /// A worker that continues a handed-over task through the echo fake.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_id: FAKE_TOOL_ID.to_owned(),
            arguments: serde_json::Value::Null,
        }
    }

    /// A worker that continues by calling a named tool.
    #[must_use]
    pub fn calling(tool_id: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            tool_id: tool_id.into(),
            arguments,
        }
    }

    /// What this worker understood from the record it was given.
    ///
    /// Written into the durable stream so "did it receive a coherent goal and
    /// history?" is answered by the task's own record rather than by reading
    /// the receiving worker's code.
    #[must_use]
    fn read_back(handoff: &HandoffBundle) -> String {
        let mut lines = vec![format!("Continuing: {}", handoff.goal)];
        lines.push(format!(
            "Inherited {} event(s) of history from {}.",
            handoff.history.len(),
            handoff
                .previous_worker
                .as_ref()
                .map_or("an unnamed worker", |worker| worker.display_name.as_str()),
        ));
        if handoff.completed_step_ids.is_empty() {
            lines.push("Nothing had been completed.".to_owned());
        } else {
            lines.push(format!(
                "Already done, not repeating: {}.",
                handoff.completed_step_ids.join(", ")
            ));
        }
        match &handoff.pending_action {
            Some(action) => lines.push(format!(
                "Was in flight: {} ({:?} to repeat).",
                action.display_summary, action.replay_safety
            )),
            None => lines.push("Nothing was in flight.".to_owned()),
        }
        lines.join(" ")
    }
}

#[async_trait]
impl AgentProvider for ReceivingWorker {
    fn describe(&self) -> AgentDescriptor {
        let model_control = ModelControl::TramontanaBrokered;
        // Deliberately different standing from the reference worker. Local
        // Only eligibility is still computed by the core, never asserted here
        // (`CORE_CONTRACTS.md` §7.2).
        let network_control = NetworkControl::Brokered;
        AgentDescriptor {
            agent_provider_id: "fake".to_owned(),
            agent_id: RECEIVING_AGENT_ID.to_owned(),
            display_name: "Receiving fake worker".to_owned(),
            integration_mode: IntegrationMode::Embedded,
            model_control,
            tool_control: ToolControl::DirectConstrained,
            network_control,
            checkpoint_support: CheckpointSupport::Portable,
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
        let Some(handoff) = request.handoff.as_ref() else {
            return Err(ErrorEnvelope::new(
                crate::contracts::ErrorCode::InvalidRequest,
                "this worker only continues handed-over work and was given no handoff",
                crate::contracts::CorrelationId::new("handoff"),
            ));
        };

        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        // Say what was understood before doing anything with it. If the bundle
        // is not enough to continue, this line is where that becomes visible.
        //
        // Recorded as a diagnostic, not as an assistant message: the host
        // permits a worker to emit only model-start and diagnostic
        // observations, so a worker cannot write a canonical message into
        // someone else's task. Being refused that is the boundary working, and
        // a diagnostic is what `CORE_CONTRACTS.md` §5.1 reserves for a
        // bounded, redacted note like this one.
        let understood = Self::read_back(handoff);
        host.emit(EventDraft::user(
            TaskEventKind::DiagnosticRecorded,
            EventPayload::DiagnosticRecorded {
                code: HANDOFF_RECEIVED.to_owned(),
                summary: understood.clone(),
            },
        ))
        .await?;

        if cancel.is_cancelled() {
            return Ok(WorkerOutcome::Cancelled);
        }

        // Continue the work under the *same* step identity the previous worker
        // used. A different one would slip past the completed-step watermark
        // and repeat an effect, which is exactly what a handoff must not do.
        // The echo fake wants the goal back; a real tool gets exactly what it
        // was configured with. Either way the arguments must match what the
        // record holds, or the core refuses the step rather than repeating it.
        let arguments = if self.tool_id == FAKE_TOOL_ID {
            serde_json::json!({ "value": handoff.goal.clone() })
        } else {
            self.arguments.clone()
        };
        let result = host
            .call_tool_step("reference-tool", &self.tool_id, arguments, &cancel)
            .await?;

        let repeated = result
            .get("skipped")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Ok(WorkerOutcome::Completed {
            summary: format!(
                "{understood} {}",
                if repeated {
                    "Reused the recorded result instead of running it again."
                } else {
                    "Ran the remaining step."
                }
            ),
        })
    }
}
