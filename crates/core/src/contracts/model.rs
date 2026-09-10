//! What a model is asked, and what it streams back.
//!
//! `CORE_CONTRACTS.md` §8 is normative. Until M5 the request carried a task id
//! and a prompt, which was enough for a fake and would not have survived
//! contact with a second provider. This is the shape every adapter now
//! implements.
//!
//! Three properties are deliberate:
//!
//! - **A request names the context it was built from.** `contextManifestId`
//!   ties inference to the release that produced it, so "what was sent to that
//!   model?" is answerable from the record rather than from a log.
//! - **A tool definition sent to a model is narrower than a tool descriptor.**
//!   A model has no business knowing a tool's risk class, capability name, or
//!   cancellation support; those are how Tramontana governs the call, and
//!   putting them in a prompt would be leaking policy into text a provider
//!   sees.
//! - **Exactly one terminal event ends a stream**, including when cancellation
//!   arrives while the underlying request is still in flight.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    ContextReleasePlan, DataClass, ErrorEnvelope, NormalizedMessage, SCHEMA_VERSION, TaskId,
};

/// A tool as a model is told about it.
///
/// Deliberately not [`ToolDescriptor`](super::ToolDescriptor): that type
/// carries the risk class, capability, and cancellation facts Tramontana uses
/// to *govern* a call. A model needs the name, the purpose, and the argument
/// shape, and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedToolDefinition {
    /// Namespaced tool identifier, as the model should call it.
    pub name: String,
    /// What it does, in the words the model sees.
    pub description: String,
    /// JSON Schema for the arguments.
    pub input_schema: serde_json::Value,
}

/// What shape of answer was asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ResponseFormat {
    /// Prose.
    Text,
    /// A document matching a schema.
    #[serde(rename_all = "camelCase")]
    JsonSchema {
        /// The schema the answer must satisfy.
        schema: serde_json::Value,
    },
}

/// Bounds on one request.
///
/// `deadlineMs` is required rather than optional: a request with no deadline
/// is one that can hang a task forever, and every provider has to honour the
/// same bound for cancellation to mean the same thing everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelLimits {
    /// Cap on generated tokens, when one applies.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_output_tokens: Option<u64>,
    /// How long the whole request may take.
    pub deadline_ms: u64,
}

impl Default for ModelLimits {
    /// Two minutes and no token cap.
    ///
    /// Long enough for a local model on a cold cache, short enough that a
    /// wedged provider surfaces as a timeout rather than a hang.
    fn default() -> Self {
        Self {
            max_output_tokens: None,
            deadline_ms: 120_000,
        }
    }
}

/// A normalized request for inference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    /// Contract schema version.
    pub schema_version: u32,
    /// Correlates this request with its `modelStarted` event and its stream.
    pub request_id: String,
    /// The task on whose behalf it is made.
    pub task_id: TaskId,
    /// The conversation so far, already normalized and redacted.
    pub messages: Vec<NormalizedMessage>,
    /// Tools the model may call, as the model sees them.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<NormalizedToolDefinition>,
    /// Capabilities a model must have to serve this request.
    ///
    /// The router matches these; a provider that cannot meet them is not a
    /// fallback, it is ineligible.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub required_capabilities: Vec<String>,
    /// How sensitive the content is, for routing and disclosure.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub data_classes: Vec<DataClass>,
    /// The context release this request was built from.
    pub context_manifest_id: String,
    /// What shape of answer was asked for.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub response_format: Option<ResponseFormat>,
    /// Bounds on the request.
    pub limits: ModelLimits,
}

impl ModelRequest {
    /// A text request carrying one message.
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        task_id: TaskId,
        messages: Vec<NormalizedMessage>,
        context_manifest_id: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            request_id: request_id.into(),
            task_id,
            messages,
            tools: Vec::new(),
            required_capabilities: Vec::new(),
            data_classes: Vec::new(),
            context_manifest_id: context_manifest_id.into(),
            response_format: None,
            limits: ModelLimits::default(),
        }
    }

    /// Whether any content in this request is more sensitive than `Public`.
    ///
    /// What a disclosure decision turns on: sending public content to a cloud
    /// endpoint is not the same act as sending a user's project to one.
    #[must_use]
    pub fn carries_private_data(&self) -> bool {
        self.data_classes
            .iter()
            .any(|class| *class != DataClass::Public)
    }
}

/// Token counts for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Tokens consumed by the request.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub input_tokens: Option<u64>,
    /// Tokens produced.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub output_tokens: Option<u64>,
    /// Whether these are counted or approximated.
    ///
    /// Recorded rather than hidden: a cost shown to a user as exact when it was
    /// estimated is a number they will make decisions on.
    pub estimated: bool,
}

/// Why a model stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum StopReason {
    /// The model finished normally.
    Stop,
    /// An output limit was reached.
    Length,
    /// The model asked to call a tool.
    ToolCall,
    /// The request was cancelled.
    Cancelled,
}

/// One item from a model stream.
///
/// Exactly one [`ModelEvent::Completed`] or [`ModelEvent::Failed`] ends a
/// stream, even when cancellation arrives while the underlying request is
/// still in flight (`CORE_CONTRACTS.md` §8). The shared provider contract
/// suite checks that for every adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ModelEvent {
    /// Incremental text.
    #[serde(rename_all = "camelCase")]
    TextDelta {
        /// The stream this belongs to.
        stream_id: String,
        /// Zero-based ordinal shared by text/tool deltas (not a byte offset).
        offset: u64,
        /// The text produced.
        text: String,
    },
    /// Incremental arguments for a tool call the model is assembling.
    #[serde(rename_all = "camelCase")]
    ToolCallDelta {
        /// The stream this belongs to.
        stream_id: String,
        /// Zero-based ordinal shared by text/tool deltas (not a byte offset).
        offset: u64,
        /// Stable position of this call within the response. Continuations
        /// carry the same index even when the provider omits its identifier.
        call_index: u64,
        /// Provider call identifier, supplied on the opening fragment.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        call_id: Option<String>,
        /// Tool name, supplied on the opening fragment. Never inferred from ID.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        name: Option<String>,
        /// The fragment.
        delta: String,
    },
    /// What the request cost.
    #[serde(rename_all = "camelCase")]
    Usage {
        /// The counts.
        usage: Usage,
    },
    /// The stream ended normally.
    #[serde(rename_all = "camelCase")]
    Completed {
        /// Why it ended.
        stop_reason: StopReason,
    },
    /// The stream ended in failure.
    #[serde(rename_all = "camelCase")]
    Failed {
        /// Normalized, already-redacted error.
        error: ErrorEnvelope,
    },
}

impl ModelEvent {
    /// Whether this event closes the stream.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }
}

/// A model request that has passed the release boundary.
///
/// **Only `services::release` can build one.** The fields are private and the
/// sole constructor is `pub(crate)`, so a provider adapter that accepts this
/// type cannot be called without a release decision having been made — the
/// same construction that makes [`AuthorizedToolRequest`] the only way to
/// reach a tool (`crate::contracts::AuthorizedToolRequest`).
///
/// It carries the plan *and* the request together rather than a digest of the
/// request. A digest could be checked and then the payload swapped; owning
/// both means the disclosure describes the bytes that are handed over, because
/// there is only one copy of them.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizedModelRequest {
    request: ModelRequest,
    plan: ContextReleasePlan,
}

impl AuthorizedModelRequest {
    /// Bind a request to the release that authorized it. Reachable only from
    /// within the core.
    #[must_use]
    pub(crate) fn new(request: ModelRequest, plan: ContextReleasePlan) -> Self {
        Self { request, plan }
    }

    /// The request to send.
    #[must_use]
    pub fn request(&self) -> &ModelRequest {
        &self.request
    }

    /// What this release discloses, and on whose authority.
    #[must_use]
    pub fn plan(&self) -> &ContextReleasePlan {
        &self.plan
    }

    /// The request, for a provider that needs to own it.
    #[must_use]
    pub fn into_request(self) -> ModelRequest {
        self.request
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{CorrelationId, ErrorCode, MessageId, MessageRole, Timestamp};

    fn request() -> ModelRequest {
        ModelRequest::new(
            "request-1",
            TaskId::new("task-1"),
            vec![NormalizedMessage::text(
                MessageId::new("message-1"),
                None,
                MessageRole::User,
                Timestamp::new("2026-01-01T00:00:00.000Z"),
                "hello",
            )],
            "context-1",
        )
    }

    #[test]
    fn a_request_names_the_context_it_was_built_from() {
        // The link that makes "what was sent to that model?" answerable from
        // the record rather than from a log.
        assert_eq!(request().context_manifest_id, "context-1");
    }

    #[test]
    fn a_request_has_a_deadline_even_when_nobody_set_one() {
        // A request with no deadline is one that can hang a task forever.
        assert!(ModelLimits::default().deadline_ms > 0);
        assert!(request().limits.max_output_tokens.is_none());
    }

    #[test]
    fn only_non_public_content_counts_as_private() {
        let mut request = request();
        assert!(!request.carries_private_data(), "no classes is not private");

        request.data_classes = vec![DataClass::Public];
        assert!(!request.carries_private_data());

        request.data_classes = vec![DataClass::Public, DataClass::Project];
        assert!(request.carries_private_data());
    }

    #[test]
    fn exactly_the_two_outcome_events_are_terminal() {
        let deltas = [
            ModelEvent::TextDelta {
                stream_id: "s1".into(),
                offset: 0,
                text: "hi".into(),
            },
            ModelEvent::ToolCallDelta {
                stream_id: "s1".into(),
                offset: 1,
                call_index: 0,
                call_id: Some("call-1".into()),
                name: Some("builtin.fs.read".into()),
                delta: "{".into(),
            },
            ModelEvent::Usage {
                usage: Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    estimated: false,
                },
            },
        ];
        for event in deltas {
            assert!(!event.is_terminal(), "{event:?} must not close a stream");
        }

        assert!(
            ModelEvent::Completed {
                stop_reason: StopReason::Stop
            }
            .is_terminal()
        );
        assert!(
            ModelEvent::Failed {
                error: ErrorEnvelope::new(
                    ErrorCode::ProviderUnavailable,
                    "gone",
                    CorrelationId::new("c1"),
                )
            }
            .is_terminal()
        );
    }

    #[test]
    fn a_tool_definition_does_not_carry_policy_to_the_model() {
        // A model governs nothing. Risk class, capability, and cancellation
        // support are how Tramontana decides, and putting them in a prompt
        // would leak policy into text a provider sees.
        let definition = NormalizedToolDefinition {
            name: "builtin.fs.read".into(),
            description: "Read a file inside the project.".into(),
            input_schema: serde_json::json!({ "type": "object" }),
        };
        let json = serde_json::to_value(&definition).unwrap();
        let fields: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(fields, ["description", "inputSchema", "name"]);
    }

    #[test]
    fn requests_and_events_serialize_as_camel_case() {
        let json = serde_json::to_value(request()).unwrap();
        assert_eq!(json["requestId"], "request-1");
        assert_eq!(json["contextManifestId"], "context-1");
        assert_eq!(json["limits"]["deadlineMs"], 120_000);
        // Empty collections are omitted rather than sent as `[]`.
        let fields = json.as_object().unwrap();
        for absent in [
            "tools",
            "requiredCapabilities",
            "dataClasses",
            "responseFormat",
        ] {
            assert!(!fields.contains_key(absent), "{absent} should be omitted");
        }

        let event = serde_json::to_value(ModelEvent::Completed {
            stop_reason: StopReason::ToolCall,
        })
        .unwrap();
        assert_eq!(event["kind"], "completed");
        assert_eq!(event["stopReason"], "toolCall");
    }
}
