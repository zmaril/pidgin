//! The atilla JavaScript extension plane.
//!
//! atilla runs pi's `(pi) => {}` TypeScript extensions on an embedded
//! `deno_core` `JsRuntime`. A `JsRuntime` owns a V8 isolate and an event loop;
//! it is `!Send` and must stay pinned to one thread for its whole life. The
//! atilla core, by contrast, is a multi-threaded tokio runtime. The two cannot
//! share a thread, so this crate bridges them with the off-thread rendezvous
//! mandated by `notes/startup/deep-hooks.md` §5 (`Affinity::OwnRuntime`):
//!
//!   * the `JsRuntime` is constructed and driven on its own dedicated OS thread
//!     (it can never be built elsewhere and moved in, because it is `!Send`);
//!   * a [`JsPlaneHandle`] — which carries only a channel, so it is `Send +
//!     Sync` — is handed to the outside world;
//!   * callers submit work over the channel and await the answer; only plain
//!     data (`serde_json::Value` / the [`Inventory`]) crosses the thread
//!     boundary, never a V8 handle.
//!
//! # What this crate does (PR-A + PR-E)
//!
//! PR-A bootstrapped the runtime host: [`JsPlaneHandle::spawn`] /
//! [`JsPlaneHandle::eval`] / [`JsPlaneHandle::shutdown`]. PR-E adds the module
//! loader and the `ExtensionAPI` registration bindings on top:
//!
//!   * [`JsPlaneHandle::load_extension_source`] transpiles a pi-style TypeScript
//!     (or JavaScript) extension, evaluates it as an ES module, extracts its
//!     default-export factory, and runs it with a `pi` object bound through
//!     `deno_core` ops (see the `api_ops` module);
//!   * every registration call the factory makes (`registerTool`, `on`,
//!     `registerCommand`, `registerShortcut`, `registerFlag`/`getFlag`,
//!     `registerMessageRenderer`/`registerEntryRenderer`) lands in a Rust-side
//!     [`Inventory`];
//!   * [`Inventory::lower_onto`] lowers that inventory onto atilla-coding's
//!     `ExtensionHost` `Registry`, the single Rust source of truth from
//!     `notes/design.md`.
//!
//! # What PR-F adds (hook DISPATCH)
//!
//! PR-F makes the registered handlers *run*: [`JsPlaneHandle::invoke_hook`]
//! invokes a previously-registered handler closure (kept in the runtime, keyed
//! by event name) with a JSON `(event, ctx)` over the rendezvous and returns its
//! shaped result; [`ExtensionRunner`] is the Rust orchestrator that dispatches a
//! hook by calling each registered handler in order and applying pi's per-hook
//! result semantics (chain / merge / short-circuit / replace + error isolation).
//! The `ctx` handlers receive exposes the data getters the acceptance suite reads
//! (notably `getSystemPrompt()`); the action methods (`sendMessage`, `exec`,
//! `setModel`, provider registration, …) remain present-but-no-op — no
//! acceptance fixture calls one, so their host-backed wiring is deferred.
//!
//! # The `deno` feature gate
//!
//! Everything above is compiled only under the non-default **`deno`** feature.
//! `deno_core` embeds V8, whose prebuilt static blob is downloaded from GitHub
//! release assets on first build — a download blocked (HTTP 403) by the sandbox
//! egress proxy every atilla session runs behind. If this crate built V8 by
//! default, `cargo build --workspace` / `cargo test --workspace` would break in
//! every sandbox the moment it landed. So the runtime lives behind
//! `#[cfg(feature = "deno")]`; the default build is an empty, V8-free crate that
//! compiles everywhere. Build the real runtime with `--features deno` (CI does
//! this in a dedicated job where the blob download succeeds).

#[cfg(feature = "deno")]
mod api_ops;
#[cfg(feature = "deno")]
mod context;
#[cfg(feature = "deno")]
mod dispatch;
#[cfg(feature = "deno")]
mod host;
#[cfg(feature = "deno")]
mod inventory;
#[cfg(feature = "deno")]
mod loader;
#[cfg(feature = "deno")]
mod resource_loader_impl;
#[cfg(feature = "deno")]
mod runner;
#[cfg(feature = "deno")]
mod runner_impl;
#[cfg(feature = "deno")]
mod runtime;

#[cfg(feature = "deno")]
pub use context::MinimalExtensionContext;
#[cfg(feature = "deno")]
pub use dispatch::HookInvocation;
#[cfg(feature = "deno")]
pub use inventory::{
    CommandRecord, FlagRecord, HookRecord, Inventory, RendererRecord, ShortcutRecord, ToolRecord,
};
#[cfg(feature = "deno")]
pub use resource_loader_impl::{RealExtensionLoader, RealExtensionRuntime};
#[cfg(feature = "deno")]
pub use runner::{ContextConfig, ExtensionRunner, LoadedExtension};
#[cfg(feature = "deno")]
pub use runner_impl::{create_deno_extension_runner, hook_event_from_str, DenoExtensionRunner};
#[cfg(feature = "deno")]
pub use runtime::{JsPlaneHandle, SourceLanguage};
