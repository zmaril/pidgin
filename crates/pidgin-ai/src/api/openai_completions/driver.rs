// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `openai-completions.ts` `createClient` + `stream` request assembly and
// pre-start error surfacing. Its shape deliberately mirrors the anthropic
// `client.rs`/`driver.rs` pair (the same empty-`AssistantMessage` error shell,
// the same only-when-absent Bearer/content-type SDK-default seam, the same
// `format_api_error` body pass-through); the clone detector reads that mirrored
// scaffolding as duplication by design.
//! OpenAI Chat Completions request assembly + stream driver, ported from pi-ai's
//! `packages/ai/src/api/openai-completions.ts` `createClient` / `stream` at
//! pinned commit `3da591ab`.
//!
//! pi builds an `OpenAI` SDK client per request (`new OpenAI({ apiKey, baseURL,
//! defaultHeaders })`, `openai-completions.ts:567`) and lets the SDK put the
//! `POST {baseURL}/chat/completions` request (`client.chat.completions.create`,
//! `openai-completions.ts:223`) on the wire; SSE is selected by the body's
//! `stream: true`. This seam-targeted port reproduces exactly that: given the
//! model, context, and options, it assembles the [`HttpRequest`] the injected
//! [`HttpTransport`](crate::seams::http::HttpTransport) is handed (URL + headers
//! + serialized body from [`build_params`]) and decodes the SSE reply through the
//! already-ported [`parse_sse_chunks`] / [`walk_chunks`].
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
//! sit below `defaultHeaders`. The `user-agent` (`OpenAI/JS <version>`) and the
//! telemetry `x-stainless-*` headers the SDK also adds are cosmetic (the API does
//! not require them and their exact strings are not load-bearing), so they are
//! left to the SDK exactly as the anthropic port leaves the non-OAuth user-agent.
//!
//! # `getClientApiKey` fallback
//!
//! pi's `getClientApiKey` (`openai-completions.ts:60`): a present `apiKey` is used
//! verbatim; otherwise, when a caller header `authorization` or
//! `cf-aig-authorization` is already set, the apiKey becomes the sentinel
//! `"unused"` (so no real Bearer is minted over a caller-supplied credential);
//! otherwise it throws `No API key for provider: <provider>`, which pi catches as
//! a pre-start `error` event. The sentinel only reaches the wire as
//! `authorization: Bearer unused` when no `authorization` header was supplied
//! (e.g. auth was carried on `cf-aig-authorization`), reproducing the SDK exactly.
//!
//! # Error surfacing
//!
//! pi encodes post-start failures as a terminal `error` event; a failure before
//! the stream starts (missing auth, a non-2xx create, a transport error) throws
//! and is caught by the same handler, which pushes a single `error` event with no
//! preceding `start`. The buffered driver reproduces that: a non-2xx status, a
//! transport error, or a missing credential yield an error-only [`StreamResult`],
//! and a non-2xx carries the API's diagnostic through [`format_api_error`] rather
//! than discarding the response body.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::seams::http::{HttpRequest, HttpTransport};
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention, Context, Model,
    OpenAICompletionsCompat, SessionAffinityFormat, StopReason,
};

use super::{
    build_params, get_compat, parse_sse_chunks, resolve_cache_retention, walk_chunks, zero_usage,
    OpenAICompletionsModel, OpenAICompletionsOptions, ResolvedCompat,
};

/// pi's `hasHeader` (`openai-completions.ts:51`): a case-insensitive lookup for a
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

/// pi's `getClientApiKey` (`openai-completions.ts:60`): resolve the credential the
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

/// The request URL the OpenAI SDK derives from `baseURL`: the chat-completions
/// endpoint under the model's base URL (`client.chat.completions.create`,
/// `openai-completions.ts:223`). The catalog base URLs already carry the API
/// version segment (e.g. `.../v1`), so only `/chat/completions` is appended.
fn request_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// pi's `mergeHeaders`-equivalent `Object.assign` in `createClient`: later sources
/// override earlier ones. Keys are lowercased per the transport seam's convention.
fn merge_into(target: &mut BTreeMap<String, String>, source: &BTreeMap<String, String>) {
    for (key, value) in source {
        target.insert(key.to_ascii_lowercase(), value.clone());
    }
}

/// pi's `createClient` session-affinity block (`openai-completions.ts:551`): gated
/// on `sessionId && compat.sendSessionAffinityHeaders`, then shaped by
/// `compat.sessionAffinityFormat`. `openrouter` sets `x-session-id`; the OpenAI
/// formats set `x-client-request-id` + `x-session-affinity`, and the full `openai`
/// format additionally sets `session_id`.
fn apply_session_affinity(
    headers: &mut BTreeMap<String, String>,
    compat: &ResolvedCompat,
    session_id: &str,
) {
    if !compat.send_session_affinity_headers {
        return;
    }
    match compat.session_affinity_format {
        SessionAffinityFormat::Openrouter => {
            headers.insert("x-session-id".to_string(), session_id.to_string());
        }
        SessionAffinityFormat::Openai => {
            headers.insert("session_id".to_string(), session_id.to_string());
            headers.insert("x-client-request-id".to_string(), session_id.to_string());
            headers.insert("x-session-affinity".to_string(), session_id.to_string());
        }
        SessionAffinityFormat::OpenaiNosession => {
            headers.insert("x-client-request-id".to_string(), session_id.to_string());
            headers.insert("x-session-affinity".to_string(), session_id.to_string());
        }
    }
}

/// The SDK derives `authorization: Bearer <apiKey>` from `new OpenAI({ apiKey })`;
/// a caller-supplied `authorization` header (already merged from
/// `model.headers` / `optionsHeaders`) wins, so this only fills the gap.
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

/// Assemble the [`HttpRequest`] for a streaming OpenAI chat-completions call,
/// reproducing pi's `createClient` header composition. `body` is the serialized
/// `ChatCompletionCreateParamsStreaming` JSON (from [`build_params`]); `api_key`
/// is the already-resolved [`client_api_key`] credential.
///
/// `session_id` is the caller's session id already gated on cache retention (pi's
/// `cacheSessionId = cacheRetention === "none" ? undefined : sessionId`); the
/// affinity block further gates it on `sendSessionAffinityHeaders`.
#[allow(clippy::too_many_arguments)]
pub fn assemble_request(
    model: &OpenAICompletionsModel,
    compat: &ResolvedCompat,
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
        apply_session_affinity(&mut headers, compat, session_id);
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

/// Build the lean [`OpenAICompletionsModel`] the request shaper / SSE walker read
/// (identity + base URL + pricing + compat) from the full boundary model, mirroring
/// the anthropic driver's `anthropic_model`.
fn lean_model(model: &Model<OpenAICompletionsCompat>) -> OpenAICompletionsModel {
    OpenAICompletionsModel {
        id: model.id.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        reasoning: model.reasoning,
        thinking_level_map: model.thinking_level_map.clone(),
        input: model.input.clone(),
        cost: model.cost.clone(),
        compat: model.compat.clone(),
    }
}

/// The empty assistant output shell for a pre-start failure, mirroring the
/// anthropic driver's `empty_output`.
fn empty_output(model: &OpenAICompletionsModel, timestamp: i64) -> AssistantMessage {
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

/// A single-`error`-event result for a failure before the stream's `start` event,
/// matching pi's catch handler (`openai-completions.ts:505`).
fn error_result(model: &OpenAICompletionsModel, timestamp: i64, message: String) -> StreamResult {
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
/// text, then to a no-body marker — so callers see the API's diagnostic instead of
/// a bare status. Identical in shape to the anthropic driver's `format_api_error`.
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
/// `stream()` request assembly and SSE handling. `timestamp` is the message
/// timestamp pi sets via `Date.now()` (threaded here for determinism, as the SSE
/// walker already is). Generic over the transport so a [`ScriptedTransport`] can
/// be injected in tests.
///
/// [`ScriptedTransport`]: crate::seams::http::ScriptedTransport
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &Model<OpenAICompletionsCompat>,
    context: &Context,
    options: &OpenAICompletionsOptions,
    timestamp: i64,
) -> StreamResult {
    let lean = lean_model(model);
    let api_key = options.api_key.as_deref();

    // pi's getClientApiKey throws before the client is built; caught as an error.
    let client_key = match client_api_key(&lean.provider, api_key, options.headers.as_ref()) {
        Ok(key) => key,
        Err(message) => return error_result(&lean, timestamp, message),
    };

    let compat = get_compat(&lean);

    // pi: cacheSessionId = cacheRetention === "none" ? undefined : options.sessionId.
    let cache_retention = resolve_cache_retention(
        options.cache_retention,
        options.cache_retention_env.as_deref(),
    );
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

    match transport.send(&request) {
        Ok(response) if response.is_ok() => {
            let chunks = parse_sse_chunks(&response.body);
            let outcome = walk_chunks(&chunks, &lean, options, timestamp);
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
