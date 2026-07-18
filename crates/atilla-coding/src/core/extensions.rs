//! Mirror of pi-coding-agent's extensions subsystem
//! (`packages/coding-agent/src/core/extensions`).
//!
//! Only the type surface on the exec-tools tool-registry critical path is ported
//! so far ([`types::ToolDefinition`] and [`types::ExtensionContext`]); the
//! extension engine/loader and the remaining extension types land later.

pub mod types;
