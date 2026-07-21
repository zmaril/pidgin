//! The provider seam: the model/streaming interface faux and real providers
//! implement.
//!
//! # What this abstracts in pi
//!
//! pi routes every model call through a provider's `stream()` /`streamSimple()`
//! functions (`Provider`/`ApiProvider` in `compat.ts`), and its tests swap the
//! whole provider out with `registerFauxProvider()` — a scripted, deterministic
//! provider that drives pi's real streaming path. The mock-seam inventory
//! (`notes/mock-inventory.md`) attributes 22 collaborator sites to this seam: the
//! agent and coding-agent suites register a faux provider and assert on the event
//! sequence and final message it produces.
//!
//! This trait is that boundary in Rust. A provider takes a model, a [`Context`],
//! and [`StreamOptions`], and yields the uniform [`AssistantMessageEvent`]
//! sequence plus the accumulated [`AssistantMessage`] — the same contract pi's
//! `stream()` fulfils.
//!
//! # Streaming model
//!
//! pi streams asynchronously through an `AssistantMessageEventStream`. The Rust
//! core produces the event sequence **eagerly** as a [`StreamResult`] (mirroring
//! the Stage-2 Anthropic parser, which returns its events and final message as a
//! value); the async iterable and inter-chunk timing are re-presented at the
//! binding boundary (the napi shim replays `events` into pi's
//! `AssistantMessageEventStream`). Determinism lives here; scheduling lives at the
//! edge.
//!
//! # Implementations
//!
//! - [`crate::providers::faux::FauxProvider`] — the deterministic test provider,
//!   a byte-compatible port of pi's `providers/faux.ts`. This is the provider the
//!   conformance suite drives.
//! - Real providers (Anthropic and the rest) implement this same trait as their
//!   HTTP/streaming paths land; Stage 2 ported the Anthropic SSE decode that a
//!   real Anthropic `Provider` will build on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;

use crate::types::{
    AssistantMessage, AssistantMessageEvent, Context, Model, SimpleStreamOptions, StreamOptions,
};
use crate::utils::sse::AssistantEventReader;

/// The eager result of a provider stream: the full event sequence and the final
/// accumulated message.
///
/// This is the same shape the Stage-2 Anthropic parser returns
/// ([`crate::api::anthropic::StreamOutcome`]); a provider and a wire parser thus
/// converge on one result type at the seam. The final message is what pi's
/// `AssistantMessageEventStream.result()` resolves to.
#[derive(Debug, Clone, Serialize)]
pub struct StreamResult {
    /// The ordered `start … done|error` event sequence.
    pub events: Vec<AssistantMessageEvent>,
    /// The final accumulated assistant message.
    pub message: AssistantMessage,
}

/// A cooperative abort flag, the Rust analog of a JS `AbortSignal`.
///
/// A provider checks [`AbortSignal::is_aborted`] and, when set, terminates with
/// an `aborted` error event exactly as pi's faux provider does when
/// `options.signal.aborted` is true. Cloneable and shareable so a caller can hold
/// the trigger while the provider holds the observer.
#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    aborted: Arc<AtomicBool>,
}

impl AbortSignal {
    /// A fresh, un-aborted signal.
    pub fn new() -> Self {
        Self::default()
    }

    /// An already-aborted signal, for the "aborted before start" path.
    pub fn aborted() -> Self {
        let signal = Self::new();
        signal.abort();
        signal
    }

    /// Trip the signal (pi's `controller.abort()`).
    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    /// Whether the signal has been tripped (`signal.aborted`).
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

/// The model/streaming provider seam.
///
/// Implemented by the faux provider and, as they land, the real wire providers.
/// `stream` returns the eager [`StreamResult`]; `api` reports the provider's api
/// id (pi's `model.api` discriminant), used by the api registry to route calls.
pub trait Provider: Send + Sync {
    /// The api id this provider serves (pi's `Api`, e.g. `"faux"`,
    /// `"anthropic-messages"`).
    fn api(&self) -> &str;

    /// Stream a response for `model` in `context`, honoring `options` and the
    /// optional abort `signal`. Per pi's contract, request/runtime failures are
    /// encoded as a terminal `error` event in the result, never returned as an
    /// `Err`.
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult;

    /// Stream a response from the simple, level-based options, pi's
    /// `streamSimple` (`ai/src/types.ts:228`). This is the entry point the agent
    /// tier drives: it carries the base [`StreamOptions`] plus the requested
    /// `reasoning` level and per-level `thinking_budgets`.
    ///
    /// # Compatibility default: reasoning dropped, base options unchanged
    ///
    /// The default extracts the base [`StreamOptions`] and runs the raw
    /// [`stream`](Self::stream), so a non-overriding provider produces a request
    /// byte-identical to the pre-seam path (no thinking configuration). Reasoning
    /// lowering is wired per-driver: the Anthropic and Mistral backends override
    /// this method to map `reasoning` onto their request. The remaining drivers
    /// (openai-completions, openai-responses, azure-responses, bedrock, google
    /// gen-ai/vertex) inherit this default and ignore `reasoning` for now; wiring
    /// each is a per-driver follow-up, mirroring the seam note. This keeps the raw
    /// path behavior identical to today (no new deviation).
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        self.stream(model, context, options.map(|o| &o.base), signal)
    }

    /// Stream a response for `model` as an incremental pull reader, the additive
    /// per-frame counterpart to [`stream`](Self::stream).
    ///
    /// # Compatibility default: not incremental
    ///
    /// The default runs the eager [`stream`](Self::stream) and replays its
    /// materialized [`StreamResult`] through
    /// [`AssistantEventReader::from_buffered`], so a non-overriding provider (the
    /// faux provider and every seam-only backend) keeps its exact buffered
    /// behavior with ~0 inter-event spread. Real backends override this to stream
    /// per frame off the wire (see
    /// [`AnthropicMessagesBackend`](crate::providers::AnthropicMessagesBackend)),
    /// where the inter-frame timing becomes observable. Nothing on the hot path
    /// calls this yet; the agent-loop consumer wiring is a follow-up.
    fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
        AssistantEventReader::from_buffered(self.stream(model, context, options, signal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_signal_trips_and_reports() {
        let signal = AbortSignal::new();
        assert!(!signal.is_aborted());
        signal.abort();
        assert!(signal.is_aborted());
        assert!(AbortSignal::aborted().is_aborted());

        // Clones share state.
        let a = AbortSignal::new();
        let b = a.clone();
        a.abort();
        assert!(b.is_aborted());
    }
}
