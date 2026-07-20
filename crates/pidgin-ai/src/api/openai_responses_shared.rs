// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `openai-responses-shared.ts`: the named-event dispatch arms
// (`reasoning_summary_text.delta` / `reasoning_text.delta` both append to a
// thinking slot and push a matching `thinking_delta` event; `output_text.delta`
// / `refusal.delta` both append to a text slot) share pi's hand-rolled slot
// lookup + push shape by design, and the `output_item.done` finalize arms mirror
// pi's `if/else` chain. The clone detector reads these mirrored arms as
// duplicates; factoring them would distort the byte-faithful port.
//! OpenAI **Responses API** named-event stream processor, ported from pi-ai's
//! `packages/ai/src/api/openai-responses-shared.ts` at pinned commit `3da591ab`.
//!
//! Unlike the Anthropic Messages dialect, the Responses API emits *named* events
//! (`response.output_item.added`, `response.output_text.delta`,
//! `response.completed`, ...) rather than indexed chunk deltas. This module is
//! the event-walker core: it takes an already-decoded slice of
//! `ResponseStreamEvent` JSON values (exactly what pi feeds through the OpenAI
//! SDK's async stream) and reproduces pi's `processResponsesStream` dispatch —
//! creating output slots keyed by `output_index`, mapping deltas onto
//! thinking/text/toolCall blocks, repairing streamed tool-argument JSON,
//! accumulating usage, computing cost (with the service-tier multiplier applied
//! post-cost), and mapping the terminal `status` to a stop reason.
//!
//! Like the Anthropic port, the design is *eager*: instead of pi's throw-based
//! control flow we return a [`StreamOutcome`] carrying the full
//! [`AssistantMessageEvent`] sequence plus the accumulated [`AssistantMessage`].
//! Where pi throws (an `error` event, a `response.failed`, or a stream that ends
//! before a terminal response event) we terminate the event sequence with an
//! `error` event carrying that message verbatim.
//!
//! This module also ports the request-side message/tool conversion helpers
//! (`convertResponsesMessages`, `convertResponsesTools`, `transformMessages`) and
//! the `encodeTextSignatureV1` / `shortHash` utilities they depend on.

use std::collections::HashMap;
use std::ops::ControlFlow;

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::api::openai_responses::OpenAIResponsesModel;
use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, Message, Modality,
    StopReason, Usage, UsageCost,
};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

// =============================================================================
// Utilities
// =============================================================================

/// Fast deterministic hash to shorten long strings (pi's `shortHash`,
/// `utils/hash.ts`). Iterates by UTF-16 code unit and uses 32-bit wrapping
/// multiplies (`Math.imul`) so the output matches pi byte-for-byte.
pub fn short_hash(input: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for ch in input.encode_utf16() {
        let code = ch as u32;
        h1 = (h1 ^ code).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ code).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", to_base36(h2), to_base36(h1))
}

fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// Encode a v1 text signature (pi's `encodeTextSignatureV1`), preserving the
/// `{"v":1,"id":...,"phase":...}` key order pi's `JSON.stringify` emits.
pub fn encode_text_signature_v1(id: &str, phase: Option<&str>) -> String {
    let id_json = serde_json::to_string(id).unwrap();
    match phase {
        Some(phase) => {
            let phase_json = serde_json::to_string(phase).unwrap();
            format!("{{\"v\":1,\"id\":{id_json},\"phase\":{phase_json}}}")
        }
        None => format!("{{\"v\":1,\"id\":{id_json}}}"),
    }
}

/// Parse a text signature back into its id + optional phase (pi's
/// `parseTextSignature`): JSON `{v:1,id,phase?}` shape or a legacy plain-string
/// id.
fn parse_text_signature(signature: Option<&str>) -> Option<(String, Option<String>)> {
    let signature = signature?;
    if signature.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<Value>(signature) {
            let v_is_1 = parsed.get("v").and_then(Value::as_i64) == Some(1);
            let id = parsed.get("id").and_then(Value::as_str);
            if v_is_1 {
                if let Some(id) = id {
                    let phase = parsed.get("phase").and_then(Value::as_str);
                    return match phase {
                        Some("commentary") => {
                            Some((id.to_string(), Some("commentary".to_string())))
                        }
                        Some("final_answer") => {
                            Some((id.to_string(), Some("final_answer".to_string())))
                        }
                        _ => Some((id.to_string(), None)),
                    };
                }
            }
        }
        // Fall through to legacy plain-string handling.
    }
    Some((signature.to_string(), None))
}

// =============================================================================
// Message conversion
// =============================================================================

/// Convert pidgin-ai [`Message`]s into OpenAI Responses input-item JSON, ported
/// from pi's `convertResponsesMessages`. `allowed_tool_call_providers` mirrors
/// pi's `OPENAI_TOOL_CALL_PROVIDERS` / `AZURE_TOOL_CALL_PROVIDERS` set that gates
/// composite `call_id|item_id` normalization.
pub fn convert_responses_messages(
    model: &OpenAIResponsesModel,
    context_messages: &[Message],
    system_prompt: Option<&str>,
    allowed_tool_call_providers: &[&str],
    include_system_prompt: bool,
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();

    let transformed = transform_messages(context_messages, model, allowed_tool_call_providers);

    if include_system_prompt {
        if let Some(system_prompt) = system_prompt.filter(|s| !s.is_empty()) {
            let supports_developer_role = model
                .compat
                .as_ref()
                .and_then(|c| c.supports_developer_role)
                .unwrap_or(true);
            let role = if model.reasoning && supports_developer_role {
                "developer"
            } else {
                "system"
            };
            messages.push(json!({ "role": role, "content": system_prompt }));
        }
    }

    let model_supports_images = model.input.contains(&Modality::Image);
    let mut msg_index = 0usize;
    for msg in &transformed {
        match msg {
            Message::User(user) => {
                let content = user_content_items(&user.content);
                if content.is_empty() {
                    // A bare-string user message still pushes a single input_text.
                    if let crate::types::UserContent::Text(text) = &user.content {
                        messages.push(json!({
                            "role": "user",
                            "content": [{ "type": "input_text", "text": text }],
                        }));
                    }
                } else {
                    messages.push(json!({ "role": "user", "content": content }));
                }
            }
            Message::Assistant(assistant) => {
                let output = assistant_output_items(assistant, model, msg_index);
                if output.is_empty() {
                    msg_index += 1;
                    continue;
                }
                messages.extend(output);
            }
            Message::ToolResult(tool_result) => {
                let text_result: String = tool_result
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = tool_result
                    .content
                    .iter()
                    .any(|c| matches!(c, ContentBlock::Image { .. }));
                let has_text = !text_result.is_empty();
                let call_id = split_composite_id(&tool_result.tool_call_id).0;

                let output: Value = if has_images && model_supports_images {
                    let mut parts: Vec<Value> = Vec::new();
                    if has_text {
                        parts.push(json!({ "type": "input_text", "text": text_result }));
                    }
                    for block in &tool_result.content {
                        if let ContentBlock::Image { data, mime_type } = block {
                            parts.push(json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{mime_type};base64,{data}"),
                            }));
                        }
                    }
                    Value::Array(parts)
                } else {
                    let text = if has_text {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };
                    Value::String(text)
                };

                messages.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
                // NOTE: deferred-tool `tool_search_call` / `tool_search_output`
                // emission (pi's addedToolNames path) is not modelled here; the
                // Rust boundary does not yet carry the deferred-tool registry.
            }
        }
        msg_index += 1;
    }

    messages
}

fn user_content_items(content: &crate::types::UserContent) -> Vec<Value> {
    match content {
        crate::types::UserContent::Text(_) => Vec::new(),
        crate::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|item| match item {
                ContentBlock::Text { text, .. } => {
                    json!({ "type": "input_text", "text": text })
                }
                ContentBlock::Image { data, mime_type } => json!({
                    "type": "input_image",
                    "detail": "auto",
                    "image_url": format!("data:{mime_type};base64,{data}"),
                }),
                _ => json!({ "type": "input_text", "text": "" }),
            })
            .collect(),
    }
}

fn assistant_output_items(
    assistant: &AssistantMessage,
    model: &OpenAIResponsesModel,
    msg_index: usize,
) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    let is_different_model = assistant.model != model.id
        && assistant.provider == model.provider
        && assistant.api == model.api;
    let mut text_block_index = 0usize;

    for block in &assistant.content {
        match block {
            ContentBlock::Thinking {
                thinking_signature, ..
            } => {
                if let Some(sig) = thinking_signature.as_ref().filter(|s| !s.is_empty()) {
                    if let Ok(reasoning_item) = serde_json::from_str::<Value>(sig) {
                        output.push(reasoning_item);
                    }
                }
            }
            ContentBlock::Text {
                text,
                text_signature,
            } => {
                let parsed = parse_text_signature(text_signature.as_deref());
                let fallback_message_id = if text_block_index == 0 {
                    format!("msg_pi_{msg_index}")
                } else {
                    format!("msg_pi_{msg_index}_{text_block_index}")
                };
                text_block_index += 1;

                let mut msg_id = parsed.as_ref().map(|(id, _)| id.clone());
                match &msg_id {
                    None => msg_id = Some(fallback_message_id),
                    Some(id) if id.chars().count() > 64 => {
                        msg_id = Some(format!("msg_{}", short_hash(id)));
                    }
                    _ => {}
                }
                let phase = parsed.and_then(|(_, phase)| phase);

                let mut item = json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                    "status": "completed",
                    "id": msg_id,
                });
                if let Some(phase) = phase {
                    item.as_object_mut()
                        .unwrap()
                        .insert("phase".to_string(), Value::String(phase));
                }
                output.push(item);
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                let (call_id, item_id_raw) = split_composite_id(id);
                let mut item_id: Option<String> = item_id_raw;
                if is_different_model && item_id.as_deref().is_some_and(|s| s.starts_with("fc_")) {
                    item_id = None;
                }
                output.push(json!({
                    "type": "function_call",
                    "id": item_id,
                    "call_id": call_id,
                    "name": name,
                    "arguments": serde_json::to_string(arguments).unwrap(),
                }));
            }
            _ => {}
        }
    }

    output
}

/// Split a composite `call_id|item_id` into its parts. When no `|` is present
/// the whole string is the call id and the item id is absent.
fn split_composite_id(id: &str) -> (String, Option<String>) {
    match id.split_once('|') {
        Some((call_id, item_id)) => (call_id.to_string(), Some(item_id.to_string())),
        None => (id.to_string(), None),
    }
}

// =============================================================================
// Tool conversion
// =============================================================================

/// Convert tools into the OpenAI Responses **flat** function-tool shape
/// (`{type:"function", name, description, parameters, strict, defer_loading?}`),
/// ported from pi's `convertResponsesTools`. Note this is *not* nested under a
/// `function` key (that is the completions dialect).
pub fn convert_responses_tools(tools: &[Value], strict: bool, defer_loading: bool) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut obj = Map::new();
            obj.insert("type".to_string(), Value::String("function".to_string()));
            obj.insert(
                "name".to_string(),
                tool.get("name").cloned().unwrap_or(Value::Null),
            );
            obj.insert(
                "description".to_string(),
                tool.get("description").cloned().unwrap_or(Value::Null),
            );
            obj.insert(
                "parameters".to_string(),
                tool.get("parameters").cloned().unwrap_or(Value::Null),
            );
            obj.insert("strict".to_string(), Value::Bool(strict));
            if defer_loading {
                obj.insert("defer_loading".to_string(), Value::Bool(true));
            }
            Value::Object(obj)
        })
        .collect()
}

// =============================================================================
// transformMessages (cross-provider normalization)
// =============================================================================

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let normalized: String = if sanitized.chars().count() > 64 {
        sanitized.chars().take(64).collect()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_string()
}

fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if normalized.chars().count() > 64 {
        normalized.chars().take(64).collect()
    } else {
        normalized
    }
}

/// Port of the `normalizeToolCallId` closure inside pi's
/// `convertResponsesMessages`, applied through `transformMessages`.
fn normalize_tool_call_id(
    id: &str,
    model: &OpenAIResponsesModel,
    source: &AssistantMessage,
    allowed: &[&str],
) -> String {
    if !allowed.contains(&model.provider.as_str()) {
        return normalize_id_part(id);
    }
    if !id.contains('|') {
        return normalize_id_part(id);
    }
    let (call_id, item_id) = id.split_once('|').unwrap();
    let normalized_call_id = normalize_id_part(call_id);
    let is_foreign = source.provider != model.provider || source.api != model.api;
    let mut normalized_item_id = if is_foreign {
        build_foreign_responses_item_id(item_id)
    } else {
        normalize_id_part(item_id)
    };
    if !normalized_item_id.starts_with("fc_") {
        normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
    }
    format!("{normalized_call_id}|{normalized_item_id}")
}

fn is_same_model(msg: &AssistantMessage, model: &OpenAIResponsesModel) -> bool {
    msg.provider == model.provider && msg.api == model.api && msg.model == model.id
}

/// Replace image blocks with a text placeholder, collapsing consecutive images
/// (pi's `replaceImagesWithPlaceholder`).
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
                let is_placeholder =
                    matches!(other, ContentBlock::Text { text, .. } if text == placeholder);
                result.push(other.clone());
                previous_was_placeholder = is_placeholder;
            }
        }
    }
    result
}

/// Port of pi's `transformMessages`: unsupported-image downgrade, thinking-block
/// handling for cross-model replay, tool-call id normalization, and synthetic
/// tool-result insertion for orphaned tool calls.
fn transform_messages(
    messages: &[Message],
    model: &OpenAIResponsesModel,
    allowed: &[&str],
) -> Vec<Message> {
    let model_supports_images = model.input.contains(&Modality::Image);
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();

    // First pass: downgrade images + transform assistant content + normalize
    // tool-result ids using the map populated by earlier assistant messages.
    let mut transformed: Vec<Message> = Vec::new();
    for msg in messages {
        match msg {
            Message::User(user) => {
                if model_supports_images {
                    transformed.push(Message::User(user.clone()));
                } else if let crate::types::UserContent::Blocks(blocks) = &user.content {
                    let mut user = user.clone();
                    user.content = crate::types::UserContent::Blocks(
                        replace_images_with_placeholder(blocks, NON_VISION_USER_IMAGE_PLACEHOLDER),
                    );
                    transformed.push(Message::User(user));
                } else {
                    transformed.push(Message::User(user.clone()));
                }
            }
            Message::ToolResult(tool_result) => {
                let mut tool_result = tool_result.clone();
                if !model_supports_images {
                    tool_result.content = replace_images_with_placeholder(
                        &tool_result.content,
                        NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                    );
                }
                if let Some(normalized) = tool_call_id_map.get(&tool_result.tool_call_id) {
                    if normalized != &tool_result.tool_call_id {
                        tool_result.tool_call_id = normalized.clone();
                    }
                }
                transformed.push(Message::ToolResult(tool_result));
            }
            Message::Assistant(assistant) => {
                let same_model = is_same_model(assistant, model);
                let mut new_content: Vec<ContentBlock> = Vec::new();
                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking {
                            thinking,
                            thinking_signature,
                            redacted,
                        } => {
                            if redacted == &Some(true) {
                                if same_model {
                                    new_content.push(block.clone());
                                }
                                continue;
                            }
                            if same_model && thinking_signature.is_some() {
                                new_content.push(block.clone());
                                continue;
                            }
                            if thinking.trim().is_empty() {
                                continue;
                            }
                            if same_model {
                                new_content.push(block.clone());
                            } else {
                                new_content.push(ContentBlock::Text {
                                    text: thinking.clone(),
                                    text_signature: None,
                                });
                            }
                        }
                        ContentBlock::Text { text, .. } => {
                            if same_model {
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
                            let mut new_thought_signature = thought_signature.clone();
                            if !same_model {
                                new_thought_signature = None;
                            }
                            let mut new_id = id.clone();
                            if !same_model {
                                let normalized =
                                    normalize_tool_call_id(id, model, assistant, allowed);
                                if normalized != *id {
                                    tool_call_id_map.insert(id.clone(), normalized.clone());
                                    new_id = normalized;
                                }
                            }
                            new_content.push(ContentBlock::ToolCall {
                                id: new_id,
                                name: name.clone(),
                                arguments: arguments.clone(),
                                thought_signature: new_thought_signature,
                            });
                        }
                        other => new_content.push(other.clone()),
                    }
                }
                let mut assistant = assistant.clone();
                assistant.content = new_content;
                transformed.push(Message::Assistant(assistant));
            }
        }
    }

    // Second pass: insert synthetic empty tool results for orphaned tool calls.
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<(String, String)> = Vec::new();
    let mut existing_tool_result_ids: Vec<String> = Vec::new();

    fn flush_synthetic(
        result: &mut Vec<Message>,
        pending: &mut Vec<(String, String)>,
        existing: &mut Vec<String>,
    ) {
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
    }

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant) => {
                flush_synthetic(
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
                    existing_tool_result_ids = Vec::new();
                }
                result.push(msg);
            }
            Message::ToolResult(tool_result) => {
                existing_tool_result_ids.push(tool_result.tool_call_id.clone());
                result.push(msg);
            }
            Message::User(_) => {
                flush_synthetic(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                result.push(msg);
            }
        }
    }
    flush_synthetic(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
    );

    result
}

// =============================================================================
// Stream processing
// =============================================================================

/// The result of processing a Responses stream: the full event sequence and the
/// accumulated final message (mirrors the Anthropic driver's [`StreamOutcome`]).
#[derive(Debug, Clone, Serialize)]
pub struct StreamOutcome {
    pub events: Vec<AssistantMessageEvent>,
    pub message: AssistantMessage,
}

/// Per-request options the stream processor reads. Only `service_tier`
/// participates in the post-cost pricing multiplier today (pi's
/// `applyServiceTierPricing`).
#[derive(Debug, Clone, Default)]
pub struct ResponsesStreamOptions {
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
}

#[derive(Debug, Clone)]
struct Slot {
    kind: SlotKind,
    content_index: usize,
    partial_json: String,
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Process a slice of *already-decoded* `ResponseStreamEvent` values for `model`,
/// returning the [`StreamOutcome`]. Ports pi's `processResponsesStream` under an
/// eager, throw-free design.
///
/// This is the decoded-events boundary (the napi shim and the existing unit
/// tests feed a `Vec<Value>`); it drives the SAME [`OpenAIResponsesSseDecoder`]
/// the incremental driver runs — one source of truth — so its events and final
/// message are byte-identical to [`parse_responses_sse_stream`] and the
/// chunk-by-chunk [`AssistantEventReader`] path.
pub fn process_responses_stream(
    events_json: &[Value],
    model: &OpenAIResponsesModel,
    options: &ResponsesStreamOptions,
    timestamp: i64,
) -> StreamOutcome {
    let mut decoder = OpenAIResponsesSseDecoder::new(model.clone(), options.clone(), timestamp);
    let mut events: Vec<AssistantMessageEvent> = Vec::new();

    // Mirrors pi's `stream()`: the `start` event precedes the dispatch loop.
    decoder.ensure_started(&mut events);

    for event in events_json {
        if decoder.push_value(event, &mut events).is_break() {
            break;
        }
    }

    let message = decoder.finalize(&mut events);
    StreamOutcome { events, message }
}

/// Process a whole OpenAI Responses SSE body for `model`, returning the
/// [`StreamOutcome`]. This is the buffered driver's whole-body parser, rewired
/// onto the SAME [`OpenAIResponsesSseDecoder`] the incremental driver uses (via a
/// one-chunk [`AssistantEventReader`]), so feeding the whole body is byte-identical
/// to feeding the reader chunk-by-chunk.
pub fn parse_responses_sse_stream(
    body: &str,
    model: &OpenAIResponsesModel,
    options: &ResponsesStreamOptions,
    timestamp: i64,
) -> StreamOutcome {
    let decoder = OpenAIResponsesSseDecoder::new(model.clone(), options.clone(), timestamp);
    let mut reader = AssistantEventReader::new(
        Box::new(std::iter::once(Ok(body.as_bytes().to_vec()))),
        Box::new(decoder),
    );
    let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();
    let message = match reader.result() {
        Some(Ok(message)) | Some(Err(message)) => message.clone(),
        // The reader always finalizes once drained (EOF is bounded), so a
        // fully-collected reader has a terminal result.
        None => unreachable!("AssistantEventReader finalizes before iteration ends"),
    };
    StreamOutcome { events, message }
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

/// The mutable accumulation state pi's `processResponsesStream` threads through
/// its dispatch loop (`slots`, the reasoning content-index map, and whether a
/// terminal response event was seen). Held by [`OpenAIResponsesSseDecoder`] so
/// the buffered and incremental paths share one dispatch.
#[derive(Debug, Default)]
struct DispatchState {
    slots: HashMap<i64, Slot>,
    /// reasoning content-index by item id, for encrypted_content backfill.
    reasoning_index_by_id: HashMap<String, usize>,
    saw_terminal: bool,
}

/// Dispatch ONE decoded Responses event, mutating `output`/`state` and pushing
/// any assistant events. Returns `Some(message)` when the event is a hard
/// terminal error (an `error` event or a `response.failed`) — the points where
/// pi's `processResponsesStream` throws. The "stream ended before a terminal
/// response event" throw is a stream-end concern and lives in the decoder's
/// `finalize`, keyed on [`DispatchState::saw_terminal`].
fn dispatch_event(
    event: &Value,
    model: &OpenAIResponsesModel,
    options: &ResponsesStreamOptions,
    output: &mut AssistantMessage,
    state: &mut DispatchState,
    events: &mut Vec<AssistantMessageEvent>,
) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    let output_index = event
        .get("output_index")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    match event_type {
        "response.created" => {
            if let Some(id) = event
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(Value::as_str)
            {
                output.response_id = Some(id.to_string());
            }
        }
        "response.output_item.added" => {
            if let Some(item) = event.get("item") {
                create_slot(output_index, item, output, &mut state.slots, events);
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            append_thinking(output_index, delta, output, &state.slots, events);
        }
        "response.reasoning_summary_part.done" => {
            append_thinking(output_index, "\n\n", output, &state.slots, events);
        }
        "response.output_text.delta" | "response.refusal.delta" => {
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            append_text(output_index, delta, output, &state.slots, events);
        }
        "response.function_call_arguments.delta" => {
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            if let Some(slot) = state.slots.get_mut(&output_index) {
                if slot.kind == SlotKind::ToolCall {
                    slot.partial_json.push_str(delta);
                    let parsed = parse_streaming_json(Some(&slot.partial_json));
                    set_tool_arguments(output, slot.content_index, parsed);
                    events.push(AssistantMessageEvent::ToolcallDelta {
                        content_index: slot.content_index as u32,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
        }
        "response.function_call_arguments.done" => {
            let arguments = event
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(slot) = state.slots.get_mut(&output_index) {
                if slot.kind == SlotKind::ToolCall {
                    let previous = slot.partial_json.clone();
                    slot.partial_json = arguments.clone();
                    let parsed = parse_streaming_json(Some(&slot.partial_json));
                    set_tool_arguments(output, slot.content_index, parsed);
                    let content_index = slot.content_index;
                    if let Some(delta) = arguments.strip_prefix(&previous) {
                        if !delta.is_empty() {
                            events.push(AssistantMessageEvent::ToolcallDelta {
                                content_index: content_index as u32,
                                delta: delta.to_string(),
                                partial: output.clone(),
                            });
                        }
                    }
                }
            }
        }
        "response.output_item.done" => {
            let item = event.get("item").cloned().unwrap_or(Value::Null);
            // getOrCreateSlot
            if !state.slots.contains_key(&output_index) {
                create_slot(output_index, &item, output, &mut state.slots, events);
            }
            finalize_output_item(
                output_index,
                &item,
                output,
                &mut state.slots,
                &mut state.reasoning_index_by_id,
                events,
            );
        }
        "response.completed" | "response.incomplete" => {
            if let Some(response) = event.get("response") {
                finalize_response(
                    response,
                    model,
                    options,
                    output,
                    &state.reasoning_index_by_id,
                );
            }
            state.saw_terminal = true;
        }
        "error" => {
            let code = event.get("code").and_then(Value::as_str).unwrap_or("");
            let message = event.get("message").and_then(Value::as_str).unwrap_or("");
            return Some(format!("Error Code {code}: {message}"));
        }
        "response.failed" => {
            // pi sets `sawTerminalResponseEvent` here before throwing; the eager
            // port returns the error message immediately, so the flag would be
            // dead. The early return is the terminal signal.
            return Some(response_failed_message(event.get("response")));
        }
        _ => {}
    }

    None
}

/// The incremental OpenAI Responses SSE decoder: the single source of truth for
/// turning wire frames (or already-decoded event values) into assistant events
/// and the accumulated message.
///
/// It carries exactly the accumulation state pi's `processResponsesStream` kept
/// (the output message, the per-`output_index` slots, and the terminal
/// bookkeeping) and drives it through the shared [`SseEventDecoder`] seam. The
/// buffered [`process_responses_stream`] (decoded-events boundary) and
/// [`parse_responses_sse_stream`] (whole-body boundary) both run this ONE
/// decoder, so their events and final message are byte-identical to the driver's
/// chunk-by-chunk [`AssistantEventReader`].
pub(crate) struct OpenAIResponsesSseDecoder {
    model: OpenAIResponsesModel,
    options: ResponsesStreamOptions,
    output: AssistantMessage,
    state: DispatchState,
    /// pi emits the `start` event before the dispatch loop; here it is emitted
    /// lazily on the first `on_frame`/`finish` so it is always the first event
    /// exactly once, whatever the chunk cadence.
    started: bool,
    /// Set when an event triggered pi's `processResponsesStream` throw (an
    /// `error` event or a `response.failed`); `finalize` turns it into the
    /// terminal `error` event.
    terminal_error: Option<String>,
}

impl OpenAIResponsesSseDecoder {
    /// A fresh decoder for `model`, seeding the empty output shell pi builds
    /// before streaming. `timestamp` is pi's `Date.now()` message timestamp.
    pub(crate) fn new(
        model: OpenAIResponsesModel,
        options: ResponsesStreamOptions,
        timestamp: i64,
    ) -> Self {
        let output = AssistantMessage {
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
        Self {
            model,
            options,
            output,
            state: DispatchState::default(),
            started: false,
            terminal_error: None,
        }
    }

    /// Emit pi's initial `start` event exactly once, before any dispatched event.
    fn ensure_started(&mut self, out: &mut Vec<AssistantMessageEvent>) {
        if !self.started {
            self.started = true;
            out.push(AssistantMessageEvent::Start {
                partial: self.output.clone(),
            });
        }
    }

    /// Dispatch one already-decoded event value — the shared entry point for the
    /// decoded-events boundary ([`process_responses_stream`]) and the SSE
    /// [`on_frame`](SseEventDecoder::on_frame). Returns [`ControlFlow::Break`] on
    /// a hard terminal error, whose message `finalize` turns into the terminal
    /// `error` event.
    fn push_value(
        &mut self,
        event: &Value,
        out: &mut Vec<AssistantMessageEvent>,
    ) -> ControlFlow<String> {
        if let Some(message) = dispatch_event(
            event,
            &self.model,
            &self.options,
            &mut self.output,
            &mut self.state,
            out,
        ) {
            self.terminal_error = Some(message.clone());
            return ControlFlow::Break(message);
        }
        ControlFlow::Continue(())
    }

    /// The terminal selection shared by every boundary: a frame-level throw wins,
    /// then the missing-terminal-event guard, then pi's `aborted`/`error`
    /// stop-reason re-throw, otherwise a `done` event. Returns the final message.
    fn finalize(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
        self.ensure_started(out);

        let terminal_error = if self.terminal_error.is_some() {
            self.terminal_error.clone()
        } else if !self.state.saw_terminal {
            Some("OpenAI Responses stream ended before a terminal response event".to_string())
        } else {
            None
        };

        match terminal_error {
            None => {
                if matches!(
                    self.output.stop_reason,
                    StopReason::Aborted | StopReason::Error
                ) {
                    let message = self
                        .output
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "An unknown error occurred".to_string());
                    finish_with_error(&mut self.output, out, message);
                } else {
                    out.push(AssistantMessageEvent::Done {
                        reason: self.output.stop_reason,
                        message: self.output.clone(),
                    });
                }
            }
            Some(message) => finish_with_error(&mut self.output, out, message),
        }

        self.output.clone()
    }
}

impl SseEventDecoder for OpenAIResponsesSseDecoder {
    fn on_frame(
        &mut self,
        frame: &ServerSentEvent,
        out: &mut Vec<AssistantMessageEvent>,
    ) -> ControlFlow<String> {
        self.ensure_started(out);

        // The Responses stream carries one JSON `ResponseStreamEvent` per frame
        // (the `event:` name mirrors the payload's `type`). A keep-alive/comment
        // frame or the spec-absent `[DONE]` sentinel carries no event body and is
        // skipped, matching the OpenAI SDK decoder's `JSON.parse` gate.
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return ControlFlow::Continue(());
        }
        let event: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => return ControlFlow::Continue(()),
        };
        self.push_value(&event, out)
    }

    fn finish(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
        self.finalize(out)
    }
}

fn response_failed_message(response: Option<&Value>) -> String {
    let error = response.and_then(|r| r.get("error"));
    if let Some(error) = error.filter(|e| !e.is_null()) {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("no message");
        return format!("{code}: {message}");
    }
    let reason = response
        .and_then(|r| r.get("incomplete_details"))
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str);
    match reason {
        Some(reason) => format!("incomplete: {reason}"),
        None => "Unknown error (no error details in response)".to_string(),
    }
}

/// Create an output slot for an item (pi's `createSlot`), pushing the block onto
/// `output.content` and emitting the matching `*_start` event.
fn create_slot(
    output_index: i64,
    item: &Value,
    output: &mut AssistantMessage,
    slots: &mut HashMap<i64, Slot>,
    events: &mut Vec<AssistantMessageEvent>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("reasoning") => {
            output.content.push(ContentBlock::Thinking {
                thinking: String::new(),
                thinking_signature: None,
                redacted: None,
            });
            let content_index = output.content.len() - 1;
            slots.insert(
                output_index,
                Slot {
                    kind: SlotKind::Thinking,
                    content_index,
                    partial_json: String::new(),
                },
            );
            events.push(AssistantMessageEvent::ThinkingStart {
                content_index: content_index as u32,
                partial: output.clone(),
            });
        }
        Some("message") => {
            output.content.push(ContentBlock::Text {
                text: String::new(),
                text_signature: None,
            });
            let content_index = output.content.len() - 1;
            slots.insert(
                output_index,
                Slot {
                    kind: SlotKind::Text,
                    content_index,
                    partial_json: String::new(),
                },
            );
            events.push(AssistantMessageEvent::TextStart {
                content_index: content_index as u32,
                partial: output.clone(),
            });
        }
        Some("function_call") => {
            let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let partial_json = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            output.content.push(ContentBlock::ToolCall {
                id: format!("{call_id}|{item_id}"),
                name,
                arguments: Value::Object(Map::new()),
                thought_signature: None,
            });
            let content_index = output.content.len() - 1;
            slots.insert(
                output_index,
                Slot {
                    kind: SlotKind::ToolCall,
                    content_index,
                    partial_json,
                },
            );
            events.push(AssistantMessageEvent::ToolcallStart {
                content_index: content_index as u32,
                partial: output.clone(),
            });
        }
        _ => {}
    }
}

fn append_thinking(
    output_index: i64,
    delta: &str,
    output: &mut AssistantMessage,
    slots: &HashMap<i64, Slot>,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let Some(slot) = slots.get(&output_index) else {
        return;
    };
    if slot.kind != SlotKind::Thinking {
        return;
    }
    if let Some(ContentBlock::Thinking { thinking, .. }) =
        output.content.get_mut(slot.content_index)
    {
        thinking.push_str(delta);
    }
    events.push(AssistantMessageEvent::ThinkingDelta {
        content_index: slot.content_index as u32,
        delta: delta.to_string(),
        partial: output.clone(),
    });
}

fn append_text(
    output_index: i64,
    delta: &str,
    output: &mut AssistantMessage,
    slots: &HashMap<i64, Slot>,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let Some(slot) = slots.get(&output_index) else {
        return;
    };
    if slot.kind != SlotKind::Text {
        return;
    }
    if let Some(ContentBlock::Text { text, .. }) = output.content.get_mut(slot.content_index) {
        text.push_str(delta);
    }
    events.push(AssistantMessageEvent::TextDelta {
        content_index: slot.content_index as u32,
        delta: delta.to_string(),
        partial: output.clone(),
    });
}

fn set_tool_arguments(output: &mut AssistantMessage, content_index: usize, parsed: Value) {
    if let Some(ContentBlock::ToolCall { arguments, .. }) = output.content.get_mut(content_index) {
        *arguments = parsed;
    }
}

/// Finalize an output item at `response.output_item.done` (pi's per-type
/// finalize arms): store the reasoning signature, encode the text signature, or
/// finalize tool-call arguments (stripping the scratch partial-JSON buffer).
fn finalize_output_item(
    output_index: i64,
    item: &Value,
    output: &mut AssistantMessage,
    slots: &mut HashMap<i64, Slot>,
    reasoning_index_by_id: &mut HashMap<String, usize>,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let Some(slot) = slots.get(&output_index).cloned() else {
        return;
    };
    let item_type = item.get("type").and_then(Value::as_str);

    match (item_type, slot.kind) {
        (Some("reasoning"), SlotKind::Thinking) => {
            let summary_text = join_text_parts(item.get("summary"));
            let content_text = join_text_parts(item.get("content"));
            let signature = serde_json::to_string(item).unwrap();
            let final_thinking = if !summary_text.is_empty() {
                summary_text
            } else if !content_text.is_empty() {
                content_text
            } else if let Some(ContentBlock::Thinking { thinking, .. }) =
                output.content.get(slot.content_index)
            {
                thinking.clone()
            } else {
                String::new()
            };
            if let Some(ContentBlock::Thinking {
                thinking,
                thinking_signature,
                ..
            }) = output.content.get_mut(slot.content_index)
            {
                *thinking = final_thinking.clone();
                *thinking_signature = Some(signature);
            }
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                reasoning_index_by_id.insert(id.to_string(), slot.content_index);
            }
            events.push(AssistantMessageEvent::ThinkingEnd {
                content_index: slot.content_index as u32,
                content: final_thinking,
                partial: output.clone(),
            });
            slots.remove(&output_index);
        }
        (Some("message"), SlotKind::Text) => {
            let final_text = item
                .get("content")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .map(|c| match c.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                c.get("text").and_then(Value::as_str).unwrap_or("")
                            }
                            _ => c.get("refusal").and_then(Value::as_str).unwrap_or(""),
                        })
                        .collect::<String>()
                })
                .unwrap_or_default();
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let phase = item.get("phase").and_then(Value::as_str);
            let signature = encode_text_signature_v1(id, phase);
            if let Some(ContentBlock::Text {
                text,
                text_signature,
            }) = output.content.get_mut(slot.content_index)
            {
                *text = final_text.clone();
                *text_signature = Some(signature);
            }
            events.push(AssistantMessageEvent::TextEnd {
                content_index: slot.content_index as u32,
                content: final_text,
                partial: output.clone(),
            });
            slots.remove(&output_index);
        }
        (Some("function_call"), SlotKind::ToolCall) => {
            let args_source = item
                .get("arguments")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if slot.partial_json.is_empty() {
                        "{}".to_string()
                    } else {
                        slot.partial_json.clone()
                    }
                });
            let parsed = parse_streaming_json(Some(&args_source));
            set_tool_arguments(output, slot.content_index, parsed);
            // The scratch partial-JSON buffer lives only in the slot; the
            // persisted ContentBlock::ToolCall never carries it.
            let tool_call = output.content[slot.content_index].clone();
            events.push(AssistantMessageEvent::ToolcallEnd {
                content_index: slot.content_index as u32,
                tool_call,
                partial: output.clone(),
            });
            slots.remove(&output_index);
        }
        _ => {}
    }
}

fn join_text_parts(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|p| p.get("text").and_then(Value::as_str).unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// Finalize usage, cost, and stop reason from a terminal `response` object
/// (pi's `finalizeResponse`).
fn finalize_response(
    response: &Value,
    model: &OpenAIResponsesModel,
    options: &ResponsesStreamOptions,
    output: &mut AssistantMessage,
    reasoning_index_by_id: &HashMap<String, usize>,
) {
    backfill_reasoning_signatures(response, output, reasoning_index_by_id);

    if let Some(id) = response.get("id").and_then(Value::as_str) {
        output.response_id = Some(id.to_string());
    }

    if let Some(usage) = response.get("usage").filter(|u| !u.is_null()) {
        let input_details = usage.get("input_tokens_details");
        let cached = input_details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_write = input_details
            .and_then(|d| d.get("cache_write_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input_tokens = u64_field(usage, "input_tokens");
        let reasoning = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        output.usage = Usage {
            input: input_tokens
                .saturating_sub(cached)
                .saturating_sub(cache_write),
            output: u64_field(usage, "output_tokens"),
            cache_read: cached,
            cache_write,
            cache_write_1h: None,
            reasoning: Some(reasoning),
            total_tokens: u64_field(usage, "total_tokens"),
            cost: UsageCost::default(),
        };
    }

    output.usage.cost = calculate_cost_with(&model.cost, &output.usage);

    // Resolve service tier (response value wins, else request option) and apply
    // the post-cost pricing multiplier.
    let resolved_service_tier = response
        .get("service_tier")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| options.service_tier.clone());
    apply_service_tier_pricing(
        &mut output.usage,
        resolved_service_tier.as_deref(),
        &model.id,
    );

    output.stop_reason = map_stop_reason(response.get("status").and_then(Value::as_str));
    if output
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
        && output.stop_reason == StopReason::Stop
    {
        output.stop_reason = StopReason::ToolUse;
    }
}

/// Backfill persisted reasoning signatures with `encrypted_content` from the
/// terminal response output (pi's `backfillReasoningSignatures`).
fn backfill_reasoning_signatures(
    response: &Value,
    output: &mut AssistantMessage,
    reasoning_index_by_id: &HashMap<String, usize>,
) {
    let Some(items) = response.get("output").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(encrypted) = item.get("encrypted_content").filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(&content_index) = reasoning_index_by_id.get(id) else {
            continue;
        };
        if let Some(ContentBlock::Thinking {
            thinking_signature, ..
        }) = output.content.get_mut(content_index)
        {
            let Some(sig) = thinking_signature.clone() else {
                continue;
            };
            let Ok(mut stored) = serde_json::from_str::<Value>(&sig) else {
                continue;
            };
            if stored
                .get("encrypted_content")
                .is_some_and(|v| !v.is_null())
            {
                continue;
            }
            if let Some(obj) = stored.as_object_mut() {
                obj.insert("encrypted_content".to_string(), encrypted.clone());
                *thinking_signature = Some(serde_json::to_string(&stored).unwrap());
            }
        }
    }
}

fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model_id: &str) {
    let multiplier = match service_tier {
        Some("flex") => 0.5,
        Some("priority") => {
            if model_id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    };
    if multiplier == 1.0 {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

/// Map a Responses `status` to a stop reason (pi's `mapStopReason`).
fn map_stop_reason(status: Option<&str>) -> StopReason {
    match status {
        None => StopReason::Stop,
        Some("completed") => StopReason::Stop,
        Some("incomplete") => StopReason::Length,
        Some("failed") | Some("cancelled") => StopReason::Error,
        Some("in_progress") | Some("queued") => StopReason::Stop,
        // pi throws for an unhandled status; we default to `stop` at the eager
        // boundary rather than panicking on an unexpected wire value.
        Some(_) => StopReason::Stop,
    }
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

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn short_hash_matches_pi_reference() {
        // Deterministic reference values computed from pi's `shortHash`.
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
    }

    #[test]
    fn encode_text_signature_shapes() {
        assert_eq!(encode_text_signature_v1("m1", None), r#"{"v":1,"id":"m1"}"#);
        assert_eq!(
            encode_text_signature_v1("m1", Some("final_answer")),
            r#"{"v":1,"id":"m1","phase":"final_answer"}"#
        );
    }
}
