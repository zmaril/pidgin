//! Integration test proving a REAL, unmodified upstream pi example extension
//! RUNS through pidgin's merged extension plane — not merely that it registers.
//!
//! The subject is `examples/extensions/pirate/index.ts`, vendored verbatim from
//! pi's `packages/coding-agent/examples/extensions/pirate.ts` (MIT, see
//! `examples/extensions/NOTICE`). It registers a `/pirate` slash command that
//! toggles a module-scoped `pirateMode` flag, and a `before_agent_start` hook
//! that appends pirate-speak to the system prompt while that flag is on. Both
//! closures capture the same `pirateMode`, so the command's side effect is
//! observable through the hook.
//!
//! The single test drives the full command -> hook interaction on ONE live
//! plane (shared state must persist across the steps):
//!
//!   1. Load the vendored file through the plane (loads clean: the only import
//!      is `import type`, erased at transpile — no bare-specifier module).
//!   2. Assert it registered command `pirate` + a `before_agent_start` handler.
//!   3. Fire `before_agent_start` — pirate mode defaults off, prompt UNCHANGED.
//!   4. Invoke the `/pirate` command through the real invoke-stored primitive
//!      (this executes the handler body, flipping the shared flag).
//!   5. Fire `before_agent_start` again — the hook now reads the flipped flag
//!      and the returned system prompt CONTAINS pirate's injected text.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use serde_json::json;

use pidgin_extensions::{ExtensionRunner, JsPlaneHandle, LoadedExtension, SourceLanguage};

/// The vendored upstream example, resolved relative to this crate.
const PIRATE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/extensions/pirate/index.ts"
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vendored_pirate_command_toggles_flag_that_before_agent_start_hook_reads() {
    // 1. Load the verbatim upstream file onto the real plane. It loads clean
    //    because its sole import is a type-only import (erased at transpile), so
    //    there is no bare specifier for pidgin's loader to resolve.
    let source = std::fs::read_to_string(PIRATE_PATH).expect("read vendored pirate extension");
    let plane = JsPlaneHandle::spawn();
    let inventory = plane
        .load_extension_source("pirate", source, SourceLanguage::TypeScript)
        .await
        .expect("vendored pirate extension loads clean (type-only import)");

    // 2. It registered exactly what pirate.ts declares.
    assert!(
        inventory.commands.iter().any(|c| c.name == "pirate"),
        "registers the /pirate command"
    );
    assert!(
        inventory
            .hooks
            .iter()
            .any(|h| h.event == "before_agent_start"),
        "registers a before_agent_start hook"
    );

    let runner = ExtensionRunner::new(
        plane,
        vec![LoadedExtension::new(
            "examples/extensions/pirate/index.ts",
            &inventory,
        )],
    );

    const BASE: &str = "You are a helpful coding assistant.";
    // A distinctive substring lifted from pirate.ts's injected instructions.
    const PIRATE_MARKER: &str = "You are now in PIRATE MODE";

    // 3. Pirate mode defaults off: the hook returns undefined, so the fold
    //    contributes nothing and the system prompt is left UNCHANGED (`None`).
    let before = runner
        .emit_before_agent_start("hi", None, BASE, json!({ "cwd": "/project" }))
        .await
        .unwrap();
    assert_eq!(
        before, None,
        "pirate mode defaults off, so before_agent_start leaves the prompt unchanged"
    );
    assert_eq!(runner.errors(), vec![]);

    // 4. Invoke the /pirate command through the REAL one-shot invoke-stored
    //    primitive — the same primitive the runner's command dispatch uses. This
    //    runs the handler body on the live plane: `pirateMode = !pirateMode`
    //    executes and flips the module-scoped flag both closures capture.
    //
    //    The handler then calls `ctx.ui.notify(...)`. invoke_stored now threads a
    //    real JS `ctx` into the command handler (built by the shared makeContext,
    //    whose `ui.notify` is a faithful no-op sink), so `ctx.ui.notify(...)`
    //    returns without throwing and the envelope is `ok == true`. The flag
    //    toggle also ran, so the shared state the hook reads is mutated — which
    //    step 5 proves end-to-end.
    let inv = runner
        .plane()
        .invoke_stored("command", "pirate", &json!([""]))
        .await
        .expect("the /pirate command dispatches through the plane and returns an envelope");
    assert!(
        inv.ok,
        "ctx.ui.notify now exists and does not throw: {:?}",
        inv.error
    );

    // 5. Fire before_agent_start again. The hook now reads pirateMode == true and
    //    appends its pirate-speak block — proving the command executed, mutated
    //    shared state, and the hook actually changed agent behavior (live
    //    dispatch, not registration).
    let after = runner
        .emit_before_agent_start("hi", None, BASE, json!({ "cwd": "/project" }))
        .await
        .unwrap()
        .expect("pirate mode on: the hook now contributes a modified system prompt");
    let prompt = after
        .system_prompt
        .expect("the modified system prompt is present");
    assert!(
        prompt.starts_with(BASE),
        "pirate's instructions are appended to the base prompt"
    );
    assert!(
        prompt.contains(PIRATE_MARKER),
        "the command's flag toggle took effect: the hook injected pirate-speak \
         (expected substring {PIRATE_MARKER:?}), got: {prompt:?}"
    );

    runner.shutdown().await;
}
