// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `api/bedrock-converse-stream.ts` stream loop: the per-item dispatch arms
// (`contentBlockStart` / `contentBlockDelta` / `contentBlockStop` build a
// matching block then push a mirrored event) share pi's hand-rolled shape by
// design. The clone detector reads the mirrored arms as duplicates; factoring
// them would distort the byte-faithful port, so the repetition is intentional.
//! Amazon Bedrock `ConverseStream` decode half, split out of [`super`] to keep
//! each faithful-port file under the straitjacket line ceiling. Ported from
//! pi-ai's `packages/ai/src/api/bedrock-converse-stream.ts` at pinned commit
//! `3da591ab`: the `for await (const item of response.stream)` inner loop plus
//! its `handle*` helpers and stop-reason / error mapping.

use serde::Serialize;
use serde_json::{json, Value};

use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, StopReason, Usage,
    UsageCost,
};
use crate::utils::json_parse::parse_streaming_json;

use super::BedrockModel;

// ---------------------------------------------------------------------------
// Stop-reason mapping (`mapStopReason`, `bedrock-converse-stream.ts:939`)
// ---------------------------------------------------------------------------

/// The `(stopReason, errorMessage)` pi derives from a Converse `stopReason`.
fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    match reason {
        Some("end_turn") | Some("stop_sequence") => (StopReason::Stop, None),
        Some("max_tokens") | Some("model_context_window_exceeded") => (StopReason::Length, None),
        Some("tool_use") => (StopReason::ToolUse, None),
        Some(other) => (StopReason::Error, Some(other.to_string())),
        None => (StopReason::Error, None),
    }
}

/// Human-readable prefixes for Bedrock SDK exception names
/// (`BEDROCK_ERROR_PREFIXES`, `bedrock-converse-stream.ts:315`).
fn bedrock_error_prefix(name: &str) -> &str {
    match name {
        "InternalServerException" => "Internal server error",
        "ModelStreamErrorException" => "Model stream error",
        "ValidationException" => "Validation error",
        "ThrottlingException" => "Throttling error",
        "ServiceUnavailableException" => "Service unavailable",
        other => other,
    }
}

const BEDROCK_DATA_RETENTION_DOCS_URL: &str =
    "https://docs.aws.amazon.com/bedrock/latest/userguide/data-retention.html";

fn data_retention_hint(core: &str) -> String {
    if core.to_lowercase().contains("data retention mode") {
        format!(" See {BEDROCK_DATA_RETENTION_DOCS_URL} for supported data retention modes.")
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Stream decode (`bedrock-converse-stream.ts:250`)
// ---------------------------------------------------------------------------

/// The result of decoding a Converse stream: the full event sequence and the
/// accumulated final message (identical shape to the Mistral port's outcome).
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

/// Per-content-block streaming scratch state, mirroring pi's `Block` extras
/// (`index`, `partialJson`).
struct DecoderState {
    /// The Converse `contentBlockIndex` correlated with each `output.content`
    /// slot, or `None` once the block has been finalized (`delete block.index`).
    block_indices: Vec<Option<i64>>,
    /// Streaming tool-argument scratch buffers keyed by `output.content` position.
    partial_json: std::collections::HashMap<usize, String>,
}

impl DecoderState {
    fn new() -> Self {
        Self {
            block_indices: Vec::new(),
            partial_json: std::collections::HashMap::new(),
        }
    }

    fn find(&self, content_block_index: i64) -> Option<usize> {
        self.block_indices
            .iter()
            .position(|b| *b == Some(content_block_index))
    }
}

/// Decode already-parsed Converse stream event items (pi's `response.stream`
/// async-iterable) into the uniform event stream and final message for `model`.
///
/// Mirrors pi's `stream()` inner loop: `messageStart` pushes a `start` event,
/// `contentBlock*`/`metadata`/`messageStop` accumulate blocks and usage, and a
/// terminal `done` is pushed — unless an exception item or an error/aborted
/// stop reason is seen, in which case pi's `catch` records a terminal `error`
/// event via `formatBedrockError`.
pub fn parse_converse_stream(
    items: &[Value],
    model: &BedrockModel,
    timestamp: i64,
) -> StreamOutcome {
    let mut output = AssistantMessage {
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
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    let mut state = DecoderState::new();
    let mut thrown: Option<String> = None;

    for item in items {
        if let Some(message_start) = item.get("messageStart") {
            let role = message_start.get("role").and_then(Value::as_str);
            if role != Some("assistant") {
                thrown = Some(
                    "Unexpected assistant message start but got user message start instead"
                        .to_string(),
                );
                break;
            }
            events.push(AssistantMessageEvent::Start {
                partial: output.clone(),
            });
        } else if let Some(event) = item.get("contentBlockStart") {
            handle_content_block_start(event, &mut output, &mut state, &mut events);
        } else if let Some(event) = item.get("contentBlockDelta") {
            handle_content_block_delta(event, &mut output, &mut state, &mut events);
        } else if let Some(event) = item.get("contentBlockStop") {
            handle_content_block_stop(event, &mut output, &mut state, &mut events);
        } else if let Some(event) = item.get("messageStop") {
            let (stop_reason, error_message) =
                map_stop_reason(event.get("stopReason").and_then(Value::as_str));
            output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                output.error_message = Some(error_message);
            }
        } else if let Some(event) = item.get("metadata") {
            handle_metadata(event, model, &mut output);
        } else if let Some(name) = exception_item_name(item) {
            let message = item
                .get(name)
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let exception_name = exception_type_name(name);
            let core = message.clone();
            thrown = Some(format!(
                "{}: {}{}",
                bedrock_error_prefix(exception_name),
                core,
                data_retention_hint(&core)
            ));
            break;
        }
    }

    // pi's post-loop guard + catch: a thrown error, or an error/aborted stop, is
    // surfaced as a terminal `error` event rather than `done`. Streaming scratch
    // fields (`index` / `partialJson`) are dropped before the message escapes.
    if thrown.is_none() && matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let message = output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_string());
        thrown = Some(format!("{}{}", message, data_retention_hint(&message)));
    }

    if let Some(message) = thrown {
        output.stop_reason = StopReason::Error;
        output.error_message = Some(message);
        events.push(AssistantMessageEvent::Error {
            reason: output.stop_reason,
            error: output.clone(),
        });
    } else {
        events.push(AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output.clone(),
        });
    }

    StreamOutcome {
        events,
        message: output,
    }
}

fn exception_item_name(item: &Value) -> Option<&'static str> {
    const NAMES: [&str; 5] = [
        "internalServerException",
        "modelStreamErrorException",
        "validationException",
        "throttlingException",
        "serviceUnavailableException",
    ];
    NAMES.into_iter().find(|name| item.get(name).is_some())
}

fn exception_type_name(item_key: &str) -> &'static str {
    match item_key {
        "internalServerException" => "InternalServerException",
        "modelStreamErrorException" => "ModelStreamErrorException",
        "validationException" => "ValidationException",
        "throttlingException" => "ThrottlingException",
        "serviceUnavailableException" => "ServiceUnavailableException",
        _ => "UnknownException",
    }
}

/// `handleContentBlockStart` (`bedrock-converse-stream.ts:440`).
fn handle_content_block_start(
    event: &Value,
    output: &mut AssistantMessage,
    state: &mut DecoderState,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let index = event
        .get("contentBlockIndex")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let start = event.get("start");

    if let Some(tool_use) = start.and_then(|s| s.get("toolUse")) {
        let id = tool_use
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = tool_use
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        output.content.push(ContentBlock::ToolCall {
            id,
            name,
            arguments: json!({}),
            thought_signature: None,
        });
        state.block_indices.push(Some(index));
        let pos = output.content.len() - 1;
        state.partial_json.insert(pos, String::new());
        events.push(AssistantMessageEvent::ToolcallStart {
            content_index: pos as u32,
            partial: output.clone(),
        });
    }
}

/// `handleContentBlockDelta` (`bedrock-converse-stream.ts:463`).
fn handle_content_block_delta(
    event: &Value,
    output: &mut AssistantMessage,
    state: &mut DecoderState,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let content_block_index = event
        .get("contentBlockIndex")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let delta = event.get("delta");
    let existing = state.find(content_block_index);

    if let Some(text) = delta.and_then(|d| d.get("text")).and_then(Value::as_str) {
        // Text blocks get no contentBlockStart; create one lazily.
        let pos = match existing {
            Some(pos) if matches!(output.content.get(pos), Some(ContentBlock::Text { .. })) => pos,
            _ => {
                output.content.push(ContentBlock::Text {
                    text: String::new(),
                    text_signature: None,
                });
                state.block_indices.push(Some(content_block_index));
                let pos = output.content.len() - 1;
                events.push(AssistantMessageEvent::TextStart {
                    content_index: pos as u32,
                    partial: output.clone(),
                });
                pos
            }
        };
        if let Some(ContentBlock::Text { text: buf, .. }) = output.content.get_mut(pos) {
            buf.push_str(text);
        }
        events.push(AssistantMessageEvent::TextDelta {
            content_index: pos as u32,
            delta: text.to_string(),
            partial: output.clone(),
        });
        return;
    }

    if let Some(tool_use) = delta.and_then(|d| d.get("toolUse")) {
        if let Some(pos) = existing {
            if matches!(output.content.get(pos), Some(ContentBlock::ToolCall { .. })) {
                let input = tool_use
                    .get("input")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let buffer = state.partial_json.entry(pos).or_default();
                buffer.push_str(&input);
                let parsed = parse_streaming_json(Some(buffer));
                if let Some(ContentBlock::ToolCall { arguments, .. }) = output.content.get_mut(pos)
                {
                    *arguments = parsed;
                }
                events.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: pos as u32,
                    delta: input,
                    partial: output.clone(),
                });
            }
        }
        return;
    }

    if let Some(reasoning) = delta.and_then(|d| d.get("reasoningContent")) {
        let pos = match existing {
            Some(pos) => pos,
            None => {
                output.content.push(ContentBlock::Thinking {
                    thinking: String::new(),
                    thinking_signature: Some(String::new()),
                    redacted: None,
                });
                state.block_indices.push(Some(content_block_index));
                let pos = output.content.len() - 1;
                events.push(AssistantMessageEvent::ThinkingStart {
                    content_index: pos as u32,
                    partial: output.clone(),
                });
                pos
            }
        };

        if matches!(output.content.get(pos), Some(ContentBlock::Thinking { .. })) {
            if let Some(text) = reasoning.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    if let Some(ContentBlock::Thinking { thinking, .. }) =
                        output.content.get_mut(pos)
                    {
                        thinking.push_str(text);
                    }
                    events.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: pos as u32,
                        delta: text.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            if let Some(signature) = reasoning.get("signature").and_then(Value::as_str) {
                if let Some(ContentBlock::Thinking {
                    thinking_signature, ..
                }) = output.content.get_mut(pos)
                {
                    let existing = thinking_signature.get_or_insert_with(String::new);
                    existing.push_str(signature);
                }
            }
        }
    }
}

/// `handleContentBlockStop` (`bedrock-converse-stream.ts:536`).
fn handle_content_block_stop(
    event: &Value,
    output: &mut AssistantMessage,
    state: &mut DecoderState,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let content_block_index = event
        .get("contentBlockIndex")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let Some(pos) = state.find(content_block_index) else {
        return;
    };
    // delete block.index
    if let Some(slot) = state.block_indices.get_mut(pos) {
        *slot = None;
    }

    match output.content.get(pos) {
        Some(ContentBlock::Text { text, .. }) => {
            let content = text.clone();
            events.push(AssistantMessageEvent::TextEnd {
                content_index: pos as u32,
                content,
                partial: output.clone(),
            });
        }
        Some(ContentBlock::Thinking { thinking, .. }) => {
            let content = thinking.clone();
            events.push(AssistantMessageEvent::ThinkingEnd {
                content_index: pos as u32,
                content,
                partial: output.clone(),
            });
        }
        Some(ContentBlock::ToolCall { .. }) => {
            let buffer = state.partial_json.remove(&pos).unwrap_or_default();
            let parsed = parse_streaming_json(Some(&buffer));
            if let Some(ContentBlock::ToolCall { arguments, .. }) = output.content.get_mut(pos) {
                *arguments = parsed;
            }
            let tool_call = output.content[pos].clone();
            events.push(AssistantMessageEvent::ToolcallEnd {
                content_index: pos as u32,
                tool_call,
                partial: output.clone(),
            });
        }
        _ => {}
    }
}

/// `handleMetadata` (`bedrock-converse-stream.ts:521`).
fn handle_metadata(event: &Value, model: &BedrockModel, output: &mut AssistantMessage) {
    let Some(usage) = event.get("usage") else {
        return;
    };
    let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    output.usage.input = field("inputTokens");
    output.usage.output = field("outputTokens");
    output.usage.cache_read = field("cacheReadInputTokens");
    output.usage.cache_write = field("cacheWriteInputTokens");
    let total = usage.get("totalTokens").and_then(Value::as_u64);
    output.usage.total_tokens = total.unwrap_or(output.usage.input + output.usage.output);
    output.usage.cost = calculate_cost_with(&model.cost, &output.usage);
}

// ---------------------------------------------------------------------------
// JSON boundary entry point
// ---------------------------------------------------------------------------

/// Decode a JSON array of Converse stream event items for the model described by
/// `model_json` and return the [`StreamOutcome`] as a JSON string.
///
/// This is the boundary entry point a napi shim would call: it collects the SDK
/// stream items into a JSON array, hands them here with the JSON-serialized
/// model, and replays the returned `events`.
pub fn parse_converse_stream_to_json(
    items_json: &str,
    model_json: &str,
    timestamp: i64,
) -> Result<String, String> {
    let items: Vec<Value> =
        serde_json::from_str(items_json).map_err(|e| format!("invalid items json: {e}"))?;
    let model: BedrockModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let outcome = parse_converse_stream(&items, &model, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}
