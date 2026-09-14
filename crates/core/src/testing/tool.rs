use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, ReplaySafety,
    RiskClass, SideEffect, ToolDescriptor,
};
use crate::ports::{ToolOutcome, ToolProvider};

/// The identifier of the fake read-only tool.
pub const FAKE_TOOL_ID: &str = "fake.tool.echo";

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
            replay_safety: ReplaySafety::Safe,
        }]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        if cancel.is_cancelled() {
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "the tool call was cancelled before it ran",
                CorrelationId::new("fake"),
            ));
        }
        if request.tool_id() != FAKE_TOOL_ID {
            return Err(ErrorEnvelope::new(
                ErrorCode::NotFound,
                format!("unknown tool {}", request.tool_id()),
                CorrelationId::new("fake"),
            ));
        }

        let value = request
            .arguments()
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

/// The identifier of the fake write tool.
pub const FAKE_WRITE_TOOL_ID: &str = "fake.tool.write";

/// A write tool whose replay safety is whatever the test declares.
///
/// It exists to prove the broker takes replay safety from a tool's
/// declaration rather than inferring it from the side effect: this tool always
/// has one. Its behaviour is idempotent either way — writing the same value
/// under the same key a second time changes nothing and says so — so a `safe`
/// declaration is honest and passes
/// [`idempotent_tool_contract`](super::contract_suites::idempotent_tool_contract),
/// while an `unsafe` one lets a test show that recovery refuses to replay it
/// regardless of how the tool would have behaved.
#[derive(Debug)]
pub struct FakeWriteTool {
    declared: ReplaySafety,
    values: Mutex<HashMap<String, String>>,
    invocations: AtomicUsize,
}

impl FakeWriteTool {
    /// A write tool declaring `declared`.
    #[must_use]
    pub fn declaring(declared: ReplaySafety) -> Self {
        Self {
            declared,
            values: Mutex::new(HashMap::new()),
            invocations: AtomicUsize::new(0),
        }
    }

    /// How many times it has been invoked.
    #[must_use]
    pub fn invocations(&self) -> usize {
        self.invocations.load(Ordering::SeqCst)
    }

    /// The value last written under `key`.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<String> {
        self.values
            .lock()
            .ok()
            .and_then(|values| values.get(key).cloned())
    }
}

#[async_trait]
impl ToolProvider for FakeWriteTool {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake-write")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: FAKE_WRITE_TOOL_ID.to_owned(),
            provider_id: "fake-write".to_owned(),
            display_name: "Write value".to_owned(),
            description: "Stores a value under a key.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "value": { "type": "string" },
                },
                "required": ["key", "value"],
                "additionalProperties": false,
            }),
            risk_class: RiskClass::Modify,
            side_effect: SideEffect::LocalReversible,
            capability: "fake.write".to_owned(),
            supports_cancellation: true,
            replay_safety: self.declared,
        }]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        if cancel.is_cancelled() {
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "the tool call was cancelled before it ran",
                CorrelationId::new("fake-write"),
            ));
        }
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let field = |name: &str| {
            request
                .arguments()
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (key, value) = (field("key"), field("value"));
        let mut values = self.values.lock().map_err(|_| {
            ErrorEnvelope::new(
                ErrorCode::Internal,
                "the fake store is poisoned",
                CorrelationId::new("fake-write"),
            )
        })?;
        if values.get(&key) == Some(&value) {
            return Ok(ToolOutcome {
                content: serde_json::json!({ "key": key, "alreadyApplied": true }),
                display_summary: format!("{key} already holds this value"),
                side_effect: SideEffect::None,
                truncated: false,
            });
        }
        values.insert(key.clone(), value);
        Ok(ToolOutcome {
            content: serde_json::json!({ "key": key, "alreadyApplied": false }),
            display_summary: format!("Wrote {key}"),
            side_effect: SideEffect::LocalReversible,
            truncated: false,
        })
    }
}
