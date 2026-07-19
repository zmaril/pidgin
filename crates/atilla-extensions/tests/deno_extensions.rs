//! Integration tests for the JS extension plane's module loading + the
//! `ExtensionAPI` registration bindings (PR-E).
//!
//! These load small inline pi-style extensions on the real embedded `deno_core`
//! runtime and assert that each registration lands in the Rust [`Inventory`],
//! and that malformed / throwing / default-less extensions produce pi's load
//! errors. They mirror the discovery-test cases PR-D deferred to the
//! JS-execution plane:
//!
//!   * registers tools / commands / hooks (handlers) / renderers / shortcuts /
//!     flags;
//!   * reports an error for invalid code, a factory that throws, and an
//!     extension with no valid default export.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use atilla_extensions::{Inventory, JsPlaneHandle, SourceLanguage};

/// Spawn a plane, load one TypeScript extension, shut the plane down, and return
/// the resulting inventory (or the pi-style load error as a string).
async fn load_ts(source: &str) -> Result<Inventory, String> {
    let plane = JsPlaneHandle::spawn();
    let result = plane
        .load_extension_source("fixture", source, SourceLanguage::TypeScript)
        .await
        .map_err(|e| e.to_string());
    plane.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_a_tool() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerTool({
                name: "greet",
                label: "Greet",
                description: "Greets a user",
                parameters: { type: "object", properties: { name: { type: "string" } } },
                execute: async (_id, _args) => ({ content: [{ type: "text", text: "hi" }] }),
            });
        };
        "#,
    )
    .await
    .expect("tool extension loads");

    assert_eq!(inv.tools.len(), 1);
    let tool = &inv.tools[0];
    assert_eq!(tool.name, "greet");
    assert_eq!(tool.label, "Greet");
    assert_eq!(tool.description, "Greets a user");
    assert_eq!(tool.parameters["type"], "object");
    assert_eq!(tool.parameters["properties"]["name"]["type"], "string");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_label_defaults_to_name() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerTool({
                name: "noLabel",
                description: "no explicit label",
                parameters: {},
                execute: async () => ({ content: [] }),
            });
        };
        "#,
    )
    .await
    .expect("tool extension loads");

    assert_eq!(inv.tools.len(), 1);
    assert_eq!(inv.tools[0].name, "noLabel");
    assert_eq!(inv.tools[0].label, "noLabel");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_hooks_in_order() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.on("tool_call", async (_event, _ctx) => {});
            pi.on("input", async (_event) => {});
            pi.on("tool_call", async (_event) => {});
        };
        "#,
    )
    .await
    .expect("hook extension loads");

    assert_eq!(inv.hooks.len(), 3);
    assert_eq!(inv.hooks[0].event, "tool_call");
    assert_eq!(inv.hooks[1].event, "input");
    assert_eq!(inv.hooks[2].event, "tool_call");
    assert_eq!(inv.hook_events(), vec!["tool_call", "input"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_a_command() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerCommand("hello", {
                description: "say hello",
                handler: async (_args, _ctx) => {},
            });
        };
        "#,
    )
    .await
    .expect("command extension loads");

    assert_eq!(inv.commands.len(), 1);
    assert_eq!(inv.commands[0].name, "hello");
    assert_eq!(inv.commands[0].description.as_deref(), Some("say hello"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_a_shortcut() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerShortcut("ctrl+g", {
                description: "go to line",
                handler: () => {},
            });
        };
        "#,
    )
    .await
    .expect("shortcut extension loads");

    assert_eq!(inv.shortcuts.len(), 1);
    assert_eq!(inv.shortcuts[0].shortcut, "ctrl+g");
    assert_eq!(inv.shortcuts[0].description.as_deref(), Some("go to line"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_flags_and_reads_them_back() {
    // The factory itself calls getFlag and throws if the default is wrong, so a
    // successful load proves the getFlag round-trip through the op.
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerFlag("verbose", { type: "boolean", default: true });
            pi.registerFlag("mode", { type: "string", default: "fast" });
            if (pi.getFlag("verbose") !== true) throw new Error("verbose default wrong");
            if (pi.getFlag("mode") !== "fast") throw new Error("mode default wrong");
            if (pi.getFlag("missing") !== undefined) throw new Error("missing flag should be undefined");
        };
        "#,
    )
    .await
    .expect("flag extension loads");

    assert_eq!(inv.flags.len(), 2);
    assert_eq!(inv.flags[0].name, "verbose");
    assert_eq!(inv.flags[0].flag_type, "boolean");
    assert_eq!(inv.flag_value("verbose"), Some(serde_json::json!(true)));
    assert_eq!(inv.flag_value("mode"), Some(serde_json::json!("fast")));
    assert_eq!(inv.flag_value("missing"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_message_and_entry_renderers() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerMessageRenderer("custom-msg", (_data) => "msg");
            pi.registerEntryRenderer("custom-entry", (_data) => "entry");
        };
        "#,
    )
    .await
    .expect("renderer extension loads");

    assert_eq!(inv.message_renderers.len(), 1);
    assert_eq!(inv.message_renderers[0].custom_type, "custom-msg");
    assert_eq!(inv.entry_renderers.len(), 1);
    assert_eq!(inv.entry_renderers[0].custom_type, "custom-entry");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_factory_can_register_everything() {
    let inv = load_ts(
        r#"
        export default (pi) => {
            pi.registerTool({ name: "t", parameters: {}, execute: async () => ({ content: [] }) });
            pi.on("input", async () => {});
            pi.registerCommand("c", { handler: async () => {} });
            pi.registerShortcut("ctrl+x", { handler: () => {} });
            pi.registerFlag("f", { type: "string", default: "v" });
            pi.registerMessageRenderer("m", () => {});
            pi.registerEntryRenderer("e", () => {});
        };
        "#,
    )
    .await
    .expect("combined extension loads");

    assert_eq!(inv.tools.len(), 1);
    assert_eq!(inv.hooks.len(), 1);
    assert_eq!(inv.commands.len(), 1);
    assert_eq!(inv.shortcuts.len(), 1);
    assert_eq!(inv.flags.len(), 1);
    assert_eq!(inv.message_renderers.len(), 1);
    assert_eq!(inv.entry_renderers.len(), 1);
    assert!(!inv.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_async_factory_is_awaited() {
    // A factory that awaits before registering must still have its registrations
    // collected — the loader drives the event loop to the factory's completion.
    let inv = load_ts(
        r#"
        export default async (pi) => {
            await Promise.resolve();
            pi.on("tool_call", async () => {});
        };
        "#,
    )
    .await
    .expect("async factory loads");

    assert_eq!(inv.hooks.len(), 1);
    assert_eq!(inv.hooks[0].event, "tool_call");
}

// -------------------------------------------------------------------------
// Error reporting (pi's three load-error cases).
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_error_for_invalid_code() {
    let err = load_ts("export default (pi) => { this is not valid !!! syntax")
        .await
        .expect_err("invalid code must fail to load");
    assert!(
        err.contains("Failed to load extension"),
        "unexpected error: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_error_when_factory_throws() {
    let err = load_ts(r#"export default (pi) => { throw new Error("boom during init"); };"#)
        .await
        .expect_err("a throwing factory must fail to load");
    assert!(
        err.contains("Failed to load extension"),
        "unexpected error: {err}"
    );
    assert!(err.contains("boom during init"), "unexpected error: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_error_when_no_default_export() {
    let err = load_ts("export const notTheDefault = (pi) => {};")
        .await
        .expect_err("missing default export must fail to load");
    assert!(
        err.contains("does not export a valid factory function"),
        "unexpected error: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_error_when_default_is_not_a_function() {
    let err = load_ts("export default 42;")
        .await
        .expect_err("a non-function default must fail to load");
    assert!(
        err.contains("does not export a valid factory function"),
        "unexpected error: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plane_survives_a_failed_load() {
    // A failed load must not poison the runtime: a subsequent good load works.
    let plane = JsPlaneHandle::spawn();

    let bad = plane
        .load_extension_source(
            "bad",
            r#"export default () => { throw new Error("nope"); };"#,
            SourceLanguage::TypeScript,
        )
        .await;
    assert!(bad.is_err());

    let good = plane
        .load_extension_source(
            "good",
            r#"export default (pi) => { pi.on("input", async () => {}); };"#,
            SourceLanguage::TypeScript,
        )
        .await
        .expect("good load after a failed one");
    assert_eq!(good.hooks.len(), 1);

    plane.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_a_plain_javascript_extension() {
    // A JavaScript entrypoint skips transpile and evaluates directly.
    let plane = JsPlaneHandle::spawn();
    let inv = plane
        .load_extension_source(
            "js-ext",
            r#"export default (pi) => { pi.registerCommand("jsc", {}); };"#,
            SourceLanguage::JavaScript,
        )
        .await
        .expect("javascript extension loads");
    assert_eq!(inv.commands.len(), 1);
    assert_eq!(inv.commands[0].name, "jsc");
    plane.shutdown().await;
}
