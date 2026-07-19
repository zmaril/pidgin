//! Bridges compaction's [`Models`] seam onto atilla-ai's real
//! [`atilla_ai::Models`] wrapper.
//!
//! Compaction summarizes through a minimal [`Models`] trait mirroring pi-ai's
//! `Models.completeSimple(model, context, options)` (see
//! [`super::compaction`]). atilla-ai now ports the streaming half of pi's
//! `Models`, so this module implements the trait for that wrapper: compaction
//! can run against the real provider registry instead of only a test fake.
//!
//! The bridge is purely additive — the [`Models`] trait definition and every
//! compaction code path are unchanged. It maps compaction's
//! [`CompletionOptions`] onto atilla-ai's
//! [`complete_simple`](atilla_ai::Models::complete_simple) call.
//!
//! # Option mapping
//!
//! [`CompletionOptions`] carries `max_tokens`, `signal`, and `reasoning`. The
//! `signal` threads straight through to the eager stream's abort flag. atilla-ai's
//! provider seam takes its per-request [`StreamOptions`](atilla_ai::StreamOptions)
//! as a documented strict subset (session/cache only) that does not yet carry
//! `max_tokens` or `reasoning`; a real dialect derives those inside its own
//! driver (e.g. `atilla_ai::api::anthropic`). This bridge therefore forwards the
//! signal and leaves the request options at their default, matching the current
//! seam boundary.

use atilla_ai::{AssistantMessage, Context, Model};

use super::compaction::{CompletionOptions, Models};

impl Models for atilla_ai::Models {
    fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &CompletionOptions,
    ) -> AssistantMessage {
        // Fully-qualified inherent call: the trait method and atilla-ai's
        // inherent `complete_simple` share a name; name the inherent one.
        atilla_ai::Models::complete_simple(self, model, context, None, options.signal.as_ref())
    }
}
