// straitjacket-allow-file:duplication — the options DTO mirrors pi's
// `AnthropicOptions`/`StreamOptions` field-for-field (`anthropic-messages.ts:199`,
// `types.ts:113`) so the JSON boundary accepts exactly what pi's `stream()`
// receives. The per-field option scaffolding reads as duplicative of
// `request::AnthropicOptions` by design: one is the wire (deserialize) shape, the
// other the internal builder input.
//! JSON-boundary entry point for Anthropic request shaping.
//!
//! This mirrors [`super::super::anthropic::parse_sse_stream_to_json`]: the napi
//! shim hands us JSON strings across the FFI boundary and receives a JSON string
//! back. [`build_params_from_json`] deserializes pi's own `getModel('anthropic',
//! id)` output (a [`Model<AnthropicMessagesCompat>`]), a [`Context`], and the
//! `AnthropicOptions` the shim assembled, calls [`build_params`], and serializes
//! the resulting `MessageCreateParamsStreaming` body back to JSON.
//!
//! The model deserializes from pi's `getModel` JSON verbatim: pi's TS `Model`
//! interface (`types.ts:706`) is camelCase and [`Model`] is
//! `#[serde(rename_all = "camelCase")]` with no `deny_unknown_fields`, so the
//! shapes match field-for-field (see the `deserialize_real_pi_*` tests).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::types::{AnthropicMessagesCompat, CacheRetention, Context, Model};

use super::request::{build_params, AnthropicOptions, ToolChoice};
use super::thinking::{AnthropicEffort, AnthropicThinkingDisplay};

/// Wire form of pi's `AnthropicOptions extends StreamOptions`
/// (`anthropic-messages.ts:199`, `types.ts:113`): the subset the shim forwards
/// as JSON. Every field is optional and camelCase, matching pi's object; unknown
/// fields (transport/callback/timeout knobs `build_params` never reads) are
/// ignored rather than rejected, exactly as pi's `stream()` ignores options it
/// does not consume.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AnthropicOptionsWire {
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    cache_retention: Option<CacheRetention>,
    session_id: Option<String>,
    env: Option<BTreeMap<String, String>>,
    metadata: Option<Map<String, Value>>,
    thinking_enabled: Option<bool>,
    thinking_budget_tokens: Option<u64>,
    effort: Option<AnthropicEffort>,
    thinking_display: Option<AnthropicThinkingDisplay>,
    tool_choice: Option<ToolChoiceWire>,
    api_key: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    interleaved_thinking: Option<bool>,
}

/// Wire form of pi's `toolChoice` (`anthropic-messages.ts:252`): either a bare
/// string (`"auto" | "any" | "none"`) or `{ "type": "tool", "name": ... }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ToolChoiceWire {
    Named(String),
    Tool { r#type: String, name: String },
}

impl ToolChoiceWire {
    fn into_tool_choice(self) -> Result<ToolChoice, String> {
        match self {
            ToolChoiceWire::Named(s) => match s.as_str() {
                "auto" => Ok(ToolChoice::Auto),
                "any" => Ok(ToolChoice::Any),
                "none" => Ok(ToolChoice::None),
                other => Err(format!("invalid tool_choice: {other:?}")),
            },
            ToolChoiceWire::Tool { r#type, name } => {
                if r#type == "tool" {
                    Ok(ToolChoice::Tool { name })
                } else {
                    Err(format!("invalid tool_choice type: {:?}", r#type))
                }
            }
        }
    }
}

impl AnthropicOptionsWire {
    fn into_options(self) -> Result<AnthropicOptions, String> {
        let tool_choice = match self.tool_choice {
            Some(tc) => Some(tc.into_tool_choice()?),
            None => None,
        };
        Ok(AnthropicOptions {
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            cache_retention: self.cache_retention,
            session_id: self.session_id,
            env: self.env,
            metadata: self.metadata,
            thinking_enabled: self.thinking_enabled,
            thinking_budget_tokens: self.thinking_budget_tokens,
            effort: self.effort,
            thinking_display: self.thinking_display,
            tool_choice,
            api_key: self.api_key,
            headers: self.headers,
            interleaved_thinking: self.interleaved_thinking,
        })
    }
}

/// Build the Anthropic streaming request body from JSON inputs and return it as
/// a JSON string.
///
/// This is the boundary entry the napi shim calls for the `build_params` flip:
/// `build_params(model_json, context_json, is_oauth, options_json) ->
/// request_body_json`. `model_json` is pi's own `getModel('anthropic', id)`
/// output (same source the shim feeds to
/// [`super::super::anthropic::parse_sse_stream_to_json`]); it deserializes into
/// [`Model<AnthropicMessagesCompat>`] as-is. `context_json` is a serialized
/// [`Context`]; `options_json` is a serialized [`AnthropicOptions`] wire object
/// (may be `"{}"` for defaults). The returned string is the exact
/// `MessageCreateParamsStreaming` body [`build_params`] produces.
pub fn build_params_from_json(
    model_json: &str,
    context_json: &str,
    is_oauth: bool,
    options_json: &str,
) -> Result<String, String> {
    let model: Model<AnthropicMessagesCompat> =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let context: Context =
        serde_json::from_str(context_json).map_err(|e| format!("invalid context json: {e}"))?;
    let options_wire: AnthropicOptionsWire =
        serde_json::from_str(options_json).map_err(|e| format!("invalid options json: {e}"))?;
    let options = options_wire.into_options()?;
    let body = build_params(&model, &context, is_oauth, &options);
    serde_json::to_string(&body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real pi `getModel('anthropic', id)` output, captured verbatim from the
    // vendored catalog snapshot (`crates/pidgin-model-catalog/data/providers/`),
    // which is a byte-faithful copy of pi's generated catalog at the pinned
    // submodule commit. These are the exact JSON shapes the napi shim forwards.

    const OPUS_4_8: &str = r#"{
        "id": "claude-opus-4-8",
        "name": "Claude Opus 4.8",
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": "https://api.anthropic.com",
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 5, "output": 25, "cacheRead": 0.5, "cacheWrite": 6.25 },
        "contextWindow": 1000000,
        "maxTokens": 128000,
        "thinkingLevelMap": { "xhigh": "xhigh", "max": "max" },
        "compat": { "forceAdaptiveThinking": true, "supportsTemperature": false }
    }"#;

    const HAIKU_4_5: &str = r#"{
        "id": "claude-haiku-4-5",
        "name": "Claude Haiku 4.5 (latest)",
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": "https://api.anthropic.com",
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 1, "output": 5, "cacheRead": 0.1, "cacheWrite": 1.25 },
        "contextWindow": 200000,
        "maxTokens": 64000
    }"#;

    const FABLE_5: &str = r#"{
        "id": "claude-fable-5",
        "name": "Claude Fable 5",
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": "https://api.anthropic.com",
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 10, "output": 50, "cacheRead": 1, "cacheWrite": 12.5 },
        "contextWindow": 1000000,
        "maxTokens": 128000,
        "thinkingLevelMap": { "off": null, "xhigh": "xhigh", "max": "max" },
        "compat": { "forceAdaptiveThinking": true }
    }"#;

    // Note: `headers` and `compat` appear before `reasoning` here — pi does not
    // fix field order, and serde is order-independent, so this must round-trip.
    const KIMI_K2P7: &str = r#"{
        "id": "k2p7",
        "name": "Kimi K2.7 Code",
        "api": "anthropic-messages",
        "provider": "kimi-coding",
        "baseUrl": "https://api.kimi.com/coding",
        "headers": { "User-Agent": "KimiCLI/1.5" },
        "compat": { "forceAdaptiveThinking": true },
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 0.95, "output": 4, "cacheRead": 0.19, "cacheWrite": 0 },
        "contextWindow": 262144,
        "maxTokens": 32768
    }"#;

    fn parse(model_json: &str) -> Model<AnthropicMessagesCompat> {
        serde_json::from_str(model_json).expect("real pi getModel JSON must deserialize as-is")
    }

    // ---- Step 3: does `Model<AnthropicMessagesCompat>` deserialize from pi's
    // real getModel JSON as-is? ----

    #[test]
    fn deserialize_real_pi_opus_4_8_as_is() {
        let m = parse(OPUS_4_8);
        assert_eq!(m.id, "claude-opus-4-8");
        assert_eq!(m.api, "anthropic-messages");
        assert_eq!(m.provider, "anthropic");
        assert_eq!(m.base_url, "https://api.anthropic.com");
        assert!(m.reasoning);
        assert_eq!(m.context_window, 1_000_000);
        assert_eq!(m.max_tokens, 128_000);
        assert_eq!(m.cost.cache_write, 6.25);
        // compat blob maps field-for-field.
        let compat = m.compat.as_ref().expect("compat present");
        assert_eq!(compat.force_adaptive_thinking, Some(true));
        assert_eq!(compat.supports_temperature, Some(false));
        // thinkingLevelMap keys deserialize via ModelThinkingLevel (lowercase).
        assert!(m.thinking_level_map.is_some());
    }

    #[test]
    fn deserialize_real_pi_haiku_4_5_no_compat_no_thinking_map() {
        let m = parse(HAIKU_4_5);
        assert_eq!(m.id, "claude-haiku-4-5");
        assert_eq!(m.max_tokens, 64_000);
        // Optional fields absent in the JSON stay None (no default/aliases needed).
        assert!(m.compat.is_none());
        assert!(m.thinking_level_map.is_none());
        assert!(m.headers.is_none());
    }

    #[test]
    fn deserialize_real_pi_fable_5_with_null_thinking_level() {
        let m = parse(FABLE_5);
        assert_eq!(m.id, "claude-fable-5");
        // `"off": null` marks a level unsupported — Option<String> == None.
        let map = m.thinking_level_map.as_ref().expect("map present");
        assert_eq!(map.get(&crate::types::ModelThinkingLevel::Off), Some(&None));
        assert_eq!(
            map.get(&crate::types::ModelThinkingLevel::Xhigh),
            Some(&Some("xhigh".to_string()))
        );
        assert_eq!(
            m.compat.as_ref().unwrap().force_adaptive_thinking,
            Some(true)
        );
    }

    #[test]
    fn deserialize_real_pi_kimi_k2p7_field_order_and_headers() {
        let m = parse(KIMI_K2P7);
        assert_eq!(m.id, "k2p7");
        assert_eq!(m.provider, "kimi-coding");
        // headers survive despite appearing before reasoning in the JSON.
        let headers = m.headers.as_ref().expect("headers present");
        assert_eq!(headers.get("User-Agent"), Some(&"KimiCLI/1.5".to_string()));
        assert_eq!(m.context_window, 262_144);
    }

    // ---- Step 3 end-to-end: feed the deserialized model into build_params ----

    #[test]
    fn build_params_from_real_opus_4_8_is_sane() {
        let context_json = r#"{ "messages": [
            { "role": "user", "content": "Hello", "timestamp": 0 }
        ] }"#;
        let options_json = r#"{ "cacheRetention": "none", "thinkingEnabled": false }"#;
        let out = build_params_from_json(OPUS_4_8, context_json, false, options_json)
            .expect("build_params_from_json succeeds");
        let body: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(body.get("model"), Some(&Value::from("claude-opus-4-8")));
        assert_eq!(body.get("max_tokens"), Some(&Value::from(128_000)));
        assert_eq!(body.get("stream"), Some(&Value::from(true)));
        // Opus 4.8 has supportsTemperature=false, so temperature is suppressed
        // even though thinking is off (matches request_tests).
        assert!(body.get("temperature").is_none());
    }

    // ---- Step 4: JSON-boundary entry contract ----

    #[test]
    fn build_params_from_json_forwards_oauth_and_tool_choice() {
        let context_json = r#"{ "messages": [
            { "role": "user", "content": "Hi", "timestamp": 0 }
        ], "tools": [ { "name": "lookup", "description": "d", "parameters": {} } ] }"#;
        // toolChoice as the bare string form pi accepts.
        let options_json =
            r#"{ "cacheRetention": "none", "thinkingEnabled": false, "toolChoice": "any" }"#;
        let out = build_params_from_json(HAIKU_4_5, context_json, false, options_json).unwrap();
        let body: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            body.get("tool_choice"),
            Some(&serde_json::json!({ "type": "any" }))
        );
    }

    #[test]
    fn build_params_from_json_tool_choice_forced_object() {
        let context_json = r#"{ "messages": [
            { "role": "user", "content": "Hi", "timestamp": 0 }
        ], "tools": [ { "name": "lookup", "description": "d", "parameters": {} } ] }"#;
        let options_json = r#"{ "cacheRetention": "none", "thinkingEnabled": false, "toolChoice": { "type": "tool", "name": "lookup" } }"#;
        let out = build_params_from_json(HAIKU_4_5, context_json, false, options_json).unwrap();
        let body: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            body.get("tool_choice"),
            Some(&serde_json::json!({ "type": "tool", "name": "lookup" }))
        );
    }

    #[test]
    fn build_params_from_json_empty_options_defaults() {
        let context_json = r#"{ "messages": [
            { "role": "user", "content": "Hello", "timestamp": 0 }
        ] }"#;
        let out = build_params_from_json(HAIKU_4_5, context_json, false, "{}").unwrap();
        let body: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(body.get("model"), Some(&Value::from("claude-haiku-4-5")));
        assert_eq!(body.get("stream"), Some(&Value::from(true)));
    }

    #[test]
    fn build_params_from_json_rejects_bad_model_json() {
        let err = build_params_from_json("{ not json", "{}", false, "{}").unwrap_err();
        assert!(err.starts_with("invalid model json:"), "{err}");
    }

    #[test]
    fn build_params_from_json_rejects_bad_tool_choice() {
        let context_json = r#"{ "messages": [
            { "role": "user", "content": "Hi", "timestamp": 0 }
        ] }"#;
        let options_json = r#"{ "toolChoice": "bogus" }"#;
        let err = build_params_from_json(HAIKU_4_5, context_json, false, options_json).unwrap_err();
        assert!(err.starts_with("invalid tool_choice:"), "{err}");
    }
}
