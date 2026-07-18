// straitjacket-allow-file[:duplication] — the two provider-error pattern tables
// and the per-case classification tests are a byte-faithful transcription of
// pi's `retry.ts` pattern sets and `retry.test.ts` cases; the repeated pattern
// literals and parallel `assert!` bodies read as duplicates but are the port's
// load-bearing fidelity surface.
//! Provider-error retry classification, ported from pi-ai's
//! `packages/ai/src/utils/retry.ts` at pinned commit `3da591ab`.
//!
//! This is pure classification: given a failed [`AssistantMessage`], decide
//! whether it looks like a transient provider/transport error worth retrying.
//! It implements NO retry policy — no backoff, no caps, no budget — exactly like
//! pi's `retry.ts`. Callers layer their own policy on top.
//!
//! Two case-insensitive alternation patterns drive the decision, transcribed
//! verbatim from pi:
//!
//! - [`NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN`] — subscription/quota/billing
//!   limits that will not clear on retry.
//! - [`RETRYABLE_PROVIDER_ERROR_PATTERN`] — transient load, HTTP 5xx/429,
//!   transport, and premature-stream failures.
//!
//! The non-retryable pattern is checked FIRST, so a message that matches both
//! (e.g. `"429 quota exceeded"` — `429` is retryable, `quota exceeded` is not)
//! is classified non-retryable. Order matters.

use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

use crate::types::{AssistantMessage, StopReason};

/// OpenCode/quota/billing limits that a retry cannot clear (pi's
/// `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN`, `retry.ts:6-24`).
const NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN: &[&str] = &[
    // OpenCode Go/free-tier limits returned as 429 JSON error types by OpenCode's
    // Zen API. These are subscription/account limits, not transient throttles.
    "GoUsageLimitError",
    "FreeUsageLimitError",
    // OpenCode Go subscription-limit text asks users to enable available-balance
    // usage after rolling/weekly/monthly limits are reached.
    "Monthly usage limit reached",
    "available balance",
    // Generic quota/budget/billing exhaustion. `insufficient_quota` is OpenAI's
    // quota/billing error code; the other strings cover common gateway wording.
    "insufficient_quota",
    "out of budget",
    "quota exceeded",
    "billing",
];

/// Transient provider/transport failures worth retrying (pi's
/// `RETRYABLE_PROVIDER_ERROR_PATTERN`, `retry.ts:26-88`).
const RETRYABLE_PROVIDER_ERROR_PATTERN: &[&str] = &[
    // Generic provider load, HTTP status, and server-side transient failures.
    "overloaded",
    "rate.?limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "524",
    "service.?unavailable",
    "server.?error",
    "internal.?error",
    // Wrapper/provider text for transient upstream failures, including OpenRouter
    // "Provider returned error" responses (#2264).
    "provider.?returned.?error",
    // Network, proxy, and fetch transport failures. This includes OpenAI Codex
    // raw-fetch failures such as "upstream connect", "connection refused", and
    // "reset before headers" (#733), plus OpenRouter connection drops (#3317).
    "network.?error",
    "connection.?error",
    "connection.?refused",
    "connection.?lost",
    "other side closed",
    "fetch failed",
    "upstream.?connect",
    "reset before headers",
    "socket hang up",
    "socket connection was closed",
    "timed? out",
    "timeout",
    "terminated",
    // WebSocket transports can report close/error text instead of HTTP/fetch text.
    "websocket.?closed",
    "websocket.?error",
    // Premature stream endings from SDKs and transports. Anthropic can throw
    // "stream ended without ..." and "Anthropic stream ended before message_stop"
    // (#4433); Bedrock/Smithy can throw an HTTP/2 no-response error (#3594).
    "ended without",
    "stream ended before message_stop",
    "stream ended before a terminal response event",
    "http2 request did not get a response",
    // Provider-requested retry delay cap failures should flow through the outer
    // retry policy so callers can surface/abort the backoff (#1123).
    "retry delay",
    // Explicit retry guidance emitted mid-stream by OpenAI Responses and Bedrock
    // stream exceptions (#6019).
    "you can retry your request",
    "try your request again",
    "please retry your request",
    // gRPC based providers (e.g. NVIDIA NIM)
    "ResourceExhausted",
];

/// Builds a case-insensitive alternation of `patterns` (pi's
/// `buildProviderErrorPattern`, `retry.ts:3-5`, which joins on `|` with the `i`
/// flag).
fn build_provider_error_pattern(patterns: &[&str]) -> Regex {
    RegexBuilder::new(&patterns.join("|"))
        .case_insensitive(true)
        .build()
        .expect("provider error patterns compile")
}

fn non_retryable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| build_provider_error_pattern(NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN))
}

fn retryable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| build_provider_error_pattern(RETRYABLE_PROVIDER_ERROR_PATTERN))
}

/// Classifies whether a failed assistant message looks like a transient provider
/// or transport error (pi's `isRetryableAssistantError`, `retry.ts:99-104`).
///
/// Returns `false` unless the message's stop reason is `error` AND it carries a
/// non-empty error message. If the non-retryable limit pattern matches (checked
/// first), returns `false`. Otherwise returns whether the retryable pattern
/// matches.
///
/// This does not implement retry policy: callers should first handle context
/// overflow separately, then apply their own retry budget, backoff, and
/// reporting before restarting the assistant turn.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    // pi: `if (message.stopReason !== "error" || !message.errorMessage) return false;`
    // `!errorMessage` is JS-falsy for both `undefined` and the empty string.
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error_message) = message
        .error_message
        .as_deref()
        .filter(|message| !message.is_empty())
    else {
        return false;
    };
    if non_retryable_pattern().is_match(error_message) {
        return false;
    }
    retryable_pattern().is_match(error_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::faux::{faux_assistant_message, faux_text, FauxAssistantOptions};

    // Mirrors the fixture strings at the top of pi's `retry.test.ts`.
    const OPENAI_EXPLICIT_RETRY_MESSAGE: &str = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.";
    const BEDROCK_EXPLICIT_RETRY_MESSAGE: &str =
        "{\"message\":\"The system encountered an unexpected error during processing. Try your request again.\"}";
    const NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE: &str =
        "ResourceExhausted: Worker local total request limit reached (288/48)";
    const BUN_FETCH_SOCKET_CLOSED_MESSAGE: &str =
        "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()";
    const OPENAI_RESPONSES_EARLY_EOF_MESSAGE: &str =
        "OpenAI Responses stream ended before a terminal response event";

    /// pi's `fauxAssistantMessage("", { stopReason: "error", errorMessage })`.
    fn error_message(error_message: &str) -> AssistantMessage {
        faux_assistant_message(
            vec![],
            FauxAssistantOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(error_message.to_string()),
                ..Default::default()
            },
            0,
        )
    }

    #[test]
    fn matches_explicit_provider_retry_guidance() {
        assert!(is_retryable_assistant_error(&error_message(
            OPENAI_EXPLICIT_RETRY_MESSAGE
        )));
        assert!(is_retryable_assistant_error(&error_message(
            BEDROCK_EXPLICIT_RETRY_MESSAGE
        )));
        assert!(is_retryable_assistant_error(&error_message(
            NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE
        )));
    }

    #[test]
    fn matches_bun_fetch_socket_drop_wording() {
        assert!(is_retryable_assistant_error(&error_message(
            BUN_FETCH_SOCKET_CLOSED_MESSAGE
        )));
    }

    #[test]
    fn matches_openai_responses_streams_that_end_before_terminal_events() {
        assert!(is_retryable_assistant_error(&error_message(
            OPENAI_RESPONSES_EARLY_EOF_MESSAGE
        )));
    }

    #[test]
    fn keeps_provider_limit_errors_non_retryable() {
        // The non-retryable `quota exceeded` wins over the retryable `429`,
        // proving the non-retryable pattern is checked first.
        assert!(!is_retryable_assistant_error(&error_message(
            "429 quota exceeded"
        )));
    }

    #[test]
    fn classifies_assistant_error_messages() {
        assert!(is_retryable_assistant_error(&error_message(
            "overloaded_error"
        )));
        assert!(is_retryable_assistant_error(&error_message(
            "524 status code (no body)"
        )));
        // A non-error message (default `stop` reason, no error text) is not
        // retryable, mirroring pi's `fauxAssistantMessage("not an error")`.
        assert!(!is_retryable_assistant_error(&faux_assistant_message(
            vec![faux_text("not an error")],
            FauxAssistantOptions::default(),
            0,
        )));
    }
}
