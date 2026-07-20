//! Context-window overflow detection, ported from pi-ai's
//! `packages/ai/src/utils/overflow.ts` at pinned commit `3da591ab`.
//!
//! [`is_context_overflow`] decides whether an assistant message represents the
//! prompt exceeding the model's context window. It handles three cases:
//!
//! 1. **Error-based overflow** — a `stopReason: "error"` whose `errorMessage`
//!    matches one of the provider-specific [`OVERFLOW_PATTERNS`] and none of the
//!    [`NON_OVERFLOW_PATTERNS`] (rate-limit / throttling exclusions that would
//!    otherwise false-positive on `too many tokens`).
//! 2. **Silent overflow (z.ai style)** — a successful `stopReason: "stop"` whose
//!    `input + cacheRead` exceeds a supplied `context_window`.
//! 3. **Length-stop overflow (Xiaomi MiMo style)** — `stopReason: "length"` with
//!    zero output and `input + cacheRead` filling at least 99% of the context
//!    window (the server truncated oversized input, leaving no room to generate).
//!
//! # Parity notes
//!
//! Every regex is transcribed verbatim from pi and compiled case-insensitively
//! with [`regex::RegexBuilder`] (pi's `/…/i`), once, behind a [`OnceLock`]. Rust
//! `regex` has no lookaround, but none of these patterns use it. `^` anchors to
//! the start of the haystack in single-line mode, matching JS without the `m`
//! flag.

use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

use crate::types::{AssistantMessage, StopReason};

/// The verbatim overflow-pattern sources (`overflow.ts:36`). Order is preserved
/// for `get_overflow_patterns` even though matching is order-independent.
const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"prompt is too long",                    // Anthropic token overflow
    r"request_too_large",                     // Anthropic request byte-size overflow (HTTP 413)
    r"input is too long for requested model", // Amazon Bedrock
    r"exceeds the context window",            // OpenAI (Completions & Responses API)
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // OpenAI-compatible proxies (LiteLLM)
    r"input token count.*exceeds the maximum", // Google (Gemini)
    r"maximum prompt length is \d+",           // xAI (Grok)
    r"reduce the length of the messages",      // Groq
    r"maximum context length is \d+ tokens",   // OpenRouter (most backends)
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter/Poolside
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together AI
    r"exceeds the limit of \d+",           // GitHub Copilot
    r"exceeds the available context size", // llama.cpp server
    r"greater than the context length",    // LM Studio
    r"context window exceeds limit",       // MiniMax
    r"exceeded model token limit",         // Kimi For Coding
    r"too large for model with \d+ maximum context length", // Mistral
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?", // DS4 server
    r"model_context_window_exceeded",      // z.ai error text
    r"prompt too long; exceeded (?:max )?context length", // Ollama explicit overflow error
    r"context[_ ]length[_ ]exceeded",      // Generic fallback
    r"too many tokens",                    // Generic fallback
    r"token limit exceeded",               // Generic fallback
    r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)", // Cerebras: 400/413 with no body
];

/// The verbatim non-overflow exclusion sources (`overflow.ts:72`).
const NON_OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"^(Throttling error|Service unavailable):", // AWS Bedrock non-overflow errors
    r"rate limit",                               // Generic rate limiting
    r"too many requests",                        // Generic HTTP 429 style
];

fn compile(sources: &[&str]) -> Vec<Regex> {
    sources
        .iter()
        .map(|source| {
            RegexBuilder::new(source)
                .case_insensitive(true)
                .build()
                .expect("overflow pattern is a valid regex")
        })
        .collect()
}

fn overflow_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| compile(OVERFLOW_PATTERN_SOURCES))
}

fn non_overflow_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| compile(NON_OVERFLOW_PATTERN_SOURCES))
}

/// Check whether an assistant message indicates a context overflow
/// (`overflow.ts:129`).
///
/// `context_window` is optional: it is required to detect the silent (case 2)
/// and length-stop (case 3) overflows, which have no error message.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    // Case 1: error message patterns.
    if message.stop_reason == StopReason::Error {
        if let Some(error_message) = &message.error_message {
            let is_non_overflow = non_overflow_patterns()
                .iter()
                .any(|pattern| pattern.is_match(error_message));
            if !is_non_overflow
                && overflow_patterns()
                    .iter()
                    .any(|pattern| pattern.is_match(error_message))
            {
                return true;
            }
        }
    }

    // Case 2: silent overflow (z.ai style) — successful but input exceeds window.
    if let Some(context_window) = context_window {
        if message.stop_reason == StopReason::Stop {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens > context_window {
                return true;
            }
        }

        // Case 3: length-stop overflow (Xiaomi MiMo style) — server truncated
        // oversized input to fill the window, leaving no room for output.
        if message.stop_reason == StopReason::Length && message.usage.output == 0 {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens as f64 >= context_window as f64 * 0.99 {
                return true;
            }
        }
    }

    false
}

/// Get the overflow patterns, for testing (`overflow.ts:163`).
pub fn get_overflow_patterns() -> &'static [Regex] {
    overflow_patterns()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Usage, UsageCost};

    fn usage(input: u64, cache_read: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            cache_read,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: input + cache_read + output,
            cost: UsageCost::default(),
        }
    }

    fn error_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            role: crate::types::AssistantRole::Assistant,
            content: vec![],
            api: "openai-completions".into(),
            provider: "ollama".into(),
            model: "qwen3.5:35b".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(0, 0, 0),
            stop_reason: StopReason::Error,
            error_message: Some(text.to_string()),
            timestamp: 0,
        }
    }

    fn length_stop(input: u64, cache_read: u64, output: u64) -> AssistantMessage {
        AssistantMessage {
            role: crate::types::AssistantRole::Assistant,
            content: vec![],
            api: "openai-completions".into(),
            provider: "xiaomi".into(),
            model: "mimo-v2.5-pro".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(input, cache_read, output),
            stop_reason: StopReason::Length,
            error_message: None,
            timestamp: 0,
        }
    }

    #[test]
    fn detects_explicit_ollama_prompt_too_long() {
        let m =
            error_message("400 `prompt too long; exceeded max context length by 100918 tokens`");
        assert!(is_context_overflow(&m, Some(32768)));
    }

    #[test]
    fn detects_together_ai_context_length() {
        let m = error_message(
            "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
        );
        assert!(is_context_overflow(&m, Some(262144)));
    }

    #[test]
    fn detects_litellm_wrapped_openai_maximum_context_length() {
        let m = error_message(
            "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
        );
        assert!(is_context_overflow(&m, Some(131072)));
    }

    #[test]
    fn detects_parenthesized_maximum_context_length() {
        let m = error_message(
            "Error: 400 Input length (265330) exceeds model's maximum context length (262144).",
        );
        assert!(is_context_overflow(&m, Some(262144)));
    }

    #[test]
    fn detects_openrouter_poolside_maximum_allowed_input_length() {
        let m = error_message(
            "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
        );
        assert!(is_context_overflow(&m, Some(131072)));
    }

    #[test]
    fn detects_ds4_configured_context_size_with_commas() {
        let plain = error_message(
            "400 Prompt has 256468 tokens, but the configured context size is 256000 tokens",
        );
        assert!(is_context_overflow(&plain, Some(256000)));
        let comma = error_message(
            "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
        );
        assert!(is_context_overflow(&comma, Some(256000)));
    }

    #[test]
    fn ignores_generic_non_overflow_errors() {
        let m = error_message("500 `model runner crashed unexpectedly`");
        assert!(!is_context_overflow(&m, Some(32768)));
    }

    #[test]
    fn excludes_bedrock_throttling_too_many_tokens() {
        let m =
            error_message("Throttling error: Too many tokens, please wait before trying again.");
        assert!(!is_context_overflow(&m, Some(200000)));
    }

    #[test]
    fn excludes_bedrock_service_unavailable() {
        let m = error_message("Service unavailable: The service is temporarily unavailable.");
        assert!(!is_context_overflow(&m, Some(200000)));
    }

    #[test]
    fn excludes_generic_rate_limit() {
        let m = error_message("Rate limit exceeded, please retry after 30 seconds.");
        assert!(!is_context_overflow(&m, Some(200000)));
    }

    #[test]
    fn excludes_too_many_requests() {
        let m = error_message("Too many requests. Please slow down.");
        assert!(!is_context_overflow(&m, Some(200000)));
    }

    #[test]
    fn detects_xiaomi_length_stop_overflow() {
        let m = length_stop(58, 1_048_512, 0);
        assert!(is_context_overflow(&m, Some(1_048_576)));
    }

    #[test]
    fn ignores_normal_length_stop_with_output() {
        let m = length_stop(1000, 0, 4096);
        assert!(!is_context_overflow(&m, Some(200000)));
    }

    #[test]
    fn ignores_length_stop_far_below_context() {
        let m = length_stop(100, 0, 0);
        assert!(!is_context_overflow(&m, Some(200000)));
    }

    #[test]
    fn detects_silent_overflow_via_context_window() {
        let mut m = length_stop(0, 0, 5);
        m.stop_reason = StopReason::Stop;
        m.usage = usage(300000, 0, 5);
        assert!(is_context_overflow(&m, Some(200000)));
        // Without a context window the silent overflow is undetectable.
        assert!(!is_context_overflow(&m, None));
    }

    #[test]
    fn cerebras_no_body_anchored_pattern() {
        assert!(is_context_overflow(
            &error_message("400 (no body)"),
            Some(1000)
        ));
        assert!(is_context_overflow(
            &error_message("413 status code (no body)"),
            Some(1000)
        ));
        // The pattern is anchored at start: a leading prefix should not match.
        assert!(!is_context_overflow(
            &error_message("Error: 400 (no body)"),
            Some(1000)
        ));
    }

    #[test]
    fn get_overflow_patterns_returns_all_sources() {
        assert_eq!(
            get_overflow_patterns().len(),
            OVERFLOW_PATTERN_SOURCES.len()
        );
    }
}
