// straitjacket-allow-file[:duplication] — the buildParams wiring and the
// parse/napi wrappers mirror the direct Gemini driver by design (pi keeps the
// two Google drivers as near-duplicate copies diverging only in client/auth
// construction). The clone detector reads the shared shape as duplication; the
// Vertex-specific surface here is the auth resolution and client-config build.
//! Google Vertex AI (`@google/genai`, `vertexai: true`) streaming driver, ported
//! from pi-ai's `packages/ai/src/api/google-vertex.ts` at pinned commit
//! `3da591ab`.
//!
//! The stream-decode loop, function-call id-synthesis, usage/cost math, and
//! request-body build are byte-identical to the direct Gemini driver and are
//! shared from [`crate::api::google_shared`]. What is unique to Vertex — and what
//! this module owns — is the client construction / auth shape covered by pi's
//! `google-vertex-api-key-resolution.test.ts`:
//!
//! - [`resolve_api_key`]: trims `options.apiKey`, discarding an empty string, the
//!   `gcp-vertex-credentials` marker, or a `<placeholder>`.
//! - A real key → an API-key client config; otherwise the ADC path resolves a
//!   project + location (and an optional `GOOGLE_APPLICATION_CREDENTIALS`
//!   keyFilename) via [`build_client_config`].
//! - [`build_http_options`]: custom-baseUrl handling with `COLLECTION` resource
//!   scope and `apiVersion` suppression when the URL already carries a version.
//!
//! Actual ADC credential acquisition (the token fetch) is the ai/auth sibling's
//! job; this module models only the request/stream mechanics and the config
//! shape the driver hands to the client constructor. Env values are read from an
//! injected [`ProviderEnv`] map (the host / auth sibling populates it), mirroring
//! pi's `getProviderEnvValue(name, env)` scoped-override lookup.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde_json::{json, Map, Value};

use super::google_generative_ai::merge_headers;
use super::google_shared::{parse_google_stream, GoogleModel, GoogleStreamDecoder, StreamOutcome};
use crate::types::{AssistantMessage, AssistantMessageEvent};
use crate::utils::sse::{AssistantEventReader, ServerSentEvent, SseEventDecoder};

pub mod adc;
pub mod client;
pub mod driver;

/// The `google-vertex` API discriminant set on the output message.
pub const API: &str = "google-vertex";

/// `google-vertex.ts:54` — the Vertex API version.
const API_VERSION: &str = "v1";
/// `google-vertex.ts:55` — sentinel meaning "use ADC, not an API key".
const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";

/// A scoped provider-env override map (pi's `ProviderEnv`). The driver reads only
/// this map; the host / auth sibling is responsible for populating it from the
/// process environment.
pub type ProviderEnv = BTreeMap<String, String>;

/// Options unique to the Vertex client (`createClient` / `createClientWithApiKey`):
/// auth inputs plus header overrides. Request-shaping options live in
/// [`super::google_shared::GoogleRequestOptions`].
#[derive(Debug, Clone, Default)]
pub struct GoogleVertexClientOptions {
    pub api_key: Option<String>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub env: ProviderEnv,
}

/// `getProviderEnvValue(name, env)` — resolve a provider env value from the
/// scoped override map. (The process-env and Bun-sandbox fallbacks pi layers on
/// belong to the host / auth sibling and are out of scope here.)
fn get_provider_env_value<'a>(name: &str, env: &'a ProviderEnv) -> Option<&'a str> {
    env.get(name).map(String::as_str).filter(|s| !s.is_empty())
}

/// `google-vertex.ts:417` — a `<placeholder>` api key: `^<[^>]+>$`.
fn is_placeholder_api_key(api_key: &str) -> bool {
    let bytes = api_key.as_bytes();
    if bytes.len() < 3 {
        return false;
    }
    if bytes[0] != b'<' || bytes[bytes.len() - 1] != b'>' {
        return false;
    }
    // Inner must be one-or-more chars, none of them `>`.
    !api_key[1..api_key.len() - 1].contains('>')
}

/// `google-vertex.ts:409` — resolve an effective Vertex API key. Returns `None`
/// (ADC path) when the key is empty, the `gcp-vertex-credentials` marker, or a
/// `<placeholder>`.
pub fn resolve_api_key(options: &GoogleVertexClientOptions) -> Option<String> {
    let api_key = options.api_key.as_deref()?.trim();
    if api_key.is_empty()
        || api_key == GCP_VERTEX_CREDENTIALS_MARKER
        || is_placeholder_api_key(api_key)
    {
        return None;
    }
    Some(api_key.to_string())
}

/// `google-vertex.ts:421` — resolve the GCP project: option, then
/// `GOOGLE_CLOUD_PROJECT`, then `GCLOUD_PROJECT`. `Err` when none is set.
pub fn resolve_project(options: &GoogleVertexClientOptions) -> Result<String, String> {
    if let Some(project) = options.project.as_deref().filter(|s| !s.is_empty()) {
        return Ok(project.to_string());
    }
    if let Some(project) = get_provider_env_value("GOOGLE_CLOUD_PROJECT", &options.env) {
        return Ok(project.to_string());
    }
    if let Some(project) = get_provider_env_value("GCLOUD_PROJECT", &options.env) {
        return Ok(project.to_string());
    }
    Err(
        "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options."
            .to_string(),
    )
}

/// `google-vertex.ts:434` — resolve the Vertex location: option, then
/// `GOOGLE_CLOUD_LOCATION`. `Err` when none is set.
pub fn resolve_location(options: &GoogleVertexClientOptions) -> Result<String, String> {
    if let Some(location) = options.location.as_deref().filter(|s| !s.is_empty()) {
        return Ok(location.to_string());
    }
    if let Some(location) = get_provider_env_value("GOOGLE_CLOUD_LOCATION", &options.env) {
        return Ok(location.to_string());
    }
    Err(
        "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options."
            .to_string(),
    )
}

/// `google-vertex.ts:404` — the `googleAuthOptions.keyFilename` from
/// `GOOGLE_APPLICATION_CREDENTIALS`, if set.
fn build_google_auth_options(env: &ProviderEnv) -> Option<Value> {
    get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env)
        .map(|key_filename| json!({ "keyFilename": key_filename }))
}

/// The service-account keyfile path pi hands the SDK as
/// `googleAuthOptions.keyFilename` — the `GOOGLE_APPLICATION_CREDENTIALS` env
/// value, when set. The one ADC source the Vertex runtime wires (see
/// [`adc`]); the backend reads this file to mint the Bearer token.
pub(crate) fn resolve_credentials_path(options: &GoogleVertexClientOptions) -> Option<String> {
    get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", &options.env).map(str::to_string)
}

/// The credential the driver assembles a Vertex request with, already resolved by
/// the backend to concrete request inputs: either the Express API key (the
/// `x-goog-api-key` path, unchanged from the Express port) or a service-account
/// Bearer token plus the project/location that place the regional ADC endpoint.
pub enum VertexRequestCredential<'a> {
    /// The Vertex Express (API-key) path: `x-goog-api-key: <key>`.
    ApiKey(&'a str),
    /// The ADC / service-account path: `Authorization: Bearer <token>` against
    /// `https://{location}-aiplatform.googleapis.com/.../projects/{project}/...`.
    Bearer {
        token: &'a str,
        project: &'a str,
        location: &'a str,
    },
}

/// `google-vertex.ts:387` — a usable custom base URL: `None` if empty or if it
/// still contains the `{location}` template placeholder.
fn resolve_custom_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed.contains("{location}") {
        return None;
    }
    Some(trimmed.to_string())
}

/// `google-vertex.ts:395` — whether a base URL's path carries an API version
/// segment (`^v\d+(?:beta\d*)?$`). Hand-rolled path extraction (no URL crate):
/// strip the `scheme://host`, then test each `/`-segment of the path.
fn base_url_includes_api_version(base_url: &str) -> bool {
    let path = match base_url.find("://") {
        Some(idx) => {
            let after = &base_url[idx + 3..];
            match after.find('/') {
                Some(slash) => &after[slash + 1..],
                None => "",
            }
        }
        None => base_url,
    };
    path.split('/').any(is_api_version_segment)
}

/// A single path segment matching `^v\d+(?:beta\d*)?$`.
fn is_api_version_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.first() != Some(&b'v') {
        return false;
    }
    let mut i = 1;
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    if i == bytes.len() {
        return true;
    }
    // optional `beta\d*`
    if segment[i..].starts_with("beta") {
        i += 4;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        return i == bytes.len();
    }
    false
}

/// `google-vertex.ts:368` — build the `httpOptions` for the client, or `None`
/// when empty. Handles custom base URL (with `COLLECTION` resource scope and
/// `apiVersion` suppression) and merged headers.
fn build_http_options(
    model: &GoogleModel,
    option_headers: &BTreeMap<String, String>,
) -> Option<Value> {
    let mut http_options = Map::new();
    if let Some(base_url) = resolve_custom_base_url(&model.base_url) {
        http_options.insert("baseUrl".to_string(), json!(base_url));
        http_options.insert("baseUrlResourceScope".to_string(), json!("COLLECTION"));
        if base_url_includes_api_version(&base_url) {
            http_options.insert("apiVersion".to_string(), json!(""));
        }
    }
    if let Some(headers) = merge_headers(model.headers.as_ref(), option_headers) {
        http_options.insert("headers".to_string(), headers);
    }
    if http_options.is_empty() {
        None
    } else {
        Some(Value::Object(http_options))
    }
}

/// Build the `GoogleGenAI` constructor configuration for Vertex
/// (`google-vertex.ts:337-366`).
///
/// Returned as the JSON object pi passes to `new GoogleGenAI(...)` — the value
/// pi's `google-vertex-api-key-resolution.test.ts` asserts on. A resolved API key
/// yields the `createClientWithApiKey` shape (`{ vertexai, apiKey, apiVersion }`);
/// otherwise the ADC shape (`{ vertexai, project, location, apiVersion,
/// googleAuthOptions? }`). `Err` when the ADC path lacks a project or location.
pub fn build_client_config(
    model: &GoogleModel,
    options: &GoogleVertexClientOptions,
) -> Result<Value, String> {
    let http_options = build_http_options(model, &options.headers);

    if let Some(api_key) = resolve_api_key(options) {
        let mut config = Map::new();
        config.insert("vertexai".to_string(), json!(true));
        config.insert("apiKey".to_string(), json!(api_key));
        config.insert("apiVersion".to_string(), json!(API_VERSION));
        if let Some(http_options) = http_options {
            config.insert("httpOptions".to_string(), http_options);
        }
        return Ok(Value::Object(config));
    }

    let project = resolve_project(options)?;
    let location = resolve_location(options)?;
    let mut config = Map::new();
    config.insert("vertexai".to_string(), json!(true));
    config.insert("project".to_string(), json!(project));
    config.insert("location".to_string(), json!(location));
    config.insert("apiVersion".to_string(), json!(API_VERSION));
    if let Some(auth_options) = build_google_auth_options(&options.env) {
        config.insert("googleAuthOptions".to_string(), auth_options);
    }
    if let Some(http_options) = http_options {
        config.insert("httpOptions".to_string(), http_options);
    }
    Ok(Value::Object(config))
}

/// Decode an already-obtained Vertex `generateContentStream` (a sequence of
/// parsed `GenerateContentResponse` chunk objects) into the uniform event stream
/// and final message for `model`.
pub fn parse_stream(chunks: &[Value], model: &GoogleModel, now_ms: i64) -> StreamOutcome {
    parse_google_stream(chunks, model, API, now_ms)
}

/// The incremental Vertex SSE decoder: it frames a `?alt=sse`
/// `streamGenerateContent` body one `data:` event at a time and runs the shared
/// [`GoogleStreamDecoder`] over the parsed chunk.
///
/// Vertex uses the same `@google/genai` `GenerateContentResponse` type as the
/// direct Gemini API and the SDK yields already-parsed chunk objects, so this
/// decoder is byte-identical to the direct-Gemini
/// [`GoogleGenerativeAiSseDecoder`](super::google_generative_ai::GoogleGenerativeAiSseDecoder)
/// except for the `google-vertex` api discriminant threaded into the shared
/// decoder core. Each frame's `data:` payload is one complete chunk JSON; a
/// `[DONE]` sentinel, an empty payload, or an unparseable payload is skipped —
/// matching the buffered framing so the two paths stay byte-identical.
pub(crate) struct GoogleVertexSseDecoder {
    inner: GoogleStreamDecoder,
}

impl GoogleVertexSseDecoder {
    /// A fresh Vertex SSE decoder for `model`.
    pub(crate) fn new(model: GoogleModel, now_ms: i64) -> Self {
        Self {
            inner: GoogleStreamDecoder::new(model, API, now_ms),
        }
    }
}

impl SseEventDecoder for GoogleVertexSseDecoder {
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

/// Parse a Vertex `?alt=sse` `streamGenerateContent` `body` into the uniform
/// event stream and final message for `model`.
///
/// This feeds the whole body through the shared
/// [`SseFrameSplitter`](crate::utils::sse::SseFrameSplitter) and the SAME
/// [`GoogleVertexSseDecoder`] the incremental driver uses, over a one-chunk
/// iterator, so the buffered driver's events + terminal message are byte-identical
/// to feeding the reader chunk-by-chunk.
pub fn parse_sse_stream(body: &str, model: &GoogleModel, now_ms: i64) -> StreamOutcome {
    let decoder = GoogleVertexSseDecoder::new(model.clone(), now_ms);
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

/// napi boundary entry point: decode the Vertex stream chunks given the model
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
