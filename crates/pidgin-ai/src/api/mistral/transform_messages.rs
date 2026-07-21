//! Cross-provider message normalization, ported from pi-ai's
//! `packages/ai/src/api/transform-messages.ts` at pinned commit `3da591ab`.
//!
//! DE-DUP / REBASE POINT: this is a **driver-local** copy. `transform-messages.ts`
//! is a shared pi helper (used by the Google dialects too) and belongs under a
//! shared `crate::api` module owned by a sibling. It lives here only because no
//! shared Rust port exists yet; when one lands, delete this file and re-point
//! [`super::transform_messages`] at the shared module.

use crate::types::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultMessage, UserContent,
};

/// The non-vision placeholder pi substitutes for images in user turns.
const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
/// The non-vision placeholder pi substitutes for images in tool-result turns.
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// The minimal model identity `transformMessages` reads: it compares the source
/// assistant message's `provider`/`api`/`model` against these to decide whether a
/// history turn came from the same model (`isSameModel`).
pub struct ModelIdentity<'a> {
    pub id: &'a str,
    pub api: &'a str,
    pub provider: &'a str,
    pub supports_images: bool,
}

fn replace_images_with_placeholder(
    content: &[ContentBlock],
    placeholder: &str,
) -> Vec<ContentBlock> {
    let mut result: Vec<ContentBlock> = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
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
            other => {
                let is_placeholder = matches!(
                    other,
                    ContentBlock::Text { text, .. } if text == placeholder
                );
                result.push(other.clone());
                previous_was_placeholder = is_placeholder;
            }
        }
    }

    result
}

fn downgrade_unsupported_images(messages: &[Message], supports_images: bool) -> Vec<Message> {
    if supports_images {
        return messages.to_vec();
    }

    messages
        .iter()
        .map(|msg| match msg {
            Message::User(user) => {
                if let UserContent::Blocks(blocks) = &user.content {
                    let mut next = user.clone();
                    next.content = UserContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    ));
                    Message::User(next)
                } else {
                    msg.clone()
                }
            }
            Message::ToolResult(tool_result) => {
                let mut next = tool_result.clone();
                next.content = replace_images_with_placeholder(
                    &tool_result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                );
                Message::ToolResult(next)
            }
            other => other.clone(),
        })
        .collect()
}

fn is_same_model(assistant: &AssistantMessage, model: &ModelIdentity) -> bool {
    assistant.provider == model.provider
        && assistant.api == model.api
        && assistant.model == model.id
}

/// Normalize tool-call IDs and thinking blocks for cross-provider compatibility,
/// then insert synthetic empty tool results for orphaned tool calls.
///
/// `normalize_tool_call_id` mirrors pi's optional `normalizeToolCallId` callback
/// (Mistral passes its 9-char normalizer). `timestamp` seeds the synthetic
/// tool-result messages pi stamps with `Date.now()`; the value is never emitted
/// on the wire.
pub fn transform_messages(
    messages: &[Message],
    model: &ModelIdentity,
    normalize_tool_call_id: &mut dyn FnMut(&str) -> String,
    timestamp: i64,
) -> Vec<Message> {
    // Build a map of original tool call IDs to normalized IDs.
    let mut tool_call_id_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // pi normalizes null/undefined content to `[]`; in the Rust boundary types
    // content is already non-null, so the `imageAwareMessages` step is the first
    // transform.
    let image_aware = downgrade_unsupported_images(messages, model.supports_images);

    // First pass: transform assistant thinking/text/tool-call blocks and
    // normalize tool-result IDs.
    let transformed: Vec<Message> = image_aware
        .iter()
        .map(|msg| match msg {
            Message::User(_) => msg.clone(),
            Message::ToolResult(tool_result) => {
                if let Some(normalized_id) = tool_call_id_map.get(&tool_result.tool_call_id) {
                    if normalized_id != &tool_result.tool_call_id {
                        let mut next = tool_result.clone();
                        next.tool_call_id = normalized_id.clone();
                        return Message::ToolResult(next);
                    }
                }
                msg.clone()
            }
            Message::Assistant(assistant) => {
                let same_model = is_same_model(assistant, model);
                let mut transformed_content: Vec<ContentBlock> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking {
                            thinking,
                            thinking_signature,
                            redacted,
                        } => {
                            if redacted == &Some(true) {
                                if same_model {
                                    transformed_content.push(block.clone());
                                }
                                continue;
                            }
                            let has_signature =
                                thinking_signature.as_deref().is_some_and(|s| !s.is_empty());
                            if same_model && has_signature {
                                transformed_content.push(block.clone());
                                continue;
                            }
                            if thinking.trim().is_empty() {
                                continue;
                            }
                            if same_model {
                                transformed_content.push(block.clone());
                            } else {
                                transformed_content.push(ContentBlock::Text {
                                    text: thinking.clone(),
                                    text_signature: None,
                                });
                            }
                        }
                        ContentBlock::Text { text, .. } => {
                            if same_model {
                                transformed_content.push(block.clone());
                            } else {
                                transformed_content.push(ContentBlock::Text {
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
                            let mut new_thought_signature = thought_signature.clone();
                            if !same_model && thought_signature.is_some() {
                                new_thought_signature = None;
                            }
                            if !same_model {
                                let normalized_id = normalize_tool_call_id(id);
                                if &normalized_id != id {
                                    tool_call_id_map.insert(id.clone(), normalized_id.clone());
                                    new_id = normalized_id;
                                }
                            }
                            transformed_content.push(ContentBlock::ToolCall {
                                id: new_id,
                                name: name.clone(),
                                arguments: arguments.clone(),
                                thought_signature: new_thought_signature,
                            });
                        }
                        other => transformed_content.push(other.clone()),
                    }
                }

                let mut next = assistant.clone();
                next.content = transformed_content;
                Message::Assistant(next)
            }
        })
        .collect();

    // Second pass: insert synthetic empty tool results for orphaned tool calls.
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<(String, String)> = Vec::new();
    let mut existing_tool_result_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    fn insert_synthetic(
        result: &mut Vec<Message>,
        pending: &mut Vec<(String, String)>,
        existing: &mut std::collections::HashSet<String>,
        timestamp: i64,
    ) {
        if pending.is_empty() {
            return;
        }
        for (id, name) in pending.iter() {
            if !existing.contains(id) {
                result.push(Message::ToolResult(ToolResultMessage {
                    role: Default::default(),
                    tool_call_id: id.clone(),
                    tool_name: name.clone(),
                    content: vec![ContentBlock::Text {
                        text: "No result provided".to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    added_tool_names: None,
                    is_error: true,
                    timestamp,
                }));
            }
        }
        pending.clear();
        *existing = std::collections::HashSet::new();
    }

    for msg in &transformed {
        match msg {
            Message::Assistant(assistant) => {
                insert_synthetic(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    timestamp,
                );

                // Skip errored/aborted assistant messages entirely.
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
                    existing_tool_result_ids = std::collections::HashSet::new();
                }

                result.push(msg.clone());
            }
            Message::ToolResult(tool_result) => {
                existing_tool_result_ids.insert(tool_result.tool_call_id.clone());
                result.push(msg.clone());
            }
            Message::User(_) => {
                insert_synthetic(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    timestamp,
                );
                result.push(msg.clone());
            }
        }
    }

    insert_synthetic(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
        timestamp,
    );

    result
}
