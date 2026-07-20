// straitjacket-allow-file:duplication -- the spawn/load/inventory-assert shape
// mirrors the sibling deno acceptance tests (deno_pirate_extension.rs,
// deno_example_extension.rs) by design: they all drive the same plane entrypoints
// over a vendored upstream file. The parallel structure is intentional.

//! Acceptance test proving the bare-specifier MODULE LOADER lets a REAL upstream
//! pi tool-registering extension load on pidgin's plane.
//!
//! The subject is `examples/extensions/reload-runtime/index.ts`, vendored
//! verbatim from pi's
//! `packages/coding-agent/examples/extensions/reload-runtime.ts` (MIT, see
//! `examples/extensions/NOTICE`). Unlike the pirate example — whose only import
//! is a type-only import erased at transpile — this extension has a **value**
//! import, `import { Type } from "typebox"`, and calls `Type.Object({})` at load
//! time to build its tool's parameter schema. That import survives the TypeScript
//! transpile, so evaluating the module triggers module resolution for the bare
//! `typebox` specifier. Before the module loader existed the plane wired no
//! `ModuleLoader` (the default `NoopModuleLoader`), and this extension failed to
//! load. This test is the headline proof that it now loads.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use serde_json::json;

use pidgin_extensions::{JsPlaneHandle, SourceLanguage};

/// The vendored upstream example, resolved relative to this crate.
const RELOAD_RUNTIME_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/extensions/reload-runtime/index.ts"
);

/// Load the real upstream `reload-runtime` extension through the plane and prove
/// its `import { Type } from "typebox"` resolves through the module loader: the
/// extension LOADS clean, registers its command and tool (so `Type.Object(...)`
/// evaluated and the tool schema built), and the tool dispatches through the
/// plane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vendored_reload_runtime_loads_through_the_typebox_module_loader() {
    let source =
        std::fs::read_to_string(RELOAD_RUNTIME_PATH).expect("read vendored reload-runtime example");
    let plane = JsPlaneHandle::spawn();

    // 1. HEADLINE: it loads with NO error. The bare `typebox` import resolved
    //    through the module loader to the vendored TypeBox 1.1.38 bundle, whose
    //    evaluation supplied `Type` so `Type.Object({})` ran at factory time.
    //    Before this slice, `load_extension_source` errored here with a module
    //    resolution failure.
    let inventory = plane
        .load_extension_source("reload-runtime", source, SourceLanguage::TypeScript)
        .await
        .expect("reload-runtime loads: its `typebox` value import resolved through the loader");

    // 2. It registered exactly what reload-runtime.ts declares.
    assert!(
        inventory
            .commands
            .iter()
            .any(|c| c.name == "reload-runtime"),
        "registers the /reload-runtime command, got {:?}",
        inventory
            .commands
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );
    assert!(
        inventory.tools.iter().any(|t| t.name == "reload_runtime"),
        "registers the reload_runtime tool (proves Type.Object(...) built the schema), got {:?}",
        inventory.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // 3. Invoke the tool through the REAL one-shot invoke-stored primitive. This
    //    runs the tool's `execute` body on the live plane. reload_runtime's
    //    execute takes no ctx: it only calls `pi.sendUserMessage(...)` (a
    //    present-but-stubbed action that returns undefined without throwing) and
    //    returns a content envelope. So the invocation runs to completion with
    //    `ok == true` and the tool's own returned content — no host dependency,
    //    unlike the pirate command whose handler touches an undefined `ctx`.
    let outcome = plane
        .invoke_stored("tool", "reload_runtime", &json!([{}]))
        .await
        .expect("the reload_runtime tool dispatches through the plane and returns an envelope");
    assert!(
        outcome.ok,
        "reload_runtime.execute ran to completion (no host/ctx needed), got error {:?}",
        outcome.error
    );
    let text = outcome.result["content"][0]["text"]
        .as_str()
        .expect("the tool returned a text content block");
    assert!(
        text.contains("Queued"),
        "the tool returned its own queued-followup content, got {text:?}"
    );

    plane.shutdown().await;
}

/// NEGATIVE: an extension importing an UNVENDORED bare specifier fails to load
/// with the clear, specifier-named error from the module loader. `typebox/value`
/// is a real pi subpath the full jiti alias map serves but that this slice
/// deliberately does not vendor — the loader must reject it by name rather than
/// silently failing or serving the wrong module.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unvendored_bare_specifier_fails_to_load_with_a_clear_error() {
    // A minimal extension whose only non-type import is the unvendored subpath.
    // The value import survives transpile, so loading triggers resolution of
    // "typebox/value", which the loader rejects.
    const SOURCE: &str = r#"
import { Value } from "typebox/value";
export default function (pi) {
  pi.registerCommand("uses-value", { description: "x", handler: async () => { void Value; } });
}
"#;

    let plane = JsPlaneHandle::spawn();
    let err = plane
        .load_extension_source("uses-value", SOURCE, SourceLanguage::TypeScript)
        .await
        .expect_err("importing the unvendored `typebox/value` subpath must fail to load");
    let msg = err.to_string();
    assert!(
        msg.contains("typebox/value"),
        "the load error names the unresolvable specifier, got: {msg}"
    );

    plane.shutdown().await;
}
