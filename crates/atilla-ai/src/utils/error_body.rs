//! Provider HTTP error-body normalization, ported from pi-ai's
//! `packages/ai/src/utils/error-body.ts` at pinned commit `3da591ab`.
//!
//! Endpoints behind a proxy/gateway can return a non-2xx response whose body the
//! provider SDK cannot fold into `error.message`. The SDK error object still
//! carries the HTTP status and raw/parsed body, but under SDK-specific field
//! names (Mistral `statusCode`/`body`, `openai` `status`/`error`, `@google/genai`
//! `status`, AWS Bedrock `$metadata`/`$response`). [`normalize_provider_error`]
//! probes those shapes into a uniform [`NormalizedProviderError`], and
//! [`format_provider_error`] composes it back into a display string.
//!
//! # Modelling the JS `unknown` throw in Rust
//!
//! pi accepts `error: unknown` and branches on `instanceof Error` plus duck-typed
//! SDK fields. Rust has no equivalent, so the boundary is modelled explicitly:
//! [`SdkError`] is the "it was an `Error`" case carrying every probed field as an
//! [`Option`], and [`ThrownError`] is the outer `Error | non-Error` split. This
//! preserves pi's observable behaviour — the field-probe precedence, the
//! empty-object-is-no-body rule, the truncation cap, and every output string —
//! while making the shape a caller must supply explicit.

use serde_json::Value;

/// The provider-error body truncation cap (`error-body.ts:16`).
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// The result of probing an SDK error object (`error-body.ts:18`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderError {
    /// HTTP status code, when one could be extracted.
    pub status: Option<u32>,
    /// Raw HTTP body reason, already trimmed and truncated to the cap.
    pub body: Option<String>,
    /// `error.message`, or `safe_json_stringify(error)` for a non-`Error` throw.
    pub message: String,
    /// True when `message` already contains the body (no separate body to add).
    pub message_carries_body: bool,
}

/// The duck-typed SDK error shape pi probes (`error-body.ts:29`,
/// `SdkErrorShape`). Every field is optional; each mirrors a provider SDK's
/// carrier for the status/body.
#[derive(Debug, Clone, Default)]
pub struct SdkError {
    /// `error.message`.
    pub message: String,
    /// Mistral: numeric `statusCode`.
    pub status_code: Option<u32>,
    /// `openai` / `@google/genai`: numeric `status`.
    pub status: Option<u32>,
    /// Mistral: raw string `body`.
    pub body: Option<String>,
    /// `openai`: parsed JSON body under `this.error`.
    pub error: Option<Value>,
    /// Bedrock: `$metadata.httpStatusCode`.
    pub metadata_http_status_code: Option<u32>,
    /// Bedrock: `$response.statusCode`.
    pub response_status_code: Option<u32>,
    /// Bedrock: `$response.body`, string or parsed object.
    pub response_body: Option<Value>,
}

/// The outer `error: unknown` split pi branches on with `instanceof Error`.
#[derive(Debug, Clone)]
pub enum ThrownError {
    /// A thrown `Error` (or SDK subclass), with its probed fields.
    Error(SdkError),
    /// A non-`Error` thrown value, stringified via `safe_json_stringify`.
    Other(Value),
}

/// Probe an SDK error object into a [`NormalizedProviderError`]
/// (`error-body.ts:38`).
pub fn normalize_provider_error(error: &ThrownError) -> NormalizedProviderError {
    let sdk = match error {
        ThrownError::Other(value) => {
            return NormalizedProviderError {
                status: None,
                body: None,
                message: safe_json_stringify(value),
                message_carries_body: false,
            };
        }
        ThrownError::Error(sdk) => sdk,
    };

    let status = extract_status(sdk);
    let body = extract_body(sdk);
    let message_carries_body = match &body {
        None => true,
        Some(body) => sdk.message.contains(body),
    };

    NormalizedProviderError {
        status,
        body,
        message: sdk.message.clone(),
        message_carries_body,
    }
}

/// Probe the HTTP status, first numeric hit wins, in SDK-field order:
/// `statusCode` (Mistral) → `status` (`openai`, `@google/genai`) →
/// `$metadata.httpStatusCode` (Bedrock) → `$response.statusCode` (Bedrock)
/// (`error-body.ts:61`).
fn extract_status(error: &SdkError) -> Option<u32> {
    error
        .status_code
        .or(error.status)
        .or(error.metadata_http_status_code)
        .or(error.response_status_code)
}

/// Probe the raw body reason, first usable hit wins; empty objects count as no
/// body, and the chosen body is trimmed and truncated (`error-body.ts:76`).
fn extract_body(error: &SdkError) -> Option<String> {
    let body_text = pick_body_text(error)?;
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

/// `body` string (Mistral) → `error` parsed JSON object (`openai`) →
/// `$response.body` string or object (Bedrock) (`error-body.ts:84`).
fn pick_body_text(error: &SdkError) -> Option<String> {
    if let Some(body) = &error.body {
        return Some(body.clone());
    }
    if is_non_empty_object(error.error.as_ref()) {
        return Some(safe_json_stringify(error.error.as_ref().unwrap()));
    }
    match &error.response_body {
        Some(Value::String(body)) => Some(body.clone()),
        response_body @ Some(_) if is_non_empty_object(response_body.as_ref()) => {
            Some(safe_json_stringify(response_body.as_ref().unwrap()))
        }
        _ => None,
    }
}

/// pi's `isNonEmptyObject`: a non-null object with at least one own key
/// (`error-body.ts:93`). Arrays are objects in JS, so a non-empty array also
/// qualifies.
fn is_non_empty_object(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Object(map)) => !map.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        _ => false,
    }
}

/// Compose a display string from a normalized error (`error-body.ts:106`).
///
/// - message already carries the body, or no body/status extracted:
///   `message`, or `"<prefix> (<status>): <message>"` when a prefix and status
///   both exist.
/// - otherwise: `"<status>: <body>"`, or `"<prefix> (<status>): <body>"`.
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        return match (prefix, norm.status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        };
    }
    let status = norm.status.expect("status checked non-none above");
    let body = norm.body.as_ref().expect("body checked non-none above");
    match prefix {
        Some(prefix) => format!("{prefix} ({status}): {body}"),
        None => format!("{status}: {body}"),
    }
}

/// Truncate `text` to `max_chars` UTF-16 code units, appending pi's suffix when
/// it was over the cap (`error-body.ts:115`).
///
/// pi measures with JS `String.length` (UTF-16 code units) and `slice`s by the
/// same unit; the port mirrors that so the cap and the reported over-cap count
/// match character-for-character.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max_chars {
        return text.to_string();
    }
    let head = String::from_utf16_lossy(&units[..max_chars]);
    let dropped = units.len() - max_chars;
    format!("{head}... [truncated {dropped} chars]")
}

/// pi's `safeJsonStringify` (`error-body.ts:120`): `JSON.stringify(value)`, or
/// `String(value)` when serialization yields `undefined` or throws.
pub fn safe_json_stringify(value: &Value) -> String {
    match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(_) => stringify_fallback(value),
    }
}

/// JS `String(value)` for the small set of shapes `safe_json_stringify` can fall
/// back on. `serde_json::to_string` only fails for maps with non-string keys,
/// which `Value` cannot represent, so this is defensive.
fn stringify_fallback(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err(message: &str) -> SdkError {
        SdkError {
            message: message.to_string(),
            ..SdkError::default()
        }
    }

    #[test]
    fn extracts_status_and_body_from_mistral_shape() {
        let error = ThrownError::Error(SdkError {
            status_code: Some(403),
            body: Some(r#"{"error":"blocked by gateway WAF"}"#.to_string()),
            ..err("Mistral request failed")
        });
        let norm = normalize_provider_error(&error);
        assert_eq!(norm.status, Some(403));
        assert_eq!(
            norm.body.as_deref(),
            Some(r#"{"error":"blocked by gateway WAF"}"#)
        );
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn reads_parsed_body_off_openai_error_when_message_opaque() {
        let error = ThrownError::Error(SdkError {
            status: Some(403),
            error: Some(json!({ "error": "blocked by gateway WAF" })),
            ..err("403 status code (no body)")
        });
        let norm = normalize_provider_error(&error);
        assert_eq!(norm.status, Some(403));
        assert_eq!(
            norm.body.as_deref(),
            Some(r#"{"error":"blocked by gateway WAF"}"#)
        );
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn preserves_message_when_google_genai_folds_body_in() {
        let body = json!({ "error": { "code": 403, "message": "Permission denied" } });
        let message = serde_json::to_string(&body).unwrap();
        let error = ThrownError::Error(SdkError {
            status: Some(403),
            ..err(&message)
        });
        let norm = normalize_provider_error(&error);
        assert_eq!(norm.status, Some(403));
        assert!(norm.message_carries_body);
        assert_eq!(norm.message, message);
    }

    #[test]
    fn extracts_status_and_body_from_bedrock_shape() {
        let error = ThrownError::Error(SdkError {
            metadata_http_status_code: Some(403),
            response_status_code: Some(403),
            response_body: Some(json!(r#"{"message":"blocked by gateway WAF"}"#)),
            ..err("UnknownError")
        });
        let norm = normalize_provider_error(&error);
        assert_eq!(norm.status, Some(403));
        assert_eq!(
            norm.body.as_deref(),
            Some(r#"{"message":"blocked by gateway WAF"}"#)
        );
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn json_stringifies_a_non_error_thrown_value() {
        let norm = normalize_provider_error(&ThrownError::Other(json!({ "reason": "boom" })));
        assert_eq!(norm.status, None);
        assert_eq!(norm.body, None);
        assert_eq!(norm.message, r#"{"reason":"boom"}"#);
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn treats_empty_parsed_body_object_as_no_body() {
        let error = ThrownError::Error(SdkError {
            status: Some(403),
            error: Some(json!({})),
            ..err("403 status code (no body)")
        });
        let norm = normalize_provider_error(&error);
        assert_eq!(norm.body, None);
        assert!(norm.message_carries_body);
    }

    #[test]
    fn truncates_the_body_at_the_cap() {
        let long_body = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
        let error = ThrownError::Error(SdkError {
            status_code: Some(500),
            body: Some(long_body.clone()),
            ..err("failed")
        });
        let norm = normalize_provider_error(&error);
        let body = norm.body.unwrap();
        assert!(body.contains("... [truncated 50 chars]"));
        assert!(body.encode_utf16().count() < long_body.encode_utf16().count());
    }

    #[test]
    fn sets_message_carries_body_when_message_already_contains_body() {
        let error = ThrownError::Error(SdkError {
            status_code: Some(500),
            body: Some("upstream exploded".to_string()),
            ..err("500: upstream exploded")
        });
        let norm = normalize_provider_error(&error);
        assert!(norm.message_carries_body);
    }

    #[test]
    fn format_surfaces_status_and_body_without_prefix() {
        let norm = normalize_provider_error(&ThrownError::Error(SdkError {
            status: Some(403),
            error: Some(json!({ "error": "blocked by gateway WAF" })),
            ..err("403 status code (no body)")
        }));
        let formatted = format_provider_error(&norm, None);
        assert!(formatted.contains("403"));
        assert!(formatted.contains("blocked by gateway WAF"));
        assert_ne!(formatted, "403 status code (no body)");
    }

    #[test]
    fn format_applies_a_provider_prefix_with_status_and_body() {
        let norm = normalize_provider_error(&ThrownError::Error(SdkError {
            status: Some(403),
            error: Some(json!({ "error": "blocked by gateway WAF" })),
            ..err("403 status code (no body)")
        }));
        assert_eq!(
            format_provider_error(&norm, Some("OpenAI API error")),
            r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#
        );
    }

    #[test]
    fn format_preserves_message_with_prefix_and_status_when_it_carries_body() {
        let body =
            serde_json::to_string(&json!({ "error": { "message": "Permission denied" } })).unwrap();
        let norm = normalize_provider_error(&ThrownError::Error(SdkError {
            status: Some(403),
            ..err(&body)
        }));
        assert_eq!(
            format_provider_error(&norm, Some("OpenAI API error")),
            format!("OpenAI API error (403): {body}")
        );
    }

    #[test]
    fn format_returns_bare_message_for_non_error_value() {
        let norm = normalize_provider_error(&ThrownError::Other(json!({ "reason": "boom" })));
        assert_eq!(format_provider_error(&norm, None), r#"{"reason":"boom"}"#);
    }

    #[test]
    fn truncate_leaves_short_text_untouched() {
        assert_eq!(truncate_error_text("short", 4000), "short");
    }
}
