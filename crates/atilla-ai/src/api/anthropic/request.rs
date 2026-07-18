// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` `buildParams` and its `AnthropicOptions` surface. The
// per-field option struct and the conditional param assembly mirror pi's object
// spreads verbatim; the clone detector may read the serde/option scaffolding as
// duplicative by design.
//! Anthropic Messages request-parameter assembly, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` (`buildParams`,
//! `AnthropicOptions`) at pinned commit `3da591ab`.
//!
//! [`build_params`] is the phase-1 entry point: given a model, a [`Context`],
//! the OAuth flag, and [`AnthropicOptions`], it produces the exact JSON body of
//! a streaming `MessageCreateParamsStreaming` request as a [`serde_json::Value`].

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Map, Value};

use crate::types::{AnthropicMessagesCompat, CacheRetention, Context, Model};

use super::cache::get_cache_control;
use super::compat::get_anthropic_compat;
use super::content::{convert_messages, sanitize_surrogates, transform_messages};
use super::thinking::{AnthropicEffort, AnthropicThinkingDisplay};
use super::tools::{convert_tools, normalize_tool_name, split_deferred_tools};

/// Anthropic tool-choice behavior (`anthropic-messages.ts:252`). String choices
/// map to Anthropic's built-in behaviors; `Tool { name }` forces a specific tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

impl ToolChoice {
    fn to_value(&self) -> Value {
        match self {
            ToolChoice::Auto => json!({ "type": "auto" }),
            ToolChoice::Any => json!({ "type": "any" }),
            ToolChoice::None => json!({ "type": "none" }),
            ToolChoice::Tool { name } => json!({ "type": "tool", "name": name }),
        }
    }
}

/// The subset of pi's `AnthropicOptions` (`anthropic-messages.ts:199`) that
/// `buildParams` reads. Phase 2 will wire the transport/callback fields; here the
/// options carry only what shapes the request body.
#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    /// `StreamOptions.temperature`.
    pub temperature: Option<f64>,
    /// `StreamOptions.maxTokens`.
    pub max_tokens: Option<u64>,
    /// `StreamOptions.cacheRetention`.
    pub cache_retention: Option<CacheRetention>,
    /// `StreamOptions.sessionId` (unused by `buildParams`; kept for parity).
    pub session_id: Option<String>,
    /// `StreamOptions.env` — provider-scoped environment overrides.
    pub env: Option<BTreeMap<String, String>>,
    /// `StreamOptions.metadata`; only a string `user_id` is forwarded.
    pub metadata: Option<Map<String, Value>>,
    /// Enable extended thinking.
    pub thinking_enabled: Option<bool>,
    /// Token budget for budget-based (non-adaptive) thinking.
    pub thinking_budget_tokens: Option<u64>,
    /// Effort level for adaptive-thinking models.
    pub effort: Option<AnthropicEffort>,
    /// How thinking content is returned.
    pub thinking_display: Option<AnthropicThinkingDisplay>,
    /// Anthropic tool-choice behavior.
    pub tool_choice: Option<ToolChoice>,
    /// `StreamOptions.apiKey` — the provider credential. `buildParams` ignores
    /// it; the `stream` driver reads it to pick the auth mode / assemble headers.
    pub api_key: Option<String>,
    /// `StreamOptions.headers` — caller-supplied header overrides merged last by
    /// the `stream` driver's `createClient` port. `buildParams` ignores it.
    pub headers: Option<BTreeMap<String, String>>,
    /// `AnthropicOptions.interleavedThinking` (default `true`). Read by the
    /// `stream` driver to decide the interleaved-thinking beta header;
    /// `buildParams` ignores it.
    pub interleaved_thinking: Option<bool>,
}

/// Read a tool's `name` field from its opaque `Value`.
fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

/// Build the streaming request parameters, mirroring pi's `buildParams`
/// (`anthropic-messages.ts:920`). The returned [`Value`] is the exact JSON body
/// pi's `MessageCreateParamsStreaming` serializes to (field order aside).
pub fn build_params(
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    is_oauth: bool,
    options: &AnthropicOptions,
) -> Value {
    let (_, cache_control) =
        get_cache_control(model, options.cache_retention, options.env.as_ref());
    let compat = get_anthropic_compat(model);
    let transformed = transform_messages(&context.messages, model);

    let tools: Vec<Value> = context.tools.clone().unwrap_or_default();
    let placement = split_deferred_tools(
        &tools,
        &transformed,
        compat.supports_tool_references,
        is_oauth,
    );
    let mut immediate_tools = placement.immediate;
    let mut deferred_tools: Vec<Value> = placement.deferred.into_iter().map(|(_, t)| t).collect();
    if immediate_tools.is_empty() && !deferred_tools.is_empty() {
        immediate_tools = deferred_tools;
        deferred_tools = Vec::new();
    }
    let deferred_tool_names: HashSet<String> = deferred_tools
        .iter()
        .map(|tool| normalize_tool_name(tool_name(tool), is_oauth))
        .collect();

    let messages = convert_messages(
        &transformed,
        is_oauth,
        cache_control.as_ref(),
        compat.allow_empty_signature,
        &deferred_tool_names,
    );

    let mut params = Map::new();
    params.insert("model".to_string(), json!(model.id));
    params.insert("messages".to_string(), Value::Array(messages));
    params.insert(
        "max_tokens".to_string(),
        json!(options.max_tokens.unwrap_or(model.max_tokens)),
    );
    params.insert("stream".to_string(), json!(true));

    // System serialization, incl. the OAuth Claude Code identity prepend.
    if is_oauth {
        let mut system = vec![system_block(
            "You are Claude Code, Anthropic's official CLI for Claude.",
            cache_control.as_ref(),
        )];
        if let Some(system_prompt) = &context.system_prompt {
            system.push(system_block(
                &sanitize_surrogates(system_prompt),
                cache_control.as_ref(),
            ));
        }
        params.insert("system".to_string(), Value::Array(system));
    } else if let Some(system_prompt) = &context.system_prompt {
        params.insert(
            "system".to_string(),
            Value::Array(vec![system_block(
                &sanitize_surrogates(system_prompt),
                cache_control.as_ref(),
            )]),
        );
    }

    // Temperature is incompatible with extended thinking and gated by compat.
    if let Some(temperature) = options.temperature {
        if !matches!(options.thinking_enabled, Some(true)) && compat.supports_temperature {
            params.insert("temperature".to_string(), json!(temperature));
        }
    }

    // Tools.
    if !immediate_tools.is_empty() || !deferred_tools.is_empty() {
        let mut tools_value = convert_tools(
            &immediate_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            if compat.supports_cache_control_on_tools {
                cache_control.as_ref()
            } else {
                None
            },
            false,
        );
        tools_value.extend(convert_tools(
            &deferred_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            None,
            true,
        ));
        params.insert("tools".to_string(), Value::Array(tools_value));
    }

    // Thinking mode: adaptive, budget-based, or explicitly disabled.
    if model.reasoning {
        apply_thinking(model, options, &mut params);
    }

    // Metadata: only a string `user_id` is forwarded.
    if let Some(metadata) = &options.metadata {
        if let Some(user_id) = metadata.get("user_id").and_then(Value::as_str) {
            params.insert("metadata".to_string(), json!({ "user_id": user_id }));
        }
    }

    // Tool choice.
    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".to_string(), tool_choice.to_value());
    }

    Value::Object(params)
}

/// Build a system text block, adding `cache_control` only when present, mirroring
/// pi's `{ type: "text", text, ...(cacheControl ? { cache_control } : {}) }`.
fn system_block(text: &str, cache_control: Option<&Value>) -> Value {
    let mut block = Map::new();
    block.insert("type".to_string(), json!("text"));
    block.insert("text".to_string(), json!(text));
    if let Some(cache_control) = cache_control {
        block.insert("cache_control".to_string(), cache_control.clone());
    }
    Value::Object(block)
}

/// Apply the thinking configuration, mirroring the `model.reasoning` block of
/// `buildParams` (`anthropic-messages.ts:1000-1029`).
fn apply_thinking(
    model: &Model<AnthropicMessagesCompat>,
    options: &AnthropicOptions,
    params: &mut Map<String, Value>,
) {
    if options.thinking_enabled == Some(true) {
        // Default to "summarized" to match older Claude 4 behavior.
        let display = options
            .thinking_display
            .unwrap_or(AnthropicThinkingDisplay::Summarized);
        let force_adaptive = model
            .compat
            .as_ref()
            .and_then(|c| c.force_adaptive_thinking)
            == Some(true);
        if force_adaptive {
            params.insert(
                "thinking".to_string(),
                json!({ "type": "adaptive", "display": display.as_str() }),
            );
            if let Some(effort) = options.effort {
                params.insert(
                    "output_config".to_string(),
                    json!({ "effort": effort.as_str() }),
                );
            }
        } else {
            let budget_tokens = options
                .thinking_budget_tokens
                .filter(|&v| v != 0)
                .unwrap_or(1024);
            params.insert(
                "thinking".to_string(),
                json!({
                    "type": "enabled",
                    "budget_tokens": budget_tokens,
                    "display": display.as_str(),
                }),
            );
        }
    } else if options.thinking_enabled == Some(false) && !off_level_is_null(model) {
        params.insert("thinking".to_string(), json!({ "type": "disabled" }));
    }
}

/// Whether the model's `thinkingLevelMap.off` is explicitly `null`, matching pi's
/// `model.thinkingLevelMap?.off !== null` guard (`anthropic-messages.ts:1026`).
/// pi sets `{ type: "disabled" }` unless that key exists and maps to `null`.
fn off_level_is_null(model: &Model<AnthropicMessagesCompat>) -> bool {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&crate::types::ModelThinkingLevel::Off))
        .map(|value| value.is_none())
        .unwrap_or(false)
}
