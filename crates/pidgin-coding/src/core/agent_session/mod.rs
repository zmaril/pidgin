//! Coding-agent turn runner, ported from
//! `packages/coding-agent/src/core/agent-session.ts`.
//!
//! pi's `AgentSession` wraps `pidgin_agent::Agent` with the coding-agent's
//! session tree, steering/follow-up queues, compaction, auto-retry, and the
//! TUI-facing event channel. This is a staged port.
//!
//! [`events`] defines [`AgentSessionEvent`] (the wire union pi emits to TUI/RPC
//! subscribers) and the [`AgentSessionEvent::from_agent_event`] bridge that lifts
//! a core `pidgin_agent::AgentEvent` into the session union. [`session`] carries
//! the [`AgentSession`] struct scaffold: the [`AgentSessionConfig`] options bag,
//! the struct and its fields, the constructor, and the `subscribe`/`emit` event
//! machinery. [`turn`] carries the turn-runner spine (`prompt`/`_runAgentPrompt`/
//! `_handleAgentEvent`); [`queue`] carries the steering / follow-up queue surface
//! (`steer`/`follow_up`/`send_user_message`/`send_custom_message`/`clear_queue`/
//! `pending_message_count`); [`retry`] carries auto-retry with exponential
//! backoff (`is_retryable_error`/`prepare_retry`/`abort_retry`/`is_retrying` and
//! the `will_retry` helpers the turn handler calls). [`compaction_turn`] carries
//! the compaction integration (`check_compaction`/`run_auto_compaction`/the manual
//! `compact`), wired into the turn spine's pre-send and post-run checks.
//! [`tree`] carries session-tree navigation (`navigate_tree`), branch
//! summarization through the compaction seam, the `session_before_tree` /
//! `session_tree` extension dispatch, and the fork-selector accessor
//! (`get_user_messages_for_forking`). The stats / export wiring lands in a later
//! PR.
//!
//! # Owning an `AgentSession`
//!
//! [`AgentSession`] is intentionally **`!Send` / `!Sync`** and its agent loop is
//! **synchronous and eager** — faithful to pi, whose runtime is single-threaded
//! JavaScript. (`!Send` comes transitively from the resource loader's `Rc<RefCell>`
//! state and other non-`Send` collaborators; a `prompt` runs the whole turn to
//! completion on the calling thread.) Do **not** try to make it `Send`: that would
//! force the loader's `Rc` to `Arc` plus `Send` bounds cross-cuttingly, for no
//! gain.
//!
//! Integrate it via the **session-actor** pattern:
//!
//! * **Construct and own** the session entirely on one thread — the turn / worker
//!   thread. It never crosses a thread boundary and is never held across a tokio
//!   `.await` (the RPC layer gives each session a dedicated OS thread).
//! * **Drive it** from other threads through a command channel carrying *owned*
//!   values — `Prompt(String)` / `Steer(String)` / `FollowUp(String)` / `Interrupt`
//!   / `SetModel(..)` / etc. The worker loop `recv`s a command and calls
//!   [`AgentSession::prompt`] / [`AgentSession::steer`] / [`AgentSession::follow_up`]
//!   on the owned session.
//! * **Events flow out** through the locked `Send + Sync` listener closure
//!   (`Arc::new(move |ev| evt_tx.send(ev.clone()))`) into an mpsc drained on the
//!   consumer thread. Only `Send` data — command values, cloned
//!   [`AgentSessionEvent`]s — and that `Send + Sync` closure ever cross threads.
//!
//! Consequence: **no mid-run re-entry.** `steer` / `follow_up` enqueue when issued
//! and drain *between* agent turns (matching pi's observable continuation), never
//! into a turn already in flight. Any interrupt of an in-flight turn (including the
//! auto-retry backoff sleep) is delivered by tripping a shared `Send + Sync` abort
//! handle, not by calling a `&self` method on the blocked session. pi test cases
//! that require genuine in-flight concurrent streaming are structurally N/A under
//! this model and are `#[ignore]`d with that reason rather than weakened.

pub mod bash;
pub mod compaction_turn;
pub mod events;
pub mod extension_turn;
pub mod host;
pub mod model;
pub mod offline_echo;
pub mod queue;
pub mod retry;
pub mod runtime;
pub mod session;
pub mod tree;
pub mod turn;

#[cfg(test)]
pub(crate) mod test_support;

pub use bash::*;
pub use compaction_turn::*;
pub use events::*;
pub use host::*;
pub use model::*;
pub use offline_echo::*;
pub use queue::*;
pub use runtime::*;
pub use session::*;
pub use tree::*;
pub use turn::*;
