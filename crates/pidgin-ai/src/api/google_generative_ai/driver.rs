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
//! injected [`HttpTransport`], frames the `?alt=sse` response body into the
//! `GenerateContentResponse` chunk sequence the SDK's `for await (chunk of
//! googleStream)` yields, and feeds those chunks into the already-ported decoder
//! ([`parse_stream`](super::parse_stream)), returning the eager [`StreamResult`].
//!
//! # Streaming model
//!
//! pi's `stream()` pushes events as the `@google/genai` SDK yields parsed chunks;
//! the SDK owns the HTTP request and the SSE framing. The Rust core produces the
//! whole event sequence eagerly from a fully-buffered response body — exactly the
//! shape the SDK's own tests exercise (a stubbed streaming `Response` whose SSE
//! body is parsed to chunks). [`sse_body_to_chunks`] reproduces the SDK's SSE
//! framing: each `data:` payload is one complete `GenerateContentResponse` JSON.
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

use serde_json::Value;

use crate::seams::http::HttpTransport;
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, StopReason, Usage, UsageCost,
};

use super::super::google_shared::{build_params, GoogleModel, GoogleRequestOptions};
use super::client::{assemble_request, serialize_body};
use super::{parse_stream, API};

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

/// Frame a buffered `?alt=sse` response body into the `GenerateContentResponse`
/// chunk sequence the SDK's `generateContentStream` yields. Each SSE event's
/// `data:` payload is one complete chunk JSON; unparseable or empty payloads (and
/// a stray `[DONE]` sentinel) are skipped.
fn sse_body_to_chunks(body: &str) -> Vec<Value> {
    let mut chunks = Vec::new();
    let mut data_lines: Vec<String> = Vec::new();

    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            flush_chunk(&mut data_lines, &mut chunks);
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let value = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(value.to_string());
        }
    }
    flush_chunk(&mut data_lines, &mut chunks);
    chunks
}

/// Parse the accumulated `data:` lines of one SSE event into a chunk, clearing
/// the buffer. A `[DONE]` sentinel or an unparseable payload is dropped.
fn flush_chunk(data_lines: &mut Vec<String>, chunks: &mut Vec<Value>) {
    if data_lines.is_empty() {
        return;
    }
    let joined = data_lines.join("\n");
    data_lines.clear();
    let trimmed = joined.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        chunks.push(value);
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
            let chunks = sse_body_to_chunks(&response.body);
            let outcome = parse_stream(&chunks, model, timestamp);
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
