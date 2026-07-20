// straitjacket-allow-file:duplication — the header-merge, `x-goog-api-key`
// only-when-absent injection, and `serialize_body` helper mirror the direct-Gemini
// assembler by design (pi keeps the two Google clients as near-duplicate copies
// diverging only in the derived URL). The clone detector reads the shared shape as
// duplication; the Vertex-specific surface here is the Express + ADC request URLs.
//! The Google Vertex AI request assembler, the `createClientWithApiKey` /
//! `createClient`-equivalent half of pi-ai's `google-vertex.ts` at pinned commit
//! `3da591ab`.
//!
//! pi builds a `new GoogleGenAI({ vertexai: true, ... })` client per request and
//! lets the `@google/genai` SDK put `client.models.generateContentStream(params)`
//! on the wire (`google-vertex.ts:98/105/355-366`). This seam-targeted port
//! reproduces the wire request that SDK derives for BOTH Vertex auth paths, given
//! the model, the serialized request body
//! ([`super::super::google_shared::build_params`]), the resolved credential, and
//! the per-request header overrides:
//! - [`assemble_request`]: the **Express (API-key) path** — the
//!   `https://aiplatform.googleapis.com/v1` host with an `x-goog-api-key` header.
//! - [`assemble_adc_request`]: the **ADC / service-account path** — the regional
//!   `https://{location}-aiplatform.googleapis.com/v1/projects/{project}/`
//!   `locations/{location}` host with an `Authorization: Bearer <token>` header.
//!   The token is minted by [`super::adc`] from a service-account keyfile.
//!
//! What the SDK derives for the Express path (reproduced here because the raw
//! [`HttpTransport`](crate::seams::http::HttpTransport) has no SDK layer):
//! - the streaming URL. For a Vertex client with an `apiKey` set, the SDK's
//!   `computeInitHttpOptions` picks the Express endpoint
//!   `https://aiplatform.googleapis.com/` and appends `apiVersion` (pi's
//!   `API_VERSION = "v1"`), so `getRequestUrlInternal` yields
//!   `https://aiplatform.googleapis.com/v1` (`@google/genai` `_api_client`). A
//!   pi custom `model.baseUrl` overrides the host (with `COLLECTION` resource
//!   scope; the version segment is suppressed when the URL already carries one).
//!   The model is transformed to `publishers/google/models/{model}` (`tModel`)
//!   and `shouldPrependVertexProjectPath` returns `false` whenever an `apiKey` is
//!   set, so no `projects/{project}/locations/{location}` prefix is added. The
//!   method path is `{model}:streamGenerateContent?alt=sse`, yielding
//!   `{base}/publishers/google/models/{model}:streamGenerateContent?alt=sse`.
//! - the `x-goog-api-key: <apiKey>` header. The SDK's `NodeAuth.addKeyHeader`
//!   appends `x-goog-api-key` (its `GOOGLE_API_KEY_HEADER`) only when the header
//!   is absent (`@google/genai` `_node_auth`), so a caller-supplied header wins.
//! - `content-type: application/json` for the JSON request body, inserted at low
//!   precedence so a caller-supplied `content-type` still wins.
//!
//! Model + per-request header overrides are merged via the ported
//! [`merge_headers`](super::merge_headers)
//! (`providerHeadersToRecord({ ...model.headers, ...optionsHeaders })`,
//! `google-vertex.ts:379`). Keys are lowercased per the transport seam's
//! convention.

use std::collections::BTreeMap;

use serde_json::Value;

use super::super::google_shared::GoogleModel;
use super::merge_headers;
use super::{base_url_includes_api_version, resolve_custom_base_url, API_VERSION};
use crate::seams::http::HttpRequest;

/// The header the `@google/genai` SDK's `NodeAuth` injects the API key on
/// (`GOOGLE_API_KEY_HEADER`), the Vertex analog of Anthropic's `x-api-key`.
pub const GOOGLE_API_KEY_HEADER: &str = "x-goog-api-key";

/// The header the ADC / service-account path authenticates on: the OAuth2
/// access token as `Authorization: Bearer <token>` (google-auth-library's
/// `getRequestHeaders`).
pub const AUTHORIZATION_HEADER: &str = "authorization";

/// The Vertex Express endpoint the SDK picks when an `apiKey` is set and no
/// custom base URL applies (`computeInitHttpOptions`:
/// `initHttpOptions.baseUrl = 'https://aiplatform.googleapis.com/'`).
const VERTEX_EXPRESS_BASE_URL: &str = "https://aiplatform.googleapis.com";

/// The versioned base the `@google/genai` SDK's `getRequestUrlInternal` derives
/// for the Vertex Express (API-key) client.
///
/// A pi custom `model.baseUrl` (no `{location}` placeholder) overrides the host
/// with `COLLECTION` resource scope; the `apiVersion` segment is suppressed when
/// that URL already carries a version (`baseUrlIncludesApiVersion`). Otherwise the
/// Express host `https://aiplatform.googleapis.com` is used with the `v1`
/// version. `shouldPrependVertexProjectPath` returns `false` for the API-key path,
/// so no `projects/{project}/locations/{location}` prefix is ever added here.
fn versioned_base(base_url: &str) -> String {
    match resolve_custom_base_url(base_url) {
        Some(custom) => custom_versioned_base(&custom),
        None => format!("{VERTEX_EXPRESS_BASE_URL}/{API_VERSION}"),
    }
}

/// The versioned base for a custom `model.baseUrl` (no `{location}` placeholder),
/// shared by the Express and ADC URL derivations: the trimmed host with the `v1`
/// version appended unless the URL already carries one
/// (`baseUrlIncludesApiVersion`, `COLLECTION` resource scope).
fn custom_versioned_base(custom: &str) -> String {
    let trimmed = custom.trim_end_matches('/').to_string();
    if base_url_includes_api_version(custom) {
        trimmed
    } else {
        format!("{trimmed}/{API_VERSION}")
    }
}

/// The streaming URL the `@google/genai` SDK derives for a Vertex Express
/// `generateContentStream` call:
/// `{versioned_base}/publishers/google/models/{model}:streamGenerateContent?alt=sse`.
fn request_url(model: &GoogleModel) -> String {
    format!(
        "{}/publishers/google/models/{}:streamGenerateContent?alt=sse",
        versioned_base(&model.base_url),
        model.id
    )
}

/// Assemble the [`HttpRequest`] for a streaming Google Vertex (Express, API-key)
/// call, reproducing the wire request pi's `createClientWithApiKey` +
/// `@google/genai` SDK produce. `body` is the serialized
/// `GenerateContentParameters` JSON (from
/// [`super::super::google_shared::build_params`]); `api_key` is the resolved
/// Vertex API key ([`super::resolve_api_key`]).
pub fn assemble_request(
    model: &GoogleModel,
    body: String,
    api_key: &str,
    options_headers: &BTreeMap<String, String>,
) -> HttpRequest {
    let mut headers = base_headers(model, options_headers);

    // NodeAuth.addKeyHeader appends x-goog-api-key only when absent, so a
    // caller-supplied header wins.
    headers
        .entry(GOOGLE_API_KEY_HEADER.to_string())
        .or_insert_with(|| api_key.to_string());

    // The SDK sets content-type for the JSON body; a caller-supplied header wins.
    headers
        .entry("content-type".to_string())
        .or_insert_with(|| "application/json".to_string());

    HttpRequest {
        method: "POST".to_string(),
        url: request_url(model),
        headers,
        body: Some(body),
    }
}

/// The merged `providerHeadersToRecord({ ...model.headers, ...optionsHeaders })`:
/// the model and per-request header overrides, lowercased per the transport
/// convention. Shared by the Express (API-key) and ADC (Bearer) assemblers.
fn base_headers(
    model: &GoogleModel,
    options_headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    if let Some(Value::Object(merged)) = merge_headers(model.headers.as_ref(), options_headers) {
        for (key, value) in merged {
            if let Some(text) = value.as_str() {
                headers.insert(key.to_ascii_lowercase(), text.to_string());
            }
        }
    }
    headers
}

/// The streaming URL the `@google/genai` SDK derives for a Vertex ADC /
/// service-account `generateContentStream` call.
///
/// With no `apiKey` the SDK targets the **regional** Vertex host
/// `https://{location}-aiplatform.googleapis.com/{apiVersion}` and
/// `shouldPrependVertexProjectPath` returns `true`, so the full-resource
/// `projects/{project}/locations/{location}` prefix is prepended before the
/// `publishers/google/models/{model}:streamGenerateContent?alt=sse` path. A pi
/// custom `model.baseUrl` (no `{location}` placeholder) overrides the host with
/// the same `COLLECTION`-scope version handling as the Express path.
fn adc_request_url(model: &GoogleModel, project: &str, location: &str) -> String {
    let base = match resolve_custom_base_url(&model.base_url) {
        Some(custom) => custom_versioned_base(&custom),
        None => format!("https://{location}-aiplatform.googleapis.com/{API_VERSION}"),
    };
    format!(
        "{base}/projects/{project}/locations/{location}/publishers/google/models/{}:streamGenerateContent?alt=sse",
        model.id
    )
}

/// Assemble the [`HttpRequest`] for a streaming Google Vertex ADC /
/// service-account call, reproducing the wire request pi's `createClient` +
/// `@google/genai` SDK + `google-auth-library` produce: the regional
/// `projects/{project}/locations/{location}` endpoint authenticated with an
/// `Authorization: Bearer <token>` header. `access_token` is the token minted by
/// [`super::adc`]; `body` is the same serialized `GenerateContentParameters` JSON
/// the Express path sends.
pub fn assemble_adc_request(
    model: &GoogleModel,
    body: String,
    access_token: &str,
    project: &str,
    location: &str,
    options_headers: &BTreeMap<String, String>,
) -> HttpRequest {
    let mut headers = base_headers(model, options_headers);

    // google-auth-library sets Authorization from the minted token; a
    // caller-supplied header wins, mirroring the Express key-header precedence.
    headers
        .entry(AUTHORIZATION_HEADER.to_string())
        .or_insert_with(|| format!("Bearer {access_token}"));

    // The SDK sets content-type for the JSON body; a caller-supplied header wins.
    headers
        .entry("content-type".to_string())
        .or_insert_with(|| "application/json".to_string());

    HttpRequest {
        method: "POST".to_string(),
        url: adc_request_url(model, project, location),
        headers,
        body: Some(body),
    }
}

/// Serialize the request body [`Value`]; only defined for a `serde_json::Value`,
/// for which serialization cannot fail.
pub fn serialize_body(body: &Value) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string())
}
