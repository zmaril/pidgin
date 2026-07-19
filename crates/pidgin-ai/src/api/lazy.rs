//! Lazy stream construction, ported from pi-ai's `packages/ai/src/api/lazy.ts`
//! at pinned commit `3da591ab`.
//!
//! pi's `lazyStream` returns an event stream *synchronously* while an
//! async `setup()` (auth resolution, dynamic module import) runs behind it:
//! setup successes forward the underlying stream, setup failures terminate the
//! returned stream with a single `error` event and an error [`AssistantMessage`]
//! built by [`create_setup_error_message`] (pi's `createSetupErrorMessage`,
//! `lazy.ts:4`).
//!
//! # Deviation: eager, not deferred (pi anchor `lazy.ts:46-60`)
//!
//! pi builds the outer stream, kicks off `setup().then(forward).catch(errorify)`
//! on a background microtask, and returns the still-empty stream immediately;
//! consumers `await` events as setup resolves. pidgin-ai has no async runtime
//! (the crate depends only on `serde`/`serde_json`), and the whole crate re-presents
//! pi's async streaming *eagerly*: the Stage-2 wire parsers and the provider seam
//! (`seams/provider.rs`) return their event sequence as a value, and scheduling is
//! re-added at the FFI boundary. This port follows that convention — [`lazy_stream`]
//! runs `setup` inline and returns a fully-populated
//! [`AssistantMessageEventStream`]. The observable *contents* of the returned
//! stream (event order, the single-`error`-then-terminal failure shape, the
//! captured terminal result) are identical to pi's; only the moment of population
//! moves from "as consumed" to "before return", exactly as elsewhere in the crate.
//!
//! # Deviation: `lazyApi` omitted (pi anchor `lazy.ts:68-77`)
//!
//! pi's `lazyApi` wraps a `() => Promise<ProviderStreams>` dynamic `import()` as a
//! `ProviderStreams`, deferring module load to the first `stream`/`streamSimple`
//! call and relying on the JS host's import cache to deduplicate. Rust has no
//! dynamic-import primitive and the provider seam ([`crate::seams::provider::Provider`])
//! is a synchronous trait returning an eager [`crate::seams::provider::StreamResult`],
//! so the lazy-load-on-first-call shape does not translate; [`lazy_stream`] is the
//! portable primitive and is ported here. A future provider-registration layer that
//! needs deferred construction can build on [`lazy_stream`] directly.

// straitjacket-allow-file:duplication

use std::fmt;

use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Model, StopReason, Usage, UsageCost,
};
use crate::utils::event_stream::{
    create_assistant_message_event_stream, AssistantMessageEventStream,
};

/// Zeroed [`Usage`], mirroring pi's inline `usage` literal in
/// `createSetupErrorMessage` (`lazy.ts:11-18`).
///
// Duplicated from `providers/registry.rs::zero_usage` (a module-private helper
// there); this file's `straitjacket-allow-file:duplication` marker records that
// the repetition is a faithful mirror of pi's per-call-site zero-usage literal.
fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

/// Builds the terminal error [`AssistantMessage`] for a setup failure — pi's
/// `createSetupErrorMessage` (`lazy.ts:4-22`).
///
/// The message is an error-as-value: empty content, zeroed usage, `stop_reason`
/// = `error`, and `error_message` set from the failure. pi stringifies with
/// `error instanceof Error ? error.message : String(error)`; the Rust analog is
/// `E: Display`, whose `to_string()` renders both an error's message and any
/// other value's string form.
///
// DEVIATION: pi sets `timestamp: Date.now()` (`lazy.ts:20`). This module is
// pure-sync and takes no clock; mirroring the sibling eager-error constructor in
// `providers/registry.rs::error_result`, the timestamp is `0`. Callers that need
// wall-clock stamping apply it at the boundary that owns the clock seam.
fn create_setup_error_message<E: fmt::Display>(model: &Model, error: &E) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(error.to_string()),
        timestamp: 0,
    }
}

/// Drains `source` into `target`, then finalizes `target` with `source`'s
/// captured terminal result — pi's `forwardStream` (`lazy.ts:30-37`).
///
/// pi does `for await (const event of source) target.push(event)` followed by
/// `target.end(hasResult(source) ? await source.result() : undefined)`. Here the
/// inner [`AssistantMessageEventStream`] is drained in FIFO order and its
/// [`result`](AssistantMessageEventStream::result) (pi's `.result()`, always
/// present on this stream type) is handed to
/// [`end`](AssistantMessageEventStream::end).
fn forward_stream(
    target: &mut AssistantMessageEventStream,
    mut source: AssistantMessageEventStream,
) {
    while let Some(event) = source.next() {
        target.push(event);
    }
    target.end(source.result().cloned());
}

/// Returns a stream synchronously while running `setup` behind it — pi's
/// `lazyStream` (`lazy.ts:46-60`).
///
/// On setup success the underlying stream is forwarded verbatim into the
/// returned stream (events plus the captured terminal result). On setup failure
/// the returned stream carries exactly one `error` event and ends with the error
/// [`AssistantMessage`] from [`create_setup_error_message`] — pi's
/// `outer.push({ type: "error", reason: "error", error: message })` then
/// `outer.end(message)`.
///
/// `setup` returns `Result<`[`AssistantMessageEventStream`]`, E>`: `Ok(inner)`
/// mirrors pi's resolved `Promise<AsyncIterable<AssistantMessageEvent>>`, and
/// `Err(error)` mirrors the rejected promise caught by pi's `.catch`. See the
/// module docs for the eager-vs-deferred deviation.
pub fn lazy_stream<F, E>(model: &Model, setup: F) -> AssistantMessageEventStream
where
    F: FnOnce() -> Result<AssistantMessageEventStream, E>,
    E: fmt::Display,
{
    let mut outer = create_assistant_message_event_stream();
    match setup() {
        Ok(inner) => forward_stream(&mut outer, inner),
        Err(error) => {
            let message = create_setup_error_message(model, &error);
            outer.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: message.clone(),
            });
            outer.end(Some(Err(message)));
        }
    }
    outer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, Modality, ModelCost};

    fn test_model() -> Model {
        Model {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            api: "test-api".to_string(),
            provider: "test-provider".to_string(),
            base_url: "https://example.test/v1".to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 10_000,
            max_tokens: 1_000,
            headers: None,
            compat: None,
        }
    }

    fn success_message() -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![ContentBlock::Text {
                text: "ok".to_string(),
                text_signature: None,
            }],
            api: "test-api".to_string(),
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: zero_usage(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    /// Builds an already-populated inner stream: a `start` event followed by a
    /// `done` terminal carrying `message`.
    fn inner_stream(message: AssistantMessage) -> AssistantMessageEventStream {
        let mut inner = create_assistant_message_event_stream();
        inner.push(AssistantMessageEvent::Start {
            partial: message.clone(),
        });
        inner.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message,
        });
        inner
    }

    #[test]
    fn passing_setup_forwards_the_underlying_stream() {
        let model = test_model();
        let message = success_message();
        let mut stream = lazy_stream(&model, || Ok::<_, String>(inner_stream(message.clone())));

        // The underlying stream's events are forwarded verbatim, in order.
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Start { .. })
        ));
        match stream.next() {
            Some(AssistantMessageEvent::Done { reason, message: m }) => {
                assert_eq!(reason, StopReason::Stop);
                assert_eq!(m, message);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(stream.next().is_none());

        // The terminal result is the underlying stream's success message.
        assert!(stream.is_done());
        match stream.result() {
            Some(Ok(captured)) => assert_eq!(captured, &message),
            other => panic!("expected Ok(message), got {other:?}"),
        }
    }

    #[test]
    fn failing_setup_yields_one_error_event_and_error_terminal() {
        let model = test_model();
        let mut stream = lazy_stream(&model, || {
            Err::<AssistantMessageEventStream, _>("boom: auth failed")
        });

        // Exactly one event, and it is the terminal `error`.
        let error_message = match stream.next() {
            Some(AssistantMessageEvent::Error { reason, error }) => {
                assert_eq!(reason, StopReason::Error);
                error
            }
            other => panic!("expected a single Error event, got {other:?}"),
        };
        assert!(stream.next().is_none(), "expected exactly one event");

        // The error message is built by the setup-error helper: empty content,
        // zeroed usage, error stop reason, the failure string, and the model's
        // identity carried through.
        assert!(error_message.content.is_empty());
        assert_eq!(error_message.usage, zero_usage());
        assert_eq!(error_message.stop_reason, StopReason::Error);
        assert_eq!(
            error_message.error_message.as_deref(),
            Some("boom: auth failed")
        );
        assert_eq!(error_message.api, "test-api");
        assert_eq!(error_message.provider, "test-provider");
        assert_eq!(error_message.model, "test-model");

        // The captured terminal result is the same error message, as a value.
        assert!(stream.is_done());
        match stream.result() {
            Some(Err(captured)) => assert_eq!(captured, &error_message),
            other => panic!("expected Err(error), got {other:?}"),
        }
    }

    #[test]
    fn setup_error_uses_display_of_the_error_value() {
        // A non-`Error`-like value stringifies via `Display`, mirroring pi's
        // `String(error)` fallback.
        let model = test_model();
        let message = create_setup_error_message(&model, &42_u32);
        assert_eq!(message.error_message.as_deref(), Some("42"));
    }
}
