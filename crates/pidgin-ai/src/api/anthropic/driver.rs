// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` `stream` and `streamSimple` drivers. The output-shell
// construction and the thinking-mode branch selection mirror pi's object
// literals and `if` ladder verbatim; the clone detector may read the shared
// AssistantMessage scaffolding as duplicative by design.
//! The Anthropic Messages stream drivers, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` `stream` / `streamSimple` at
//! pinned commit `3da591ab`.
//!
//! [`stream`] is the buffered, seam-targeted analogue of pi's `stream()`: it
//! picks the auth mode, builds the request body ([`build_params`]), assembles the
//! [`HttpRequest`] ([`assemble_request`]), performs it over the injected
//! [`HttpTransport`], and feeds the response body into the already-ported SSE
//! parser ([`parse_sse_stream`]), returning the eager [`StreamResult`].
//!
//! # Streaming model
//!
//! pi's `stream()` is asynchronous and pushes events into an
//! `AssistantMessageEventStream` as SSE chunks arrive. The Rust core (matching
//! the Stage-2 parser and the [`Provider`](crate::seams::provider::Provider)
//! seam) produces the whole event sequence eagerly from a fully-buffered
//! response body; inter-chunk timing is re-presented at the binding edge. This is
//! exactly the shape pi's fixture tests exercise — they stub `fetch` to return a
//! canned `Response` whose complete SSE body is then parsed.
//!
//! # Error surfacing
//!
//! pi encodes post-start failures as a terminal `error` event. Failures that
//! occur before the stream starts (missing auth, a non-2xx create, a transport
//! error) throw in pi and are caught by the same handler, which pushes a single
//! `error` event with no preceding `start`. The buffered driver reproduces that:
//! a truncated 200 body surfaces the parser's
//! `"Anthropic stream ended before message_stop"`; a non-2xx status, a transport
//! error, or a missing credential yield an error-only [`StreamResult`]. A non-2xx
//! create carries the API's diagnostic through [`format_api_error`], mirroring
//! the SDK `APIError`'s `` `${status} ${message}` `` shape pi surfaces
//! (`anthropic-messages.ts:752`), rather than discarding the response body.

use std::ops::ControlFlow;

use crate::seams::http::HttpTransport;
use crate::seams::provider::StreamResult;
use crate::types::{
    AnthropicMessagesCompat, AssistantMessage, AssistantMessageEvent, AssistantRole,
    CacheRetention, Context, Model, StopReason, Usage, UsageCost,
};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

use super::cache::resolve_cache_retention;
use super::client::{
    assemble_request, assert_request_auth, resolve_auth_mode, serialize_body, AuthMode,
};
use super::request::{build_params, AnthropicOptions};
use super::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
    SimpleStreamOptions,
};
use super::thinking::map_thinking_level_to_effort;
use super::{parse_sse_stream, AnthropicModel, AnthropicSseDecoder};

/// Build the lean [`AnthropicModel`] the SSE parser needs (identity + pricing)
/// from a full boundary model.
fn anthropic_model(model: &Model<AnthropicMessagesCompat>) -> AnthropicModel {
    AnthropicModel {
        id: model.id.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        cost: model.cost.clone(),
    }
}

/// The empty assistant output shell pi seeds before streaming
/// (`anthropic-messages.ts:491`).
fn empty_output(model: &Model<AnthropicMessagesCompat>, timestamp: i64) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp,
    }
}

/// A single-`error`-event result, matching pi's catch handler for a failure that
/// occurs before the stream's `start` event (`anthropic-messages.ts:744`).
fn error_result(
    model: &Model<AnthropicMessagesCompat>,
    timestamp: i64,
    message: String,
) -> StreamResult {
    let mut output = empty_output(model, timestamp);
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message);
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: output.clone(),
        }],
        message: output,
    }
}

/// Format a non-2xx create response into the terminal error message, mirroring
/// the Anthropic SDK's `APIError` shape (`` `${status} ${message}` ``) that pi
/// surfaces as the caught `error.message` (`anthropic-messages.ts:752`). The
/// SDK derives the message from the JSON error body's `error.message`
/// (`APIError.makeMessage`); this reproduces that, falling back to the raw body
/// text, then to a no-body marker — so callers see the API's diagnostic instead
/// of a bare status.
fn format_api_error(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    let detail = serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
        });
    match detail {
        Some(message) => format!("{status} {message}"),
        None if !trimmed.is_empty() => format!("{status} {trimmed}"),
        None => format!("{status} status code (no body)"),
    }
}

/// Stream a response for `model` over the injected `transport`, mirroring pi's
/// `stream()` request assembly and SSE handling. `timestamp` is the message
/// timestamp pi sets via `Date.now()` (threaded here for determinism, as the SSE
/// parser already is).
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    options: &AnthropicOptions,
    timestamp: i64,
) -> StreamResult {
    let api_key = options.api_key.as_deref();

    // pi asserts auth before constructing the client; a failure throws and is
    // caught as an error event.
    if let Err(message) = assert_request_auth(&model.provider, api_key, options.headers.as_ref()) {
        return error_result(model, timestamp, message);
    }

    let mode = resolve_auth_mode(&model.provider, api_key);
    let is_oauth = matches!(mode, AuthMode::OAuth);

    // pi: cacheSessionId = cacheRetention === "none" ? undefined : options.sessionId.
    let cache_retention = resolve_cache_retention(options.cache_retention, options.env.as_ref());
    let session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.session_id.as_deref()
    };
    let interleaved_thinking = options.interleaved_thinking.unwrap_or(true);

    // build_params already sets `stream: true`.
    let body = build_params(model, context, is_oauth, options);
    let request = assemble_request(
        mode,
        model,
        context,
        serialize_body(&body),
        api_key,
        options.headers.as_ref(),
        interleaved_thinking,
        session_id,
    );

    match transport.send(&request) {
        Ok(response) if response.is_ok() => {
            let outcome =
                parse_sse_stream(&response.body, &anthropic_model(model), is_oauth, timestamp);
            StreamResult {
                events: outcome.events,
                message: outcome.message,
            }
        }
        Ok(response) => error_result(
            model,
            timestamp,
            format_api_error(response.status, &response.body),
        ),
        Err(error) => error_result(model, timestamp, error.to_string()),
    }
}

/// A decoder that yields nothing per frame and emits a single terminal `error`
/// event at `finish` -- the streaming analogue of [`error_result`] for a failure
/// that occurs before the SSE stream starts (missing auth, a non-2xx create, a
/// transport error). It carries pi's caught `error` message on an empty output
/// shell so [`stream_streaming`] can return an [`AssistantEventReader`] on every
/// path (no preceding `start`, matching pi's pre-stream catch handler).
struct TerminalErrorDecoder {
    error: AssistantMessage,
}

impl SseEventDecoder for TerminalErrorDecoder {
    fn on_frame(
        &mut self,
        _frame: &ServerSentEvent,
        _out: &mut Vec<AssistantMessageEvent>,
    ) -> ControlFlow<String> {
        ControlFlow::Continue(())
    }

    fn finish(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
        out.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: self.error.clone(),
        });
        self.error.clone()
    }
}

/// A single-`error`-event reader over an empty chunk stream, the streaming
/// analogue of [`error_result`]: EOF `finish` emits exactly one terminal `error`
/// (byte-identical to the buffered pre-stream failure).
fn error_reader<'a>(
    model: &Model<AnthropicMessagesCompat>,
    timestamp: i64,
    message: String,
) -> AssistantEventReader<'a> {
    let mut output = empty_output(model, timestamp);
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message);
    AssistantEventReader::new(
        Box::new(std::iter::empty()),
        Box::new(TerminalErrorDecoder { error: output }),
    )
}

/// Buffer a streaming body's chunks into a lossy UTF-8 string, stopping at the
/// first read error -- used only for a non-2xx error body, whose diagnostic is
/// short and which pi reads whole before throwing.
fn drain_chunks<'a>(chunks: Box<dyn Iterator<Item = std::io::Result<Vec<u8>>> + 'a>) -> String {
    let mut body = Vec::new();
    for chunk in chunks {
        match chunk {
            Ok(bytes) => body.extend_from_slice(&bytes),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&body).to_string()
}

/// Stream a response for `model` over the injected `transport`, delivering events
/// incrementally through the shared [`AssistantEventReader`].
///
/// This mirrors [`stream`]'s request assembly and error surfacing but performs
/// the request via [`HttpTransport::send_streaming`], so the returned reader
/// pulls one chunk at a time and decodes it through the SAME
/// [`AnthropicSseDecoder`] the buffered path uses -- one source of truth for the
/// event sequence. A pre-stream failure (missing auth, a non-2xx create, a
/// transport error) yields a single-`error` reader, mirroring pi's catch handler
/// exactly as [`stream`] does. The buffered [`stream`] stays the default backend
/// path until the provider seam is wired to streaming.
pub fn stream_streaming<'a, T: HttpTransport + ?Sized>(
    transport: &'a T,
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    options: &AnthropicOptions,
    timestamp: i64,
) -> AssistantEventReader<'a> {
    let api_key = options.api_key.as_deref();

    // pi asserts auth before constructing the client; a failure throws and is
    // caught as an error event.
    if let Err(message) = assert_request_auth(&model.provider, api_key, options.headers.as_ref()) {
        return error_reader(model, timestamp, message);
    }

    let mode = resolve_auth_mode(&model.provider, api_key);
    let is_oauth = matches!(mode, AuthMode::OAuth);

    let cache_retention = resolve_cache_retention(options.cache_retention, options.env.as_ref());
    let session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.session_id.as_deref()
    };
    let interleaved_thinking = options.interleaved_thinking.unwrap_or(true);

    let body = build_params(model, context, is_oauth, options);
    let request = assemble_request(
        mode,
        model,
        context,
        serialize_body(&body),
        api_key,
        options.headers.as_ref(),
        interleaved_thinking,
        session_id,
    );

    // Status + headers arrive up front, so the error-vs-parse decision is made
    // before the body streams -- exactly as the buffered path decides on
    // `response.is_ok()`.
    match transport.send_streaming(&request) {
        Ok(response) if (200..300).contains(&response.status) => {
            let decoder = AnthropicSseDecoder::new(anthropic_model(model), is_oauth, timestamp);
            AssistantEventReader::new(response.chunks, Box::new(decoder))
        }
        Ok(response) => {
            let body = drain_chunks(response.chunks);
            error_reader(model, timestamp, format_api_error(response.status, &body))
        }
        Err(error) => error_reader(model, timestamp, error.to_string()),
    }
}

/// Stream a response from the simple, level-based options, mirroring pi's
/// `streamSimple()` (`anthropic-messages.ts:786`). It maps `reasoning` onto the
/// adaptive-effort or budget-based thinking configuration and delegates to
/// [`stream`].
pub fn stream_simple<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    options: &SimpleStreamOptions,
    timestamp: i64,
) -> StreamResult {
    // pi asserts auth synchronously at the top of streamSimple; encoded here as
    // an error result to keep the function total.
    if let Err(message) = assert_request_auth(
        &model.provider,
        options.api_key.as_deref(),
        options.headers.as_ref(),
    ) {
        return error_result(model, timestamp, message);
    }

    let base = build_base_options(model, context, options);

    let Some(reasoning) = options.reasoning else {
        // No reasoning requested: thinking explicitly disabled.
        let opts = AnthropicOptions {
            thinking_enabled: Some(false),
            ..base
        };
        return stream(transport, model, context, &opts, timestamp);
    };

    let force_adaptive = model
        .compat
        .as_ref()
        .and_then(|c| c.force_adaptive_thinking)
        .unwrap_or(false);

    if force_adaptive {
        // Adaptive-thinking models: use an effort level.
        let effort = map_thinking_level_to_effort(model, Some(reasoning));
        let opts = AnthropicOptions {
            thinking_enabled: Some(true),
            effort: Some(effort),
            ..base
        };
        return stream(transport, model, context, &opts, timestamp);
    }

    // Older models: budget-based thinking.
    let adjusted = adjust_max_tokens_for_thinking(
        base.max_tokens,
        model.max_tokens,
        reasoning,
        options.thinking_budgets.as_ref(),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
    let thinking_budget = adjusted
        .thinking_budget
        .min(max_tokens.saturating_sub(1024));
    let opts = AnthropicOptions {
        max_tokens: Some(max_tokens),
        thinking_enabled: Some(true),
        thinking_budget_tokens: Some(thinking_budget),
        ..base
    };
    stream(transport, model, context, &opts, timestamp)
}
