// straitjacket-allow-file[:duplication] — the client-config builder and the
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
//! Following the atilla design (see `notes/startup/communications.md`), the HTTP
//! transport is supplied by the host; the Rust side is the pure decode/transform
//! half. [`parse_stream`] takes the already-obtained `generateContentStream`
//! chunks (what pi's `for await (chunk of googleStream)` yields) and reproduces
//! the dispatch that follows.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::google_shared::{parse_google_stream, GoogleModel, StreamOutcome};

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
