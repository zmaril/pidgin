// straitjacket-allow-file:duplication — these tests transcribe pi's Mistral
// fixtures and payload-capture assertions. The model/context builders and the
// per-case chunk objects are near-identical by design; the clone detector reads
// them as duplicates, but they are distinct, load-bearing wire fixtures kept
// faithful to pi's `mistral-tool-schema.test.ts`, `mistral-reasoning-mode.test.ts`,
// and the `consumeChatStream` behaviour.
//! Unit tests for the Mistral conversations driver, mirroring pi's
//! `packages/ai/test/mistral-tool-schema.test.ts`,
//! `mistral-reasoning-mode.test.ts`, and the `chat.stream` decode contract.

use super::*;
use crate::types::{Message, ModelThinkingLevel, UserContent, UserMessage, UserRole};
use serde_json::json;

fn test_cost() -> ModelCost {
    ModelCost {
        input: 1.0,
        output: 5.0,
        cache_read: 0.1,
        cache_write: 1.25,
        tiers: None,
    }
}

/// A minimal Mistral model fixture. `reasoning` / `thinking_level_map` are the
/// fields `streamSimple`'s reasoning selection reads.
fn model(id: &str, reasoning: bool) -> MistralModel {
    MistralModel {
        id: id.to_string(),
        api: "mistral-conversations".to_string(),
        provider: "mistral".to_string(),
        cost: test_cost(),
        reasoning,
        input: vec![Modality::Text],
        thinking_level_map: None,
        base_url: "http://127.0.0.1:9".to_string(),
        max_tokens: 8192,
        headers: None,
    }
}

fn user_text(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn hello_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![user_text("Hello")],
        tools: None,
    }
}

// ---------------------------------------------------------------------------
// mistral-tool-schema.test.ts — tool params clean + strict:false
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_has_strict_false_and_clean_params() {
    let m = model("devstral-medium-latest", false);
    let parameters = json!({
        "type": "object",
        "properties": {
            "nested": {
                "type": "object",
                "properties": { "value": { "type": "string" } }
            }
        }
    });
    let context = Context {
        system_prompt: None,
        messages: vec![user_text("Hi")],
        tools: Some(vec![json!({
            "name": "inspect_schema",
            "description": "Inspect the schema",
            "parameters": parameters.clone()
        })]),
    };

    let payload = build_chat_payload(&m, &context, &MistralOptions::default());
    let tools = payload["tools"].as_array().expect("tools present");
    assert_eq!(tools.len(), 1);

    let function = &tools[0]["function"];
    assert_eq!(function["name"], json!("inspect_schema"));
    assert_eq!(tools[0]["type"], json!("function"));
    // `strict:false` is load-bearing (Mistral rejects strict tool validation).
    assert_eq!(function["strict"], json!(false));

    // Params round-trip cleanly (pi's stripSymbolKeys removes only JS symbol keys;
    // the observable JSON payload is preserved).
    assert_eq!(function["parameters"], parameters);
    assert_eq!(
        function["parameters"]["properties"]["nested"]["properties"]["value"]["type"],
        json!("string")
    );
}

// ---------------------------------------------------------------------------
// mistral-reasoning-mode.test.ts — reasoning-mode selection + prompt cache key
// ---------------------------------------------------------------------------

fn capture_payload(m: &MistralModel, options: SimpleMistralOptions) -> Value {
    let resolved = resolve_simple_options(m, &options);
    build_chat_payload(m, &hello_context(), &resolved)
}

#[test]
fn uses_reasoning_effort_for_mistral_small_4() {
    let payload = capture_payload(
        &model("mistral-small-2603", true),
        SimpleMistralOptions {
            reasoning: Some(ModelThinkingLevel::Medium),
            ..Default::default()
        },
    );
    assert_eq!(payload["reasoningEffort"], json!("high"));
    assert!(payload.get("promptMode").is_none());
}

#[test]
fn omits_reasoning_controls_for_mistral_small_4_when_off() {
    let payload = capture_payload(
        &model("mistral-small-2603", true),
        SimpleMistralOptions::default(),
    );
    assert!(payload.get("reasoningEffort").is_none());
    assert!(payload.get("promptMode").is_none());
}

#[test]
fn uses_prompt_mode_for_magistral_reasoning_models() {
    let payload = capture_payload(
        &model("magistral-medium-latest", true),
        SimpleMistralOptions {
            reasoning: Some(ModelThinkingLevel::Medium),
            ..Default::default()
        },
    );
    assert_eq!(payload["promptMode"], json!("reasoning"));
    assert!(payload.get("reasoningEffort").is_none());
}

#[test]
fn uses_reasoning_effort_for_mistral_medium_35() {
    let payload = capture_payload(
        &model("mistral-medium-3.5", true),
        SimpleMistralOptions {
            reasoning: Some(ModelThinkingLevel::Medium),
            ..Default::default()
        },
    );
    assert_eq!(payload["reasoningEffort"], json!("high"));
    assert!(payload.get("promptMode").is_none());
}

#[test]
fn omits_reasoning_controls_for_mistral_medium_35_when_off() {
    let payload = capture_payload(
        &model("mistral-medium-3.5", true),
        SimpleMistralOptions::default(),
    );
    assert!(payload.get("reasoningEffort").is_none());
    assert!(payload.get("promptMode").is_none());
}

#[test]
fn uses_session_id_as_prompt_cache_key() {
    let payload = capture_payload(
        &model("mistral-large-latest", false),
        SimpleMistralOptions {
            session_id: Some("session-123".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(payload["promptCacheKey"], json!("session-123"));
}

#[test]
fn omits_prompt_cache_key_when_cache_retention_disabled() {
    let payload = capture_payload(
        &model("mistral-large-latest", false),
        SimpleMistralOptions {
            session_id: Some("session-123".to_string()),
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
    );
    assert!(payload.get("promptCacheKey").is_none());
}

#[test]
fn prompt_caching_sets_x_affinity_header() {
    let m = model("mistral-large-latest", false);
    let resolved = resolve_simple_options(
        &m,
        &SimpleMistralOptions {
            session_id: Some("session-123".to_string()),
            ..Default::default()
        },
    );
    let headers = build_request_headers(&m, &resolved);
    assert_eq!(
        headers.get("x-affinity").map(String::as_str),
        Some("session-123")
    );
}

// ---------------------------------------------------------------------------
// 9-char tool-id normalization (deriveMistralToolCallId / normalizer)
// ---------------------------------------------------------------------------

#[test]
fn already_nine_char_alnum_id_is_verbatim() {
    // attempt 0 + normalized length 9 → used as-is.
    let id = derive_mistral_tool_call_id("abcdef123", 0);
    assert_eq!(id, "abcdef123");
}

#[test]
fn long_id_is_hashed_to_nine_chars() {
    let id = derive_mistral_tool_call_id("call_this_is_way_too_long_and_has_symbols!!", 0);
    assert_eq!(id.len(), 9);
    assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    // Deterministic for the same seed.
    assert_eq!(
        id,
        derive_mistral_tool_call_id("call_this_is_way_too_long_and_has_symbols!!", 0)
    );
}

#[test]
fn short_hash_is_deterministic_and_base36() {
    // Reproduces pi's `shortHash` (utils/hash.ts): two base36 unsigned 32-bit
    // halves. Deterministic and stable across calls.
    let a = short_hash("toolcall:0");
    assert_eq!(a, short_hash("toolcall:0"));
    assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    // The empty-string seed yields a fixed, non-empty digest.
    assert!(!short_hash("").is_empty());
}

#[test]
fn normalizer_dedupes_colliding_ids() {
    let mut normalizer = MistralToolCallIdNormalizer::new();
    let a = normalizer.normalize("x");
    let b = normalizer.normalize("x");
    // Stable per original id.
    assert_eq!(a, b);
    assert_eq!(a.len(), 9);

    // A different original id yields a different normalized id (collision-free).
    let c = normalizer.normalize("some-other-original-id");
    assert_ne!(a, c);
}

// ---------------------------------------------------------------------------
// toChatMessages — tool results as {role:"tool"} messages
// ---------------------------------------------------------------------------

#[test]
fn tool_result_becomes_role_tool_message() {
    use crate::types::{ToolResultMessage, ToolResultRole};
    let messages = vec![Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call_1".to_string(),
        tool_name: "search".to_string(),
        content: vec![ContentBlock::Text {
            text: "result body".to_string(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    })];
    let chat = to_chat_messages(&messages, false);
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0]["role"], json!("tool"));
    assert_eq!(chat[0]["toolCallId"], json!("call_1"));
    assert_eq!(chat[0]["name"], json!("search"));
    assert_eq!(chat[0]["content"][0]["text"], json!("result body"));
}

#[test]
fn errored_tool_result_gets_error_prefix() {
    use crate::types::{ToolResultMessage, ToolResultRole};
    let messages = vec![Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call_1".to_string(),
        tool_name: "search".to_string(),
        content: vec![ContentBlock::Text {
            text: "boom".to_string(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: true,
        timestamp: 0,
    })];
    let chat = to_chat_messages(&messages, false);
    assert_eq!(chat[0]["content"][0]["text"], json!("[tool error] boom"));
}

#[test]
fn system_prompt_is_unshifted() {
    let context = Context {
        system_prompt: Some("be terse".to_string()),
        messages: vec![user_text("Hello")],
        tools: None,
    };
    let payload = build_chat_payload(
        &model("mistral-large-latest", false),
        &context,
        &MistralOptions::default(),
    );
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], json!("system"));
    assert_eq!(messages[0]["content"], json!("be terse"));
    assert_eq!(messages[1]["role"], json!("user"));
}

// ---------------------------------------------------------------------------
// consumeChatStream — streaming decode into events + final message
// ---------------------------------------------------------------------------

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
fn decodes_text_stream_with_usage_and_cost() {
    let chunks = vec![
        json!({ "id": "resp_1", "choices": [{ "index": 0, "delta": { "content": "Hello" } }] }),
        json!({
            "id": "resp_1",
            "choices": [{ "index": 0, "delta": { "content": " world" }, "finishReason": "stop" }],
            "usage": { "promptTokens": 10, "completionTokens": 5, "totalTokens": 15 }
        }),
    ];
    let outcome = parse_chat_stream(&chunks, &model("mistral-large-latest", false), 0);

    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert_eq!(outcome.message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Text {
            text: "Hello world".to_string(),
            text_signature: None,
        }]
    );
    assert_eq!(outcome.message.usage.input, 10);
    assert_eq!(outcome.message.usage.output, 5);
    assert_eq!(outcome.message.usage.total_tokens, 15);
    // Cost is recomputed from the model's rates.
    assert!(outcome.message.usage.cost.total > 0.0);
}

#[test]
fn decodes_thinking_then_text() {
    let chunks = vec![
        json!({
            "choices": [{
                "index": 0,
                "delta": { "content": [{ "type": "thinking", "thinking": [{ "type": "text", "text": "pondering" }] }] }
            }]
        }),
        json!({
            "choices": [{ "index": 0, "delta": { "content": "answer" }, "finishReason": "stop" }]
        }),
    ];
    let outcome = parse_chat_stream(&chunks, &model("magistral-medium-latest", true), 0);
    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    match &outcome.message.content[0] {
        ContentBlock::Thinking { thinking, .. } => assert_eq!(thinking, "pondering"),
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &outcome.message.content[1] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "answer"),
        other => panic!("expected text block, got {other:?}"),
    }
}

#[test]
fn decodes_tool_call_stream() {
    let chunks = vec![
        json!({
            "id": "r",
            "choices": [{
                "index": 0,
                "delta": {
                    "toolCalls": [{
                        "id": "call_ABC",
                        "index": 0,
                        "function": { "name": "echo", "arguments": "{\"text\":" }
                    }]
                }
            }]
        }),
        json!({
            "id": "r",
            "choices": [{
                "index": 0,
                "delta": {
                    "toolCalls": [{
                        "id": "call_ABC",
                        "index": 0,
                        "function": { "name": "echo", "arguments": "\"hi\"}" }
                    }]
                },
                "finishReason": "tool_calls"
            }]
        }),
    ];
    let outcome = parse_chat_stream(&chunks, &model("mistral-large-latest", false), 0);
    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);
    match &outcome.message.content[0] {
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            // Provider-supplied id is kept verbatim in the stream path.
            assert_eq!(id, "call_ABC");
            assert_eq!(name, "echo");
            assert_eq!(arguments, &json!({ "text": "hi" }));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn synthesizes_tool_id_when_absent() {
    let chunks = vec![json!({
        "choices": [{
            "index": 0,
            "delta": {
                "toolCalls": [{
                    "index": 0,
                    "function": { "name": "noop", "arguments": "{}" }
                }]
            },
            "finishReason": "tool_calls"
        }]
    })];
    let outcome = parse_chat_stream(&chunks, &model("mistral-large-latest", false), 0);
    match &outcome.message.content[0] {
        ContentBlock::ToolCall { id, .. } => {
            assert_eq!(id, &derive_mistral_tool_call_id("toolcall:0", 0));
            assert_eq!(id.len(), 9);
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn reads_cached_prompt_tokens_from_details() {
    let chunks = vec![json!({
        "choices": [{ "index": 0, "delta": { "content": "hi" }, "finishReason": "stop" }],
        "usage": {
            "promptTokens": 100,
            "completionTokens": 4,
            "totalTokens": 104,
            "promptTokensDetails": { "cachedTokens": 40 }
        }
    })];
    let outcome = parse_chat_stream(&chunks, &model("mistral-large-latest", false), 0);
    assert_eq!(outcome.message.usage.cache_read, 40);
    assert_eq!(outcome.message.usage.input, 60);
    assert_eq!(outcome.message.usage.output, 4);
}

#[test]
fn error_finish_reason_produces_error_event() {
    let chunks = vec![json!({
        "choices": [{ "index": 0, "delta": {}, "finishReason": "error" }]
    })];
    let outcome = parse_chat_stream(&chunks, &model("mistral-large-latest", false), 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("An unknown error occurred")
    );
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn maps_model_length_to_length_stop() {
    let chunks = vec![json!({
        "choices": [{ "index": 0, "delta": { "content": "x" }, "finishReason": "model_length" }]
    })];
    let outcome = parse_chat_stream(&chunks, &model("mistral-large-latest", false), 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Length);
}

#[test]
fn json_boundary_roundtrips() {
    let chunks_json = json!([
        { "id": "r", "choices": [{ "index": 0, "delta": { "content": "hi" }, "finishReason": "stop" }] }
    ])
    .to_string();
    let model_json = json!({
        "id": "mistral-large-latest",
        "api": "mistral-conversations",
        "provider": "mistral",
        "cost": { "input": 1.0, "output": 5.0, "cacheRead": 0.1, "cacheWrite": 1.25 }
    })
    .to_string();
    let out = parse_chat_stream_to_json(&chunks_json, &model_json, 0).expect("valid");
    assert!(out.contains("\"type\":\"done\""));
    assert!(out.contains("\"text\":\"hi\""));
}
