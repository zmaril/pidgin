// straitjacket-allow-file:duplication — the header-merge, `x-goog-api-key`
// only-when-absent injection, and `serialize_body` helper are mirrored by the
// Vertex assembler (`api/google_vertex/client.rs`) by design (pi keeps the two
// Google clients as near-duplicate copies diverging only in the derived URL).
// The clone detector reads the shared shape as duplication; the dialect-specific
// surface here is the direct-Gemini request URL.
//! The Google Generative AI request assembler, the `createClient`-equivalent
//! half of pi-ai's `google-generative-ai.ts` at pinned commit `3da591ab`.
//!
//! pi builds a `new GoogleGenAI({ apiKey, httpOptions })` client per request and
//! lets the `@google/genai` SDK put `client.models.generateContentStream(params)`
//! on the wire (`google-generative-ai.ts:81/87/322-341`). This seam-targeted port
//! reproduces exactly the wire request that SDK derives, given the model, the
//! serialized request body ([`super::super::google_shared::build_params`]), the
//! credential, and the per-request header overrides.
//!
//! What the SDK derives (reproduced here because the raw
//! [`HttpTransport`](crate::seams::http::HttpTransport) has no SDK layer):
//! - the streaming URL. The SDK joins `baseUrl / apiVersion / path` and appends
//!   `?alt=sse` for a streaming call (`js-genai` `_api_client.ts`); pi's
//!   `createClient` sets `httpOptions.apiVersion = ""` whenever `model.baseUrl`
//!   is present (`google-generative-ai.ts:328-331`), so the version segment is
//!   the one already baked into `model.baseUrl` (e.g.
//!   `https://generativelanguage.googleapis.com/v1beta`). The method path is
//!   `models/{model}:streamGenerateContent`, yielding
//!   `{base_url}/models/{model}:streamGenerateContent?alt=sse`.
//! - the `x-goog-api-key: <apiKey>` header. The SDK's NodeAuth `addAuthHeaders`
//!   appends `x-goog-api-key` (its `GOOGLE_API_KEY_HEADER`) only when the header
//!   is absent (`js-genai` `_node_auth.ts`), so a caller-supplied header wins.
//! - `content-type: application/json` for the JSON request body, inserted at low
//!   precedence so a caller-supplied `content-type` still wins.
//!
//! Model + per-request header overrides are merged via the ported
//! [`merge_headers`](super::merge_headers)
//! (`providerHeadersToRecord({ ...model.headers, ...optionsHeaders })`,
//! `google-generative-ai.ts:332`). Keys are lowercased per the transport seam's
//! convention.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::seams::http::HttpRequest;

use super::super::google_shared::GoogleModel;
use super::merge_headers;

/// The header the `@google/genai` SDK's NodeAuth injects the API key on
/// (`GOOGLE_API_KEY_HEADER`), the direct-Gemini analog of Anthropic's
/// `x-api-key`.
pub const GOOGLE_API_KEY_HEADER: &str = "x-goog-api-key";

/// The streaming URL the `@google/genai` SDK derives for a
/// `generateContentStream` call: `{base_url}/models/{model}:streamGenerateContent?alt=sse`.
///
/// `model.base_url` already carries the version path (pi sets `apiVersion = ""`
/// in `createClient` whenever it is present), so no version segment is appended.
fn request_url(base_url: &str, model_id: &str) -> String {
    format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/'),
        model_id
    )
}

/// Assemble the [`HttpRequest`] for a streaming Google Generative AI call,
/// reproducing the wire request pi's `createClient` + `@google/genai` SDK
/// produce. `body` is the serialized `GenerateContentParameters` JSON (from
/// [`super::super::google_shared::build_params`]).
pub fn assemble_request(
    model: &GoogleModel,
    body: String,
    api_key: Option<&str>,
    options_headers: &BTreeMap<String, String>,
) -> HttpRequest {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();

    // providerHeadersToRecord({ ...model.headers, ...optionsHeaders }): the model
    // and per-request header overrides, lowercased per the transport convention.
    if let Some(Value::Object(merged)) = merge_headers(model.headers.as_ref(), options_headers) {
        for (key, value) in merged {
            if let Some(text) = value.as_str() {
                headers.insert(key.to_ascii_lowercase(), text.to_string());
            }
        }
    }

    // NodeAuth.addAuthHeaders appends x-goog-api-key only when absent, so a
    // caller-supplied header wins.
    if let Some(api_key) = api_key {
        headers
            .entry(GOOGLE_API_KEY_HEADER.to_string())
            .or_insert_with(|| api_key.to_string());
    }

    // The SDK sets content-type for the JSON body; a caller-supplied header wins.
    headers
        .entry("content-type".to_string())
        .or_insert_with(|| "application/json".to_string());

    HttpRequest {
        method: "POST".to_string(),
        url: request_url(&model.base_url, &model.id),
        headers,
        body: Some(body),
    }
}

/// Serialize the request body [`Value`]; only defined for a `serde_json::Value`,
/// for which serialization cannot fail.
pub fn serialize_body(body: &Value) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string())
}
