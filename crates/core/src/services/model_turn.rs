//! Bounded assembly of an untrusted model stream. Assembly is not permission
//! to execute: the worker must still validate the advertised tool/schema and
//! pass every call through the tool broker.

use std::collections::BTreeMap;

use crate::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, ModelEvent, StopReason, Usage};

/// In-memory limits for one normalized turn (UTF-8 bytes, not token estimates).
#[derive(Debug, Clone, Copy)]
pub struct ModelTurnLimits {
    /// Text plus argument fragments across the entire turn.
    pub content_bytes: usize,
    /// Argument bytes in any single call.
    pub argument_bytes: usize,
    /// Distinct calls, independent of the numeric indices supplied by a peer.
    pub tool_calls: usize,
    /// Bound empty-fragment/usage floods as well as nonempty output.
    pub events: usize,
    /// Maximum bytes in an identifier or tool name.
    pub identity_bytes: usize,
}

impl Default for ModelTurnLimits {
    fn default() -> Self {
        Self {
            content_bytes: 1_048_576,
            argument_bytes: 65_536,
            tool_calls: 16,
            events: 100_000,
            identity_bytes: 256,
        }
    }
}

/// A complete candidate call, never an authorization or replay identity.
#[derive(Debug, PartialEq)]
pub struct ModelToolCall {
    /// Provider identifier used to correlate an observation with this call.
    pub call_id: String,
    /// Model-supplied tool name; the registry must validate it before execution.
    pub name: String,
    /// Parsed object; the tool's input schema must still validate its contents.
    pub arguments: serde_json::Value,
}

/// The result of a stream with one validated terminal event.
#[derive(Debug)]
pub struct ModelTurn {
    /// Coalesced text, possibly partial when the stop reason says so.
    pub text: String,
    /// Candidates are exposed only for a successful `ToolCall` stop.
    pub tool_calls: Vec<ModelToolCall>,
    /// Cumulative counts, merging split input/output reports without summing.
    pub usage: Option<Usage>,
    /// Why the provider stopped.
    pub stop_reason: StopReason,
}

#[derive(Debug)]
struct PartialCall {
    call_id: String,
    name: String,
    arguments: String,
}

/// Incremental, fail-closed stream validator. Once a push fails, finishing
/// cannot succeed even if the caller ignores the error and sends more input.
#[derive(Debug)]
pub struct ModelTurnAccumulator {
    stream_id: String,
    correlation_id: CorrelationId,
    limits: ModelTurnLimits,
    text: String,
    calls: BTreeMap<u64, PartialCall>,
    usage: Option<Usage>,
    outcome: Option<StopReason>,
    failure: Option<ErrorEnvelope>,
    next_offset: u64,
    events: usize,
    bytes: usize,
}

impl ModelTurnAccumulator {
    /// Start a turn for exactly this request's stream.
    #[must_use]
    pub fn new(stream_id: String, correlation_id: CorrelationId, limits: ModelTurnLimits) -> Self {
        Self {
            stream_id,
            correlation_id,
            limits,
            text: String::new(),
            calls: BTreeMap::new(),
            usage: None,
            outcome: None,
            failure: None,
            next_offset: 0,
            events: 0,
            bytes: 0,
        }
    }

    /// Validated text so far, suitable for preserving an explicitly partial reply.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Validate and retain one event. Errors never echo model-supplied content.
    pub fn push(&mut self, event: &ModelEvent) -> Result<(), ErrorEnvelope> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = self.accept(event);
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        result
    }

    fn error(&self, code: ErrorCode, message: &str) -> ErrorEnvelope {
        ErrorEnvelope::new(code, message, self.correlation_id.clone())
    }

    fn accept(&mut self, event: &ModelEvent) -> Result<(), ErrorEnvelope> {
        if self.outcome.is_some() {
            return Err(self.error(
                ErrorCode::ProtocolError,
                "Model data followed a terminal event",
            ));
        }
        if self.events >= self.limits.events {
            return Err(self.error(ErrorCode::LimitExceeded, "Model turn event limit exceeded"));
        }
        self.events += 1;
        match event {
            ModelEvent::TextDelta {
                stream_id,
                offset,
                text,
            } => {
                self.delta(stream_id, *offset, text.len())?;
                self.text.push_str(text);
            }
            ModelEvent::ToolCallDelta {
                stream_id,
                offset,
                call_index,
                call_id,
                name,
                delta,
            } => {
                self.delta(stream_id, *offset, delta.len())?;
                if let Some(call) = self.calls.get(call_index) {
                    if call_id.as_ref().is_some_and(|id| id != &call.call_id)
                        || name.as_ref().is_some_and(|name| name != &call.name)
                    {
                        return Err(self.error(
                            ErrorCode::ProtocolError,
                            "Model tool identity changed within a call",
                        ));
                    }
                } else {
                    if self.calls.len() >= self.limits.tool_calls {
                        return Err(
                            self.error(ErrorCode::LimitExceeded, "Model tool call limit exceeded")
                        );
                    }
                    let (Some(call_id), Some(name)) = (call_id, name) else {
                        return Err(self.error(
                            ErrorCode::ProtocolError,
                            "Model tool call opened without an identifier and name",
                        ));
                    };
                    for identity in [call_id, name] {
                        if identity.len() > self.limits.identity_bytes {
                            return Err(self.error(
                                ErrorCode::LimitExceeded,
                                "Model tool identity limit exceeded",
                            ));
                        }
                        if identity.is_empty() || identity.chars().any(char::is_control) {
                            return Err(self.error(
                                ErrorCode::ProtocolError,
                                "Model tool identity is invalid",
                            ));
                        }
                    }
                    if self.calls.values().any(|call| &call.call_id == call_id) {
                        return Err(self.error(
                            ErrorCode::ProtocolError,
                            "Model reused a tool call identifier",
                        ));
                    }
                    self.calls.insert(
                        *call_index,
                        PartialCall {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                        },
                    );
                }
                let argument_bytes = self
                    .calls
                    .get(call_index)
                    .map_or(0, |call| call.arguments.len());
                if delta.len() > self.limits.argument_bytes.saturating_sub(argument_bytes) {
                    return Err(self.error(
                        ErrorCode::LimitExceeded,
                        "Model tool argument limit exceeded",
                    ));
                }
                if let Some(call) = self.calls.get_mut(call_index) {
                    call.arguments.push_str(delta);
                }
            }
            ModelEvent::Usage { usage } => {
                self.usage = Some(self.usage.map_or(*usage, |previous| Usage {
                    input_tokens: previous.input_tokens.max(usage.input_tokens),
                    output_tokens: previous.output_tokens.max(usage.output_tokens),
                    estimated: previous.estimated || usage.estimated,
                }));
            }
            ModelEvent::Completed { stop_reason } => self.outcome = Some(*stop_reason),
            ModelEvent::Failed { error } => return Err(error.clone()),
        }
        Ok(())
    }

    fn delta(&mut self, stream_id: &str, offset: u64, bytes: usize) -> Result<(), ErrorEnvelope> {
        if stream_id != self.stream_id || offset != self.next_offset {
            return Err(self.error(
                ErrorCode::ProtocolError,
                "Model delta stream or offset is invalid",
            ));
        }
        if bytes > self.limits.content_bytes.saturating_sub(self.bytes) {
            return Err(self.error(
                ErrorCode::LimitExceeded,
                "Model turn content limit exceeded",
            ));
        }
        self.next_offset = self.next_offset.checked_add(1).ok_or_else(|| {
            self.error(
                ErrorCode::LimitExceeded,
                "Model delta offset limit exceeded",
            )
        })?;
        self.bytes += bytes;
        Ok(())
    }

    /// Finish after stream closure. No incomplete/error/length/cancelled turn
    /// can produce executable candidates, even if earlier arguments were valid.
    pub fn finish(self) -> Result<ModelTurn, ErrorEnvelope> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let reason = self.outcome.ok_or_else(|| {
            self.error(
                ErrorCode::ProtocolError,
                "Model stream ended without an outcome",
            )
        })?;
        let mut tool_calls = Vec::new();
        if reason == StopReason::ToolCall {
            if self.calls.is_empty() {
                return Err(self.error(
                    ErrorCode::ProtocolError,
                    "Model tool stop contained no calls",
                ));
            }
            for call in self.calls.values() {
                let arguments: serde_json::Value =
                    serde_json::from_str(&call.arguments).map_err(|_| {
                        self.error(
                            ErrorCode::ProtocolError,
                            "Model tool arguments are not complete JSON",
                        )
                    })?;
                if !arguments.is_object() {
                    return Err(self.error(
                        ErrorCode::ProtocolError,
                        "Model tool arguments must be an object",
                    ));
                }
                tool_calls.push(ModelToolCall {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments,
                });
            }
        } else if reason == StopReason::Stop && !self.calls.is_empty() {
            return Err(self.error(
                ErrorCode::ProtocolError,
                "Model tool calls ended with a text stop",
            ));
        }
        Ok(ModelTurn {
            text: self.text,
            tool_calls,
            usage: self.usage,
            stop_reason: reason,
        })
    }
}
