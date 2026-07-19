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
//!     data (`serde_json::Value`) crosses the thread boundary, never a V8
//!     handle.
//!
//! This is PR-A of the extension plane: the runtime host bootstrap. The public
//! surface is deliberately tiny — [`JsPlaneHandle::spawn`],
//! [`JsPlaneHandle::eval`], and [`JsPlaneHandle::shutdown`] — enough to prove
//! that V8 boots off-thread and that JavaScript results round-trip back to the
//! caller. The pi-facing surface (tool/hook ops, TypeScript transpile,
//! discovery, the registry) is layered on in later PRs. The shape is
//! productionized from the `throwaway/deno-hello` spike.
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
mod runtime;

#[cfg(feature = "deno")]
pub use runtime::JsPlaneHandle;
