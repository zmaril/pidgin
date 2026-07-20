// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// message/content conversion: `convertMessages`, `convertContentBlocks`,
// `convertToolResult`, and `normalizeToolCallId` (`api/anthropic-messages.ts`).
// The per-role and per-block arms are walls of near-identical branch/serde
// shaping by design; the clone detector reads them as duplicates, but factoring
// them would distort the byte-faithful port, so the repetition is intentional.
//! Message and content-block conversion into the Anthropic Messages request
//! shape, ported from pi-ai's `packages/ai/src/api/anthropic-messages.ts` at
//! pinned commit `3da591ab`. The cross-model normalization that precedes it
//! (`transform-messages.ts`) lives in the sibling [`super::transform_messages`]
//! module, which borrows [`normalize_tool_call_id`] from here.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::types::{
    AssistantMessage, ContentBlock, Message, ToolResultMessage, UserContent, UserMessage,
};

use super::tools::{normalize_tool_name, to_claude_code_name};

/// Remove unpaired Unicode surrogate characters, mirroring pi's
/// `sanitizeSurrogates` (`utils/sanitize-unicode.ts`). Rust `String`s are always
/// valid UTF-8, so lone surrogates cannot occur and this is the identity on
/// every input Rust can represent; it exists to keep the port's call sites
/// aligned with pi's.
// Follow-up (#N): provenance is pi's third file `utils/sanitize-unicode.ts`, not
// anthropic-messages.ts; it lives here beside its only caller. A future
// micro-split to a `sanitize_unicode.rs` sibling is possible but out of scope.
pub fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

/// Normalize a tool-call id to Anthropic's `^[a-zA-Z0-9_-]+$` pattern and 64-char
/// cap, mirroring pi's `normalizeToolCallId` (`anthropic-messages.ts:1050`).
pub fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

// ---------------------------------------------------------------------------
// convertContentBlocks / convertToolResult (`anthropic-messages.ts`)
// ---------------------------------------------------------------------------

/// Whether a content block is an image. (Duplicated in
/// [`super::transform_messages`]; pi inlines the `type === "image"` check.)
fn is_image(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Image { .. })
}

/// Convert `(text | image)` content blocks to the Anthropic representation,
/// mirroring pi's `convertContentBlocks` (`anthropic-messages.ts:115`): a plain
/// concatenated string when there are no images, else a block array (prefixed
/// with a placeholder text block when images carry no text).
pub fn convert_content_blocks(content: &[ContentBlock]) -> Value {
    let has_images = content.iter().any(is_image);
    if !has_images {
        let joined = content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(sanitize_surrogates(&joined));
    }

    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => {
                json!({ "type": "text", "text": sanitize_surrogates(text) })
            }
            ContentBlock::Image { data, mime_type } => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": mime_type, "data": data },
            }),
            _ => json!({ "type": "text", "text": String::new() }),
        })
        .collect();

    let has_text = blocks
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some("text"));
    if !has_text {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

/// A converted tool result plus any displaced sibling content, mirroring the
/// return of pi's `convertToolResult` (`anthropic-messages.ts:1054`).
struct ConvertedToolResult {
    tool_result: Value,
    sibling_content: Vec<Value>,
}

/// Convert a tool-result message, emitting tool-reference blocks for newly
/// loaded deferred tools and displacing ordinary content to siblings, mirroring
/// pi's `convertToolResult` (`anthropic-messages.ts:1054`).
fn convert_tool_result(
    msg: &ToolResultMessage,
    is_oauth: bool,
    deferred_tool_names: &HashSet<String>,
    loaded_tool_names: &mut HashSet<String>,
) -> ConvertedToolResult {
    let mut references: Vec<Value> = Vec::new();
    for name in msg.added_tool_names.iter().flatten() {
        let normalized = normalize_tool_name(name, is_oauth);
        if !deferred_tool_names.contains(&normalized) || loaded_tool_names.contains(&normalized) {
            continue;
        }
        loaded_tool_names.insert(normalized);
        references.push(json!({
            "type": "tool_reference",
            "tool_name": if is_oauth { to_claude_code_name(name) } else { name.clone() },
        }));
    }

    let converted_content = convert_content_blocks(&msg.content);
    let content_value = if !references.is_empty() {
        Value::Array(references.clone())
    } else {
        converted_content.clone()
    };
    let tool_result = json!({
        "type": "tool_result",
        "tool_use_id": msg.tool_call_id,
        "content": content_value,
        "is_error": msg.is_error,
    });

    let sibling_content = if references.is_empty() {
        Vec::new()
    } else {
        match converted_content {
            Value::String(text) => vec![json!({ "type": "text", "text": text })],
            Value::Array(blocks) => blocks,
            other => vec![other],
        }
    };

    ConvertedToolResult {
        tool_result,
        sibling_content,
    }
}

// ---------------------------------------------------------------------------
// convertMessages (`anthropic-messages.ts:1089`)
// ---------------------------------------------------------------------------

/// Convert transformed messages into Anthropic `messages[]`, mirroring pi's
/// `convertMessages` (`anthropic-messages.ts:1089`). Applies `cache_control` to
/// the final user block when present.
pub fn convert_messages(
    transformed: &[Message],
    is_oauth: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
    deferred_tool_names: &HashSet<String>,
) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut loaded_tool_names: HashSet<String> = HashSet::new();

    let mut i = 0;
    while i < transformed.len() {
        match &transformed[i] {
            Message::User(user) => {
                push_user_message(user, &mut params);
                i += 1;
            }
            Message::Assistant(assistant) => {
                push_assistant_message(assistant, is_oauth, allow_empty_signature, &mut params);
                i += 1;
            }
            Message::ToolResult(_) => {
                // Collect all consecutive tool-result messages.
                let mut tool_results: Vec<Value> = Vec::new();
                let mut sibling_content: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed.len() {
                    let Message::ToolResult(result) = &transformed[j] else {
                        break;
                    };
                    let converted = convert_tool_result(
                        result,
                        is_oauth,
                        deferred_tool_names,
                        &mut loaded_tool_names,
                    );
                    tool_results.push(converted.tool_result);
                    sibling_content.extend(converted.sibling_content);
                    j += 1;
                }
                let mut content = tool_results;
                content.extend(sibling_content);
                params.push(json!({ "role": "user", "content": content }));
                i = j;
            }
        }
    }

    if let Some(cache_control) = cache_control {
        apply_cache_control_to_last_user(&mut params, cache_control);
    }

    params
}

/// Push a user message, mirroring the `user` arm of `convertMessages`
/// (`anthropic-messages.ts:1103`).
fn push_user_message(user: &UserMessage, params: &mut Vec<Value>) {
    match &user.content {
        UserContent::Text(text) => {
            if !text.trim().is_empty() {
                params.push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
            }
        }
        UserContent::Blocks(blocks) => {
            let converted: Vec<Value> = blocks
                .iter()
                .filter_map(|item| match item {
                    ContentBlock::Text { text, .. } => Some(json!({
                        "type": "text",
                        "text": sanitize_surrogates(text),
                    })),
                    ContentBlock::Image { data, mime_type } => Some(json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": mime_type, "data": data },
                    })),
                    _ => None,
                })
                .collect();
            // pi maps only text/image, then filters out blank text blocks.
            let filtered: Vec<Value> = converted
                .into_iter()
                .filter(|b| {
                    if b.get("type").and_then(Value::as_str) == Some("text") {
                        b.get("text")
                            .and_then(Value::as_str)
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false)
                    } else {
                        true
                    }
                })
                .collect();
            if filtered.is_empty() {
                return;
            }
            params.push(json!({ "role": "user", "content": filtered }));
        }
    }
}

/// Push an assistant message, mirroring the `assistant` arm of `convertMessages`
/// (`anthropic-messages.ts:1141`).
fn push_assistant_message(
    assistant: &AssistantMessage,
    is_oauth: bool,
    allow_empty_signature: bool,
    params: &mut Vec<Value>,
) {
    let mut blocks: Vec<Value> = Vec::new();
    for block in &assistant.content {
        match block {
            ContentBlock::Text { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                blocks.push(json!({ "type": "text", "text": sanitize_surrogates(text) }));
            }
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                redacted,
            } => {
                if redacted == &Some(true) {
                    blocks.push(json!({
                        "type": "redacted_thinking",
                        "data": thinking_signature.clone().unwrap_or_default(),
                    }));
                    continue;
                }
                let has_signature = thinking_signature
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                if thinking.trim().is_empty() && !has_signature {
                    continue;
                }
                if !has_signature {
                    if allow_empty_signature {
                        blocks.push(json!({
                            "type": "thinking",
                            "thinking": sanitize_surrogates(thinking),
                            "signature": "",
                        }));
                    } else {
                        blocks.push(json!({
                            "type": "text",
                            "text": sanitize_surrogates(thinking),
                        }));
                    }
                } else {
                    blocks.push(json!({
                        "type": "thinking",
                        "thinking": sanitize_surrogates(thinking),
                        "signature": thinking_signature.clone().unwrap_or_default(),
                    }));
                }
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": if is_oauth { to_claude_code_name(name) } else { name.clone() },
                    "input": arguments,
                }));
            }
            ContentBlock::Image { .. } | ContentBlock::Unknown => {}
        }
    }
    if blocks.is_empty() {
        return;
    }
    params.push(json!({ "role": "assistant", "content": blocks }));
}

/// Stamp `cache_control` on the last user message's final cacheable block,
/// mirroring the tail of pi's `convertMessages` (`anthropic-messages.ts:1229`).
fn apply_cache_control_to_last_user(params: &mut [Value], cache_control: &Value) {
    let Some(last) = params.last_mut() else {
        return;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return;
    }
    match last.get_mut("content") {
        Some(Value::Array(content)) => {
            if let Some(last_block) = content.last_mut() {
                let block_type = last_block.get("type").and_then(Value::as_str);
                if matches!(
                    block_type,
                    Some("text") | Some("image") | Some("tool_result")
                ) {
                    if let Value::Object(map) = last_block {
                        map.insert("cache_control".to_string(), cache_control.clone());
                    }
                }
            }
        }
        Some(Value::String(text)) => {
            let text = text.clone();
            let mut block = Map::new();
            block.insert("type".to_string(), json!("text"));
            block.insert("text".to_string(), json!(text));
            block.insert("cache_control".to_string(), cache_control.clone());
            if let Value::Object(obj) = last {
                obj.insert(
                    "content".to_string(),
                    Value::Array(vec![Value::Object(block)]),
                );
            }
        }
        _ => {}
    }
}
