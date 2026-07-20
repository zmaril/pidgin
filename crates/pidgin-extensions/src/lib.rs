//! The pidgin extension planes: an embedded JavaScript engine (`deno`) and an
//! embedded Python engine (`python`), sharing one engine-neutral registration
//! core.
//!
//! pidgin runs pi's `(pi) => {}` TypeScript extensions on an embedded
//! `deno_core` `JsRuntime`. A `JsRuntime` owns a V8 isolate and an event loop;
//! it is `!Send` and must stay pinned to one thread for its whole life. The
//! pidgin core, by contrast, is a multi-threaded tokio runtime. The two cannot
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
//!   * [`Inventory::lower_onto`] lowers that inventory onto pidgin-coding's
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
//! egress proxy every pidgin session runs behind. If this crate built V8 by
//! default, `cargo build --workspace` / `cargo test --workspace` would break in
//! every sandbox the moment it landed. So the runtime lives behind
//! `#[cfg(feature = "deno")]`; the default build is an empty, V8-free crate that
//! compiles everywhere. Build the real runtime with `--features deno` (CI does
//! this in a dedicated job where the blob download succeeds).
//!
//! # The `python` feature gate (the offline sibling engine)
//!
//! The non-default **`python`** feature compiles a second engine — a PyO3-backed
//! host that loads pi-style extensions authored in Python (`def extension(pi):
//! pi.register_command / register_tool / pi.on(...)`) and produces the SAME host
//! records as the deno engine. Unlike V8, libpython is present in every sandbox,
//! so `python` BUILDS and TESTS in-session (PyO3's `auto-initialize` embeds
//! CPython). Both engines share the engine-neutral registration core: the
//! [`Inventory`] plain-data records and the [`host`](crate::host) lowering onto
//! pidgin-coding's `Registry` are gated `any(deno, python)`, so one
//! Inventory→Registry path serves both. Only the V8/deno-specific modules
//! (`runtime`/`JsPlaneHandle`, `api_ops`/`BOOTSTRAP_JS`, the transpiling
//! `loader`, the async-dispatch `runner`, `oauth_login_impl`) stay deno-only.

// ---------------------------------------------------------------------------
// Engine-neutral registration core (shared by `deno` and `python`)
// ---------------------------------------------------------------------------
// `Inventory` is plain data and `host::lower_inventory` lowers it onto the core
// `Registry`; neither touches V8 or libpython, so both engines reuse them.
#[cfg(any(feature = "deno", feature = "python"))]
mod host;
#[cfg(any(feature = "deno", feature = "python"))]
mod inventory;

#[cfg(any(feature = "deno", feature = "python"))]
pub use inventory::{
    CommandRecord, FlagRecord, HookRecord, Inventory, ProviderRecord, RendererRecord,
    ShortcutRecord, ToolRecord,
};

// ---------------------------------------------------------------------------
// Combined (deno + python) engine — one loader/runner over both planes
// ---------------------------------------------------------------------------
// The combined seam composes each engine's own `spawn` / factory entry points;
// it is available whenever at least one engine is compiled in.
#[cfg(any(feature = "deno", feature = "python"))]
mod combined;

#[cfg(any(feature = "deno", feature = "python"))]
pub use combined::{
    create_combined_extension_runner, CombinedExtensionLoader, CombinedExtensionRunner,
    CombinedExtensionRuntime, EngineSelection,
};

// ---------------------------------------------------------------------------
// V8 / deno-specific engine (the `deno` feature)
// ---------------------------------------------------------------------------
#[cfg(feature = "deno")]
mod api_ops;
#[cfg(feature = "deno")]
mod context;
#[cfg(feature = "deno")]
mod dispatch;
#[cfg(feature = "deno")]
mod loader;
#[cfg(feature = "deno")]
mod module_loader;
#[cfg(feature = "deno")]
mod oauth_login_impl;
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
pub use dispatch::{HookInvocation, StoredInvocation};
#[cfg(feature = "deno")]
pub use oauth_login_impl::DenoExtensionOAuthLogin;
#[cfg(feature = "deno")]
pub use resource_loader_impl::{RealExtensionLoader, RealExtensionRuntime};
#[cfg(feature = "deno")]
pub use runner::{ContextConfig, ExtensionRunner, LoadedExtension};
#[cfg(feature = "deno")]
pub use runner_impl::{
    create_deno_extension_runner, create_deno_extension_runner_from_runtime_ref,
    hook_event_from_str, DenoExtensionRunner,
};
#[cfg(feature = "deno")]
pub use runtime::{JsPlaneHandle, SourceLanguage};

// ---------------------------------------------------------------------------
// PyO3 / Python-specific engine (the `python` feature)
// ---------------------------------------------------------------------------
#[cfg(feature = "python")]
mod python;

#[cfg(feature = "python")]
pub use python::{
    create_python_extension_runner, LoadedPyExtension, PythonExtensionLoader,
    PythonExtensionRunner, PythonExtensionRuntime,
};
