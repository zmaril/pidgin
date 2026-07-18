// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` `stream` and `streamSimple` drivers. The output-shell
// construction and the thinking-mode branch selection mirror pi's object
// literals and `if` ladder verbatim; the clone detector may read the shared
// AssistantMessage scaffolding as duplicative by design.
//! The Anthropic Messages stream drivers, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` `stream` / `streamSimple` at
//! pinned commit `3da591ab`.
//!
//! [`stream`] is the buffered, seam-targeted analogue of pi's `stream()`: it
//! picks the auth mode, builds the request body ([`build_params`]), assembles the
//! [`HttpRequest`] ([`assemble_request`]), performs it over the injected
//! [`HttpTransport`], and feeds the response body into the already-ported SSE
//! parser ([`parse_sse_stream`]), returning the eager [`StreamResult`].
//!
//! # Streaming model
//!
//! pi's `stream()` is asynchronous and pushes events into an
//! `AssistantMessageEventStream` as SSE chunks arrive. The Rust core (matching
//! the Stage-2 parser and the [`Provider`](crate::seams::provider::Provider)
//! seam) produces the whole event sequence eagerly from a fully-buffered
//! response body; inter-chunk timing is re-presented at the binding edge. This is
//! exactly the shape pi's fixture tests exercise — they stub `fetch` to return a
//! canned `Response` whose complete SSE body is then parsed.
//!
//! # Error surfacing
//!
//! pi encodes post-start failures as a terminal `error` event. Failures that
//! occur before the stream starts (missing auth, a non-2xx create, a transport
//! error) throw in pi and are caught by the same handler, which pushes a single
//! `error` event with no preceding `start`. The buffered driver reproduces that:
//! a truncated 200 body surfaces the parser's
//! `"Anthropic stream ended before message_stop"`; a non-2xx status, a transport
//! error, or a missing credential yield an error-only [`StreamResult`].

use crate::seams::http::HttpTransport;
use crate::seams::provider::StreamResult;
use crate::types::{
    AnthropicMessagesCompat, AssistantMessage, AssistantMessageEvent, AssistantRole,
    CacheRetention, Context, Model, StopReason, Usage, UsageCost,
};

use super::cache::resolve_cache_retention;
use super::client::{
    assemble_request, assert_request_auth, resolve_auth_mode, serialize_body, AuthMode,
};
use super::request::{build_params, AnthropicOptions};
use super::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
    SimpleStreamOptions,
};
use super::thinking::map_thinking_level_to_effort;
use super::{parse_sse_stream, AnthropicModel};

/// Build the lean [`AnthropicModel`] the SSE parser needs (identity + pricing)
/// from a full boundary model.
fn anthropic_model(model: &Model<AnthropicMessagesCompat>) -> AnthropicModel {
    AnthropicModel {
        id: model.id.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        cost: model.cost.clone(),
    }
}

/// The empty assistant output shell pi seeds before streaming
/// (`anthropic-messages.ts:491`).
fn empty_output(model: &Model<AnthropicMessagesCompat>, timestamp: i64) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp,
    }
}

/// A single-`error`-event result, matching pi's catch handler for a failure that
/// occurs before the stream's `start` event (`anthropic-messages.ts:744`).
fn error_result(
    model: &Model<AnthropicMessagesCompat>,
    timestamp: i64,
    message: String,
) -> StreamResult {
    let mut output = empty_output(model, timestamp);
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message);
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: output.clone(),
        }],
        message: output,
    }
}

/// Stream a response for `model` over the injected `transport`, mirroring pi's
/// `stream()` request assembly and SSE handling. `timestamp` is the message
/// timestamp pi sets via `Date.now()` (threaded here for determinism, as the SSE
/// parser already is).
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    options: &AnthropicOptions,
    timestamp: i64,
) -> StreamResult {
    let api_key = options.api_key.as_deref();

    // pi asserts auth before constructing the client; a failure throws and is
    // caught as an error event.
    if let Err(message) = assert_request_auth(&model.provider, api_key, options.headers.as_ref()) {
        return error_result(model, timestamp, message);
    }

    let mode = resolve_auth_mode(&model.provider, api_key);
    let is_oauth = matches!(mode, AuthMode::OAuth);

    // pi: cacheSessionId = cacheRetention === "none" ? undefined : options.sessionId.
    let cache_retention = resolve_cache_retention(options.cache_retention, options.env.as_ref());
    let session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.session_id.as_deref()
    };
    let interleaved_thinking = options.interleaved_thinking.unwrap_or(true);

    // build_params already sets `stream: true`.
    let body = build_params(model, context, is_oauth, options);
    let request = assemble_request(
        mode,
        model,
        context,
        serialize_body(&body),
        api_key,
        options.headers.as_ref(),
        interleaved_thinking,
        session_id,
    );

    match transport.send(&request) {
        Ok(response) if response.is_ok() => {
            let outcome =
                parse_sse_stream(&response.body, &anthropic_model(model), is_oauth, timestamp);
            StreamResult {
                events: outcome.events,
                message: outcome.message,
            }
        }
        Ok(response) => error_result(
            model,
            timestamp,
            format!("Anthropic request failed with status {}", response.status),
        ),
        Err(error) => error_result(model, timestamp, error.to_string()),
    }
}

/// Stream a response from the simple, level-based options, mirroring pi's
/// `streamSimple()` (`anthropic-messages.ts:786`). It maps `reasoning` onto the
/// adaptive-effort or budget-based thinking configuration and delegates to
/// [`stream`].
pub fn stream_simple<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    options: &SimpleStreamOptions,
    timestamp: i64,
) -> StreamResult {
    // pi asserts auth synchronously at the top of streamSimple; encoded here as
    // an error result to keep the function total.
    if let Err(message) = assert_request_auth(
        &model.provider,
        options.api_key.as_deref(),
        options.headers.as_ref(),
    ) {
        return error_result(model, timestamp, message);
    }

    let base = build_base_options(model, context, options);

    let Some(reasoning) = options.reasoning else {
        // No reasoning requested: thinking explicitly disabled.
        let opts = AnthropicOptions {
            thinking_enabled: Some(false),
            ..base
        };
        return stream(transport, model, context, &opts, timestamp);
    };

    let force_adaptive = model
        .compat
        .as_ref()
        .and_then(|c| c.force_adaptive_thinking)
        .unwrap_or(false);

    if force_adaptive {
        // Adaptive-thinking models: use an effort level.
        let effort = map_thinking_level_to_effort(model, Some(reasoning));
        let opts = AnthropicOptions {
            thinking_enabled: Some(true),
            effort: Some(effort),
            ..base
        };
        return stream(transport, model, context, &opts, timestamp);
    }

    // Older models: budget-based thinking.
    let adjusted = adjust_max_tokens_for_thinking(
        base.max_tokens,
        model.max_tokens,
        reasoning,
        options.thinking_budgets.as_ref(),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
    let thinking_budget = adjusted
        .thinking_budget
        .min(max_tokens.saturating_sub(1024));
    let opts = AnthropicOptions {
        max_tokens: Some(max_tokens),
        thinking_enabled: Some(true),
        thinking_budget_tokens: Some(thinking_budget),
        ..base
    };
    stream(transport, model, context, &opts, timestamp)
}
