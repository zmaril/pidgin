// straitjacket-allow-file[:duplication] — these tests transcribe pi's Anthropic
// driver/header fixtures (`anthropic-eager-tool-input-compat`, the OAuth vs
// API-key header switching `createClient` exercises, and the buffered-body SSE
// assertions). The model/context/SSE literals are walls of near-identical JSON
// by design; the clone detector reads them as duplicates, but they are distinct,
// load-bearing wire fixtures kept faithful to pi's cases.
//! Unit tests for the Anthropic stream drivers and `createClient` header
//! switching, ported from pi's fixture/mock-driven suites:
//! `packages/ai/test/anthropic-eager-tool-input-compat.test.ts` (per-tool eager
//! streaming + `anthropic-beta` beta composition) and the header-switching
//! branches `createClient` exercises (OAuth Claude-Code identity headers vs the
//! API-key `x-api-key` path). Live-key suites (`stream.test.ts`,
//! `interleaved-thinking.test.ts`, `deferred-tools.test.ts` live cases) stay out.
//!
//! pi drives these by stubbing `fetch` / standing up a local HTTP server and
//! reading the captured request; here [`ScriptedTransport`] plays that role,
//! recording the [`HttpRequest`] the driver assembles.

use serde_json::{json, Value};

use super::driver::{stream, stream_simple, stream_streaming};
use super::request::AnthropicOptions;
use super::simple_options::SimpleStreamOptions;
use crate::seams::http::{HttpRequest, HttpResponse, ScriptedTransport};
use crate::seams::provider::StreamResult;
use crate::types::{
    AnthropicMessagesCompat, AssistantMessageEvent, CacheRetention, ContentBlock, Context, Model,
    StopReason, ThinkingLevel,
};
use crate::utils::sse::AssistantEventReader;

/// Build a `Model<AnthropicMessagesCompat>` from overrides, matching the neutral
/// test model pi's `anthropic-eager-tool-input-compat` fixtures use
/// (`claude-opus-4-8`, provider `test-anthropic`, `forceAdaptiveThinking: true`).
fn make_model(overrides: Value) -> Model<AnthropicMessagesCompat> {
    let mut base = json!({
        "id": "claude-opus-4-8",
        "name": "Claude Opus 4.8",
        "api": "anthropic-messages",
        "provider": "test-anthropic",
        "baseUrl": "https://api.anthropic.test",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200000,
        "maxTokens": 32000,
        "compat": { "forceAdaptiveThinking": true },
    });
    let base_obj = base.as_object_mut().unwrap();
    for (key, value) in overrides.as_object().unwrap() {
        base_obj.insert(key.clone(), value.clone());
    }
    serde_json::from_value(base).unwrap()
}

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": "Look up a value",
        "parameters": { "type": "object", "properties": { "value": { "type": "string" } } },
    })
}

fn context_with_tools(tools: Vec<Value>) -> Context {
    let mut ctx = json!({
        "messages": [{ "role": "user", "content": "Use the tool", "timestamp": 0 }],
    });
    if !tools.is_empty() {
        ctx.as_object_mut()
            .unwrap()
            .insert("tools".to_string(), Value::Array(tools));
    }
    serde_json::from_value(ctx).unwrap()
}

/// An SSE body carrying a full `message_start` … `message_stop` exchange that
/// yields a single text block "Hello".
///
/// Shared with the provider-backend tests
/// ([`crate::providers::anthropic_backend`]), which drive the same fixture end to
/// end through the generic [`Provider`](crate::seams::provider::Provider) seam.
pub(crate) fn hello_sse_body() -> String {
    let events: Vec<(&str, String)> = vec![
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": { "input_tokens": 3, "output_tokens": 0 }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "" } })
            .to_string(),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0,
                    "delta": { "type": "text_delta", "text": "Hello" } })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" },
                    "usage": { "output_tokens": 1 } })
            .to_string(),
        ),
        (
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ),
    ];
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An SSE body that opens a message but never sends `message_stop` — the
/// truncated-stream case. Shared with the provider-backend tests.
pub(crate) fn truncated_sse_body() -> String {
    format!(
        "event: message_start\ndata: {}\n",
        json!({
            "type": "message_start",
            "message": { "id": "msg_test", "usage": { "input_tokens": 3, "output_tokens": 0 } }
        })
    )
}

fn only_request(transport: &ScriptedTransport) -> HttpRequest {
    let requests = transport.requests();
    assert_eq!(requests.len(), 1, "expected exactly one request");
    requests.into_iter().next().unwrap()
}

fn body_json(request: &HttpRequest) -> Value {
    serde_json::from_str(request.body.as_deref().unwrap()).unwrap()
}

fn first_tool(body: &Value) -> &Value {
    &body["tools"][0]
}

fn api_key_options(api_key: &str) -> AnthropicOptions {
    AnthropicOptions {
        api_key: Some(api_key.to_string()),
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// createClient header switching: OAuth vs API-key
// ---------------------------------------------------------------------------

#[test]
fn oauth_path_emits_claude_code_identity_headers() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("sk-ant-oat-secret");

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer sk-ant-oat-secret")
    );
    // Adaptive model + no extra betas -> exactly the two OAuth betas.
    assert_eq!(
        request.headers.get("anthropic-beta").map(String::as_str),
        Some("claude-code-20250219,oauth-2025-04-20")
    );
    assert_eq!(
        request.headers.get("user-agent").map(String::as_str),
        Some("claude-cli/2.1.75")
    );
    assert_eq!(
        request.headers.get("x-app").map(String::as_str),
        Some("cli")
    );
    assert_eq!(
        request.headers.get("accept").map(String::as_str),
        Some("application/json")
    );
    assert!(!request.headers.contains_key("x-api-key"));
}

#[test]
fn oauth_path_prepends_claude_code_system_prompt_in_body() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("sk-ant-oat-secret");

    stream(&transport, &model, &context, &options, 0);

    let body = body_json(&only_request(&transport));
    assert_eq!(
        body["system"][0]["text"],
        json!("You are Claude Code, Anthropic's official CLI for Claude.")
    );
}

#[test]
fn api_key_path_emits_x_api_key_and_no_authorization() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    assert_eq!(
        request.headers.get("x-api-key").map(String::as_str),
        Some("test-key")
    );
    assert!(!request.headers.contains_key("authorization"));
    // No fine-grained/interleaved betas for an adaptive model with no tools.
    assert!(!request.headers.contains_key("anthropic-beta"));
}

#[test]
fn request_carries_sdk_injected_default_headers() {
    // pi delegates content-type + anthropic-version to the Anthropic SDK; the
    // raw transport must supply them or the API 400s ("anthropic-version:
    // header is required"). Both auth modes get them; accept stays as
    // createClient sets it (application/json).
    for api_key in ["test-key", "sk-ant-oat-secret"] {
        let transport = ScriptedTransport::new();
        transport.push_ok(hello_sse_body());

        let model = make_model(json!({}));
        let context = context_with_tools(vec![]);
        let options = api_key_options(api_key);

        stream(&transport, &model, &context, &options, 0);

        let request = only_request(&transport);
        assert_eq!(
            request.headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01"),
            "anthropic-version missing for key {api_key}"
        );
        assert_eq!(
            request.headers.get("content-type").map(String::as_str),
            Some("application/json"),
            "content-type missing for key {api_key}"
        );
        assert_eq!(
            request.headers.get("accept").map(String::as_str),
            Some("application/json"),
            "accept changed for key {api_key}"
        );
    }
}

#[test]
fn caller_supplied_headers_win_over_sdk_defaults() {
    // The SDK-injected defaults are low precedence: a caller-supplied
    // content-type/anthropic-version (via options.headers) is preserved.
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let mut headers = std::collections::BTreeMap::new();
    headers.insert("content-type".to_string(), "application/custom".to_string());
    headers.insert("anthropic-version".to_string(), "2099-01-01".to_string());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = AnthropicOptions {
        api_key: Some("test-key".to_string()),
        cache_retention: Some(CacheRetention::None),
        headers: Some(headers),
        ..Default::default()
    };

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/custom")
    );
    assert_eq!(
        request.headers.get("anthropic-version").map(String::as_str),
        Some("2099-01-01")
    );
}

#[test]
fn request_targets_the_v1_messages_endpoint() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({ "baseUrl": "https://api.anthropic.test/" }));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://api.anthropic.test/v1/messages");
}

#[test]
fn session_affinity_header_gated_on_compat_and_retention() {
    // sendSessionAffinityHeaders + a session id + caching enabled -> header set.
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());
    let model = make_model(json!({ "compat": { "sendSessionAffinityHeaders": true } }));
    let context = context_with_tools(vec![]);
    let options = AnthropicOptions {
        api_key: Some("test-key".to_string()),
        session_id: Some("sess-123".to_string()),
        cache_retention: Some(CacheRetention::Short),
        ..Default::default()
    };
    stream(&transport, &model, &context, &options, 0);
    assert_eq!(
        only_request(&transport)
            .headers
            .get("x-session-affinity")
            .map(String::as_str),
        Some("sess-123")
    );

    // cacheRetention "none" gates the session id out entirely.
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());
    let options = AnthropicOptions {
        api_key: Some("test-key".to_string()),
        session_id: Some("sess-123".to_string()),
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    };
    stream(&transport, &model, &context, &options, 0);
    assert!(!only_request(&transport)
        .headers
        .contains_key("x-session-affinity"));
}

// ---------------------------------------------------------------------------
// anthropic-eager-tool-input-compat.test.ts (headers + tool body)
// ---------------------------------------------------------------------------

#[test]
fn sends_per_tool_eager_input_streaming_by_default() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![tool("lookup")]);
    let options = api_key_options("test-key");

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    let body = body_json(&request);
    assert_eq!(first_tool(&body)["eager_input_streaming"], json!(true));
    assert!(!request.headers.contains_key("anthropic-beta"));
}

#[test]
fn uses_fine_grained_beta_when_eager_streaming_disabled() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({
        "compat": { "forceAdaptiveThinking": true, "supportsEagerToolInputStreaming": false }
    }));
    let context = context_with_tools(vec![tool("lookup")]);
    let options = api_key_options("test-key");

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    let body = body_json(&request);
    assert!(first_tool(&body).get("eager_input_streaming").is_none());
    assert_eq!(
        request.headers.get("anthropic-beta").map(String::as_str),
        Some("fine-grained-tool-streaming-2025-05-14")
    );
}

#[test]
fn no_fine_grained_beta_when_no_tools() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({
        "compat": { "forceAdaptiveThinking": true, "supportsEagerToolInputStreaming": false }
    }));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    stream(&transport, &model, &context, &options, 0);

    let request = only_request(&transport);
    let body = body_json(&request);
    assert!(body.get("tools").is_none());
    assert!(!request.headers.contains_key("anthropic-beta"));
}

// ---------------------------------------------------------------------------
// Interleaved-thinking beta composition (non-adaptive models)
// ---------------------------------------------------------------------------

#[test]
fn interleaved_beta_added_for_non_adaptive_model() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    // A non-adaptive model with tools + eager streaming off: both betas compose,
    // fine-grained first (createClient push order).
    let model = make_model(json!({
        "compat": { "forceAdaptiveThinking": false, "supportsEagerToolInputStreaming": false }
    }));
    let context = context_with_tools(vec![tool("lookup")]);
    let options = api_key_options("test-key");

    stream(&transport, &model, &context, &options, 0);

    assert_eq!(
        only_request(&transport)
            .headers
            .get("anthropic-beta")
            .map(String::as_str),
        Some("fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14")
    );
}

#[test]
fn interleaved_beta_suppressed_by_option() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({ "compat": { "forceAdaptiveThinking": false } }));
    let context = context_with_tools(vec![]);
    let options = AnthropicOptions {
        api_key: Some("test-key".to_string()),
        cache_retention: Some(CacheRetention::None),
        interleaved_thinking: Some(false),
        ..Default::default()
    };

    stream(&transport, &model, &context, &options, 0);

    assert!(!only_request(&transport)
        .headers
        .contains_key("anthropic-beta"));
}

// ---------------------------------------------------------------------------
// Body flow-through + parsed StreamResult
// ---------------------------------------------------------------------------

#[test]
fn build_params_body_flows_through_and_result_parses() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let result: StreamResult = stream(&transport, &model, &context, &options, 0);

    // Request body carries build_params output.
    let body = body_json(&only_request(&transport));
    assert_eq!(body["model"], json!("claude-opus-4-8"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["messages"][0]["role"], json!("user"));

    // Parsed result: start … done, single text block "Hello".
    assert!(matches!(
        result.events.first(),
        Some(AssistantMessageEvent::Start { .. })
    ));
    assert!(matches!(
        result.events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
    assert_eq!(result.message.stop_reason, StopReason::Stop);
    assert_eq!(result.message.content.len(), 1);
    assert_eq!(
        result.message.content[0],
        ContentBlock::Text {
            text: "Hello".to_string(),
            text_signature: None,
        }
    );
}

#[test]
fn truncated_stream_surfaces_message_stop_error() {
    let transport = ScriptedTransport::new();
    transport.push_ok(truncated_sse_body());

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let result = stream(&transport, &model, &context, &options, 0);

    assert_eq!(result.message.stop_reason, StopReason::Error);
    assert_eq!(
        result.message.error_message.as_deref(),
        Some("Anthropic stream ended before message_stop")
    );
    assert!(matches!(
        result.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn non_2xx_status_surfaces_error_result() {
    let transport = ScriptedTransport::new();
    transport.push_response(Ok(HttpResponse {
        status: 429,
        headers: Default::default(),
        body: String::new(),
    }));

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let result = stream(&transport, &model, &context, &options, 0);

    assert_eq!(result.message.stop_reason, StopReason::Error);
    assert_eq!(result.events.len(), 1);
    assert!(matches!(
        result.events[0],
        AssistantMessageEvent::Error { .. }
    ));
    // No body: mirror the SDK APIError "no body" marker, prefixed by status.
    assert_eq!(
        result.message.error_message.as_deref(),
        Some("429 status code (no body)")
    );
}

#[test]
fn non_2xx_status_surfaces_json_error_body_diagnostic() {
    // A 400 with the API's JSON error body (the `anthropic-version` failure the
    // header fix prevents): the terminal message must carry the body's
    // diagnostic text, not just the status, mirroring the SDK APIError shape
    // pi surfaces (anthropic-messages.ts:752).
    let transport = ScriptedTransport::new();
    transport.push_response(Ok(HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": "anthropic-version: header is required"
            }
        })
        .to_string(),
    }));

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let result = stream(&transport, &model, &context, &options, 0);

    assert_eq!(result.message.stop_reason, StopReason::Error);
    assert_eq!(
        result.message.error_message.as_deref(),
        Some("400 anthropic-version: header is required")
    );
}

#[test]
fn non_2xx_status_falls_back_to_raw_body_when_not_json() {
    // A non-JSON error body (e.g. an upstream gateway) is surfaced verbatim
    // after the status rather than discarded.
    let transport = ScriptedTransport::new();
    transport.push_response(Ok(HttpResponse {
        status: 502,
        headers: Default::default(),
        body: "upstream connect error".to_string(),
    }));

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let result = stream(&transport, &model, &context, &options, 0);

    assert_eq!(
        result.message.error_message.as_deref(),
        Some("502 upstream connect error")
    );
}

#[test]
fn missing_auth_surfaces_no_api_key_error() {
    let transport = ScriptedTransport::new();
    // No response queued: the driver must not reach the transport.
    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = AnthropicOptions::default();

    let result = stream(&transport, &model, &context, &options, 0);

    assert_eq!(result.message.stop_reason, StopReason::Error);
    assert_eq!(
        result.message.error_message.as_deref(),
        Some("No API key for provider: test-anthropic")
    );
    assert!(transport.requests().is_empty());
}

// ---------------------------------------------------------------------------
// streamSimple mapping
// ---------------------------------------------------------------------------

#[test]
fn stream_simple_without_reasoning_disables_thinking() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    // Non-adaptive model: thinking disabled emits {"type":"disabled"}.
    let model = make_model(json!({ "compat": { "forceAdaptiveThinking": false } }));
    let context = context_with_tools(vec![]);
    let options = SimpleStreamOptions {
        api_key: Some("test-key".to_string()),
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    };

    stream_simple(&transport, &model, &context, &options, 0);

    let body = body_json(&only_request(&transport));
    assert_eq!(body["thinking"], json!({ "type": "disabled" }));
}

#[test]
fn stream_simple_adaptive_reasoning_maps_to_effort() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    // Adaptive model + reasoning high -> adaptive thinking + effort output_config.
    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = SimpleStreamOptions {
        api_key: Some("test-key".to_string()),
        cache_retention: Some(CacheRetention::None),
        reasoning: Some(ThinkingLevel::High),
        ..Default::default()
    };

    stream_simple(&transport, &model, &context, &options, 0);

    let body = body_json(&only_request(&transport));
    assert_eq!(body["thinking"]["type"], json!("adaptive"));
    assert_eq!(body["output_config"], json!({ "effort": "high" }));
}

#[test]
fn stream_simple_budget_reasoning_sets_budget_tokens() {
    let transport = ScriptedTransport::new();
    transport.push_ok(hello_sse_body());

    // Non-adaptive model + reasoning medium -> budget-based thinking.
    let model = make_model(json!({ "compat": { "forceAdaptiveThinking": false } }));
    let context = context_with_tools(vec![]);
    let options = SimpleStreamOptions {
        api_key: Some("test-key".to_string()),
        cache_retention: Some(CacheRetention::None),
        reasoning: Some(ThinkingLevel::Medium),
        ..Default::default()
    };

    stream_simple(&transport, &model, &context, &options, 0);

    let body = body_json(&only_request(&transport));
    assert_eq!(body["thinking"]["type"], json!("enabled"));
    // Default medium budget is 8192, fits inside the model cap.
    assert_eq!(body["thinking"]["budget_tokens"], json!(8192));
}

// ---------------------------------------------------------------------------
// Incremental streaming: stream_streaming over the shared AssistantEventReader
// ---------------------------------------------------------------------------

/// Drain a finished reader's terminal message (`Ok` or `Err`).
fn reader_message(reader: &AssistantEventReader<'_>) -> crate::types::AssistantMessage {
    match reader.result() {
        Some(Ok(message)) | Some(Err(message)) => message.clone(),
        None => panic!("reader did not finalize"),
    }
}

#[test]
fn stream_streaming_matches_buffered_stream_byte_for_byte() {
    // Equivalence: over the default one-chunk `send_streaming` (ScriptedTransport
    // implements only `send`), the incremental path yields the EXACT same events
    // and final message as the buffered `stream()` -- proving the single decoder
    // is the one source of truth.
    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let buffered_transport = ScriptedTransport::new();
    buffered_transport.push_ok(hello_sse_body());
    let buffered = stream(&buffered_transport, &model, &context, &options, 0);

    let streaming_transport = ScriptedTransport::new();
    streaming_transport.push_ok(hello_sse_body());
    let mut reader = stream_streaming(&streaming_transport, &model, &context, &options, 0);
    let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();
    let message = reader_message(&reader);

    assert_eq!(events, buffered.events);
    assert_eq!(message, buffered.message);
    assert!(matches!(
        events.first(),
        Some(AssistantMessageEvent::Start { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
}

#[test]
fn stream_streaming_non_2xx_yields_single_error_with_body_diagnostic() {
    // A non-2xx create carries the API's JSON error body through
    // `format_api_error`; the streaming path surfaces it as the single terminal
    // error, with no `start` and no panic -- mirroring the buffered error path.
    let transport = ScriptedTransport::new();
    transport.push_response(Ok(HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": "anthropic-version: header is required"
            }
        })
        .to_string(),
    }));

    let model = make_model(json!({}));
    let context = context_with_tools(vec![]);
    let options = api_key_options("test-key");

    let mut reader = stream_streaming(&transport, &model, &context, &options, 0);
    let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));
    let message = reader_message(&reader);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("400 anthropic-version: header is required")
    );
}

/// Native-http, loopback-server timing: a delayed chunked SSE server serving the
/// `hello_sse_body()` frames one write at a time proves events arrive
/// incrementally through the anthropic driver (non-zero inter-event spread),
/// versus the buffered path's all-at-once delivery.
#[cfg(feature = "native-http")]
mod streaming_native {
    use super::*;
    use crate::seams::ReqwestTransport;
    use crate::types::ContentBlock;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Split an SSE body into frame-aligned pieces (each ends with the blank-line
    /// delimiter, the trailing frame without one) so each chunked write carries a
    /// complete frame.
    fn split_sse_frames(body: &str) -> Vec<String> {
        let mut pieces = Vec::new();
        let mut rest = body;
        while let Some(idx) = rest.find("\n\n") {
            pieces.push(rest[..idx + 2].to_string());
            rest = &rest[idx + 2..];
        }
        if !rest.is_empty() {
            pieces.push(rest.to_string());
        }
        pieces
    }

    /// Read past the request head so the client can proceed to read the response.
    fn drain_request_head(stream: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
    }

    /// Spawn a one-shot loopback server that writes the `hello_sse_body()` frames
    /// as separate `Transfer-Encoding: chunked` writes, sleeping `delay` between
    /// them so the frames arrive with real inter-write timing.
    fn spawn_hello_sse_server(delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let pieces = split_sse_frames(&hello_sse_body());
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                drain_request_head(&mut stream);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .unwrap();
                for piece in pieces {
                    let chunk = format!("{:X}\r\n{piece}\r\n", piece.len());
                    stream.write_all(chunk.as_bytes()).unwrap();
                    stream.flush().unwrap();
                    thread::sleep(delay);
                }
                stream.write_all(b"0\r\n\r\n").unwrap();
                stream.flush().unwrap();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn stream_streaming_delivers_events_incrementally_over_native_http() {
        let delay = Duration::from_millis(15);
        let url = spawn_hello_sse_server(delay);

        let transport = ReqwestTransport::builder().no_proxy().build();
        let model = make_model(json!({ "baseUrl": url }));
        let context = context_with_tools(vec![]);
        let options = api_key_options("test-key");

        let mut reader = stream_streaming(&transport, &model, &context, &options, 0);
        let start = Instant::now();
        let mut stamped: Vec<(Duration, AssistantMessageEvent)> = Vec::new();
        for event in reader.by_ref() {
            stamped.push((start.elapsed(), event));
        }

        let events: Vec<AssistantMessageEvent> = stamped.iter().map(|(_, e)| e.clone()).collect();

        // Same logical hello exchange as the buffered path.
        assert!(matches!(
            events.first(),
            Some(AssistantMessageEvent::Start { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Done { .. })
        ));
        let text_deltas = events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::TextDelta { .. }))
            .count();
        assert_eq!(text_deltas, 1);

        // Incrementality: the events span real wall-clock time (they did NOT all
        // arrive at once), proving the driver pulls chunk-by-chunk. Bounded well
        // below the ~5*delay total so CI can never hang or flake.
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread >= {delay:?}, got {spread:?}",
        );

        let message = reader_message(&reader);
        assert_eq!(message.stop_reason, StopReason::Stop);
        assert_eq!(
            message.content,
            vec![ContentBlock::Text {
                text: "Hello".to_string(),
                text_signature: None,
            }]
        );
    }
}
