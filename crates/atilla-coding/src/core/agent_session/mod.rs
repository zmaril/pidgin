//! Coding-agent turn runner, ported from
//! `packages/coding-agent/src/core/agent-session.ts`.
//!
//! pi's `AgentSession` wraps `atilla_agent::Agent` with the coding-agent's
//! session tree, steering/follow-up queues, compaction, auto-retry, and the
//! TUI-facing event channel. This is a staged port.
//!
//! [`events`] defines [`AgentSessionEvent`] (the wire union pi emits to TUI/RPC
//! subscribers) and the [`AgentSessionEvent::from_agent_event`] bridge that lifts
//! a core `atilla_agent::AgentEvent` into the session union. [`session`] carries
//! the [`AgentSession`] struct scaffold: the [`AgentSessionConfig`] options bag,
//! the struct and its fields, the constructor, and the `subscribe`/`emit` event
//! machinery. [`turn`] carries the turn-runner spine (`prompt`/`_runAgentPrompt`/
//! `_handleAgentEvent`); [`queue`] carries the steering / follow-up queue surface
//! (`steer`/`follow_up`/`send_user_message`/`send_custom_message`/`clear_queue`/
//! `pending_message_count`). The compaction / auto-retry / tree-nav / stats
//! wiring lands in later PRs.

pub mod events;
pub mod queue;
pub mod session;
pub mod turn;

#[cfg(test)]
pub(crate) mod test_support;

pub use events::*;
pub use queue::*;
pub use session::*;
pub use turn::*;
