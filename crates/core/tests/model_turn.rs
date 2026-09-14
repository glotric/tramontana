//! Host-side validation of normalized streams, independent of any provider.
#![allow(clippy::unwrap_used, clippy::panic, reason = "assertions in tests")]

use tramontana_core::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, ModelEvent, StopReason, Usage,
};
use tramontana_core::services::{ModelTurnAccumulator, ModelTurnLimits};

fn accumulator(limits: ModelTurnLimits) -> ModelTurnAccumulator {
    ModelTurnAccumulator::new("stream".into(), CorrelationId::new("test"), limits)
}
fn call(offset: u64, index: u64, id: Option<&str>, name: Option<&str>, delta: &str) -> ModelEvent {
    ModelEvent::ToolCallDelta {
        stream_id: "stream".into(),
        offset,
        call_index: index,
        call_id: id.map(str::to_owned),
        name: name.map(str::to_owned),
        delta: delta.into(),
    }
}
fn text(offset: u64, text: &str) -> ModelEvent {
    ModelEvent::TextDelta {
        stream_id: "stream".into(),
        offset,
        text: text.into(),
    }
}
fn completed(stop_reason: StopReason) -> ModelEvent {
    ModelEvent::Completed { stop_reason }
}

#[test]
fn interleaved_calls_keep_identity_arguments_and_index_order() {
    let mut turn = accumulator(ModelTurnLimits::default());
    for event in [
        text(0, "Looking."),
        call(1, 3, Some("second"), Some("builtin.fs.list"), "{"),
        call(2, 1, Some("first"), Some("builtin.fs.read"), "{\"path\":\""),
        call(3, 3, None, None, "}"),
        call(4, 1, None, None, "README.md\"}"),
        completed(StopReason::ToolCall),
    ] {
        turn.push(&event).unwrap();
    }
    let turn = turn.finish().unwrap();
    assert_eq!(turn.text, "Looking.");
    assert_eq!(turn.tool_calls[0].call_id, "first");
    assert_eq!(turn.tool_calls[0].name, "builtin.fs.read");
    assert_eq!(
        turn.tool_calls[0].arguments,
        serde_json::json!({"path":"README.md"})
    );
    assert_eq!(turn.tool_calls[1].call_id, "second");
    assert_eq!(turn.tool_calls[1].arguments, serde_json::json!({}));
}

#[test]
fn usage_is_merged_without_double_counting() {
    let mut turn = accumulator(ModelTurnLimits::default());
    for (input_tokens, output_tokens, estimated) in [
        (Some(12), None, false),
        (None, Some(3), false),
        (Some(12), Some(5), true),
        (None, Some(4), false),
    ] {
        turn.push(&ModelEvent::Usage {
            usage: Usage {
                input_tokens,
                output_tokens,
                estimated,
            },
        })
        .unwrap();
    }
    turn.push(&completed(StopReason::Stop)).unwrap();
    assert_eq!(
        turn.finish().unwrap().usage,
        Some(Usage {
            input_tokens: Some(12),
            output_tokens: Some(5),
            estimated: true
        })
    );
}

#[test]
fn failures_poison_assembly_and_do_not_echo_content() {
    for bad in [
        call(1, 0, Some("changed-secret"), None, ""),
        call(1, 0, None, Some("changed-secret"), ""),
        call(1, 1, Some("id"), Some("tool"), "{}"),
        call(1, 1, None, Some("tool"), "{}"),
        call(1, 1, Some("new"), None, "{}"),
        call(1, 1, Some(""), Some("tool"), "{}"),
        call(1, 1, Some("new\n"), Some("tool"), "{}"),
        text(0, "secret"),
        text(2, "secret"),
        ModelEvent::TextDelta {
            stream_id: "wrong".into(),
            offset: 1,
            text: "secret".into(),
        },
    ] {
        let mut turn = accumulator(ModelTurnLimits::default());
        turn.push(&call(0, 0, Some("id"), Some("tool"), "{}"))
            .unwrap();
        let error = turn.push(&bad).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProtocolError);
        assert!(!error.message.contains("secret"));
        assert_eq!(
            turn.push(&completed(StopReason::ToolCall)),
            Err(error.clone())
        );
        assert_eq!(turn.finish().unwrap_err(), error);
    }
}

#[test]
fn incomplete_or_non_object_json_cannot_produce_calls() {
    for args in ["", "{", "[]", "null", "1", "\"string\"", "{}{}", "{\"x\":}"] {
        let mut turn = accumulator(ModelTurnLimits::default());
        turn.push(&call(0, 0, Some("id"), Some("tool"), args))
            .unwrap();
        turn.push(&completed(StopReason::ToolCall)).unwrap();
        assert_eq!(
            turn.finish().unwrap_err().code,
            ErrorCode::ProtocolError,
            "{args}"
        );
    }
}

#[test]
fn truncated_cancelled_failed_and_unterminated_streams_never_expose_calls() {
    for reason in [StopReason::Length, StopReason::Cancelled] {
        let mut turn = accumulator(ModelTurnLimits::default());
        turn.push(&call(0, 0, Some("id"), Some("tool"), "{}"))
            .unwrap();
        turn.push(&completed(reason)).unwrap();
        let turn = turn.finish().unwrap();
        assert_eq!(turn.stop_reason, reason);
        assert!(turn.tool_calls.is_empty());
    }
    let mut turn = accumulator(ModelTurnLimits::default());
    turn.push(&call(0, 0, Some("id"), Some("tool"), "{}"))
        .unwrap();
    assert_eq!(turn.finish().unwrap_err().code, ErrorCode::ProtocolError);
    let mut turn = accumulator(ModelTurnLimits::default());
    turn.push(&call(0, 0, Some("id"), Some("tool"), "{}"))
        .unwrap();
    let error = ErrorEnvelope::new(
        ErrorCode::ProviderUnavailable,
        "Provider failed",
        CorrelationId::new("test"),
    );
    assert_eq!(
        turn.push(&ModelEvent::Failed {
            error: error.clone()
        }),
        Err(error.clone())
    );
    assert_eq!(turn.finish().unwrap_err(), error);
}

#[test]
fn stop_reason_and_exactly_one_terminal_are_enforced() {
    let mut turn = accumulator(ModelTurnLimits::default());
    turn.push(&completed(StopReason::ToolCall)).unwrap();
    assert_eq!(turn.finish().unwrap_err().code, ErrorCode::ProtocolError);
    let mut turn = accumulator(ModelTurnLimits::default());
    turn.push(&call(0, 0, Some("id"), Some("tool"), "{}"))
        .unwrap();
    turn.push(&completed(StopReason::Stop)).unwrap();
    assert_eq!(turn.finish().unwrap_err().code, ErrorCode::ProtocolError);
    for extra in [completed(StopReason::Stop), text(0, "after")] {
        let mut turn = accumulator(ModelTurnLimits::default());
        turn.push(&completed(StopReason::Stop)).unwrap();
        assert_eq!(
            turn.push(&extra).unwrap_err().code,
            ErrorCode::ProtocolError
        );
        assert!(turn.finish().is_err());
    }
}

#[test]
fn byte_limits_count_utf8_and_across_text_and_calls_before_retaining() {
    let mut turn = accumulator(ModelTurnLimits {
        content_bytes: 4,
        ..Default::default()
    });
    turn.push(&text(0, "é")).unwrap();
    turn.push(&call(1, 0, Some("id"), Some("tool"), "{}"))
        .unwrap();
    assert_eq!(
        turn.push(&text(2, "x")).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(turn.text(), "é");
}

#[test]
fn every_count_and_size_bound_is_enforced() {
    for (limits, events) in [
        (
            ModelTurnLimits {
                argument_bytes: 1,
                ..Default::default()
            },
            vec![
                call(0, 0, Some("id"), Some("tool"), "{"),
                call(1, 0, None, None, "}"),
            ],
        ),
        (
            ModelTurnLimits {
                tool_calls: 1,
                ..Default::default()
            },
            vec![
                call(0, u64::MAX, Some("id"), Some("tool"), "{}"),
                call(1, 0, Some("id2"), Some("tool"), "{}"),
            ],
        ),
        (
            ModelTurnLimits {
                identity_bytes: 2,
                ..Default::default()
            },
            vec![call(0, 0, Some("id"), Some("tool"), "{}")],
        ),
        (
            ModelTurnLimits {
                events: 1,
                ..Default::default()
            },
            vec![text(0, ""), text(1, "")],
        ),
    ] {
        let mut turn = accumulator(limits);
        let mut error = None;
        for event in events {
            if let Err(found) = turn.push(&event) {
                error = Some(found);
                break;
            }
        }
        assert_eq!(error.unwrap().code, ErrorCode::LimitExceeded);
        assert_eq!(turn.finish().unwrap_err().code, ErrorCode::LimitExceeded);
    }
}
