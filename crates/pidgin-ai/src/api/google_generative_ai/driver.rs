// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the transport send / non-2xx
// error-body surfacing mirror the Anthropic driver by design; the clone detector
// reads the shared boundary-type construction and the send/`format_api_error`
// ladder as duplicative. Each is a distinct, load-bearing transcription kept
// parallel to the anthropic-messages driver on purpose.
//! The Google Generative AI stream driver, the transport-driving half of pi-ai's
//! `google-generative-ai.ts` `stream` at pinned commit `3da591ab`.
//!
//! [`stream`] is the buffered, seam-targeted analogue of pi's async `stream()`:
//! it asserts the credential, builds the request body
//! ([`build_params`](super::super::google_shared::build_params)), assembles the
//! [`HttpRequest`](crate::seams::http::HttpRequest)
//! ([`assemble_request`](super::client::assemble_request)), performs it over the
//! injected [`HttpTransport`], and feeds the `?alt=sse` response body through the
//! already-ported decoder ([`parse_sse_stream`](super::parse_sse_stream)),
//! returning the eager [`StreamResult`].
//!
//! [`stream_streaming`] is the incremental analogue: it performs the request via
//! [`HttpTransport::send_streaming`] and returns an
//! [`AssistantEventReader`](crate::utils::sse::AssistantEventReader) that pulls
//! one chunk at a time, decoding through the SAME
//! [`GoogleGenerativeAiSseDecoder`](super::GoogleGenerativeAiSseDecoder) the
//! buffered path uses — one source of truth for the event sequence.
//!
//! # Streaming model
//!
//! pi's `stream()` pushes events as the `@google/genai` SDK yields parsed chunks;
//! the SDK owns the HTTP request and the SSE framing. Both entry points here run
//! the shared SSE decoder: [`stream`] frames a fully-buffered body in one shot,
//! [`stream_streaming`] pulls it chunk-by-chunk off the wire. Each `data:`
//! payload is one complete `GenerateContentResponse` JSON.
//!
//! # Error surfacing
//!
//! pi encodes a failure as a terminal `error` event; a failure before the stream
//! starts (missing credential, a pre-aborted `build_params`, a non-2xx create, a
//! transport error) is caught by the same handler and pushed as a single `error`
//! event with no preceding `start`. The buffered driver reproduces that: a
//! missing API key yields `No API key for provider: <provider>`
//! (`google-generative-ai.ts:78-80`); `build_params` returning `Err` (a
//! pre-aborted signal) surfaces its message; a non-2xx create carries the API's
//! diagnostic through [`format_api_error`] rather than discarding the body.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde_json::Value;

use crate::seams::http::HttpTransport;
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, StopReason, Usage, UsageCost,
};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

use super::super::google_shared::{build_params, GoogleModel, GoogleRequestOptions};
use super::client::{assemble_request, serialize_body};
use super::{parse_sse_stream, GoogleGenerativeAiSseDecoder, API};

/// The empty assistant output shell pi seeds before streaming
/// (`google-generative-ai.ts:58-74`).
fn empty_output(model: &GoogleModel, timestamp: i64) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: API.to_string(),
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
/// occurs before the stream's `start` event (`google-generative-ai.ts:267-277`).
fn error_result(model: &GoogleModel, timestamp: i64, message: String) -> StreamResult {
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

/// Format a non-2xx create response into the terminal error message. pi surfaces
/// a create failure as `formatProviderError(normalizeProviderError(error))`; the
/// Google API error body is `{ "error": { "code", "message", "status" } }`, so
/// this extracts `error.message` and prefixes the status, falling back to the raw
/// body then a no-body marker — so callers see the API's diagnostic instead of a
/// bare status.
fn format_api_error(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    let detail = serde_json::from_str::<Value>(trimmed)
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
/// `stream()` request assembly and chunk handling. `api_key` is the resolved
/// credential (pi's `options.apiKey`); `options_headers` are the per-request
/// header overrides; `request_options` carries the request-shaping subset used by
/// [`build_params`]. `timestamp` is the message timestamp pi sets via
/// `Date.now()` (threaded here for determinism, as the decoder already is).
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &GoogleModel,
    context: &Context,
    api_key: Option<&str>,
    options_headers: &BTreeMap<String, String>,
    request_options: &GoogleRequestOptions,
    timestamp: i64,
) -> StreamResult {
    // pi throws `No API key for provider: <provider>` before constructing the
    // client (`google-generative-ai.ts:78-80`); a failure is caught as an error
    // event.
    if api_key.is_none() {
        return error_result(
            model,
            timestamp,
            format!("No API key for provider: {}", model.provider),
        );
    }

    // build_params returns Err for a pre-aborted signal (pi's synchronous throw
    // in buildParams); surface it as a pre-start error event.
    let body = match build_params(model, context, request_options, timestamp) {
        Ok(body) => body,
        Err(message) => return error_result(model, timestamp, message),
    };

    let request = assemble_request(model, serialize_body(&body), api_key, options_headers);

    match transport.send(&request) {
        Ok(response) if response.is_ok() => {
            // Feed the whole `?alt=sse` body through the SAME
            // `GoogleGenerativeAiSseDecoder` the incremental path uses, so the
            // buffered events + terminal message are byte-identical.
            let outcome = parse_sse_stream(&response.body, model, timestamp);
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
/// that occurs before the SSE stream starts (missing credential, a pre-aborted
/// `build_params`, a non-2xx create, a transport error). It carries pi's caught
/// `error` message on an empty output shell so [`stream_streaming`] can return an
/// [`AssistantEventReader`] on every path (no preceding `start`, matching pi's
/// pre-stream catch handler).
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
    model: &GoogleModel,
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
/// [`GoogleGenerativeAiSseDecoder`] the buffered path uses -- one source of truth
/// for the event sequence. A pre-stream failure (missing credential, a pre-aborted
/// `build_params`, a non-2xx create, a transport error) yields a single-`error`
/// reader, mirroring pi's catch handler exactly as [`stream`] does. `timestamp`
/// is threaded through [`build_params`] and the decoder for determinism.
pub fn stream_streaming<'a, T: HttpTransport + ?Sized>(
    transport: &'a T,
    model: &GoogleModel,
    context: &Context,
    api_key: Option<&str>,
    options_headers: &BTreeMap<String, String>,
    request_options: &GoogleRequestOptions,
    timestamp: i64,
) -> AssistantEventReader<'a> {
    // pi throws `No API key for provider: <provider>` before constructing the
    // client (`google-generative-ai.ts:78-80`); a failure is caught as an error
    // event.
    if api_key.is_none() {
        return error_reader(
            model,
            timestamp,
            format!("No API key for provider: {}", model.provider),
        );
    }

    // build_params returns Err for a pre-aborted signal (pi's synchronous throw
    // in buildParams); surface it as a pre-start error event.
    let body = match build_params(model, context, request_options, timestamp) {
        Ok(body) => body,
        Err(message) => return error_reader(model, timestamp, message),
    };

    let request = assemble_request(model, serialize_body(&body), api_key, options_headers);

    // Status + headers arrive up front, so the error-vs-parse decision is made
    // before the body streams -- exactly as the buffered path decides on
    // `response.is_ok()`.
    match transport.send_streaming(&request) {
        Ok(response) if (200..300).contains(&response.status) => {
            let decoder = GoogleGenerativeAiSseDecoder::new(model.clone(), timestamp);
            AssistantEventReader::new(response.chunks, Box::new(decoder))
        }
        Ok(response) => {
            let body = drain_chunks(response.chunks);
            error_reader(model, timestamp, format_api_error(response.status, &body))
        }
        Err(error) => error_reader(model, timestamp, error.to_string()),
    }
}
