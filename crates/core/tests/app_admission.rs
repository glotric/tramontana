//! What a task's model may be offered from an App (ADRs 0035 and 0036).
//!
//! Registration says a tool exists; admission says whether a model is told
//! about it; permission says whether it may run. These are the middle rule.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, ErrorEnvelope, ProviderId, ReplaySafety, RiskClass, SideEffect,
    ToolDescriptor,
};
use tramontana_core::ports::{ToolAdmission, ToolOutcome, ToolProvider};
use tramontana_core::services::{ModelToolCatalogue, ToolRegistry};

/// An App offering one tool it says only reads, and one it says nothing about.
#[derive(Debug)]
struct Notes;

fn tool(tool_id: &str, risk_class: RiskClass, side_effect: SideEffect) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_owned(),
        provider_id: "mcp.notes".to_owned(),
        display_name: tool_id.to_owned(),
        description: "a tool of an App".to_owned(),
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
        risk_class,
        side_effect,
        capability: "mcp.notes".to_owned(),
        supports_cancellation: true,
        replay_safety: ReplaySafety::Unknown,
    }
}

#[async_trait]
impl ToolProvider for Notes {
    fn id(&self) -> ProviderId {
        ProviderId::new("mcp.notes")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![
            tool("mcp.notes.search", RiskClass::Read, SideEffect::None),
            tool(
                "mcp.notes.stamp",
                RiskClass::Dangerous,
                SideEffect::External,
            ),
        ]
    }

    async fn invoke(
        &self,
        _request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        unreachable!("admission tests never invoke")
    }
}

/// A built-in read, to show the rule is about Apps and not about reads.
#[derive(Debug)]
struct Builtin;

#[async_trait]
impl ToolProvider for Builtin {
    fn id(&self) -> ProviderId {
        ProviderId::new("builtin")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            provider_id: "builtin".to_owned(),
            capability: "filesystem.read".to_owned(),
            replay_safety: ReplaySafety::Safe,
            ..tool("builtin.fs.read", RiskClass::Read, SideEffect::None)
        }]
    }

    async fn invoke(
        &self,
        _request: AuthorizedToolRequest,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        unreachable!("admission tests never invoke")
    }
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(Builtin) as Arc<dyn ToolProvider>)
        .unwrap();
    registry
        .register(Arc::new(Notes) as Arc<dyn ToolProvider>)
        .unwrap();
    registry
}

#[test]
fn an_apps_tool_is_not_offered_to_a_task_that_did_not_admit_apps() {
    let registry = registry();
    assert!(
        ModelToolCatalogue::select(&registry, &["mcp.notes.search"], ToolAdmission::READ_ONLY)
            .is_err(),
        "an App's tool was offered without the person admitting Apps"
    );
    assert!(
        ModelToolCatalogue::select(
            &registry,
            &["mcp.notes.search"],
            ToolAdmission::READ_WRITE.with_recipes(),
        )
        .is_err(),
        "writes or recipes admitted an App's tool"
    );
}

#[test]
fn admitting_apps_offers_the_tools_an_app_says_only_read() {
    let registry = registry();
    let catalogue = ModelToolCatalogue::select(
        &registry,
        &["builtin.fs.read", "mcp.notes.search"],
        ToolAdmission::READ_ONLY.with_apps(),
    )
    .unwrap();

    let names: Vec<&str> = catalogue
        .definitions()
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(names, ["builtin.fs.read", "mcp.notes.search"]);
}

#[test]
fn admitting_apps_does_not_offer_a_tool_that_claims_nothing() {
    let registry = registry();
    assert!(
        ModelToolCatalogue::select(
            &registry,
            &["mcp.notes.stamp"],
            ToolAdmission::READ_ONLY.with_apps(),
        )
        .is_err(),
        "a tool whose effect the App did not declare was offered"
    );
    // And it takes the whole selection with it, rather than being dropped:
    // a task is never quietly given less than it asked for.
    assert!(
        ModelToolCatalogue::select(
            &registry,
            &["mcp.notes.search", "mcp.notes.stamp"],
            ToolAdmission::READ_ONLY.with_apps(),
        )
        .is_err()
    );
}

#[test]
fn admitting_apps_changes_nothing_about_built_in_tools() {
    let registry = registry();
    for admission in [
        ToolAdmission::READ_ONLY,
        ToolAdmission::READ_ONLY.with_apps(),
    ] {
        let catalogue =
            ModelToolCatalogue::select(&registry, &["builtin.fs.read"], admission).unwrap();
        assert_eq!(catalogue.definitions().len(), 1);
    }
}
