//! Minimized model history projected from canonical task events, never a
//! worker-private transcript or reconstructed raw output from audit summaries.
use std::collections::BTreeSet;

use crate::contracts::{
    ContextCategory, ContextManifest, CorrelationId, DataClass, ErrorCode, ErrorEnvelope,
    EventEnvelope, EventPayload, MessageId, MessagePart, MessageRole, ModelRequest,
    NormalizedMessage, SCHEMA_VERSION, TaskEventKind, TaskId, TaskRecord, Visibility,
};

use super::{
    ModelContentProvenance, ModelContextInput, ModelToolCatalogue, model_context, model_input,
};

/// A bounded explicit selection of canonical messages. Core callers choose
/// IDs; workers cannot inject bodies, roles or sensitivity through this API.
pub struct CanonicalModelHistory {
    task_id: TaskId,
    messages: Vec<NormalizedMessage>,
    provenance: Vec<ModelContentProvenance>,
}

impl std::fmt::Debug for CanonicalModelHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanonicalModelHistory")
            .field("messages", &self.messages.len())
            .finish_non_exhaustive()
    }
}

impl CanonicalModelHistory {
    /// Read a gap-free event prefix ending at `task.last_sequence`. Selection
    /// is explicit (empty means goal only) and always retains canonical order.
    /// Calls/results must remain complete. Missing raw observations, partial
    /// messages, resumed-step summaries and unknown parts are not repaired.
    ///
    /// `initial` is the core-owned goal provenance, not a worker-supplied label.
    /// This performs no replay, tool invocation, persistence or release.
    pub fn select(
        task: &TaskRecord,
        initial: &ContextManifest,
        events: &[EventEnvelope],
        selected: &[MessageId],
    ) -> Result<Self, ErrorEnvelope> {
        if events.len() > 100_000 || selected.len() > 127 {
            return Err(error(ErrorCode::LimitExceeded));
        }
        if task.schema_version != SCHEMA_VERSION
            || initial.schema_version != SCHEMA_VERSION
            || initial.task_id != task.task_id
            || task.last_sequence != events.len() as u64
        {
            return Err(error(ErrorCode::InvalidRequest));
        }
        model_input::input_size(&task.goal_summary)?;
        let goals: Vec<_> = initial
            .sources
            .iter()
            .filter(|source| source.source_id == "conversation.goal")
            .collect();
        let [goal] = goals.as_slice() else {
            return Err(error(ErrorCode::PermissionDenied));
        };
        if !goal.included
            || goal.category != ContextCategory::Conversation
            || goal.data_class == DataClass::Secret
            || goal.content_hash != crate::contracts::content_hash(&task.goal_summary)
            || goal.byte_count != task.goal_summary.len() as u64
        {
            return Err(error(ErrorCode::PermissionDenied));
        }
        let wanted: BTreeSet<_> = selected.iter().collect();
        if wanted.len() != selected.len() {
            return Err(error(ErrorCode::InvalidRequest));
        }
        let mut event_ids = BTreeSet::new();
        let mut message_ids = BTreeSet::new();
        let mut chosen = Vec::new();
        let mut parts = 1usize; // goal
        for (index, event) in events.iter().enumerate() {
            if event.schema_version != SCHEMA_VERSION
                || event.task_id != task.task_id
                || event.conversation_id != task.conversation_id
                || event.sequence != index as u64 + 1
                || !event_ids.insert(&event.event_id)
            {
                return Err(error(ErrorCode::InvalidRequest));
            }
            let EventPayload::MessageCommitted { message } = &event.payload else {
                continue;
            };
            if !message_ids.insert(&message.message_id) {
                return Err(error(ErrorCode::InvalidRequest));
            }
            if !wanted.contains(&message.message_id) {
                continue;
            }
            if event.kind != TaskEventKind::MessageCommitted
                || event.visibility == Visibility::Secret
                || message.task_id.as_ref() != Some(&task.task_id)
                || message.conversation_id != task.conversation_id
                || !matches!(message.role, MessageRole::Assistant | MessageRole::Tool)
            {
                return Err(error(ErrorCode::PermissionDenied));
            }
            parts = parts.saturating_add(message.parts.len());
            if parts > 256 {
                return Err(error(ErrorCode::LimitExceeded));
            }
            for part in &message.parts {
                match part {
                    MessagePart::ToolCall { arguments, .. } => complete_tool_content(arguments)?,
                    MessagePart::ToolResult { content, .. } => complete_tool_content(content)?,
                    _ => {}
                }
            }
            chosen.push(message);
        }
        if chosen.len() != wanted.len() {
            return Err(error(ErrorCode::NotFound));
        }
        model_input::input_size(&chosen)?;
        // Refuse secret-shaped semantic strings; do not rewrite canonical
        // arguments/results at release time or claim we recovered ingestion codes.
        for message in &chosen {
            let value =
                serde_json::to_value(message).map_err(|_| error(ErrorCode::InvalidRequest))?;
            if model_context::json_has_secret(&value) {
                return Err(error(ErrorCode::PermissionDenied));
            }
        }
        if super::redact(&task.goal_summary).text != task.goal_summary {
            return Err(error(ErrorCode::PermissionDenied));
        }
        let mut first = NormalizedMessage::text(
            MessageId::new(format!("goal-{}", task.task_id)),
            task.conversation_id.clone(),
            MessageRole::User,
            task.created_at.clone(),
            &task.goal_summary,
        );
        first.task_id = Some(task.task_id.clone());
        let mut messages = vec![first];
        let mut provenance = vec![ModelContentProvenance {
            source_id: goal.source_id.clone(),
            data_class: goal.data_class,
            redactions: goal.redactions.clone(),
        }];
        for message in chosen {
            provenance.push(ModelContentProvenance {
                source_id: format!(
                    "message.{}",
                    crate::contracts::content_hash(message.message_id.as_str())
                ),
                // Canonical messages do not yet store sensitivity/redaction
                // provenance. Conservatively sensitive, never inferred public.
                data_class: DataClass::Sensitive,
                redactions: Vec::new(),
            });
            messages.push(message.clone());
        }
        // Reuse the same grammar/budgets as release and provider adapters.
        let request = ModelRequest::new(
            "history-validation",
            task.task_id.clone(),
            messages,
            "history-validation",
        );
        super::validate_model_input(&request)?;
        Ok(Self {
            task_id: task.task_id.clone(),
            messages: request.messages,
            provenance,
        })
    }

    /// Construct the next request with fresh host IDs. The caller must still
    /// prepare, route, journal and authorize it before provider dispatch.
    pub fn into_input(
        self,
        request_id: impl Into<String>,
        context_manifest_id: impl Into<String>,
        catalogue: ModelToolCatalogue,
    ) -> Result<ModelContextInput, ErrorEnvelope> {
        let tools = catalogue.provenance();
        let mut request =
            ModelRequest::new(request_id, self.task_id, self.messages, context_manifest_id);
        request.tools = catalogue.definitions;
        super::validate_model_input(&request)?;
        Ok(ModelContextInput {
            request,
            messages: self.provenance,
            tools,
        })
    }
}

fn complete_tool_content(value: &serde_json::Value) -> Result<(), ErrorEnvelope> {
    model_input::json_limits(value)?;
    fn omitted(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => {
                text.contains(super::REDACTED_OUTPUT) || text.contains(super::REDACTED_PATH)
            }
            serde_json::Value::Array(items) => items.iter().any(omitted),
            serde_json::Value::Object(items) => items.iter().any(|(key, value)| {
                key.contains(super::REDACTED_OUTPUT)
                    || key.contains(super::REDACTED_PATH)
                    || omitted(value)
            }),
            _ => false,
        }
    }
    // Export omissions are not raw observations or executable arguments. A
    // caller must select another complete exchange or recollect explicitly.
    if omitted(value) {
        return Err(error(ErrorCode::InvalidRequest));
    }
    Ok(())
}

fn error(code: ErrorCode) -> ErrorEnvelope {
    ErrorEnvelope::new(
        code,
        "Canonical model history selection refused",
        CorrelationId::new("model-history"),
    )
}
