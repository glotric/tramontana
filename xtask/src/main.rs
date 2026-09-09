//! Repository build tooling.
//!
//! Currently one job: emit the committed JSON Schema for the contract types in
//! `tramontana-core`. This lives outside `apps/cli` so build tooling never
//! ships inside the product binary, and outside `crates/core` so the domain
//! crate has no binary targets.
//!
//! `pnpm contracts:generate` runs this and then converts the schema to
//! TypeScript. `pnpm contracts:check` runs the same pipeline into a temporary
//! directory and fails when the committed artifacts are stale.

mod fixtures;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, SchemaGenerator, generate::SchemaSettings};
use tramontana_core::contracts::{
    AgentDescriptor, CheckpointWorker, ConfigurationDiagnostic, ConfigurationKind,
    ConfigurationSource, ConversationRecord, DataClass, DiagnosticSeverity, EffectiveConfiguration,
    ErrorCode, ErrorEnvelope, EventEnvelope, EventPayload, ExecutionLocation, ModelDescriptor,
    NormalizedMessage, PendingAction, PendingActionKind, ProjectRecord, ReplaySafety,
    ResumeEligibility, ResumeMode, RiskClass, RoutingMode, SecretRef, SecretStore, SideEffect,
    StreamFrame, TaskCheckpoint, TaskEventKind, TaskRecord, TaskSnapshot, TaskState, TaskUpdate,
    ToolDescriptor, Visibility,
};

/// Filename of the single bundled contract schema.
///
/// One document rather than one file per type: the contracts share primitives
/// heavily, and a single `$defs` map keeps every reference resolvable without
/// cross-file `$ref` handling in the TypeScript generator.
const SCHEMA_FILE: &str = "tramontana-contracts.schema.json";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_else(|| "help".to_owned());
    let out_dir = args.next().map(PathBuf::from);

    match task.as_str() {
        "emit-schema" => {
            let dir = out_dir.unwrap_or_else(|| workspace_root().join("packages/shared/schema"));
            let written = emit_schema(&dir)?;
            println!("{}", written.display());
            Ok(())
        }
        "emit-fixtures" => {
            let dir =
                out_dir.unwrap_or_else(|| workspace_root().join("tests/fixtures/contracts/v1"));
            for written in fixtures::emit_fixtures(&dir)? {
                println!("{}", written.display());
            }
            Ok(())
        }
        "help" | "-h" | "--help" => {
            println!("usage: cargo run -p xtask -- <emit-schema|emit-fixtures> [OUT_DIR]");
            Ok(())
        }
        other => bail!("unknown xtask `{other}`; try `emit-schema` or `emit-fixtures`"),
    }
}

/// Repository root, derived from this crate's manifest location.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Register every contract type and write the bundled schema into `dir`.
fn emit_schema(dir: &Path) -> Result<PathBuf> {
    // The serialize contract honors `skip_serializing_if`, so an absent
    // `Option` becomes a non-required property rather than a nullable one.
    // `CORE_CONTRACTS.md` §2 reserves `null` for "known empty" differing from
    // absent, and a schema that allows both would licence exactly the
    // ambiguity that rule exists to prevent.
    let mut generator = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();

    // Registering a type places it in `$defs` and returns a `$ref` we discard;
    // the generated TypeScript is produced from the definitions, not a root.
    fn register<T: JsonSchema>(generator: &mut SchemaGenerator) {
        let _ = generator.subschema_for::<T>();
    }

    register::<DataClass>(&mut generator);
    register::<Visibility>(&mut generator);
    register::<RiskClass>(&mut generator);
    register::<SideEffect>(&mut generator);
    register::<RoutingMode>(&mut generator);
    register::<SecretStore>(&mut generator);
    register::<SecretRef>(&mut generator);
    register::<ExecutionLocation>(&mut generator);

    register::<TaskState>(&mut generator);
    register::<TaskSnapshot>(&mut generator);
    register::<ErrorCode>(&mut generator);
    register::<ErrorEnvelope>(&mut generator);
    register::<TaskEventKind>(&mut generator);
    register::<EventPayload>(&mut generator);
    register::<EventEnvelope>(&mut generator);
    register::<StreamFrame>(&mut generator);
    register::<TaskUpdate>(&mut generator);
    register::<NormalizedMessage>(&mut generator);
    register::<ModelDescriptor>(&mut generator);
    register::<AgentDescriptor>(&mut generator);
    register::<ToolDescriptor>(&mut generator);
    register::<ProjectRecord>(&mut generator);
    register::<ConversationRecord>(&mut generator);
    register::<TaskRecord>(&mut generator);
    register::<PendingActionKind>(&mut generator);
    register::<ReplaySafety>(&mut generator);
    register::<PendingAction>(&mut generator);
    register::<ResumeMode>(&mut generator);
    register::<ResumeEligibility>(&mut generator);
    register::<CheckpointWorker>(&mut generator);
    register::<TaskCheckpoint>(&mut generator);
    register::<ConfigurationKind>(&mut generator);
    register::<DiagnosticSeverity>(&mut generator);
    register::<ConfigurationDiagnostic>(&mut generator);
    register::<ConfigurationSource>(&mut generator);
    register::<EffectiveConfiguration>(&mut generator);

    let mut defs = generator.take_definitions(true);
    for schema in defs.values_mut() {
        drop_null_from_optional_properties(schema);
    }

    let document = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://tramontana.app/schema/tramontana-contracts.schema.json",
        "title": "TramontanaContracts",
        "description": concat!(
            "Generated from crates/core/src/contracts. Do not edit by hand; ",
            "run `pnpm contracts:generate`.",
        ),
        "x-schemaVersion": tramontana_core::SCHEMA_VERSION,
        "$defs": defs,
    });

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating schema directory {}", dir.display()))?;
    let path = dir.join(SCHEMA_FILE);
    let mut json = serde_json::to_string_pretty(&document)?;
    json.push('\n');
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Remove `"null"` from the type of any property that is already optional.
///
/// schemars maps `Option<T>` to a nullable type, but with
/// `skip_serializing_if = "Option::is_none"` an absent value is simply absent
/// from the JSON. `CORE_CONTRACTS.md` §2 reserves `null` for "known empty"
/// differing from absent, so a schema permitting both would licence exactly
/// the ambiguity that rule exists to prevent.
///
/// A property that is genuinely nullable stays nullable by being `required`.
fn drop_null_from_optional_properties(schema: &mut serde_json::Value) {
    if let Some(object) = schema.as_object_mut() {
        let required: Vec<String> = object
            .get("required")
            .and_then(|value| value.as_array())
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        if let Some(properties) = object.get_mut("properties").and_then(|p| p.as_object_mut()) {
            for (name, property) in properties.iter_mut() {
                if !required.contains(name) {
                    remove_null_type(property);
                }
            }
        }

        for value in object.values_mut() {
            drop_null_from_optional_properties(value);
        }
    } else if let Some(items) = schema.as_array_mut() {
        for value in items {
            drop_null_from_optional_properties(value);
        }
    }
}

/// Collapse `"type": ["x", "null"]` to `"type": "x"` on one schema node.
fn remove_null_type(schema: &mut serde_json::Value) {
    let Some(types) = schema.get_mut("type").and_then(|t| t.as_array_mut()) else {
        return;
    };
    types.retain(|entry| entry.as_str() != Some("null"));
    if types.len() == 1 {
        let only = types[0].clone();
        if let Some(object) = schema.as_object_mut() {
            object.insert("type".to_owned(), only);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn emitted_schema_contains_every_registered_contract() {
        let dir = std::env::temp_dir().join(format!("tramontana-schema-{}", std::process::id()));
        let path = emit_schema(&dir).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        let defs = value["$defs"].as_object().expect("$defs is an object");
        for name in [
            "DataClass",
            "Visibility",
            "RiskClass",
            "SideEffect",
            "RoutingMode",
            "SecretStore",
            "SecretRef",
            "ExecutionLocation",
            "TaskState",
            "TaskSnapshot",
            "ErrorCode",
            "ErrorEnvelope",
            "TaskEventKind",
            "EventPayload",
            "EventEnvelope",
            "StreamFrame",
            "TaskUpdate",
            "NormalizedMessage",
            "ModelDescriptor",
            "AgentDescriptor",
            "ToolDescriptor",
            "ProjectRecord",
            "ConversationRecord",
            "TaskRecord",
            "PendingActionKind",
            "ReplaySafety",
            "PendingAction",
            "ResumeMode",
            "ResumeEligibility",
            "CheckpointWorker",
            "TaskCheckpoint",
            "ConfigurationKind",
            "DiagnosticSeverity",
            "ConfigurationDiagnostic",
            "ConfigurationSource",
            "EffectiveConfiguration",
        ] {
            assert!(defs.contains_key(name), "missing definition for {name}");
        }
        assert_eq!(value["x-schemaVersion"], tramontana_core::SCHEMA_VERSION);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn optional_properties_are_absent_rather_than_nullable() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "kept": { "type": ["string", "null"] },
                "optional": { "type": ["string", "null"] },
            },
            "required": ["kept"],
        });
        drop_null_from_optional_properties(&mut schema);

        // Required means the field is always present, so an explicit null there
        // is a real "known empty" and is preserved.
        assert_eq!(
            schema["properties"]["kept"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert_eq!(
            schema["properties"]["optional"]["type"],
            serde_json::json!("string")
        );
    }
}
