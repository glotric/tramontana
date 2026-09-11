//! Provider-neutral validation and disclosure units for model input.
use std::collections::BTreeSet;
use std::io::Write;

use crate::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, MessagePart, MessageRole, MessageStatus,
    ModelCapability, ModelDescriptor, ModelRequest, ResponseFormat, SCHEMA_VERSION,
};

/// Maximum serialized normalized request size, including schemas and metadata.
pub const MODEL_INPUT_BYTES: usize = 1_048_576;

fn error(code: ErrorCode, message: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("model-input"))
}

/// Check the supported conversation grammar before approval or network work.
/// This validates structure, not tool argument schemas or permission to run tools.
pub fn validate_model_input(request: &ModelRequest) -> Result<(), ErrorEnvelope> {
    if request.schema_version != SCHEMA_VERSION
        || request.messages.is_empty()
        || request.limits.deadline_ms == 0
        || request.limits.max_output_tokens == Some(0)
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Invalid model request envelope or limits",
        ));
    }
    if request.messages.len() > 128 || request.tools.len() > 64 {
        return Err(error(
            ErrorCode::LimitExceeded,
            "Model input count limit exceeded",
        ));
    }
    if matches!(
        request.response_format,
        Some(ResponseFormat::JsonSchema { .. })
    ) {
        return Err(error(
            ErrorCode::CapabilityUnsupported,
            "Schema-constrained responses are not implemented",
        ));
    }
    let mut names = BTreeSet::new();
    for tool in &request.tools {
        identity(&tool.name)?;
        if !names.insert(&tool.name)
            || !tool.input_schema.is_object()
            || tool
                .input_schema
                .get("type")
                .and_then(serde_json::Value::as_str)
                != Some("object")
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Tool definitions need unique names and object schemas",
            ));
        }
        json_limits(&tool.input_schema)?;
    }
    let mut message_ids = BTreeSet::new();
    let mut call_ids = BTreeSet::new();
    let mut pending = BTreeSet::new();
    let mut non_system = false;
    let mut saw_user = false;
    let mut parts = 0;
    for message in &request.messages {
        if message.schema_version != SCHEMA_VERSION
            || message.status != MessageStatus::Committed
            || !message_ids.insert(&message.message_id)
            || message.parts.is_empty()
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Model history contains an invalid or uncommitted message",
            ));
        }
        parts += message.parts.len();
        if parts > 256 {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Model input part limit exceeded",
            ));
        }
        if message.role == MessageRole::System && non_system {
            return Err(error(
                ErrorCode::InvalidRequest,
                "System messages must precede conversation content",
            ));
        }
        if !non_system && !matches!(message.role, MessageRole::System | MessageRole::User) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Model conversation must start with a user message",
            ));
        }
        non_system |= message.role != MessageRole::System;
        saw_user |= message.role == MessageRole::User;
        if !pending.is_empty() && message.role != MessageRole::Tool {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Tool calls must be followed by their results",
            ));
        }
        let mut saw_call = false;
        for part in &message.parts {
            match part {
                MessagePart::Text { .. } if message.role != MessageRole::Tool && !saw_call => {}
                MessagePart::ToolCall {
                    invocation_id,
                    tool_id,
                    arguments,
                } if message.role == MessageRole::Assistant => {
                    identity(invocation_id.as_str())?;
                    identity(tool_id)?;
                    if !arguments.is_object() || !call_ids.insert(invocation_id) {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "Tool calls require unique identities and object arguments",
                        ));
                    }
                    json_limits(arguments)?;
                    pending.insert(invocation_id);
                    saw_call = true;
                }
                MessagePart::ToolResult {
                    invocation_id,
                    content,
                    ..
                } if message.role == MessageRole::Tool => {
                    if !pending.remove(invocation_id) {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "Tool result has no pending call",
                        ));
                    }
                    json_limits(content)?;
                }
                MessagePart::Unsupported => {
                    return Err(error(
                        ErrorCode::CapabilityUnsupported,
                        "Unsupported model message part",
                    ));
                }
                _ => {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Message parts do not match their role or order",
                    ));
                }
            }
        }
    }
    if !pending.is_empty()
        || !saw_user
        || request
            .messages
            .last()
            .is_some_and(|message| message.role == MessageRole::Assistant)
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Model history is not ready for a new assistant turn",
        ));
    }
    serde_json::to_writer(ByteBudget(MODEL_INPUT_BYTES), request)
        .map_err(|_| error(ErrorCode::LimitExceeded, "Model input byte limit exceeded"))?;
    Ok(())
}

/// Defend the adapter as well as the router: required features cannot be
/// silently dropped even if a caller supplies a request through another host.
pub fn validate_model_capabilities(
    request: &ModelRequest,
    descriptor: &ModelDescriptor,
) -> Result<(), ErrorEnvelope> {
    for capability in required_model_capabilities(request)? {
        if !descriptor.capabilities.contains(&capability) {
            return Err(error(
                ErrorCode::CapabilityUnsupported,
                "Selected model lacks a required capability",
            ));
        }
    }
    Ok(())
}

pub(crate) fn required_model_capabilities(
    request: &ModelRequest,
) -> Result<Vec<ModelCapability>, ErrorEnvelope> {
    validate_model_input(request)?;
    let mut capabilities = Vec::new();
    let uses_tools = !request.tools.is_empty()
        || request.messages.iter().any(|message| {
            message.parts.iter().any(|part| {
                matches!(
                    part,
                    MessagePart::ToolCall { .. } | MessagePart::ToolResult { .. }
                )
            })
        });
    if uses_tools {
        capabilities.push(ModelCapability::ToolCalls);
    }
    for required in &request.required_capabilities {
        let capability = match required.as_str() {
            "text" => ModelCapability::Text,
            "streaming" => ModelCapability::Streaming,
            "toolCalls" => ModelCapability::ToolCalls,
            "cancellation" => ModelCapability::Cancellation,
            _ => {
                return Err(error(
                    ErrorCode::CapabilityUnsupported,
                    "Requested model capability is not implemented",
                ));
            }
        };
        if !capabilities.contains(&capability) {
            capabilities.push(capability);
        }
    }
    Ok(capabilities)
}

/// Content units used by disclosure. Text keeps its existing hash convention;
/// structured parts and tool definitions include *all* semantic fields. These
/// are logical content bytes, not HTTP framing or provider-generated wrappers.
pub fn model_input_content(request: &ModelRequest) -> Result<Vec<String>, ErrorEnvelope> {
    validate_model_input(request)?;
    let mut content = Vec::new();
    for message in &request.messages {
        for part in &message.parts {
            content.push(match part {
                MessagePart::Text { text } => text.clone(),
                _ => serde_json::to_string(part).map_err(|_| {
                    error(
                        ErrorCode::InvalidRequest,
                        "Model input could not be encoded",
                    )
                })?,
            });
        }
    }
    for tool in &request.tools {
        content.push(serde_json::to_string(tool).map_err(|_| {
            error(
                ErrorCode::InvalidRequest,
                "Tool definition could not be encoded",
            )
        })?);
    }
    Ok(content)
}

fn identity(value: &str) -> Result<(), ErrorEnvelope> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Model tool identity is invalid",
        ));
    }
    if value.len() > 256 {
        return Err(error(
            ErrorCode::LimitExceeded,
            "Model tool identity limit exceeded",
        ));
    }
    Ok(())
}

fn json_limits(value: &serde_json::Value) -> Result<(), ErrorEnvelope> {
    let mut stack = vec![(value, 0)];
    let mut visited = 0usize;
    while let Some((value, depth)) = stack.pop() {
        visited += 1;
        let children = match value {
            serde_json::Value::Array(items) => items.len(),
            serde_json::Value::Object(items) => items.len(),
            _ => 0,
        };
        if depth > 32 || children > 100_000usize.saturating_sub(visited + stack.len()) {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Model input JSON complexity limit exceeded",
            ));
        }
        match value {
            serde_json::Value::Array(items) => {
                stack.extend(items.iter().map(|value| (value, depth + 1)))
            }
            serde_json::Value::Object(items) => {
                stack.extend(items.values().map(|value| (value, depth + 1)))
            }
            _ => {}
        }
    }
    Ok(())
}

struct ByteBudget(usize);
impl Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::other("model input size limit"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
