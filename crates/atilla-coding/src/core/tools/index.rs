//! Deferred port of pi's `core/tools/index.ts`.
//!
//! This module only re-exports the tool factories and assembles the default
//! tool registry consumed by the agent loop. It carries no algorithm of its
//! own; Not yet ported: it is registry wiring that depends on the agent-loop
//! and tool-definition surfaces.
