//! Node-API surface for the provider-error retry classifier.
//!
//! This exposes the Rust port of pi's `packages/ai/src/utils/retry.ts`
//! (`isRetryableAssistantError`, ported bit-exactly in
//! [`pidgin_ai::is_retryable_assistant_error`]) to pi's `packages/ai`
//! `retry.test.ts`. The Rust module owns the entire decision: the ordered
//! non-retryable-limit-vs-retryable pattern tables and the "stop reason must be
//! `error` and carry a non-empty message" gate.
//!
//! # The seam: a whole `AssistantMessage` crosses as JSON
//!
//! pi's `isRetryableAssistantError(message: AssistantMessage)` reads a plain,
//! fully-serializable value — no closures, streams, or live object identity — so
//! the shim marshals it honestly with `JSON.stringify` and this function
//! deserializes the complete [`pidgin_ai::AssistantMessage`] back before calling
//! the ported classifier. The round-trip is faithful: pi's `fauxAssistantMessage`
//! output (the shape the test builds) is a strict match for the Rust struct —
//! `role`/`content`/`api`/`provider`/`model`/`usage`/`stopReason`/`errorMessage`/
//! `timestamp`, with `undefined` optionals dropped by `JSON.stringify` and read
//! back as absent. No field is faked or projected away; the classifier receives
//! exactly the message the caller passed.

use napi_derive::napi;

use pidgin_ai::{is_retryable_assistant_error, AssistantMessage};

/// pi's `isRetryableAssistantError`. Takes a JSON-stringified `AssistantMessage`,
/// deserializes the whole message, and returns whether it looks like a transient
/// provider/transport error worth retrying. Every classification decision (the
/// ordered non-retryable/retryable pattern tables, the `stopReason === "error"`
/// and non-empty `errorMessage` gate) runs in Rust.
#[napi(js_name = "isRetryableAssistantError")]
pub fn is_retryable_assistant_error_native(error_json: String) -> napi::Result<bool> {
    let message: AssistantMessage = serde_json::from_str(&error_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid assistant message: {err}")))?;
    Ok(is_retryable_assistant_error(&message))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact JSON pi's `fauxAssistantMessage` serializes to (with `undefined`
    /// optionals dropped by `JSON.stringify`), proving the whole message
    /// round-trips through the addon boundary. `error_message: None` omits the
    /// key, mirroring the `JSON.stringify` drop for `fauxAssistantMessage`'s
    /// unset `errorMessage`.
    fn faux_message_json(content: &str, stop_reason: &str, error_message: Option<&str>) -> String {
        let mut message = serde_json::json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": content }],
            "api": "faux",
            "provider": "faux",
            "model": "faux-1",
            "usage": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": stop_reason,
            "timestamp": 0,
        });
        if let Some(error_message) = error_message {
            message["errorMessage"] = serde_json::json!(error_message);
        }
        message.to_string()
    }

    /// pi's `fauxAssistantMessage("", { stopReason: "error", errorMessage })`.
    fn error_message_json(error_message: &str) -> String {
        faux_message_json("", "error", Some(error_message))
    }

    #[test]
    fn round_trips_and_classifies_retryable_error() {
        assert!(
            is_retryable_assistant_error_native(error_message_json("overloaded_error")).unwrap()
        );
        assert!(is_retryable_assistant_error_native(error_message_json(
            "524 status code (no body)"
        ))
        .unwrap());
    }

    #[test]
    fn keeps_provider_limit_errors_non_retryable() {
        assert!(
            !is_retryable_assistant_error_native(error_message_json("429 quota exceeded")).unwrap()
        );
    }

    #[test]
    fn non_error_message_is_not_retryable() {
        // pi's `fauxAssistantMessage("not an error")`: default `stop` reason, a
        // text block, and no error message (dropped by `JSON.stringify`).
        let json = faux_message_json("not an error", "stop", None);
        assert!(!is_retryable_assistant_error_native(json).unwrap());
    }
}
