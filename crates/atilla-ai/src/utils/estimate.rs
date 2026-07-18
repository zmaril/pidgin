//! Context token estimation, ported from pi-ai's
//! `packages/ai/src/utils/estimate.ts` at pinned commit `3da591ab`.
//!
//! A heuristic token accountant used to size requests before a provider reports
//! real usage. [`estimate_context_tokens`] anchors on the most recent applicable
//! assistant `usage` block and estimates only the messages after it; when no such
//! block exists it estimates every message plus the system-prompt/tools prefix.
//! The character heuristic is `ceil(chars / 4)`, with images counted as a flat
//! 4800 characters.
//!
//! # Parity notes
//!
//! - All lengths use JS `String.length` (UTF-16 code units) via
//!   [`str::encode_utf16`], matching pi and this crate's faux provider.
//! - `calculateContextTokens` mirrors pi's `usage.totalTokens || <sum>`: the
//!   `||` treats `0` as falsy, so a zero `total_tokens` falls back to the
//!   component sum.
//! - `getLastAssistantUsageInfo` tracks the newest message timestamp seen so far
//!   and ignores an assistant `usage` block once a newer prefix message has been
//!   inserted before it (e.g. a compaction summary). The port seeds that
//!   "latest prefix timestamp" with `i64::MIN` in place of JS
//!   `Number.NEGATIVE_INFINITY`.
//! - Tools are opaque JSON `Value`s here (see [`crate::types::Context`]); their
//!   token cost is `estimate_text_tokens(serde_json::to_string(tools))`.

use serde_json::Value;

use crate::types::{ContentBlock, Context, Message, Usage, UserContent};

/// A context token estimate (`estimate.ts:3`, `ContextUsageEstimate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: u64,
    /// Tokens reported by the most recent applicable assistant usage block.
    pub usage_tokens: u64,
    /// Estimated tokens after the most recent applicable assistant usage block.
    pub trailing_tokens: u64,
    /// Index of the message that provided usage, or `None` when none applies.
    pub last_usage_index: Option<usize>,
}

const CHARS_PER_TOKEN: u64 = 4;
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// JS `String.length`: the count of UTF-16 code units.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// pi's `calculateContextTokens` (`estimate.ts:17`): `totalTokens`, or the
/// component sum when `totalTokens` is zero (JS `||` falsy-zero fallback).
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens != 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// pi's local `safeJsonStringify` (`estimate.ts:21`): the compact JSON, or
/// `"[unserializable]"` on failure. (`Value` always serializes, so the fallback
/// is defensive.)
fn safe_json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_string())
}

/// Estimate tokens for raw text: `ceil(len / 4)` over UTF-16 code units
/// (`estimate.ts:37`).
pub fn estimate_text_tokens(text: &str) -> u64 {
    (js_len(text) as u64).div_ceil(CHARS_PER_TOKEN)
}

/// Character count of user/tool-result content: string length, or the sum of
/// text-block lengths with images counted as 4800 chars each (`estimate.ts:29`).
fn user_content_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(text) => js_len(text),
        UserContent::Blocks(blocks) => blocks_chars(blocks),
    }
}

/// `estimateTextAndImageContentChars` for a block list (`estimate.ts:29`): a text
/// block counts its text length, every other block counts as an image (4800).
fn blocks_chars(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => js_len(text),
            _ => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

/// Estimate tokens for user/tool-result content (`estimate.ts:41`,
/// `estimateTextAndImageContentTokens`).
pub fn estimate_text_and_image_content_tokens(content: &UserContent) -> u64 {
    (user_content_chars(content) as u64).div_ceil(CHARS_PER_TOKEN)
}

/// Estimate tokens for a single message (`estimate.ts:45`).
pub fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::User(user) => estimate_text_and_image_content_tokens(&user.content),
        Message::ToolResult(result) => {
            (blocks_chars(&result.content) as u64).div_ceil(CHARS_PER_TOKEN)
        }
        Message::Assistant(assistant) => {
            let mut chars: usize = 0;
            for block in &assistant.content {
                match block {
                    ContentBlock::Text { text, .. } => chars += js_len(text),
                    ContentBlock::Thinking { thinking, .. } => chars += js_len(thinking),
                    ContentBlock::ToolCall {
                        name, arguments, ..
                    } => chars += js_len(name) + js_len(&safe_json_stringify(arguments)),
                    // Images/unknown blocks do not appear in pi's AssistantContent.
                    ContentBlock::Image { .. } | ContentBlock::Unknown => {}
                }
            }
            (chars as u64).div_ceil(CHARS_PER_TOKEN)
        }
    }
}

fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::User(m) => m.timestamp,
        Message::Assistant(m) => m.timestamp,
        Message::ToolResult(m) => m.timestamp,
    }
}

/// pi's `getLastAssistantUsageInfo` (`estimate.ts:63`): the newest assistant
/// usage block that still describes the current prefix, i.e. not stranded behind
/// a later-inserted message, and not from an aborted/errored turn with zero
/// tokens.
fn last_assistant_usage_info(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut usage_info: Option<(Usage, usize)> = None;

    for (i, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            let applies_to_prefix = assistant.timestamp >= latest_prefix_timestamp;
            if applies_to_prefix
                && assistant.stop_reason != crate::types::StopReason::Aborted
                && assistant.stop_reason != crate::types::StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((assistant.usage.clone(), i));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }

    usage_info
}

/// pi's `estimateMessages` (`estimate.ts:89`).
fn estimate_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = last_assistant_usage_info(messages) {
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

/// pi's `estimateToolsTokens` (`estimate.ts:105`): zero for no/empty tools, else
/// the token estimate of the tools serialized to JSON.
fn estimate_tools_tokens(tools: Option<&[Value]>) -> u64 {
    match tools {
        Some(tools) if !tools.is_empty() => {
            estimate_text_tokens(&safe_json_stringify(&Value::Array(tools.to_vec())))
        }
        _ => 0,
    }
}

fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

/// Estimate total context tokens (`estimate.ts:114`, `estimateContextTokens`).
///
/// When an assistant usage block anchors the estimate, only tool definitions
/// added after that anchor (via `toolResult.addedToolNames`) contribute extra
/// prefix tokens; otherwise the system prompt and full tool list are added.
pub fn estimate_context_tokens(context: &Context) -> ContextUsageEstimate {
    let estimate = estimate_messages(&context.messages);

    if let Some(last_usage_index) = estimate.last_usage_index {
        // Names of tools introduced after the anchoring usage block.
        let mut added_names: Vec<String> = Vec::new();
        for message in &context.messages[last_usage_index + 1..] {
            if let Message::ToolResult(result) = message {
                if let Some(added) = &result.added_tool_names {
                    for name in added {
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
                    .filter(|tool| added_names.iter().any(|name| name == tool_name(tool)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let added_tool_tokens = estimate_tools_tokens(Some(&added_tools));

        return ContextUsageEstimate {
            tokens: estimate.tokens + added_tool_tokens,
            usage_tokens: estimate.usage_tokens,
            trailing_tokens: estimate.trailing_tokens + added_tool_tokens,
            last_usage_index: estimate.last_usage_index,
        };
    }

    let prefix_tokens = context
        .system_prompt
        .as_deref()
        .map(estimate_text_tokens)
        .unwrap_or(0)
        + estimate_tools_tokens(context.tools.as_deref());

    ContextUsageEstimate {
        tokens: estimate.tokens + prefix_tokens,
        usage_tokens: estimate.usage_tokens,
        trailing_tokens: estimate.trailing_tokens + prefix_tokens,
        last_usage_index: estimate.last_usage_index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, AssistantRole, StopReason, UsageCost, UserContent, UserMessage, UserRole,
    };

    fn usage(total_tokens: u64) -> Usage {
        Usage {
            input: total_tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens,
            cost: UsageCost::default(),
        }
    }

    fn user_text(content: &str, timestamp: i64) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(content.to_string()),
            timestamp,
        })
    }

    fn assistant(timestamp: i64, total_tokens: u64) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![ContentBlock::Text {
                text: "kept".into(),
                text_signature: None,
            }],
            api: "openai-responses".into(),
            provider: "openai".into(),
            model: "test-model".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(total_tokens),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp,
        })
    }

    #[test]
    fn calculate_context_tokens_falls_back_to_components() {
        let mut u = usage(0);
        u.input = 10;
        u.output = 5;
        u.cache_read = 3;
        u.cache_write = 2;
        u.total_tokens = 0;
        assert_eq!(calculate_context_tokens(&u), 20);
    }

    #[test]
    fn calculate_context_tokens_prefers_total() {
        assert_eq!(calculate_context_tokens(&usage(1234)), 1234);
    }

    #[test]
    fn estimate_text_tokens_ceils_by_four() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("abc"), 1); // ceil(3/4)
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2); // ceil(5/4)
        assert_eq!(estimate_text_tokens(&"x".repeat(4000)), 1000);
    }

    #[test]
    fn estimate_message_tokens_toolcall_counts_name_and_args() {
        let message = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "c1".into(),
                name: "read".into(),                           // 4 chars
                arguments: serde_json::json!({ "path": "x" }), // {"path":"x"} = 12 chars
                thought_signature: None,
            }],
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(0),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 1,
        });
        // chars = 4 + 12 = 16 → ceil(16/4) = 4
        assert_eq!(estimate_message_tokens(&message), 4);
    }

    #[test]
    fn ignores_stale_assistant_usage_after_newer_inserted_message() {
        // Mirrors context-estimate.test.ts: assistant (ts 100) is stranded behind
        // a user message at ts 200, so its usage does not apply.
        let context = Context {
            system_prompt: Some("system".into()), // 6 chars → 2 tokens
            messages: vec![
                user_text("summary", 200),          // 7 chars → 2 tokens
                assistant(100, 9_500),              // "kept" → 1 token
                user_text(&"x".repeat(4_000), 300), // 1000 tokens
            ],
            tools: None,
        };
        let estimate = estimate_context_tokens(&context);
        assert_eq!(
            estimate,
            ContextUsageEstimate {
                tokens: 1_005,
                usage_tokens: 0,
                trailing_tokens: 1_005,
                last_usage_index: None,
            }
        );
    }

    #[test]
    fn uses_assistant_usage_after_response_to_inserted_context() {
        let context = Context {
            system_prompt: None,
            messages: vec![
                user_text("summary", 200),
                assistant(100, 9_500),
                user_text("new prompt", 300),
                assistant(400, 2_000),
                user_text("tail", 500), // 4 chars → 1 token
            ],
            tools: None,
        };
        let estimate = estimate_context_tokens(&context);
        assert_eq!(
            estimate,
            ContextUsageEstimate {
                tokens: 2_001,
                usage_tokens: 2_000,
                trailing_tokens: 1,
                last_usage_index: Some(3),
            }
        );
    }

    #[test]
    fn errored_assistant_usage_is_ignored() {
        let mut errored = assistant(100, 5_000);
        if let Message::Assistant(a) = &mut errored {
            a.stop_reason = StopReason::Error;
        }
        let context = Context {
            system_prompt: None,
            messages: vec![user_text("hi", 50), errored, user_text("there", 200)],
            tools: None,
        };
        let estimate = estimate_context_tokens(&context);
        assert_eq!(estimate.last_usage_index, None);
    }

    #[test]
    fn image_block_counts_as_estimated_chars() {
        let message = Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![ContentBlock::Image {
                data: "aW1n".into(),
                mime_type: "image/png".into(),
            }]),
            timestamp: 1,
        });
        // 4800 chars → ceil(4800/4) = 1200 tokens
        assert_eq!(estimate_message_tokens(&message), 1_200);
    }
}
