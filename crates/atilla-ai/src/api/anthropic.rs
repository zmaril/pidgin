// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` dispatch: the per-block `content_block_start` arms
// (`thinking` / `redacted_thinking` both build a Thinking block then push a
// matching `ThinkingStart` event) and the parallel `dispatch_content_block_*`
// helpers share pi's hand-rolled event-guard + `match` shape by design. The
// clone detector reads these mirrored arms as duplicates; factoring them would
// distort the byte-faithful port, so the repetition is intentional.
//! Anthropic Messages SSE streaming parser, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` at pinned commit `3da591ab`.
//!
//! This is the SSE-decode + event-dispatch core of pi's hand-rolled Anthropic
//! driver: the part that turns a raw `text/event-stream` body into atilla-ai's
//! uniform [`AssistantMessageEvent`] stream and the accumulated
//! [`AssistantMessage`]. The request-shaping, auth, and HTTP transport of pi's
//! `stream()` live outside this module; here we take an already-obtained SSE
//! body (exactly what pi feeds through `iterateSseMessages`) and reproduce the
//! dispatch that follows.
//!
//! Faithful to pi's behaviour:
//! - The SSE framing ([`iterate_sse_messages`]) mirrors pi's `iterateSseMessages`
//!   line splitting, `event:`/`data:` field accumulation, comment (`:`) skipping,
//!   and trailing-event flush.
//! - The Anthropic event filter + JSON-repair parse mirrors
//!   `iterateAnthropicEvents`, including the `event: error` throw and the
//!   `"Anthropic stream ended before message_stop"` error reproduced byte-for-byte.
//! - The dispatch over `message_start` / `content_block_*` / `message_delta`
//!   maps deltas to `text` / `thinking` / `tool_use` blocks, repairs streamed
//!   tool-argument JSON, accumulates usage, computes cost, and maps stop reasons
//!   exactly as pi's `stream()` inner loop does.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod boundary;
pub mod cache;
pub mod client;
pub mod compat;
pub mod content;
pub mod driver;
pub mod request;
pub mod simple_options;
pub mod thinking;
pub mod tools;

use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, ModelCost, StopReason,
    Usage, UsageCost,
};
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};

/// The Anthropic wire event names pi's `iterateAnthropicEvents` accepts; every
/// other named SSE event (pings, proxy stats, unknown types) is ignored
/// (`anthropic-messages.ts:304-311`).
const ANTHROPIC_MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// The minimum slice of a pi `Model` this driver needs: identity for the output
/// message plus pricing for cost accounting. Deserialized leniently so any
/// additional pi model fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicModel {
    pub id: String,
    pub api: String,
    pub provider: String,
    pub cost: ModelCost,
}

/// A decoded server-sent event (pi's `ServerSentEvent`).
#[derive(Debug, Clone, PartialEq)]
struct ServerSentEvent {
    event: Option<String>,
    data: String,
    raw: Vec<String>,
}

#[derive(Debug, Default)]
struct SseDecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

fn flush_sse_event(state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if state.event.is_none() && state.data.is_empty() {
        return None;
    }
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.data.clear();
    state.raw.clear();
    Some(event)
}

fn decode_sse_line(line: &str, state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }

    state.raw.push(line.to_string());
    if line.starts_with(':') {
        return None;
    }

    let (field_name, mut value) = match line.find(':') {
        None => (line, String::new()),
        Some(idx) => (&line[..idx], line[idx + 1..].to_string()),
    };
    if let Some(stripped) = value.strip_prefix(' ') {
        value = stripped.to_string();
    }

    if field_name == "event" {
        state.event = Some(value);
    } else if field_name == "data" {
        state.data.push(value);
    }

    None
}

fn next_line_break_index(text: &str) -> Option<usize> {
    let cr = text.find('\r');
    let lf = text.find('\n');
    match (cr, lf) {
        (None, lf) => lf,
        (cr, None) => cr,
        (Some(cr), Some(lf)) => Some(cr.min(lf)),
    }
}

/// One line consumed from `text`, plus the remaining tail. Byte indices are
/// safe: the delimiters are ASCII `\r`/`\n`.
fn consume_line(text: &str) -> Option<(String, String)> {
    let line_break = next_line_break_index(text)?;
    let bytes = text.as_bytes();
    let mut next = line_break + 1;
    if bytes[line_break] == b'\r' && bytes.get(next) == Some(&b'\n') {
        next += 1;
    }
    Some((text[..line_break].to_string(), text[next..].to_string()))
}

/// Frame a complete SSE body into events, mirroring pi's `iterateSseMessages`.
///
/// pi streams the body in chunks; here the whole body is available, which yields
/// the identical sequence: consume every full line, then decode any trailing
/// partial line and flush a dangling event.
fn iterate_sse_messages(body: &str) -> Vec<ServerSentEvent> {
    let mut state = SseDecoderState::default();
    let mut events = Vec::new();
    let mut buffer = body.to_string();

    while let Some((line, rest)) = consume_line(&buffer) {
        buffer = rest;
        if let Some(event) = decode_sse_line(&line, &mut state) {
            events.push(event);
        }
    }

    if !buffer.is_empty() {
        if let Some(event) = decode_sse_line(&buffer, &mut state) {
            events.push(event);
        }
    }

    if let Some(event) = flush_sse_event(&mut state) {
        events.push(event);
    }

    events
}

/// Map an Anthropic `stop_reason` (+ refusal details) to a stop reason and
/// optional error message, mirroring pi's `mapStopReason`
/// (`anthropic-messages.ts:1287`). Returns `Err` for an unhandled reason, which
/// pi surfaces by throwing `Unhandled stop reason: <reason>`.
fn map_stop_reason(
    reason: &str,
    stop_details: Option<&Value>,
) -> Result<(StopReason, Option<String>), String> {
    match reason {
        "end_turn" => Ok((StopReason::Stop, None)),
        "max_tokens" => Ok((StopReason::Length, None)),
        "tool_use" => Ok((StopReason::ToolUse, None)),
        "refusal" => {
            let explanation = stop_details
                .and_then(|d| d.get("explanation"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "The model refused to complete the request".to_string());
            Ok((StopReason::Error, Some(explanation)))
        }
        "pause_turn" => Ok((StopReason::Stop, None)),
        "stop_sequence" => Ok((StopReason::Stop, None)),
        "sensitive" => Ok((StopReason::Error, None)),
        other => Err(format!("Unhandled stop reason: {other}")),
    }
}

/// A content block under construction, tracking the Anthropic block `index` and
/// a tool call's growing partial-JSON buffer (pi's local `Block` type).
#[derive(Debug, Clone)]
struct WorkingBlock {
    index: i64,
    block: ContentBlock,
    partial_json: String,
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The result of parsing an Anthropic SSE stream: the full event sequence and
/// the accumulated final message (what pi's `AssistantMessageEventStream.result()`
/// resolves to).
#[derive(Debug, Clone, Serialize)]
pub struct StreamOutcome {
    pub events: Vec<AssistantMessageEvent>,
    pub message: AssistantMessage,
}

/// Parse an Anthropic SSE `body` for the model described by `model_json` and
/// return the [`StreamOutcome`] as a JSON string.
///
/// This is the boundary entry point the napi shim calls: the shim reads the SSE
/// bytes from the injected transport's `Response`, hands them here with the
/// JSON-serialized model, and replays the returned `events` into pi's
/// `AssistantMessageEventStream`. `model_json` need only carry the fields of
/// [`AnthropicModel`] (`id`, `api`, `provider`, `cost`); any extra pi model
/// fields are ignored.
pub fn parse_sse_stream_to_json(
    body: &str,
    model_json: &str,
    is_oauth: bool,
    timestamp: i64,
) -> Result<String, String> {
    let model: AnthropicModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let outcome = parse_sse_stream(body, &model, is_oauth, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}

/// Parse an Anthropic Messages SSE `body` into the uniform event stream and
/// final message for `model`.
///
/// This reproduces pi's `stream()` inner loop (`anthropic-messages.ts:557-755`):
/// it pushes a `start` event, dispatches each Anthropic wire event, accumulates
/// usage and cost, and terminates with a `done` event on success or an `error`
/// event when the stream reports a refusal / error stop, an `event: error`
/// frame, an unparseable event, or ends before `message_stop`.
///
/// `is_oauth` selects Claude-Code tool-name normalization; the caller passes
/// `false` when a client/transport was injected (the tool name is used verbatim,
/// as in pi's non-OAuth path). `timestamp` is the message timestamp pi sets via
/// `Date.now()`.
pub fn parse_sse_stream(
    body: &str,
    model: &AnthropicModel,
    is_oauth: bool,
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
    let mut blocks: Vec<WorkingBlock> = Vec::new();
    let mut events: Vec<AssistantMessageEvent> = Vec::new();

    // Mirrors pi: the `start` event is emitted before the dispatch loop.
    events.push(AssistantMessageEvent::Start {
        partial: render_partial(&output, &blocks),
    });

    let terminal_error = run_dispatch(body, model, is_oauth, &mut output, &mut blocks, &mut events);

    output.content = render_content(&blocks);

    match terminal_error {
        None => {
            // pi's post-loop guard: an error/aborted stop is re-thrown and
            // surfaced as an error event rather than a done event.
            if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
                let message = output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "An unknown error occurred".to_string());
                finish_with_error(&mut output, &mut events, message);
            } else {
                events.push(AssistantMessageEvent::Done {
                    reason: output.stop_reason,
                    message: output.clone(),
                });
            }
        }
        Some(message) => finish_with_error(&mut output, &mut events, message),
    }

    StreamOutcome {
        events,
        message: output,
    }
}

fn finish_with_error(
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
    message: String,
) {
    // No abort signal is modeled here, so the failure is always `error`
    // (pi picks `aborted` only when `signal.aborted`).
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message);
    events.push(AssistantMessageEvent::Error {
        reason: output.stop_reason,
        error: output.clone(),
    });
}

/// Run the Anthropic event dispatch. Returns `Some(message)` when the stream
/// terminates with a hard error (an `event: error` frame, an unparseable event,
/// or ending before `message_stop`), matching where pi's `iterateAnthropicEvents`
/// throws.
fn run_dispatch(
    body: &str,
    model: &AnthropicModel,
    is_oauth: bool,
    output: &mut AssistantMessage,
    blocks: &mut Vec<WorkingBlock>,
    events: &mut Vec<AssistantMessageEvent>,
) -> Option<String> {
    let mut saw_message_start = false;
    let mut saw_message_end = false;

    for sse in iterate_sse_messages(body) {
        let name = sse.event.as_deref().unwrap_or("");

        if name == "error" {
            return Some(sse.data);
        }
        if !ANTHROPIC_MESSAGE_EVENTS.contains(&name) {
            continue;
        }

        let event = match parse_json_with_repair(&sse.data) {
            Ok(value) => value,
            Err(error) => {
                return Some(format!(
                    "Could not parse Anthropic SSE event {}: {}; data={}; raw={}",
                    sse.event.as_deref().unwrap_or("null"),
                    error,
                    sse.data,
                    sse.raw.join("\\n"),
                ));
            }
        };

        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                saw_message_start = true;
                // pi emits no event for message_start beyond the initial `start`
                // already pushed before the loop; it only captures usage.
                let _ = &events;
                dispatch_message_start(&event, model, output);
            }
            Some("message_stop") => {
                saw_message_end = true;
            }
            Some("content_block_start") => {
                dispatch_content_block_start(&event, is_oauth, output, blocks, events);
            }
            Some("content_block_delta") => {
                dispatch_content_block_delta(&event, output, blocks, events);
            }
            Some("content_block_stop") => {
                dispatch_content_block_stop(&event, output, blocks, events);
            }
            Some("message_delta") => {
                if let Err(message) = dispatch_message_delta(&event, model, output) {
                    return Some(message);
                }
            }
            _ => {}
        }
    }

    if saw_message_start && !saw_message_end {
        return Some("Anthropic stream ended before message_stop".to_string());
    }

    None
}

fn dispatch_message_start(event: &Value, model: &AnthropicModel, output: &mut AssistantMessage) {
    let message = event.get("message");
    if let Some(id) = message.and_then(|m| m.get("id")).and_then(Value::as_str) {
        output.response_id = Some(id.to_string());
    }
    let usage = message.and_then(|m| m.get("usage"));
    if let Some(usage) = usage {
        output.usage.input = u64_field(usage, "input_tokens");
        output.usage.output = u64_field(usage, "output_tokens");
        output.usage.cache_read = u64_field(usage, "cache_read_input_tokens");
        output.usage.cache_write = u64_field(usage, "cache_creation_input_tokens");
        let ephemeral_1h = usage
            .get("cache_creation")
            .and_then(|c| c.get("ephemeral_1h_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        output.usage.cache_write_1h = Some(ephemeral_1h);
    }
    recompute_totals_and_cost(model, &mut output.usage);
}

fn dispatch_content_block_start(
    event: &Value,
    is_oauth: bool,
    output: &mut AssistantMessage,
    blocks: &mut Vec<WorkingBlock>,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let index = event.get("index").and_then(Value::as_i64).unwrap_or(0);
    let content_block = match event.get("content_block") {
        Some(cb) => cb,
        None => return,
    };
    let block_type = content_block.get("type").and_then(Value::as_str);

    match block_type {
        Some("text") => {
            blocks.push(WorkingBlock {
                index,
                block: ContentBlock::Text {
                    text: String::new(),
                    text_signature: None,
                },
                partial_json: String::new(),
            });
            events.push(AssistantMessageEvent::TextStart {
                content_index: (blocks.len() - 1) as u32,
                partial: render_partial(output, blocks),
            });
        }
        Some("thinking") => {
            blocks.push(WorkingBlock {
                index,
                block: ContentBlock::Thinking {
                    thinking: String::new(),
                    thinking_signature: Some(String::new()),
                    redacted: None,
                },
                partial_json: String::new(),
            });
            events.push(AssistantMessageEvent::ThinkingStart {
                content_index: (blocks.len() - 1) as u32,
                partial: render_partial(output, blocks),
            });
        }
        Some("redacted_thinking") => {
            let data = content_block
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            blocks.push(WorkingBlock {
                index,
                block: ContentBlock::Thinking {
                    thinking: "[Reasoning redacted]".to_string(),
                    thinking_signature: Some(data),
                    redacted: Some(true),
                },
                partial_json: String::new(),
            });
            events.push(AssistantMessageEvent::ThinkingStart {
                content_index: (blocks.len() - 1) as u32,
                partial: render_partial(output, blocks),
            });
        }
        Some("tool_use") => {
            let id = content_block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let raw_name = content_block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // The Claude-Code name normalization (`fromClaudeCodeName`) only runs
            // on the OAuth path, which requires the request's tool list; the
            // injected-transport path this parser serves is non-OAuth, so the
            // name is used verbatim, matching pi.
            let _ = is_oauth;
            let name = raw_name;
            let arguments = content_block
                .get("input")
                .cloned()
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| Value::Object(Map::new()));
            blocks.push(WorkingBlock {
                index,
                block: ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                    thought_signature: None,
                },
                partial_json: String::new(),
            });
            events.push(AssistantMessageEvent::ToolcallStart {
                content_index: (blocks.len() - 1) as u32,
                partial: render_partial(output, blocks),
            });
        }
        _ => {}
    }
}

fn dispatch_content_block_delta(
    event: &Value,
    output: &mut AssistantMessage,
    blocks: &mut [WorkingBlock],
    events: &mut Vec<AssistantMessageEvent>,
) {
    let event_index = event.get("index").and_then(Value::as_i64).unwrap_or(0);
    let delta = match event.get("delta") {
        Some(d) => d,
        None => return,
    };
    let delta_type = delta.get("type").and_then(Value::as_str);
    let Some(pos) = blocks.iter().position(|b| b.index == event_index) else {
        return;
    };

    match delta_type {
        Some("text_delta") => {
            let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
            if let ContentBlock::Text {
                text: block_text, ..
            } = &mut blocks[pos].block
            {
                block_text.push_str(text);
                events.push(AssistantMessageEvent::TextDelta {
                    content_index: pos as u32,
                    delta: text.to_string(),
                    partial: render_partial(output, blocks),
                });
            }
        }
        Some("thinking_delta") => {
            let thinking = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
            if let ContentBlock::Thinking {
                thinking: block_thinking,
                ..
            } = &mut blocks[pos].block
            {
                block_thinking.push_str(thinking);
                events.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: pos as u32,
                    delta: thinking.to_string(),
                    partial: render_partial(output, blocks),
                });
            }
        }
        Some("input_json_delta") => {
            let partial = delta
                .get("partial_json")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(blocks[pos].block, ContentBlock::ToolCall { .. }) {
                blocks[pos].partial_json.push_str(partial);
                let parsed = parse_streaming_json(Some(&blocks[pos].partial_json));
                if let ContentBlock::ToolCall { arguments, .. } = &mut blocks[pos].block {
                    *arguments = parsed;
                }
                events.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: pos as u32,
                    delta: partial.to_string(),
                    partial: render_partial(output, blocks),
                });
            }
        }
        Some("signature_delta") => {
            let signature = delta.get("signature").and_then(Value::as_str).unwrap_or("");
            if let ContentBlock::Thinking {
                thinking_signature, ..
            } = &mut blocks[pos].block
            {
                let current = thinking_signature.take().unwrap_or_default();
                *thinking_signature = Some(current + signature);
            }
        }
        _ => {}
    }
}

fn dispatch_content_block_stop(
    event: &Value,
    output: &mut AssistantMessage,
    blocks: &mut [WorkingBlock],
    events: &mut Vec<AssistantMessageEvent>,
) {
    let event_index = event.get("index").and_then(Value::as_i64).unwrap_or(0);
    let Some(pos) = blocks.iter().position(|b| b.index == event_index) else {
        return;
    };

    // Emit the terminal event for the block. The partial reflects the block's
    // finalized state; content indices are the block's position.
    match &blocks[pos].block {
        ContentBlock::Text { text, .. } => {
            let content = text.clone();
            events.push(AssistantMessageEvent::TextEnd {
                content_index: pos as u32,
                content,
                partial: render_partial(output, blocks),
            });
        }
        ContentBlock::Thinking { thinking, .. } => {
            let content = thinking.clone();
            events.push(AssistantMessageEvent::ThinkingEnd {
                content_index: pos as u32,
                content,
                partial: render_partial(output, blocks),
            });
        }
        ContentBlock::ToolCall { .. } => {
            let parsed = parse_streaming_json(Some(&blocks[pos].partial_json));
            if let ContentBlock::ToolCall { arguments, .. } = &mut blocks[pos].block {
                *arguments = parsed;
            }
            let tool_call = blocks[pos].block.clone();
            events.push(AssistantMessageEvent::ToolcallEnd {
                content_index: pos as u32,
                tool_call,
                partial: render_partial(output, blocks),
            });
        }
        ContentBlock::Image { .. } | ContentBlock::Unknown => {}
    }
}

fn dispatch_message_delta(
    event: &Value,
    model: &AnthropicModel,
    output: &mut AssistantMessage,
) -> Result<(), String> {
    let delta = event.get("delta");
    if let Some(stop_reason) = delta
        .and_then(|d| d.get("stop_reason"))
        .and_then(Value::as_str)
    {
        let stop_details = delta.and_then(|d| d.get("stop_details"));
        let (reason, error_message) = map_stop_reason(stop_reason, stop_details)?;
        output.stop_reason = reason;
        if let Some(error_message) = error_message {
            output.error_message = Some(error_message);
        }
    }

    // Only update usage fields that are present (not null), preserving
    // message_start values when a proxy omits them here.
    if let Some(usage) = event.get("usage").filter(|u| !u.is_null()) {
        if let Some(v) = usage.get("input_tokens").and_then(Value::as_u64) {
            output.usage.input = v;
        }
        if let Some(v) = usage.get("output_tokens").and_then(Value::as_u64) {
            output.usage.output = v;
        }
        if let Some(v) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
            output.usage.cache_read = v;
        }
        if let Some(v) = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
        {
            output.usage.cache_write = v;
        }
        if let Some(v) = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("thinking_tokens"))
            .and_then(Value::as_u64)
        {
            output.usage.reasoning = Some(v);
        }
    }

    recompute_totals_and_cost(model, &mut output.usage);
    Ok(())
}

fn recompute_totals_and_cost(model: &AnthropicModel, usage: &mut Usage) {
    usage.total_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
    usage.cost = calculate_cost_with(&model.cost, usage);
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

/// Render the working blocks as finalized content blocks (no `index`/`partialJson`
/// scratch leaks; those fields do not exist on [`ContentBlock`]).
fn render_content(blocks: &[WorkingBlock]) -> Vec<ContentBlock> {
    blocks.iter().map(|b| b.block.clone()).collect()
}

/// A snapshot of the accumulating message for a non-terminal event's `partial`.
fn render_partial(output: &AssistantMessage, blocks: &[WorkingBlock]) -> AssistantMessage {
    let mut partial = output.clone();
    partial.content = render_content(blocks);
    partial
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod request_tests;

#[cfg(test)]
pub(crate) mod driver_tests;
