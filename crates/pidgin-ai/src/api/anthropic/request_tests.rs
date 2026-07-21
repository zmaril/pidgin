// straitjacket-allow-file:duplication — these tests transcribe pi's Anthropic
// request-shaping fixtures (`anthropic-temperature-compat`,
// `anthropic-force-adaptive-thinking`, `anthropic-eager-tool-input-compat`,
// `anthropic-empty-thinking-signature-compat`, `anthropic-cache-write-1h-cost`).
// The model/context/payload literals are walls of near-identical JSON by design;
// the clone detector reads them as duplicates, but they are distinct,
// load-bearing wire fixtures kept faithful to pi's test cases.
//! Unit tests for Anthropic request shaping, porting the assertions from pi's
//! `packages/ai/test/anthropic-*.test.ts` fixture-driven suites.
//!
//! pi drives these through `streamSimple`/`stream` and captures the payload via
//! `onPayload`. Phase 1 ports only the request-shaping surface, so these tests
//! call [`build_params`] directly with [`AnthropicOptions`] set to the values
//! `streamSimple`/`stream` would have produced (noted per case), and assert on
//! the resulting JSON body.

use serde_json::{json, Value};

use super::request::{build_params, AnthropicOptions, ToolChoice};
use super::thinking::{map_thinking_level_to_effort, AnthropicEffort, AnthropicThinkingDisplay};
use crate::types::{AnthropicMessagesCompat, CacheRetention, Context, Model, ThinkingLevel};

/// Build a `Model<AnthropicMessagesCompat>` from overrides, filling the
/// non-load-bearing fields with the neutral values pi's test models use.
fn make_model(overrides: Value) -> Model<AnthropicMessagesCompat> {
    let mut base = json!({
        "id": "claude-opus-4-8",
        "name": "Test Model",
        "api": "anthropic-messages",
        "provider": "test-anthropic",
        "baseUrl": "http://127.0.0.1:9",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200000,
        "maxTokens": 32000,
    });
    let base_obj = base.as_object_mut().unwrap();
    for (key, value) in overrides.as_object().unwrap() {
        base_obj.insert(key.clone(), value.clone());
    }
    serde_json::from_value(base).unwrap()
}

fn simple_context() -> Context {
    serde_json::from_value(json!({
        "messages": [{ "role": "user", "content": "Hello", "timestamp": 0 }],
    }))
    .unwrap()
}

fn temperature_of(params: &Value) -> Option<&Value> {
    params.get("temperature")
}

// ---------------------------------------------------------------------------
// anthropic-temperature-compat.test.ts
//
// pi drives `streamSimple(model, ctx, { temperature })` with no `reasoning`,
// which maps to `AnthropicOptions { thinkingEnabled: false, temperature }`.
// ---------------------------------------------------------------------------

fn temperature_options(temperature: f64) -> AnthropicOptions {
    AnthropicOptions {
        temperature: Some(temperature),
        thinking_enabled: Some(false),
        ..Default::default()
    }
}

#[test]
fn omits_temperature_for_opus_4_7() {
    // Opus 4.7 sets supportsTemperature: false in the catalog.
    let model = make_model(json!({ "id": "claude-opus-4-7", "provider": "anthropic",
        "compat": { "supportsTemperature": false } }));
    let params = build_params(&model, &simple_context(), false, &temperature_options(0.0));
    assert!(temperature_of(&params).is_none());
}

#[test]
fn omits_temperature_for_opus_4_8() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic",
        "compat": { "supportsTemperature": false } }));
    let params = build_params(&model, &simple_context(), false, &temperature_options(0.0));
    assert!(temperature_of(&params).is_none());
}

#[test]
fn omits_default_temperature_for_opus_4_7() {
    let model = make_model(json!({ "id": "claude-opus-4-7", "provider": "anthropic",
        "compat": { "supportsTemperature": false } }));
    let params = build_params(&model, &simple_context(), false, &temperature_options(1.0));
    assert!(temperature_of(&params).is_none());
}

#[test]
fn keeps_temperature_for_opus_4_6() {
    // Opus 4.6 supports temperature (no override needed).
    let model = make_model(json!({ "id": "claude-opus-4-6", "provider": "anthropic" }));
    let params = build_params(&model, &simple_context(), false, &temperature_options(0.0));
    assert_eq!(temperature_of(&params), Some(&json!(0.0)));
}

#[test]
fn keeps_temperature_for_sonnet_4_6() {
    let model = make_model(json!({ "id": "claude-sonnet-4-6", "provider": "anthropic" }));
    let params = build_params(&model, &simple_context(), false, &temperature_options(0.0));
    assert_eq!(temperature_of(&params), Some(&json!(0.0)));
}

#[test]
fn omits_temperature_for_custom_model_with_supports_temperature_disabled() {
    let model = make_model(
        json!({ "id": "vendor--claude-opus-4-7", "provider": "vendor-proxy",
        "compat": { "supportsTemperature": false } }),
    );
    let params = build_params(&model, &simple_context(), false, &temperature_options(0.0));
    assert!(temperature_of(&params).is_none());
}

// ---------------------------------------------------------------------------
// anthropic-force-adaptive-thinking.test.ts
//
// pi drives `streamSimple(model, ctx, { reasoning })`. With `reasoning` set and
// `forceAdaptiveThinking` true this maps to
// `{ thinkingEnabled: true, effort: mapThinkingLevelToEffort(model, reasoning) }`;
// otherwise to budget-based `{ thinkingEnabled: true, thinkingBudgetTokens }`.
// With no `reasoning` it maps to `{ thinkingEnabled: false }`.
// ---------------------------------------------------------------------------

#[test]
fn sends_legacy_thinking_payload_for_custom_model_ids_by_default() {
    let model =
        make_model(json!({ "id": "vendor--claude-opus-latest", "provider": "vendor-proxy" }));
    let options = AnthropicOptions {
        thinking_enabled: Some(true),
        thinking_budget_tokens: Some(8192),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(
        params.get("thinking").and_then(|t| t.get("type")),
        Some(&json!("enabled"))
    );
    assert!(params.get("output_config").is_none());
}

#[test]
fn sends_adaptive_thinking_payload_when_force_adaptive_thinking_is_true() {
    let model = make_model(
        json!({ "id": "vendor--claude-opus-latest", "provider": "vendor-proxy",
        "compat": { "forceAdaptiveThinking": true } }),
    );
    let effort = map_thinking_level_to_effort(&model, Some(ThinkingLevel::Medium));
    let options = AnthropicOptions {
        thinking_enabled: Some(true),
        effort: Some(effort),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(
        params.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(
        params.get("output_config"),
        Some(&json!({ "effort": "medium" }))
    );
}

#[test]
fn uses_adaptive_thinking_with_native_xhigh_effort_for_fable_5() {
    // Fable 5 maps xhigh -> "xhigh" in thinkingLevelMap.
    let model = make_model(json!({ "id": "claude-fable-5", "provider": "anthropic",
        "thinkingLevelMap": { "xhigh": "xhigh" },
        "compat": { "forceAdaptiveThinking": true } }));
    let effort = map_thinking_level_to_effort(&model, Some(ThinkingLevel::Xhigh));
    assert_eq!(effort, AnthropicEffort::Xhigh);
    let options = AnthropicOptions {
        thinking_enabled: Some(true),
        effort: Some(effort),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(
        params.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(
        params.get("output_config"),
        Some(&json!({ "effort": "xhigh" }))
    );
}

#[test]
fn uses_adaptive_thinking_effort_without_token_budget_for_kimi_coding() {
    // Kimi Coding models map their reasoning levels 1:1 in thinkingLevelMap.
    for (reasoning, expected) in [
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::Max, "max"),
    ] {
        let model = make_model(json!({ "id": "k3", "provider": "kimi-coding",
            "thinkingLevelMap": { "medium": "medium", "max": "max" },
            "compat": { "forceAdaptiveThinking": true } }));
        let effort = map_thinking_level_to_effort(&model, Some(reasoning));
        let options = AnthropicOptions {
            thinking_enabled: Some(true),
            effort: Some(effort),
            ..Default::default()
        };
        let params = build_params(&model, &simple_context(), false, &options);
        assert_eq!(
            params.get("thinking"),
            Some(&json!({ "type": "adaptive", "display": "summarized" }))
        );
        assert_eq!(
            params.get("output_config"),
            Some(&json!({ "effort": expected }))
        );
    }
}

#[test]
fn allows_built_in_adaptive_models_to_opt_out_with_force_adaptive_thinking_false() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic",
        "compat": { "forceAdaptiveThinking": false } }));
    let options = AnthropicOptions {
        thinking_enabled: Some(true),
        thinking_budget_tokens: Some(8192),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(
        params.get("thinking").and_then(|t| t.get("type")),
        Some(&json!("enabled"))
    );
    assert!(params.get("output_config").is_none());
}

#[test]
fn preserves_thinking_disabled_when_reasoning_off_regardless_of_override() {
    let model = make_model(
        json!({ "id": "vendor--claude-opus-latest", "provider": "vendor-proxy",
        "compat": { "forceAdaptiveThinking": true } }),
    );
    let options = AnthropicOptions {
        thinking_enabled: Some(false),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(params.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(params.get("output_config").is_none());
}

// ---------------------------------------------------------------------------
// anthropic-eager-tool-input-compat.test.ts (params side)
//
// pi drives `stream(model, ctx, { cacheRetention: "none" })`, so
// `AnthropicOptions { cacheRetention: none }` with thinking left unset.
// ---------------------------------------------------------------------------

fn lookup_tool() -> Value {
    json!({
        "name": "lookup",
        "description": "Look up a value",
        "parameters": {
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
        },
    })
}

fn tool_context(tools: Vec<Value>) -> Context {
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

fn eager_options() -> AnthropicOptions {
    AnthropicOptions {
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    }
}

fn first_tool(params: &Value) -> &Value {
    &params.get("tools").and_then(Value::as_array).unwrap()[0]
}

#[test]
fn sends_per_tool_eager_input_streaming_by_default() {
    let model = make_model(json!({ "compat": { "forceAdaptiveThinking": true } }));
    let params = build_params(
        &model,
        &tool_context(vec![lookup_tool()]),
        false,
        &eager_options(),
    );
    assert_eq!(
        first_tool(&params).get("eager_input_streaming"),
        Some(&json!(true))
    );
}

#[test]
fn omits_eager_input_streaming_when_disabled() {
    let model = make_model(json!({ "compat": {
        "forceAdaptiveThinking": true,
        "supportsEagerToolInputStreaming": false,
    } }));
    let params = build_params(
        &model,
        &tool_context(vec![lookup_tool()]),
        false,
        &eager_options(),
    );
    assert!(first_tool(&params).get("eager_input_streaming").is_none());
}

#[test]
fn omits_tools_when_there_are_none() {
    let model = make_model(json!({ "compat": {
        "forceAdaptiveThinking": true,
        "supportsEagerToolInputStreaming": false,
    } }));
    let params = build_params(&model, &tool_context(vec![]), false, &eager_options());
    assert!(params.get("tools").is_none());
}

#[test]
fn emits_expected_tool_input_schema() {
    let model = make_model(json!({ "compat": { "forceAdaptiveThinking": true } }));
    let params = build_params(
        &model,
        &tool_context(vec![lookup_tool()]),
        false,
        &eager_options(),
    );
    let tool = first_tool(&params);
    assert_eq!(tool.get("name"), Some(&json!("lookup")));
    assert_eq!(tool.get("description"), Some(&json!("Look up a value")));
    assert_eq!(
        tool.get("input_schema"),
        Some(&json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
        }))
    );
}

// ---------------------------------------------------------------------------
// anthropic-empty-thinking-signature-compat.test.ts
//
// pi drives `streamSimple(model, ctx, {})` (no reasoning) -> thinkingEnabled
// false. Asserts on the converted assistant message content.
// ---------------------------------------------------------------------------

fn empty_signature_model(allow_empty_signature: Option<bool>) -> Model<AnthropicMessagesCompat> {
    let mut overrides = json!({
        "id": "mimo-v2.5-pro",
        "name": "MiMo-V2.5-Pro",
        "provider": "xiaomi-token-plan-ams",
        "contextWindow": 1048576,
        "maxTokens": 1024,
    });
    if let Some(allow) = allow_empty_signature {
        overrides.as_object_mut().unwrap().insert(
            "compat".to_string(),
            json!({ "allowEmptySignature": allow }),
        );
    }
    make_model(overrides)
}

fn empty_signature_context(thinking_signature: &str, thinking: &str) -> Context {
    serde_json::from_value(json!({
        "messages": [
            { "role": "user", "content": "first", "timestamp": 0 },
            {
                "role": "assistant",
                "content": [{
                    "type": "thinking",
                    "thinking": thinking,
                    "thinkingSignature": thinking_signature,
                }],
                "provider": "xiaomi-token-plan-ams",
                "api": "anthropic-messages",
                "model": "mimo-v2.5-pro",
                "timestamp": 0,
                "usage": {
                    "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                    "totalTokens": 0,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
                },
                "stopReason": "stop",
            },
            { "role": "user", "content": "second", "timestamp": 0 },
        ],
    }))
    .unwrap()
}

fn assistant_content(params: &Value) -> Value {
    params
        .get("messages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .and_then(|m| m.get("content"))
        .cloned()
        .unwrap()
}

fn no_reasoning_options() -> AnthropicOptions {
    AnthropicOptions {
        thinking_enabled: Some(false),
        ..Default::default()
    }
}

#[test]
fn converts_empty_signature_thinking_to_text_by_default() {
    let model = empty_signature_model(None);
    let params = build_params(
        &model,
        &empty_signature_context("", "internal reasoning"),
        false,
        &no_reasoning_options(),
    );
    assert_eq!(
        assistant_content(&params),
        json!([{ "type": "text", "text": "internal reasoning" }])
    );
}

#[test]
fn preserves_empty_thinking_text_when_signature_present() {
    let model = empty_signature_model(None);
    let params = build_params(
        &model,
        &empty_signature_context("signed-thinking", ""),
        false,
        &no_reasoning_options(),
    );
    assert_eq!(
        assistant_content(&params),
        json!([{ "type": "thinking", "thinking": "", "signature": "signed-thinking" }])
    );
}

#[test]
fn preserves_empty_signature_thinking_when_allow_empty_signature_enabled() {
    let model = empty_signature_model(Some(true));
    let params = build_params(
        &model,
        &empty_signature_context(" ", "internal reasoning"),
        false,
        &no_reasoning_options(),
    );
    assert_eq!(
        assistant_content(&params),
        json!([{ "type": "thinking", "thinking": "internal reasoning", "signature": "" }])
    );
}

// ---------------------------------------------------------------------------
// anthropic-cache-write-1h-cost.test.ts (params side: ttl:"1h" emission)
// ---------------------------------------------------------------------------

fn cache_context() -> Context {
    serde_json::from_value(json!({
        "systemPrompt": "You are helpful.",
        "messages": [{ "role": "user", "content": "hi", "timestamp": 0 }],
    }))
    .unwrap()
}

#[test]
fn get_cache_control_emits_ttl_1h_for_long_retention() {
    use super::cache::get_cache_control;
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic" }));
    let (retention, cache_control) = get_cache_control(&model, Some(CacheRetention::Long), None);
    assert_eq!(retention, CacheRetention::Long);
    assert_eq!(
        cache_control,
        Some(json!({ "type": "ephemeral", "ttl": "1h" }))
    );
}

#[test]
fn get_cache_control_omits_ttl_when_long_retention_unsupported() {
    use super::cache::get_cache_control;
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic",
        "compat": { "supportsLongCacheRetention": false } }));
    let (_, cache_control) = get_cache_control(&model, Some(CacheRetention::Long), None);
    assert_eq!(cache_control, Some(json!({ "type": "ephemeral" })));
}

#[test]
fn get_cache_control_none_for_none_retention() {
    use super::cache::get_cache_control;
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic" }));
    let (retention, cache_control) = get_cache_control(&model, Some(CacheRetention::None), None);
    assert_eq!(retention, CacheRetention::None);
    assert!(cache_control.is_none());
}

#[test]
fn build_params_stamps_ttl_1h_on_system_and_last_user_for_long_retention() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic" }));
    let options = AnthropicOptions {
        cache_retention: Some(CacheRetention::Long),
        thinking_enabled: Some(false),
        ..Default::default()
    };
    let params = build_params(&model, &cache_context(), false, &options);
    let ttl_1h = json!({ "type": "ephemeral", "ttl": "1h" });

    let system = params.get("system").and_then(Value::as_array).unwrap();
    assert_eq!(system[0].get("cache_control"), Some(&ttl_1h));

    let messages = params.get("messages").and_then(Value::as_array).unwrap();
    let last = messages.last().unwrap();
    let last_block = last
        .get("content")
        .and_then(Value::as_array)
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(last_block.get("cache_control"), Some(&ttl_1h));
}

// ---------------------------------------------------------------------------
// Additional build_params shape checks (not from a single pi fixture, but
// exercising the OAuth system prepend, base fields, and tool_choice).
// ---------------------------------------------------------------------------

#[test]
fn oauth_prepends_claude_code_identity_system_block() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic" }));
    let context: Context = serde_json::from_value(json!({
        "systemPrompt": "Custom prompt.",
        "messages": [{ "role": "user", "content": "hi", "timestamp": 0 }],
    }))
    .unwrap();
    let options = AnthropicOptions {
        cache_retention: Some(CacheRetention::None),
        thinking_enabled: Some(false),
        ..Default::default()
    };
    let params = build_params(&model, &context, true, &options);
    let system = params.get("system").and_then(Value::as_array).unwrap();
    assert_eq!(
        system[0].get("text"),
        Some(&json!(
            "You are Claude Code, Anthropic's official CLI for Claude."
        ))
    );
    assert_eq!(system[1].get("text"), Some(&json!("Custom prompt.")));
    // cacheRetention none => no cache_control stamped.
    assert!(system[0].get("cache_control").is_none());
}

#[test]
fn base_fields_model_max_tokens_and_stream() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic" }));
    let options = AnthropicOptions {
        cache_retention: Some(CacheRetention::None),
        thinking_enabled: Some(false),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(params.get("model"), Some(&json!("claude-opus-4-8")));
    assert_eq!(params.get("max_tokens"), Some(&json!(32000)));
    assert_eq!(params.get("stream"), Some(&json!(true)));
}

#[test]
fn tool_choice_string_and_forced_tool_serialize() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic" }));
    let ctx = tool_context(vec![lookup_tool()]);

    let mut options = AnthropicOptions {
        cache_retention: Some(CacheRetention::None),
        thinking_enabled: Some(false),
        tool_choice: Some(ToolChoice::Any),
        ..Default::default()
    };
    let params = build_params(&model, &ctx, false, &options);
    assert_eq!(params.get("tool_choice"), Some(&json!({ "type": "any" })));

    options.tool_choice = Some(ToolChoice::Tool {
        name: "lookup".to_string(),
    });
    let params = build_params(&model, &ctx, false, &options);
    assert_eq!(
        params.get("tool_choice"),
        Some(&json!({ "type": "tool", "name": "lookup" }))
    );
}

#[test]
fn thinking_display_omitted_is_forwarded() {
    let model = make_model(json!({ "id": "claude-opus-4-8", "provider": "anthropic",
        "compat": { "forceAdaptiveThinking": true } }));
    let options = AnthropicOptions {
        thinking_enabled: Some(true),
        thinking_display: Some(AnthropicThinkingDisplay::Omitted),
        effort: Some(AnthropicEffort::High),
        ..Default::default()
    };
    let params = build_params(&model, &simple_context(), false, &options);
    assert_eq!(
        params.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "omitted" }))
    );
}
