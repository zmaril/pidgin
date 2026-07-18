// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// message/content conversion: `transformMessages` (`api/transform-messages.ts`),
// `convertMessages`, `convertContentBlocks`, `convertToolResult`, and
// `normalizeToolCallId` (`api/anthropic-messages.ts`). The per-role and
// per-block arms are walls of near-identical branch/serde shaping by design; the
// clone detector reads them as duplicates, but factoring them would distort the
// byte-faithful port, so the repetition is intentional.
//! Message and content-block conversion into the Anthropic Messages request
//! shape, ported from pi-ai's `packages/ai/src/api/transform-messages.ts` and
//! `packages/ai/src/api/anthropic-messages.ts` at pinned commit `3da591ab`.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::types::{
    AnthropicMessagesCompat, AssistantMessage, ContentBlock, Message, Model, StopReason,
    ToolResultMessage, ToolResultRole, UserContent, UserMessage,
};

use super::tools::{normalize_tool_name, to_claude_code_name};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// Remove unpaired Unicode surrogate characters, mirroring pi's
/// `sanitizeSurrogates` (`utils/sanitize-unicode.ts`). Rust `String`s are always
/// valid UTF-8, so lone surrogates cannot occur and this is the identity on
/// every input Rust can represent; it exists to keep the port's call sites
/// aligned with pi's.
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
// transformMessages (`api/transform-messages.ts`)
// ---------------------------------------------------------------------------

/// Whether a content block is an image.
fn is_image(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Image { .. })
}

/// Replace image blocks with a placeholder text block, collapsing runs of
/// images into a single placeholder, mirroring pi's
/// `replaceImagesWithPlaceholder` (`transform-messages.ts:15`).
fn replace_images_with_placeholder(
    content: &[ContentBlock],
    placeholder: &str,
) -> Vec<ContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        if is_image(block) {
            if !previous_was_placeholder {
                result.push(ContentBlock::Text {
                    text: placeholder.to_string(),
                    text_signature: None,
                });
            }
            previous_was_placeholder = true;
            continue;
        }
        let is_placeholder =
            matches!(block, ContentBlock::Text { text, .. } if text == placeholder);
        result.push(block.clone());
        previous_was_placeholder = is_placeholder;
    }
    result
}

/// Downgrade images to placeholders for non-vision models, mirroring pi's
/// `downgradeUnsupportedImages` (`transform-messages.ts:35`).
fn downgrade_unsupported_images(
    messages: &[Message],
    model: &Model<AnthropicMessagesCompat>,
) -> Vec<Message> {
    if model.input.contains(&crate::types::Modality::Image) {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|msg| match msg {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => Message::User(UserMessage {
                    content: UserContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    )),
                    ..user.clone()
                }),
                UserContent::Text(_) => msg.clone(),
            },
            Message::ToolResult(result) => Message::ToolResult(ToolResultMessage {
                content: replace_images_with_placeholder(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                ),
                ..result.clone()
            }),
            Message::Assistant(_) => msg.clone(),
        })
        .collect()
}

/// Whether an assistant message was produced by the same model this request
/// targets (`transform-messages.ts:95`).
fn is_same_model(assistant: &AssistantMessage, model: &Model<AnthropicMessagesCompat>) -> bool {
    assistant.provider == model.provider
        && assistant.api == model.api
        && assistant.model == model.id
}

/// Transform assistant content for replay, mirroring the assistant branch of
/// pi's `transformMessages` first pass (`transform-messages.ts:100-148`).
fn transform_assistant_content(
    assistant: &AssistantMessage,
    same_model: bool,
    tool_call_id_map: &mut Vec<(String, String)>,
) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    for block in &assistant.content {
        match block {
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                redacted,
            } => {
                if redacted == &Some(true) {
                    if same_model {
                        out.push(block.clone());
                    }
                    continue;
                }
                let has_signature = thinking_signature
                    .as_deref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                if same_model && has_signature {
                    out.push(block.clone());
                    continue;
                }
                if thinking.trim().is_empty() {
                    continue;
                }
                if same_model {
                    out.push(block.clone());
                } else {
                    out.push(ContentBlock::Text {
                        text: thinking.clone(),
                        text_signature: None,
                    });
                }
            }
            ContentBlock::Text { text, .. } => {
                if same_model {
                    out.push(block.clone());
                } else {
                    out.push(ContentBlock::Text {
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
                let mut new_thought_signature = thought_signature.clone();
                if !same_model {
                    new_thought_signature = None;
                }
                let mut new_id = id.clone();
                if !same_model {
                    let normalized = normalize_tool_call_id(id);
                    if &normalized != id {
                        tool_call_id_map.push((id.clone(), normalized.clone()));
                        new_id = normalized;
                    }
                }
                out.push(ContentBlock::ToolCall {
                    id: new_id,
                    name: name.clone(),
                    arguments: arguments.clone(),
                    thought_signature: new_thought_signature,
                });
            }
            ContentBlock::Image { .. } | ContentBlock::Unknown => out.push(block.clone()),
        }
    }
    out
}

/// Normalize a conversation for a target model, mirroring pi's
/// `transformMessages` (`transform-messages.ts:64`): downgrade unsupported
/// images, transform assistant content and tool-call ids, and insert synthetic
/// tool results for orphaned tool calls. The `normalizeToolCallId` argument pi
/// threads is always its own `normalizeToolCallId`, applied here on the
/// cross-model path.
pub fn transform_messages(
    messages: &[Message],
    model: &Model<AnthropicMessagesCompat>,
) -> Vec<Message> {
    let image_aware = downgrade_unsupported_images(messages, model);

    // First pass: transform assistant content; map tool-call ids as we go so a
    // later tool-result message can adopt the normalized id.
    let mut tool_call_id_map: Vec<(String, String)> = Vec::new();
    let mut transformed: Vec<Message> = Vec::new();
    for msg in &image_aware {
        match msg {
            Message::User(_) => transformed.push(msg.clone()),
            Message::ToolResult(result) => {
                let mapped = tool_call_id_map
                    .iter()
                    .find(|(from, _)| from == &result.tool_call_id)
                    .map(|(_, to)| to.clone());
                match mapped {
                    Some(normalized) if normalized != result.tool_call_id => {
                        transformed.push(Message::ToolResult(ToolResultMessage {
                            tool_call_id: normalized,
                            ..result.clone()
                        }));
                    }
                    _ => transformed.push(msg.clone()),
                }
            }
            Message::Assistant(assistant) => {
                let same_model = is_same_model(assistant, model);
                let content =
                    transform_assistant_content(assistant, same_model, &mut tool_call_id_map);
                transformed.push(Message::Assistant(AssistantMessage {
                    content,
                    ..assistant.clone()
                }));
            }
        }
    }

    // Second pass: insert synthetic empty tool results for orphaned tool calls
    // and drop errored/aborted assistant turns.
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<(String, String)> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant) => {
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                if matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    continue;
                }
                let tool_calls: Vec<(String, String)> = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids = HashSet::new();
                }
                result.push(msg);
            }
            Message::ToolResult(tool_result) => {
                existing_tool_result_ids.insert(tool_result.tool_call_id.clone());
                result.push(msg);
            }
            Message::User(_) => {
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                result.push(msg);
            }
        }
    }
    insert_synthetic_tool_results(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
    );

    result
}

/// Flush pending orphaned tool calls into synthetic error tool results
/// (`transform-messages.ts:163`). The synthetic message's timestamp is not part
/// of the request shape, so it is fixed at `0`.
fn insert_synthetic_tool_results(
    result: &mut Vec<Message>,
    pending_tool_calls: &mut Vec<(String, String)>,
    existing_tool_result_ids: &mut HashSet<String>,
) {
    if pending_tool_calls.is_empty() {
        return;
    }
    for (id, name) in pending_tool_calls.iter() {
        if !existing_tool_result_ids.contains(id) {
            result.push(Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
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
    pending_tool_calls.clear();
    *existing_tool_result_ids = HashSet::new();
}

// ---------------------------------------------------------------------------
// convertContentBlocks / convertToolResult (`anthropic-messages.ts`)
// ---------------------------------------------------------------------------

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
