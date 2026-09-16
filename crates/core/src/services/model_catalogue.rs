//! Explicit, bounded model-facing selection; a catalogue is never permission.
use std::collections::BTreeSet;

use crate::contracts::{
    CorrelationId, DataClass, ErrorCode, ErrorEnvelope, NormalizedToolDefinition, RiskClass,
    SideEffect, ToolDescriptor,
};

use crate::ports::ToolAdmission;

use super::{ModelContentProvenance, RECIPE_TOOL_ID, ToolRegistry, model_context, model_input};

/// Definitions selected by core from a registry snapshot, not supplied by a worker.
/// No provider handles, grants, arguments or execution methods are exposed.
pub struct ModelToolCatalogue {
    pub(super) definitions: Vec<NormalizedToolDefinition>,
}

impl std::fmt::Debug for ModelToolCatalogue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelToolCatalogue")
            .field("count", &self.definitions.len())
            .finish_non_exhaustive()
    }
}

impl ModelToolCatalogue {
    /// [`select`](Self::select) at the read-only level: cancellable reads with
    /// no side effects, the M6 catalogue.
    pub fn select_read_only(
        registry: &ToolRegistry,
        selected: &[&str],
    ) -> Result<Self, ErrorEnvelope> {
        Self::select(registry, selected, ToolAdmission::READ_ONLY)
    }

    /// Select exactly these tools in stable canonical-name order. Empty means
    /// no tools, never all registered tools. Every tool must be cancellable and
    /// allowed at `admission`; one that is not refuses the whole selection
    /// rather than being dropped from it. Selection does not evaluate or bypass
    /// permissions: an offered write is still asked about each time it runs.
    /// Input schemas are bounded object schemas; argument-schema validation is
    /// a separate execution-boundary responsibility, not performed here.
    pub fn select(
        registry: &ToolRegistry,
        selected: &[&str],
        admission: ToolAdmission,
    ) -> Result<Self, ErrorEnvelope> {
        if selected.len() > 64 {
            return Err(error(ErrorCode::LimitExceeded));
        }
        let names: BTreeSet<_> = selected.iter().copied().collect();
        if names.len() != selected.len() {
            return Err(error(ErrorCode::InvalidRequest));
        }
        let mut descriptors = Vec::new();
        for name in names {
            let registered = registry
                .get(name)
                .ok_or_else(|| error(ErrorCode::NotFound))?;
            let descriptor = &registered.descriptor;
            if !admits(admission, descriptor)
                || !descriptor.supports_cancellation
                || descriptor.provider_id != registered.provider.id().as_str()
            {
                return Err(error(ErrorCode::CapabilityUnsupported));
            }
            if name.len() > 256
                || descriptor
                    .input_schema
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    != Some("object")
            {
                return Err(error(ErrorCode::InvalidRequest));
            }
            model_input::json_limits(&descriptor.input_schema)?;
            super::ToolInputSchema::compile(&descriptor.input_schema)?;
            descriptors.push(descriptor);
        }
        // Bound the entire selection before copying untrusted descriptions/schemas.
        model_input::input_size(&descriptors)?;
        for descriptor in &descriptors {
            let value =
                serde_json::to_value(descriptor).map_err(|_| error(ErrorCode::InvalidRequest))?;
            if model_context::json_has_secret(&value) {
                return Err(error(ErrorCode::PermissionDenied));
            }
        }
        Ok(Self {
            definitions: descriptors
                .into_iter()
                .map(|descriptor| NormalizedToolDefinition {
                    name: descriptor.tool_id.clone(),
                    description: descriptor.description.clone(),
                    input_schema: descriptor.input_schema.clone(),
                })
                .collect(),
        })
    }

    /// The selected provider-neutral definitions. Names remain canonical, not
    /// provider wire aliases. Metadata is untrusted content, not instructions.
    #[must_use]
    pub fn definitions(&self) -> &[NormalizedToolDefinition] {
        &self.definitions
    }

    pub(super) fn provenance(&self) -> Vec<ModelContentProvenance> {
        self.definitions
            .iter()
            .map(|definition| ModelContentProvenance {
                source_id: format!(
                    "tool-definition.{}",
                    crate::contracts::content_hash(&definition.name)
                ),
                // Registry metadata has no sensitivity field. Never assume that a
                // description/schema (including an App's) is public information.
                data_class: DataClass::Sensitive,
                redactions: Vec::new(),
            })
            .collect()
    }
}

fn error(code: ErrorCode) -> ErrorEnvelope {
    ErrorEnvelope::new(
        code,
        "Model tool catalogue selection refused",
        CorrelationId::new("model-catalogue"),
    )
}

/// Whether this tool may be offered at `admission`.
///
/// Risk class and side effect must agree: a "modify" tool that declares an
/// external or irreversible effect is not a local write, whatever its risk
/// class says. Nothing dangerous, irreversible or external is offered at any
/// level (ADR 0021) — with one exception, matched by exact id and the built-in
/// provider so no other tool can borrow it: the project's configured recipes,
/// which a model can only name and a person approves on every run (ADR 0025).
fn admits(admission: ToolAdmission, descriptor: &ToolDescriptor) -> bool {
    if descriptor.tool_id == RECIPE_TOOL_ID && descriptor.provider_id == "builtin" {
        return admission.recipes;
    }
    if super::is_app_tool(&descriptor.tool_id) {
        // An App's tool reaches a model only in a task that admits Apps, and
        // only where the App says it only reads. The claim is the App's own,
        // so it decides what may be *offered*, never what may run: every call
        // is still asked about (ADRs 0035 and 0036).
        return admission.apps
            && descriptor.risk_class == RiskClass::Read
            && descriptor.side_effect == SideEffect::None;
    }
    match (descriptor.risk_class, descriptor.side_effect) {
        (RiskClass::Read, SideEffect::None) => true,
        (RiskClass::Modify, SideEffect::LocalReversible) => admission.writes,
        _ => false,
    }
}
