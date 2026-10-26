//! Preparing one bounded model turn from host-owned content and provenance.
//!
//! This is not an authority to collect files or trust a worker's classification.
//! The caller supplies only explicitly selected, already-redacted content and
//! its provenance. Routing and release remain separate decisions.

use std::collections::{BTreeMap, BTreeSet};

use crate::contracts::{
    ContextCategory, ContextManifest, ContextSource, CorrelationId, DataClass, DestinationClass,
    ErrorCode, ErrorEnvelope, ExecutionLocation, ModelRequest, RoutingDecision, RoutingMode,
    Timestamp,
};

use super::{AvailabilitySnapshot, RoutingInput, model_input_content, redact, route};

/// Host-owned provenance for a message or tool definition. A worker must not
/// choose sensitivity or relabel tool output as instructions.
#[derive(Debug, Clone)]
pub struct ModelContentProvenance {
    /// Stable, secret-free source identifier. Multipart messages add a part suffix.
    pub source_id: String,
    /// Sensitivity established at ingestion, never inferred from model output.
    pub data_class: DataClass,
    /// Redactions already applied at ingestion; carried even when a second scan finds none.
    pub redactions: Vec<String>,
}

/// A request plus provenance supplied by the core host, not by a worker.
pub struct ModelContextInput {
    /// Already minimized, normalized and redacted history and definitions.
    pub request: ModelRequest,
    /// Exactly one entry per message, in request order.
    pub messages: Vec<ModelContentProvenance>,
    /// Exactly one entry per tool definition, in request order.
    pub tools: Vec<ModelContentProvenance>,
}

/// One prepared turn. No provider can be called with this alone: the request
/// must still pass ReleaseManager, and the host must journal the decisions.
pub struct PreparedModelContext {
    /// Validated request with data classes derived from actual sources.
    pub request: ModelRequest,
    /// Fresh manifest covering every selected content unit.
    pub manifest: ContextManifest,
    /// Fresh route for this manifest, never a cloned earlier decision.
    pub decision: RoutingDecision,
}

opaque_debug!(ModelContextInput);

opaque_debug!(PreparedModelContext);

/// Assemble and route a turn without filesystem, provider or approval effects.
///
/// Rejects excess input rather than trimming calls away from their results.
/// Secret-shaped content is refused, never silently edited after validation.
/// The host must assign fresh request/context/decision IDs for every turn.
pub fn prepare_model_context(
    input: ModelContextInput,
    decision_id: impl Into<String>,
    mode: RoutingMode,
    snapshot: AvailabilitySnapshot,
    now: Timestamp,
) -> Result<PreparedModelContext, ErrorEnvelope> {
    prepare_model_context_on(input, decision_id, mode, snapshot, now, None)
}

/// [`prepare_model_context`] for a worker pinned to one model, which may use
/// that model or none (ADR 0048).
///
/// # Errors
///
/// As [`prepare_model_context`], and [`ErrorCode::ProviderUnavailable`] when
/// the required model cannot serve the request.
pub fn prepare_model_context_on(
    mut input: ModelContextInput,
    decision_id: impl Into<String>,
    mode: RoutingMode,
    snapshot: AvailabilitySnapshot,
    now: Timestamp,
    required: Option<&crate::contracts::RoutedModel>,
) -> Result<PreparedModelContext, ErrorEnvelope> {
    let correlation = CorrelationId::new("model-context");
    let invalid = || {
        ErrorEnvelope::new(
            ErrorCode::InvalidRequest,
            "Model context provenance is invalid",
            correlation.clone(),
        )
    };
    if input.messages.len() != input.request.messages.len()
        || input.tools.len() != input.request.tools.len()
    {
        return Err(invalid());
    }
    // A streamed host needs these on every turn, regardless of provider name.
    for capability in ["text", "streaming", "cancellation"] {
        if !input
            .request
            .required_capabilities
            .iter()
            .any(|value| value == capability)
        {
            input.request.required_capabilities.push(capability.into());
        }
    }
    let content = model_input_content(&input.request)?;
    // Check metadata bounds before cloning it or adding part suffixes.
    for source in input.messages.iter().chain(&input.tools) {
        if source.source_id.is_empty()
            || source.source_id.len() > 256
            || source.redactions.len() > 32
            || source.redactions.iter().any(|code| code.len() > 128)
        {
            return Err(invalid());
        }
    }
    let normalized = serde_json::to_value(&input.request).map_err(|_| invalid())?;
    if json_has_secret(&normalized) {
        return Err(ErrorEnvelope::new(
            ErrorCode::PermissionDenied,
            "Model context contains prohibited secret material",
            correlation.clone(),
        ));
    }
    let mut provenance = Vec::new();
    for (message, source) in input.request.messages.iter().zip(&input.messages) {
        let category = match message.role {
            crate::contracts::MessageRole::System => ContextCategory::Instruction,
            crate::contracts::MessageRole::Tool => ContextCategory::ToolResult,
            _ => ContextCategory::Conversation,
        };
        for index in 0..message.parts.len() {
            let source_id = if message.parts.len() == 1 {
                source.source_id.clone()
            } else {
                format!("{}.part-{index}", source.source_id)
            };
            provenance.push((source_id, category, source));
        }
    }
    for source in &input.tools {
        provenance.push((
            source.source_id.clone(),
            ContextCategory::RepositoryMetadata,
            source,
        ));
    }
    let mut ids = BTreeSet::new();
    let mut classifications = BTreeMap::new();
    let mut sources = Vec::new();
    for ((source_id, category, source), content) in provenance.into_iter().zip(content) {
        if source_id.is_empty()
            || source_id.len() > 256
            || source_id.chars().any(char::is_control)
            || !ids.insert(source_id.clone())
            || source.redactions.len() > 32
            || source.redactions.iter().any(|code| {
                code.len() > 128
                    || !code.starts_with("redaction.")
                    || !code
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
            })
        {
            return Err(invalid());
        }
        if source.data_class == DataClass::Secret
            || text_has_secret(&source_id)
            || source.redactions.iter().any(|code| text_has_secret(code))
        {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "Model context contains prohibited secret material",
                correlation.clone(),
            ));
        }
        let mut redactions = source.redactions.clone();
        redactions.sort_unstable();
        redactions.dedup();
        // Release deduplicates by content hash. Never let the first occurrence
        // conceal stronger sensitivity or different provenance on another.
        let classification = (category, source.data_class, redactions.clone());
        if classifications
            .insert(
                crate::contracts::content_hash(&content),
                classification.clone(),
            )
            .is_some_and(|previous| previous != classification)
        {
            return Err(invalid());
        }
        sources.push(ContextSource::redacted(
            source_id,
            category,
            source.data_class,
            &content,
            redactions,
        ));
    }
    let mut manifest = ContextManifest::new(
        input.request.context_manifest_id.clone(),
        input.request.task_id.clone(),
        DestinationClass::Local,
        sources,
        now,
    );
    input.request.data_classes = manifest.released_data_classes();
    let mut routing = RoutingInput::new(mode, snapshot, &manifest.context_manifest_id);
    routing.required_capabilities =
        super::model_input::required_model_capabilities(&input.request)?;
    routing.carries_private_data = input.request.carries_private_data();
    routing.required = required.cloned();
    let decision = route(decision_id, &routing).map_err(|refusal| {
        let code = if refusal
            .reason_codes
            .iter()
            .any(|reason| reason == crate::contracts::routing_reason::CAPABILITY_REQUIRED)
        {
            ErrorCode::CapabilityUnsupported
        } else {
            ErrorCode::ProviderUnavailable
        };
        ErrorEnvelope::new(
            code,
            format!("No model can serve this context: {refusal}"),
            correlation,
        )
    })?;
    manifest.destination_class = match decision.location {
        ExecutionLocation::Local { .. } => DestinationClass::Local,
        ExecutionLocation::Cloud { .. } => DestinationClass::Cloud,
    };
    Ok(PreparedModelContext {
        request: input.request,
        manifest,
        decision,
    })
}

fn text_has_secret(text: &str) -> bool {
    // Already-redacted assignments/headers can still report a reason code.
    // This backstop checks whether any bytes would change, not that code.
    redact(text).text != text
}

pub(crate) fn json_has_secret(value: &serde_json::Value) -> bool {
    // Depth/count are already bounded. Scan semantic strings, not escaped
    // JSON, so nested credentials cannot hide behind JSON framing.
    match value {
        serde_json::Value::String(text) => text_has_secret(text),
        serde_json::Value::Array(values) => values.iter().any(json_has_secret),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            text_has_secret(key)
                || json_has_secret(value)
                || match value {
                    serde_json::Value::String(text) if !text.is_empty() => {
                        text_has_secret(&format!("{key}={text}"))
                    }
                    serde_json::Value::Number(number) => {
                        text_has_secret(&format!("{key}={number}"))
                    }
                    _ => false,
                }
        }),
        _ => false,
    }
}
