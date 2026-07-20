// straitjacket-allow-file:duplication -- the spawn/load/inventory-assert/invoke
// shape mirrors the sibling deno acceptance tests (deno_typebox_module_loader.rs,
// deno_pirate_extension.rs, deno_example_extension.rs) by design: they all drive
// the same plane entrypoints over a vendored upstream file. The parallel
// structure is intentional.

//! Acceptance test proving the pi RUNTIME SHIMS let a REAL upstream pi
//! `defineTool` tool extension load, register, and invoke on pidgin's plane.
//!
//! The subject is `examples/extensions/hello/index.ts`, vendored verbatim from
//! pi's `packages/coding-agent/examples/extensions/hello.ts` (MIT, see
//! `examples/extensions/NOTICE`). Unlike `reload-runtime` (which imports `Type`
//! straight from the bare `typebox` specifier), hello.ts imports pi's OWN
//! helpers: `Type` from `@earendil-works/pi-ai` and `defineTool` from
//! `@earendil-works/pi-coding-agent`. Both are VALUE imports that survive the
//! TypeScript transpile, so evaluating the module triggers module resolution for
//! those two `@earendil-works/*` specifiers. The module loader now serves each
//! from a small hand-written faithful shim:
//!
//!   * `@earendil-works/pi-ai`  → re-exports `Type` (nest-resolved from `typebox`,
//!     which resolves through the SAME loader to the shared vendored bundle) plus
//!     `StringEnum`.
//!   * `@earendil-works/pi-coding-agent` → the identity `defineTool`.
//!
//! This test is the headline proof that a `defineTool` tool extension importing
//! pi's helpers now loads, registers its tool (so `Type.Object(...)` built the
//! schema and `defineTool` passed the tool through), and invokes.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use serde_json::json;

use pidgin_extensions::{JsPlaneHandle, SourceLanguage};

/// The vendored upstream example, resolved relative to this crate.
const HELLO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/extensions/hello/index.ts"
);

/// Load the real upstream `hello` extension through the plane and prove its
/// `defineTool` / `Type` imports resolve through the pi runtime shims: the
/// extension LOADS clean (both `@earendil-works/*` shims + the pi-ai shim's
/// nested `typebox` import resolved), registers its `hello` tool (so
/// `Type.Object(...)` built the schema and `defineTool` passed the tool through),
/// and the pure `execute` dispatches through the plane returning `Hello, world!`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vendored_hello_tool_loads_through_the_pi_runtime_shims() {
    let source = std::fs::read_to_string(HELLO_PATH).expect("read vendored hello example");
    let plane = JsPlaneHandle::spawn();

    // 1. HEADLINE: it loads with NO error. `defineTool` resolved from the
    //    pi-coding-agent shim, `Type` from the pi-ai shim, and the pi-ai shim's
    //    own nested `import { Type } from "typebox"` resolved through the SAME
    //    loader to the vendored TypeBox 1.1.38 bundle — so `Type.Object({...})`
    //    ran at factory time. Before this slice, `load_extension_source` errored
    //    here with a bare-specifier resolution failure on `@earendil-works/pi-ai`.
    let inventory = plane
        .load_extension_source("hello", source, SourceLanguage::TypeScript)
        .await
        .expect("hello loads: its `defineTool`/`Type` imports resolved through the pi shims");

    // 2. It registered exactly what hello.ts declares: the `hello` tool (and no
    //    command — hello.ts only calls pi.registerTool).
    assert!(
        inventory.tools.iter().any(|t| t.name == "hello"),
        "registers the hello tool (proves defineTool passed it through and Type.Object built the \
         schema), got {:?}",
        inventory.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    assert!(
        inventory.commands.is_empty(),
        "hello.ts registers no command, got {:?}",
        inventory
            .commands
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );

    // 3. Invoke the tool through the REAL one-shot invoke-stored primitive. This
    //    runs the tool's `execute` body on the live plane. hello's execute is
    //    PURE: it takes no ctx/host and returns a content envelope built from its
    //    `name` param. So the invocation runs to completion with `ok == true` and
    //    the tool's own returned content.
    //
    //    A tool's args array maps to pi's `execute(toolCallId, params, …)`
    //    positionally: slot 0 is the tool-call id, slot 1 is the `params` object.
    //    (See the canonical `invoke_stored("tool", "echo", &json!(["call-1", {…}]))`
    //    in deno_oauth_phase_a.rs.) hello.ts reads `params.name`, so the greeted
    //    name must live in slot 1 — a bare `[{ "name": "world" }]` would land in
    //    slot 0 and leave `params` undefined.
    let outcome = plane
        .invoke_stored("tool", "hello", &json!(["call-hello", { "name": "world" }]))
        .await
        .expect("the hello tool dispatches through the plane and returns an envelope");
    assert!(
        outcome.ok,
        "hello.execute ran to completion (no host/ctx needed), got error {:?}",
        outcome.error
    );
    let text = outcome.result["content"][0]["text"]
        .as_str()
        .expect("the tool returned a text content block");
    assert!(
        text.contains("Hello, world!"),
        "the tool greeted its `name` param (proves Type built the schema and the pure execute ran), \
         got {text:?}"
    );

    plane.shutdown().await;
}
