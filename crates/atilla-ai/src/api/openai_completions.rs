// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `openai-completions.ts`: the `thinkingFormat` switch arms, the per-provider
// `detectCompat` boolean lattice, and the `convertMessages` role branches are
// walls of near-identical option-shaping by design. The clone detector reads
// these mirrored arms as duplicates; factoring them would distort the
// byte-faithful port, so the repetition is intentional.
//! OpenAI Chat Completions request-shaping + streaming-chunk walker, ported from
//! pi-ai's `packages/ai/src/api/openai-completions.ts` at pinned commit
//! `3da591ab`.
//!
//! This is the wire-dialect core of pi's OpenAI-completions driver split into two
//! eager, non-async halves that mirror the Anthropic module's design:
//!
//! - Request shaping ([`build_params`], [`convert_messages`], [`convert_tools`],
//!   [`detect_compat`]/[`get_compat`]) reproduces pi's `buildParams` /
//!   `convertMessages` / `convertTools` / `detectCompat` / `getCompat`, turning a
//!   [`Context`] + [`OpenAICompletionsOptions`] into the JSON request body the
//!   OpenAI SDK would send.
//! - Response walking ([`parse_sse_chunks`], [`walk_chunks`]) reproduces pi's
//!   `stream()` inner loop (`openai-completions.ts:346-467`): it walks the
//!   already-decoded `ChatCompletionChunk` sequence, coalesces tool-call deltas by
//!   stable stream index (then id), repairs streamed argument JSON, accumulates
//!   usage/cost, and terminates with a `done` event or an `error` event — never a
//!   Rust `Err` once the stream has started, matching pi's contract.
//!
//! HTTP transport, auth, and the session-affinity header plumbing of pi's
//! `stream()`/`createClient` live outside this module; here we take the request
//! inputs and (separately) the decoded chunk stream and reproduce the pure
//! transforms around them.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, CacheControlFormat, CacheRetention,
    ContentBlock, DeferredToolsMode, MaxTokensField, Message, Modality, ModelCost,
    ModelThinkingLevel, OpenAICompletionsCompat, OpenRouterRouting, SessionAffinityFormat,
    StopReason, ThinkingFormat, ThinkingLevel, ThinkingLevelMap, Usage, UsageCost,
    VercelGatewayRouting,
};
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};

/// OpenAI's documented 64-character cap on `prompt_cache_key`
/// (`openai-prompt-cache.ts`).
const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// The minimum slice of a pi `Model<"openai-completions">` this driver needs.
///
/// Deserialized leniently (like [`crate::api::anthropic::AnthropicModel`]) so any
/// additional pi model fields are ignored. `compat` carries the *raw* per-model
/// overrides; [`get_compat`] overlays them on the provider/base-URL auto-detection.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAICompletionsModel {
    pub id: String,
    pub api: String,
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default)]
    pub input: Vec<Modality>,
    pub cost: ModelCost,
    #[serde(default)]
    pub compat: Option<OpenAICompletionsCompat>,
}

/// The request-shaping inputs pi's `OpenAICompletionsOptions` (extending
/// `StreamOptions`) exposes. Only the fields the ported transforms read are
/// modeled; transport/callback tuning is out of scope.
#[derive(Debug, Clone, Default)]
pub struct OpenAICompletionsOptions {
    pub max_tokens: Option<u64>,
    pub reasoning_effort: Option<ThinkingLevel>,
    /// `"auto" | "none" | "required" | { type, function }` — kept opaque and
    /// forwarded verbatim onto `tool_choice`.
    pub tool_choice: Option<Value>,
    pub temperature: Option<f64>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    /// Stand-in for `getProviderEnvValue("PI_CACHE_RETENTION", env)`: when set to
    /// `"long"` it drives [`resolve_cache_retention`] exactly as the env var does.
    pub cache_retention_env: Option<String>,
}

/// The fully-resolved compat settings pi's `ResolvedOpenAICompletionsCompat`
/// carries: every optional field from [`OpenAICompletionsCompat`] collapsed to a
/// concrete value via [`detect_compat`], except `cacheControlFormat` /
/// `deferredToolsMode` which stay optional.
#[derive(Debug, Clone)]
pub struct ResolvedCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub open_router_routing: OpenRouterRouting,
    pub vercel_gateway_routing: VercelGatewayRouting,
    pub chat_template_kwargs: BTreeMap<String, Value>,
    pub zai_tool_stream: bool,
    pub supports_strict_mode: bool,
    pub cache_control_format: Option<CacheControlFormat>,
    pub send_session_affinity_headers: bool,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
}

/// The result of walking an OpenAI-completions chunk stream: the full event
/// sequence and the accumulated final message (what pi's
/// `AssistantMessageEventStream.result()` resolves to). Mirrors
/// [`crate::api::anthropic::StreamOutcome`].
#[derive(Debug, Clone, Serialize)]
pub struct StreamOutcome {
    pub events: Vec<AssistantMessageEvent>,
    pub message: AssistantMessage,
}

// ---------------------------------------------------------------------------
// Compat auto-detection (`openai-completions.ts:1237-1355`)
// ---------------------------------------------------------------------------

/// Auto-detect compat settings from provider name and base URL, mirroring pi's
/// `detectCompat`. Used as the base before [`get_compat`] overlays `model.compat`.
pub fn detect_compat(model: &OpenAICompletionsModel) -> ResolvedCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_openrouter = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");

    let is_non_standard = is_nvidia
        || provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || base_url.contains("deepseek.com")
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    let use_max_tokens = base_url.contains("chutes.ai")
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_deepseek = provider == "deepseek" || base_url.contains("deepseek.com");
    let is_openrouter_developer_role_model =
        is_openrouter && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_control_format = if provider == "openrouter" && model.id.starts_with("anthropic/") {
        Some(CacheControlFormat::Anthropic)
    } else {
        None
    };

    let thinking_format = if is_deepseek {
        ThinkingFormat::Deepseek
    } else if is_zai {
        ThinkingFormat::Zai
    } else if is_together {
        ThinkingFormat::Together
    } else if is_ant_ling {
        ThinkingFormat::AntLing
    } else if is_openrouter {
        ThinkingFormat::Openrouter
    } else {
        ThinkingFormat::Openai
    };

    ResolvedCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_openrouter_developer_role_model
            || (!is_non_standard && !is_openrouter),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format,
        open_router_routing: OpenRouterRouting::default(),
        vercel_gateway_routing: VercelGatewayRouting::default(),
        chat_template_kwargs: BTreeMap::new(),
        zai_tool_stream: false,
        supports_strict_mode: !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia,
        cache_control_format,
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: if is_openrouter {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
    }
}

/// Resolve compat settings: auto-detect, then overlay explicit `model.compat`
/// entries, mirroring pi's `getCompat` (`openai-completions.ts:1326-1355`).
pub fn get_compat(model: &OpenAICompletionsModel) -> ResolvedCompat {
    let detected = detect_compat(model);
    let Some(compat) = &model.compat else {
        return detected;
    };

    ResolvedCompat {
        supports_store: compat.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: compat
            .supports_developer_role
            .unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: compat
            .supports_reasoning_effort
            .unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: compat
            .supports_usage_in_streaming
            .unwrap_or(detected.supports_usage_in_streaming),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: compat
            .requires_tool_result_name
            .unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: compat
            .requires_assistant_after_tool_result
            .unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: compat
            .requires_thinking_as_text
            .unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: compat
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        // pi: `model.compat.openRouterRouting ?? {}` (not `?? detected`).
        open_router_routing: compat.open_router_routing.clone().unwrap_or_default(),
        vercel_gateway_routing: compat
            .vercel_gateway_routing
            .clone()
            .unwrap_or(detected.vercel_gateway_routing),
        chat_template_kwargs: compat
            .chat_template_kwargs
            .clone()
            .unwrap_or(detected.chat_template_kwargs),
        zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        supports_strict_mode: compat
            .supports_strict_mode
            .unwrap_or(detected.supports_strict_mode),
        cache_control_format: compat
            .cache_control_format
            .or(detected.cache_control_format),
        send_session_affinity_headers: compat
            .send_session_affinity_headers
            .unwrap_or(detected.send_session_affinity_headers),
        deferred_tools_mode: compat.deferred_tools_mode.or(detected.deferred_tools_mode),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: compat
            .supports_long_cache_retention
            .unwrap_or(detected.supports_long_cache_retention),
    }
}

/// Resolve the effective cache retention, mirroring pi's `resolveCacheRetention`.
fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&str>,
) -> CacheRetention {
    if let Some(c) = cache_retention {
        return c;
    }
    if env == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

// ---------------------------------------------------------------------------
// Thinking-level map helpers
// ---------------------------------------------------------------------------

fn level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// `model.thinkingLevelMap?.[level]`: `None` = key/map absent (JS `undefined`),
/// `Some(None)` = mapped to `null`, `Some(Some(s))` = mapped to a string.
fn map_lookup(model: &OpenAICompletionsModel, level: ModelThinkingLevel) -> Option<Option<String>> {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|m| m.get(&level).cloned())
}

/// `model.thinkingLevelMap?.[effort] ?? effort` — the mapped string, or the raw
/// effort name when the lookup is `undefined`/`null`.
fn mapped_effort_or(model: &OpenAICompletionsModel, effort: ThinkingLevel) -> String {
    match map_lookup(model, to_model_level(effort)) {
        Some(Some(s)) => s,
        _ => level_str(effort).to_string(),
    }
}

/// `model.thinkingLevelMap?.off !== null` — true unless the `off` key is `null`.
fn off_not_null(model: &OpenAICompletionsModel) -> bool {
    !matches!(map_lookup(model, ModelThinkingLevel::Off), Some(None))
}

/// `model.thinkingLevelMap?.off ?? "none"`.
fn off_mapped_or_none(model: &OpenAICompletionsModel) -> String {
    match map_lookup(model, ModelThinkingLevel::Off) {
        Some(Some(s)) => s,
        _ => "none".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Message / tool helpers (`openai-completions.ts:66-98`)
// ---------------------------------------------------------------------------

/// Whether the conversation contains tool calls or tool results — Anthropic (via
/// proxy) requires the `tools` param present when it does.
fn has_tool_history(messages: &[Message]) -> bool {
    for msg in messages {
        match msg {
            Message::ToolResult(_) => return true,
            Message::Assistant(a) => {
                if a.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
                {
                    return true;
                }
            }
            Message::User(_) => {}
        }
    }
    false
}

/// Tool names that became deferred (kimi mode) via tool-result `addedToolNames`.
fn get_deferred_tool_names(messages: &[Message]) -> HashSet<String> {
    let mut names = HashSet::new();
    for msg in messages {
        if let Message::ToolResult(t) = msg {
            if let Some(added) = &t.added_tool_names {
                for name in added {
                    names.insert(name.clone());
                }
            }
        }
    }
    names
}

/// Look up `tools` by name, preserving the iteration order of `names`.
fn get_tools_by_name(tools: Option<&Vec<Value>>, names: &[String]) -> Vec<Value> {
    let Some(tools) = tools else {
        return Vec::new();
    };
    let by_name: HashMap<&str, &Value> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(|n| (n, t)))
        .collect();
    names
        .iter()
        .filter_map(|n| by_name.get(n.as_str()).map(|t| (*t).clone()))
        .collect()
}

/// Identity stand-in for pi's `sanitizeSurrogates`: Rust `str` is always
/// well-formed UTF-8 and cannot contain unpaired surrogates, so the removal pass
/// is a no-op on any value we can hold.
fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

fn is_encrypted_reasoning_detail(detail: &Value) -> bool {
    detail.get("type").and_then(Value::as_str) == Some("reasoning.encrypted")
        && detail
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && detail
            .get("data")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// transformMessages (`transform-messages.ts`)
// ---------------------------------------------------------------------------

fn normalize_tool_call_id(model: &OpenAICompletionsModel, id: &str) -> String {
    // Pipe-separated IDs from the OpenAI Responses API: keep the call_id part,
    // sanitize to allowed chars, truncate to 40.
    if id.contains('|') {
        let call_id = id.split('|').next().unwrap_or("");
        let sanitized: String = call_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        return sanitized.chars().take(40).collect();
    }
    if model.provider == "openai" {
        if id.chars().count() > 40 {
            return id.chars().take(40).collect();
        }
        return id.to_string();
    }
    id.to_string()
}

fn thinking_text(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Thinking { thinking, .. } => Some(thinking),
        _ => None,
    }
}

/// Port of pi's `transformMessages`: cross-model thinking/text/tool-call
/// normalization plus synthetic tool-result insertion for orphaned tool calls.
fn transform_messages(messages: &[Message], model: &OpenAICompletionsModel) -> Vec<Message> {
    // Downgrade unsupported images (models without image input) to placeholders.
    let supports_image = model.input.contains(&Modality::Image);
    let image_aware: Vec<Message> = messages
        .iter()
        .map(|msg| {
            if supports_image {
                return msg.clone();
            }
            match msg {
                Message::User(u) => {
                    if let crate::types::UserContent::Blocks(blocks) = &u.content {
                        let mut nu = u.clone();
                        nu.content =
                            crate::types::UserContent::Blocks(replace_images_with_placeholder(
                                blocks,
                                "(image omitted: model does not support images)",
                            ));
                        Message::User(nu)
                    } else {
                        msg.clone()
                    }
                }
                Message::ToolResult(t) => {
                    let mut nt = t.clone();
                    nt.content = replace_images_with_placeholder(
                        &t.content,
                        "(tool image omitted: model does not support images)",
                    );
                    Message::ToolResult(nt)
                }
                Message::Assistant(_) => msg.clone(),
            }
        })
        .collect();

    // First pass: normalize assistant content + tool-call ids.
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    let mut transformed: Vec<Message> = Vec::with_capacity(image_aware.len());
    for msg in &image_aware {
        match msg {
            Message::User(_) => transformed.push(msg.clone()),
            Message::ToolResult(t) => {
                let mut nt = t.clone();
                if let Some(normalized) = tool_call_id_map.get(&t.tool_call_id) {
                    if normalized != &t.tool_call_id {
                        nt.tool_call_id = normalized.clone();
                    }
                }
                transformed.push(Message::ToolResult(nt));
            }
            Message::Assistant(a) => {
                let is_same_model =
                    a.provider == model.provider && a.api == model.api && a.model == model.id;
                let mut new_content: Vec<ContentBlock> = Vec::new();
                for block in &a.content {
                    match block {
                        ContentBlock::Thinking {
                            thinking,
                            thinking_signature,
                            redacted,
                        } => {
                            if *redacted == Some(true) {
                                if is_same_model {
                                    new_content.push(block.clone());
                                }
                                continue;
                            }
                            let has_sig =
                                thinking_signature.as_ref().is_some_and(|s| !s.is_empty());
                            if is_same_model && has_sig {
                                new_content.push(block.clone());
                                continue;
                            }
                            if thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_model {
                                new_content.push(block.clone());
                            } else {
                                new_content.push(ContentBlock::Text {
                                    text: thinking.clone(),
                                    text_signature: None,
                                });
                            }
                        }
                        ContentBlock::Text { text, .. } => {
                            if is_same_model {
                                new_content.push(block.clone());
                            } else {
                                new_content.push(ContentBlock::Text {
                                    text: text.clone(),
                                    text_signature: None,
                                });
                            }
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            thought_signature,
                        } => {
                            let mut new_id = id.clone();
                            let mut new_sig = thought_signature.clone();
                            if !is_same_model && thought_signature.is_some() {
                                new_sig = None;
                            }
                            if !is_same_model {
                                let normalized = normalize_tool_call_id(model, id);
                                if normalized != *id {
                                    tool_call_id_map.insert(id.clone(), normalized.clone());
                                    new_id = normalized;
                                }
                            }
                            new_content.push(ContentBlock::ToolCall {
                                id: new_id,
                                name: name.clone(),
                                arguments: arguments.clone(),
                                thought_signature: new_sig,
                            });
                        }
                        other => new_content.push(other.clone()),
                    }
                }
                let mut na = a.clone();
                na.content = new_content;
                transformed.push(Message::Assistant(na));
            }
        }
    }

    // Second pass: insert synthetic empty tool results for orphaned tool calls.
    let mut result: Vec<Message> = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut existing: HashSet<String> = HashSet::new();

    let flush = |result: &mut Vec<Message>,
                 pending: &mut Vec<(String, String)>,
                 existing: &mut HashSet<String>| {
        if !pending.is_empty() {
            for (id, name) in pending.iter() {
                if !existing.contains(id) {
                    result.push(Message::ToolResult(crate::types::ToolResultMessage {
                        role: crate::types::ToolResultRole::ToolResult,
                        tool_call_id: id.clone(),
                        tool_name: name.clone(),
                        content: vec![ContentBlock::Text {
                            text: "No result provided".to_string(),
                            text_signature: None,
                        }],
                        details: None,
                        added_tool_names: None,
                        is_error: true,
                        timestamp: 0,
                    }));
                }
            }
            pending.clear();
            existing.clear();
        }
    };

    for msg in transformed {
        match &msg {
            Message::Assistant(a) => {
                flush(&mut result, &mut pending, &mut existing);
                if matches!(a.stop_reason, StopReason::Error | StopReason::Aborted) {
                    continue;
                }
                let tool_calls: Vec<(String, String)> = a
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending = tool_calls;
                    existing = HashSet::new();
                }
                result.push(msg);
            }
            Message::ToolResult(t) => {
                existing.insert(t.tool_call_id.clone());
                result.push(msg);
            }
            Message::User(_) => {
                flush(&mut result, &mut pending, &mut existing);
                result.push(msg);
            }
        }
    }
    flush(&mut result, &mut pending, &mut existing);

    result
}

fn replace_images_with_placeholder(
    blocks: &[ContentBlock],
    placeholder: &str,
) -> Vec<ContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;
    for block in blocks {
        match block {
            ContentBlock::Image { .. } => {
                if !previous_was_placeholder {
                    result.push(ContentBlock::Text {
                        text: placeholder.to_string(),
                        text_signature: None,
                    });
                }
                previous_was_placeholder = true;
            }
            ContentBlock::Text { text, .. } => {
                previous_was_placeholder = text == placeholder;
                result.push(block.clone());
            }
            other => {
                previous_was_placeholder = false;
                result.push(other.clone());
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// convertMessages (`openai-completions.ts:886-1150`)
// ---------------------------------------------------------------------------

/// Convert an atilla-ai [`Context`] into the OpenAI Chat Completions
/// `messages` array, mirroring pi's `convertMessages`.
pub fn convert_messages(
    model: &OpenAICompletionsModel,
    context: &crate::types::Context,
    compat: &ResolvedCompat,
) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let transformed = transform_messages(&context.messages, model);

    if let Some(system_prompt) = &context.system_prompt {
        let use_developer_role = model.reasoning && compat.supports_developer_role;
        let role = if use_developer_role {
            "developer"
        } else {
            "system"
        };
        params.push(json!({ "role": role, "content": sanitize_surrogates(system_prompt) }));
    }

    let mut last_role: Option<String> = None;
    let mut i = 0usize;
    while i < transformed.len() {
        let msg = &transformed[i];
        let role = message_role(msg);

        // Bridge a user message directly after a tool result when required.
        if compat.requires_assistant_after_tool_result
            && last_role.as_deref() == Some("toolResult")
            && role == "user"
        {
            params.push(
                json!({ "role": "assistant", "content": "I have processed the tool results." }),
            );
        }

        match msg {
            Message::User(u) => match &u.content {
                crate::types::UserContent::Text(text) => {
                    params.push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
                }
                crate::types::UserContent::Blocks(blocks) => {
                    let mut content: Vec<Value> = Vec::new();
                    for item in blocks {
                        match item {
                                ContentBlock::Text { text, .. } => content.push(json!({
                                    "type": "text",
                                    "text": sanitize_surrogates(text),
                                })),
                                ContentBlock::Image { data, mime_type } => content.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{};base64,{}", mime_type, data) },
                                })),
                                _ => {}
                            }
                    }
                    if content.is_empty() {
                        last_role = Some(role.to_string());
                        i += 1;
                        continue;
                    }
                    params.push(json!({ "role": "user", "content": content }));
                }
            },
            Message::Assistant(a) => {
                if let Some(value) = convert_assistant_message(model, compat, a) {
                    params.push(value);
                }
            }
            Message::ToolResult(_) => {
                // Consume the run of consecutive tool results starting at `i`.
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut deferred_tool_names: HashSet<String> = HashSet::new();
                let mut j = i;
                while j < transformed.len() {
                    let Message::ToolResult(tool_msg) = &transformed[j] else {
                        break;
                    };
                    let text_result: String = tool_msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_msg
                        .content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Image { .. }));

                    let tool_result_text = if !text_result.is_empty() {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };

                    let mut tool_result_msg = Map::new();
                    tool_result_msg.insert("role".to_string(), json!("tool"));
                    tool_result_msg.insert(
                        "content".to_string(),
                        json!(sanitize_surrogates(&tool_result_text)),
                    );
                    tool_result_msg
                        .insert("tool_call_id".to_string(), json!(tool_msg.tool_call_id));
                    if compat.requires_tool_result_name && !tool_msg.tool_name.is_empty() {
                        tool_result_msg.insert("name".to_string(), json!(tool_msg.tool_name));
                    }
                    params.push(Value::Object(tool_result_msg));

                    if compat.deferred_tools_mode == Some(DeferredToolsMode::Kimi) {
                        if let Some(added) = &tool_msg.added_tool_names {
                            for name in added {
                                deferred_tool_names.insert(name.clone());
                            }
                        }
                    }

                    if has_images && model.input.contains(&Modality::Image) {
                        for block in &tool_msg.content {
                            if let ContentBlock::Image { data, mime_type } = block {
                                image_blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{};base64,{}", mime_type, data) },
                                }));
                            }
                        }
                    }
                    j += 1;
                }
                i = j - 1;

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({
                            "role": "assistant",
                            "content": "I have processed the tool results.",
                        }));
                    }
                    let mut content = vec![json!({
                        "type": "text",
                        "text": "Attached image(s) from tool result:",
                    })];
                    content.extend(image_blocks);
                    params.push(json!({ "role": "user", "content": content }));
                    last_role = Some("user".to_string());
                } else {
                    last_role = Some("toolResult".to_string());
                }

                if !deferred_tool_names.is_empty() {
                    let mut names: Vec<String> = deferred_tool_names.into_iter().collect();
                    names.sort();
                    let deferred_tools = get_tools_by_name(context.tools.as_ref(), &names);
                    if !deferred_tools.is_empty() {
                        // Kimi accepts a system message carrying tools, omitting content.
                        params.push(json!({
                            "role": "system",
                            "tools": convert_tools(&deferred_tools, compat),
                        }));
                    }
                }
                i += 1;
                continue;
            }
        }

        last_role = Some(role.to_string());
        i += 1;
    }

    params
}

fn message_role(msg: &Message) -> &'static str {
    match msg {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolResult(_) => "toolResult",
    }
}

fn convert_assistant_message(
    model: &OpenAICompletionsModel,
    compat: &ResolvedCompat,
    a: &AssistantMessage,
) -> Option<Value> {
    let mut assistant_map = Map::new();
    assistant_map.insert("role".to_string(), json!("assistant"));
    // Some providers reject null content, use empty string instead.
    let mut content_val: Value = if compat.requires_assistant_after_tool_result {
        json!("")
    } else {
        Value::Null
    };

    let assistant_text_parts: Vec<String> = a
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                Some(sanitize_surrogates(text))
            }
            _ => None,
        })
        .collect();
    let assistant_text: String = assistant_text_parts.concat();
    let text_parts_json: Vec<Value> = assistant_text_parts
        .iter()
        .map(|t| json!({ "type": "text", "text": t }))
        .collect();

    let non_empty_thinking: Vec<&str> = a
        .content
        .iter()
        .filter_map(thinking_text)
        .filter(|t| !t.trim().is_empty())
        .collect();

    if !non_empty_thinking.is_empty() {
        if compat.requires_thinking_as_text {
            let thinking_text = non_empty_thinking
                .iter()
                .map(|t| sanitize_surrogates(t))
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut arr = vec![json!({ "type": "text", "text": thinking_text })];
            arr.extend(text_parts_json.clone());
            content_val = Value::Array(arr);
        } else {
            if !assistant_text.is_empty() {
                content_val = json!(assistant_text);
            }
            // Signature from the first thinking block, remapped for opencode-go.
            let mut signature = a.content.iter().find_map(|b| match b {
                ContentBlock::Thinking {
                    thinking_signature, ..
                } => thinking_signature.clone(),
                _ => None,
            });
            if model.provider == "opencode-go" && signature.as_deref() == Some("reasoning") {
                signature = Some("reasoning_content".to_string());
            }
            if let Some(sig) = signature {
                if !sig.is_empty() {
                    assistant_map.insert(sig, json!(non_empty_thinking.join("\n")));
                }
            }
        }
    } else if !assistant_text.is_empty() {
        content_val = json!(assistant_text);
    }

    let tool_calls: Vec<&ContentBlock> = a
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolCall { .. }))
        .collect();
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        let calls: Vec<Value> = tool_calls
            .iter()
            .map(|b| {
                let ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } = b
                else {
                    unreachable!()
                };
                json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
                    },
                })
            })
            .collect();
        assistant_map.insert("tool_calls".to_string(), Value::Array(calls));

        let reasoning_details: Vec<Value> = tool_calls
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall {
                    thought_signature: Some(sig),
                    ..
                } => parse_json_with_repair(sig).ok(),
                _ => None,
            })
            .collect();
        if !reasoning_details.is_empty() {
            assistant_map.insert(
                "reasoning_details".to_string(),
                Value::Array(reasoning_details),
            );
        }
    }

    if compat.requires_reasoning_content_on_assistant_messages
        && model.reasoning
        && !assistant_map.contains_key("reasoning_content")
    {
        assistant_map.insert("reasoning_content".to_string(), json!(""));
    }

    let has_content = match &content_val {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => false,
    };
    if !has_content && !has_tool_calls {
        return None;
    }

    assistant_map.insert("content".to_string(), content_val);
    Some(Value::Object(assistant_map))
}

// ---------------------------------------------------------------------------
// convertTools (`openai-completions.ts:1152-1166`)
// ---------------------------------------------------------------------------

/// Convert atilla-ai tools to the OpenAI `{type:"function", function:{...}}`
/// shape. `strict:false` is included only when the provider supports strict mode;
/// otherwise the field is omitted entirely (some providers reject unknown fields).
pub fn convert_tools(tools: &[Value], compat: &ResolvedCompat) -> Vec<Value> {
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
            if let Some(parameters) = tool.get("parameters") {
                function.insert("parameters".to_string(), parameters.clone());
            }
            if compat.supports_strict_mode {
                function.insert("strict".to_string(), json!(false));
            }
            json!({ "type": "function", "function": Value::Object(function) })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// buildParams (`openai-completions.ts:575-731`)
// ---------------------------------------------------------------------------

/// Clamp a `prompt_cache_key` to OpenAI's 64-character limit (by Unicode scalar).
fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_string());
    }
    Some(
        key.chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect(),
    )
}

/// The Anthropic-style cache-control marker to stamp on the request, if any.
fn get_compat_cache_control(
    compat: &ResolvedCompat,
    cache_retention: CacheRetention,
) -> Option<Value> {
    if compat.cache_control_format != Some(CacheControlFormat::Anthropic)
        || cache_retention == CacheRetention::None
    {
        return None;
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        Some(json!({ "type": "ephemeral", "ttl": "1h" }))
    } else {
        Some(json!({ "type": "ephemeral" }))
    }
}

/// Build the full OpenAI Chat Completions request body, mirroring pi's
/// `buildParams`.
pub fn build_params(
    model: &OpenAICompletionsModel,
    context: &crate::types::Context,
    options: &OpenAICompletionsOptions,
) -> Value {
    let compat = get_compat(model);
    let cache_retention = resolve_cache_retention(
        options.cache_retention,
        options.cache_retention_env.as_deref(),
    );
    let mut messages = convert_messages(model, context, &compat);
    let cache_control = get_compat_cache_control(&compat, cache_retention);

    // Tools: active (non-deferred) tools, else `[]` when there is tool history.
    let deferred = if compat.deferred_tools_mode == Some(DeferredToolsMode::Kimi) {
        get_deferred_tool_names(&context.messages)
    } else {
        HashSet::new()
    };
    let active_tools: Vec<Value> = context
        .tools
        .iter()
        .flatten()
        .filter(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .map(|n| !deferred.contains(n))
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    let mut tools: Option<Vec<Value>> = None;
    let mut tool_stream = false;
    if !active_tools.is_empty() {
        tools = Some(convert_tools(&active_tools, &compat));
        if compat.zai_tool_stream {
            tool_stream = true;
        }
    } else if has_tool_history(&context.messages) {
        tools = Some(Vec::new());
    }

    if let Some(cc) = &cache_control {
        apply_anthropic_cache_control(&mut messages, tools.as_mut(), cc);
    }

    let mut params = Map::new();
    params.insert("model".to_string(), json!(model.id));
    params.insert("messages".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), json!(true));

    // prompt_cache_key / prompt_cache_retention.
    let cache_key_applies = (model.base_url.contains("api.openai.com")
        && cache_retention != CacheRetention::None)
        || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention);
    if cache_key_applies {
        if let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref()) {
            params.insert("prompt_cache_key".to_string(), json!(key));
        }
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".to_string(), json!("24h"));
    }

    if compat.supports_usage_in_streaming {
        params.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }
    if compat.supports_store {
        params.insert("store".to_string(), json!(false));
    }
    if let Some(mt) = options.max_tokens {
        if compat.max_tokens_field == MaxTokensField::MaxTokens {
            params.insert("max_tokens".to_string(), json!(mt));
        } else {
            params.insert("max_completion_tokens".to_string(), json!(mt));
        }
    }
    if let Some(temp) = options.temperature {
        params.insert("temperature".to_string(), json!(temp));
    }
    if let Some(tools) = tools {
        params.insert("tools".to_string(), Value::Array(tools));
    }
    if tool_stream {
        params.insert("tool_stream".to_string(), json!(true));
    }
    if let Some(tc) = &options.tool_choice {
        params.insert("tool_choice".to_string(), tc.clone());
    }

    apply_thinking_format(&mut params, model, options, &compat);

    // Provider routing preferences come from the *raw* model.compat.
    if let Some(raw) = &model.compat {
        if let Some(routing) = &raw.open_router_routing {
            params.insert(
                "provider".to_string(),
                serde_json::to_value(routing).unwrap_or(Value::Null),
            );
        }
        if let Some(routing) = &raw.vercel_gateway_routing {
            if routing.only.is_some() || routing.order.is_some() {
                let mut gateway = Map::new();
                if let Some(only) = &routing.only {
                    gateway.insert("only".to_string(), json!(only));
                }
                if let Some(order) = &routing.order {
                    gateway.insert("order".to_string(), json!(order));
                }
                params.insert(
                    "providerOptions".to_string(),
                    json!({ "gateway": Value::Object(gateway) }),
                );
            }
        }
    }

    Value::Object(params)
}

/// Apply pi's `thinkingFormat` switch (`openai-completions.ts:638-712`), mutating
/// `params` with the provider-appropriate reasoning parameters.
fn apply_thinking_format(
    params: &mut Map<String, Value>,
    model: &OpenAICompletionsModel,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedCompat,
) {
    let effort = options.reasoning_effort;
    let has_effort = effort.is_some();
    let reasoning = model.reasoning;
    let tf = compat.thinking_format;

    if tf == ThinkingFormat::Zai && reasoning {
        params.insert(
            "thinking".to_string(),
            if has_effort {
                json!({ "type": "enabled", "clear_thinking": false })
            } else {
                json!({ "type": "disabled" })
            },
        );
        if has_effort && compat.supports_reasoning_effort {
            match map_lookup(model, to_model_level(effort.unwrap())) {
                None => {
                    params.insert(
                        "reasoning_effort".to_string(),
                        json!(level_str(effort.unwrap())),
                    );
                }
                Some(Some(s)) => {
                    params.insert("reasoning_effort".to_string(), json!(s));
                }
                Some(None) => {}
            }
        }
    } else if tf == ThinkingFormat::Qwen && reasoning {
        params.insert("enable_thinking".to_string(), json!(has_effort));
    } else if tf == ThinkingFormat::QwenChatTemplate && reasoning {
        params.insert(
            "chat_template_kwargs".to_string(),
            json!({ "enable_thinking": has_effort, "preserve_thinking": true }),
        );
    } else if tf == ThinkingFormat::ChatTemplate && reasoning {
        if let Some(kwargs) = build_chat_template_kwargs(model, options, compat) {
            params.insert("chat_template_kwargs".to_string(), kwargs);
        }
    } else if tf == ThinkingFormat::Deepseek && reasoning {
        if has_effort {
            params.insert("thinking".to_string(), json!({ "type": "enabled" }));
        } else if off_not_null(model) {
            params.insert("thinking".to_string(), json!({ "type": "disabled" }));
        }
        if has_effort && compat.supports_reasoning_effort {
            params.insert(
                "reasoning_effort".to_string(),
                json!(mapped_effort_or(model, effort.unwrap())),
            );
        }
    } else if tf == ThinkingFormat::Openrouter && reasoning {
        if has_effort {
            params.insert(
                "reasoning".to_string(),
                json!({ "effort": mapped_effort_or(model, effort.unwrap()) }),
            );
        } else if off_not_null(model) {
            params.insert(
                "reasoning".to_string(),
                json!({ "effort": off_mapped_or_none(model) }),
            );
        }
    } else if tf == ThinkingFormat::AntLing && reasoning && has_effort {
        if let Some(Some(s)) = map_lookup(model, to_model_level(effort.unwrap())) {
            params.insert("reasoning".to_string(), json!({ "effort": s }));
        }
    } else if tf == ThinkingFormat::Together && reasoning {
        params.insert("reasoning".to_string(), json!({ "enabled": has_effort }));
        if has_effort && compat.supports_reasoning_effort {
            params.insert(
                "reasoning_effort".to_string(),
                json!(mapped_effort_or(model, effort.unwrap())),
            );
        }
    } else if tf == ThinkingFormat::StringThinking && reasoning {
        if has_effort {
            params.insert(
                "thinking".to_string(),
                json!(mapped_effort_or(model, effort.unwrap())),
            );
        } else if off_not_null(model) {
            params.insert("thinking".to_string(), json!(off_mapped_or_none(model)));
        }
    } else if has_effort && reasoning && compat.supports_reasoning_effort {
        // OpenAI-style reasoning_effort.
        params.insert(
            "reasoning_effort".to_string(),
            json!(mapped_effort_or(model, effort.unwrap())),
        );
    } else if !has_effort && reasoning && compat.supports_reasoning_effort {
        if let Some(Some(off)) = map_lookup(model, ModelThinkingLevel::Off) {
            params.insert("reasoning_effort".to_string(), json!(off));
        }
    }
}

/// Build `chat_template_kwargs` for the `chat-template` thinking format, mirroring
/// pi's `buildChatTemplateKwargs`.
fn build_chat_template_kwargs(
    model: &OpenAICompletionsModel,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedCompat,
) -> Option<Value> {
    let mut kwargs = Map::new();
    for (key, value) in &compat.chat_template_kwargs {
        if let Some(resolved) = resolve_chat_template_kwarg_value(model, options, value) {
            kwargs.insert(key.clone(), resolved);
        }
    }
    if kwargs.is_empty() {
        None
    } else {
        Some(Value::Object(kwargs))
    }
}

/// Resolve a single `chat_template_kwargs` value, mirroring pi's
/// `resolveChatTemplateKwargValue`.
fn resolve_chat_template_kwarg_value(
    model: &OpenAICompletionsModel,
    options: &OpenAICompletionsOptions,
    value: &Value,
) -> Option<Value> {
    let Some(obj) = value.as_object() else {
        // Primitives and JSON `null` pass through unchanged.
        return Some(value.clone());
    };

    let effort = options.reasoning_effort;
    let omit_when_off = obj
        .get("omitWhenOff")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if effort.is_none() && omit_when_off {
        return None;
    }
    if obj.get("$var").and_then(Value::as_str) == Some("thinking.enabled") {
        return Some(Value::Bool(effort.is_some()));
    }

    let mapped = match effort {
        Some(l) => map_lookup(model, to_model_level(l)),
        None => map_lookup(model, ModelThinkingLevel::Off),
    };
    match mapped {
        None => effort.map(|l| json!(level_str(l))),
        Some(Some(s)) => Some(json!(s)),
        Some(None) => None,
    }
}

// ---------------------------------------------------------------------------
// Anthropic cache-control markers (`openai-completions.ts:783-884`)
// ---------------------------------------------------------------------------

fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Vec<Value>>,
    cache_control: &Value,
) {
    add_cache_control_to_system_prompt(messages, cache_control);
    if let Some(tools) = tools {
        if let Some(last) = tools.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".to_string(), cache_control.clone());
            }
        }
    }
    add_cache_control_to_last_conversation_message(messages, cache_control);
}

fn add_cache_control_to_system_prompt(messages: &mut [Value], cache_control: &Value) {
    for message in messages.iter_mut() {
        let role = message.get("role").and_then(Value::as_str);
        if role == Some("system") || role == Some("developer") {
            add_cache_control_to_text_content(message, cache_control);
            return;
        }
    }
}

fn add_cache_control_to_last_conversation_message(messages: &mut [Value], cache_control: &Value) {
    for message in messages.iter_mut().rev() {
        let role = message.get("role").and_then(Value::as_str);
        if (role == Some("user") || role == Some("assistant"))
            && add_cache_control_to_text_content(message, cache_control)
        {
            return;
        }
    }
}

fn add_cache_control_to_text_content(message: &mut Value, cache_control: &Value) -> bool {
    let Some(obj) = message.as_object_mut() else {
        return false;
    };
    match obj.get("content") {
        Some(Value::String(s)) => {
            if s.is_empty() {
                return false;
            }
            let text = s.clone();
            obj.insert(
                "content".to_string(),
                json!([{ "type": "text", "text": text, "cache_control": cache_control }]),
            );
            true
        }
        Some(Value::Array(_)) => {
            let Some(Value::Array(parts)) = obj.get_mut("content") else {
                return false;
            };
            for part in parts.iter_mut().rev() {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(part_obj) = part.as_object_mut() {
                        part_obj.insert("cache_control".to_string(), cache_control.clone());
                    }
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// SSE chunk decode (`data:`-framed wire path)
// ---------------------------------------------------------------------------

/// Split a raw `text/event-stream` body into decoded `ChatCompletionChunk` JSON
/// values: take every `data: ` payload, stop at the `data: [DONE]` sentinel, and
/// JSON-parse (with repair) each chunk. This is the unnamed-SSE chunk protocol,
/// kept separate from [`walk_chunks`].
pub fn parse_sse_chunks(body: &str) -> Vec<Value> {
    let mut chunks = Vec::new();
    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = rest.strip_prefix(' ').unwrap_or(rest);
        if payload == "[DONE]" {
            break;
        }
        if let Ok(value) = parse_json_with_repair(payload) {
            chunks.push(value);
        }
    }
    chunks
}

// ---------------------------------------------------------------------------
// Usage & stop-reason mapping (`openai-completions.ts:1168-1230`)
// ---------------------------------------------------------------------------

fn usage_number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Map an OpenAI chunk `usage` object to atilla-ai [`Usage`] + cost, mirroring
/// pi's `parseChunkUsage`.
pub fn parse_chunk_usage(raw_usage: &Value, cost: &ModelCost) -> Usage {
    let prompt_tokens = usage_number(raw_usage, "prompt_tokens") as i64;
    let details = raw_usage.get("prompt_tokens_details");
    let cache_read_tokens = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            raw_usage
                .get("prompt_cache_hit_tokens")
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    let cache_write_tokens = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // Do not subtract writes from cached_tokens; clamp the derived input at 0.
    let input =
        (prompt_tokens - cache_read_tokens as i64 - cache_write_tokens as i64).max(0) as u64;
    let output_tokens = usage_number(raw_usage, "completion_tokens");
    let reasoning = raw_usage
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut usage = Usage {
        input,
        output: output_tokens,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        reasoning: Some(reasoning),
        total_tokens: input + output_tokens + cache_read_tokens + cache_write_tokens,
        cost: UsageCost::default(),
    };
    usage.cost = calculate_cost_with(cost, &usage);
    usage
}

/// Map an OpenAI `finish_reason` to a stop reason + optional error message,
/// mirroring pi's `mapStopReason`.
pub fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    match reason {
        None => (StopReason::Stop, None),
        Some("stop") | Some("end") => (StopReason::Stop, None),
        Some("length") => (StopReason::Length, None),
        Some("function_call") | Some("tool_calls") => (StopReason::ToolUse, None),
        Some(other) => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// Chunk walker (`openai-completions.ts:346-467`)
// ---------------------------------------------------------------------------

/// A content block under construction, tracking the growing tool-argument buffer
/// and the provider's stream index (pi's `StreamingToolCallBlock` scratch state).
#[derive(Debug, Clone)]
struct WorkingBlock {
    block: ContentBlock,
    partial_args: String,
    stream_index: Option<i64>,
}

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

fn render_content(blocks: &[WorkingBlock]) -> Vec<ContentBlock> {
    blocks.iter().map(|b| b.block.clone()).collect()
}

fn render_partial(output: &AssistantMessage, blocks: &[WorkingBlock]) -> AssistantMessage {
    let mut partial = output.clone();
    partial.content = render_content(blocks);
    partial
}

/// Walk the decoded OpenAI-completions chunk stream into the uniform event stream
/// and final message, reproducing pi's `stream()` inner loop.
///
/// Terminates with a `done` event on success, or an `error` event (stop reason
/// [`StopReason::Error`]) when the stream reports an error/aborted stop or ends
/// without a `finish_reason` — never a Rust `Err`, matching pi's contract.
pub fn walk_chunks(
    chunks: &[Value],
    model: &OpenAICompletionsModel,
    _options: &OpenAICompletionsOptions,
    timestamp: i64,
) -> StreamOutcome {
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
    let mut blocks: Vec<WorkingBlock> = Vec::new();
    let mut events: Vec<AssistantMessageEvent> = Vec::new();

    let mut text_pos: Option<usize> = None;
    let mut thinking_pos: Option<usize> = None;
    let mut tool_by_index: HashMap<i64, usize> = HashMap::new();
    let mut tool_by_id: HashMap<String, usize> = HashMap::new();
    let mut pending_reasoning: HashMap<String, String> = HashMap::new();
    let mut has_finish_reason = false;

    events.push(AssistantMessageEvent::Start {
        partial: render_partial(&output, &blocks),
    });

    for chunk in chunks {
        if !chunk.is_object() {
            continue;
        }

        // responseId ||= chunk.id
        if output.response_id.as_deref().unwrap_or("").is_empty() {
            if let Some(id) = chunk.get("id").and_then(Value::as_str) {
                output.response_id = Some(id.to_string());
            }
        }
        // responseModel ||= chunk.model when it differs from the requested id.
        if output.response_model.as_deref().unwrap_or("").is_empty() {
            if let Some(chunk_model) = chunk.get("model").and_then(Value::as_str) {
                if !chunk_model.is_empty() && chunk_model != model.id {
                    output.response_model = Some(chunk_model.to_string());
                }
            }
        }
        let chunk_has_usage = chunk.get("usage").is_some_and(|u| !u.is_null());
        if chunk_has_usage {
            output.usage = parse_chunk_usage(chunk.get("usage").unwrap(), &model.cost);
        }

        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first());
        let Some(choice) = choice.filter(|c| !c.is_null()) else {
            continue;
        };

        // Fallback: some providers return usage in choice.usage.
        if !chunk_has_usage {
            if let Some(choice_usage) = choice.get("usage").filter(|u| !u.is_null()) {
                output.usage = parse_chunk_usage(choice_usage, &model.cost);
            }
        }

        if let Some(finish_reason) = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let (stop_reason, error_message) = map_stop_reason(Some(finish_reason));
            output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                output.error_message = Some(error_message);
            }
            has_finish_reason = true;
        }

        let Some(delta) = choice.get("delta").filter(|d| !d.is_null()) else {
            continue;
        };

        // Text content.
        if let Some(content) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let pos = ensure_text_block(&mut blocks, &mut text_pos, &output, &mut events);
            if let ContentBlock::Text { text, .. } = &mut blocks[pos].block {
                text.push_str(content);
            }
            let partial = render_partial(&output, &blocks);
            events.push(AssistantMessageEvent::TextDelta {
                content_index: pos as u32,
                delta: content.to_string(),
                partial,
            });
        }

        // Thinking / reasoning: first non-empty of the three reasoning fields.
        let reasoning_fields = ["reasoning_content", "reasoning", "reasoning_text"];
        let found = reasoning_fields.into_iter().find(|field| {
            delta
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        });
        if let Some(field) = found {
            let reasoning_delta = delta.get(field).and_then(Value::as_str).unwrap_or("");
            let signature = if model.provider == "opencode-go" && field == "reasoning" {
                "reasoning_content".to_string()
            } else {
                field.to_string()
            };
            let pos = ensure_thinking_block(
                &mut blocks,
                &mut thinking_pos,
                &signature,
                &output,
                &mut events,
            );
            if let ContentBlock::Thinking { thinking, .. } = &mut blocks[pos].block {
                thinking.push_str(reasoning_delta);
            }
            let partial = render_partial(&output, &blocks);
            events.push(AssistantMessageEvent::ThinkingDelta {
                content_index: pos as u32,
                delta: reasoning_delta.to_string(),
                partial,
            });
        }

        // Tool calls.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let pos = ensure_tool_call_block(
                    tc,
                    &mut blocks,
                    &mut tool_by_index,
                    &mut tool_by_id,
                    &mut pending_reasoning,
                    &output,
                    &mut events,
                );

                // Backfill id/name from later deltas.
                let tc_id = tc
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let tc_name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                if let ContentBlock::ToolCall { id, name, .. } = &mut blocks[pos].block {
                    if id.is_empty() {
                        if let Some(new_id) = tc_id {
                            *id = new_id.to_string();
                            tool_by_id.insert(new_id.to_string(), pos);
                        }
                    }
                    if name.is_empty() {
                        if let Some(new_name) = tc_name {
                            *name = new_name.to_string();
                        }
                    }
                }

                let mut delta_str = String::new();
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    delta_str = args.to_string();
                    blocks[pos].partial_args.push_str(args);
                    let parsed = parse_streaming_json(Some(&blocks[pos].partial_args));
                    if let ContentBlock::ToolCall { arguments, .. } = &mut blocks[pos].block {
                        *arguments = parsed;
                    }
                }
                let partial = render_partial(&output, &blocks);
                events.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: pos as u32,
                    delta: delta_str,
                    partial,
                });
            }
        }

        // Encrypted reasoning details (thoughtSignature carriers).
        if let Some(reasoning_details) = delta.get("reasoning_details").and_then(Value::as_array) {
            for detail in reasoning_details {
                if is_encrypted_reasoning_detail(detail) {
                    let serialized = serde_json::to_string(detail).unwrap_or_default();
                    let id = detail.get("id").and_then(Value::as_str).unwrap_or("");
                    if let Some(&pos) = tool_by_id.get(id) {
                        if let ContentBlock::ToolCall {
                            thought_signature, ..
                        } = &mut blocks[pos].block
                        {
                            *thought_signature = Some(serialized);
                        }
                    } else {
                        pending_reasoning.insert(id.to_string(), serialized);
                    }
                }
            }
        }
    }

    // Finalize each block, emitting its terminal event.
    for pos in 0..blocks.len() {
        match &blocks[pos].block {
            ContentBlock::Text { text, .. } => {
                let content = text.clone();
                let partial = render_partial(&output, &blocks);
                events.push(AssistantMessageEvent::TextEnd {
                    content_index: pos as u32,
                    content,
                    partial,
                });
            }
            ContentBlock::Thinking { thinking, .. } => {
                let content = thinking.clone();
                let partial = render_partial(&output, &blocks);
                events.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: pos as u32,
                    content,
                    partial,
                });
            }
            ContentBlock::ToolCall { .. } => {
                let parsed = parse_streaming_json(Some(&blocks[pos].partial_args));
                if let ContentBlock::ToolCall { arguments, .. } = &mut blocks[pos].block {
                    *arguments = parsed;
                }
                let tool_call = blocks[pos].block.clone();
                let partial = render_partial(&output, &blocks);
                events.push(AssistantMessageEvent::ToolcallEnd {
                    content_index: pos as u32,
                    tool_call,
                    partial,
                });
            }
            ContentBlock::Image { .. } | ContentBlock::Unknown => {}
        }
    }

    output.content = render_content(&blocks);

    // Post-loop guards, matching pi's throw order (no abort signal is modeled).
    let terminal: Option<String> = if output.stop_reason == StopReason::Aborted {
        Some("Request was aborted".to_string())
    } else if output.stop_reason == StopReason::Error {
        Some(
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "Provider returned an error stop reason".to_string()),
        )
    } else if !has_finish_reason {
        Some("Stream ended without finish_reason".to_string())
    } else {
        None
    };

    match terminal {
        None => events.push(AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output.clone(),
        }),
        Some(message) => finish_with_error(&mut output, &mut events, message),
    }

    StreamOutcome {
        events,
        message: output,
    }
}

fn finish_with_error(
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
    message: String,
) {
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message);
    events.push(AssistantMessageEvent::Error {
        reason: output.stop_reason,
        error: output.clone(),
    });
}

fn ensure_text_block(
    blocks: &mut Vec<WorkingBlock>,
    text_pos: &mut Option<usize>,
    output: &AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) -> usize {
    if let Some(pos) = *text_pos {
        return pos;
    }
    blocks.push(WorkingBlock {
        block: ContentBlock::Text {
            text: String::new(),
            text_signature: None,
        },
        partial_args: String::new(),
        stream_index: None,
    });
    let pos = blocks.len() - 1;
    *text_pos = Some(pos);
    let partial = render_partial(output, blocks);
    events.push(AssistantMessageEvent::TextStart {
        content_index: pos as u32,
        partial,
    });
    pos
}

fn ensure_thinking_block(
    blocks: &mut Vec<WorkingBlock>,
    thinking_pos: &mut Option<usize>,
    signature: &str,
    output: &AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) -> usize {
    if let Some(pos) = *thinking_pos {
        return pos;
    }
    blocks.push(WorkingBlock {
        block: ContentBlock::Thinking {
            thinking: String::new(),
            thinking_signature: Some(signature.to_string()),
            redacted: None,
        },
        partial_args: String::new(),
        stream_index: None,
    });
    let pos = blocks.len() - 1;
    *thinking_pos = Some(pos);
    let partial = render_partial(output, blocks);
    events.push(AssistantMessageEvent::ThinkingStart {
        content_index: pos as u32,
        partial,
    });
    pos
}

#[allow(clippy::too_many_arguments)]
fn ensure_tool_call_block(
    tc: &Value,
    blocks: &mut Vec<WorkingBlock>,
    tool_by_index: &mut HashMap<i64, usize>,
    tool_by_id: &mut HashMap<String, usize>,
    pending_reasoning: &mut HashMap<String, String>,
    output: &AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) -> usize {
    let stream_index = tc.get("index").and_then(Value::as_i64);
    let tc_id = tc
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    // Key by stream index first, then id, so id mutation mid-stream still
    // coalesces onto one block.
    let mut pos = stream_index.and_then(|i| tool_by_index.get(&i).copied());
    if pos.is_none() {
        if let Some(id) = tc_id {
            pos = tool_by_id.get(id).copied();
        }
    }

    let pos = match pos {
        Some(pos) => pos,
        None => {
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            blocks.push(WorkingBlock {
                block: ContentBlock::ToolCall {
                    id: id.clone(),
                    name,
                    arguments: json!({}),
                    thought_signature: None,
                },
                partial_args: String::new(),
                stream_index,
            });
            let pos = blocks.len() - 1;
            if let Some(i) = stream_index {
                tool_by_index.insert(i, pos);
            }
            if !id.is_empty() {
                tool_by_id.insert(id, pos);
            }
            let partial = render_partial(output, blocks);
            events.push(AssistantMessageEvent::ToolcallStart {
                content_index: pos as u32,
                partial,
            });
            pos
        }
    };

    if let Some(i) = stream_index {
        if blocks[pos].stream_index.is_none() {
            blocks[pos].stream_index = Some(i);
            tool_by_index.insert(i, pos);
        }
    }
    if let Some(id) = tc_id {
        tool_by_id.insert(id.to_string(), pos);
    }
    apply_pending_reasoning(blocks, pos, pending_reasoning);
    pos
}

fn apply_pending_reasoning(
    blocks: &mut [WorkingBlock],
    pos: usize,
    pending_reasoning: &mut HashMap<String, String>,
) {
    if let ContentBlock::ToolCall {
        id,
        thought_signature,
        ..
    } = &mut blocks[pos].block
    {
        if id.is_empty() {
            return;
        }
        if let Some(sig) = pending_reasoning.remove(id) {
            *thought_signature = Some(sig);
        }
    }
}

// ---------------------------------------------------------------------------
// JSON boundary wrappers (napi entry points)
// ---------------------------------------------------------------------------

/// Build the request body from JSON inputs and return it as a JSON string. The
/// napi shim reads pi's `Model`/`Context`/options, hands them here, and forwards
/// the result to the transport.
pub fn build_params_from_json(
    model_json: &str,
    context_json: &str,
    options: &OpenAICompletionsOptions,
) -> Result<String, String> {
    let model: OpenAICompletionsModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let context: crate::types::Context =
        serde_json::from_str(context_json).map_err(|e| format!("invalid context json: {e}"))?;
    let params = build_params(&model, &context, options);
    serde_json::to_string(&params).map_err(|e| e.to_string())
}

/// Walk an array of decoded chunks (given as a JSON array string) and return the
/// [`StreamOutcome`] as a JSON string.
pub fn walk_chunks_from_json(
    chunks_json: &str,
    model_json: &str,
    options: &OpenAICompletionsOptions,
    timestamp: i64,
) -> Result<String, String> {
    let chunks: Vec<Value> =
        serde_json::from_str(chunks_json).map_err(|e| format!("invalid chunks json: {e}"))?;
    let model: OpenAICompletionsModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let outcome = walk_chunks(&chunks, &model, options, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}

/// Parse a raw SSE `body` for `model_json` and return the [`StreamOutcome`] as a
/// JSON string: [`parse_sse_chunks`] then [`walk_chunks`].
pub fn parse_sse_stream_to_json(
    body: &str,
    model_json: &str,
    options: &OpenAICompletionsOptions,
    timestamp: i64,
) -> Result<String, String> {
    let model: OpenAICompletionsModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let chunks = parse_sse_chunks(body);
    let outcome = walk_chunks(&chunks, &model, options, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
