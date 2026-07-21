// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `google-shared.ts` plus a Google-specific `transform-messages.ts` port the
// drivers depend on. The per-branch `convertMessages` arms (user / assistant /
// toolResult) and the stream-decode block-flush arms are walls of near-identical
// part-building JSON by design; the clone detector reads them as duplicates.
// They are distinct, load-bearing transcriptions kept verbatim to mirror the
// upstream wire behaviour exactly.
// straitjacket-allow-file:file-size — TODO(straitjacket): this file is 1632 lines, over
// the 1500-line ceiling. Declared explicitly so it suppresses only file-size, not every
// rule (the old bracket form was a silent catch-all). The overrun is the streamSimple
// reasoning-lowering helpers ported alongside the shared model/build_params slice; pi keeps
// the per-dialect getThinkingLevel/getGemini3ThinkingLevel/getGoogleBudget in separate
// files, so they are not collapsible. Remove once the file is split into a directory module
// (see PR follow-up).
//! Shared helpers for the Google Generative AI and Google Vertex drivers, ported
//! from pi-ai's `packages/ai/src/api/google-shared.ts` at pinned commit
//! `3da591ab`, together with a Google-specific port of
//! `api/transform-messages.ts` (message normalization + tool-call-id mapping +
//! synthetic tool-result insertion). Unpaired-surrogate stripping
//! (`utils/sanitize-unicode.ts`) is not re-ported here: it is shared from
//! [`crate::utils::sanitize_unicode`].
//!
//! Faithful to pi's behaviour:
//! - [`is_thinking_part`] / [`retain_thought_signature`] reproduce the streamed
//!   thought-signature retention contract.
//! - [`convert_messages`] reproduces the Gemini `Content[]` build: user / model /
//!   toolResult turns, thought-signature resolution (same-provider-and-model +
//!   valid base64), Gemini-3 multimodal `functionResponse.parts` nesting vs the
//!   separate image user turn for Gemini < 3, and functionResponse turn merging.
//! - [`convert_tools`] reproduces the `functionDeclarations` build and the
//!   `sanitizeForOpenApi` meta-key stripping (preserving `$ref`).
//! - [`parse_google_stream`] reproduces the `generateContentStream` decode loop:
//!   walking `candidates[].content.parts[]` into text / thinking / tool-call
//!   events, function-call id-synthesis, usage/cost math, and stop-reason mapping.
//!
//! Provenance note: `transform_messages` is a Google-specific port of
//! `transform-messages.ts` — it threads a `requiresToolCallId`-gated tool-call-id
//! normalizer and a caller-supplied `now` into synthetic tool results, unlike the
//! anthropic port in [`crate::api::anthropic::transform_messages`], which hard-wires
//! anthropic's unconditional normalizer and a fixed timestamp. Each provider keeps
//! its own `transform-messages.ts` port (mistral / bedrock / openai / anthropic),
//! so this one stays local by design. The JSON-Schema meta-key set is
//! `google-shared.ts`-local (not duplicated elsewhere in the crate).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, Message, Modality,
    ModelCost, ModelThinkingLevel, StopReason, ThinkingBudgets, Usage, UsageCost, UserContent,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;

// ---------------------------------------------------------------------------
// Model slice
// ---------------------------------------------------------------------------

/// The minimum slice of a pi `Model` the Google drivers need. Deserialized
/// leniently so any additional pi model fields are ignored, and every field the
/// drivers do not always require carries a default so partial model JSON (as the
/// unit tests build) still deserializes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleModel {
    pub id: String,
    #[serde(default)]
    pub api: String,
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<Modality>,
    pub cost: ModelCost,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
}

impl GoogleModel {
    fn supports_image(&self) -> bool {
        self.input.contains(&Modality::Image)
    }
}

// ---------------------------------------------------------------------------
// Thought-signature helpers (`google-shared.ts:33-72`)
// ---------------------------------------------------------------------------

/// `google-shared.ts:33` — `part.thought === true` is the definitive thinking
/// marker; a bare `thoughtSignature` does not make a part thinking content.
pub fn is_thinking_part(part: &Value) -> bool {
    part.get("thought").and_then(Value::as_bool) == Some(true)
}

/// `google-shared.ts:46` — preserve the last non-empty thought signature for the
/// current streamed block; an omitted or empty incoming signature keeps the
/// existing one.
pub fn retain_thought_signature(
    existing: Option<String>,
    incoming: Option<&str>,
) -> Option<String> {
    match incoming {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => existing,
    }
}

/// Thought signatures must be base64 for Google APIs (`google-shared.ts:52-58`):
/// length a multiple of 4 and matching `^[A-Za-z0-9+/]+={0,2}$`.
fn is_valid_thought_signature(signature: Option<&str>) -> bool {
    let Some(sig) = signature else {
        return false;
    };
    if sig.is_empty() {
        return false;
    }
    if sig.len() % 4 != 0 {
        return false;
    }
    // `^[A-Za-z0-9+/]+={0,2}$`: one-or-more base64 chars, then 0-2 trailing `=`.
    let bytes = sig.as_bytes();
    let mut i = 0;
    let mut body_len = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let is_body = c.is_ascii_alphanumeric() || c == b'+' || c == b'/';
        if !is_body {
            break;
        }
        body_len += 1;
        i += 1;
    }
    if body_len == 0 {
        return false;
    }
    let mut pad = 0;
    while i < bytes.len() {
        if bytes[i] != b'=' {
            return false;
        }
        pad += 1;
        i += 1;
    }
    pad <= 2
}

/// `google-shared.ts:63` — only keep signatures from the same provider/model and
/// with valid base64.
fn resolve_thought_signature(
    is_same_provider_and_model: bool,
    signature: Option<&str>,
) -> Option<String> {
    if is_same_provider_and_model && is_valid_thought_signature(signature) {
        signature.map(str::to_string)
    } else {
        None
    }
}

/// `google-shared.ts:70` — models via Google APIs that require explicit tool call
/// IDs in function calls/responses.
pub fn requires_tool_call_id(model_id: &str) -> bool {
    model_id.starts_with("claude-") || model_id.starts_with("gpt-oss-")
}

/// `google-shared.ts:74` — the leading Gemini major version, e.g. `gemini-3-pro`
/// → `3`; `None` for non-Gemini ids.
fn get_gemini_major_version(model_id: &str) -> Option<u32> {
    let lower = model_id.to_lowercase();
    let rest = lower.strip_prefix("gemini")?;
    let rest = rest.strip_prefix("-live").unwrap_or(rest);
    let rest = rest.strip_prefix('-')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// `google-shared.ts:80` — Gemini ≥ 3 supports multimodal function responses
/// (images nested in `functionResponse.parts`); non-Gemini ids default to `true`.
fn supports_multimodal_function_response(model_id: &str) -> bool {
    match get_gemini_major_version(model_id) {
        Some(v) => v >= 3,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// transform-messages.ts
// ---------------------------------------------------------------------------
//
// pi's `sanitizeSurrogates` (`utils/sanitize-unicode.ts`) is shared, not
// re-ported: the call sites below use [`sanitize_surrogates`] from
// [`crate::utils::sanitize_unicode`] (imported above), the same shared util the
// bedrock driver consumes.

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// `transform-messages.ts:15` — replace image blocks with a single placeholder
/// text block, collapsing runs of adjacent images.
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
                previous_was_placeholder =
                    matches!(other, ContentBlock::Text { text, .. } if text == placeholder);
                result.push(other.clone());
            }
        }
    }

    result
}

/// `transform-messages.ts:35` — downgrade images to placeholders for non-vision
/// models; pass through unchanged when the model accepts images.
fn downgrade_unsupported_images(messages: &[Message], supports_image: bool) -> Vec<Message> {
    if supports_image {
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
            Message::ToolResult(tr) => {
                let mut next = tr.clone();
                next.content =
                    replace_images_with_placeholder(&tr.content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                Message::ToolResult(next)
            }
            other => other.clone(),
        })
        .collect()
}

/// `transform-messages.ts:64` — normalize a message list for a target model:
/// downgrade unsupported images, drop/convert cross-model thinking and text,
/// normalize tool-call ids, and insert synthetic tool results for orphaned tool
/// calls.
///
/// `normalize_tool_call_id` mirrors pi's optional callback; the Google drivers
/// pass a normalizer only meaningful for `requiresToolCallId` models. `now_ms`
/// stands in for pi's `Date.now()` on synthetic tool results (deterministic in
/// tests).
fn transform_messages(
    messages: &[Message],
    model: &GoogleModel,
    normalize_tool_call_id: impl Fn(&str) -> String,
    now_ms: i64,
) -> Vec<Message> {
    let image_aware = downgrade_unsupported_images(messages, model.supports_image());

    // First pass: transform assistant content, normalize toolResult ids.
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    let mut transformed: Vec<Message> = Vec::with_capacity(image_aware.len());

    for msg in &image_aware {
        match msg {
            Message::User(_) => transformed.push(msg.clone()),
            Message::ToolResult(tr) => {
                if let Some(normalized) = tool_call_id_map.get(&tr.tool_call_id) {
                    if normalized != &tr.tool_call_id {
                        let mut next = tr.clone();
                        next.tool_call_id = normalized.clone();
                        transformed.push(Message::ToolResult(next));
                        continue;
                    }
                }
                transformed.push(msg.clone());
            }
            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;

                let mut content: Vec<ContentBlock> = Vec::new();
                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking {
                            thinking,
                            thinking_signature,
                            redacted,
                        } => {
                            if redacted == &Some(true) {
                                if is_same_model {
                                    content.push(block.clone());
                                }
                                continue;
                            }
                            if is_same_model && thinking_signature.is_some() {
                                content.push(block.clone());
                                continue;
                            }
                            if thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_model {
                                content.push(block.clone());
                            } else {
                                content.push(ContentBlock::Text {
                                    text: thinking.clone(),
                                    text_signature: None,
                                });
                            }
                        }
                        ContentBlock::Text { text, .. } => {
                            if is_same_model {
                                content.push(block.clone());
                            } else {
                                content.push(ContentBlock::Text {
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
                            let mut new_sig = thought_signature.clone();
                            if !is_same_model && thought_signature.is_some() {
                                new_sig = None;
                            }
                            if !is_same_model {
                                let normalized = normalize_tool_call_id(id);
                                if normalized != *id {
                                    tool_call_id_map.insert(id.clone(), normalized.clone());
                                    new_id = normalized;
                                }
                            }
                            content.push(ContentBlock::ToolCall {
                                id: new_id,
                                name: name.clone(),
                                arguments: arguments.clone(),
                                thought_signature: new_sig,
                            });
                        }
                        other => content.push(other.clone()),
                    }
                }

                let mut next = assistant.clone();
                next.content = content;
                transformed.push(Message::Assistant(next));
            }
        }
    }

    // Second pass: insert synthetic empty tool results for orphaned tool calls.
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<(String, String)> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();

    fn flush_synthetic(
        result: &mut Vec<Message>,
        pending: &mut Vec<(String, String)>,
        existing: &mut HashSet<String>,
        now_ms: i64,
    ) {
        if pending.is_empty() {
            return;
        }
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
                    timestamp: now_ms,
                }));
            }
        }
        pending.clear();
        *existing = HashSet::new();
    }

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant) => {
                flush_synthetic(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    now_ms,
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
            Message::ToolResult(tr) => {
                existing_tool_result_ids.insert(tr.tool_call_id.clone());
                result.push(msg);
            }
            Message::User(_) => {
                flush_synthetic(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    now_ms,
                );
                result.push(msg);
            }
        }
    }

    flush_synthetic(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
        now_ms,
    );

    result
}

// ---------------------------------------------------------------------------
// convertMessages (`google-shared.ts:91`)
// ---------------------------------------------------------------------------

/// `google-shared.ts:91` — convert internal messages to Gemini `Content[]`.
///
/// Each returned `Value` is a Gemini `Content` object (`{ role, parts }`); parts
/// are the heterogeneous Gemini `Part` shapes (`text`, `inlineData`,
/// `functionCall`, `functionResponse`, thinking) kept as JSON, matching how the
/// `@google/genai` SDK is fed.
pub fn convert_messages(
    model: &GoogleModel,
    context: &crate::types::Context,
    now_ms: i64,
) -> Vec<Value> {
    let mut contents: Vec<Value> = Vec::new();

    let normalize = |id: &str| -> String {
        if !requires_tool_call_id(&model.id) {
            return id.to_string();
        }
        let filtered: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        filtered.chars().take(64).collect()
    };

    let transformed = transform_messages(&context.messages, model, normalize, now_ms);

    for msg in &transformed {
        match msg {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{ "text": sanitize_surrogates(text) }],
                    }));
                }
                UserContent::Blocks(blocks) => {
                    let mut parts: Vec<Value> = Vec::new();
                    for item in blocks {
                        match item {
                            ContentBlock::Text { text, .. } => {
                                parts.push(json!({ "text": sanitize_surrogates(text) }));
                            }
                            ContentBlock::Image { data, mime_type } => {
                                parts.push(json!({
                                    "inlineData": { "mimeType": mime_type, "data": data },
                                }));
                            }
                            _ => {}
                        }
                    }
                    if parts.is_empty() {
                        continue;
                    }
                    contents.push(json!({ "role": "user", "parts": parts }));
                }
            },
            Message::Assistant(assistant) => {
                let is_same_provider_and_model =
                    assistant.provider == model.provider && assistant.model == model.id;
                let mut parts: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Text {
                            text,
                            text_signature,
                        } => {
                            if text.trim().is_empty() {
                                continue;
                            }
                            let sig = resolve_thought_signature(
                                is_same_provider_and_model,
                                text_signature.as_deref(),
                            );
                            let mut part = Map::new();
                            part.insert("text".to_string(), json!(sanitize_surrogates(text)));
                            if let Some(sig) = sig {
                                part.insert("thoughtSignature".to_string(), json!(sig));
                            }
                            parts.push(Value::Object(part));
                        }
                        ContentBlock::Thinking {
                            thinking,
                            thinking_signature,
                            ..
                        } => {
                            if thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_provider_and_model {
                                let sig = resolve_thought_signature(
                                    is_same_provider_and_model,
                                    thinking_signature.as_deref(),
                                );
                                let mut part = Map::new();
                                part.insert("thought".to_string(), json!(true));
                                part.insert(
                                    "text".to_string(),
                                    json!(sanitize_surrogates(thinking)),
                                );
                                if let Some(sig) = sig {
                                    part.insert("thoughtSignature".to_string(), json!(sig));
                                }
                                parts.push(Value::Object(part));
                            } else {
                                parts.push(json!({ "text": sanitize_surrogates(thinking) }));
                            }
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            thought_signature,
                        } => {
                            let sig = resolve_thought_signature(
                                is_same_provider_and_model,
                                thought_signature.as_deref(),
                            );
                            let mut function_call = Map::new();
                            function_call.insert("name".to_string(), json!(name));
                            function_call.insert(
                                "args".to_string(),
                                if arguments.is_null() {
                                    json!({})
                                } else {
                                    arguments.clone()
                                },
                            );
                            if requires_tool_call_id(&model.id) {
                                function_call.insert("id".to_string(), json!(id));
                            }
                            let mut part = Map::new();
                            part.insert("functionCall".to_string(), Value::Object(function_call));
                            if let Some(sig) = sig {
                                part.insert("thoughtSignature".to_string(), json!(sig));
                            }
                            parts.push(Value::Object(part));
                        }
                        _ => {}
                    }
                }

                if parts.is_empty() {
                    continue;
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Message::ToolResult(tr) => {
                let text_result: String = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let image_content: Vec<(&String, &String)> = if model.supports_image() {
                    tr.content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Image { data, mime_type } => Some((data, mime_type)),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let has_text = !text_result.is_empty();
                let has_images = !image_content.is_empty();
                let multimodal = supports_multimodal_function_response(&model.id);

                let response_value = if has_text {
                    sanitize_surrogates(&text_result)
                } else if has_images {
                    "(see attached image)".to_string()
                } else {
                    String::new()
                };

                let image_parts: Vec<Value> = image_content
                    .iter()
                    .map(|(data, mime_type)| {
                        json!({ "inlineData": { "mimeType": mime_type, "data": data } })
                    })
                    .collect();

                let include_id = requires_tool_call_id(&model.id);
                let mut function_response = Map::new();
                function_response.insert("name".to_string(), json!(tr.tool_name));
                function_response.insert(
                    "response".to_string(),
                    if tr.is_error {
                        json!({ "error": response_value })
                    } else {
                        json!({ "output": response_value })
                    },
                );
                if has_images && multimodal {
                    function_response.insert("parts".to_string(), json!(image_parts.clone()));
                }
                if include_id {
                    function_response.insert("id".to_string(), json!(tr.tool_call_id));
                }
                let function_response_part =
                    json!({ "functionResponse": Value::Object(function_response) });

                // Merge into a trailing user turn already carrying functionResponses.
                let should_merge = contents
                    .last()
                    .map(|last| {
                        last.get("role").and_then(Value::as_str) == Some("user")
                            && last
                                .get("parts")
                                .and_then(Value::as_array)
                                .map(|parts| {
                                    parts.iter().any(|p| p.get("functionResponse").is_some())
                                })
                                .unwrap_or(false)
                    })
                    .unwrap_or(false);

                if should_merge {
                    if let Some(last) = contents.last_mut() {
                        if let Some(parts) = last.get_mut("parts").and_then(Value::as_array_mut) {
                            parts.push(function_response_part);
                        }
                    }
                } else {
                    contents.push(json!({ "role": "user", "parts": [function_response_part] }));
                }

                // Gemini < 3: images go in a separate user turn.
                if has_images && !multimodal {
                    let mut parts: Vec<Value> = vec![json!({ "text": "Tool result image:" })];
                    parts.extend(image_parts);
                    contents.push(json!({ "role": "user", "parts": parts }));
                }
            }
        }
    }

    contents
}

// ---------------------------------------------------------------------------
// convertTools (`google-shared.ts:237-288`)
// ---------------------------------------------------------------------------

const JSON_SCHEMA_META_DECLARATIONS: [&str; 8] = [
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    "definitions",
];

/// `google-shared.ts:251` — recursively strip JSON-Schema meta declarations from
/// an object. Non-objects and arrays are returned unchanged (arrays are not
/// recursed into, matching pi), so `$ref` (not a meta key) survives.
fn sanitize_for_open_api(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, value) in map {
                if JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()) {
                    continue;
                }
                result.insert(key.clone(), sanitize_for_open_api(value));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

/// `google-shared.ts:272` — convert tools to Gemini `functionDeclarations`.
///
/// `None` for an empty tool list. With `use_parameters = true` the sanitized
/// OpenAPI `parameters` field is emitted; otherwise the raw `parametersJsonSchema`
/// (full JSON Schema) is passed through verbatim.
pub fn convert_tools(tools: &[Value], use_parameters: bool) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut decl = Map::new();
            if let Some(name) = tool.get("name") {
                decl.insert("name".to_string(), name.clone());
            }
            if let Some(description) = tool.get("description") {
                decl.insert("description".to_string(), description.clone());
            }
            let parameters = tool.get("parameters").cloned().unwrap_or(Value::Null);
            if use_parameters {
                decl.insert("parameters".to_string(), sanitize_for_open_api(&parameters));
            } else {
                decl.insert("parametersJsonSchema".to_string(), parameters);
            }
            Value::Object(decl)
        })
        .collect();
    Some(vec![json!({ "functionDeclarations": declarations })])
}

/// `google-shared.ts:293` — map a tool-choice string to a Gemini
/// `FunctionCallingConfigMode` name.
pub fn map_tool_choice(choice: &str) -> &'static str {
    match choice {
        "auto" => "AUTO",
        "none" => "NONE",
        "any" => "ANY",
        _ => "AUTO",
    }
}

/// `google-shared.ts:309` — map a Gemini `FinishReason` (as its wire string) to a
/// [`StopReason`]. `STOP` → `stop`, `MAX_TOKENS` → `length`; every other finish
/// reason (the safety / blocklist / recitation / malformed-call set) → `error`.
pub fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

/// `google-shared.ts:341` — map a raw string finish reason to a [`StopReason`]
/// (identical mapping to [`map_stop_reason`] at the wire level).
pub fn map_stop_reason_string(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

// ---------------------------------------------------------------------------
// build_params (`google-generative-ai.ts:343` / `google-vertex.ts:442`)
// ---------------------------------------------------------------------------

/// Thinking request controls (pi's `GoogleOptions.thinking`).
#[derive(Debug, Clone, Default)]
pub struct GoogleThinkingOption {
    pub enabled: bool,
    /// `-1` for dynamic, `0` to disable.
    pub budget_tokens: Option<i64>,
    pub level: Option<String>,
}

/// The request-shaping subset of pi's `GoogleOptions` / `GoogleVertexOptions`
/// shared by both drivers' `buildParams`.
#[derive(Debug, Clone, Default)]
pub struct GoogleRequestOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub tool_choice: Option<String>,
    pub thinking: Option<GoogleThinkingOption>,
    /// Whether the caller's abort signal is already aborted.
    pub aborted: bool,
}

fn is_gemma4(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    id.contains("gemma-4") || id.contains("gemma4")
}

fn is_gemini3_pro(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    // `/gemini-3(?:\.\d+)?-pro/`
    contains_gemini3_variant(&id, "-pro")
}

fn is_gemini3_flash(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    contains_gemini3_variant(&id, "-flash")
        || id == "gemini-flash-latest"
        || id == "gemini-flash-lite-latest"
}

/// Hand-rolled matcher for `/gemini-3(?:\.\d+)?<suffix>/` (substring, not anchored).
fn contains_gemini3_variant(id: &str, suffix: &str) -> bool {
    let bytes = id.as_bytes();
    let mut i = 0;
    while let Some(pos) = id[i..].find("gemini-3") {
        let start = i + pos + "gemini-3".len();
        let mut j = start;
        // optional `\.\d+`
        if bytes.get(j) == Some(&b'.') {
            let mut k = j + 1;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k > j + 1 {
                j = k;
            }
        }
        if id[j..].starts_with(suffix) {
            return true;
        }
        i = start;
    }
    false
}

// The Gemini-3-flash and Gemma-4 arms both resolve to `MINIMAL`; pi keeps them
// as distinct branches (`getDisabledThinkingConfig`) and this port mirrors that.
#[allow(clippy::if_same_then_else)]
fn disabled_thinking_config(model_id: &str) -> Value {
    if is_gemini3_pro(model_id) {
        json!({ "thinkingLevel": "LOW" })
    } else if is_gemini3_flash(model_id) {
        json!({ "thinkingLevel": "MINIMAL" })
    } else if is_gemma4(model_id) {
        json!({ "thinkingLevel": "MINIMAL" })
    } else {
        json!({ "thinkingBudget": 0 })
    }
}

// ---------------------------------------------------------------------------
// streamSimple reasoning lowering
// (`google-generative-ai.ts:284-509` / `google-vertex.ts:301-584`)
// ---------------------------------------------------------------------------

/// pi's `ClampedThinkingLevel` (`google-*.ts` — `Exclude<ThinkingLevel, "xhigh" |
/// "max">`): the effort google's thinking lowering operates on after the requested
/// level is model-clamped and `off` is mapped to `high`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleEffort {
    Minimal,
    Low,
    Medium,
    High,
}

impl GoogleEffort {
    /// pi `google-generative-ai.ts:299-300` / `google-vertex.ts:314-315`:
    /// `const effort = clampedReasoning === "off" ? "high" : clampedReasoning`.
    ///
    /// The requested level is first model-clamped by the caller (pi's
    /// `clampThinkingLevel`); this maps the clamped result to the effort. Unlike
    /// the openai dialects (which OMIT reasoning for `off`), google maps `off ⇒
    /// high` — thinking stays ON. pi's `ClampedThinkingLevel` type excludes
    /// `xhigh`/`max`; google models never expose those in their thinking maps, so
    /// clamp never yields them here and this collapses them to `high` defensively
    /// (unreachable in practice).
    pub fn from_clamped(level: ModelThinkingLevel) -> Self {
        match level {
            ModelThinkingLevel::Minimal => Self::Minimal,
            ModelThinkingLevel::Low => Self::Low,
            ModelThinkingLevel::Medium => Self::Medium,
            ModelThinkingLevel::High
            | ModelThinkingLevel::Off
            | ModelThinkingLevel::Xhigh
            | ModelThinkingLevel::Max => Self::High,
        }
    }
}

/// pi `customBudgets?.[effort]` (`getGoogleBudget`) — index the per-level custom
/// token budget by effort. pi's `ThinkingBudgets` is a partial record keyed by the
/// clamped levels; the Rust struct carries exactly `minimal`/`low`/`medium`/`high`.
fn custom_budget(custom: Option<&ThinkingBudgets>, effort: GoogleEffort) -> Option<i64> {
    let budgets = custom?;
    let value = match effort {
        GoogleEffort::Minimal => budgets.minimal,
        GoogleEffort::Low => budgets.low,
        GoogleEffort::Medium => budgets.medium,
        GoogleEffort::High => budgets.high,
    };
    value.map(|n| n as i64)
}

/// pi `google-generative-ai.ts:436-467` `getThinkingLevel` — gen-ai's level path.
/// Gemini-3-Pro collapses `minimal`/`low ⇒ LOW` and `medium`/`high ⇒ HIGH`;
/// Gemma-4 collapses `minimal`/`low ⇒ MINIMAL` and `medium`/`high ⇒ HIGH`; every
/// other (gemini-3-flash) model maps the effort straight through.
fn gen_ai_thinking_level(effort: GoogleEffort, model_id: &str) -> String {
    if is_gemini3_pro(model_id) {
        return match effort {
            GoogleEffort::Minimal | GoogleEffort::Low => "LOW",
            GoogleEffort::Medium | GoogleEffort::High => "HIGH",
        }
        .to_string();
    }
    if is_gemma4(model_id) {
        return match effort {
            GoogleEffort::Minimal | GoogleEffort::Low => "MINIMAL",
            GoogleEffort::Medium | GoogleEffort::High => "HIGH",
        }
        .to_string();
    }
    straight_through_level(effort)
}

/// pi `google-vertex.ts:528-552` `getGemini3ThinkingLevel` — vertex's level path.
/// Same Gemini-3-Pro collapse as gen-ai, but with NO Gemma-4 branch (vertex never
/// gates on gemma); every other model maps the effort straight through.
fn vertex_thinking_level(effort: GoogleEffort, model_id: &str) -> String {
    if is_gemini3_pro(model_id) {
        return match effort {
            GoogleEffort::Minimal | GoogleEffort::Low => "LOW",
            GoogleEffort::Medium | GoogleEffort::High => "HIGH",
        }
        .to_string();
    }
    straight_through_level(effort)
}

/// The default (non-pro, non-gemma) effort → `GoogleThinkingLevel` string map,
/// shared verbatim by both dialects' level paths (`google-generative-ai.ts:457-466`
/// / `google-vertex.ts:542-551`).
fn straight_through_level(effort: GoogleEffort) -> String {
    match effort {
        GoogleEffort::Minimal => "MINIMAL",
        GoogleEffort::Low => "LOW",
        GoogleEffort::Medium => "MEDIUM",
        GoogleEffort::High => "HIGH",
    }
    .to_string()
}

/// pi `google-generative-ai.ts:469-509` `getGoogleBudget` — gen-ai's budget path.
/// A caller-supplied custom budget wins; else per-model-id tables for `2.5-pro`,
/// `2.5-flash-lite`, and `2.5-flash` (the flash-lite check precedes flash since
/// `"2.5-flash-lite".includes("2.5-flash")`); any other model returns `-1`
/// (dynamic). pi indexes `model.id` case-sensitively.
fn gen_ai_budget(model_id: &str, effort: GoogleEffort, custom: Option<&ThinkingBudgets>) -> i64 {
    if let Some(budget) = custom_budget(custom, effort) {
        return budget;
    }
    if model_id.contains("2.5-pro") {
        return match effort {
            GoogleEffort::Minimal => 128,
            GoogleEffort::Low => 2048,
            GoogleEffort::Medium => 8192,
            GoogleEffort::High => 32768,
        };
    }
    if model_id.contains("2.5-flash-lite") {
        return match effort {
            GoogleEffort::Minimal => 512,
            GoogleEffort::Low => 2048,
            GoogleEffort::Medium => 8192,
            GoogleEffort::High => 24576,
        };
    }
    if model_id.contains("2.5-flash") {
        return match effort {
            GoogleEffort::Minimal => 128,
            GoogleEffort::Low => 2048,
            GoogleEffort::Medium => 8192,
            GoogleEffort::High => 24576,
        };
    }
    -1
}

/// pi `google-vertex.ts:554-584` `getGoogleBudget` — vertex's budget path. Same
/// shape as gen-ai but with NO `2.5-flash-lite` table (a flash-lite id falls
/// through to the `2.5-flash` table); any other model returns `-1` (dynamic).
fn vertex_budget(model_id: &str, effort: GoogleEffort, custom: Option<&ThinkingBudgets>) -> i64 {
    if let Some(budget) = custom_budget(custom, effort) {
        return budget;
    }
    if model_id.contains("2.5-pro") {
        return match effort {
            GoogleEffort::Minimal => 128,
            GoogleEffort::Low => 2048,
            GoogleEffort::Medium => 8192,
            GoogleEffort::High => 32768,
        };
    }
    if model_id.contains("2.5-flash") {
        return match effort {
            GoogleEffort::Minimal => 128,
            GoogleEffort::Low => 2048,
            GoogleEffort::Medium => 8192,
            GoogleEffort::High => 24576,
        };
    }
    -1
}

/// pi `google-generative-ai.ts:303-319` — build the enabled `thinking` option for
/// the gen-ai `streamSimple`. Gemini-3-Pro / Gemini-3-Flash / Gemma-4 models take
/// the `level` path; every other model takes the `budgetTokens` path.
pub fn gen_ai_thinking_option(
    model_id: &str,
    effort: GoogleEffort,
    custom_budgets: Option<&ThinkingBudgets>,
) -> GoogleThinkingOption {
    if is_gemini3_pro(model_id) || is_gemini3_flash(model_id) || is_gemma4(model_id) {
        GoogleThinkingOption {
            enabled: true,
            budget_tokens: None,
            level: Some(gen_ai_thinking_level(effort, model_id)),
        }
    } else {
        GoogleThinkingOption {
            enabled: true,
            budget_tokens: Some(gen_ai_budget(model_id, effort, custom_budgets)),
            level: None,
        }
    }
}

/// pi `google-vertex.ts:318-334` — build the enabled `thinking` option for the
/// vertex `streamSimple`. Gemini-3-Pro / Gemini-3-Flash models take the `level`
/// path (NO Gemma-4 gate, unlike gen-ai); every other model takes the
/// `budgetTokens` path (vertex's tables, which have no flash-lite branch).
pub fn vertex_thinking_option(
    model_id: &str,
    effort: GoogleEffort,
    custom_budgets: Option<&ThinkingBudgets>,
) -> GoogleThinkingOption {
    if is_gemini3_pro(model_id) || is_gemini3_flash(model_id) {
        GoogleThinkingOption {
            enabled: true,
            budget_tokens: None,
            level: Some(vertex_thinking_level(effort, model_id)),
        }
    } else {
        GoogleThinkingOption {
            enabled: true,
            budget_tokens: Some(vertex_budget(model_id, effort, custom_budgets)),
            level: None,
        }
    }
}

/// `buildParams` — build the Gemini `generateContent` request body. Shared by
/// both Google drivers (they differ only in client/auth construction, not the
/// request shape). Returns `Err("Request aborted")` when a pre-aborted signal is
/// passed, mirroring pi.
pub fn build_params(
    model: &GoogleModel,
    context: &crate::types::Context,
    options: &GoogleRequestOptions,
    now_ms: i64,
) -> Result<Value, String> {
    let contents = convert_messages(model, context, now_ms);

    let mut config = Map::new();
    if let Some(temperature) = options.temperature {
        config.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        config.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if let Some(system_prompt) = &context.system_prompt {
        config.insert(
            "systemInstruction".to_string(),
            json!(sanitize_surrogates(system_prompt)),
        );
    }
    let has_tools = context
        .tools
        .as_ref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if has_tools {
        if let Some(tools) = &context.tools {
            if let Some(converted) = convert_tools(tools, false) {
                config.insert("tools".to_string(), json!(converted));
            }
        }
    }

    if has_tools {
        if let Some(choice) = &options.tool_choice {
            config.insert(
                "toolConfig".to_string(),
                json!({
                    "functionCallingConfig": { "mode": map_tool_choice(choice) },
                }),
            );
        }
    }

    if let Some(thinking) = &options.thinking {
        if thinking.enabled && model.reasoning {
            let mut thinking_config = Map::new();
            thinking_config.insert("includeThoughts".to_string(), json!(true));
            if let Some(level) = &thinking.level {
                // gen-ai passes the `GoogleThinkingLevel` string through as-is
                // (`google-generative-ai.ts:378`); vertex maps it via
                // `THINKING_LEVEL_MAP` to the SDK `ThinkingLevel` enum
                // (`google-vertex.ts:476`), whose members serialize to the SAME
                // strings (`MINIMAL`/`LOW`/`MEDIUM`/`HIGH`), so the wire value is
                // identical for both dialects.
                thinking_config.insert("thinkingLevel".to_string(), json!(level));
            } else if let Some(budget) = thinking.budget_tokens {
                thinking_config.insert("thinkingBudget".to_string(), json!(budget));
            }
            config.insert("thinkingConfig".to_string(), Value::Object(thinking_config));
        } else if model.reasoning && !thinking.enabled {
            config.insert(
                "thinkingConfig".to_string(),
                disabled_thinking_config(&model.id),
            );
        }
    }

    if options.aborted {
        return Err("Request aborted".to_string());
    }

    Ok(json!({
        "model": model.id,
        "contents": contents,
        "config": Value::Object(config),
    }))
}

// ---------------------------------------------------------------------------
// Stream decode (`google-generative-ai.ts:57` / `google-vertex.ts:75`)
// ---------------------------------------------------------------------------

/// The result of decoding a Google `generateContentStream`: the uniform event
/// sequence and the accumulated final message.
#[derive(Debug, Clone, Serialize)]
pub struct StreamOutcome {
    pub events: Vec<AssistantMessageEvent>,
    pub message: AssistantMessage,
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

/// Which non-tool block is currently open during decode.
enum CurrentBlock {
    Text,
    Thinking,
}

/// The incremental Google `generateContentStream` decoder core: the single
/// source of truth for turning parsed `GenerateContentResponse` chunks into
/// assistant events and the accumulated message.
///
/// It carries exactly the accumulation state the shared inner loop of pi's
/// `stream()` kept — the output message, the currently-open text/thinking block,
/// and the synthetic-id counter — and exposes it as a chunk-at-a-time seam so
/// both the buffered [`parse_google_stream`] and the direct-Gemini incremental
/// SSE decoder run this ONE core, producing byte-identical events + message.
pub struct GoogleStreamDecoder {
    model: GoogleModel,
    now_ms: i64,
    output: AssistantMessage,
    current: Option<CurrentBlock>,
    /// Scoped per decoder for deterministic ids (pi keeps a module-level counter,
    /// but the observable synthesis is `${name}_${now}_${n}` and tests supply ids
    /// or a fixed clock; a per-decoder counter keeps output reproducible).
    tool_call_counter: u64,
    /// pi pushes the `start` event before the chunk loop; here it is emitted
    /// lazily on the first `process_chunk`/`finish` so it is always the first
    /// event exactly once, whatever the chunk cadence.
    started: bool,
}

impl GoogleStreamDecoder {
    /// A fresh decoder for `model` under `api` (`"google-generative-ai"` or
    /// `"google-vertex"`), seeding the empty output shell pi builds before
    /// streaming.
    pub fn new(model: GoogleModel, api: &str, now_ms: i64) -> Self {
        let output = AssistantMessage {
            role: AssistantRole::Assistant,
            content: Vec::new(),
            api: api.to_string(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: zero_usage(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: now_ms,
        };
        Self {
            model,
            now_ms,
            output,
            current: None,
            tool_call_counter: 0,
            started: false,
        }
    }

    /// Emit pi's initial `start` event exactly once, before any chunk's events.
    pub fn ensure_started(&mut self, events: &mut Vec<AssistantMessageEvent>) {
        if !self.started {
            self.started = true;
            events.push(AssistantMessageEvent::Start {
                partial: self.output.clone(),
            });
        }
    }

    /// Decode ONE parsed `GenerateContentResponse` chunk, updating the
    /// accumulation and pushing its events. Mirrors the body of pi's
    /// `for await (chunk of googleStream)` loop.
    pub fn process_chunk(&mut self, chunk: &Value, events: &mut Vec<AssistantMessageEvent>) {
        self.ensure_started(events);

        if self.output.response_id.is_none() {
            if let Some(id) = chunk.get("responseId").and_then(Value::as_str) {
                if !id.is_empty() {
                    self.output.response_id = Some(id.to_string());
                }
            }
        }

        let candidate = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first());

        if let Some(candidate) = candidate {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
            {
                for part in parts {
                    decode_part(
                        part,
                        &mut self.output,
                        events,
                        &mut self.current,
                        &mut self.tool_call_counter,
                        self.now_ms,
                    );
                }
            }

            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                self.output.stop_reason = map_stop_reason(reason);
                if self
                    .output
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
                {
                    self.output.stop_reason = StopReason::ToolUse;
                }
            }
        }

        if let Some(usage_meta) = chunk.get("usageMetadata") {
            apply_usage(&mut self.output, usage_meta, &self.model);
        }
    }

    /// The chunk stream ended: flush a trailing open block, push the terminal
    /// `done`/`error`, and return the final accumulated message.
    pub fn finish(&mut self, events: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
        self.ensure_started(events);

        // Flush a trailing open block.
        flush_current(&mut self.output, events, &mut self.current);

        if matches!(
            self.output.stop_reason,
            StopReason::Aborted | StopReason::Error
        ) {
            finish_with_error(
                &mut self.output,
                events,
                "An unknown error occurred".to_string(),
            );
        } else {
            events.push(AssistantMessageEvent::Done {
                reason: self.output.stop_reason,
                message: self.output.clone(),
            });
        }

        self.output.clone()
    }
}

/// Decode an already-parsed sequence of Google `GenerateContentResponse` chunks
/// into the uniform event stream and final message for `model`, under the given
/// `api` (`"google-generative-ai"` or `"google-vertex"`).
///
/// This reproduces the shared inner loop of pi's `stream()` for both Google
/// drivers (`google-generative-ai.ts:93-266` == `google-vertex.ts:111-283`,
/// byte-identical): walk `candidates[0].content.parts[]`, emitting text /
/// thinking / tool-call events, synthesizing function-call ids from `now_ms` + a
/// counter when missing or duplicated, mapping finish reasons, and computing
/// usage/cost. The HTTP transport, client construction, and abort-signal wiring
/// live in the per-driver modules; here the chunks are already obtained (exactly
/// what pi's `for await (chunk of googleStream)` yields). Runs the shared
/// [`GoogleStreamDecoder`] so its output is byte-identical to the direct-Gemini
/// incremental SSE path.
pub fn parse_google_stream(
    chunks: &[Value],
    model: &GoogleModel,
    api: &str,
    now_ms: i64,
) -> StreamOutcome {
    let mut decoder = GoogleStreamDecoder::new(model.clone(), api, now_ms);
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    for chunk in chunks {
        decoder.process_chunk(chunk, &mut events);
    }
    let message = decoder.finish(&mut events);
    StreamOutcome { events, message }
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn apply_usage(output: &mut AssistantMessage, usage_meta: &Value, model: &GoogleModel) {
    let prompt = u64_field(usage_meta, "promptTokenCount");
    let cached = u64_field(usage_meta, "cachedContentTokenCount");
    let candidates = u64_field(usage_meta, "candidatesTokenCount");
    let thoughts = u64_field(usage_meta, "thoughtsTokenCount");
    let total = u64_field(usage_meta, "totalTokenCount");

    output.usage = Usage {
        input: prompt.saturating_sub(cached),
        output: candidates + thoughts,
        cache_read: cached,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: Some(thoughts),
        total_tokens: total,
        cost: UsageCost::default(),
    };
    output.usage.cost = calculate_cost_with(&model.cost, &output.usage);
}

fn decode_part(
    part: &Value,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
    current: &mut Option<CurrentBlock>,
    tool_call_counter: &mut u64,
    now_ms: i64,
) {
    // Text / thinking part.
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        let is_thinking = is_thinking_part(part);
        let incoming_sig = part.get("thoughtSignature").and_then(Value::as_str);
        let need_switch = match current {
            None => true,
            Some(CurrentBlock::Thinking) => !is_thinking,
            Some(CurrentBlock::Text) => is_thinking,
        };
        if need_switch {
            flush_current(output, events, current);
            if is_thinking {
                output.content.push(ContentBlock::Thinking {
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                });
                *current = Some(CurrentBlock::Thinking);
                events.push(AssistantMessageEvent::ThinkingStart {
                    content_index: block_index(output),
                    partial: output.clone(),
                });
            } else {
                output.content.push(ContentBlock::Text {
                    text: String::new(),
                    text_signature: None,
                });
                *current = Some(CurrentBlock::Text);
                events.push(AssistantMessageEvent::TextStart {
                    content_index: block_index(output),
                    partial: output.clone(),
                });
            }
        }

        let idx = output.content.len() - 1;
        match &mut output.content[idx] {
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                ..
            } => {
                thinking.push_str(text);
                *thinking_signature =
                    retain_thought_signature(thinking_signature.take(), incoming_sig);
            }
            ContentBlock::Text {
                text: block_text,
                text_signature,
            } => {
                block_text.push_str(text);
                *text_signature = retain_thought_signature(text_signature.take(), incoming_sig);
            }
            _ => {}
        }

        let event = if is_thinking {
            AssistantMessageEvent::ThinkingDelta {
                content_index: block_index(output),
                delta: text.to_string(),
                partial: output.clone(),
            }
        } else {
            AssistantMessageEvent::TextDelta {
                content_index: block_index(output),
                delta: text.to_string(),
                partial: output.clone(),
            }
        };
        events.push(event);
    }

    // Function-call part.
    if let Some(function_call) = part.get("functionCall") {
        flush_current(output, events, current);
        *current = None;

        let provided_id = function_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let duplicate = provided_id
            .map(|id| {
                output.content.iter().any(|b| match b {
                    ContentBlock::ToolCall { id: existing, .. } => existing == id,
                    _ => false,
                })
            })
            .unwrap_or(false);
        let needs_new_id = provided_id.is_none() || duplicate;
        let tool_call_id = if needs_new_id {
            *tool_call_counter += 1;
            format!("{}_{}_{}", name, now_ms, tool_call_counter)
        } else {
            provided_id.unwrap().to_string()
        };

        let arguments = function_call
            .get("args")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| json!({}));
        let thought_signature = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let tool_call = ContentBlock::ToolCall {
            id: tool_call_id,
            name,
            arguments: arguments.clone(),
            thought_signature,
        };
        output.content.push(tool_call.clone());

        events.push(AssistantMessageEvent::ToolcallStart {
            content_index: block_index(output),
            partial: output.clone(),
        });
        events.push(AssistantMessageEvent::ToolcallDelta {
            content_index: block_index(output),
            delta: serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
            partial: output.clone(),
        });
        events.push(AssistantMessageEvent::ToolcallEnd {
            content_index: block_index(output),
            tool_call,
            partial: output.clone(),
        });
    }
}

fn block_index(output: &AssistantMessage) -> u32 {
    (output.content.len() - 1) as u32
}

fn flush_current(
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
    current: &mut Option<CurrentBlock>,
) {
    let Some(kind) = current.take() else {
        return;
    };
    let idx = output.content.len() - 1;
    match (&kind, &output.content[idx]) {
        (CurrentBlock::Text, ContentBlock::Text { text, .. }) => {
            let content = text.clone();
            events.push(AssistantMessageEvent::TextEnd {
                content_index: idx as u32,
                content,
                partial: output.clone(),
            });
        }
        (CurrentBlock::Thinking, ContentBlock::Thinking { thinking, .. }) => {
            let content = thinking.clone();
            events.push(AssistantMessageEvent::ThinkingEnd {
                content_index: idx as u32,
                content,
                partial: output.clone(),
            });
        }
        _ => {}
    }
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

#[cfg(test)]
mod tests;
