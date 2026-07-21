// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `api/bedrock-converse-stream.ts`: the per-item stream-dispatch arms
// (`contentBlockStart` / `contentBlockDelta` / `contentBlockStop` build a
// matching block then push a mirrored event) and the request-build helpers share
// pi's hand-rolled shapes by design. The clone detector reads the mirrored arms
// and the parallel model-matching helpers as duplicates; factoring them would
// distort the byte-faithful port, so the repetition is intentional.
//! Amazon Bedrock `ConverseStream` driver, ported from pi-ai's
//! `packages/ai/src/api/bedrock-converse-stream.ts` at pinned commit `3da591ab`.
//!
//! pi's Bedrock driver wraps the `@aws-sdk/client-bedrock-runtime` SDK's
//! `ConverseStreamCommand`. Following the crate convention (no SDK, hand-rolled
//! wire logic, exactly as the Mistral and Anthropic ports do), this module ports
//! the two halves that carry the wire contract as **pure functions**:
//!
//! - Request build ([`build_command_input`], [`build_client_config`],
//!   [`convert_messages`], [`build_system_prompt`], [`convert_tool_config`],
//!   [`build_additional_model_request_fields`], [`apply_custom_headers`]): turns a
//!   [`Context`] + [`BedrockOptions`] into the JSON `ConverseStream` command input
//!   pi hands to `client.send`, plus the SDK client config (region / endpoint /
//!   bearer-token resolution) and the caller-header middleware behaviour.
//! - Stream decode ([`parse_converse_stream`]): consumes already-parsed Converse
//!   stream event items (pi's `for await (const item of response.stream)`) into
//!   pidgin-ai's uniform [`AssistantMessageEvent`] stream plus the accumulated
//!   [`AssistantMessage`].
//!
//! # Divergences from pi (documented seams)
//!
//! - **Env is injected, not read from the process.** pi resolves `AWS_REGION`,
//!   `AWS_PROFILE`, bearer tokens, etc. from `options.env` layered over
//!   `process.env`. This port keeps `options.env` (scoped overrides) but takes
//!   the ambient environment as an explicit `process_env: &ProviderEnv`
//!   parameter, so config resolution is a deterministic pure function. In
//!   production the (not-yet-wired) provider seam passes a snapshot of the real
//!   environment; the tests pass a controlled map. Resolution order (scoped, then
//!   ambient) matches pi's `getProviderEnvValue`.
//! - **Transport-only fields are not modelled.** pi's `config.requestHandler`
//!   (HTTP proxy / force-HTTP1 handling) is a Node transport object with no
//!   observable JSON shape and is not part of the wire contract; it is omitted
//!   from [`build_client_config`]. Everything the endpoint-resolution tests assert
//!   (region / endpoint / profile / token / authSchemePreference / credentials) is
//!   preserved.
//! - **`createImageBlock` bytes.** pi decodes base64 to a `Uint8Array`; here the
//!   decoded bytes become a JSON array of octets. No Bedrock test exercises image
//!   content through the driver, so this is only a representation choice.
//!
//! DE-DUP / REBASE POINTS:
//! - [`transform_messages`] ports `api/transform-messages.ts` (see that module).
//! - [`sanitize_surrogates`] reuses the shared [`crate::utils::sanitize_unicode`]
//!   port.

use std::collections::BTreeMap;

use base64::Engine as _;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::api::anthropic::simple_options::{
    clamp_max_tokens_to_context_window, ThinkingBudgets as AdjustThinkingBudgets,
};
use crate::types::{
    CacheRetention, ContentBlock, Message, Modality, ModelCost, ThinkingLevel, ThinkingLevelMap,
    ToolResultMessage, UserContent,
};
use crate::utils::headers::{provider_headers_to_record, ProviderHeaders};
use crate::utils::provider_env::ProviderEnv;
use crate::utils::sanitize_unicode::sanitize_surrogates;

pub mod driver;
pub(crate) mod eventstream;
pub(crate) mod sigv4;
mod stream;
mod transform_messages;
pub use eventstream::{decode_event_stream, EventStreamError};
pub use stream::{parse_converse_stream, parse_converse_stream_to_json, StreamOutcome};
use transform_messages::{transform_messages as transform_messages_impl, ModelIdentity};

#[cfg(test)]
mod tests;

// Re-export the boundary `Context` for callers building payloads.
pub use crate::types::Context;

/// pi's `EMPTY_TEXT_PLACEHOLDER` (`bedrock-converse-stream.ts:103`). Bedrock
/// rejects empty text blocks, so blank content is replaced with this sentinel.
const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";

/// The interleaved-thinking beta flag pi attaches for non-adaptive Claude models
/// (`bedrock-converse-stream.ts:1055`).
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// The Smithy `build`-step middleware descriptor pi registers to inject caller
/// headers (`bedrock-converse-stream.ts:389`). Ported as a constant since the
/// Smithy middleware stack itself is SDK-internal; the observable contract is the
/// `(step, name, priority)` triple plus the reserved-header filtering below.
pub const CUSTOM_HEADERS_MIDDLEWARE_STEP: &str = "build";
/// The middleware name pi registers (`bedrock-converse-stream.ts:389`).
pub const CUSTOM_HEADERS_MIDDLEWARE_NAME: &str = "pi-ai-custom-headers";
/// The middleware priority pi registers (`bedrock-converse-stream.ts:389`).
pub const CUSTOM_HEADERS_MIDDLEWARE_PRIORITY: &str = "low";

// ---------------------------------------------------------------------------
// Model & options
// ---------------------------------------------------------------------------

/// The minimum slice of a pi `Model<"bedrock-converse-stream">` this driver
/// needs. Deserialized leniently so any additional pi model fields are ignored,
/// which makes bridging from the catalog `Model` a plain `serde_json` round-trip.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
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
    pub base_url: Option<String>,
    #[serde(default)]
    pub max_tokens: u64,
    /// The model's context window in tokens, read by the `streamSimple`
    /// context-clamp (pi's `clampMaxTokensToContext`, `simple-options.ts:15`).
    #[serde(default)]
    pub context_window: u64,
}

impl BedrockModel {
    fn supports_images(&self) -> bool {
        self.input.contains(&Modality::Image)
    }

    pub(crate) fn name_ref(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// Controls how Claude's thinking content is returned (pi's
/// `BedrockThinkingDisplay`, `bedrock-converse-stream.ts:65`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockThinkingDisplay {
    Summarized,
    Omitted,
}

impl BedrockThinkingDisplay {
    fn as_str(self) -> &'static str {
        match self {
            BedrockThinkingDisplay::Summarized => "summarized",
            BedrockThinkingDisplay::Omitted => "omitted",
        }
    }
}

/// A Bedrock `toolChoice`, mirroring pi's `BedrockOptions["toolChoice"]`
/// (`bedrock-converse-stream.ts:70`).
#[derive(Debug, Clone, PartialEq)]
pub enum BedrockToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

/// Custom token budgets per thinking level (pi's `ThinkingBudgets`).
pub type ThinkingBudgets = BTreeMap<ThinkingLevel, u64>;

/// Provider-specific request options, mirroring pi's `BedrockOptions` plus the
/// `StreamOptions` subset the driver reads (`bedrock-converse-stream.ts:67`).
#[derive(Debug, Clone, Default)]
pub struct BedrockOptions {
    pub region: Option<String>,
    pub profile: Option<String>,
    pub tool_choice: Option<BedrockToolChoice>,
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub interleaved_thinking: Option<bool>,
    pub thinking_display: Option<BedrockThinkingDisplay>,
    pub request_metadata: Option<BTreeMap<String, String>>,
    pub bearer_token: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub headers: Option<ProviderHeaders>,
    pub cache_retention: Option<CacheRetention>,
    /// Scoped provider-env overrides (pi's `options.env`).
    pub env: Option<ProviderEnv>,
}

// ---------------------------------------------------------------------------
// Model-matching helpers (`bedrock-converse-stream.ts:569`)
// ---------------------------------------------------------------------------

/// Candidate lowercase strings pi matches model families against
/// (`getModelMatchCandidates`, `bedrock-converse-stream.ts:569`): for each of
/// `[id, name?]`, both the lowercased value and the lowercased value with runs of
/// `[\s_.:]` collapsed to `-`.
fn get_model_match_candidates(model_id: &str, model_name: Option<&str>) -> Vec<String> {
    let values: Vec<&str> = match model_name {
        Some(name) => vec![model_id, name],
        None => vec![model_id],
    };
    let sep = Regex::new(r"[\s_.:]+").expect("valid separator regex");
    let mut out = Vec::new();
    for value in values {
        let lower = value.to_lowercase();
        let dashed = sep.replace_all(&lower, "-").to_string();
        out.push(lower);
        out.push(dashed);
    }
    out
}

/// Map a caller [`ThinkingLevel`] to the catalog's [`ModelThinkingLevel`] key
/// used by `thinkingLevelMap` lookups.
fn to_model_thinking_level(level: ThinkingLevel) -> crate::types::ModelThinkingLevel {
    use crate::types::ModelThinkingLevel;
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// Whether the model supports adaptive thinking (Opus 4.6+, Sonnet 4.6, Claude 5)
/// (`supportsAdaptiveThinking`, `bedrock-converse-stream.ts:577`).
///
/// `pub(crate)` so the sibling `driver::stream_simple` port can take pi's
/// adaptive-vs-budget Claude sub-branch (`:403`).
pub(crate) fn supports_adaptive_thinking(model_id: &str, model_name: Option<&str>) -> bool {
    let candidates = get_model_match_candidates(model_id, model_name);
    candidates.iter().any(|s| {
        s.contains("opus-4-6")
            || s.contains("opus-4-7")
            || s.contains("opus-4-8")
            || s.contains("sonnet-4-6")
            || s.contains("sonnet-5")
            || s.contains("fable-5")
    })
}

/// Whether the model has a native `xhigh` effort level
/// (`supportsNativeXhighEffort`, `bedrock-converse-stream.ts:590`).
fn supports_native_xhigh_effort(model: &BedrockModel) -> bool {
    let candidates = get_model_match_candidates(&model.id, model.name_ref());
    candidates.iter().any(|s| {
        s.contains("opus-4-7")
            || s.contains("opus-4-8")
            || s.contains("sonnet-5")
            || s.contains("fable-5")
    })
}

/// Map a requested thinking level to a Bedrock effort string
/// (`mapThinkingLevelToEffort`, `bedrock-converse-stream.ts:597`).
fn map_thinking_level_to_effort(model: &BedrockModel, level: Option<ThinkingLevel>) -> String {
    if level == Some(ThinkingLevel::Xhigh) && supports_native_xhigh_effort(model) {
        return "xhigh".to_string();
    }

    let mapped = level
        .and_then(|level| {
            model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&to_model_thinking_level(level)))
        })
        .and_then(|v| v.clone());
    if let Some(mapped) = mapped {
        return mapped;
    }

    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low".to_string(),
        Some(ThinkingLevel::Medium) => "medium".to_string(),
        Some(ThinkingLevel::High) => "high".to_string(),
        _ => "high".to_string(),
    }
}

/// Whether the model is an Anthropic Claude model on Bedrock
/// (`isAnthropicClaudeModel`, `bedrock-converse-stream.ts:638`).
///
/// `pub(crate)` so the sibling `driver::stream_simple` port can branch Claude vs
/// non-Claude exactly as pi's `streamSimple` does (`:402`).
pub(crate) fn is_anthropic_claude_model(model: &BedrockModel) -> bool {
    let id = model.id.to_lowercase();
    let name = model.name_ref().unwrap_or("").to_lowercase();
    id.contains("anthropic.claude")
        || id.contains("anthropic/claude")
        || name.contains("anthropic.claude")
        || name.contains("anthropic/claude")
        || name.contains("claude")
}

/// Whether the model supports prompt caching (`supportsPromptCaching`,
/// `bedrock-converse-stream.ts:662`).
fn supports_prompt_caching(
    model: &BedrockModel,
    scoped: Option<&ProviderEnv>,
    process_env: &ProviderEnv,
) -> bool {
    let candidates = get_model_match_candidates(&model.id, model.name_ref());

    let has_claude_ref = candidates.iter().any(|s| s.contains("claude"));
    if !has_claude_ref {
        // Application inference profiles don't contain the model name in the ARN.
        // Allow users to force cache points via environment variable.
        return env_value("AWS_BEDROCK_FORCE_CACHE", scoped, process_env).as_deref() == Some("1");
    }
    if candidates
        .iter()
        .any(|s| s.contains("fable-5") || s.contains("sonnet-5"))
    {
        return true;
    }
    if candidates.iter().any(|s| s.contains("-4-")) {
        return true;
    }
    if candidates.iter().any(|s| s.contains("claude-3-7-sonnet")) {
        return true;
    }
    if candidates.iter().any(|s| s.contains("claude-3-5-haiku")) {
        return true;
    }
    false
}

/// Whether the model supports thinking signatures in reasoningContent
/// (`supportsThinkingSignature`, `bedrock-converse-stream.ts:691`).
fn supports_thinking_signature(model: &BedrockModel) -> bool {
    is_anthropic_claude_model(model)
}

// ---------------------------------------------------------------------------
// Environment resolution (documented injection seam)
// ---------------------------------------------------------------------------

/// Resolve an env value from scoped overrides, then the ambient process env,
/// treating empty strings as absent — pi's `getProviderEnvValue(name, scoped)`
/// with the ambient environment injected instead of read from `process.env`.
fn env_value(
    name: &str,
    scoped: Option<&ProviderEnv>,
    process_env: &ProviderEnv,
) -> Option<String> {
    if let Some(value) = scoped.and_then(|env| env.get(name)) {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }
    match process_env.get(name) {
        Some(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

/// Resolve an env value from the ambient process env only, mirroring pi's
/// `getProviderEnvValue(name)` (no scoped argument) used for the ambient-profile
/// probe (`bedrock-converse-stream.ts:137`).
fn ambient_env_value(name: &str, process_env: &ProviderEnv) -> Option<String> {
    match process_env.get(name) {
        Some(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Cache-retention resolution (`resolveCacheRetention`, ...:623)
// ---------------------------------------------------------------------------

fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    scoped: Option<&ProviderEnv>,
    process_env: &ProviderEnv,
) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if env_value("PI_CACHE_RETENTION", scoped, process_env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

// ---------------------------------------------------------------------------
// Content-block builders (`bedrock-converse-stream.ts:715`)
// ---------------------------------------------------------------------------

/// `normalizeToolCallId` (`bedrock-converse-stream.ts:715`): sanitize to
/// `[a-zA-Z0-9_-]`, truncate to 64 chars.
fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.chars().count() > 64 {
        sanitized.chars().take(64).collect()
    } else {
        sanitized
    }
}

/// `createNonBlankTextBlock` (`bedrock-converse-stream.ts:720`): sanitize then
/// drop the block entirely when the result is all-whitespace.
fn create_non_blank_text_block(text: &str) -> Option<Value> {
    let sanitized = sanitize_surrogates(text);
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(json!({ "text": sanitized }))
    }
}

/// `createRequiredTextBlock` (`bedrock-converse-stream.ts:725`): like
/// [`create_non_blank_text_block`] but substitutes the empty-text placeholder.
fn create_required_text_block(text: &str) -> Value {
    create_non_blank_text_block(text).unwrap_or_else(|| json!({ "text": EMPTY_TEXT_PLACEHOLDER }))
}

/// `createImageBlock` (`bedrock-converse-stream.ts:1064`): map the MIME type to a
/// Bedrock `ImageFormat` and decode the base64 payload to bytes.
fn create_image_block(mime_type: &str, data: &str) -> Result<Value, String> {
    let format = match mime_type {
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => return Err(format!("Unknown image type: {other}")),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap_or_default();
    let byte_values: Vec<Value> = bytes.into_iter().map(|b| json!(b)).collect();
    Ok(json!({ "source": { "bytes": byte_values }, "format": format }))
}

/// `convertToolResultContent` (`bedrock-converse-stream.ts:729`).
fn convert_tool_result_content(content: &[ContentBlock]) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for c in content {
        match c {
            ContentBlock::Image { data, mime_type } => {
                if let Ok(image) = create_image_block(mime_type, data) {
                    result.push(json!({ "image": image }));
                }
            }
            ContentBlock::Text { text, .. } => {
                if let Some(block) = create_non_blank_text_block(text) {
                    result.push(block);
                }
            }
            _ => {}
        }
    }
    if result.is_empty() {
        result.push(json!({ "text": EMPTY_TEXT_PLACEHOLDER }));
    }
    result
}

// ---------------------------------------------------------------------------
// System prompt & message conversion (`bedrock-converse-stream.ts:695`, :743)
// ---------------------------------------------------------------------------

fn cache_point(cache_retention: CacheRetention) -> Value {
    let mut point = Map::new();
    point.insert("type".to_string(), json!("default"));
    if cache_retention == CacheRetention::Long {
        point.insert("ttl".to_string(), json!("ONE_HOUR"));
    }
    json!({ "cachePoint": Value::Object(point) })
}

/// `buildSystemPrompt` (`bedrock-converse-stream.ts:695`).
pub fn build_system_prompt(
    system_prompt: Option<&str>,
    model: &BedrockModel,
    cache_retention: CacheRetention,
    scoped: Option<&ProviderEnv>,
    process_env: &ProviderEnv,
) -> Option<Vec<Value>> {
    let system_prompt = system_prompt?;
    let mut blocks: Vec<Value> = vec![json!({ "text": sanitize_surrogates(system_prompt) })];

    if cache_retention != CacheRetention::None
        && supports_prompt_caching(model, scoped, process_env)
    {
        blocks.push(cache_point(cache_retention));
    }

    Some(blocks)
}

/// `convertMessages` (`bedrock-converse-stream.ts:743`).
pub fn convert_messages(
    context: &Context,
    model: &BedrockModel,
    cache_retention: CacheRetention,
    scoped: Option<&ProviderEnv>,
    process_env: &ProviderEnv,
) -> Vec<Value> {
    let transformed = transform_messages_impl(
        &context.messages,
        &ModelIdentity {
            id: &model.id,
            api: &model.api,
            provider: &model.provider,
            supports_images: model.supports_images(),
        },
        &mut normalize_tool_call_id,
        0,
    );

    let mut result: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < transformed.len() {
        match &transformed[i] {
            Message::User(user) => {
                let mut content: Vec<Value> = Vec::new();
                match &user.content {
                    UserContent::Text(text) => {
                        content.push(create_required_text_block(text));
                    }
                    UserContent::Blocks(blocks) => {
                        for c in blocks {
                            match c {
                                ContentBlock::Text { text, .. } => {
                                    if let Some(block) = create_non_blank_text_block(text) {
                                        content.push(block);
                                    }
                                }
                                ContentBlock::Image { data, mime_type } => {
                                    if let Ok(image) = create_image_block(mime_type, data) {
                                        content.push(json!({ "image": image }));
                                    }
                                }
                                _ => continue,
                            }
                        }
                        if content.is_empty() {
                            content.push(json!({ "text": EMPTY_TEXT_PLACEHOLDER }));
                        }
                    }
                }
                result.push(json!({ "role": "user", "content": content }));
            }
            Message::Assistant(assistant) => {
                // Skip assistant messages with empty content (e.g. aborted requests);
                // Bedrock rejects empty content arrays.
                if assistant.content.is_empty() {
                    i += 1;
                    continue;
                }
                let content_blocks = convert_assistant_content(&assistant.content, model);
                if content_blocks.is_empty() {
                    i += 1;
                    continue;
                }
                result.push(json!({ "role": "assistant", "content": content_blocks }));
            }
            Message::ToolResult(tool_result) => {
                let mut tool_results: Vec<Value> = Vec::new();
                tool_results.push(tool_result_block(tool_result));

                // Look ahead for consecutive toolResult messages; Bedrock requires
                // all tool results to be in one user message.
                let mut j = i + 1;
                while j < transformed.len() {
                    if let Message::ToolResult(next) = &transformed[j] {
                        tool_results.push(tool_result_block(next));
                        j += 1;
                    } else {
                        break;
                    }
                }
                i = j - 1;
                result.push(json!({ "role": "user", "content": tool_results }));
            }
        }
        i += 1;
    }

    // Add a cache point to the last user message for supported Claude models.
    if cache_retention != CacheRetention::None
        && supports_prompt_caching(model, scoped, process_env)
        && !result.is_empty()
    {
        let last_index = result.len() - 1;
        let last = &mut result[last_index];
        let is_user = last.get("role").and_then(Value::as_str) == Some("user");
        if is_user {
            if let Some(Value::Array(content)) = last.get_mut("content") {
                content.push(cache_point(cache_retention));
            }
        }
    }

    result
}

fn tool_result_block(tool_result: &ToolResultMessage) -> Value {
    json!({
        "toolResult": {
            "toolUseId": tool_result.tool_call_id,
            "content": convert_tool_result_content(&tool_result.content),
            "status": if tool_result.is_error { "error" } else { "success" },
        }
    })
}

fn convert_assistant_content(content: &[ContentBlock], model: &BedrockModel) -> Vec<Value> {
    let mut content_blocks: Vec<Value> = Vec::new();
    for c in content {
        match c {
            ContentBlock::Text { text, .. } => {
                if let Some(block) = create_non_blank_text_block(text) {
                    content_blocks.push(block);
                }
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                content_blocks.push(json!({
                    "toolUse": { "toolUseId": id, "name": name, "input": arguments }
                }));
            }
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                ..
            } => {
                let thinking = sanitize_surrogates(thinking);
                if thinking.trim().is_empty() {
                    continue;
                }
                if supports_thinking_signature(model) {
                    // Signatures arrive after thinking deltas. If a partial or
                    // externally persisted message lacks a signature, Bedrock
                    // rejects the replayed reasoning block. Fall back to plain
                    // text, matching Anthropic.
                    let has_signature = thinking_signature
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty());
                    if !has_signature {
                        content_blocks.push(json!({ "text": thinking }));
                    } else {
                        content_blocks.push(json!({
                            "reasoningContent": {
                                "reasoningText": {
                                    "text": thinking,
                                    "signature": thinking_signature,
                                }
                            }
                        }));
                    }
                } else {
                    content_blocks.push(json!({
                        "reasoningContent": { "reasoningText": { "text": thinking } }
                    }));
                }
            }
            _ => {}
        }
    }
    content_blocks
}

// ---------------------------------------------------------------------------
// Tool config (`convertToolConfig`, `bedrock-converse-stream.ts:908`)
// ---------------------------------------------------------------------------

/// `convertToolConfig` (`bedrock-converse-stream.ts:908`).
pub fn convert_tool_config(
    tools: Option<&[Value]>,
    tool_choice: Option<&BedrockToolChoice>,
) -> Option<Value> {
    let tools = tools.filter(|t| !t.is_empty())?;
    if matches!(tool_choice, Some(BedrockToolChoice::None)) {
        return None;
    }

    let bedrock_tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "toolSpec": {
                    "name": tool.get("name"),
                    "description": tool.get("description"),
                    "inputSchema": { "json": tool.get("parameters") },
                }
            })
        })
        .collect();

    let bedrock_tool_choice: Option<Value> = match tool_choice {
        Some(BedrockToolChoice::Auto) => Some(json!({ "auto": {} })),
        Some(BedrockToolChoice::Any) => Some(json!({ "any": {} })),
        Some(BedrockToolChoice::Tool { name }) => Some(json!({ "tool": { "name": name } })),
        _ => None,
    };

    let mut config = Map::new();
    config.insert("tools".to_string(), Value::Array(bedrock_tools));
    if let Some(choice) = bedrock_tool_choice {
        config.insert("toolChoice".to_string(), choice);
    }
    Some(Value::Object(config))
}

// ---------------------------------------------------------------------------
// GovCloud detection & thinking payload (`bedrock-converse-stream.ts:1004`, :1014)
// ---------------------------------------------------------------------------

fn get_configured_bedrock_region(
    options: &BedrockOptions,
    process_env: &ProviderEnv,
) -> Option<String> {
    if let Some(region) = &options.region {
        if !region.is_empty() {
            return Some(region.clone());
        }
    }
    env_value("AWS_REGION", options.env.as_ref(), process_env)
        .or_else(|| env_value("AWS_DEFAULT_REGION", options.env.as_ref(), process_env))
}

fn is_gov_cloud_bedrock_target(
    model: &BedrockModel,
    options: &BedrockOptions,
    process_env: &ProviderEnv,
) -> bool {
    if let Some(region) = get_configured_bedrock_region(options, process_env) {
        if region.to_lowercase().starts_with("us-gov-") {
            return true;
        }
    }
    let model_id = model.id.to_lowercase();
    model_id.starts_with("us-gov.") || model_id.starts_with("arn:aws-us-gov:")
}

// ---------------------------------------------------------------------------
// streamSimple support (`bedrock-converse-stream.ts:392`, `simple-options.ts`)
// ---------------------------------------------------------------------------

/// pi's `clampMaxTokensToContext` (`simple-options.ts:15`) for a [`BedrockModel`],
/// delegating to the shared window-keyed core (`api/anthropic/simple_options.rs`)
/// so pi's single helper stays a single implementation across dialects.
pub(crate) fn clamp_max_tokens_to_context(
    model: &BedrockModel,
    context: &Context,
    max_tokens: u64,
) -> u64 {
    clamp_max_tokens_to_context_window(model.context_window, context, max_tokens)
}

/// Project the Bedrock per-level budget map onto the struct shape pi's shared
/// `adjustMaxTokensForThinking` (`simple-options.ts:50`) reads, so the Bedrock
/// `streamSimple` port can reuse the Anthropic-hosted helper unchanged. Only the
/// token-based levels (through `high`) carry a budget; `xhigh`/`max` collapse to
/// `high` before the lookup (`clampReasoning`).
pub(crate) fn to_adjust_budgets(budgets: &ThinkingBudgets) -> AdjustThinkingBudgets {
    AdjustThinkingBudgets {
        minimal: budgets.get(&ThinkingLevel::Minimal).copied(),
        low: budgets.get(&ThinkingLevel::Low).copied(),
        medium: budgets.get(&ThinkingLevel::Medium).copied(),
        high: budgets.get(&ThinkingLevel::High).copied(),
    }
}

/// `buildAdditionalModelRequestFields` (`bedrock-converse-stream.ts:1014`).
pub fn build_additional_model_request_fields(
    model: &BedrockModel,
    options: &BedrockOptions,
    process_env: &ProviderEnv,
) -> Option<Value> {
    let reasoning = options.reasoning?;
    if !model.reasoning {
        return None;
    }

    if !is_anthropic_claude_model(model) {
        return None;
    }

    // GovCloud Bedrock rejects the Claude thinking.display field.
    let display: Option<&str> = if is_gov_cloud_bedrock_target(model, options, process_env) {
        None
    } else {
        Some(
            options
                .thinking_display
                .unwrap_or(BedrockThinkingDisplay::Summarized)
                .as_str(),
        )
    };

    let adaptive = supports_adaptive_thinking(&model.id, model.name_ref());
    let mut result = Map::new();

    if adaptive {
        let mut thinking = Map::new();
        thinking.insert("type".to_string(), json!("adaptive"));
        if let Some(display) = display {
            thinking.insert("display".to_string(), json!(display));
        }
        result.insert("thinking".to_string(), Value::Object(thinking));
        result.insert(
            "output_config".to_string(),
            json!({ "effort": map_thinking_level_to_effort(model, Some(reasoning)) }),
        );
    } else {
        let default_budget = match reasoning {
            ThinkingLevel::Minimal => 1024,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            // Budget-based Claude clamps extended levels to high.
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 16384,
        };
        // Custom budgets only cover token-based levels through high.
        let level = match reasoning {
            ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
            other => other,
        };
        let budget = options
            .thinking_budgets
            .as_ref()
            .and_then(|b| b.get(&level))
            .copied()
            .unwrap_or(default_budget);

        let mut thinking = Map::new();
        thinking.insert("type".to_string(), json!("enabled"));
        thinking.insert("budget_tokens".to_string(), json!(budget));
        if let Some(display) = display {
            thinking.insert("display".to_string(), json!(display));
        }
        result.insert("thinking".to_string(), Value::Object(thinking));
    }

    if !adaptive && options.interleaved_thinking.unwrap_or(true) {
        result.insert(
            "anthropic_beta".to_string(),
            json!([INTERLEAVED_THINKING_BETA]),
        );
    }

    Some(Value::Object(result))
}

// ---------------------------------------------------------------------------
// Command input (`bedrock-converse-stream.ts:223`)
// ---------------------------------------------------------------------------

/// Build the `ConverseStream` command input pi passes to `client.send`
/// (`bedrock-converse-stream.ts:223`). This is what pi exposes to `onPayload`.
pub fn build_command_input(
    model: &BedrockModel,
    context: &Context,
    options: &BedrockOptions,
    process_env: &ProviderEnv,
) -> Value {
    let scoped = options.env.as_ref();
    let cache_retention = resolve_cache_retention(options.cache_retention, scoped, process_env);

    let inference_max_tokens = options.max_tokens.or_else(|| {
        if is_anthropic_claude_model(model) {
            Some(model.max_tokens)
        } else {
            None
        }
    });

    let mut inference_config = Map::new();
    if let Some(max_tokens) = inference_max_tokens {
        inference_config.insert("maxTokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = options.temperature {
        inference_config.insert("temperature".to_string(), json!(temperature));
    }

    let mut command = Map::new();
    command.insert("modelId".to_string(), json!(model.id));
    command.insert(
        "messages".to_string(),
        Value::Array(convert_messages(
            context,
            model,
            cache_retention,
            scoped,
            process_env,
        )),
    );
    if let Some(system) = build_system_prompt(
        context.system_prompt.as_deref(),
        model,
        cache_retention,
        scoped,
        process_env,
    ) {
        command.insert("system".to_string(), Value::Array(system));
    }
    command.insert(
        "inferenceConfig".to_string(),
        Value::Object(inference_config),
    );
    if let Some(tool_config) =
        convert_tool_config(context.tools.as_deref(), options.tool_choice.as_ref())
    {
        command.insert("toolConfig".to_string(), tool_config);
    }
    if let Some(fields) = build_additional_model_request_fields(model, options, process_env) {
        command.insert("additionalModelRequestFields".to_string(), fields);
    }
    if let Some(request_metadata) = &options.request_metadata {
        command.insert(
            "requestMetadata".to_string(),
            json!(request_metadata
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect::<Map<String, Value>>()),
        );
    }

    Value::Object(command)
}

// ---------------------------------------------------------------------------
// Client config / endpoint resolution (`bedrock-converse-stream.ts:133`)
// ---------------------------------------------------------------------------

/// `getStandardBedrockEndpointRegion` (`bedrock-converse-stream.ts:977`).
fn get_standard_bedrock_endpoint_region(base_url: Option<&str>) -> Option<String> {
    let base_url = base_url?;
    let parsed = url::Url::parse(base_url).ok()?;
    let hostname = parsed.host_str()?.to_lowercase();
    let re = Regex::new(r"^bedrock-runtime(?:-fips)?\.([a-z0-9-]+)\.amazonaws\.com(?:\.cn)?$")
        .expect("valid endpoint regex");
    re.captures(&hostname)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// `shouldUseExplicitBedrockEndpoint` (`bedrock-converse-stream.ts:991`).
fn should_use_explicit_bedrock_endpoint(
    base_url: Option<&str>,
    configured_region: Option<&str>,
    has_ambient_configured_profile: bool,
) -> bool {
    let endpoint_region = get_standard_bedrock_endpoint_region(base_url);
    if endpoint_region.is_none() {
        return true;
    }
    configured_region.is_none() && !has_ambient_configured_profile
}

/// `getConfiguredBedrockCredentials` (`bedrock-converse-stream.ts:963`).
fn get_configured_bedrock_credentials(
    scoped: Option<&ProviderEnv>,
    process_env: &ProviderEnv,
) -> Option<Value> {
    let access_key_id = env_value("AWS_ACCESS_KEY_ID", scoped, process_env)?;
    let secret_access_key = env_value("AWS_SECRET_ACCESS_KEY", scoped, process_env)?;
    let mut creds = Map::new();
    creds.insert("accessKeyId".to_string(), json!(access_key_id));
    creds.insert("secretAccessKey".to_string(), json!(secret_access_key));
    if let Some(session_token) = env_value("AWS_SESSION_TOKEN", scoped, process_env) {
        creds.insert("sessionToken".to_string(), json!(session_token));
    }
    Some(Value::Object(creds))
}

/// Build the SDK client config pi assembles before `new BedrockRuntimeClient`
/// (`bedrock-converse-stream.ts:133`), following the Node.js branch (the only one
/// the tests exercise). See the module divergence note on `process_env` and the
/// omitted `requestHandler`.
pub fn build_client_config(
    model: &BedrockModel,
    options: &BedrockOptions,
    process_env: &ProviderEnv,
) -> Value {
    let scoped = options.env.as_ref();
    let mut config = Map::new();

    // config.profile = options.profile || getProviderEnvValue("AWS_PROFILE", options.env)
    let profile = options
        .profile
        .clone()
        .filter(|p| !p.is_empty())
        .or_else(|| env_value("AWS_PROFILE", scoped, process_env));
    if let Some(profile) = profile {
        config.insert("profile".to_string(), json!(profile));
    }

    let configured_region = get_configured_bedrock_region(options, process_env);
    let has_ambient_configured_profile = ambient_env_value("AWS_PROFILE", process_env).is_some();
    let endpoint_region = get_standard_bedrock_endpoint_region(model.base_url.as_deref());
    let use_explicit_endpoint = should_use_explicit_bedrock_endpoint(
        model.base_url.as_deref(),
        configured_region.as_deref(),
        has_ambient_configured_profile,
    );

    if use_explicit_endpoint {
        if let Some(base_url) = &model.base_url {
            config.insert("endpoint".to_string(), json!(base_url));
        }
    }

    let skip_auth = env_value("AWS_BEDROCK_SKIP_AUTH", scoped, process_env).as_deref() == Some("1");
    let bearer_token = options
        .bearer_token
        .clone()
        .or_else(|| options.api_key.clone())
        .or_else(|| env_value("AWS_BEARER_TOKEN_BEDROCK", scoped, process_env));
    let use_bearer_token = bearer_token.is_some() && !skip_auth;

    // Region resolution: ARN-embedded > explicit option > env vars > default.
    let arn_re = Regex::new(r"^arn:aws(?:-[a-z0-9-]+)?:bedrock:([a-z0-9-]+):")
        .expect("valid ARN region regex");
    let arn_region = arn_re
        .captures(&model.id)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string());

    let region: Option<String> = if let Some(arn_region) = arn_region {
        Some(arn_region)
    } else if let Some(configured_region) = &configured_region {
        Some(configured_region.clone())
    } else if endpoint_region.is_some() && use_explicit_endpoint {
        endpoint_region.clone()
    } else if !has_ambient_configured_profile {
        Some("us-east-1".to_string())
    } else {
        None
    };
    if let Some(region) = region {
        config.insert("region".to_string(), json!(region));
    }

    if skip_auth {
        config.insert(
            "credentials".to_string(),
            json!({ "accessKeyId": "dummy-access-key", "secretAccessKey": "dummy-secret-key" }),
        );
    }
    if !skip_auth {
        if let Some(credentials) = get_configured_bedrock_credentials(scoped, process_env) {
            config.insert("credentials".to_string(), credentials);
        }
    }

    if use_bearer_token {
        // `bearer_token` is Some here because `use_bearer_token` requires it.
        config.insert(
            "token".to_string(),
            json!({ "token": bearer_token.expect("bearer token present") }),
        );
        config.insert(
            "authSchemePreference".to_string(),
            json!(["httpBearerAuth"]),
        );
    }

    Value::Object(config)
}

// ---------------------------------------------------------------------------
// Custom-header middleware (`addCustomHeadersMiddleware`, ...:376)
// ---------------------------------------------------------------------------

/// Whether the driver registers the caller-header middleware for these options
/// (pi only calls `addCustomHeadersMiddleware` when `providerHeadersToRecord`
/// returns a non-empty record, `bedrock-converse-stream.ts:217`).
pub fn custom_headers_record(options: &BedrockOptions) -> Option<BTreeMap<String, String>> {
    provider_headers_to_record(options.headers.as_ref())
}

/// `isReservedHeader` (`bedrock-converse-stream.ts:364`): `x-amz-*`,
/// `authorization`, and `host` are reserved (compared case-insensitively).
fn is_reserved_header(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.starts_with("x-amz-") || lower == "authorization" || lower == "host"
}

/// Apply caller-supplied headers to an outgoing request's header map, mirroring
/// the `build`-step middleware body (`bedrock-converse-stream.ts:377`): reserved
/// SigV4 / auth headers are skipped; all other caller headers override.
pub fn apply_custom_headers(
    request_headers: &mut BTreeMap<String, String>,
    custom_headers: &BTreeMap<String, String>,
) {
    for (key, value) in custom_headers {
        if !is_reserved_header(key) {
            request_headers.insert(key.clone(), value.clone());
        }
    }
}
