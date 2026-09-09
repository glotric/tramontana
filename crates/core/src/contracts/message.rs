//! Normalized conversation messages.
//!
//! `CORE_CONTRACTS.md` §6. The point of this type is what it refuses to hold:
//! provider-only reasoning fields, encrypted thinking, raw SDK response
//! objects, and worker-private transcript items are **not** message parts. An
//! adapter may keep such things as short-lived private state, but the durable
//! record is this, so a conversation survives replacing the provider that
//! produced it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ConversationId, InvocationId, MessageId, SCHEMA_VERSION, TaskId, Timestamp};

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    /// Instruction content assembled by Tramontana.
    System,
    /// The person.
    User,
    /// The model or worker.
    Assistant,
    /// A tool result being fed back.
    Tool,
}

/// Whether a message is finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MessageStatus {
    /// Complete and durable.
    Committed,
    /// Stream ended early; content up to that point was kept.
    Partial,
    /// Replaced by a later message, retained for history.
    Superseded,
}

/// One piece of a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum MessagePart {
    /// Prose.
    #[serde(rename_all = "camelCase")]
    Text {
        /// The text.
        text: String,
    },
    /// A request to run a tool.
    #[serde(rename_all = "camelCase")]
    ToolCall {
        /// The invocation this call opened.
        invocation_id: InvocationId,
        /// Namespaced tool identifier.
        tool_id: String,
        /// Arguments, already validated against the tool's schema.
        arguments: serde_json::Value,
    },
    /// The result of running a tool.
    #[serde(rename_all = "camelCase")]
    ToolResult {
        /// The invocation this result closes.
        invocation_id: InvocationId,
        /// Structured, redacted result content.
        content: serde_json::Value,
        /// Whether output was cut short by a limit.
        truncated: bool,
    },
    /// A part this build does not understand.
    #[serde(other)]
    Unsupported,
}

/// A durable, provider-neutral message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedMessage {
    /// Contract schema version.
    pub schema_version: u32,
    /// Unique identifier.
    pub message_id: MessageId,
    /// Conversation this message belongs to, when it has one.
    ///
    /// Optional because a task can be started outside a conversation, and its
    /// assistant answer is still the durable record of what the model said.
    /// Dropping the message because there was no conversation would leave the
    /// exported task with tool calls and no answer.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub conversation_id: Option<ConversationId>,
    /// Task that produced it, when one did.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task_id: Option<TaskId>,
    /// Who produced it.
    pub role: MessageRole,
    /// When it was created.
    pub created_at: Timestamp,
    /// Whether it is finished.
    pub status: MessageStatus,
    /// Its content.
    pub parts: Vec<MessagePart>,
}

impl NormalizedMessage {
    /// Build a single-part text message.
    #[must_use]
    pub fn text(
        message_id: MessageId,
        conversation_id: Option<ConversationId>,
        role: MessageRole,
        created_at: Timestamp,
        text: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            message_id,
            conversation_id,
            task_id: None,
            role,
            created_at,
            status: MessageStatus::Committed,
            parts: vec![MessagePart::Text { text: text.into() }],
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_text_message_serializes_with_tagged_parts() {
        let message = NormalizedMessage::text(
            MessageId::new("m1"),
            Some(ConversationId::new("c1")),
            MessageRole::Assistant,
            Timestamp::new("2026-09-07T12:00:00.000Z"),
            "hello",
        );
        let json = serde_json::to_value(&message).unwrap();

        assert_eq!(json["role"], "assistant");
        assert_eq!(json["status"], "committed");
        assert_eq!(json["parts"][0]["kind"], "text");
        assert_eq!(json["parts"][0]["text"], "hello");
        assert!(!json.as_object().unwrap().contains_key("taskId"));
    }

    #[test]
    fn a_message_from_a_conversationless_task_omits_the_conversation() {
        // The CLI starts tasks outside a conversation. The answer is still the
        // durable record of what the model said, so the message exists and the
        // field is simply absent.
        let message = NormalizedMessage::text(
            MessageId::new("m1"),
            None,
            MessageRole::Assistant,
            Timestamp::new("2026-09-07T12:00:00.000Z"),
            "hello",
        );
        let json = serde_json::to_value(&message).unwrap();
        assert!(!json.as_object().unwrap().contains_key("conversationId"));
    }

    #[test]
    fn an_unknown_part_kind_is_read_as_unsupported() {
        // A provider-specific part from a newer build must not break replay of
        // an existing conversation.
        let json = serde_json::json!({
            "schemaVersion": 1,
            "messageId": "m1",
            "conversationId": "c1",
            "role": "assistant",
            "createdAt": "2026-09-07T12:00:00.000Z",
            "status": "committed",
            "parts": [
                { "kind": "text", "text": "kept" },
                { "kind": "encryptedThinking", "blob": "..." },
            ],
        });

        let message: NormalizedMessage = serde_json::from_value(json).unwrap();
        assert_eq!(message.parts.len(), 2);
        assert_eq!(
            message.parts[0],
            MessagePart::Text {
                text: "kept".into()
            }
        );
        assert_eq!(message.parts[1], MessagePart::Unsupported);
    }

    #[test]
    fn tool_parts_round_trip_with_their_invocation() {
        let part = MessagePart::ToolResult {
            invocation_id: InvocationId::new("i1"),
            content: serde_json::json!({ "files": 3 }),
            truncated: true,
        };
        let json = serde_json::to_value(&part).unwrap();
        assert_eq!(json["kind"], "toolResult");
        assert_eq!(json["invocationId"], "i1");
        assert_eq!(json["truncated"], true);
        assert_eq!(serde_json::from_value::<MessagePart>(json).unwrap(), part);
    }
}
