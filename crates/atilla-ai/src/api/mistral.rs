// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `api/mistral-conversations.ts`: the per-item content-delta arms (`text` /
// `thinking` / bare string) and the request-build helpers share pi's hand-rolled
// shapes by design. The clone detector reads the mirrored arms as duplicates;
// factoring them would distort the byte-faithful port, so the repetition is
// intentional.
//! Mistral `chat.stream` conversations driver, ported from pi-ai's
//! `packages/ai/src/api/mistral-conversations.ts` at pinned commit `3da591ab`.
//!
//! pi's Mistral driver wraps the `@mistralai/mistralai` SDK's `chat.stream`
//! async-iterable. This module ports the two halves that carry the wire
//! contract as **pure functions**, exactly as the Anthropic port splits SSE
//! decode from transport:
//!
//! - Request build ([`build_chat_payload`], [`build_request_headers`],
//!   [`to_function_tools`], [`to_chat_messages`], [`resolve_simple_options`]):
//!   turns a [`Context`] + options into the JSON body pi hands to
//!   `mistral.chat.stream`, including `strict:false` tool params, `{role:"tool"}`
//!   results, 9-char tool-id normalization, prompt caching, and reasoning mode.
//! - Stream decode ([`parse_chat_stream`]): consumes already-parsed
//!   `CompletionChunk` objects (pi's `event.data`) into atilla-ai's uniform
//!   [`AssistantMessageEvent`] stream plus the accumulated [`AssistantMessage`].
//!
//! DE-DUP / REBASE POINTS (driver-local helpers that belong in sibling-owned
//! shared modules once those land in Rust):
//! - [`short_hash`] ports `utils/hash.ts`.
//! - [`sanitize_surrogates`] ports `utils/sanitize-unicode.ts` (a no-op on Rust
//!   `&str`, which cannot hold unpaired surrogates).
//! - [`transform_messages`] ports `api/transform-messages.ts` (see that module).
//! - [`clamp_thinking_level`] / [`supported_thinking_levels`] port the matching
//!   `models.ts` helpers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention, ContentBlock, Message,
    Modality, ModelCost, ModelThinkingLevel, StopReason, ThinkingLevelMap, Usage, UsageCost,
    UserContent,
};
use crate::utils::json_parse::parse_streaming_json;

mod transform_messages;
use transform_messages::{transform_messages as transform_messages_impl, ModelIdentity};

#[cfg(test)]
mod tests;

/// Mistral requires tool-call IDs to be exactly 9 alphanumeric characters
/// (`mistral-conversations.ts:31`).
const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;

/// The minimum slice of a pi `Model` this driver needs. Deserialized leniently so
/// any additional pi model fields are ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MistralModel {
    pub id: String,
    pub api: String,
    pub provider: String,
    pub cost: ModelCost,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<Modality>,
    #[serde(default)]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub max_tokens: u64,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
}

impl MistralModel {
    fn supports_images(&self) -> bool {
        self.input.contains(&Modality::Image)
    }
}

/// A Mistral `toolChoice`, mirroring pi's `MistralOptions["toolChoice"]`
/// (`mistral-conversations.ts:40`).
#[derive(Debug, Clone, PartialEq)]
pub enum MistralToolChoice {
    Auto,
    None,
    Any,
    Required,
    Function { name: String },
}

/// Provider-specific request options, mirroring pi's `MistralOptions` plus the
/// `StreamOptions` subset the driver reads (`mistral-conversations.ts:39`).
///
/// This is a **driver-local** option struct; the shared [`crate::types::StreamOptions`]
/// is intentionally not widened (it currently models only `session_id` /
/// `cache_retention`).
#[derive(Debug, Clone, Default)]
pub struct MistralOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub tool_choice: Option<MistralToolChoice>,
    pub prompt_mode: Option<String>,
    pub reasoning_effort: Option<String>,
    pub session_id: Option<String>,
    pub cache_retention: Option<CacheRetention>,
    pub headers: Option<BTreeMap<String, String>>,
}

/// Provider-agnostic options `streamSimple` maps into [`MistralOptions`]
/// (`mistral-conversations.ts:110`). The `reasoning` level is the caller's
/// requested thinking level (pi's `SimpleStreamOptions["reasoning"]`).
#[derive(Debug, Clone, Default)]
pub struct SimpleMistralOptions {
    pub reasoning: Option<ModelThinkingLevel>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub session_id: Option<String>,
    pub cache_retention: Option<CacheRetention>,
}

// ---------------------------------------------------------------------------
// Hashing & tool-id normalization (`utils/hash.ts`, `mistral-conversations.ts:153`)
// ---------------------------------------------------------------------------

/// Fast deterministic hash to shorten long strings, ported byte-for-byte from
/// pi's `utils/hash.ts` (`shortHash`). Uses `wrapping` 32-bit arithmetic to match
/// JavaScript's `Math.imul` / `>>>` semantics exactly.
pub fn short_hash(input: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    // pi iterates JS UTF-16 code units via `charCodeAt`; iterate UTF-16 here to
    // reproduce the hash for astral characters bit-for-bit.
    for ch in input.encode_utf16() {
        let ch = ch as u32;
        h1 = (h1 ^ ch).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ ch).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", to_base36(h2), to_base36(h1))
}

/// `(n >>> 0).toString(36)`.
fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ascii")
}

fn retain_alphanumeric(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Derive a candidate 9-char Mistral tool-call ID for `id` on `attempt`
/// (`mistral-conversations.ts:175`).
pub fn derive_mistral_tool_call_id(id: &str, attempt: u32) -> String {
    let normalized = retain_alphanumeric(id);
    if attempt == 0 && normalized.len() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() {
        id.to_string()
    } else {
        normalized
    };
    let seed = if attempt == 0 {
        seed_base
    } else {
        format!("{seed_base}:{attempt}")
    };
    retain_alphanumeric(&short_hash(&seed))
        .chars()
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

/// Collision-free tool-call ID normalizer, mirroring pi's
/// `createMistralToolCallIdNormalizer` (`mistral-conversations.ts:153`): stable
/// per-original-id mapping with linear-probe dedupe on collisions.
#[derive(Default)]
pub struct MistralToolCallIdNormalizer {
    id_map: std::collections::HashMap<String, String>,
    reverse_map: std::collections::HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.id_map.get(id) {
            return existing.clone();
        }
        let mut attempt = 0;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            match self.reverse_map.get(&candidate) {
                Some(owner) if owner != id => {
                    attempt += 1;
                }
                _ => {
                    self.id_map.insert(id.to_string(), candidate.clone());
                    self.reverse_map.insert(candidate.clone(), id.to_string());
                    return candidate;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unicode sanitization (`utils/sanitize-unicode.ts`)
// ---------------------------------------------------------------------------

/// Remove unpaired Unicode surrogates (`sanitizeSurrogates`). Rust `&str` is
/// guaranteed well-formed UTF-8 and cannot contain unpaired surrogates, so this
/// is the identity function — the observable JS behaviour for valid input.
fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

// ---------------------------------------------------------------------------
// Request build
// ---------------------------------------------------------------------------

/// Recursively clone a tool-parameter schema, mirroring pi's `stripSymbolKeys`
/// (`mistral-conversations.ts:497`). TypeBox symbol keys are a JS-only concern
/// with no `serde_json` analogue, so the observable effect is a clean deep clone
/// of the JSON-Schema value.
fn strip_symbol_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_symbol_keys).collect()),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, entry) in map {
                result.insert(key.clone(), strip_symbol_keys(entry));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

/// Convert `Context.tools` into Mistral `function` tool definitions with
/// `strict:false` and cleaned params (`mistral-conversations.ts:485`).
pub fn to_function_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut function = Map::new();
            if let Some(name) = tool.get("name") {
                function.insert("name".to_string(), name.clone());
            }
            if let Some(description) = tool.get("description") {
                function.insert("description".to_string(), description.clone());
            }
            let parameters = tool
                .get("parameters")
                .map(strip_symbol_keys)
                .unwrap_or(Value::Null);
            function.insert("parameters".to_string(), parameters);
            function.insert("strict".to_string(), Value::Bool(false));
            json!({ "type": "function", "function": Value::Object(function) })
        })
        .collect()
}

fn map_tool_choice(choice: &MistralToolChoice) -> Value {
    match choice {
        MistralToolChoice::Auto => json!("auto"),
        MistralToolChoice::None => json!("none"),
        MistralToolChoice::Any => json!("any"),
        MistralToolChoice::Required => json!("required"),
        MistralToolChoice::Function { name } => {
            json!({ "type": "function", "function": { "name": name } })
        }
    }
}

/// Whether prompt caching applies: enabled unless `cacheRetention === "none"`,
/// and only when a session id is present (`mistral-conversations.ts:270`).
fn should_use_prompt_caching(options: &MistralOptions) -> Option<&str> {
    let disabled = matches!(options.cache_retention, Some(CacheRetention::None));
    match (disabled, options.session_id.as_deref()) {
        (false, Some(session_id)) if !session_id.is_empty() => Some(session_id),
        _ => None,
    }
}

/// Build the `chat.stream` request body pi passes to `mistral.chat.stream`
/// (`buildChatPayload`, `mistral-conversations.ts:240`).
///
/// `transform_messages` (tool-id normalization) is applied to `context.messages`
/// exactly as pi's `stream()` does before `buildChatPayload`.
pub fn build_chat_payload(
    model: &MistralModel,
    context: &Context,
    options: &MistralOptions,
) -> Value {
    let mut normalizer = MistralToolCallIdNormalizer::new();
    let transformed = transform_messages_impl(
        &context.messages,
        &ModelIdentity {
            id: &model.id,
            api: &model.api,
            provider: &model.provider,
            supports_images: model.supports_images(),
        },
        &mut |id| normalizer.normalize(id),
        0,
    );

    let mut messages = to_chat_messages(&transformed, model.supports_images());

    let mut payload = Map::new();
    payload.insert("model".to_string(), json!(model.id));
    payload.insert("stream".to_string(), json!(true));

    if let Some(tools) = &context.tools {
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(to_function_tools(tools)));
        }
    }
    if let Some(temperature) = options.temperature {
        payload.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        payload.insert("maxTokens".to_string(), json!(max_tokens));
    }
    if let Some(tool_choice) = &options.tool_choice {
        payload.insert("toolChoice".to_string(), map_tool_choice(tool_choice));
    }
    if let Some(prompt_mode) = &options.prompt_mode {
        payload.insert("promptMode".to_string(), json!(prompt_mode));
    }
    if let Some(reasoning_effort) = &options.reasoning_effort {
        payload.insert("reasoningEffort".to_string(), json!(reasoning_effort));
    }
    if let Some(session_id) = should_use_prompt_caching(options) {
        payload.insert("promptCacheKey".to_string(), json!(session_id));
    }

    if let Some(system_prompt) = &context.system_prompt {
        messages.insert(
            0,
            json!({ "role": "system", "content": sanitize_surrogates(system_prompt) }),
        );
    }
    payload.insert("messages".to_string(), Value::Array(messages));

    Value::Object(payload)
}

/// Build the request headers pi wires via `buildRequestOptions`
/// (`mistral-conversations.ts:213`): model headers, then caller headers, then the
/// `x-affinity` prompt-cache header (unless a caller already set it).
pub fn build_request_headers(
    model: &MistralModel,
    options: &MistralOptions,
) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    if let Some(model_headers) = &model.headers {
        for (k, v) in model_headers {
            headers.insert(k.clone(), v.clone());
        }
    }
    if let Some(option_headers) = &options.headers {
        for (k, v) in option_headers {
            headers.insert(k.clone(), v.clone());
        }
    }
    if let Some(session_id) = should_use_prompt_caching(options) {
        headers
            .entry("x-affinity".to_string())
            .or_insert_with(|| session_id.to_string());
    }
    headers
}

fn build_tool_result_text(
    text: &str,
    has_images: bool,
    supports_images: bool,
    is_error: bool,
) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".to_string()
            } else {
                "(see attached image)".to_string()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".to_string()
        } else {
            "(image omitted: model does not support images)".to_string()
        };
    }

    if is_error {
        "[tool error] (no tool output)".to_string()
    } else {
        "(no tool output)".to_string()
    }
}

fn image_data_uri(mime_type: &str, data: &str) -> String {
    format!("data:{mime_type};base64,{data}")
}

/// Convert transformed [`Message`]s into Mistral chat messages
/// (`toChatMessages`, `mistral-conversations.ts:513`).
pub fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    result.push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
                }
                UserContent::Blocks(blocks) => {
                    let had_images = blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Image { .. }));
                    let mut content: Vec<Value> = Vec::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text, .. } => {
                                content.push(
                                    json!({ "type": "text", "text": sanitize_surrogates(text) }),
                                );
                            }
                            ContentBlock::Image { data, mime_type } if supports_images => {
                                content.push(json!({
                                    "type": "image_url",
                                    "imageUrl": image_data_uri(mime_type, data)
                                }));
                            }
                            _ => {}
                        }
                    }
                    if !content.is_empty() {
                        result.push(json!({ "role": "user", "content": content }));
                        continue;
                    }
                    if had_images && !supports_images {
                        result.push(json!({
                            "role": "user",
                            "content": "(image omitted: model does not support images)"
                        }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut content_parts: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            if !text.trim().is_empty() {
                                content_parts.push(
                                    json!({ "type": "text", "text": sanitize_surrogates(text) }),
                                );
                            }
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            if !thinking.trim().is_empty() {
                                content_parts.push(json!({
                                    "type": "thinking",
                                    "thinking": [{ "type": "text", "text": sanitize_surrogates(thinking) }]
                                }));
                            }
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => {
                            let args = if arguments.is_null() {
                                json!({})
                            } else {
                                arguments.clone()
                            };
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string())
                                }
                            }));
                        }
                        _ => {}
                    }
                }

                let mut assistant_message = Map::new();
                assistant_message.insert("role".to_string(), json!("assistant"));
                if !content_parts.is_empty() {
                    assistant_message
                        .insert("content".to_string(), Value::Array(content_parts.clone()));
                }
                if !tool_calls.is_empty() {
                    assistant_message
                        .insert("toolCalls".to_string(), Value::Array(tool_calls.clone()));
                }
                if !content_parts.is_empty() || !tool_calls.is_empty() {
                    result.push(Value::Object(assistant_message));
                }
            }
            Message::ToolResult(tool_result) => {
                let text_result = tool_result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentBlock::Text { text, .. } => Some(sanitize_surrogates(text)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = tool_result
                    .content
                    .iter()
                    .any(|p| matches!(p, ContentBlock::Image { .. }));
                let tool_text = build_tool_result_text(
                    &text_result,
                    has_images,
                    supports_images,
                    tool_result.is_error,
                );
                let mut tool_content: Vec<Value> =
                    vec![json!({ "type": "text", "text": tool_text })];
                for part in &tool_result.content {
                    if !supports_images {
                        continue;
                    }
                    if let ContentBlock::Image { data, mime_type } = part {
                        tool_content.push(json!({
                            "type": "image_url",
                            "imageUrl": image_data_uri(mime_type, data)
                        }));
                    }
                }
                result.push(json!({
                    "role": "tool",
                    "toolCallId": tool_result.tool_call_id,
                    "name": tool_result.tool_name,
                    "content": tool_content
                }));
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Reasoning-mode selection (`streamSimple`, `mistral-conversations.ts:110`)
// ---------------------------------------------------------------------------

fn uses_reasoning_effort(model: &MistralModel) -> bool {
    model.id == "mistral-small-2603"
        || model.id == "mistral-small-latest"
        || model.id == "mistral-medium-3.5"
}

fn uses_prompt_mode_reasoning(model: &MistralModel) -> bool {
    model.reasoning && !uses_reasoning_effort(model)
}

fn map_reasoning_effort(model: &MistralModel, level: ModelThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level).cloned().flatten())
        .unwrap_or_else(|| "high".to_string())
}

/// The extended thinking-level ladder pi uses for clamping
/// (`models.ts:661`).
const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// Port of `models.ts:getSupportedThinkingLevels`.
fn supported_thinking_levels(model: &MistralModel) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    EXTENDED_THINKING_LEVELS
        .iter()
        .copied()
        .filter(|level| {
            let mapped = model.thinking_level_map.as_ref().and_then(|m| m.get(level));
            // `null` (Some(None)) marks the level unsupported.
            if matches!(mapped, Some(None)) {
                return false;
            }
            if matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max) {
                // Only supported when explicitly mapped to a non-null value.
                return matches!(mapped, Some(Some(_)));
            }
            true
        })
        .collect()
}

/// Port of `models.ts:clampThinkingLevel`.
fn clamp_thinking_level(model: &MistralModel, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let requested_index = EXTENDED_THINKING_LEVELS.iter().position(|l| *l == level);
    let Some(requested_index) = requested_index else {
        return available
            .first()
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
    };
    for candidate in EXTENDED_THINKING_LEVELS.iter().skip(requested_index) {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

/// Map provider-agnostic [`SimpleMistralOptions`] to [`MistralOptions`], mirroring
/// pi's `streamSimple` reasoning-mode selection (`mistral-conversations.ts:110`).
pub fn resolve_simple_options(
    model: &MistralModel,
    options: &SimpleMistralOptions,
) -> MistralOptions {
    let clamped = options
        .reasoning
        .map(|level| clamp_thinking_level(model, level));
    let reasoning = match clamped {
        Some(ModelThinkingLevel::Off) => None,
        other => other,
    };
    let should_use_reasoning = model.reasoning && reasoning.is_some();

    let prompt_mode = if should_use_reasoning && uses_prompt_mode_reasoning(model) {
        Some("reasoning".to_string())
    } else {
        None
    };
    let reasoning_effort = if should_use_reasoning && uses_reasoning_effort(model) {
        Some(map_reasoning_effort(
            model,
            reasoning.expect("reasoning present"),
        ))
    } else {
        None
    };

    MistralOptions {
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        tool_choice: None,
        prompt_mode,
        reasoning_effort,
        session_id: options.session_id.clone(),
        cache_retention: options.cache_retention,
        headers: None,
    }
}

// ---------------------------------------------------------------------------
// Stream decode (`consumeChatStream`, `mistral-conversations.ts:295`)
// ---------------------------------------------------------------------------

/// Map a Mistral chat `finish_reason` to a [`StopReason`]
/// (`mapChatStopReason`, `mistral-conversations.ts:649`).
fn map_chat_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        None => StopReason::Stop,
        Some("stop") => StopReason::Stop,
        Some("length") | Some("model_length") => StopReason::Length,
        Some("tool_calls") => StopReason::ToolUse,
        Some("error") => StopReason::Error,
        Some(_) => StopReason::Stop,
    }
}

/// Read Mistral's cached-prompt-token count across the snake/camel-case variants
/// pi tolerates (`getMistralCachedPromptTokens`, `mistral-conversations.ts:274`).
fn get_mistral_cached_prompt_tokens(usage: &Value, prompt_tokens: u64) -> u64 {
    let raw = usage
        .get("promptTokensDetails")
        .and_then(|d| d.get("cachedTokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
        })
        .or_else(|| {
            usage
                .get("promptTokenDetails")
                .and_then(|d| d.get("cachedTokens"))
        })
        .or_else(|| {
            usage
                .get("prompt_token_details")
                .and_then(|d| d.get("cached_tokens"))
        })
        .or_else(|| usage.get("numCachedTokens"))
        .or_else(|| usage.get("num_cached_tokens"));
    let cached = raw
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .map(|n| n.max(0.0) as u64)
        .unwrap_or(0);
    cached.min(prompt_tokens)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The result of decoding a Mistral chat stream: the full event sequence and the
/// accumulated final message (identical shape to
/// [`crate::seams::provider::StreamResult`] and the Anthropic port's outcome).
#[derive(Debug, Clone, Serialize)]
pub struct StreamOutcome {
    pub events: Vec<AssistantMessageEvent>,
    pub message: AssistantMessage,
}

// Re-export the boundary `Context` for callers building payloads.
pub use crate::types::Context;

fn zero_usage() -> Usage {
    Usage {
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

fn recompute_cost(model: &MistralModel, usage: &mut Usage) {
    usage.cost = calculate_cost_with(&model.cost, usage);
}

/// The kind of the currently-accumulating text/thinking block, mirroring pi's
/// `currentBlock` discriminant.
#[derive(PartialEq)]
enum CurrentKind {
    Text,
    Thinking,
}

/// Decode already-parsed Mistral `CompletionChunk` objects (pi's `event.data`)
/// into the uniform event stream and final message for `model`.
///
/// Mirrors pi's `stream()` inner loop: a `start` event, then `consumeChatStream`,
/// then a terminal `done` — or, when the decoded stop reason is `error`/`aborted`,
/// a terminal `error` carrying `"An unknown error occurred"` (pi throws that
/// message, and its `catch` records it via `formatMistralError`).
pub fn parse_chat_stream(chunks: &[Value], model: &MistralModel, timestamp: i64) -> StreamOutcome {
    let mut output = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp,
    };
    let mut events: Vec<AssistantMessageEvent> = Vec::new();

    events.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    consume_chat_stream(chunks, model, &mut output, &mut events);

    // pi's post-loop guard: an error/aborted stop is re-thrown and surfaced as an
    // error event (`An unknown error occurred`) rather than a done event.
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let message = output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_string());
        output.stop_reason = StopReason::Error;
        output.error_message = Some(message);
        events.push(AssistantMessageEvent::Error {
            reason: output.stop_reason,
            error: output.clone(),
        });
    } else {
        events.push(AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output.clone(),
        });
    }

    StreamOutcome {
        events,
        message: output,
    }
}

fn consume_chat_stream(
    chunks: &[Value],
    model: &MistralModel,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let mut current: Option<CurrentKind> = None;
    // Tool blocks keyed by `${callId}:${index}` → content index. `tool_order`
    // preserves pi's Map insertion order for the terminal `toolcall_end` flush.
    let mut tool_blocks_by_key: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut tool_order: Vec<usize> = Vec::new();
    let mut tool_partial_args: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();

    for chunk in chunks {
        if output.response_id.is_none() {
            if let Some(id) = chunk.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    output.response_id = Some(id.to_string());
                }
            }
        }

        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            let prompt_tokens = u64_field(usage, "promptTokens");
            let cached_prompt_tokens = get_mistral_cached_prompt_tokens(usage, prompt_tokens);
            output.usage.input = prompt_tokens.saturating_sub(cached_prompt_tokens);
            output.usage.output = u64_field(usage, "completionTokens");
            output.usage.cache_read = cached_prompt_tokens;
            output.usage.cache_write = 0;
            let total = usage.get("totalTokens").and_then(Value::as_u64);
            output.usage.total_tokens = total.unwrap_or(
                output.usage.input
                    + output.usage.output
                    + output.usage.cache_read
                    + output.usage.cache_write,
            );
            recompute_cost(model, &mut output.usage);
        }

        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
            continue;
        };

        if let Some(finish_reason) = choice.get("finishReason").and_then(Value::as_str) {
            output.stop_reason = map_chat_stop_reason(Some(finish_reason));
        }

        let delta = choice.get("delta");

        if let Some(content) = delta
            .and_then(|d| d.get("content"))
            .filter(|c| !c.is_null())
        {
            let items: Vec<Value> = match content {
                Value::String(s) => vec![Value::String(s.clone())],
                Value::Array(arr) => arr.clone(),
                other => vec![other.clone()],
            };
            for item in &items {
                consume_content_item(item, &mut current, output, events);
            }
        }

        let tool_calls = delta
            .and_then(|d| d.get("toolCalls"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool_call in &tool_calls {
            if let Some(kind) = current.take() {
                finish_current_block(&kind, output, events);
            }
            let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
            let provided_id = tool_call.get("id").and_then(Value::as_str);
            let call_id = match provided_id {
                Some(id) if id != "null" => id.to_string(),
                _ => derive_mistral_tool_call_id(&format!("toolcall:{index}"), 0),
            };
            let key = format!("{call_id}:{index}");
            let name = tool_call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let block_index = match tool_blocks_by_key.get(&key) {
                Some(&idx)
                    if matches!(output.content.get(idx), Some(ContentBlock::ToolCall { .. })) =>
                {
                    idx
                }
                _ => {
                    output.content.push(ContentBlock::ToolCall {
                        id: call_id.clone(),
                        name,
                        arguments: json!({}),
                        thought_signature: None,
                    });
                    let idx = output.content.len() - 1;
                    tool_blocks_by_key.insert(key.clone(), idx);
                    tool_order.push(idx);
                    tool_partial_args.insert(idx, String::new());
                    events.push(AssistantMessageEvent::ToolcallStart {
                        content_index: idx as u32,
                        partial: output.clone(),
                    });
                    idx
                }
            };

            let args_delta = match tool_call.get("function").and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => s.clone(),
                Some(other) if !other.is_null() => {
                    serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string())
                }
                _ => "{}".to_string(),
            };
            let partial = tool_partial_args.entry(block_index).or_default();
            partial.push_str(&args_delta);
            let parsed = parse_streaming_json(Some(partial));
            if let Some(ContentBlock::ToolCall { arguments, .. }) =
                output.content.get_mut(block_index)
            {
                *arguments = parsed;
            }
            events.push(AssistantMessageEvent::ToolcallDelta {
                content_index: block_index as u32,
                delta: args_delta,
                partial: output.clone(),
            });
        }
    }

    if let Some(kind) = current.take() {
        finish_current_block(&kind, output, events);
    }
    for &index in &tool_order {
        if !matches!(
            output.content.get(index),
            Some(ContentBlock::ToolCall { .. })
        ) {
            continue;
        }
        let partial = tool_partial_args.get(&index).cloned().unwrap_or_default();
        let parsed = parse_streaming_json(Some(&partial));
        if let Some(ContentBlock::ToolCall { arguments, .. }) = output.content.get_mut(index) {
            *arguments = parsed;
        }
        let tool_call = output.content[index].clone();
        events.push(AssistantMessageEvent::ToolcallEnd {
            content_index: index as u32,
            tool_call,
            partial: output.clone(),
        });
    }
}

/// Handle a single content-delta item (bare string, `text`, or `thinking`),
/// mirroring pi's per-item branch in `consumeChatStream`.
fn consume_content_item(
    item: &Value,
    current: &mut Option<CurrentKind>,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    if let Some(text) = item.as_str() {
        push_text_delta(&sanitize_surrogates(text), current, output, events);
        return;
    }
    match item.get("type").and_then(Value::as_str) {
        Some("thinking") => {
            let delta_text = item
                .get("thinking")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            let thinking_delta = sanitize_surrogates(&delta_text);
            if thinking_delta.is_empty() {
                return;
            }
            if !matches!(current, Some(CurrentKind::Thinking)) {
                if let Some(kind) = current.take() {
                    finish_current_block(&kind, output, events);
                }
                output.content.push(ContentBlock::Thinking {
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                });
                *current = Some(CurrentKind::Thinking);
                events.push(AssistantMessageEvent::ThinkingStart {
                    content_index: (output.content.len() - 1) as u32,
                    partial: output.clone(),
                });
            }
            if let Some(ContentBlock::Thinking { thinking, .. }) = output.content.last_mut() {
                thinking.push_str(&thinking_delta);
            }
            events.push(AssistantMessageEvent::ThinkingDelta {
                content_index: (output.content.len() - 1) as u32,
                delta: thinking_delta,
                partial: output.clone(),
            });
        }
        Some("text") => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or("");
            push_text_delta(&sanitize_surrogates(text), current, output, events);
        }
        _ => {}
    }
}

fn push_text_delta(
    text_delta: &str,
    current: &mut Option<CurrentKind>,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    if !matches!(current, Some(CurrentKind::Text)) {
        if let Some(kind) = current.take() {
            finish_current_block(&kind, output, events);
        }
        output.content.push(ContentBlock::Text {
            text: String::new(),
            text_signature: None,
        });
        *current = Some(CurrentKind::Text);
        events.push(AssistantMessageEvent::TextStart {
            content_index: (output.content.len() - 1) as u32,
            partial: output.clone(),
        });
    }
    if let Some(ContentBlock::Text { text, .. }) = output.content.last_mut() {
        text.push_str(text_delta);
    }
    events.push(AssistantMessageEvent::TextDelta {
        content_index: (output.content.len() - 1) as u32,
        delta: text_delta.to_string(),
        partial: output.clone(),
    });
}

/// Emit the terminal `text_end` / `thinking_end` for the current block, mirroring
/// pi's `finishCurrentBlock` (which uses `blockIndex()` = last content index).
fn finish_current_block(
    kind: &CurrentKind,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let content_index = (output.content.len() - 1) as u32;
    match kind {
        CurrentKind::Text => {
            let content = match output.content.last() {
                Some(ContentBlock::Text { text, .. }) => text.clone(),
                _ => String::new(),
            };
            events.push(AssistantMessageEvent::TextEnd {
                content_index,
                content,
                partial: output.clone(),
            });
        }
        CurrentKind::Thinking => {
            let content = match output.content.last() {
                Some(ContentBlock::Thinking { thinking, .. }) => thinking.clone(),
                _ => String::new(),
            };
            events.push(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content,
                partial: output.clone(),
            });
        }
    }
}

/// Decode a JSON array of Mistral `CompletionChunk` objects for the model
/// described by `model_json` and return the [`StreamOutcome`] as a JSON string.
///
/// This is the boundary entry point a napi shim calls: the shim collects the
/// SDK/transport chunks (pi's `event.data`) into a JSON array, hands them here
/// with the JSON-serialized model, and replays the returned `events`.
pub fn parse_chat_stream_to_json(
    chunks_json: &str,
    model_json: &str,
    timestamp: i64,
) -> Result<String, String> {
    let chunks: Vec<Value> =
        serde_json::from_str(chunks_json).map_err(|e| format!("invalid chunks json: {e}"))?;
    let model: MistralModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let outcome = parse_chat_stream(&chunks, &model, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}
