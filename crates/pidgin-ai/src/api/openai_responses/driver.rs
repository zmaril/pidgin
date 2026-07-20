// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `openai-responses.ts` `createClient` + `stream` request assembly and pre-start
// error surfacing. Its shape deliberately mirrors the openai-completions and
// anthropic `driver.rs` pairs (the same empty-`AssistantMessage` error shell, the
// same only-when-absent Bearer/content-type SDK-default seam, the same
// `format_api_error` body pass-through, the same streaming-native
// `stream_streaming`); the clone detector reads that mirrored scaffolding as
// duplication by design.
//! OpenAI **Responses API** request assembly + stream driver, ported from pi-ai's
//! `packages/ai/src/api/openai-responses.ts` `createClient` / `stream` at pinned
//! commit `3da591ab`.
//!
//! pi builds an `OpenAI` SDK client per request (`new OpenAI({ apiKey, baseURL,
//! defaultHeaders })`, `openai-responses.ts:225`) and lets the SDK put the
//! `POST {baseURL}/responses` request (`client.responses.create`,
//! `openai-responses.ts:139`) on the wire; SSE is selected by the body's
//! `stream: true` (already set by [`build_params`]). This seam-targeted port
//! reproduces exactly that: given the model, context, and options, it assembles
//! the [`HttpRequest`] the injected [`HttpTransport`] is handed (URL + headers +
//! serialized body from [`build_params`]) and decodes the SSE reply through the
//! already-ported [`OpenAIResponsesSseDecoder`].
//!
//! # SDK-injected headers (the #184 lesson)
//!
//! pi's `createClient` writes only `model.headers`, the session-affinity headers,
//! and `optionsHeaders`; the official OpenAI TS SDK injects the auth + content
//! type before the request hits the wire (`new OpenAI({ apiKey })` derives
//! `authorization: Bearer <apiKey>`, and the JSON-body POST carries `content-type:
//! application/json`). The raw transport has no such SDK layer, so both are
//! supplied here at low precedence (only-when-absent) so a caller /
//! `optionsHeaders` value still wins — matching the SDK, whose built-in defaults
//! sit below `defaultHeaders`. The `user-agent` and `x-stainless-*` telemetry
//! headers the SDK also adds are cosmetic and left out, exactly as the
//! openai-completions port leaves them.
//!
//! # `getClientApiKey` fallback
//!
//! pi's `getClientApiKey` (`openai-responses.ts:41`): a present `apiKey` is used
//! verbatim; otherwise, when a caller header `authorization` or
//! `cf-aig-authorization` is already set, the apiKey becomes the sentinel
//! `"unused"` (so no real Bearer is minted over a caller-supplied credential);
//! otherwise it throws `No API key for provider: <provider>`, which pi catches as
//! a pre-start `error` event.
//!
//! # Error surfacing
//!
//! pi encodes post-start failures as a terminal `error` event; a failure before
//! the stream starts (missing auth, a non-2xx create, a transport error) throws
//! and is caught by the same handler, which pushes a single `error` event with no
//! preceding `start`. Both drivers reproduce that: [`stream`] yields an
//! error-only [`StreamResult`] and [`stream_streaming`] a single-`error` reader,
//! and a non-2xx carries the API's diagnostic through [`format_api_error`].

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde_json::Value;

use crate::api::openai_responses_shared::{
    parse_responses_sse_stream, OpenAIResponsesSseDecoder, ResponsesStreamOptions,
};
use crate::seams::http::{HttpRequest, HttpTransport};
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention, Context, Model,
    OpenAIResponsesCompat, StopReason,
};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

use super::{
    build_params, get_compat, resolve_cache_retention, session_affinity_headers,
    OpenAIResponsesModel, OpenAIResponsesOptions, ResolvedResponsesCompat,
};

/// pi's `hasHeader` (`openai-responses.ts:32`): a case-insensitive lookup for a
/// header whose value is non-empty after trimming.
fn has_header(headers: Option<&BTreeMap<String, String>>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_ascii_lowercase();
    headers
        .iter()
        .any(|(key, value)| key.to_ascii_lowercase() == expected && !value.trim().is_empty())
}

/// pi's `getClientApiKey` (`openai-responses.ts:41`): resolve the credential the
/// SDK client is constructed with. A present `apiKey` is used verbatim; otherwise
/// a caller-supplied `authorization` / `cf-aig-authorization` header yields the
/// `"unused"` sentinel; otherwise this is the pre-start `No API key` failure.
pub fn client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&BTreeMap<String, String>>,
) -> Result<String, String> {
    if let Some(key) = api_key {
        // pi's `if (apiKey)` is a truthiness check; an empty string is falsy and
        // falls through to the header fallback.
        if !key.is_empty() {
            return Ok(key.to_string());
        }
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// The request URL the OpenAI SDK derives from `baseURL`: the Responses endpoint
/// under the model's base URL (`client.responses.create`,
/// `openai-responses.ts:139`). The catalog base URLs already carry the API
/// version segment (e.g. `.../v1`), so only `/responses` is appended.
fn request_url(base_url: &str) -> String {
    format!("{}/responses", base_url.trim_end_matches('/'))
}

/// pi's `Object.assign` header merge in `createClient`: later sources override
/// earlier ones. Keys are lowercased per the transport seam's convention.
fn merge_into(target: &mut BTreeMap<String, String>, source: &BTreeMap<String, String>) {
    for (key, value) in source {
        target.insert(key.to_ascii_lowercase(), value.clone());
    }
}

/// The SDK derives `authorization: Bearer <apiKey>` from `new OpenAI({ apiKey })`;
/// a caller-supplied `authorization` header (already merged from `model.headers` /
/// `optionsHeaders`) wins, so this only fills the gap.
fn set_bearer_auth(headers: &mut BTreeMap<String, String>, api_key: &str) {
    headers
        .entry("authorization".to_string())
        .or_insert_with(|| format!("Bearer {api_key}"));
}

/// Supply the SDK-equivalent `content-type: application/json` pi's `createClient`
/// leaves to the OpenAI TS SDK (the JSON-body POST default). Inserted only when
/// absent, so a caller-supplied `content-type` keeps precedence — matching the
/// SDK, whose built-in defaults sit below `defaultHeaders`.
fn apply_sdk_default_headers(headers: &mut BTreeMap<String, String>) {
    headers
        .entry("content-type".to_string())
        .or_insert_with(|| "application/json".to_string());
}

/// Serialize the request body JSON; only defined for a `serde_json::Value`, whose
/// serialization cannot fail.
fn serialize_body(body: &Value) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string())
}

/// Assemble the [`HttpRequest`] for a streaming OpenAI Responses call, reproducing
/// pi's `createClient` header composition (`openai-responses.ts:191`). `body` is
/// the serialized `ResponseCreateParamsStreaming` JSON (from [`build_params`]);
/// `api_key` is the already-resolved [`client_api_key`] credential.
///
/// `session_id` is the caller's session id already gated on cache retention (pi's
/// `cacheSessionId = cacheRetention === "none" ? undefined : sessionId`); the
/// affinity block shapes it by `compat.sessionAffinityFormat`.
#[allow(clippy::too_many_arguments)]
pub fn assemble_request(
    model: &OpenAIResponsesModel,
    compat: &ResolvedResponsesCompat,
    body: String,
    api_key: &str,
    model_headers: Option<&BTreeMap<String, String>>,
    options_headers: Option<&BTreeMap<String, String>>,
    session_id: Option<&str>,
) -> HttpRequest {
    // pi: `const headers = { ...model.headers }`.
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    if let Some(model_headers) = model_headers {
        merge_into(&mut headers, model_headers);
    }
    // github-copilot dynamic vision headers (`buildCopilotDynamicHeaders`) are a
    // sibling concern and are not assembled here.
    if let Some(session_id) = session_id {
        merge_into(
            &mut headers,
            &session_affinity_headers(compat, Some(session_id)),
        );
    }
    // pi merges optionsHeaders last so they override defaults.
    if let Some(options_headers) = options_headers {
        merge_into(&mut headers, options_headers);
    }

    set_bearer_auth(&mut headers, api_key);
    apply_sdk_default_headers(&mut headers);

    HttpRequest {
        method: "POST".to_string(),
        url: request_url(&model.base_url),
        headers,
        body: Some(body),
    }
}

/// Build the lean [`OpenAIResponsesModel`] the request shaper / SSE decoder read
/// from the full boundary model, mirroring the openai-completions driver's
/// `lean_model`.
fn lean_model(model: &Model<OpenAIResponsesCompat>) -> OpenAIResponsesModel {
    OpenAIResponsesModel {
        id: model.id.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        cost: model.cost.clone(),
        reasoning: model.reasoning,
        thinking_level_map: model.thinking_level_map.clone(),
        input: model.input.clone(),
        headers: model.headers.clone(),
        compat: model.compat.clone(),
    }
}

/// The empty assistant output shell for a pre-start failure, mirroring the
/// openai-completions driver's `empty_output`.
fn empty_output(model: &OpenAIResponsesModel, timestamp: i64) -> AssistantMessage {
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
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp,
    }
}

/// A zeroed [`Usage`], the empty-shell usage a pre-start failure carries.
fn zero_usage() -> crate::types::Usage {
    crate::types::Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: crate::types::UsageCost::default(),
    }
}

/// A single-`error`-event result for a failure before the stream's `start` event,
/// matching pi's catch handler (`openai-responses.ts:158`).
fn error_result(model: &OpenAIResponsesModel, timestamp: i64, message: String) -> StreamResult {
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

/// Format a non-2xx create response into the terminal error message, mirroring the
/// OpenAI SDK's `APIError` shape (`` `${status} ${message}` ``): the message is
/// pulled from the JSON error body's `error.message`, falling back to the raw body
/// text, then to a no-body marker. Identical in shape to the sibling drivers'
/// `format_api_error`.
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

/// The resolved request inputs shared by [`stream`] and [`stream_streaming`]: the
/// lean model, the resolved credential, the compat, the serialized body, and the
/// cache-gated session id — pi's `createClient` inputs computed once per driver.
struct PreparedRequest {
    lean: OpenAIResponsesModel,
    request: HttpRequest,
    stream_options: ResponsesStreamOptions,
}

/// The pre-start failure both drivers surface: the lean model (for the error
/// output shell) plus pi's caught message. Boxed so the shared
/// [`prepare_request`] `Result` keeps a small `Err` variant.
struct PreparedError {
    lean: OpenAIResponsesModel,
    message: String,
}

/// Resolve auth, compat, and body, then assemble the [`HttpRequest`], or return
/// the pre-start error message pi's `getClientApiKey` throws. Shared by both
/// drivers so their request assembly is byte-identical.
fn prepare_request(
    model: &Model<OpenAIResponsesCompat>,
    context: &Context,
    options: &OpenAIResponsesOptions,
) -> Result<PreparedRequest, Box<PreparedError>> {
    let lean = lean_model(model);
    let api_key = options.api_key.as_deref();

    // pi's getClientApiKey throws before the client is built; caught as an error.
    let client_key = match client_api_key(&lean.provider, api_key, options.headers.as_ref()) {
        Ok(key) => key,
        Err(message) => return Err(Box::new(PreparedError { lean, message })),
    };

    let compat = get_compat(&lean);

    // pi: cacheSessionId = cacheRetention === "none" ? undefined : options.sessionId.
    let cache_retention = resolve_cache_retention(options.cache_retention);
    let session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.session_id.as_deref()
    };

    // build_params already sets `stream: true`.
    let body = build_params(&lean, context, options);
    let request = assemble_request(
        &lean,
        &compat,
        serialize_body(&body),
        &client_key,
        model.headers.as_ref(),
        options.headers.as_ref(),
        session_id,
    );

    let stream_options = ResponsesStreamOptions {
        service_tier: options.service_tier.clone(),
    };

    Ok(PreparedRequest {
        lean,
        request,
        stream_options,
    })
}

/// Stream a response for `model` over the injected `transport` (buffered), mirroring
/// pi's `stream()` request assembly and SSE handling. `timestamp` is the message
/// timestamp pi sets via `Date.now()` (threaded here for determinism, as the SSE
/// decoder already is). Generic over the transport so a [`ScriptedTransport`] can
/// be injected in tests.
///
/// [`ScriptedTransport`]: crate::seams::http::ScriptedTransport
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &Model<OpenAIResponsesCompat>,
    context: &Context,
    options: &OpenAIResponsesOptions,
    timestamp: i64,
) -> StreamResult {
    let prepared = match prepare_request(model, context, options) {
        Ok(prepared) => prepared,
        Err(failure) => return error_result(&failure.lean, timestamp, failure.message),
    };
    let PreparedRequest {
        lean,
        request,
        stream_options,
    } = prepared;

    match transport.send(&request) {
        Ok(response) if response.is_ok() => {
            let outcome =
                parse_responses_sse_stream(&response.body, &lean, &stream_options, timestamp);
            StreamResult {
                events: outcome.events,
                message: outcome.message,
            }
        }
        Ok(response) => error_result(
            &lean,
            timestamp,
            format_api_error(response.status, &response.body),
        ),
        Err(error) => error_result(&lean, timestamp, error.to_string()),
    }
}

/// A decoder that yields nothing per frame and emits a single terminal `error`
/// event at `finish` — the streaming analogue of [`error_result`] for a failure
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
    model: &OpenAIResponsesModel,
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
/// first read error — used only for a non-2xx error body, whose diagnostic is
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
/// This mirrors [`stream`]'s request assembly and error surfacing but performs the
/// request via [`HttpTransport::send_streaming`], so the returned reader pulls one
/// chunk at a time and decodes it through the SAME [`OpenAIResponsesSseDecoder`]
/// the buffered path uses — one source of truth for the event sequence. A
/// pre-stream failure (missing auth, a non-2xx create, a transport error) yields a
/// single-`error` reader, mirroring pi's catch handler exactly as [`stream`] does.
pub fn stream_streaming<'a, T: HttpTransport + ?Sized>(
    transport: &'a T,
    model: &Model<OpenAIResponsesCompat>,
    context: &Context,
    options: &OpenAIResponsesOptions,
    timestamp: i64,
) -> AssistantEventReader<'a> {
    let prepared = match prepare_request(model, context, options) {
        Ok(prepared) => prepared,
        Err(failure) => return error_reader(&failure.lean, timestamp, failure.message),
    };
    let PreparedRequest {
        lean,
        request,
        stream_options,
    } = prepared;

    // Status + headers arrive up front, so the error-vs-parse decision is made
    // before the body streams — exactly as the buffered path decides on
    // `response.is_ok()`.
    match transport.send_streaming(&request) {
        Ok(response) if (200..300).contains(&response.status) => {
            let decoder = OpenAIResponsesSseDecoder::new(lean, stream_options, timestamp);
            AssistantEventReader::new(response.chunks, Box::new(decoder))
        }
        Ok(response) => {
            let body = drain_chunks(response.chunks);
            error_reader(&lean, timestamp, format_api_error(response.status, &body))
        }
        Err(error) => error_reader(&lean, timestamp, error.to_string()),
    }
}
