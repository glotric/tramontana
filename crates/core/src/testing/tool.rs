use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, RiskClass, SideEffect, ToolDescriptor,
};
use crate::ports::{ToolInvocation, ToolOutcome, ToolProvider};

/// The identifier of the fake read-only tool.
pub const FAKE_TOOL_ID: &str = "fake.echo";

/// A tool provider offering one read-only echo tool.
///
/// Read-only and side-effect free on purpose: M1 has no permission boundary
/// yet, so the skeleton must not be able to demonstrate anything that would
/// need one. Modifying tools arrive in M7, behind M3's policy.
#[derive(Debug, Default)]
pub struct FakeToolProvider;

impl FakeToolProvider {
    /// A provider offering the echo tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolProvider for FakeToolProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: FAKE_TOOL_ID.to_owned(),
            provider_id: "fake".to_owned(),
            display_name: "Echo".to_owned(),
            description: "Returns its argument unchanged.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false,
            }),
            risk_class: RiskClass::Read,
            side_effect: SideEffect::None,
            capability: "fake.echo".to_owned(),
            supports_cancellation: true,
        }]
    }

    async fn invoke(
        &self,
        invocation: ToolInvocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        if cancel.is_cancelled() {
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "the tool call was cancelled before it ran",
                CorrelationId::new("fake"),
            ));
        }
        if invocation.tool_id != FAKE_TOOL_ID {
            return Err(ErrorEnvelope::new(
                ErrorCode::NotFound,
                format!("unknown tool {}", invocation.tool_id),
                CorrelationId::new("fake"),
            ));
        }

        let value = invocation
            .arguments
            .get("value")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();

        Ok(ToolOutcome {
            content: serde_json::json!({ "echoed": value }),
            display_summary: format!("Echoed {} characters", value.len()),
            side_effect: SideEffect::None,
            truncated: false,
        })
    }
}
