//! The honesty suite for tools that declare safe replay has teeth.
//!
//! `idempotent_tool_contract` is how a tool's `replaySafety: safe` claim is
//! checked (ADR 0019). A suite that every tool passes proves nothing about the
//! tools, so this file shows it passing an honest tool, failing one that
//! over-claims, and refusing to judge a tool that makes no claim.

#![allow(clippy::unwrap_used, clippy::panic)]

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, ErrorEnvelope, ProviderId, ReplaySafety, SideEffect, ToolDescriptor,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};
use tramontana_core::testing::contract_suites::idempotent_tool_contract;
use tramontana_core::testing::{FAKE_WRITE_TOOL_ID, FakeWriteTool};

fn roots() -> Vec<String> {
    vec!["/synthetic".to_owned()]
}

fn arguments() -> serde_json::Value {
    serde_json::json!({ "key": "greeting", "value": "hello" })
}

#[tokio::test]
async fn an_honest_idempotent_write_passes() {
    let tool = FakeWriteTool::declaring(ReplaySafety::Safe);
    idempotent_tool_contract(&tool, FAKE_WRITE_TOOL_ID, arguments(), roots()).await;
    assert_eq!(tool.value("greeting").as_deref(), Some("hello"));
    assert_eq!(
        tool.invocations(),
        3,
        "the suite must actually replay the step"
    );
}

/// Declares safe replay and appends on every call: exactly the write that
/// must not make that claim.
#[derive(Debug, Default)]
struct OverclaimingTool;

#[async_trait]
impl ToolProvider for OverclaimingTool {
    fn id(&self) -> ProviderId {
        ProviderId::new("overclaiming")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        let mut tools = FakeWriteTool::declaring(ReplaySafety::Safe).list_tools();
        tools[0].provider_id = "overclaiming".to_owned();
        tools
    }

    async fn invoke(
        &self,
        _request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        Ok(ToolOutcome {
            content: serde_json::json!({ "appended": true }),
            display_summary: "Appended a line".to_owned(),
            side_effect: SideEffect::LocalReversible,
            truncated: false,
        })
    }
}

#[tokio::test]
#[should_panic(expected = "a replay changed something again")]
async fn a_write_that_overclaims_safe_replay_fails() {
    idempotent_tool_contract(&OverclaimingTool, FAKE_WRITE_TOOL_ID, arguments(), roots()).await;
}

#[tokio::test]
#[should_panic(expected = "does not declare safe replay")]
async fn a_tool_that_makes_no_claim_is_not_judged_as_though_it_had() {
    let tool = FakeWriteTool::declaring(ReplaySafety::Unsafe);
    idempotent_tool_contract(&tool, FAKE_WRITE_TOOL_ID, arguments(), roots()).await;
}
