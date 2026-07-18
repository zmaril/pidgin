//! Golden tests for the proxy-stream reconstruction.
//!
//! pi's own suite has **zero** coverage of `proxy.ts`, so every expected value
//! here is derived directly from the `proxy.ts` source semantics (quoted inline
//! where a golden value is non-obvious), not ported from an upstream test.

use super::*;

use atilla_ai::seams::clock::FakeClock;
use atilla_ai::{Modality, ModelCost};
use serde_json::json;

/// A fixed clock so the reconstructed `timestamp` is deterministic.
const FIXED_NOW_MS: i64 = 1_700_000_000_000;

fn test_clock() -> FakeClock {
    FakeClock::new(FIXED_NOW_MS)
}

/// A minimal model carrying just the `api`/`provider`/`id` that
/// `ProxyPartial::new` copies into the reconstructed message.
fn test_model() -> Model {
    Model {
        id: "model-x".to_string(),
        name: "Model X".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        base_url: "https://example.test".to_string(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 200_000,
        max_tokens: 8_192,
        headers: None,
        compat: None,
    }
}

fn sample_usage() -> Usage {
    Usage {
        input: 12,
        output: 34,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 46,
        cost: UsageCost::default(),
    }
}

// ---------------------------------------------------------------------------
// Wire-shape: the bandwidth-stripped SSE payloads deserialize byte-for-byte.
// ---------------------------------------------------------------------------

#[test]
fn proxy_event_wire_shapes_round_trip() {
    // Each JSON is exactly what the server's `JSON.stringify(proxyEvent)` emits.
    let cases: Vec<(serde_json::Value, ProxyAssistantMessageEvent)> = vec![
        (
            json!({ "type": "start" }),
            ProxyAssistantMessageEvent::Start,
        ),
        (
            json!({ "type": "text_start", "contentIndex": 0 }),
            ProxyAssistantMessageEvent::TextStart { content_index: 0 },
        ),
        (
            json!({ "type": "text_delta", "contentIndex": 0, "delta": "hi" }),
            ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "hi".to_string(),
            },
        ),
        (
            json!({ "type": "text_end", "contentIndex": 0 }),
            ProxyAssistantMessageEvent::TextEnd {
                content_index: 0,
                content_signature: None,
            },
        ),
        (
            json!({ "type": "text_end", "contentIndex": 0, "contentSignature": "sig" }),
            ProxyAssistantMessageEvent::TextEnd {
                content_index: 0,
                content_signature: Some("sig".to_string()),
            },
        ),
        (
            json!({ "type": "thinking_start", "contentIndex": 1 }),
            ProxyAssistantMessageEvent::ThinkingStart { content_index: 1 },
        ),
        (
            json!({ "type": "thinking_delta", "contentIndex": 1, "delta": "mm" }),
            ProxyAssistantMessageEvent::ThinkingDelta {
                content_index: 1,
                delta: "mm".to_string(),
            },
        ),
        (
            json!({ "type": "thinking_end", "contentIndex": 1, "contentSignature": "ts" }),
            ProxyAssistantMessageEvent::ThinkingEnd {
                content_index: 1,
                content_signature: Some("ts".to_string()),
            },
        ),
        (
            json!({ "type": "toolcall_start", "contentIndex": 2, "id": "call_1", "toolName": "read" }),
            ProxyAssistantMessageEvent::ToolcallStart {
                content_index: 2,
                id: "call_1".to_string(),
                tool_name: "read".to_string(),
            },
        ),
        (
            json!({ "type": "toolcall_delta", "contentIndex": 2, "delta": "{" }),
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 2,
                delta: "{".to_string(),
            },
        ),
        (
            json!({ "type": "toolcall_end", "contentIndex": 2 }),
            ProxyAssistantMessageEvent::ToolcallEnd { content_index: 2 },
        ),
    ];

    for (wire, expected) in cases {
        let parsed: ProxyAssistantMessageEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(parsed, expected, "deserialize {wire}");
        assert_eq!(
            serde_json::to_value(&expected).unwrap(),
            wire,
            "serialize {wire}"
        );
    }
}

#[test]
fn done_and_error_wire_shapes_round_trip() {
    let done = json!({
        "type": "done",
        "reason": "toolUse",
        "usage": serde_json::to_value(sample_usage()).unwrap(),
    });
    let parsed: ProxyAssistantMessageEvent = serde_json::from_value(done.clone()).unwrap();
    assert_eq!(
        parsed,
        ProxyAssistantMessageEvent::Done {
            reason: StopReason::ToolUse,
            usage: sample_usage(),
        }
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), done);

    let error = json!({
        "type": "error",
        "reason": "error",
        "errorMessage": "boom",
        "usage": serde_json::to_value(sample_usage()).unwrap(),
    });
    let parsed: ProxyAssistantMessageEvent = serde_json::from_value(error.clone()).unwrap();
    assert_eq!(
        parsed,
        ProxyAssistantMessageEvent::Error {
            reason: StopReason::Error,
            error_message: Some("boom".to_string()),
            usage: sample_usage(),
        }
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), error);
}

#[test]
fn unknown_tag_falls_through_to_unknown_variant() {
    let parsed: ProxyAssistantMessageEvent =
        serde_json::from_value(json!({ "type": "future_event", "x": 1 })).unwrap();
    assert_eq!(parsed, ProxyAssistantMessageEvent::Unknown);
}

// ---------------------------------------------------------------------------
// Per-event effect on the reconstructed message (unit-level state machine).
// ---------------------------------------------------------------------------

#[test]
fn start_reconstructs_empty_partial_from_model() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    let event = state
        .process_proxy_event(ProxyAssistantMessageEvent::Start)
        .unwrap()
        .unwrap();

    // return { type: "start", partial };  — partial stamped from the model.
    let AssistantMessageEvent::Start { partial } = event else {
        panic!("expected Start");
    };
    assert_eq!(partial.role, AssistantRole::Assistant);
    assert_eq!(partial.api, "anthropic-messages");
    assert_eq!(partial.provider, "anthropic");
    assert_eq!(partial.model, "model-x");
    assert!(partial.content.is_empty());
    assert_eq!(partial.stop_reason, StopReason::Stop);
    assert_eq!(partial.error_message, None);
    assert_eq!(partial.timestamp, FIXED_NOW_MS);
    assert_eq!(partial.usage.total_tokens, 0);
}

#[test]
fn text_lifecycle_accumulates_and_signs() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);

    state
        .process_proxy_event(ProxyAssistantMessageEvent::TextStart { content_index: 0 })
        .unwrap();
    state
        .process_proxy_event(ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "Hello, ".to_string(),
        })
        .unwrap();
    state
        .process_proxy_event(ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "world".to_string(),
        })
        .unwrap();
    let end = state
        .process_proxy_event(ProxyAssistantMessageEvent::TextEnd {
            content_index: 0,
            content_signature: Some("sig-1".to_string()),
        })
        .unwrap()
        .unwrap();

    // return { ..., content: content.text, partial };
    let AssistantMessageEvent::TextEnd {
        content_index,
        content,
        ..
    } = end
    else {
        panic!("expected TextEnd");
    };
    assert_eq!(content_index, 0);
    assert_eq!(content, "Hello, world");

    // content.textSignature = proxyEvent.contentSignature;
    assert_eq!(
        state.message.content[0],
        ContentBlock::Text {
            text: "Hello, world".to_string(),
            text_signature: Some("sig-1".to_string()),
        }
    );
}

#[test]
fn thinking_lifecycle_accumulates_and_signs() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);

    state
        .process_proxy_event(ProxyAssistantMessageEvent::ThinkingStart { content_index: 0 })
        .unwrap();
    state
        .process_proxy_event(ProxyAssistantMessageEvent::ThinkingDelta {
            content_index: 0,
            delta: "step 1 ".to_string(),
        })
        .unwrap();
    let end = state
        .process_proxy_event(ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 0,
            content_signature: Some("think-sig".to_string()),
        })
        .unwrap()
        .unwrap();

    let AssistantMessageEvent::ThinkingEnd { content, .. } = end else {
        panic!("expected ThinkingEnd");
    };
    assert_eq!(content, "step 1 ");
    assert_eq!(
        state.message.content[0],
        ContentBlock::Thinking {
            thinking: "step 1 ".to_string(),
            thinking_signature: Some("think-sig".to_string()),
            redacted: None,
        }
    );
}

#[test]
fn toolcall_partial_json_accumulates_incrementally() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);

    state
        .process_proxy_event(ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 0,
            id: "call_1".to_string(),
            tool_name: "read".to_string(),
        })
        .unwrap();

    // At start, arguments is `{}` (pi's `arguments: {}`).
    let ContentBlock::ToolCall { arguments, .. } = &state.message.content[0] else {
        panic!("expected toolCall");
    };
    assert_eq!(*arguments, json!({}));

    // First fragment `{"path":` is a dangling key — parseStreamingJson drops it,
    // so arguments stays `{}` (see json_parse's `streaming_json_dangling_key_dropped`).
    state
        .process_proxy_event(ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "{\"path\":".to_string(),
        })
        .unwrap();
    let ContentBlock::ToolCall { arguments, .. } = &state.message.content[0] else {
        panic!("expected toolCall");
    };
    assert_eq!(*arguments, json!({}));

    // Completing the value yields the full object.
    state
        .process_proxy_event(ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "\"a.txt\"}".to_string(),
        })
        .unwrap();
    let ContentBlock::ToolCall { arguments, .. } = &state.message.content[0] else {
        panic!("expected toolCall");
    };
    assert_eq!(*arguments, json!({ "path": "a.txt" }));

    // toolcall_end emits the finalized block; the partialJson side channel is
    // dropped (pi `delete content.partialJson`) — the block still has no such field.
    let end = state
        .process_proxy_event(ProxyAssistantMessageEvent::ToolcallEnd { content_index: 0 })
        .unwrap()
        .unwrap();
    let AssistantMessageEvent::ToolcallEnd { tool_call, .. } = end else {
        panic!("expected ToolcallEnd");
    };
    assert_eq!(
        tool_call,
        ContentBlock::ToolCall {
            id: "call_1".to_string(),
            name: "read".to_string(),
            arguments: json!({ "path": "a.txt" }),
            thought_signature: None,
        }
    );
    assert!(!state.tool_call_json.contains_key(&0));
}

// ---------------------------------------------------------------------------
// Error branches (pi's `throw` and the `undefined`/skip arms).
// ---------------------------------------------------------------------------

#[test]
fn text_delta_on_wrong_type_returns_err() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    // No text_start: index 0 is a hole → error branch.
    let err = state
        .process_proxy_event(ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".to_string(),
        })
        .unwrap_err();
    assert_eq!(err, "Received text_delta for non-text content");
}

#[test]
fn thinking_delta_on_wrong_type_returns_err() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    state
        .process_proxy_event(ProxyAssistantMessageEvent::TextStart { content_index: 0 })
        .unwrap();
    let err = state
        .process_proxy_event(ProxyAssistantMessageEvent::ThinkingDelta {
            content_index: 0,
            delta: "x".to_string(),
        })
        .unwrap_err();
    assert_eq!(err, "Received thinking_delta for non-thinking content");
}

#[test]
fn toolcall_delta_on_wrong_type_returns_err() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    state
        .process_proxy_event(ProxyAssistantMessageEvent::TextStart { content_index: 0 })
        .unwrap();
    let err = state
        .process_proxy_event(ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "{".to_string(),
        })
        .unwrap_err();
    assert_eq!(err, "Received toolcall_delta for non-toolCall content");
}

#[test]
fn toolcall_end_on_non_toolcall_returns_none_without_throwing() {
    // pi's toolcall_end for non-toolCall content: `return undefined;` (NO throw).
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    state
        .process_proxy_event(ProxyAssistantMessageEvent::TextStart { content_index: 0 })
        .unwrap();
    let out = state
        .process_proxy_event(ProxyAssistantMessageEvent::ToolcallEnd { content_index: 0 })
        .unwrap();
    assert!(out.is_none());
}

#[test]
fn unknown_event_produces_no_output() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    let out = state
        .process_proxy_event(ProxyAssistantMessageEvent::Unknown)
        .unwrap();
    assert!(out.is_none());
}

// ---------------------------------------------------------------------------
// Full eager transform via `stream_proxy`.
// ---------------------------------------------------------------------------

#[test]
fn full_start_text_toolcall_done_sequence() {
    let clock = test_clock();
    let proxy_events = vec![
        ProxyAssistantMessageEvent::Start,
        ProxyAssistantMessageEvent::TextStart { content_index: 0 },
        ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "Hi".to_string(),
        },
        ProxyAssistantMessageEvent::TextEnd {
            content_index: 0,
            content_signature: None,
        },
        ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 1,
            id: "call_1".to_string(),
            tool_name: "read".to_string(),
        },
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 1,
            delta: "{\"path\":".to_string(),
        },
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 1,
            delta: "\"a.txt\"}".to_string(),
        },
        ProxyAssistantMessageEvent::ToolcallEnd { content_index: 1 },
        ProxyAssistantMessageEvent::Done {
            reason: StopReason::ToolUse,
            usage: sample_usage(),
        },
    ];

    let result = stream_proxy(&test_model(), &clock, proxy_events, None);

    // Every proxy event above emits exactly one reconstructed event.
    assert_eq!(result.events.len(), 9);
    assert!(matches!(
        result.events[0],
        AssistantMessageEvent::Start { .. }
    ));
    assert!(matches!(
        result.events.last(),
        Some(AssistantMessageEvent::Done {
            reason: StopReason::ToolUse,
            ..
        })
    ));

    // Final reconstructed message.
    let message = &result.message;
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.usage, sample_usage());
    assert_eq!(
        message.content,
        vec![
            ContentBlock::Text {
                text: "Hi".to_string(),
                text_signature: None,
            },
            ContentBlock::ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: json!({ "path": "a.txt" }),
                thought_signature: None,
            },
        ]
    );

    // The Done event's message equals the final message (pi: `message: partial`).
    let AssistantMessageEvent::Done {
        message: done_msg, ..
    } = result.events.last().unwrap()
    else {
        panic!("expected Done last");
    };
    assert_eq!(done_msg, message);
}

#[test]
fn server_error_event_is_terminal_error() {
    // A server-sent `error` proxy event flows through normally (pi does not throw).
    let clock = test_clock();
    let proxy_events = vec![
        ProxyAssistantMessageEvent::Start,
        ProxyAssistantMessageEvent::Error {
            reason: StopReason::Error,
            error_message: Some("upstream 500".to_string()),
            usage: sample_usage(),
        },
    ];

    let result = stream_proxy(&test_model(), &clock, proxy_events, None);
    assert_eq!(result.events.len(), 2);

    let AssistantMessageEvent::Error { reason, error } = result.events.last().unwrap() else {
        panic!("expected Error last");
    };
    assert_eq!(*reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("upstream 500"));
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.usage, sample_usage());

    assert_eq!(result.message.stop_reason, StopReason::Error);
    assert_eq!(
        result.message.error_message.as_deref(),
        Some("upstream 500")
    );
}

#[test]
fn thrown_error_is_caught_into_terminal_error_event() {
    // A text_delta with no preceding text_start throws inside process_proxy_event;
    // stream_proxy's catch synthesizes a terminal error (pi's try/catch).
    let clock = test_clock();
    let proxy_events = vec![
        ProxyAssistantMessageEvent::Start,
        ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".to_string(),
        },
        // Never reached — processing stops at the throw.
        ProxyAssistantMessageEvent::Done {
            reason: StopReason::Stop,
            usage: sample_usage(),
        },
    ];

    let result = stream_proxy(&test_model(), &clock, proxy_events, None);

    // Start emitted, then the synthesized error; the Done is never processed.
    assert_eq!(result.events.len(), 2);
    let AssistantMessageEvent::Error { reason, error } = result.events.last().unwrap() else {
        panic!("expected Error last");
    };
    assert_eq!(*reason, StopReason::Error);
    assert_eq!(
        error.error_message.as_deref(),
        Some("Received text_delta for non-text content")
    );
    assert_eq!(result.message.stop_reason, StopReason::Error);
    // usage was never overwritten by a done/error proxy event: stays zeroed.
    assert_eq!(result.message.usage.total_tokens, 0);
}

#[test]
fn aborted_signal_yields_aborted_error() {
    let clock = test_clock();
    let signal = AbortSignal::aborted();
    let proxy_events = vec![ProxyAssistantMessageEvent::Start];

    let result = stream_proxy(&test_model(), &clock, proxy_events, Some(&signal));

    // The abort guard fires before the first event, so no events reconstruct
    // besides the synthesized terminal error.
    assert_eq!(result.events.len(), 1);
    let AssistantMessageEvent::Error { reason, error } = &result.events[0] else {
        panic!("expected Error");
    };
    assert_eq!(*reason, StopReason::Aborted);
    assert_eq!(
        error.error_message.as_deref(),
        Some("Request aborted by user")
    );
    assert_eq!(result.message.stop_reason, StopReason::Aborted);
}

#[test]
fn done_reason_and_usage_overwrite_partial() {
    let clock = test_clock();
    let mut state = ProxyPartial::new(&test_model(), &clock);
    let event = state
        .process_proxy_event(ProxyAssistantMessageEvent::Done {
            reason: StopReason::Length,
            usage: sample_usage(),
        })
        .unwrap()
        .unwrap();

    let AssistantMessageEvent::Done { reason, message } = event else {
        panic!("expected Done");
    };
    assert_eq!(reason, StopReason::Length);
    assert_eq!(message.stop_reason, StopReason::Length);
    assert_eq!(message.usage, sample_usage());
    assert_eq!(state.message.stop_reason, StopReason::Length);
}
