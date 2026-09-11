#![allow(clippy::unwrap_used, reason = "assertions in tests")]
use tramontana_core::contracts::*;
use tramontana_core::services::*;
use tramontana_core::testing::FakeModelProvider;

fn message(id: &str, role: MessageRole, parts: Vec<MessagePart>) -> NormalizedMessage {
    let mut message = NormalizedMessage::text(
        MessageId::new(id),
        None,
        role,
        Timestamp::new("2026-01-01T00:00:00Z"),
        "",
    );
    message.parts = parts;
    message
}
fn request() -> ModelRequest {
    ModelRequest::new(
        "request",
        TaskId::new("task"),
        vec![
            message(
                "user",
                MessageRole::User,
                vec![MessagePart::Text {
                    text: "Read the docs".into(),
                }],
            ),
            message(
                "assistant",
                MessageRole::Assistant,
                vec![MessagePart::ToolCall {
                    invocation_id: InvocationId::new("invocation"),
                    tool_id: "builtin.fs.read".into(),
                    arguments: serde_json::json!({"path":"README.md"}),
                }],
            ),
            message(
                "result",
                MessageRole::Tool,
                vec![MessagePart::ToolResult {
                    invocation_id: InvocationId::new("invocation"),
                    content: serde_json::json!({"text":"Docs"}),
                    truncated: true,
                }],
            ),
        ],
        "context",
    )
}
fn descriptor() -> ModelDescriptor {
    let mut descriptor = FakeModelProvider::descriptor();
    descriptor.capabilities.push(ModelCapability::ToolCalls);
    descriptor
}

#[test]
fn a_complete_tool_round_trip_is_valid_but_requires_verified_capability() {
    let request = request();
    validate_model_capabilities(&request, &descriptor()).unwrap();
    assert_eq!(
        validate_model_capabilities(&request, &FakeModelProvider::descriptor())
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnsupported
    );
    let content = model_input_content(&request).unwrap();
    assert_eq!(content[0], "Read the docs");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&content[1]).unwrap()["toolId"],
        "builtin.fs.read"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&content[2]).unwrap()["truncated"],
        true
    );
}

#[test]
fn orphan_duplicate_missing_and_wrong_role_tool_parts_are_rejected() {
    let base = request();
    let mut orphan = base.clone();
    orphan.messages.remove(1);
    let mut missing = base.clone();
    missing.messages.pop();
    let mut duplicate_result = base.clone();
    duplicate_result.messages[2]
        .parts
        .push(base.messages[2].parts[0].clone());
    let mut duplicate_call = base.clone();
    duplicate_call.messages[1]
        .parts
        .push(base.messages[1].parts[0].clone());
    let mut wrong_role = base.clone();
    wrong_role.messages[2].role = MessageRole::User;
    let mut middle_user = base.clone();
    middle_user.messages.insert(
        2,
        message(
            "interposed",
            MessageRole::User,
            vec![MessagePart::Text {
                text: "interrupt".into(),
            }],
        ),
    );
    let mut uncommitted = base.clone();
    uncommitted.messages[1].status = MessageStatus::Partial;
    let mut scalar = base.clone();
    if let MessagePart::ToolCall { arguments, .. } = &mut scalar.messages[1].parts[0] {
        *arguments = serde_json::json!(1);
    }
    let mut late_system = base.clone();
    late_system.messages.push(message(
        "late",
        MessageRole::System,
        vec![MessagePart::Text {
            text: "policy".into(),
        }],
    ));
    let mut after_call = base.clone();
    after_call.messages[1].parts.push(MessagePart::Text {
        text: "late".into(),
    });
    let mut assistant_first = base.clone();
    assistant_first.messages.insert(
        0,
        message(
            "prefill",
            MessageRole::Assistant,
            vec![MessagePart::Text {
                text: "prefill".into(),
            }],
        ),
    );
    for invalid in [
        orphan,
        missing,
        duplicate_result,
        duplicate_call,
        wrong_role,
        middle_user,
        uncommitted,
        scalar,
        late_system,
        after_call,
        assistant_first,
    ] {
        assert_eq!(
            validate_model_input(&invalid).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}

#[test]
fn unsupported_parts_and_response_modes_are_not_silently_dropped() {
    let mut request = request();
    request.messages[0].parts.push(MessagePart::Unsupported);
    assert_eq!(
        validate_model_input(&request).unwrap_err().code,
        ErrorCode::CapabilityUnsupported
    );
    request.messages[0].parts.pop();
    request.response_format = Some(ResponseFormat::JsonSchema {
        schema: serde_json::json!({}),
    });
    assert_eq!(
        validate_model_capabilities(&request, &descriptor())
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnsupported
    );
    request.response_format = Some(ResponseFormat::Text);
    request.required_capabilities = vec!["imaginary".into()];
    assert_eq!(
        validate_model_capabilities(&request, &descriptor())
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnsupported
    );
}

#[test]
fn input_bounds_cover_metadata_bytes_parts_tools_and_json_complexity() {
    let base = request();
    let mut bytes = base.clone();
    bytes.messages[0].parts = vec![MessagePart::Text {
        text: "é".repeat(MODEL_INPUT_BYTES / 2),
    }];
    let mut parts = base.clone();
    parts.messages[0].parts = vec![MessagePart::Text { text: "".into() }; 257];
    let mut messages = base.clone();
    messages.messages = vec![base.messages[0].clone(); 129];
    let tool = NormalizedToolDefinition {
        name: "builtin.fs.read".into(),
        description: "Read".into(),
        input_schema: serde_json::json!({"type":"object"}),
    };
    let mut tools = base.clone();
    tools.tools = vec![tool.clone(); 65];
    let mut depth = base.clone();
    let mut value = serde_json::json!(null);
    for _ in 0..34 {
        value = serde_json::json!([value]);
    }
    if let MessagePart::ToolResult { content, .. } = &mut depth.messages[2].parts[0] {
        *content = value;
    }
    let mut width = base.clone();
    if let MessagePart::ToolResult { content, .. } = &mut width.messages[2].parts[0] {
        *content = serde_json::Value::Array(vec![serde_json::Value::Null; 100_001]);
    }
    for invalid in [bytes, parts, messages, tools, depth, width] {
        assert_eq!(
            validate_model_input(&invalid).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
    }
    let mut duplicates = base;
    duplicates.tools = vec![tool.clone(), tool];
    assert_eq!(
        validate_model_input(&duplicates).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}
