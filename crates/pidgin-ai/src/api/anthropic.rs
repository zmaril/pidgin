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
//! driver: the part that turns a raw `text/event-stream` body into pidgin-ai's
//! uniform [`AssistantMessageEvent`] stream and the accumulated
//! [`AssistantMessage`]. The request-shaping, auth, and HTTP transport of pi's
//! `stream()` live outside this module; here we take an already-obtained SSE
//! body (exactly what pi feeds through `iterateSseMessages`) and reproduce the
//! dispatch that follows.
//!
//! Faithful to pi's behaviour:
//! - The SSE framing (the shared [`SseFrameSplitter`](crate::utils::sse::SseFrameSplitter))
//!   mirrors pi's `iterateSseMessages` line splitting, `event:`/`data:` field
//!   accumulation, comment (`:`) skipping, and trailing-event flush.
//! - The Anthropic event filter + JSON-repair parse mirrors
//!   `iterateAnthropicEvents`, including the `event: error` throw and the
//!   `"Anthropic stream ended before message_stop"` error reproduced byte-for-byte.
//! - The dispatch over `message_start` / `content_block_*` / `message_delta`
//!   maps deltas to `text` / `thinking` / `tool_use` blocks, repairs streamed
//!   tool-argument JSON, accumulates usage, computes cost, and maps stop reasons
//!   exactly as pi's `stream()` inner loop does.

use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod boundary;
pub mod cache;
pub mod client;
pub mod compat;
pub mod content;
pub mod deferred_tools;
pub mod driver;
pub mod estimate;
pub mod request;
pub mod simple_options;
pub mod thinking;
pub mod tools;
pub mod transform_messages;

use crate::cost::calculate_cost_with;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, ModelCost, StopReason,
    Usage, UsageCost,
};
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

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
    // Single source of truth: feed the whole body through the SAME
    // [`AnthropicSseDecoder`] the incremental driver uses, over a one-chunk
    // iterator. The shared [`AssistantEventReader`] flushes the frame splitter
    // and runs `finish` at EOF, so the events + terminal message are
    // byte-identical to feeding the reader chunk-by-chunk.
    let decoder = AnthropicSseDecoder::new(model.clone(), is_oauth, timestamp);
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
    // No abort signal is modeled here, so the failure is always `error`
    // (pi picks `aborted` only when `signal.aborted`).
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message);
    events.push(AssistantMessageEvent::Error {
        reason: output.stop_reason,
        error: output.clone(),
    });
}

/// The incremental Anthropic Messages SSE decoder: the single source of truth
/// for turning wire frames into assistant events and the accumulated message.
///
/// It carries exactly the accumulation state pi's `stream()` inner loop kept —
/// the output message, the working blocks, and the `message_start` /
/// `message_stop` bookkeeping — and drives it via the shared
/// [`SseEventDecoder`] seam. The buffered [`parse_sse_stream`] and the driver's
/// incremental reader both run this ONE decoder, so their events and final
/// message are byte-identical.
pub(crate) struct AnthropicSseDecoder {
    model: AnthropicModel,
    is_oauth: bool,
    output: AssistantMessage,
    blocks: Vec<WorkingBlock>,
    /// pi emits the `start` event before the dispatch loop; here it is emitted
    /// lazily on the first `on_frame`/`finish` so it is always the first event
    /// exactly once, whatever the chunk cadence.
    started: bool,
    saw_message_start: bool,
    saw_message_end: bool,
    /// Set when a frame triggers pi's `iterateAnthropicEvents` throw (an
    /// `event: error` frame, an unparseable event, or an unhandled stop reason);
    /// `finish` turns it into the terminal `error` event.
    terminal_error: Option<String>,
}

impl AnthropicSseDecoder {
    /// A fresh decoder for `model`, seeding the empty output shell pi builds
    /// before streaming. `is_oauth` selects Claude-Code tool-name normalization
    /// and `timestamp` is pi's `Date.now()` message timestamp.
    pub(crate) fn new(model: AnthropicModel, is_oauth: bool, timestamp: i64) -> Self {
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
            is_oauth,
            output,
            blocks: Vec::new(),
            started: false,
            saw_message_start: false,
            saw_message_end: false,
            terminal_error: None,
        }
    }

    /// Emit pi's initial `start` event exactly once, before any frame's events.
    fn ensure_started(&mut self, out: &mut Vec<AssistantMessageEvent>) {
        if !self.started {
            self.started = true;
            out.push(AssistantMessageEvent::Start {
                partial: render_partial(&self.output, &self.blocks),
            });
        }
    }
}

impl SseEventDecoder for AnthropicSseDecoder {
    fn on_frame(
        &mut self,
        frame: &ServerSentEvent,
        out: &mut Vec<AssistantMessageEvent>,
    ) -> ControlFlow<String> {
        self.ensure_started(out);
        let name = frame.event.as_deref().unwrap_or("");

        if name == "error" {
            self.terminal_error = Some(frame.data.clone());
            return ControlFlow::Break(frame.data.clone());
        }
        if !ANTHROPIC_MESSAGE_EVENTS.contains(&name) {
            return ControlFlow::Continue(());
        }

        let event = match parse_json_with_repair(&frame.data) {
            Ok(value) => value,
            Err(error) => {
                let message = format!(
                    "Could not parse Anthropic SSE event {}: {}; data={}; raw={}",
                    frame.event.as_deref().unwrap_or("null"),
                    error,
                    frame.data,
                    frame.raw.join("\\n"),
                );
                self.terminal_error = Some(message.clone());
                return ControlFlow::Break(message);
            }
        };

        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                self.saw_message_start = true;
                // pi emits no event for message_start beyond the initial `start`
                // already emitted above; it only captures usage.
                dispatch_message_start(&event, &self.model, &mut self.output);
            }
            Some("message_stop") => {
                self.saw_message_end = true;
            }
            Some("content_block_start") => {
                dispatch_content_block_start(
                    &event,
                    self.is_oauth,
                    &mut self.output,
                    &mut self.blocks,
                    out,
                );
            }
            Some("content_block_delta") => {
                dispatch_content_block_delta(&event, &mut self.output, &mut self.blocks, out);
            }
            Some("content_block_stop") => {
                dispatch_content_block_stop(&event, &mut self.output, &mut self.blocks, out);
            }
            Some("message_delta") => {
                if let Err(message) = dispatch_message_delta(&event, &self.model, &mut self.output)
                {
                    self.terminal_error = Some(message.clone());
                    return ControlFlow::Break(message);
                }
            }
            _ => {}
        }

        ControlFlow::Continue(())
    }

    fn finish(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
        self.ensure_started(out);
        self.output.content = render_content(&self.blocks);

        // pi's post-loop terminal selection: a frame-level throw wins, then the
        // `message_stop` guard, then the error/aborted stop-reason re-throw,
        // otherwise a `done` event.
        let terminal_error = if self.terminal_error.is_some() {
            self.terminal_error.clone()
        } else if self.saw_message_start && !self.saw_message_end {
            Some("Anthropic stream ended before message_stop".to_string())
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
