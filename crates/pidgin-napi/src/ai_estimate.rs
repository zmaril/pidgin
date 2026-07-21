//! Node-API surface for context-token estimation + the simple-options
//! `maxTokens` context clamp.
//!
//! This exposes two ports to pi's `packages/ai` suites:
//!
//! * pi's `packages/ai/src/utils/estimate.ts` (ported bit-exactly in
//!   [`pidgin_ai::utils::estimate`]) — the pure heuristic token accountant
//!   (`calculateContextTokens`, `estimateTextTokens`,
//!   `estimateTextAndImageContentTokens`, `estimateMessageTokens`,
//!   `estimateContextTokens`). Every arithmetic decision runs in Rust: the
//!   `ceil(chars / 4)` character heuristic, the flat 4800-char image estimate,
//!   the last-applicable-assistant-usage anchoring, and the
//!   system-prompt/tools prefix accounting.
//! * pi's `packages/ai/src/api/simple-options.ts` `clampMaxTokensToContext`
//!   (ported in [`pidgin_ai::api::anthropic::simple_options`]) — the
//!   `contextWindow − estimateContextTokens − CONTEXT_SAFETY_TOKENS` clamp,
//!   floored at `MIN_MAX_TOKENS` and capped at the caller's `maxTokens`.
//!
//! # The seam: the whole `Context` crosses as a JSON envelope
//!
//! pi's estimators read plain, fully-serializable values (a `Usage`, a
//! `Message`, a `Context`) — no closures, streams, or live object identity — so
//! each shim marshals its argument honestly with `JSON.stringify` and this layer
//! deserializes the complete value back before estimating. `estimateContextTokens`
//! also accepts a bare `Message[]`; the shim wraps that as `{ messages }`, which
//! the Rust port treats identically (no system prompt, no tools → zero prefix
//! tokens), matching pi's `estimateMessages`-only path for an array argument.
//!
//! The `maxTokens` clamp is the ONLY piece of `buildBaseOptions` that crosses:
//! its non-serializable fields (`signal`, `onPayload`, `onResponse`, `transport`)
//! are live JS values that cannot cross the addon boundary, so the shim keeps the
//! whole options-object assembly in TS and routes only the numeric clamp — which
//! reads solely `model.contextWindow` in pi — through here. The dummy carrier
//! [`Model`] this builds exists only to reach the public window-keyed clamp; the
//! port reads nothing but its `context_window`, so the other fields are inert.

use napi_derive::napi;
use serde::Serialize;

use pidgin_ai::api::anthropic::simple_options::clamp_max_tokens_to_context;
use pidgin_ai::types::{
    AnthropicMessagesCompat, Context, Message, Model, ModelCost, Usage, UserContent,
};
use pidgin_ai::utils::estimate::{
    calculate_context_tokens, estimate_context_tokens, estimate_message_tokens,
    estimate_text_and_image_content_tokens, estimate_text_tokens, ContextUsageEstimate,
};

/// The wire shape of pi's `ContextUsageEstimate` (`estimate.ts:3`). Serialized
/// with camelCase keys; `lastUsageIndex` is emitted as `null` (not omitted) when
/// no usage block anchors the estimate, so `JSON.parse` reads back `null` and pi's
/// `toEqual({ lastUsageIndex: null })` matches exactly.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextUsageEstimateJs {
    tokens: u64,
    usage_tokens: u64,
    trailing_tokens: u64,
    last_usage_index: Option<u32>,
}

impl From<ContextUsageEstimate> for ContextUsageEstimateJs {
    fn from(estimate: ContextUsageEstimate) -> Self {
        Self {
            tokens: estimate.tokens,
            usage_tokens: estimate.usage_tokens,
            trailing_tokens: estimate.trailing_tokens,
            last_usage_index: estimate.last_usage_index.map(|index| index as u32),
        }
    }
}

/// pi's `calculateContextTokens` (`estimate.ts:17`). Takes a JSON-stringified
/// `Usage` and returns `totalTokens`, or the component sum when `totalTokens` is
/// zero (JS `||` falsy-zero fallback), computed in Rust.
#[napi(js_name = "calculateContextTokens")]
pub fn calculate_context_tokens_native(usage_json: String) -> napi::Result<u32> {
    let usage: Usage = serde_json::from_str(&usage_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid usage: {err}")))?;
    Ok(calculate_context_tokens(&usage) as u32)
}

/// pi's `estimateTextTokens` (`estimate.ts:37`): `ceil(len / 4)` over the text's
/// UTF-16 code units.
#[napi(js_name = "estimateTextTokens")]
pub fn estimate_text_tokens_native(text: String) -> u32 {
    estimate_text_tokens(&text) as u32
}

/// pi's `estimateTextAndImageContentTokens` (`estimate.ts:41`). Takes a
/// JSON-stringified `string | Array<TextContent | ImageContent>`; text blocks
/// count their length, every other block counts as a flat 4800-char image, then
/// `ceil(chars / 4)` — all in Rust.
#[napi(js_name = "estimateTextAndImageContentTokens")]
pub fn estimate_text_and_image_content_tokens_native(content_json: String) -> napi::Result<u32> {
    let content: UserContent = serde_json::from_str(&content_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid content: {err}")))?;
    Ok(estimate_text_and_image_content_tokens(&content) as u32)
}

/// pi's `estimateMessageTokens` (`estimate.ts:45`). Takes a JSON-stringified
/// `Message`; the per-role/per-block character accounting runs in Rust.
#[napi(js_name = "estimateMessageTokens")]
pub fn estimate_message_tokens_native(message_json: String) -> napi::Result<u32> {
    let message: Message = serde_json::from_str(&message_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid message: {err}")))?;
    Ok(estimate_message_tokens(&message) as u32)
}

/// pi's `estimateContextTokens` (`estimate.ts:114`). Takes a JSON-stringified
/// `Context` (the shim wraps a bare `Message[]` as `{ messages }`) and returns
/// the `ContextUsageEstimate` as JSON. The usage anchoring, added-tool
/// accounting, and system-prompt/tools prefix all run in Rust.
#[napi(js_name = "estimateContextTokens")]
pub fn estimate_context_tokens_native(context_json: String) -> napi::Result<String> {
    let context: Context = serde_json::from_str(&context_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid context: {err}")))?;
    let estimate = ContextUsageEstimateJs::from(estimate_context_tokens(&context));
    serde_json::to_string(&estimate)
        .map_err(|err| napi::Error::from_reason(format!("serialize estimate: {err}")))
}

/// pi's `clampMaxTokensToContext` (`simple-options.ts:15`). `context_window` is
/// the model's window (pi reads only `model.contextWindow`); `context_json` is a
/// JSON-stringified `Context`; `max_tokens` is the caller's cap. The
/// `window − estimateContextTokens − CONTEXT_SAFETY_TOKENS` arithmetic, its
/// `MIN_MAX_TOKENS` floor, and the zero-window short-circuit all run in Rust.
///
/// The [`Model`] built here is an inert carrier for the window: the ported clamp
/// reads nothing but `context_window`, so every other field is a placeholder.
#[napi(js_name = "clampMaxTokensToContext")]
pub fn clamp_max_tokens_to_context_native(
    context_window: u32,
    context_json: String,
    max_tokens: u32,
) -> napi::Result<u32> {
    let context: Context = serde_json::from_str(&context_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid context: {err}")))?;
    let model = Model::<AnthropicMessagesCompat> {
        id: String::new(),
        name: String::new(),
        api: String::new(),
        provider: String::new(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: Vec::new(),
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: u64::from(context_window),
        max_tokens: 0,
        headers: None,
        compat: None,
    };
    Ok(clamp_max_tokens_to_context(&model, &context, u64::from(max_tokens)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// pi's `createAssistant(timestamp, totalTokens)` (`context-estimate.test.ts`):
    /// a `"kept"`-text assistant message carrying a `totalTokens` usage block.
    /// Shared by both context fixtures so the message shape lives in one place.
    fn assistant_json(timestamp: i64, total_tokens: u64) -> Value {
        json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "kept" }],
            "api": "openai-responses",
            "provider": "openai",
            "model": "test-model",
            "usage": {
                "input": total_tokens, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                "totalTokens": total_tokens,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": timestamp
        })
    }

    /// The exact `Context` pi's `context-estimate.test.ts` case 1 builds: a
    /// stale assistant usage (ts 100) stranded behind a user message at ts 200,
    /// so no usage anchors the estimate.
    fn stale_usage_context() -> String {
        json!({
            "systemPrompt": "system",
            "messages": [
                { "role": "user", "content": "summary", "timestamp": 200 },
                assistant_json(100, 9500),
                { "role": "user", "content": "x".repeat(4000), "timestamp": 300 }
            ]
        })
        .to_string()
    }

    #[test]
    fn estimate_context_tokens_matches_case_one() {
        let json = estimate_context_tokens_native(stale_usage_context()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["tokens"], 1_005);
        assert_eq!(value["usageTokens"], 0);
        assert_eq!(value["trailingTokens"], 1_005);
        // Emitted as JSON `null`, not omitted, so pi's `toEqual` matches.
        assert!(value["lastUsageIndex"].is_null());
    }

    #[test]
    fn clamp_matches_case_one_max_tokens() {
        // model.contextWindow = 10_000, model.maxTokens = 8_000, estimate = 1_005,
        // CONTEXT_SAFETY_TOKENS = 4_096 → available = 4_899, min(8_000, 4_899).
        let clamped =
            clamp_max_tokens_to_context_native(10_000, stale_usage_context(), 8_000).unwrap();
        assert_eq!(clamped, 4_899);
    }

    #[test]
    fn estimate_context_tokens_matches_case_two() {
        let context = json!({
            "messages": [
                { "role": "user", "content": "summary", "timestamp": 200 },
                assistant_json(100, 9500),
                { "role": "user", "content": "new prompt", "timestamp": 300 },
                assistant_json(400, 2000),
                { "role": "user", "content": "tail", "timestamp": 500 }
            ]
        })
        .to_string();
        let out = estimate_context_tokens_native(context).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["tokens"], 2_001);
        assert_eq!(value["usageTokens"], 2_000);
        assert_eq!(value["trailingTokens"], 1);
        assert_eq!(value["lastUsageIndex"], 3);
    }

    #[test]
    fn estimate_text_tokens_ceils_by_four() {
        assert_eq!(estimate_text_tokens_native("abc".to_string()), 1);
        assert_eq!(estimate_text_tokens_native("abcde".to_string()), 2);
    }

    #[test]
    fn calculate_context_tokens_falls_back_to_components() {
        let usage = serde_json::json!({
            "input": 10, "output": 5, "cacheRead": 3, "cacheWrite": 2,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        })
        .to_string();
        assert_eq!(calculate_context_tokens_native(usage).unwrap(), 20);
    }
}
