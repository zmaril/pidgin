// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `api/simple-options.ts` (`buildBaseOptions`, `adjustMaxTokensForThinking`,
// `clampMaxTokensToContext`, `clampReasoning`). The per-level budget and clamp
// arms mirror pi's hand-rolled shape; the clone detector reads them as
// duplicative by design.
//! `streamSimple` support helpers, ported from pi-ai's
//! `packages/ai/src/api/simple-options.ts` at pinned commit `3da591ab`.
//!
//! These compute the `maxTokens` / `thinkingBudgetTokens` that
//! [`super::driver::stream_simple`] threads into
//! [`super::request::AnthropicOptions`]: the context clamp and the
//! thinking-budget adjustment. The context-token estimator these consume (pi's
//! `utils/estimate.ts`) lives in the sibling [`super::estimate`] module.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{AnthropicMessagesCompat, CacheRetention, Context, Model, ThinkingLevel};

use super::estimate::estimate_context_tokens;
use super::request::AnthropicOptions;

/// `simple-options.ts:12`.
const CONTEXT_SAFETY_TOKENS: i64 = 4096;
/// `simple-options.ts:13`.
const MIN_MAX_TOKENS: i64 = 1;

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
