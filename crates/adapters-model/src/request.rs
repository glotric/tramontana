//! Lossless protocol mapping after core input validation. No routing or policy.
use crate::transport::error;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tramontana_core::contracts::{
    ErrorCode, ErrorEnvelope, MessagePart, MessageRole, ModelEvent, ModelRequest,
};

pub(crate) struct NamedWire<W> {
    pub(crate) format: W,
    pub(crate) names: WireNames,
}

impl<W: crate::transport::WireFormat> crate::transport::WireFormat for NamedWire<W> {
    fn decode(
        &self,
        payload: &str,
        stream_id: &str,
        offset: &mut u64,
    ) -> (Vec<ModelEvent>, crate::transport::Signal) {
        let (mut events, signal) = self.format.decode(payload, stream_id, offset);
        self.names.normalize(&mut events);
        (events, signal)
    }
}

/// Request-local aliases fit both APIs without changing canonical tool or
/// invocation IDs. Sorting makes the map independent of tool registration order.
#[derive(Debug)]
pub(crate) struct WireNames {
    tools: BTreeMap<String, String>,
    calls: BTreeMap<String, String>,
}

impl WireNames {
    pub(crate) fn new(request: &ModelRequest) -> Self {
        let mut tools: BTreeSet<String> =
            request.tools.iter().map(|tool| tool.name.clone()).collect();
        let mut calls = BTreeSet::new();
        for message in &request.messages {
            for part in &message.parts {
                if let MessagePart::ToolCall {
                    invocation_id,
                    tool_id,
                    ..
                } = part
                {
                    tools.insert(tool_id.clone());
                    calls.insert(invocation_id.to_string());
                }
            }
        }
        Self {
            tools: tools
                .into_iter()
                .enumerate()
                .map(|(i, name)| (name, format!("tool_{i}")))
                .collect(),
            calls: calls
                .into_iter()
                .enumerate()
                .map(|(i, id)| (id, format!("call_{i}")))
                .collect(),
        }
    }

    fn tool(&self, name: &str) -> Result<&str, ErrorEnvelope> {
        self.tools
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "Unmapped tool name"))
    }
    fn call(&self, id: &str) -> Result<&str, ErrorEnvelope> {
        self.calls
            .get(id)
            .map(String::as_str)
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "Unmapped tool invocation"))
    }

    /// Unknown names remain untrusted candidates for host rejection. Resolving
    /// a known alias does not authorize a call, or validate its arguments.
    pub(crate) fn normalize(&self, events: &mut [ModelEvent]) {
        for event in events {
            if let ModelEvent::ToolCallDelta {
                name: Some(name), ..
            } = event
                && let Some((canonical, _)) = self.tools.iter().find(|(_, alias)| *alias == name)
            {
                *name = canonical.clone();
            }
        }
    }
}

fn observation(content: &Value, truncated: bool) -> String {
    // Never hide the truncation marker inside an apparently complete result.
    json!({"content":content,"truncated":truncated}).to_string()
}

pub(crate) fn openai_body(
    request: &ModelRequest,
    model: &str,
    names: &WireNames,
) -> Result<Value, ErrorEnvelope> {
    let mut messages = Vec::new();
    for message in &request.messages {
        if message.role == MessageRole::Tool {
            for part in &message.parts {
                if let MessagePart::ToolResult {
                    invocation_id,
                    content,
                    truncated,
                } = part
                {
                    messages.push(
                        json!({"role":"tool","tool_call_id":names.call(invocation_id.as_str())?,
                        "content":observation(content,*truncated)}),
                    );
                }
            }
            continue;
        }
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let mut text = Vec::new();
        let mut calls = Vec::new();
        for part in &message.parts {
            match part {
                MessagePart::Text { text: part } => text.push(part.as_str()),
                MessagePart::ToolCall {
                    invocation_id,
                    tool_id,
                    arguments,
                } => calls.push(json!({
                    "id":names.call(invocation_id.as_str())?,"type":"function",
                    "function":{"name":names.tool(tool_id)?,"arguments":arguments.to_string()}})),
                _ => {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Unexpected model message part",
                    ));
                }
            }
        }
        let mut encoded = json!({"role":role,"content":text.join("\n")});
        if !calls.is_empty() {
            encoded["tool_calls"] = calls.into();
        }
        messages.push(encoded);
    }
    let mut body = json!({"model":model,"messages":messages,"stream":true,"stream_options":{"include_usage":true}});
    if let Some(tokens) = request.limits.max_output_tokens {
        body["max_tokens"] = tokens.into();
    }
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(|tool| Ok(json!({"type":"function","function":{
            "name":names.tool(&tool.name)?,"description":tool.description,"parameters":tool.input_schema}})))
            .collect::<Result<Vec<_>,ErrorEnvelope>>()?.into();
    }
    Ok(body)
}

pub(crate) fn anthropic_body(
    request: &ModelRequest,
    model: &str,
    default_tokens: u64,
    names: &WireNames,
) -> Result<Value, ErrorEnvelope> {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        if message.role == MessageRole::System {
            for part in &message.parts {
                if let MessagePart::Text { text } = part {
                    system.push(text.as_str());
                }
            }
            continue;
        }
        let role = if message.role == MessageRole::Assistant {
            "assistant"
        } else {
            "user"
        };
        let mut content = Vec::new();
        for part in &message.parts {
            content.push(match part {
                MessagePart::Text {text} => json!({"type":"text","text":text}),
                MessagePart::ToolCall {invocation_id,tool_id,arguments} => json!({"type":"tool_use",
                    "id":names.call(invocation_id.as_str())?,"name":names.tool(tool_id)?,"input":arguments}),
                MessagePart::ToolResult {invocation_id,content,truncated} => json!({"type":"tool_result",
                    "tool_use_id":names.call(invocation_id.as_str())?,"content":observation(content,*truncated)}),
                _ => return Err(error(ErrorCode::InvalidRequest,"Unexpected model message part")),
            });
        }
        // Consecutive result messages must be one user turn immediately after
        // the assistant's calls. Subsequent user text follows those blocks.
        if let Some(last) = messages.last_mut().filter(|last| last["role"] == role)
            && let Some(blocks) = last["content"].as_array_mut()
        {
            blocks.extend(content);
        } else {
            messages.push(json!({"role":role,"content":content}));
        }
    }
    let mut body = json!({"model":model,"messages":messages,"stream":true,
        "max_tokens":request.limits.max_output_tokens.unwrap_or(default_tokens)});
    if !system.is_empty() {
        body["system"] = system.join("\n\n").into();
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                Ok(json!({"name":names.tool(&tool.name)?,
            "description":tool.description,"input_schema":tool.input_schema}))
            })
            .collect::<Result<Vec<_>, ErrorEnvelope>>()?
            .into();
    }
    Ok(body)
}
