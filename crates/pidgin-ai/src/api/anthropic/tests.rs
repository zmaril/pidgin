// straitjacket-allow-file:duplication — these tests transcribe pi's Anthropic
// SSE fixtures verbatim: the `message_start` / `content_block_*` / `message_delta`
// / `message_stop` event objects are walls of near-identical JSON by design, and
// the clone detector reads them as duplicates. They are distinct, load-bearing
// wire fixtures kept byte-for-byte with pi's test cases.
//! Unit tests for the Anthropic SSE parser, mirroring representative cases from
//! pi's `packages/ai/test/anthropic-sse-parsing.test.ts`.

use super::*;
use serde_json::json;

/// Build the SSE body exactly as pi's `createSseResponse` test helper does:
/// `event: <event>\ndata: <data>\n` per entry, joined by a blank line.
fn create_sse_body(events: &[(&str, String)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_model() -> AnthropicModel {
    AnthropicModel {
        id: "claude-haiku-4-5".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        cost: ModelCost {
            input: 1.0,
            output: 5.0,
            cache_read: 0.1,
            cache_write: 1.25,
            tiers: None,
        },
    }
}

fn minimal_events() -> Vec<(&'static str, String)> {
    vec![
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            })
            .to_string(),
        ),
        (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "Hello" }
            })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        (
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]
}

fn event_kinds(outcome: &StreamOutcome) -> Vec<&'static str> {
    outcome
        .events
        .iter()
        .map(|e| match e {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
            AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
            AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
        })
        .collect()
}

#[test]
fn repairs_malformed_sse_json_and_streamed_tool_json() {
    let malformed_tool_json_delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"A\H\",\"text\":\"col1	col2\"}"}}"#;

    let body = create_sse_body(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_test",
                    "name": "edit",
                    "input": {}
                }
            })
            .to_string(),
        ),
        ("content_block_delta", malformed_tool_json_delta.to_string()),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        (
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    let message = &outcome.message;

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(message.error_message.is_none());

    let tool_call = message
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolCall { arguments, .. } => Some(arguments),
            _ => None,
        })
        .expect("tool call present");
    assert_eq!(tool_call, &json!({ "path": "A\\H", "text": "col1\tcol2" }));
}

#[test]
fn preserves_refusal_stop_details_from_message_delta() {
    let explanation = "This request triggered restrictions on violative cyber content and was blocked under Anthropic's Usage Policy.";
    let body = create_sse_body(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_01XFUDYJgAACzvnptvVoYEL",
                    "usage": {
                        "input_tokens": 412,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "refusal",
                    "stop_details": {
                        "type": "refusal",
                        "category": "cyber",
                        "explanation": explanation
                    }
                },
                "usage": {
                    "input_tokens": 412,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        (
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(outcome.message.error_message.as_deref(), Some(explanation));
    // The terminal event is an error carrying the accumulated message.
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn message_delta_without_usage_is_noop_for_usage_accumulation() {
    let events: Vec<(&str, String)> = minimal_events()
        .into_iter()
        .map(|(name, data)| {
            if name == "message_delta" {
                (
                    "message_delta",
                    json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } })
                        .to_string(),
                )
            } else {
                (name, data)
            }
        })
        .collect();
    let body = create_sse_body(&events);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert!(outcome.message.error_message.is_none());
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Text {
            text: "Hello".to_string(),
            text_signature: None,
        }]
    );
    assert_eq!(outcome.message.usage.input, 12);
    assert_eq!(outcome.message.usage.total_tokens, 12);
}

#[test]
fn ignores_unknown_sse_events_after_message_stop() {
    let mut events = minimal_events();
    events.push(("done", "[DONE]".to_string()));
    events.push(("proxy.stats", "not json".to_string()));
    let body = create_sse_body(&events);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert!(outcome.message.error_message.is_none());
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Text {
            text: "Hello".to_string(),
            text_signature: None,
        }]
    );
}

#[test]
fn full_lifecycle_event_ordering() {
    let outcome = parse_sse_stream(&create_sse_body(&minimal_events()), &test_model(), false, 0);
    assert_eq!(
        event_kinds(&outcome),
        ["start", "text_start", "text_delta", "text_end", "done",]
    );
}

#[test]
fn errors_when_stream_ends_before_message_stop() {
    // Drop the terminating message_stop event.
    let events: Vec<(&str, String)> = minimal_events()
        .into_iter()
        .filter(|(name, _)| *name != "message_stop")
        .collect();
    let body = create_sse_body(&events);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("Anthropic stream ended before message_stop")
    );
}

#[test]
fn error_sse_event_terminates_stream() {
    let body = create_sse_body(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": { "id": "msg_x", "usage": {
                    "input_tokens": 1, "output_tokens": 0,
                    "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
                } }
            })
            .to_string(),
        ),
        ("error", "overloaded_error".to_string()),
    ]);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("overloaded_error")
    );
}

#[test]
fn thinking_and_tool_and_text_lifecycle() {
    let body = create_sse_body(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": { "id": "msg_x", "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
                } }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }).to_string(),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "hmm" } }).to_string(),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig" } }).to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "t1", "name": "echo", "input": {} } }).to_string(),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"text\":\"hi\"}" } }).to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 1 }).to_string(),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": {
                "input_tokens": 10, "output_tokens": 7,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
            } }).to_string(),
        ),
        (
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]);

    let outcome = parse_sse_stream(&body, &test_model(), false, 0);
    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );

    // Thinking signature accumulated from signature_delta.
    match &outcome.message.content[0] {
        ContentBlock::Thinking {
            thinking,
            thinking_signature,
            ..
        } => {
            assert_eq!(thinking, "hmm");
            assert_eq!(thinking_signature.as_deref(), Some("sig"));
        }
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &outcome.message.content[1] {
        ContentBlock::ToolCall {
            name, arguments, ..
        } => {
            assert_eq!(name, "echo");
            assert_eq!(arguments, &json!({ "text": "hi" }));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);
    assert_eq!(outcome.message.usage.output, 7);
}
