//! Node-API surface for the provider HTTP error-body normalizer.
//!
//! This exposes the Rust port of pi's `packages/ai/src/utils/error-body.ts`
//! (ported bit-exactly in [`pidgin_ai::utils::error_body`]) to pi's
//! `packages/ai` `error-body.test.ts`. The Rust module owns every decision the
//! normalizer makes: the SDK-shape field-probe precedence (Mistral
//! `statusCode`/`body` → `openai` `status`/`error` → `@google/genai` `status` →
//! AWS Bedrock `$metadata`/`$response`), the empty-object-is-no-body rule, the
//! UTF-16 truncation cap, the `messageCarriesBody` flag, and the
//! `formatProviderError` compose rules.
//!
//! # The seam: field-probe precedence in Rust, `unknown` unwrapping in TS
//!
//! pi's `normalizeProviderError(error: unknown)` branches on `instanceof Error`
//! and then duck-types SDK fields off the caught value. JavaScript's dynamic
//! `unknown` cannot cross the addon boundary as a live object, so the shim does
//! the ONE thing only the JS runtime can — split `Error` vs non-`Error` and
//! pluck the candidate carrier fields off the caught value — and hands the
//! result across as a plain JSON envelope. It makes NO decisions: which field
//! wins, whether a body is "empty", and every output string are all decided
//! here in Rust. The envelope's numeric/string slots are coerced with the same
//! `typeof === "number"` / `typeof === "string"` gates pi applies inside
//! `extractStatus` / `pickBodyText`, so a wrong-typed carrier falls through to
//! the next candidate exactly as it does in pi.
//!
//! # Marshaling
//!
//! Everything crosses as JSON strings. `normalizeProviderError` takes the
//! envelope JSON and returns the normalized struct as JSON (absent `status` /
//! `body` are omitted so they read back as `undefined`, not `null`);
//! `formatProviderError` takes the normalized struct JSON plus an optional
//! prefix; `truncateErrorText` takes a string and a length. The object-valued
//! carriers (`error`, `$response.body`) cross as nested JSON values and are
//! re-serialized by the Rust `safeJsonStringify` — no JS closures, streams, or
//! stable object identity cross the boundary. pi's exported `safeJsonStringify`
//! itself stays in TS: it exists to absorb JS-runtime `JSON.stringify` edge
//! cases (`undefined`, functions, circular refs) over arbitrary values, which
//! cannot be reproduced from Rust without a JS engine, so the shim re-exports
//! pi's original for that one symbol.

use napi_derive::napi;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_ai::utils::error_body::{
    format_provider_error, normalize_provider_error, truncate_error_text, NormalizedProviderError,
    SdkError, ThrownError,
};

/// The marshaled `error: unknown` split the shim produces: `kind: "error"`
/// carries the plucked SDK carrier fields; `kind: "other"` carries a non-`Error`
/// thrown value verbatim for `safeJsonStringify`.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ThrownEnvelope {
    // Boxed: `SdkErrorEnvelope` is far larger than the `Other` variant, so the
    // box keeps the enum small (clippy::large_enum_variant).
    Error(Box<SdkErrorEnvelope>),
    Other {
        #[serde(default)]
        value: Option<Value>,
    },
}

/// The duck-typed carrier fields plucked off a thrown `Error`. Each numeric /
/// string field crosses as a raw JSON value and is coerced here with pi's
/// `typeof` gate; the object-valued carriers (`error`, `responseBody`) cross as
/// nested values and are handed to the Rust normalizer untouched.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SdkErrorEnvelope {
    #[serde(default)]
    message: String,
    #[serde(default)]
    status_code: Option<Value>,
    #[serde(default)]
    status: Option<Value>,
    #[serde(default)]
    body: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    metadata_http_status_code: Option<Value>,
    #[serde(default)]
    response_status_code: Option<Value>,
    #[serde(default)]
    response_body: Option<Value>,
}

/// pi's `typeof x === "number"` gate: a JSON number becomes the typed slot,
/// anything else falls through as absent so the next candidate wins.
fn coerce_number(value: Option<Value>) -> Option<u32> {
    match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|v| v as u32)
            .or_else(|| n.as_f64().map(|v| v as u32)),
        _ => None,
    }
}

/// pi's `typeof x === "string"` gate: a JSON string becomes the typed slot,
/// anything else falls through as absent.
fn coerce_string(value: Option<Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

impl SdkErrorEnvelope {
    fn into_sdk(self) -> SdkError {
        SdkError {
            message: self.message,
            status_code: coerce_number(self.status_code),
            status: coerce_number(self.status),
            body: coerce_string(self.body),
            error: self.error,
            metadata_http_status_code: coerce_number(self.metadata_http_status_code),
            response_status_code: coerce_number(self.response_status_code),
            response_body: self.response_body,
        }
    }
}

/// The normalized struct returned to JS. `status` / `body` are omitted when
/// absent so they read back as `undefined` (matching pi's optional interface
/// fields), not JSON `null`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    message: String,
    message_carries_body: bool,
}

/// The normalized struct received back from JS for `formatProviderError`. Absent
/// `status` / `body` deserialize to `None`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedIn {
    #[serde(default)]
    status: Option<u32>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    message_carries_body: bool,
}

/// pi's `normalizeProviderError`. Takes the shim's JSON envelope (the plucked
/// `Error` carrier fields, or a non-`Error` value), runs the whole field-probe /
/// truncation / `messageCarriesBody` normalizer in Rust, and returns the result
/// as JSON.
#[napi(js_name = "normalizeProviderError")]
pub fn normalize_provider_error_napi(envelope_json: String) -> napi::Result<String> {
    let envelope: ThrownEnvelope = serde_json::from_str(&envelope_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid error envelope: {err}")))?;

    let thrown = match envelope {
        ThrownEnvelope::Error(sdk) => ThrownError::Error((*sdk).into_sdk()),
        ThrownEnvelope::Other { value } => ThrownError::Other(value.unwrap_or(Value::Null)),
    };

    let norm = normalize_provider_error(&thrown);
    let out = NormalizedOut {
        status: norm.status,
        body: norm.body,
        message: norm.message,
        message_carries_body: norm.message_carries_body,
    };
    serde_json::to_string(&out)
        .map_err(|err| napi::Error::from_reason(format!("serialize normalized error: {err}")))
}

/// pi's `formatProviderError`. Takes a normalized struct as JSON plus an optional
/// provider prefix, and composes the display string in Rust.
#[napi(js_name = "formatProviderError")]
pub fn format_provider_error_napi(
    norm_json: String,
    prefix: Option<String>,
) -> napi::Result<String> {
    let input: NormalizedIn = serde_json::from_str(&norm_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid normalized error: {err}")))?;
    let norm = NormalizedProviderError {
        status: input.status,
        body: input.body,
        message: input.message,
        message_carries_body: input.message_carries_body,
    };
    Ok(format_provider_error(&norm, prefix.as_deref()))
}

/// pi's `truncateErrorText`: truncate `text` to `max_chars` UTF-16 code units,
/// appending pi's `... [truncated N chars]` suffix when it was over the cap.
#[napi(js_name = "truncateErrorText")]
pub fn truncate_error_text_napi(text: String, max_chars: i64) -> String {
    truncate_error_text(&text, max_chars.max(0) as usize)
}
