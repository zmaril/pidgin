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

use serde_json::{json, Map, Value};

use super::google_generative_ai::merge_headers;
use super::google_shared::{parse_google_stream, GoogleModel, StreamOutcome};

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
    env.get(name)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
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
fn build_http_options(model: &GoogleModel, option_headers: &BTreeMap<String, String>) -> Option<Value> {
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
