// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `mistral-conversations.ts` `stream` / `streamSimple` request assembly. The
// pre-start error-shell construction (empty `AssistantMessage` + zeroed `Usage`)
// and the buffered `send` -> decode flow mirror the sibling Anthropic driver by
// design; the clone detector reads the shared boundary-type scaffolding as
// duplicative, but each is a distinct, load-bearing wire assembly kept verbatim.
//! The Mistral `chat.stream` driver, ported from pi-ai's
//! `packages/ai/src/api/mistral-conversations.ts` `stream` / `streamSimple` at
//! pinned commit `3da591ab`.
//!
//! pi builds a `Mistral` SDK client per request (`new Mistral({ apiKey,
//! serverURL })`) and lets the SDK put `mistral.chat.stream(payload,
//! requestOptions)` on the wire (`mistral-conversations.ts:65-78`). The SDK
//! transforms the response SSE into the `CompletionChunk` objects pi's
//! `consumeChatStream` reads (`event.data`). This seam-targeted port reproduces
//! that boundary: it assembles the [`HttpRequest`] the injected
//! [`HttpTransport`](crate::seams::http::HttpTransport) performs, then feeds the
//! SSE response body through the already-ported [`MistralSseDecoder`] -- the
//! buffered [`stream`] over the whole body, the incremental [`stream_streaming`]
//! one arriving chunk at a time -- so both share one decode path.
//!
//! # What this port owns
//!
//! - the `POST {baseUrl}/v1/chat/completions` request URL + method the Mistral
//!   SDK derives from `serverURL` for the streaming chat endpoint;
//! - the SDK-equivalent default headers pi delegates to `@mistralai/mistralai`:
//!   `authorization: Bearer <apiKey>` (the SDK derives it from `apiKey`) and
//!   `content-type: application/json`. Both are supplied only-when-absent so a
//!   caller header (already merged from `model.headers` / `options.headers` and
//!   the `x-affinity` prompt-cache header by [`build_request_headers`]) wins;
//! - the pre-request auth assertion pi encodes as `if (!apiKey) throw` and the
//!   non-2xx error surfacing pi's `formatMistralError` shapes as
//!   `` `Mistral API error (${status}): ${body}` ``.
//!
//! # Streaming model & error surfacing
//!
//! Like the Anthropic port, [`stream`] is the buffered analogue of pi's async
//! `stream()`: it produces the whole event sequence eagerly from a fully-buffered
//! response body, while [`stream_streaming`] delivers the same events
//! incrementally through the shared [`AssistantEventReader`] as chunks arrive. A
//! missing credential, a non-2xx create, or a transport error yield a single
//! pre-`start` `error` (pi's `catch`), while a decoded `error`/`aborted` stop is
//! surfaced by the ported decoder itself.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde_json::Value;

use crate::seams::http::{HttpRequest, HttpTransport};
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, StopReason, Usage, UsageCost,
};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

use super::{
    build_chat_payload, build_request_headers, parse_sse_stream, resolve_simple_options,
    MistralModel, MistralOptions, MistralSseDecoder, SimpleMistralOptions,
};

/// pi's `MAX_MISTRAL_ERROR_BODY_CHARS` (`mistral-conversations.ts:32`).
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;

/// The default request URL the Mistral SDK derives from `serverURL`: the
/// streaming chat-completions endpoint under the model's base URL
/// (`mistral.chat.stream` -> `POST /v1/chat/completions`).
fn request_url(base_url: &str) -> String {
    format!("{}/v1/chat/completions", base_url.trim_end_matches('/'))
}

/// pi's `if (!apiKey) throw new Error(...)` guard at the top of `stream` /
/// `streamSimple` (`mistral-conversations.ts:59-62` / `115-118`). An empty
/// credential is falsy in JS, so it is treated as missing here too.
fn assert_request_auth(provider: &str, api_key: Option<&str>) -> Result<(), String> {
    if api_key.map(|key| !key.is_empty()).unwrap_or(false) {
        return Ok(());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// Supply the SDK-equivalent default headers pi's `new Mistral(...)` delegates to
/// `@mistralai/mistralai`: `content-type: application/json`. Inserted only when
/// absent so a caller-supplied header (already merged by
/// [`build_request_headers`]) keeps precedence.
fn apply_sdk_default_headers(headers: &mut BTreeMap<String, String>) {
    headers
        .entry("content-type".to_string())
        .or_insert_with(|| "application/json".to_string());
}

/// Set `authorization: Bearer <apiKey>` from the credential unless a caller
/// already supplied an `authorization` header. The Mistral SDK derives this from
/// its `apiKey` security setting; user `requestOptions.headers` override it.
fn set_bearer_auth(headers: &mut BTreeMap<String, String>, api_key: Option<&str>) {
    if let Some(api_key) = api_key {
        headers
            .entry("authorization".to_string())
            .or_insert_with(|| format!("Bearer {api_key}"));
    }
}

/// Serialize the request body; only defined for a `serde_json::Value` so a
/// serialization failure is impossible.
fn serialize_body(body: &Value) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string())
}

/// Assemble the [`HttpRequest`] for a streaming Mistral `chat.stream` call. `body`
/// is the serialized `ChatCompletionStreamRequest` JSON (from
/// [`build_chat_payload`], which already sets `stream: true`).
fn assemble_request(
    model: &MistralModel,
    body: String,
    api_key: Option<&str>,
    options: &MistralOptions,
) -> HttpRequest {
    // build_request_headers merges model.headers, options.headers, and the
    // x-affinity prompt-cache header (pi's `buildRequestOptions`).
    let mut headers = build_request_headers(model, options);
    apply_sdk_default_headers(&mut headers);
    set_bearer_auth(&mut headers, api_key);

    HttpRequest {
        method: "POST".to_string(),
        url: request_url(&model.base_url),
        headers,
        body: Some(body),
    }
}

/// Format a non-2xx create response into the terminal error message, mirroring
/// pi's `formatMistralError` (`mistral-conversations.ts:185-197`): `` `Mistral
/// API error (${statusCode}): ${truncated body}` `` when a body is present,
/// falling back to the bare `(status)` shell for an empty body.
fn format_api_error(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return format!("Mistral API error ({status})");
    }
    format!(
        "Mistral API error ({status}): {}",
        truncate_error_text(trimmed, MAX_MISTRAL_ERROR_BODY_CHARS)
    )
}

/// pi's `truncateErrorText` (`mistral-conversations.ts:199-202`).
fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!(
        "{truncated}... [truncated {} chars]",
        char_count - max_chars
    )
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

/// The empty assistant output shell carrying pi's pre-`start` `catch` error
/// (`stopReason: "error"` + `errorMessage`), shared by the buffered
/// [`error_result`] and the streaming [`error_reader`].
fn error_output(model: &MistralModel, timestamp: i64, message: String) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(message),
        timestamp,
    }
}

/// A single-`error`-event result for a failure before the stream's `start` event
/// (missing auth, a non-2xx create, a transport error), matching pi's pre-`start`
/// `catch` handler (`mistral-conversations.ts:92-101`).
fn error_result(model: &MistralModel, timestamp: i64, message: String) -> StreamResult {
    let output = error_output(model, timestamp, message);
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: output.clone(),
        }],
        message: output,
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
    model: &MistralModel,
    timestamp: i64,
    message: String,
) -> AssistantEventReader<'a> {
    let output = error_output(model, timestamp, message);
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

/// Stream a response for `model` over the injected `transport`, mirroring pi's
/// `stream()` request assembly and `chat.stream` handling. `api_key` is the
/// resolved credential pi reads from `options.apiKey` (threaded explicitly here,
/// as the ported [`MistralOptions`] is a driver-local option struct that does not
/// carry the credential). `timestamp` is the message timestamp pi sets via
/// `Date.now()`.
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &MistralModel,
    context: &Context,
    options: &MistralOptions,
    api_key: Option<&str>,
    timestamp: i64,
) -> StreamResult {
    // pi asserts the credential before constructing the client; a failure throws
    // and is caught as a pre-`start` error event.
    if let Err(message) = assert_request_auth(&model.provider, api_key) {
        return error_result(model, timestamp, message);
    }

    // build_chat_payload already applies transform_messages + sets stream: true.
    let payload = build_chat_payload(model, context, options);
    let request = assemble_request(model, serialize_body(&payload), api_key, options);

    match transport.send(&request) {
        Ok(response) if response.is_ok() => {
            // Feed the whole SSE body through the SAME decoder the incremental
            // path uses, so the buffered events + message are byte-identical.
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

/// Stream a response for `model` over the injected `transport`, delivering events
/// incrementally through the shared [`AssistantEventReader`].
///
/// This mirrors [`stream`]'s request assembly and error surfacing but performs the
/// request via [`HttpTransport::send_streaming`], so the returned reader pulls one
/// chunk at a time and decodes it through the SAME [`MistralSseDecoder`] the
/// buffered path uses -- one source of truth for the event sequence. A pre-stream
/// failure (missing auth, a non-2xx create, a transport error) yields a
/// single-`error` reader, mirroring pi's catch handler exactly as [`stream`] does.
pub fn stream_streaming<'a, T: HttpTransport + ?Sized>(
    transport: &'a T,
    model: &MistralModel,
    context: &Context,
    options: &MistralOptions,
    api_key: Option<&str>,
    timestamp: i64,
) -> AssistantEventReader<'a> {
    // pi asserts the credential before constructing the client; a failure throws
    // and is caught as a pre-`start` error event.
    if let Err(message) = assert_request_auth(&model.provider, api_key) {
        return error_reader(model, timestamp, message);
    }

    // build_chat_payload already applies transform_messages + sets stream: true.
    let payload = build_chat_payload(model, context, options);
    let request = assemble_request(model, serialize_body(&payload), api_key, options);

    // Status + headers arrive up front, so the error-vs-parse decision is made
    // before the body streams -- exactly as the buffered path decides on
    // `response.is_ok()`.
    match transport.send_streaming(&request) {
        Ok(response) if (200..300).contains(&response.status) => {
            let decoder = MistralSseDecoder::new(model.clone(), timestamp);
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
/// `streamSimple()` (`mistral-conversations.ts:110`). It maps the requested
/// reasoning level onto the prompt-mode / reasoning-effort configuration via the
/// ported [`resolve_simple_options`] and delegates to [`stream`].
pub fn stream_simple<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &MistralModel,
    context: &Context,
    options: &SimpleMistralOptions,
    api_key: Option<&str>,
    timestamp: i64,
) -> StreamResult {
    // pi asserts the credential synchronously at the top of streamSimple.
    if let Err(message) = assert_request_auth(&model.provider, api_key) {
        return error_result(model, timestamp, message);
    }

    let resolved = resolve_simple_options(model, options);
    stream(transport, model, context, &resolved, api_key, timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::seams::http::ScriptedTransport;
    use crate::types::{ContentBlock, Message, ModelCost, UserContent, UserMessage, UserRole};
    use serde_json::json;

    fn test_cost() -> ModelCost {
        ModelCost {
            input: 1.0,
            output: 5.0,
            cache_read: 0.1,
            cache_write: 1.25,
            tiers: None,
        }
    }

    fn model(base_url: &str) -> MistralModel {
        MistralModel {
            id: "mistral-large-latest".to_string(),
            api: "mistral-conversations".to_string(),
            provider: "mistral".to_string(),
            cost: test_cost(),
            reasoning: false,
            input: vec![crate::types::Modality::Text],
            thinking_level_map: None,
            base_url: base_url.to_string(),
            max_tokens: 8192,
            headers: None,
        }
    }

    fn user_context() -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Hi".to_string()),
                timestamp: 0,
            })],
            tools: None,
        }
    }

    /// A scripted `chat.stream` SSE body yielding a single `Hello world` text
    /// block, in the SDK-shaped (camelCase) `CompletionChunk` frames the ported
    /// decoder reads (mirroring `mistral/tests.rs`).
    fn hello_sse_body() -> String {
        [
            "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finishReason\":\"stop\"}],\"usage\":{\"promptTokens\":10,\"completionTokens\":5,\"totalTokens\":15}}\n\n",
            "data: [DONE]\n\n",
        ]
        .concat()
    }

    #[test]
    fn stream_decodes_hello_and_builds_request() {
        let transport = ScriptedTransport::new();
        transport.push_ok(hello_sse_body());

        let m = model("https://api.mistral.test");
        let result = stream(
            &transport,
            &m,
            &user_context(),
            &MistralOptions::default(),
            Some("sk-mistral-key"),
            0,
        );

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello world".to_string(),
                text_signature: None,
            }]
        );

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].url,
            "https://api.mistral.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-mistral-key")
        );
        assert_eq!(
            requests[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        // The serialized body carries stream:true and the model id.
        let body: Value = serde_json::from_str(requests[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["model"], json!("mistral-large-latest"));
    }

    // Incremental `stream_streaming` over the one-chunk ScriptedTransport (default
    // `send_streaming`) yields the SAME events and final message as the buffered
    // `stream`, and assembles the same threaded request -- proving the buffered
    // path stays byte-identical while both share one decoder.
    #[test]
    fn stream_streaming_matches_buffered_over_scripted() {
        let m = model("https://api.mistral.test");
        let ctx = user_context();
        let opts = MistralOptions::default();

        let buffered_transport = ScriptedTransport::new();
        buffered_transport.push_ok(hello_sse_body());
        let buffered = stream(
            &buffered_transport,
            &m,
            &ctx,
            &opts,
            Some("sk-mistral-key"),
            0,
        );

        let streaming_transport = ScriptedTransport::new();
        streaming_transport.push_ok(hello_sse_body());
        let mut reader = stream_streaming(
            &streaming_transport,
            &m,
            &ctx,
            &opts,
            Some("sk-mistral-key"),
            0,
        );
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

        assert_eq!(events, buffered.events);
        assert_eq!(
            reader.result().and_then(|r| r.as_ref().ok()),
            Some(&buffered.message)
        );

        let requests = streaming_transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://api.mistral.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-mistral-key")
        );
    }

    // A non-2xx create over the streaming path surfaces the API's error body as a
    // single terminal `error` reader (no preceding `start`), mirroring the
    // buffered `error_result` shape.
    #[test]
    fn stream_streaming_non_2xx_yields_single_error() {
        let transport = ScriptedTransport::new();
        transport.push_response(Ok(crate::seams::http::HttpResponse {
            status: 429,
            headers: BTreeMap::new(),
            body: "{\"message\":\"rate limited\"}".to_string(),
        }));

        let m = model("https://api.mistral.test");
        let mut reader = stream_streaming(
            &transport,
            &m,
            &user_context(),
            &MistralOptions::default(),
            Some("sk-mistral-key"),
            0,
        );
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));
        let result = reader.result().expect("finished");
        let message = result.as_ref().expect_err("error terminal");
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(
            message.error_message.as_deref(),
            Some("Mistral API error (429): {\"message\":\"rate limited\"}")
        );
    }

    #[test]
    fn missing_api_key_is_a_clean_error_without_request() {
        let transport = ScriptedTransport::new();
        let m = model("https://api.mistral.test");
        let result = stream(
            &transport,
            &m,
            &user_context(),
            &MistralOptions::default(),
            None,
            0,
        );

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("No API key for provider: mistral")
        );
        assert_eq!(result.events.len(), 1);
        assert!(matches!(
            result.events[0],
            AssistantMessageEvent::Error { .. }
        ));
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn non_2xx_surfaces_error_body() {
        let transport = ScriptedTransport::new();
        transport.push_response(Ok(crate::seams::http::HttpResponse {
            status: 429,
            headers: BTreeMap::new(),
            body: "{\"message\":\"rate limited\"}".to_string(),
        }));

        let m = model("https://api.mistral.test");
        let result = stream(
            &transport,
            &m,
            &user_context(),
            &MistralOptions::default(),
            Some("sk-mistral-key"),
            0,
        );

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Mistral API error (429): {\"message\":\"rate limited\"}")
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn stream_simple_threads_reasoning_and_streams() {
        let transport = ScriptedTransport::new();
        transport.push_ok(hello_sse_body());

        let m = model("https://api.mistral.test");
        let result = stream_simple(
            &transport,
            &m,
            &user_context(),
            &SimpleMistralOptions::default(),
            Some("sk-mistral-key"),
            0,
        );

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(
            transport.requests()[0].url,
            "https://api.mistral.test/v1/chat/completions"
        );
    }
}
