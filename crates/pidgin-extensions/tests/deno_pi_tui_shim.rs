// straitjacket-allow-file:duplication -- the spawn/load/inventory-assert/invoke
// shape mirrors the sibling deno acceptance tests (deno_pi_runtime_shim.rs,
// deno_typebox_module_loader.rs, deno_pirate_extension.rs) by design: they all
// drive the same plane entrypoints over a vendored upstream file. The parallel
// structure is intentional.

//! Acceptance test proving the pi-tui RENDER-STUB shim (plus the pi-ai /
//! pi-coding-agent shims and the typebox module loader) lets a REAL upstream pi
//! tool extension whose display hook imports a pi-tui UI component LOAD, register,
//! and invoke on pidgin's HEADLESS plane.
//!
//! The subject is `examples/extensions/structured-output/index.ts`, vendored
//! verbatim from pi's `packages/coding-agent/examples/extensions/
//! structured-output.ts` (MIT, see `examples/extensions/NOTICE`). It imports three
//! value specifiers that survive the TypeScript transpile, so evaluating the
//! module triggers module resolution for each:
//!
//!   * `defineTool` from `@earendil-works/pi-coding-agent` (identity shim),
//!   * `Text` from `@earendil-works/pi-tui` (render-stub shim — the marker class),
//!   * `Type` from `typebox` (the vendored TypeBox bundle).
//!
//! `Text` is used ONLY inside the tool's `renderResult` display hook, which the
//! headless plane never calls (invoke runs the tool's `execute`; the inventory
//! records metadata only). So the render-stub marker class is faithful for
//! load / register / invoke — the plane never renders. This test is the proof that
//! resolving `@earendil-works/pi-tui` to markers is enough for such an extension.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use serde_json::json;

use pidgin_extensions::{JsPlaneHandle, SourceLanguage};

/// The vendored upstream example, resolved relative to this crate.
const STRUCTURED_OUTPUT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/extensions/structured-output/index.ts"
);

/// Load the real upstream `structured-output` extension through the plane and
/// prove its `defineTool` / `Text` / `Type` imports all resolve: the extension
/// LOADS clean (the pi-tui render-stub, the pi-coding-agent + pi-ai shims, and the
/// pi-ai shim's nested `typebox` import all resolved), registers its
/// `structured_output` tool (so `Type.Object(...)` built the schema and
/// `defineTool` passed the tool through), and the pure `execute` dispatches
/// through the plane returning the saved-headline envelope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vendored_structured_output_tool_loads_through_the_pi_tui_shim() {
    let source = std::fs::read_to_string(STRUCTURED_OUTPUT_PATH)
        .expect("read vendored structured-output example");
    let plane = JsPlaneHandle::spawn();

    // 1. HEADLINE: it loads with NO error. `Text` resolved from the pi-tui
    //    render-stub shim, `defineTool` from the pi-coding-agent shim, and `Type`
    //    from typebox (directly and via the pi-ai shim's nested import) — so
    //    `Type.Object({...})` ran at factory time. Before the pi-tui shim,
    //    `load_extension_source` errored here with a bare-specifier resolution
    //    failure on `@earendil-works/pi-tui`.
    let inventory = plane
        .load_extension_source("structured-output", source, SourceLanguage::TypeScript)
        .await
        .expect(
            "structured-output loads: its `defineTool`/`Text`/`Type` imports resolved (the pi-tui \
             render-stub serves `Text` even though renderResult is never called headless)",
        );

    // 2. It registered exactly what structured-output.ts declares: the
    //    `structured_output` tool (and no command — it only calls
    //    pi.registerTool). Registration succeeds even though the tool object
    //    carries a `renderResult` that closes over the pi-tui `Text` marker: the
    //    headless plane records tool metadata and never invokes the renderer.
    assert!(
        inventory.tools.iter().any(|t| t.name == "structured_output"),
        "registers the structured_output tool (proves defineTool passed it through and Type.Object \
         built the schema), got {:?}",
        inventory.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    assert!(
        inventory.commands.is_empty(),
        "structured-output.ts registers no command, got {:?}",
        inventory
            .commands
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );

    // 3. Invoke the tool through the REAL one-shot invoke-stored primitive. This
    //    runs the tool's `execute` body on the live plane. structured-output's
    //    execute is PURE: it reads `params` and returns a content/details envelope
    //    (with `terminate: true`) — it never touches pi-tui, so the invocation
    //    runs to completion with `ok == true`.
    //
    //    A tool's args array maps to pi's `execute(toolCallId, params, …)`
    //    positionally: slot 0 is the tool-call id, slot 1 is the `params` object.
    //    structured-output reads `params.headline`, so the headline must live in
    //    slot 1. `renderResult` (which uses the pi-tui `Text` marker) is NEVER
    //    exercised on this headless plane.
    let outcome = plane
        .invoke_stored(
            "tool",
            "structured_output",
            &json!([
                "call-1",
                {
                    "headline": "Ship the shim",
                    "summary": "The pi-tui render-stub lets renderer/tool examples load.",
                    "actionItems": ["vendor the example", "add the deno test"]
                }
            ]),
        )
        .await
        .expect("the structured_output tool dispatches through the plane and returns an envelope");
    assert!(
        outcome.ok,
        "structured_output.execute ran to completion (pure, no pi-tui touch), got error {:?}",
        outcome.error
    );
    let text = outcome.result["content"][0]["text"]
        .as_str()
        .expect("the tool returned a text content block");
    assert!(
        text.contains("Saved structured output: Ship the shim"),
        "the tool echoed its `headline` param (proves Type built the schema and the pure execute \
         ran), got {text:?}"
    );

    plane.shutdown().await;
}
