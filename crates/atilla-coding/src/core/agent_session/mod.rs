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
//! machinery. The turn-runner methods (`prompt`/`steer`/`follow_up`/`compact`/
//! tree-nav/stats/export) and the runtime/tool-registry wiring land in later PRs.

pub mod events;
pub mod session;
pub mod turn;

pub use events::*;
pub use session::*;
pub use turn::*;
