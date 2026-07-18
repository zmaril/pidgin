// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` thinking types and `mapThinkingLevelToEffort`. The
// small enums and switch arms mirror pi verbatim; the clone detector may read
// the serde enum scaffolding as duplicative by design.
//! Extended-thinking configuration types and helpers, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` at pinned commit `3da591ab`.

use serde::{Deserialize, Serialize};

use crate::types::{AnthropicMessagesCompat, Model, ModelThinkingLevel, ThinkingLevel};

/// Effort level for adaptive-thinking models (`anthropic-messages.ts:164`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl AnthropicEffort {
    /// The wire string this effort serializes to (as it appears in
    /// `output_config.effort`).
    pub fn as_str(self) -> &'static str {
        match self {
            AnthropicEffort::Low => "low",
            AnthropicEffort::Medium => "medium",
            AnthropicEffort::High => "high",
            AnthropicEffort::Xhigh => "xhigh",
            AnthropicEffort::Max => "max",
        }
    }
}

/// How thinking content is returned in API responses (`anthropic-messages.ts:166`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

impl AnthropicThinkingDisplay {
    /// The wire string this display serializes to.
    pub fn as_str(self) -> &'static str {
        match self {
            AnthropicThinkingDisplay::Summarized => "summarized",
            AnthropicThinkingDisplay::Omitted => "omitted",
        }
    }
}

/// Map a simple reasoning level to an adaptive-thinking effort, mirroring pi's
/// `mapThinkingLevelToEffort` (`anthropic-messages.ts:766`). A model's
/// `thinkingLevelMap` override wins when it maps the level to a string; otherwise
/// the level is bucketed (minimal/low → low, medium → medium, high/xhigh/max/…
/// → high, with `high` as the default for an absent level).
pub fn map_thinking_level_to_effort(
    model: &Model<AnthropicMessagesCompat>,
    level: Option<ThinkingLevel>,
) -> AnthropicEffort {
    if let Some(level) = level {
        if let Some(mapped) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&to_model_level(level)))
            .and_then(|value| value.as_deref())
        {
            if let Some(effort) = parse_effort(mapped) {
                return effort;
            }
        }
    }

    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => AnthropicEffort::Low,
        Some(ThinkingLevel::Medium) => AnthropicEffort::Medium,
        Some(ThinkingLevel::High) => AnthropicEffort::High,
        _ => AnthropicEffort::High,
    }
}

/// Widen a [`ThinkingLevel`] to the matching [`ModelThinkingLevel`] key, the way
/// pi indexes `thinkingLevelMap` (a `Partial<Record<ModelThinkingLevel, …>>`)
/// with a `ThinkingLevel` value.
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

/// Parse a `thinkingLevelMap` override string into an [`AnthropicEffort`]. pi
/// casts the mapped string to `AnthropicEffort` unchecked; here we accept the
/// five known effort strings and fall through to the bucketing otherwise.
fn parse_effort(value: &str) -> Option<AnthropicEffort> {
    match value {
        "low" => Some(AnthropicEffort::Low),
        "medium" => Some(AnthropicEffort::Medium),
        "high" => Some(AnthropicEffort::High),
        "xhigh" => Some(AnthropicEffort::Xhigh),
        "max" => Some(AnthropicEffort::Max),
        _ => None,
    }
}
