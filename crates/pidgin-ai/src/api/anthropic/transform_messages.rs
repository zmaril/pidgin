// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `api/transform-messages.ts` (`transformMessages` and its helpers). The per-role
// and per-block arms are walls of near-identical branch/serde shaping by design;
// the clone detector reads them as duplicates, but factoring them would distort
// the byte-faithful port, so the repetition is intentional.
//! Cross-model message normalization, ported from pi-ai's
//! `packages/ai/src/api/transform-messages.ts` at pinned commit `3da591ab`.
//!
//! [`transform_messages`] downgrades unsupported images, transforms assistant
//! content and tool-call ids, and inserts synthetic tool results for orphaned
//! tool calls, ahead of the message conversion in [`super::content`]. It borrows
//! [`normalize_tool_call_id`](super::content::normalize_tool_call_id) from the
//! anthropic-messages side, matching pi, where `transformMessages` receives
//! `normalizeToolCallId` from `anthropic-messages.ts`.

use std::collections::HashSet;

use crate::types::{
    AnthropicMessagesCompat, AssistantMessage, ContentBlock, Message, Model, StopReason,
    ToolResultMessage, ToolResultRole, UserContent, UserMessage,
};

use super::content::normalize_tool_call_id;

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

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
