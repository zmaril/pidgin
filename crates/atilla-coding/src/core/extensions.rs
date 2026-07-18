//! Mirror of pi-coding-agent's extensions subsystem
//! (`packages/coding-agent/src/core/extensions`).
//!
//! Only the type surface on the exec-tools tool-registry critical path is ported
//! so far ([`types::ToolDefinition`] and [`types::ExtensionContext`]), plus the
//! [`loader`] trait seam the resource-loader orchestrator calls; the dynamic
//! extension engine itself (pi's `jiti` host) is owned by the extension-plane
//! session and lands later.

pub mod loader;
pub mod types;
