#![allow(clippy::unwrap_used, reason = "assertions in tests")]
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tramontana_core::{contracts::*, ports::*, services::*};

struct Provider(Vec<ToolDescriptor>);
#[async_trait]
impl ToolProvider for Provider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fixture")
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.0.clone()
    }
    async fn invoke(
        &self,
        _: AuthorizedToolRequest,
        _: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        unreachable!("catalogue selection must never invoke tools")
    }
}
fn descriptor(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: name.into(),
        provider_id: "fixture".into(),
        display_name: "Fixture tool".into(),
        description: "Untrusted fixture metadata".into(),
        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        risk_class: RiskClass::Read,
        side_effect: SideEffect::None,
        capability: "filesystem.read".into(),
        supports_cancellation: true,
        replay_safety: tramontana_core::contracts::ReplaySafety::Safe,
    }
}
fn registry(tools: Vec<ToolDescriptor>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(Provider(tools))).unwrap();
    registry
}

#[test]
fn explicit_selection_is_stable_complete_and_grants_nothing() {
    let registry = registry(vec![
        descriptor("fixture.fs.read"),
        descriptor("fixture.fs.list"),
        descriptor("fixture.fs.search"),
    ]);
    assert!(
        ModelToolCatalogue::select_read_only(&registry, &[])
            .unwrap()
            .definitions()
            .is_empty()
    );
    let catalogue =
        ModelToolCatalogue::select_read_only(&registry, &["fixture.fs.read", "fixture.fs.list"])
            .unwrap();
    assert_eq!(
        catalogue
            .definitions()
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        vec!["fixture.fs.list", "fixture.fs.read"]
    );
    assert_eq!(
        catalogue.definitions()[1].input_schema,
        descriptor("fixture.fs.read").input_schema
    );
    assert_eq!(
        catalogue.definitions()[1].description,
        "Untrusted fixture metadata"
    );
    assert!(!format!("{catalogue:?}").contains("metadata"));
    // Merely building this value does not call the provider or any permission port.
}

#[test]
fn unknown_duplicate_and_excess_selections_fail_closed() {
    let registry = registry(vec![descriptor("fixture.fs.read")]);
    for (ids, expected) in [
        (vec!["fixture.fs.missing"], ErrorCode::NotFound),
        (vec!["fixture.fs.read"; 2], ErrorCode::InvalidRequest),
        (vec!["fixture.fs.read"; 65], ErrorCode::LimitExceeded),
    ] {
        assert_eq!(
            ModelToolCatalogue::select_read_only(&registry, &ids)
                .unwrap_err()
                .code,
            expected
        );
    }
}

#[test]
fn non_read_or_uncancellable_tools_cannot_enter_the_m6_catalogue() {
    for scenario in ["modify", "dangerous", "effect", "cancel"] {
        let mut tool = descriptor("fixture.fs.read");
        match scenario {
            "modify" => tool.risk_class = RiskClass::Modify,
            "dangerous" => tool.risk_class = RiskClass::Dangerous,
            "effect" => tool.side_effect = SideEffect::External,
            "cancel" => tool.supports_cancellation = false,
            _ => unreachable!(),
        }
        assert_eq!(
            ModelToolCatalogue::select_read_only(&registry(vec![tool]), &["fixture.fs.read"])
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnsupported
        );
    }
}

#[test]
fn read_write_admission_adds_local_reversible_writes_and_nothing_worse() {
    let mut write = descriptor("fixture.fs.write");
    write.risk_class = RiskClass::Modify;
    write.side_effect = SideEffect::LocalReversible;
    write.capability = "filesystem.write".into();
    let both = registry(vec![descriptor("fixture.fs.read"), write.clone()]);
    let catalogue = ModelToolCatalogue::select(
        &both,
        &["fixture.fs.write", "fixture.fs.read"],
        ToolAdmission::READ_WRITE,
    )
    .unwrap();
    assert_eq!(
        catalogue
            .definitions()
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        vec!["fixture.fs.read", "fixture.fs.write"]
    );
    // The same write, not asked for, is not offered.
    assert_eq!(
        ModelToolCatalogue::select(&both, &["fixture.fs.write"], ToolAdmission::READ_ONLY)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnsupported
    );
    // Risk and effect must both say "local and reversible": no pair that is
    // dangerous, irreversible, external or self-contradictory gets in.
    for (risk, effect) in [
        (RiskClass::Dangerous, SideEffect::LocalIrreversible),
        (RiskClass::Dangerous, SideEffect::LocalReversible),
        (RiskClass::Dangerous, SideEffect::External),
        (RiskClass::Dangerous, SideEffect::None),
        (RiskClass::Modify, SideEffect::LocalIrreversible),
        (RiskClass::Modify, SideEffect::External),
        (RiskClass::Modify, SideEffect::None),
        (RiskClass::Read, SideEffect::LocalReversible),
        (RiskClass::Read, SideEffect::External),
    ] {
        let mut tool = write.clone();
        tool.risk_class = risk;
        tool.side_effect = effect;
        assert_eq!(
            ModelToolCatalogue::select(
                &registry(vec![tool]),
                &["fixture.fs.write"],
                ToolAdmission::READ_WRITE
            )
            .unwrap_err()
            .code,
            ErrorCode::CapabilityUnsupported,
            "{risk:?} / {effect:?}"
        );
    }
    let mut stuck = write;
    stuck.supports_cancellation = false;
    assert_eq!(
        ModelToolCatalogue::select(
            &registry(vec![stuck]),
            &["fixture.fs.write"],
            ToolAdmission::READ_WRITE
        )
        .unwrap_err()
        .code,
        ErrorCode::CapabilityUnsupported
    );
}

#[test]
fn schemas_and_metadata_are_bounded_and_credential_free() {
    for scenario in [
        "schema",
        "depth",
        "size",
        "description-secret",
        "schema-secret",
    ] {
        let mut tool = descriptor("fixture.fs.read");
        match scenario {
            "schema" => tool.input_schema = json!({"type":"array"}),
            "depth" => {
                let mut nested = json!(null);
                for _ in 0..34 {
                    nested = json!({"nested":nested});
                }
                tool.input_schema["properties"] = nested;
            }
            "size" => tool.description = "x".repeat(MODEL_INPUT_BYTES),
            "description-secret" => tool.description = "API_KEY=fixture-secret".into(),
            "schema-secret" => tool.input_schema["description"] = json!("API_KEY=fixture-secret"),
            _ => unreachable!(),
        }
        let error =
            ModelToolCatalogue::select_read_only(&registry(vec![tool]), &["fixture.fs.read"])
                .unwrap_err();
        let expected = match scenario {
            "schema" => ErrorCode::InvalidRequest,
            "depth" | "size" => ErrorCode::LimitExceeded,
            _ => ErrorCode::PermissionDenied,
        };
        assert_eq!(error.code, expected, "{scenario}");
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }
}

#[test]
fn registration_rejects_duplicate_batch_ids_and_false_ownership_atomically() {
    for scenario in ["duplicate", "ownership"] {
        let mut registry = ToolRegistry::new();
        let first = descriptor("fixture.fs.read");
        let mut second = descriptor("fixture.fs.list");
        if scenario == "duplicate" {
            second.tool_id = first.tool_id.clone();
        } else {
            second.provider_id = "someone-else".into();
        }
        let error = registry
            .register(Arc::new(Provider(vec![first, second])))
            .unwrap_err();
        if scenario == "duplicate" {
            assert!(matches!(error, RegistryError::DuplicateTool { .. }));
        } else {
            assert!(matches!(error, RegistryError::ProviderMismatch { .. }));
        }
        assert!(registry.descriptors().is_empty());
    }
}

/// A provider under a chosen id, so a test can play the built-in one.
struct Named(&'static str, Vec<ToolDescriptor>);
#[async_trait]
impl ToolProvider for Named {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.0)
    }
    fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.1.clone()
    }
    async fn invoke(
        &self,
        _: AuthorizedToolRequest,
        _: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        unreachable!("catalogue selection must never invoke tools")
    }
}

fn dangerous(tool_id: &str, provider: &str) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.into(),
        provider_id: provider.into(),
        display_name: "Run".into(),
        description: "Runs a command.".into(),
        input_schema: json!({"type":"object","properties":{"recipe":{"type":"string"}},"required":["recipe"],"additionalProperties":false}),
        risk_class: RiskClass::Dangerous,
        side_effect: SideEffect::External,
        capability: "command.recipe".into(),
        supports_cancellation: true,
        replay_safety: ReplaySafety::Unsafe,
    }
}

#[test]
fn recipes_are_the_one_dangerous_tool_offered_and_only_when_admitted() {
    let mut builtin = ToolRegistry::new();
    builtin
        .register(Arc::new(Named(
            "builtin",
            vec![
                dangerous(RECIPE_TOOL_ID, "builtin"),
                dangerous("builtin.shell.run", "builtin"),
            ],
        )))
        .unwrap();
    let select = |registry: &ToolRegistry, tool: &str, admission: ToolAdmission| {
        ModelToolCatalogue::select(registry, &[tool], admission).map(|_| ())
    };
    let refused = |result: Result<(), ErrorEnvelope>| result.unwrap_err().code;

    // Offered only when recipes were asked for; writes do not bring them.
    assert!(
        select(
            &builtin,
            RECIPE_TOOL_ID,
            ToolAdmission::READ_ONLY.with_recipes()
        )
        .is_ok()
    );
    for admission in [ToolAdmission::READ_ONLY, ToolAdmission::READ_WRITE] {
        assert_eq!(
            refused(select(&builtin, RECIPE_TOOL_ID, admission)),
            ErrorCode::CapabilityUnsupported,
            "{admission:?}"
        );
    }
    // The exception is the recipe tool, not "dangerous tools".
    assert_eq!(
        refused(select(
            &builtin,
            "builtin.shell.run",
            ToolAdmission::READ_WRITE.with_recipes()
        )),
        ErrorCode::CapabilityUnsupported
    );
    // Nor can another provider borrow the id.
    let mut borrowed = ToolRegistry::new();
    borrowed
        .register(Arc::new(Named(
            "fixture",
            vec![dangerous(RECIPE_TOOL_ID, "fixture")],
        )))
        .unwrap();
    assert_eq!(
        refused(select(
            &borrowed,
            RECIPE_TOOL_ID,
            ToolAdmission::READ_WRITE.with_recipes()
        )),
        ErrorCode::CapabilityUnsupported
    );
}
