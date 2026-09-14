//! Deliberately restricted, bounded tool schemas. Unknown keywords fail closed.
use super::{model_context, model_input};
use crate::contracts::{CorrelationId, ErrorCode, ErrorEnvelope};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum serialized schema or argument size accepted by the tool broker.
pub const TOOL_INPUT_BYTES: usize = 65_536;

/// Compiled M6 schema subset, with no references, regex engines or I/O.
/// Not a general JSON Schema implementation. Root arguments must be objects.
pub struct ToolInputSchema(Node);
struct Node {
    kind: Kind,
    properties: BTreeMap<String, Node>,
    required: BTreeSet<String>,
    additional: bool,
    items: Option<Box<Node>>,
    choices: Option<BTreeSet<String>>,
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Object,
    Array,
    String,
    Number,
    Integer,
    Boolean,
    Null,
}

impl std::fmt::Debug for ToolInputSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolInputSchema").finish_non_exhaustive()
    }
}

impl ToolInputSchema {
    /// Compile explicit types, properties, required, boolean additionalProperties,
    /// homogeneous array items, string enums, title and description. Other
    /// keywords/types/dialects (including all refs) are refused, never ignored.
    pub fn compile(schema: &Value) -> Result<Self, ErrorEnvelope> {
        model_input::json_limits(schema)?;
        model_input::bounded_size(schema, TOOL_INPUT_BYTES)?;
        if model_context::json_has_secret(schema) {
            return Err(error(ErrorCode::PermissionDenied));
        }
        let node = Node::compile(schema)?;
        if node.kind != Kind::Object {
            return Err(error(ErrorCode::InvalidRequest));
        }
        Ok(Self(node))
    }

    /// Validate without coercion, defaults or argument rewriting, before policy.
    pub fn validate(&self, arguments: &Value) -> Result<(), ErrorEnvelope> {
        model_input::json_limits(arguments)?;
        model_input::bounded_size(arguments, TOOL_INPUT_BYTES)?;
        if model_context::json_has_secret(arguments) {
            return Err(error(ErrorCode::PermissionDenied));
        }
        if !self.0.accepts(arguments) {
            return Err(error(ErrorCode::InvalidRequest));
        }
        Ok(())
    }
}

impl Node {
    fn compile(schema: &Value) -> Result<Self, ErrorEnvelope> {
        let object = schema
            .as_object()
            .ok_or_else(|| error(ErrorCode::InvalidRequest))?;
        for (key, value) in object {
            match key.as_str() {
                "type" | "properties" | "required" | "additionalProperties" | "items" | "enum" => {}
                "description" | "title" if value.is_string() => {}
                "description" | "title" => return Err(error(ErrorCode::InvalidRequest)),
                _ => return Err(error(ErrorCode::CapabilityUnsupported)),
            }
        }
        let kind = match schema.get("type").and_then(Value::as_str) {
            Some("object") => Kind::Object,
            Some("array") => Kind::Array,
            Some("string") => Kind::String,
            Some("number") => Kind::Number,
            Some("integer") => Kind::Integer,
            Some("boolean") => Kind::Boolean,
            Some("null") => Kind::Null,
            _ => return Err(error(ErrorCode::CapabilityUnsupported)),
        };
        let mut node = Self {
            kind,
            properties: BTreeMap::new(),
            required: BTreeSet::new(),
            additional: true,
            items: None,
            choices: None,
        };
        for key in ["properties", "required", "additionalProperties"] {
            if object.contains_key(key) && kind != Kind::Object {
                return Err(error(ErrorCode::InvalidRequest));
            }
        }
        if let Some(properties) = schema.get("properties") {
            for (key, value) in properties
                .as_object()
                .ok_or_else(|| error(ErrorCode::InvalidRequest))?
            {
                node.properties.insert(key.clone(), Self::compile(value)?);
            }
        }
        if let Some(required) = schema.get("required") {
            for name in required
                .as_array()
                .ok_or_else(|| error(ErrorCode::InvalidRequest))?
            {
                let name = name
                    .as_str()
                    .ok_or_else(|| error(ErrorCode::InvalidRequest))?;
                if !node.properties.contains_key(name) || !node.required.insert(name.into()) {
                    return Err(error(ErrorCode::InvalidRequest));
                }
            }
        }
        if let Some(additional) = schema.get("additionalProperties") {
            node.additional = additional
                .as_bool()
                .ok_or_else(|| error(ErrorCode::CapabilityUnsupported))?;
        }
        if kind == Kind::Array {
            node.items = Some(Box::new(Self::compile(
                schema
                    .get("items")
                    .ok_or_else(|| error(ErrorCode::CapabilityUnsupported))?,
            )?));
        } else if object.contains_key("items") {
            return Err(error(ErrorCode::InvalidRequest));
        }
        if let Some(choices) = schema.get("enum") {
            if kind != Kind::String {
                return Err(error(ErrorCode::CapabilityUnsupported));
            }
            let mut values = BTreeSet::new();
            for value in choices
                .as_array()
                .ok_or_else(|| error(ErrorCode::InvalidRequest))?
            {
                let value = value
                    .as_str()
                    .ok_or_else(|| error(ErrorCode::InvalidRequest))?;
                if !values.insert(value.into()) {
                    return Err(error(ErrorCode::InvalidRequest));
                }
            }
            if values.is_empty() {
                return Err(error(ErrorCode::InvalidRequest));
            }
            node.choices = Some(values);
        }
        Ok(node)
    }

    fn accepts(&self, value: &Value) -> bool {
        match (self.kind, value) {
            (Kind::Object, Value::Object(values)) => {
                self.required.iter().all(|key| values.contains_key(key))
                    && values.iter().all(|(key, value)| {
                        self.properties
                            .get(key)
                            .map_or(self.additional, |schema| schema.accepts(value))
                    })
            }
            (Kind::Array, Value::Array(values)) => self
                .items
                .as_ref()
                .is_some_and(|schema| values.iter().all(|value| schema.accepts(value))),
            (Kind::String, Value::String(text)) => self
                .choices
                .as_ref()
                .is_none_or(|choices| choices.contains(text)),
            (Kind::Number, Value::Number(_))
            | (Kind::Boolean, Value::Bool(_))
            | (Kind::Null, Value::Null) => true,
            (Kind::Integer, Value::Number(number)) => {
                number.is_i64()
                    || number.is_u64()
                    || number.as_f64().is_some_and(|n| n.fract() == 0.0)
            }
            _ => false,
        }
    }
}

fn error(code: ErrorCode) -> ErrorEnvelope {
    ErrorEnvelope::new(
        code,
        "Tool schema or arguments refused",
        CorrelationId::new("tool-schema"),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good schemas; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn validates_the_bounded_object_subset_without_coercion() {
        let schema = ToolInputSchema::compile(&serde_json::json!({
            "type":"object", "properties": {
                "path": {"type":"string"},
                "limit": {"type":"integer"},
                "mode": {"type":"string", "enum":["fast","safe"]}
            }, "required":["path"], "additionalProperties":false
        }))
        .unwrap();
        assert!(
            schema
                .validate(&serde_json::json!({"path":"README.md","limit":2,"mode":"safe"}))
                .is_ok()
        );
        for value in [
            serde_json::json!({}),
            serde_json::json!({"path":3}),
            serde_json::json!({"path":"README.md","extra":true}),
            serde_json::json!({"path":"README.md","mode":"other"}),
            serde_json::json!({"path":"README.md","limit":1.5}),
        ] {
            assert_eq!(
                schema.validate(&value).unwrap_err().code,
                ErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn supports_nested_arrays_and_rejects_unsupported_keywords_or_types() {
        let schema = ToolInputSchema::compile(&serde_json::json!({"type":"object","properties":{"paths":{"type":"array","items":{"type":"string"}}}})).unwrap();
        assert!(
            schema
                .validate(&serde_json::json!({"paths":["a","b"]}))
                .is_ok()
        );
        for value in [
            serde_json::json!({"type":"object","properties":{},"additionalProperties":true,"$ref":"#/x"}),
            serde_json::json!({"type":"object","properties":{"x":{"type":"string","pattern":".*"}}}),
            serde_json::json!({"type":"array","items":{"type":"string"}}),
            serde_json::json!({"type":"object","properties":{"x":{"type":"string"}},"required":["missing"]}),
        ] {
            assert!(ToolInputSchema::compile(&value).is_err());
        }
    }

    #[test]
    fn schema_and_arguments_are_bounded_and_secret_free() {
        let giant = serde_json::json!({"type":"object","description":"x".repeat(TOOL_INPUT_BYTES)});
        assert_eq!(
            ToolInputSchema::compile(&giant).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
        let secret = serde_json::json!({"type":"object","description":"API_KEY=fixture-secret"});
        assert_eq!(
            ToolInputSchema::compile(&secret).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        let schema = ToolInputSchema::compile(
            &serde_json::json!({"type":"object","properties":{"value":{"type":"string"}}}),
        )
        .unwrap();
        assert_eq!(
            schema
                .validate(&serde_json::json!({"value":"API_KEY=fixture-secret"}))
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
    }
}
