//! Coding-agent turn runner, ported from
//! `packages/coding-agent/src/core/agent-session.ts`.
//!
//! pi's `AgentSession` wraps `atilla_agent::Agent` with the coding-agent's
//! session tree, steering/follow-up queues, compaction, auto-retry, and the
//! TUI-facing event channel. This is a staged port.
//!
//! **PR1 (this change): event types only.** [`events`] defines
//! [`AgentSessionEvent`] (the wire union pi emits to TUI/RPC subscribers) and
//! the [`AgentSessionEvent::from_agent_event`] bridge that lifts a core
//! `atilla_agent::AgentEvent` into the session union. The `AgentSession` struct
//! itself, its `subscribe`/`_emit` machinery, and the `ExtensionRunner` seam land
//! in later PRs.

pub mod events;

pub use events::*;
