//! A queue-backed event stream, ported from pi-ai's
//! `packages/ai/src/utils/event-stream.ts` at pinned commit `3da591ab`.
//!
//! pi's `EventStream<T, R>` is an `AsyncIterable`: producers `push` events onto
//! an internal queue while an `async` consumer awaits them, and a terminal event
//! resolves a `finalResultPromise` carrying the run's result `R`. atilla-ai has
//! no async runtime (the crate depends only on `serde`/`serde_json`), so this
//! port models the same object as a **synchronous** queue-backed stream matching
//! the FFI "blocking `next_event()` over an opaque handle" contract from
//! `notes/design.md`: the producer side pushes events, the consumer side drains
//! them in FIFO order with [`EventStream::next`], and the terminal result is
//! captured as a plain value readable via [`EventStream::result`].
//!
//! # Terminal semantics (faithful to pi)
//!
//! - `done` and `error` are the ONLY terminal events. The first terminal event
//!   flips the stream to done and captures the final result.
//! - After a terminal event, further `push`es are no-ops (pi's `if (this.done)
//!   return;`).
//! - The terminal error is an **error-as-value**: pi's `AssistantMessageEventStream`
//!   resolves its promise with `event.error` (an [`AssistantMessage`]) rather than
//!   rejecting, so a failed run never throws. Here the captured result is a
//!   `Result<R, E>` — `Ok` for `done`, `Err` for `error` — and reading it never
//!   panics.
//!
//! # Fusing pi's two constructor callbacks
//!
//! pi's constructor takes `isComplete(event) -> bool` and `extractResult(event)
//! -> R`, where `extractResult` is only ever called when `isComplete` returned
//! true and otherwise throws "Unexpected event type for final result". This port
//! fuses the pair into a single `terminal(event) -> Option<Result<R, E>>`:
//! `None` mirrors `isComplete == false`, and `Some(_)` carries what
//! `extractResult` would have produced. The fusion is behaviour-preserving
//! (`isComplete(e) == terminal(e).is_some()`) and removes pi's throwing arm, so
//! the terminal-error path stays panic-free by construction.

use std::collections::VecDeque;

use crate::types::{AssistantMessage, AssistantMessageEvent};

/// A synchronous, queue-backed event stream mirroring pi's `EventStream<T, R>`.
///
/// `T` is the event type, `R` the success result carried by a `done` terminal,
/// and `E` the error value carried by an `error` terminal (pi collapses both
/// into `R`; the split here makes the error-as-value contract explicit).
pub struct EventStream<T, R, E> {
    /// FIFO queue of events awaiting a consumer (pi's `queue`).
    queue: VecDeque<T>,
    /// Set once a terminal event has been observed (pi's `done`).
    done: bool,
    /// The captured terminal result: `Ok` from `done`, `Err` from `error`
    /// (pi's `finalResultPromise` value). `None` until a terminal event or an
    /// [`EventStream::end`] with a result.
    result: Option<Result<R, E>>,
    /// Classifies an event as terminal and extracts its result, fusing pi's
    /// `isComplete` + `extractResult` (see the module docs).
    terminal: fn(&T) -> Option<Result<R, E>>,
}

impl<T, R, E> EventStream<T, R, E> {
    /// Creates an empty stream (pi's `constructor`). `terminal` returns `Some`
    /// for a terminal event — `Ok` for success, `Err` for failure — and `None`
    /// for every non-terminal event.
    pub fn new(terminal: fn(&T) -> Option<Result<R, E>>) -> Self {
        Self {
            queue: VecDeque::new(),
            done: false,
            result: None,
            terminal,
        }
    }

    /// Enqueues an event (pi's `push`).
    ///
    /// A no-op once the stream is done. A terminal event flips the stream to
    /// done and captures its result, and — exactly as in pi, where the terminal
    /// event is still delivered to the consumer — is itself enqueued so a
    /// draining consumer observes it.
    pub fn push(&mut self, event: T) {
        if self.done {
            return;
        }
        if let Some(result) = (self.terminal)(&event) {
            self.done = true;
            self.result = Some(result);
        }
        self.queue.push_back(event);
    }

    /// Finalizes the stream (pi's `end`), optionally capturing a final result.
    ///
    /// Already-queued events remain drainable; only after they are consumed does
    /// [`EventStream::next`] report completion.
    pub fn end(&mut self, result: Option<Result<R, E>>) {
        self.done = true;
        if result.is_some() {
            self.result = result;
        }
    }

    /// Pops the next queued event in FIFO order, or `None` when the queue is
    /// empty. When the queue is empty and [`EventStream::is_done`] is true, the
    /// stream is fully drained (pi's iterator `return`).
    // Deliberately named `next` to mirror the FFI "blocking `next_event()` over
    // an opaque handle" contract (see the module docs) rather than
    // `std::iter::Iterator`: an empty queue does not imply completion (the
    // producer may push more unless `is_done`), so this is not a fused iterator.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    /// Whether a terminal event (or [`EventStream::end`]) has finalized the
    /// stream. Queued events may still remain to be drained.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The captured terminal result (pi's resolved `finalResultPromise`): `Ok`
    /// from a `done` terminal, `Err` from an `error` terminal, or `None` if no
    /// terminal result has been captured yet. Reading it never panics.
    pub fn result(&self) -> Option<&Result<R, E>> {
        self.result.as_ref()
    }
}

/// pi's `AssistantMessageEventStream`: an [`EventStream`] over
/// [`AssistantMessageEvent`] whose terminal is `done` (carrying the final
/// [`AssistantMessage`]) or `error` (carrying the error [`AssistantMessage`]).
///
/// Both terminals resolve to an [`AssistantMessage`], matching pi where
/// `R == AssistantMessage`; the `Ok`/`Err` split records which terminal fired
/// while keeping the error a value.
pub type AssistantMessageEventStream =
    EventStream<AssistantMessageEvent, AssistantMessage, AssistantMessage>;

/// pi's `createAssistantMessageEventStream()` factory.
///
/// Terminal classification mirrors pi's constructor: `done` extracts
/// `event.message`, `error` extracts `event.error`, every other event is
/// non-terminal.
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    EventStream::new(|event| match event {
        AssistantMessageEvent::Done { message, .. } => Some(Ok(message.clone())),
        AssistantMessageEvent::Error { error, .. } => Some(Err(error.clone())),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::faux::{faux_assistant_message, faux_text, FauxAssistantOptions};
    use crate::types::{AssistantMessage, StopReason};

    fn done_message() -> AssistantMessage {
        faux_assistant_message(vec![faux_text("hello")], FauxAssistantOptions::default(), 0)
    }

    fn error_message(msg: &str) -> AssistantMessage {
        faux_assistant_message(
            vec![],
            FauxAssistantOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(msg.to_string()),
                ..Default::default()
            },
            0,
        )
    }

    fn start_event(message: AssistantMessage) -> AssistantMessageEvent {
        AssistantMessageEvent::Start { partial: message }
    }

    #[test]
    fn drains_non_terminal_events_in_fifo_order() {
        let mut stream = create_assistant_message_event_stream();
        stream.push(start_event(done_message()));
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "a".to_string(),
            partial: done_message(),
        });
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "b".to_string(),
            partial: done_message(),
        });

        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Start { .. })
        ));
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::TextDelta { delta, .. }) if delta == "a"
        ));
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::TextDelta { delta, .. }) if delta == "b"
        ));
        assert!(stream.next().is_none());
        // No terminal event yet.
        assert!(!stream.is_done());
        assert!(stream.result().is_none());
    }

    #[test]
    fn done_terminal_captures_message_and_blocks_further_pushes() {
        let mut stream = create_assistant_message_event_stream();
        let message = done_message();
        stream.push(start_event(message.clone()));
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: message.clone(),
        });

        assert!(stream.is_done());
        // The terminal `done` is captured as an `Ok` value...
        match stream.result() {
            Some(Ok(captured)) => assert_eq!(captured, &message),
            other => panic!("expected Ok(message), got {other:?}"),
        }

        // ...and the terminal event is still delivered to the consumer.
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Start { .. })
        ));
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Done { .. })
        ));
        assert!(stream.next().is_none());

        // Further pushes after a terminal event are no-ops.
        stream.push(start_event(done_message()));
        assert!(stream.next().is_none());
    }

    #[test]
    fn error_terminal_captures_error_as_a_value() {
        let mut stream = create_assistant_message_event_stream();
        let error = error_message("overloaded_error");
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: error.clone(),
        });

        assert!(stream.is_done());
        // The terminal error is an `Err` VALUE, not a panic/throw.
        match stream.result() {
            Some(Err(captured)) => assert_eq!(captured, &error),
            other => panic!("expected Err(error), got {other:?}"),
        }

        // Only the `error` terminal is enqueued.
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Error { .. })
        ));
        assert!(stream.next().is_none());
    }

    #[test]
    fn error_after_done_is_ignored() {
        let mut stream = create_assistant_message_event_stream();
        let message = done_message();
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: message.clone(),
        });
        // A later error must not overwrite the captured success.
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: error_message("late error"),
        });

        assert!(matches!(stream.result(), Some(Ok(_))));
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Done { .. })
        ));
        // The post-terminal error was never enqueued.
        assert!(stream.next().is_none());
    }

    #[test]
    fn end_finalizes_without_a_result() {
        let mut stream = create_assistant_message_event_stream();
        stream.push(start_event(done_message()));
        stream.end(None);

        assert!(stream.is_done());
        assert!(stream.result().is_none());
        // Queued events are still drained before completion.
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Start { .. })
        ));
        assert!(stream.next().is_none());
    }

    #[test]
    fn end_can_capture_a_final_result() {
        let mut stream = create_assistant_message_event_stream();
        let message = done_message();
        stream.end(Some(Ok(message.clone())));

        assert!(stream.is_done());
        assert!(matches!(stream.result(), Some(Ok(captured)) if captured == &message));
        // After `end`, pushes are no-ops.
        stream.push(start_event(done_message()));
        assert!(stream.next().is_none());
    }
}
