// straitjacket-allow-file:duplication — these tests transcribe pi's OpenAI
// Responses fixtures verbatim: the `response.*` named-event objects and the
// per-test model literals are walls of near-identical JSON by design, and the
// clone detector reads them as duplicates. They are distinct, load-bearing wire
// fixtures kept faithful to pi's test cases under
// `packages/ai/test/openai-responses-*.test.ts`.
//! Unit tests for the OpenAI Responses driver, mirroring representative cases
//! from pi's `packages/ai/test/openai-responses-*.test.ts`.

use super::*;
use crate::api::openai_responses_shared::{
    convert_responses_messages, parse_responses_sse_stream, process_responses_stream, short_hash,
    ResponsesStreamOptions, StreamOutcome,
};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, Context, Message,
    Modality, ModelThinkingLevel, StopReason, ToolResultMessage, ToolResultRole, UsageCost,
    UserContent, UserMessage, UserRole,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn base_model() -> OpenAIResponsesModel {
    OpenAIResponsesModel {
        id: "gpt-5-mini".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        cost: ModelCost {
            input: 1.0,
            output: 5.0,
            cache_read: 0.1,
            cache_write: 0.0,
            tiers: None,
        },
        reasoning: true,
        thinking_level_map: None,
        input: vec![Modality::Text],
        headers: None,
        compat: None,
    }
}

fn process(events: &[Value]) -> StreamOutcome {
    process_responses_stream(events, &base_model(), &ResponsesStreamOptions::default(), 0)
}

fn process_with(
    model: &OpenAIResponsesModel,
    events: &[Value],
    service_tier: Option<&str>,
) -> StreamOutcome {
    let options = ResponsesStreamOptions {
        service_tier: service_tier.map(str::to_string),
    };
    process_responses_stream(events, model, &options, 0)
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

// ===========================================================================
// Terminal-event handling (openai-responses-terminal-event.test.ts)
// ===========================================================================

fn early_eof_events() -> Vec<Value> {
    vec![
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_early_eof" } }),
        json!({
            "type": "response.output_item.added",
            "sequence_number": 1,
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_early_eof", "summary": [] }
        }),
        json!({
            "type": "response.reasoning_text.delta",
            "sequence_number": 2,
            "output_index": 0,
            "content_index": 0,
            "item_id": "rs_early_eof",
            "delta": "partial reasoning before the stream ends"
        }),
    ]
}

#[test]
fn rejects_streams_that_end_before_a_terminal_response_event() {
    let outcome = process(&early_eof_events());
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("OpenAI Responses stream ended before a terminal response event")
    );
    // The terminal event is an `error` carrying the accumulated message.
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn finalizes_completed_terminal_events_as_stop() {
    let events = vec![json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_completed",
            "status": "completed",
            "usage": {
                "input_tokens": 20,
                "output_tokens": 7,
                "total_tokens": 27,
                "input_tokens_details": { "cached_tokens": 2, "cache_write_tokens": 3 }
            }
        }
    })];

    let outcome = process(&events);
    assert_eq!(
        outcome.message.response_id.as_deref(),
        Some("resp_completed")
    );
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert_eq!(outcome.message.usage.input, 15);
    assert_eq!(outcome.message.usage.output, 7);
    assert_eq!(outcome.message.usage.cache_read, 2);
    assert_eq!(outcome.message.usage.cache_write, 3);
    assert_eq!(outcome.message.usage.total_tokens, 27);
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
}

#[test]
fn finalizes_incomplete_terminal_events_as_length_stops() {
    let events = vec![json!({
        "type": "response.incomplete",
        "sequence_number": 0,
        "response": {
            "id": "resp_incomplete",
            "status": "incomplete",
            "usage": {
                "input_tokens": 30,
                "output_tokens": 12,
                "total_tokens": 42,
                "input_tokens_details": { "cached_tokens": 5 }
            }
        }
    })];

    let outcome = process(&events);
    assert_eq!(
        outcome.message.response_id.as_deref(),
        Some("resp_incomplete")
    );
    assert_eq!(outcome.message.stop_reason, StopReason::Length);
    assert_eq!(outcome.message.usage.input, 25);
    assert_eq!(outcome.message.usage.output, 12);
    assert_eq!(outcome.message.usage.cache_read, 5);
    assert_eq!(outcome.message.usage.cache_write, 0);
    assert_eq!(outcome.message.usage.total_tokens, 42);
}

#[test]
fn rejects_failed_terminal_events_with_the_provider_error() {
    let events = vec![json!({
        "type": "response.failed",
        "sequence_number": 0,
        "response": {
            "id": "resp_failed",
            "status": "failed",
            "error": { "code": "server_error", "message": "boom" }
        }
    })];

    let outcome = process(&events);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("server_error: boom")
    );
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn error_event_terminates_with_code_and_message() {
    let events = vec![json!({
        "type": "error",
        "code": "rate_limit",
        "message": "slow down"
    })];

    let outcome = process(&events);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("Error Code rate_limit: slow down")
    );
}

// ===========================================================================
// Full text lifecycle + event ordering
// ===========================================================================

#[test]
fn full_text_lifecycle_event_ordering() {
    let events = vec![
        json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] }
        }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "Hello" }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "delta": " world" }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Hello world" }]
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp_1", "status": "completed" }
        }),
    ];

    let outcome = process(&events);
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
    match &outcome.message.content[0] {
        ContentBlock::Text {
            text,
            text_signature,
        } => {
            assert_eq!(text, "Hello world");
            assert_eq!(text_signature.as_deref(), Some(r#"{"v":1,"id":"msg_1"}"#));
        }
        other => panic!("expected text block, got {other:?}"),
    }
}

#[test]
fn reasoning_and_tool_lifecycle_marks_tool_use() {
    let events = vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_1", "summary": [] }
        }),
        json!({ "type": "response.reasoning_text.delta", "output_index": 0, "delta": "think" }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_1", "summary": [], "content": [{ "text": "final thought" }] }
        }),
        json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "echo", "arguments": "" }
        }),
        json!({ "type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"text\":\"hi\"}" }),
        json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "echo", "arguments": "{\"text\":\"hi\"}" }
        }),
        json!({ "type": "response.completed", "response": { "id": "resp_2", "status": "completed" } }),
    ];

    let outcome = process(&events);
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
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);
    match &outcome.message.content[0] {
        ContentBlock::Thinking { thinking, .. } => assert_eq!(thinking, "final thought"),
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &outcome.message.content[1] {
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(id, "call_1|fc_1");
            assert_eq!(name, "echo");
            assert_eq!(arguments, &json!({ "text": "hi" }));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

// ===========================================================================
// partialJson cleanup (openai-responses-partial-json-cleanup.test.ts)
// ===========================================================================

#[test]
fn removes_partial_json_from_persisted_tool_call_blocks() {
    let arguments_json = r#"{"path":"README.md","content":"updated"}"#;
    let events = vec![
        json!({
            "type": "response.output_item.added",
            "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": "" }
        }),
        json!({ "type": "response.function_call_arguments.delta", "delta": "{\"path\":\"README.md\"" }),
        json!({ "type": "response.function_call_arguments.delta", "delta": ",\"content\":\"updated\"}" }),
        json!({ "type": "response.function_call_arguments.done", "arguments": arguments_json }),
        json!({
            "type": "response.output_item.done",
            "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": arguments_json }
        }),
        json!({ "type": "response.completed", "sequence_number": 5, "response": { "id": "resp_test", "status": "completed" } }),
    ];

    let outcome = process(&events);
    assert_eq!(outcome.message.content.len(), 1);
    match &outcome.message.content[0] {
        ContentBlock::ToolCall { arguments, .. } => {
            assert_eq!(
                arguments,
                &json!({ "path": "README.md", "content": "updated" })
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }

    // The persisted tool call serializes without any `partialJson` scratch key.
    let serialized = serde_json::to_value(&outcome.message.content[0]).unwrap();
    assert!(serialized.get("partialJson").is_none());

    // And the emitted toolcall_end carries the same partialJson-free block.
    let tool_call_end = outcome
        .events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::ToolcallEnd { tool_call, .. } => Some(tool_call),
            _ => None,
        })
        .expect("toolcall_end present");
    let end_serialized = serde_json::to_value(tool_call_end).unwrap();
    assert!(end_serialized.get("partialJson").is_none());
    // stopReason becomes toolUse because a tool call is present.
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);
}

// ===========================================================================
// Service-tier pricing multiplier (openai-responses-compat.test.ts)
// ===========================================================================

fn service_tier_model(id: &str) -> OpenAIResponsesModel {
    OpenAIResponsesModel {
        id: id.to_string(),
        cost: ModelCost {
            input: 2.0,
            output: 8.0,
            cache_read: 0.2,
            cache_write: 0.0,
            tiers: None,
        },
        ..base_model()
    }
}

fn service_tier_events(service_tier: &str, token_count: u64) -> Vec<Value> {
    vec![json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "service_tier": service_tier,
            "usage": {
                "input_tokens": token_count,
                "output_tokens": token_count,
                "total_tokens": token_count * 2,
                "input_tokens_details": { "cached_tokens": 0 }
            }
        }
    })]
}

#[test]
fn applies_service_tier_cost_multipliers() {
    let token_count = 100_000u64;
    let token_scale = token_count as f64 / 1_000_000.0;

    for (model_id, tier, multiplier) in [
        ("gpt-5.4", "priority", 2.0),
        ("gpt-5.5", "priority", 2.5),
        ("gpt-5.5", "flex", 0.5),
    ] {
        let model = service_tier_model(model_id);
        let outcome = process_with(&model, &service_tier_events(tier, token_count), Some(tier));
        let cost = outcome.message.usage.cost;
        assert!(
            (cost.input - model.cost.input * multiplier * token_scale).abs() < 1e-9,
            "input cost for {model_id}/{tier}"
        );
        assert!(
            (cost.output - model.cost.output * multiplier * token_scale).abs() < 1e-9,
            "output cost for {model_id}/{tier}"
        );
        assert!(
            (cost.total - (model.cost.input + model.cost.output) * multiplier * token_scale).abs()
                < 1e-9,
            "total cost for {model_id}/{tier}"
        );
    }
}

// ===========================================================================
// Request shaping: build_params (openai-responses-compat.test.ts)
// ===========================================================================

fn user_context(text: &str) -> Context {
    Context {
        system_prompt: Some("sys".to_string()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_string()),
            timestamp: 0,
        })],
        tools: None,
    }
}

#[test]
fn build_params_core_shape() {
    let params = build_params(
        &base_model(),
        &user_context("hi"),
        &OpenAIResponsesOptions::default(),
    );
    assert_eq!(params["model"], json!("gpt-5-mini"));
    assert_eq!(params["stream"], json!(true));
    assert_eq!(params["store"], json!(false));
    // developer role for reasoning models with supportsDeveloperRole default.
    let input = params["input"].as_array().unwrap();
    assert_eq!(input[0], json!({ "role": "developer", "content": "sys" }));
    assert_eq!(
        input[1],
        json!({ "role": "user", "content": [{ "type": "input_text", "text": "hi" }] })
    );
}

#[test]
fn build_params_forwards_required_tool_choice_and_flat_tools() {
    let mut context = user_context("Do not call ping.");
    context.tools = Some(vec![json!({
        "name": "ping",
        "description": "Ping",
        "parameters": { "type": "object", "properties": { "value": { "type": "string" } } }
    })]);
    let options = OpenAIResponsesOptions {
        tool_choice: Some(json!("required")),
        ..Default::default()
    };

    let params = build_params(&base_model(), &context, &options);
    assert_eq!(params["tool_choice"], json!("required"));
    let tools = params["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    // FLAT function-tool shape (not nested under `function`).
    assert_eq!(tools[0]["type"], json!("function"));
    assert_eq!(tools[0]["name"], json!("ping"));
    assert_eq!(tools[0]["strict"], json!(false));
    assert!(tools[0].get("function").is_none());
}

#[test]
fn build_params_sends_none_reasoning_effort_when_off_absent() {
    // model.thinkingLevelMap is None (off undefined) → include reasoning "none".
    let model = base_model();
    let params = build_params(
        &model,
        &user_context("hi"),
        &OpenAIResponsesOptions::default(),
    );
    assert_eq!(params["reasoning"], json!({ "effort": "none" }));
}

#[test]
fn build_params_omits_reasoning_when_off_is_null() {
    // model.thinkingLevelMap.off === null → omit reasoning entirely.
    let mut model = base_model();
    let mut map = ThinkingLevelMap::new();
    map.insert(ModelThinkingLevel::Off, None);
    model.thinking_level_map = Some(map);
    let params = build_params(
        &model,
        &user_context("hi"),
        &OpenAIResponsesOptions::default(),
    );
    assert!(params.get("reasoning").is_none());
}

#[test]
fn build_params_sets_reasoning_effort_and_summary_when_requested() {
    let model = base_model();
    let options = OpenAIResponsesOptions {
        reasoning_effort: Some("high".to_string()),
        ..Default::default()
    };
    let params = build_params(&model, &user_context("hi"), &options);
    assert_eq!(
        params["reasoning"],
        json!({ "effort": "high", "summary": "auto" })
    );
    assert_eq!(params["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn build_params_clamps_prompt_cache_key_to_64_chars() {
    let session_id = "x".repeat(67);
    let options = OpenAIResponsesOptions {
        session_id: Some(session_id),
        ..Default::default()
    };
    let params = build_params(&base_model(), &user_context("hi"), &options);
    assert_eq!(params["prompt_cache_key"], json!("x".repeat(64)));
}

#[test]
fn build_params_max_output_tokens_min_16() {
    let options = OpenAIResponsesOptions {
        max_tokens: Some(4),
        ..Default::default()
    };
    let params = build_params(&base_model(), &user_context("hi"), &options);
    assert_eq!(params["max_output_tokens"], json!(16));
}

#[test]
fn build_params_omits_cache_key_when_retention_none() {
    let options = OpenAIResponsesOptions {
        session_id: Some("session-123".to_string()),
        cache_retention: Some(crate::types::CacheRetention::None),
        ..Default::default()
    };
    let params = build_params(&base_model(), &user_context("hi"), &options);
    assert!(params.get("prompt_cache_key").is_none());
}

// ===========================================================================
// Compat defaults + session-affinity headers (openai-responses-compat.test.ts)
// ===========================================================================

#[test]
fn compat_defaults() {
    let compat = get_compat(&base_model());
    assert!(compat.supports_developer_role);
    assert!(compat.supports_long_cache_retention);
    assert!(!compat.supports_tool_search);
    assert_eq!(
        compat.session_affinity_format,
        SessionAffinityFormat::Openai
    );
}

#[test]
fn compat_auto_detects_openrouter_affinity() {
    let mut model = base_model();
    model.provider = "openrouter".to_string();
    model.base_url = "https://openrouter.ai/api/v1".to_string();
    let compat = get_compat(&model);
    assert_eq!(
        compat.session_affinity_format,
        SessionAffinityFormat::Openrouter
    );
}

#[test]
fn session_affinity_headers_openai() {
    let compat = get_compat(&base_model());
    let headers = session_affinity_headers(&compat, Some("session-123"));
    assert_eq!(
        headers.get("session_id").map(String::as_str),
        Some("session-123")
    );
    assert_eq!(
        headers.get("x-client-request-id").map(String::as_str),
        Some("session-123")
    );
    assert!(!headers.contains_key("x-session-id"));
}

#[test]
fn session_affinity_headers_openrouter() {
    let mut model = base_model();
    model.compat = Some(OpenAIResponsesCompat {
        session_affinity_format: Some(SessionAffinityFormat::Openrouter),
        ..Default::default()
    });
    let compat = get_compat(&model);
    let headers = session_affinity_headers(&compat, Some("session-proxy"));
    assert_eq!(
        headers.get("x-session-id").map(String::as_str),
        Some("session-proxy")
    );
    assert!(!headers.contains_key("session_id"));
    assert!(!headers.contains_key("x-client-request-id"));
}

#[test]
fn session_affinity_headers_openai_nosession() {
    let mut model = base_model();
    model.compat = Some(OpenAIResponsesCompat {
        session_affinity_format: Some(SessionAffinityFormat::OpenaiNosession),
        ..Default::default()
    });
    let compat = get_compat(&model);
    let headers = session_affinity_headers(&compat, Some("session-123"));
    assert!(!headers.contains_key("session_id"));
    assert_eq!(
        headers.get("x-client-request-id").map(String::as_str),
        Some("session-123")
    );
    assert!(!headers.contains_key("x-session-id"));
}

// ===========================================================================
// Message conversion: message ids (openai-responses-message-id.test.ts)
// ===========================================================================

fn usage_zero() -> crate::types::Usage {
    crate::types::Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

fn codex_model() -> OpenAIResponsesModel {
    OpenAIResponsesModel {
        id: "gpt-5.5".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai-codex".to_string(),
        ..base_model()
    }
}

#[test]
fn generates_unique_fallback_message_ids() {
    // Cross-provider assistant (anthropic) → thinking becomes text, so two text
    // blocks in one turn get fallback ids msg_pi_1 and msg_pi_1_1.
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "private reasoning".to_string(),
                thinking_signature: None,
                redacted: None,
            },
            ContentBlock::Text {
                text: "visible answer".to_string(),
                text_signature: None,
            },
        ],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-8".to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: usage_zero(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    };
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".to_string()),
            timestamp: 0,
        }),
        Message::Assistant(assistant),
    ];

    let input = convert_responses_messages(
        &codex_model(),
        &messages,
        Some("You are concise."),
        &["openai", "openai-codex", "opencode"],
        true,
    );
    let message_ids: Vec<String> = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert_eq!(message_ids, vec!["msg_pi_1", "msg_pi_1_1"]);
}

// ===========================================================================
// Foreign tool-call id normalization (openai-responses-foreign-toolcall-id.test.ts)
// ===========================================================================

const COPILOT_RAW_TOOL_CALL_ID: &str = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";

#[test]
fn hashes_foreign_tool_item_ids_into_bounded_fc_shape() {
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            id: COPILOT_RAW_TOOL_CALL_ID.to_string(),
            name: "edit".to_string(),
            arguments: json!({ "path": "src/styles/app.css" }),
            thought_signature: None,
        }],
        api: "openai-responses".to_string(),
        provider: "github-copilot".to_string(),
        model: "gpt-5.5".to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: usage_zero(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    };
    let tool_result = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: COPILOT_RAW_TOOL_CALL_ID.to_string(),
        tool_name: "edit".to_string(),
        content: vec![ContentBlock::Text {
            text: "ok".to_string(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    };
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Use the tool.".to_string()),
            timestamp: 0,
        }),
        Message::Assistant(assistant),
        Message::ToolResult(tool_result),
    ];

    let input = convert_responses_messages(
        &codex_model(),
        &messages,
        Some("You are concise."),
        &["openai", "openai-codex", "opencode"],
        true,
    );
    let function_call = input
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .expect("function_call present");

    let item_id_raw = COPILOT_RAW_TOOL_CALL_ID.split('|').nth(1).unwrap();
    let expected = format!("fc_{}", short_hash(item_id_raw));
    let id = function_call.get("id").and_then(Value::as_str).unwrap();
    assert_eq!(id, expected);
    assert!(id.chars().count() <= 64);
    assert!(id.starts_with("fc_"));
    assert!(id[3..].chars().all(|c| c.is_ascii_alphanumeric()));
}

// ===========================================================================
// Empty tool result (openai-responses-empty-tool-result.test.ts)
// ===========================================================================

#[test]
fn empty_tool_result_uses_no_tool_output_placeholder() {
    let model = OpenAIResponsesModel {
        id: "gpt-4o-mini".to_string(),
        provider: "openai".to_string(),
        api: "openai-responses".to_string(),
        reasoning: false,
        ..base_model()
    };
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            id: "tool-1".to_string(),
            name: "bash".to_string(),
            arguments: json!({ "command": "true" }),
            thought_signature: None,
        }],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: usage_zero(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    };
    let tool_result = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "tool-1".to_string(),
        tool_name: "bash".to_string(),
        content: vec![ContentBlock::Text {
            text: String::new(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    };
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Run the command".to_string()),
            timestamp: 0,
        }),
        Message::Assistant(assistant),
        Message::ToolResult(tool_result),
    ];

    let input = convert_responses_messages(
        &model,
        &messages,
        None,
        &["openai", "openai-codex", "opencode"],
        true,
    );
    let function_call_output = input
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .expect("function_call_output present");
    assert_eq!(
        function_call_output.get("output").and_then(Value::as_str),
        Some("(no tool output)")
    );
}

/// Serialize a slice of decoded Responses events into an SSE body, one frame per
/// event carrying the real `event:` name plus the `data:` JSON.
fn sse_body(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| {
            let name = event.get("type").and_then(Value::as_str).unwrap_or("");
            format!("event: {name}\ndata: {event}\n\n")
        })
        .collect()
}

// The buffered whole-body SSE parser is byte-identical to the decoded-events
// boundary: both drive the same `OpenAIResponsesSseDecoder`, so a full text
// lifecycle yields the SAME events and terminal message. This is the
// buffered-byte-identical guarantee the streaming-native retrofit rests on.
#[test]
fn parse_responses_sse_stream_matches_process_responses_stream() {
    let events = vec![
        json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] }
        }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "Hello" }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "delta": " world" }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Hello world" }]
            }
        }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed" } }),
    ];

    let buffered = process(&events);
    let from_sse = parse_responses_sse_stream(
        &sse_body(&events),
        &base_model(),
        &ResponsesStreamOptions::default(),
        0,
    );

    assert_eq!(from_sse.events, buffered.events);
    assert_eq!(from_sse.message, buffered.message);
}

// A frame split across two chunks decodes identically to the whole body fed at
// once: the shared `AssistantEventReader`/`SseFrameSplitter` buffers the partial
// line across the chunk boundary.
#[test]
fn responses_sse_frames_split_across_chunks_decode_identically() {
    use crate::api::openai_responses_shared::OpenAIResponsesSseDecoder;
    use crate::types::AssistantMessageEvent;
    use crate::utils::sse::AssistantEventReader;

    let events = vec![
        json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] }
        }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "Hi" }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Hi" }]
            }
        }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed" } }),
    ];
    let body = sse_body(&events);
    let whole = process(&events);

    // Split the body into single-byte chunks and drive the reader.
    let chunks: Vec<std::io::Result<Vec<u8>>> =
        body.as_bytes().iter().map(|b| Ok(vec![*b])).collect();
    let decoder =
        OpenAIResponsesSseDecoder::new(base_model(), ResponsesStreamOptions::default(), 0);
    let mut reader = AssistantEventReader::new(Box::new(chunks.into_iter()), Box::new(decoder));
    let dripped: Vec<AssistantMessageEvent> = reader.by_ref().collect();

    assert_eq!(dripped, whole.events);
    assert_eq!(
        reader.result().and_then(|r| r.as_ref().ok()),
        Some(&whole.message)
    );
}
