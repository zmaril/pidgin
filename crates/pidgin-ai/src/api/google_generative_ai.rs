// straitjacket-allow-file:duplication — the client-config builder and the
// buildParams wiring mirror pi's `google-vertex.ts` counterparts closely by
// design (pi keeps the two drivers as near-duplicate copies that diverge only in
// client/auth construction). The clone detector reads the shared shape as
// duplication; it is a faithful, load-bearing transcription kept parallel to the
// Vertex driver on purpose.
//! Google Generative AI (`@google/genai`) streaming driver, ported from pi-ai's
//! `packages/ai/src/api/google-generative-ai.ts` at pinned commit `3da591ab`.
//!
//! The stream-decode loop, function-call id-synthesis, usage/cost math, and
//! request-body build are shared with the Vertex driver and live in
//! [`crate::api::google_shared`]. This module carries the parts unique to the
//! direct Gemini API: the `GoogleGenAI` client configuration shape
//! (`createClient`) and the driver's thin `parse` / napi entry wrappers.
//!
//! Following the pidgin design (see `notes/startup/communications.md`), the HTTP
//! transport is supplied by the host; the Rust side is the pure decode/transform
//! half. [`parse_stream`] takes the already-obtained `generateContentStream`
//! chunks (what pi's `for await (chunk of googleStream)` yields) and reproduces
//! the dispatch that follows.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde_json::{json, Map, Value};

use super::google_shared::{parse_google_stream, GoogleModel, GoogleStreamDecoder, StreamOutcome};
use crate::types::{AssistantMessage, AssistantMessageEvent};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

pub mod client;
pub mod driver;

/// The `google-generative-ai` API discriminant set on the output message.
pub const API: &str = "google-generative-ai";

/// Options unique to the direct Gemini client (`createClient`): the API key and
/// any per-request header overrides. Request-shaping options (temperature,
/// tools, thinking) live in [`super::google_shared::GoogleRequestOptions`].
#[derive(Debug, Clone, Default)]
pub struct GoogleClientOptions {
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

/// Build the `GoogleGenAI` constructor configuration for the direct Gemini API
/// (`google-generative-ai.ts:322-341`).
///
/// Returned as the JSON object pi passes to `new GoogleGenAI(...)`. A custom
/// `model.baseUrl` sets `httpOptions.baseUrl` and suppresses the appended
/// version path (`apiVersion: ""`); model + option headers are merged into
/// `httpOptions.headers`. `httpOptions` is omitted entirely when empty.
pub fn build_client_config(model: &GoogleModel, options: &GoogleClientOptions) -> Value {
    let mut http_options = Map::new();
    if !model.base_url.is_empty() {
        http_options.insert("baseUrl".to_string(), json!(model.base_url));
        http_options.insert("apiVersion".to_string(), json!(""));
    }
    if let Some(headers) = merge_headers(model.headers.as_ref(), &options.headers) {
        http_options.insert("headers".to_string(), headers);
    }

    let mut config = Map::new();
    if let Some(api_key) = &options.api_key {
        config.insert("apiKey".to_string(), json!(api_key));
    }
    if !http_options.is_empty() {
        config.insert("httpOptions".to_string(), Value::Object(http_options));
    }
    Value::Object(config)
}

/// `providerHeadersToRecord({ ...model.headers, ...optionsHeaders })` — merge the
/// model's headers with per-request overrides; `None` when the result is empty.
pub(crate) fn merge_headers(
    model_headers: Option<&BTreeMap<String, String>>,
    option_headers: &BTreeMap<String, String>,
) -> Option<Value> {
    let mut merged = Map::new();
    if let Some(model_headers) = model_headers {
        for (k, v) in model_headers {
            merged.insert(k.clone(), json!(v));
        }
    }
    for (k, v) in option_headers {
        merged.insert(k.clone(), json!(v));
    }
    if merged.is_empty() {
        None
    } else {
        Some(Value::Object(merged))
    }
}

/// Decode an already-obtained `generateContentStream` (a sequence of parsed
/// `GenerateContentResponse` chunk objects) into the uniform event stream and
/// final message for `model`.
pub fn parse_stream(chunks: &[Value], model: &GoogleModel, now_ms: i64) -> StreamOutcome {
    parse_google_stream(chunks, model, API, now_ms)
}

/// The incremental direct-Gemini SSE decoder: it frames a `?alt=sse`
/// `streamGenerateContent` body one `data:` event at a time and runs the shared
/// [`GoogleStreamDecoder`] over the parsed chunk.
///
/// The `@google/genai` SDK yields already-parsed `GenerateContentResponse`
/// objects, so this decoder inlines the per-frame JSON parse the SDK performs:
/// each frame's `data:` payload is one complete chunk JSON. A `[DONE]` sentinel,
/// an empty payload, or an unparseable payload is skipped — matching the buffered
/// `sse_body_to_chunks` framing exactly, so the two paths stay byte-identical.
/// The accumulated thought-signature retention (per streamed text/thinking block)
/// lives in the shared decoder core and is carried through into the terminal
/// message emitted by [`finish`](SseEventDecoder::finish).
pub(crate) struct GoogleGenerativeAiSseDecoder {
    inner: GoogleStreamDecoder,
}

impl GoogleGenerativeAiSseDecoder {
    /// A fresh direct-Gemini SSE decoder for `model`.
    pub(crate) fn new(model: GoogleModel, now_ms: i64) -> Self {
        Self {
            inner: GoogleStreamDecoder::new(model, API, now_ms),
        }
    }
}

impl SseEventDecoder for GoogleGenerativeAiSseDecoder {
    fn on_frame(
        &mut self,
        frame: &ServerSentEvent,
        out: &mut Vec<AssistantMessageEvent>,
    ) -> ControlFlow<String> {
        let data = frame.data.trim();
        // A stray `[DONE]` sentinel or an empty payload carries no chunk; the SDK
        // never surfaces one, so skip it (mirrors the buffered `flush_chunk`).
        if data.is_empty() || data == "[DONE]" {
            return ControlFlow::Continue(());
        }
        // Inline the SDK's per-frame parse: each `data:` payload is one complete
        // `GenerateContentResponse`. An unparseable payload is dropped (the
        // buffered path drops it too), never a terminal error.
        if let Ok(chunk) = serde_json::from_str::<Value>(data) {
            self.inner.process_chunk(&chunk, out);
        }
        ControlFlow::Continue(())
    }

    fn finish(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
        self.inner.finish(out)
    }
}

/// Parse a direct-Gemini `?alt=sse` `streamGenerateContent` `body` into the
/// uniform event stream and final message for `model`.
///
/// This feeds the whole body through the shared
/// [`SseFrameSplitter`](crate::utils::sse::SseFrameSplitter) and the SAME
/// [`GoogleGenerativeAiSseDecoder`] the incremental driver uses, over a one-chunk
/// iterator, so the buffered driver's events + terminal message are byte-identical
/// to feeding the reader chunk-by-chunk.
pub fn parse_sse_stream(body: &str, model: &GoogleModel, now_ms: i64) -> StreamOutcome {
    let decoder = GoogleGenerativeAiSseDecoder::new(model.clone(), now_ms);
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

/// napi boundary entry point: decode the Gemini stream chunks given the model
/// JSON and return the [`StreamOutcome`] as a JSON string. `chunks_json` is a
/// JSON array of parsed `GenerateContentResponse` objects.
pub fn parse_stream_to_json(
    chunks_json: &str,
    model_json: &str,
    timestamp: i64,
) -> Result<String, String> {
    let chunks: Vec<Value> =
        serde_json::from_str(chunks_json).map_err(|e| format!("invalid chunks json: {e}"))?;
    let model: GoogleModel =
        serde_json::from_str(model_json).map_err(|e| format!("invalid model json: {e}"))?;
    let outcome = parse_stream(&chunks, &model, timestamp);
    serde_json::to_string(&outcome).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
