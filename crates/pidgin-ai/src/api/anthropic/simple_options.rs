// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `api/simple-options.ts` (`buildBaseOptions`, `adjustMaxTokensForThinking`,
// `clampMaxTokensToContext`, `clampReasoning`) and the `utils/estimate.ts`
// context-token estimator those helpers consume. pi keeps the estimator under
// `utils/`; the Rust port cannot edit outside `api/anthropic*`, so the estimator
// is colocated here. The per-role/per-block estimation arms mirror pi's
// hand-rolled `switch`/`if` shape; the clone detector reads them as duplicative
// by design.
//! `streamSimple` support helpers, ported from pi-ai's
//! `packages/ai/src/api/simple-options.ts` and the slice of
//! `packages/ai/src/utils/estimate.ts` it depends on, at pinned commit
//! `3da591ab`.
//!
//! These compute the `maxTokens` / `thinkingBudgetTokens` that
//! [`super::driver::stream_simple`] threads into
//! [`super::request::AnthropicOptions`]: the model/context token estimate,
//! the context clamp, and the thinking-budget adjustment.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::types::{
    AnthropicMessagesCompat, CacheRetention, ContentBlock, Context, Message, Model, ThinkingLevel,
    Usage, UserContent,
};

use super::request::AnthropicOptions;

/// `simple-options.ts:12`.
const CONTEXT_SAFETY_TOKENS: i64 = 4096;
/// `simple-options.ts:13`.
const MIN_MAX_TOKENS: i64 = 1;
/// `estimate.ts:15`.
const CHARS_PER_TOKEN: i64 = 4;
/// `estimate.ts:16`.
const ESTIMATED_IMAGE_CHARS: i64 = 4800;

/// Per-level thinking budgets a caller can override (pi's `ThinkingBudgets`,
/// `types.ts`). Each field is optional; unset falls back to the default budget.
#[derive(Debug, Clone, Default)]
pub struct ThinkingBudgets {
    pub minimal: Option<u64>,
    pub low: Option<u64>,
    pub medium: Option<u64>,
    pub high: Option<u64>,
}

/// The simple, level-based stream options (pi's `SimpleStreamOptions`, the subset
/// `streamSimple` reads). Mirrors [`AnthropicOptions`] on the auth/cache fields
/// but expresses thinking as a single [`ThinkingLevel`] (`reasoning`).
#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub api_key: Option<String>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub metadata: Option<serde_json::Map<String, Value>>,
}

// ---------------------------------------------------------------------------
// utils/estimate.ts (the slice clampMaxTokensToContext consumes)
// ---------------------------------------------------------------------------

/// pi's `ContextUsageEstimate` (`estimate.ts:3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: i64,
    pub usage_tokens: i64,
    pub trailing_tokens: i64,
    pub last_usage_index: Option<usize>,
}

/// pi's `calculateContextTokens` (`estimate.ts:19`).
fn calculate_context_tokens(usage: &Usage) -> i64 {
    if usage.total_tokens != 0 {
        usage.total_tokens as i64
    } else {
        (usage.input + usage.output + usage.cache_read + usage.cache_write) as i64
    }
}

/// JS `String.length` approximation: UTF-16 code units are counted as Unicode
/// scalar values here (exact for the BMP; pi's estimate is itself an
/// approximation, and Rust `String` cannot hold lone surrogates).
fn js_len(text: &str) -> i64 {
    text.chars().count() as i64
}

/// pi's `safeJsonStringify` (`estimate.ts:27`): compact JSON, or a placeholder.
fn safe_json_stringify<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_string())
}

/// pi's `estimateTextTokens` (`estimate.ts:40`).
fn estimate_text_tokens(text: &str) -> i64 {
    div_ceil(js_len(text), CHARS_PER_TOKEN)
}

/// Ceiling division for non-negative operands (`Math.ceil(a / b)`).
fn div_ceil(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

/// pi's `estimateTextAndImageContentChars` for a bare-string content
/// (`estimate.ts:35`).
fn estimate_string_content_tokens(text: &str) -> i64 {
    div_ceil(js_len(text), CHARS_PER_TOKEN)
}

/// pi's `estimateTextAndImageContentChars` for block content (`estimate.ts:36`):
/// text blocks contribute their length, images a fixed estimate.
fn estimate_block_content_tokens(blocks: &[ContentBlock]) -> i64 {
    let mut chars = 0;
    for block in blocks {
        chars += match block {
            ContentBlock::Text { text, .. } => js_len(text),
            ContentBlock::Image { .. } => ESTIMATED_IMAGE_CHARS,
            _ => 0,
        };
    }
    div_ceil(chars, CHARS_PER_TOKEN)
}

/// pi's `estimateMessageTokens` (`estimate.ts:52`).
fn estimate_message_tokens(message: &Message) -> i64 {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Text(text) => estimate_string_content_tokens(text),
            UserContent::Blocks(blocks) => estimate_block_content_tokens(blocks),
        },
        Message::ToolResult(result) => estimate_block_content_tokens(&result.content),
        Message::Assistant(assistant) => {
            let mut chars = 0;
            for block in &assistant.content {
                chars += match block {
                    ContentBlock::Text { text, .. } => js_len(text),
                    ContentBlock::Thinking { thinking, .. } => js_len(thinking),
                    ContentBlock::ToolCall {
                        name, arguments, ..
                    } => js_len(name) + js_len(&safe_json_stringify(arguments)),
                    _ => 0,
                };
            }
            div_ceil(chars, CHARS_PER_TOKEN)
        }
    }
}

/// The timestamp of any message variant (pi reads `message.timestamp`).
fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::User(m) => m.timestamp,
        Message::Assistant(m) => m.timestamp,
        Message::ToolResult(m) => m.timestamp,
    }
}

/// pi's `getLastAssistantUsageInfo` (`estimate.ts:74`).
fn get_last_assistant_usage_info(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut usage_info: Option<(Usage, usize)> = None;

    for (i, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            let usage_applies_to_prefix = assistant.timestamp >= latest_prefix_timestamp;
            let stop_ok = !matches!(
                assistant.stop_reason,
                crate::types::StopReason::Aborted | crate::types::StopReason::Error
            );
            if usage_applies_to_prefix && stop_ok && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((assistant.usage.clone(), i));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }

    usage_info
}

/// pi's `estimateMessages` (`estimate.ts:100`).
fn estimate_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = get_last_assistant_usage_info(messages) {
        let usage_tokens = calculate_context_tokens(&usage);
        let mut trailing_tokens = 0;
        for message in &messages[index + 1..] {
            trailing_tokens += estimate_message_tokens(message);
        }
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }

    let mut tokens = 0;
    for message in messages {
        tokens += estimate_message_tokens(message);
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// Read a tool's `name` field from its opaque `Value`.
fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

/// pi's `estimateToolsTokens` (`estimate.ts:130`): the serialized tool list's
/// text-token estimate, `0` for an empty/absent list.
fn estimate_tools_tokens(tools: &[Value]) -> i64 {
    if tools.is_empty() {
        return 0;
    }
    estimate_text_tokens(&safe_json_stringify(tools))
}

/// pi's `estimateContextTokens` for a `Context` (`estimate.ts:139`).
pub fn estimate_context_tokens(context: &Context) -> ContextUsageEstimate {
    let estimate = estimate_messages(&context.messages);

    if let Some(last_usage_index) = estimate.last_usage_index {
        // Tools that became available after the last usage-bearing message.
        let mut added_names: Vec<String> = Vec::new();
        for message in &context.messages[last_usage_index + 1..] {
            if let Message::ToolResult(result) = message {
                if let Some(names) = &result.added_tool_names {
                    for name in names {
                        if !added_names.contains(name) {
                            added_names.push(name.clone());
                        }
                    }
                }
            }
        }
        let added_tools: Vec<Value> = context
            .tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .filter(|tool| added_names.iter().any(|n| n == tool_name(tool)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let added_tool_tokens = estimate_tools_tokens(&added_tools);
        return ContextUsageEstimate {
            tokens: estimate.tokens + added_tool_tokens,
            usage_tokens: estimate.usage_tokens,
            trailing_tokens: estimate.trailing_tokens + added_tool_tokens,
            last_usage_index: estimate.last_usage_index,
        };
    }

    let system_tokens = context
        .system_prompt
        .as_ref()
        .map(|s| estimate_text_tokens(s))
        .unwrap_or(0);
    let tools_slice = context.tools.as_deref().unwrap_or(&[]);
    let prefix_tokens = system_tokens + estimate_tools_tokens(tools_slice);

    ContextUsageEstimate {
        tokens: estimate.tokens + prefix_tokens,
        usage_tokens: estimate.usage_tokens,
        trailing_tokens: estimate.trailing_tokens + prefix_tokens,
        last_usage_index: estimate.last_usage_index,
    }
}

// ---------------------------------------------------------------------------
// api/simple-options.ts
// ---------------------------------------------------------------------------

/// pi's `clampMaxTokensToContext` (`simple-options.ts:15`).
pub fn clamp_max_tokens_to_context(
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    max_tokens: u64,
) -> u64 {
    let max_tokens = max_tokens as i64;
    if model.context_window == 0 {
        return MIN_MAX_TOKENS.max(max_tokens) as u64;
    }
    let available = model.context_window as i64
        - estimate_context_tokens(context).tokens
        - CONTEXT_SAFETY_TOKENS;
    max_tokens.min(MIN_MAX_TOKENS.max(available)) as u64
}

/// pi's `clampReasoning` (`simple-options.ts:45`): `xhigh`/`max` collapse to
/// `high` so the level maps onto a budget key.
fn clamp_reasoning(effort: ThinkingLevel) -> ThinkingLevel {
    match effort {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    }
}

/// The default per-level budgets (pi's `defaultBudgets`, `simple-options.ts:57`).
fn default_budget(level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Minimal => 1024,
        ThinkingLevel::Low => 2048,
        ThinkingLevel::Medium => 8192,
        ThinkingLevel::High => 16384,
        // Unreachable after `clamp_reasoning`; kept total.
        ThinkingLevel::Xhigh | ThinkingLevel::Max => 16384,
    }
}

/// Resolve the budget for `level`, honoring caller overrides
/// (`{ ...defaultBudgets, ...customBudgets }`).
fn budget_for(level: ThinkingLevel, custom: Option<&ThinkingBudgets>) -> u64 {
    let override_value = custom.and_then(|budgets| match level {
        ThinkingLevel::Minimal => budgets.minimal,
        ThinkingLevel::Low => budgets.low,
        ThinkingLevel::Medium => budgets.medium,
        ThinkingLevel::High => budgets.high,
        ThinkingLevel::Xhigh | ThinkingLevel::Max => None,
    });
    override_value.unwrap_or_else(|| default_budget(level))
}

/// The result of [`adjust_max_tokens_for_thinking`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustedThinking {
    pub max_tokens: u64,
    pub thinking_budget: u64,
}

/// pi's `adjustMaxTokensForThinking` (`simple-options.ts:50`). `base_max_tokens`
/// is `None` when the caller set no explicit output cap (use the model cap and
/// fit thinking inside it).
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustedThinking {
    let min_output_tokens: u64 = 1024;
    let level = clamp_reasoning(reasoning_level);
    let mut thinking_budget = budget_for(level, custom_budgets);

    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => base.saturating_add(thinking_budget).min(model_max_tokens),
    };

    if max_tokens <= thinking_budget {
        thinking_budget = max_tokens.saturating_sub(min_output_tokens);
    }

    AdjustedThinking {
        max_tokens,
        thinking_budget,
    }
}

/// pi's `buildBaseOptions` (`simple-options.ts:20`), projected onto the
/// [`AnthropicOptions`] fields the Anthropic driver reads. `max_tokens` is
/// clamped to the context window here, as in pi.
pub fn build_base_options(
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    options: &SimpleStreamOptions,
) -> AnthropicOptions {
    let max_tokens = clamp_max_tokens_to_context(
        model,
        context,
        options.max_tokens.unwrap_or(model.max_tokens),
    );
    AnthropicOptions {
        temperature: options.temperature,
        max_tokens: Some(max_tokens),
        cache_retention: options.cache_retention,
        session_id: options.session_id.clone(),
        env: options.env.clone(),
        metadata: options.metadata.clone(),
        api_key: options.api_key.clone(),
        headers: options.headers.clone(),
        ..Default::default()
    }
}
