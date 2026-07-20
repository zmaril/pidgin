//! OpenAI **Responses API** request shaping, ported from pi-ai's
//! `packages/ai/src/api/openai-responses.ts` at pinned commit `3da591ab`.
//!
//! This is the request-side counterpart to
//! [`crate::api::openai_responses_shared`]'s stream processor: it resolves the
//! per-model compat defaults (`getCompat`), builds the `ResponseCreateParams`
//! payload (`buildParams`), and derives the session-affinity cache headers pi's
//! `createClient` sets. The HTTP transport, auth, and OpenAI-SDK client
//! construction of pi's `stream()` live outside this module; a napi shim can
//! drive [`build_params`] and [`crate::api::openai_responses_shared::process_responses_stream`]
//! directly through the JSON wrappers at the bottom of this file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, process_responses_stream,
    ResponsesStreamOptions, StreamOutcome,
};
use crate::types::{
    CacheRetention, Context, Modality, ModelCost, ModelThinkingLevel, OpenAIResponsesCompat,
    SessionAffinityFormat, ThinkingLevelMap,
};

/// pi's `OPENAI_TOOL_CALL_PROVIDERS` — the providers whose composite
/// `call_id|item_id` tool-call ids are normalized (`openai-responses.ts:28`).
pub const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];

/// OpenAI Responses rejects `max_output_tokens` below 16
/// (`openai-responses.ts:30`).
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// The minimum slice of a pi `Model` this driver needs for request shaping and
/// stream processing: identity + api/provider (for cross-model detection and
/// output-message construction), pricing (for cost), the reasoning knobs, the
/// input modalities, base URL, optional per-model headers, and the optional
/// Responses compat overrides. Deserialized leniently so extra pi model fields
/// are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAIResponsesModel {
    pub id: String,
    pub api: String,
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    pub cost: ModelCost,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default)]
    pub input: Vec<Modality>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub compat: Option<OpenAIResponsesCompat>,
}

/// OpenAI Responses request options (pi's `OpenAIResponsesOptions`), the subset
/// this Stage reads. All fields are optional and skip serialization when absent.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIResponsesOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Free-form `tool_choice` (string like `"required"` or an object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

/// The resolved compat settings (pi's `Required<OpenAIResponsesCompat>`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedResponsesCompat {
    pub supports_developer_role: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub supports_tool_search: bool,
}

fn detect_session_affinity_format(provider: &str, base_url: &str) -> SessionAffinityFormat {
    if provider == "openrouter" || base_url.contains("openrouter.ai") {
        SessionAffinityFormat::Openrouter
    } else {
        SessionAffinityFormat::Openai
    }
}

/// Resolve the Responses compat defaults, overlaying any per-model overrides
/// (pi's `getCompat`).
pub fn get_compat(model: &OpenAIResponsesModel) -> ResolvedResponsesCompat {
    let compat = model.compat.as_ref();
    ResolvedResponsesCompat {
        supports_developer_role: compat
            .and_then(|c| c.supports_developer_role)
            .unwrap_or(true),
        session_affinity_format: compat
            .and_then(|c| c.session_affinity_format)
            .unwrap_or_else(|| detect_session_affinity_format(&model.provider, &model.base_url)),
        supports_long_cache_retention: compat
            .and_then(|c| c.supports_long_cache_retention)
            .unwrap_or(true),
        supports_tool_search: compat.and_then(|c| c.supports_tool_search).unwrap_or(false),
    }
}

/// Resolve cache retention, defaulting to `short` (pi's `resolveCacheRetention`;
/// the `PI_CACHE_RETENTION` env fallback is out of scope at this boundary).
fn resolve_cache_retention(cache_retention: Option<CacheRetention>) -> CacheRetention {
    cache_retention.unwrap_or(CacheRetention::Short)
}

fn get_prompt_cache_retention(
    compat: &ResolvedResponsesCompat,
    cache_retention: CacheRetention,
) -> Option<&'static str> {
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        Some("24h")
    } else {
        None
    }
}

/// Derive the session-affinity cache headers pi's `createClient` sets for a
/// session id. Returns an empty map when there is no session id. Explicit
/// per-request headers (which override these) are the caller's responsibility.
pub fn session_affinity_headers(
    compat: &ResolvedResponsesCompat,
    session_id: Option<&str>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    let Some(session_id) = session_id else {
        return headers;
    };
    match compat.session_affinity_format {
        SessionAffinityFormat::Openrouter => {
            headers.insert("x-session-id".to_string(), session_id.to_string());
        }
        SessionAffinityFormat::Openai => {
            headers.insert("session_id".to_string(), session_id.to_string());
            headers.insert("x-client-request-id".to_string(), session_id.to_string());
        }
        SessionAffinityFormat::OpenaiNosession => {
            headers.insert("x-client-request-id".to_string(), session_id.to_string());
        }
    }
    headers
}

/// Build the OpenAI Responses `ResponseCreateParams` payload (pi's
/// `buildParams`).
pub fn build_params(
    model: &OpenAIResponsesModel,
    context: &Context,
    options: &OpenAIResponsesOptions,
) -> Value {
    let compat = get_compat(model);
    let messages = convert_responses_messages(
        model,
        &context.messages,
        context.system_prompt.as_deref(),
        &OPENAI_TOOL_CALL_PROVIDERS,
        true,
    );

    let cache_retention = resolve_cache_retention(options.cache_retention);

    let mut params = Map::new();
    params.insert("model".to_string(), Value::String(model.id.clone()));
    params.insert("input".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), Value::Bool(true));

    if cache_retention != CacheRetention::None {
        if let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref()) {
            params.insert("prompt_cache_key".to_string(), Value::String(key));
        }
    }
    if let Some(retention) = get_prompt_cache_retention(&compat, cache_retention) {
        params.insert(
            "prompt_cache_retention".to_string(),
            Value::String(retention.to_string()),
        );
    }
    params.insert("store".to_string(), Value::Bool(false));

    if let Some(max_tokens) = options.max_tokens {
        params.insert(
            "max_output_tokens".to_string(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }

    if let Some(temperature) = options.temperature {
        params.insert("temperature".to_string(), json!(temperature));
    }

    if let Some(service_tier) = &options.service_tier {
        params.insert(
            "service_tier".to_string(),
            Value::String(service_tier.clone()),
        );
    }

    // The deferred-tool split is not modelled at this boundary; all context
    // tools are sent immediately (pi's `toolPlacement.immediate`).
    let tools = context.tools.clone().unwrap_or_default();
    if !tools.is_empty() {
        params.insert(
            "tools".to_string(),
            Value::Array(convert_responses_tools(&tools, false, false)),
        );
    }

    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".to_string(), tool_choice.clone());
    }

    if model.reasoning {
        if options.reasoning_effort.is_some() || options.reasoning_summary.is_some() {
            let effort = match &options.reasoning_effort {
                Some(effort) => {
                    thinking_level_lookup(model, effort).unwrap_or_else(|| effort.clone())
                }
                None => "medium".to_string(),
            };
            let summary = options
                .reasoning_summary
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "auto".to_string());
            params.insert(
                "reasoning".to_string(),
                json!({ "effort": effort, "summary": summary }),
            );
            params.insert(
                "include".to_string(),
                json!(["reasoning.encrypted_content"]),
            );
        } else if model.provider != "github-copilot" && !thinking_off_is_null(model) {
            let effort = thinking_off_value(model).unwrap_or_else(|| "none".to_string());
            params.insert("reasoning".to_string(), json!({ "effort": effort }));
        }
        if model.provider == "xai" {
            params.insert(
                "include".to_string(),
                json!(["reasoning.encrypted_content"]),
            );
        }
    }

    Value::Object(params)
}

/// `model.thinkingLevelMap?.[effort]` — the provider-specific value for a
/// requested reasoning effort, when present and non-null.
fn thinking_level_lookup(model: &OpenAIResponsesModel, effort: &str) -> Option<String> {
    let level = parse_thinking_level(effort)?;
    model
        .thinking_level_map
        .as_ref()?
        .get(&level)
        .cloned()
        .flatten()
}

fn parse_thinking_level(effort: &str) -> Option<ModelThinkingLevel> {
    match effort {
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        "off" => Some(ModelThinkingLevel::Off),
        _ => None,
    }
}

/// True when `model.thinkingLevelMap.off` is present and explicitly `null`
/// (JS `=== null`), which suppresses the default `reasoning` block.
fn thinking_off_is_null(model: &OpenAIResponsesModel) -> bool {
    matches!(
        model
            .thinking_level_map
            .as_ref()
            .map(|m| m.get(&ModelThinkingLevel::Off)),
        Some(Some(None))
    )
}

/// `model.thinkingLevelMap?.off` when it is a concrete string value.
fn thinking_off_value(model: &OpenAIResponsesModel) -> Option<String> {
    model
        .thinking_level_map
        .as_ref()?
        .get(&ModelThinkingLevel::Off)
        .cloned()
        .flatten()
}

// =============================================================================
// JSON string wrappers (napi boundary)
// =============================================================================

/// Build the Responses params from JSON strings and return the params as a JSON
/// string. A convenience boundary for a napi shim.
pub fn build_params_from_json(
    model_json: &str,
    context_json: &str,
    options_json: &str,
) -> Result<String, String> {
    let model: OpenAIResponsesModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let context: Context =
        serde_json::from_str(context_json).map_err(|e| format!("invalid context json: {e}"))?;
    let options: OpenAIResponsesOptions =
        serde_json::from_str(options_json).map_err(|e| format!("invalid options json: {e}"))?;
    let params = build_params(&model, &context, &options);
    serde_json::to_string(&params).map_err(|e| e.to_string())
}

/// Process a JSON array of decoded `ResponseStreamEvent` values and return the
/// [`StreamOutcome`] as a JSON string. A convenience boundary for a napi shim.
pub fn process_responses_stream_from_json(
    events_json: &str,
    model_json: &str,
    service_tier: Option<String>,
    timestamp: i64,
) -> Result<String, String> {
    let events: Vec<Value> =
        serde_json::from_str(events_json).map_err(|e| format!("invalid events json: {e}"))?;
    let model: OpenAIResponsesModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let options = ResponsesStreamOptions { service_tier };
    let outcome: StreamOutcome = process_responses_stream(&events, &model, &options, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
