//! Mirror of pi-coding-agent's extensions subsystem
//! (`packages/coding-agent/src/core/extensions`).
//!
//! Ported so far:
//! - [`types`] — the tool-registry critical-path types
//!   ([`types::ToolDefinition`], [`types::ExtensionContext`]).
//! - [`loader`] — the extension-loader trait seam the resource-loader
//!   orchestrator calls (the dynamic extension engine itself, pi's `jiti` host,
//!   is owned by the extension-plane session and lands later).
//! - [`events`] — the faithful port of pi's 33 hook-event payload and result
//!   types (the `ExtensionEvent` union), split into a directory module.
//! - [`hook`] — the [`hook::Hook`] trait, the [`hook::HookEvent`] event-name
//!   enum, and the [`hook::HookOutcome`] / [`hook::Affinity`] design types.
//! - [`command`] — the [`command::Command`] trait and the
//!   [`command::RegisteredCommand`] descriptor.
//! - [`registry`] — the [`registry::ExtensionHost`] registration surface and the
//!   [`registry::Registry`] inventory.
//! - [`discovery`] — the filesystem-convention scan that locates extensions and
//!   resolves each declared entrypoint into a [`discovery::DiscoveredExtension`]
//!   inventory (pure Rust, no JS execution).
//!
//! The JS-execution plane (running each discovered entrypoint on the embedded
//! `deno_core` runtime) and the `ExtensionRunner` hook-dispatch machinery land in
//! later ports. Apart from [`discovery`], these modules are pure types and traits
//! — no runtime, no `deno_core`, no JS.

pub mod command;
pub mod discovery;
pub mod events;
pub mod hook;
pub mod loader;
pub mod registry;
pub mod types;
