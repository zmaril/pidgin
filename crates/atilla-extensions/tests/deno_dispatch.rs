//! Integration tests for the live hook-DISPATCH engine (PR-F).
//!
//! These load small inline pi-style extensions onto the real embedded
//! `deno_core` runtime, construct an [`ExtensionRunner`], dispatch a hook, and
//! assert the shaped result + error-isolation behavior match pi. They mirror the
//! exact assertions in pi's acceptance suite:
//!
//!   * `extensions-input-event.test.ts` — the `emitInput` cases (continue /
//!     transform / image preserve-vs-replace / chain / handled short-circuit /
//!     source + streamingBehavior passthrough / error-catch-and-continue /
//!     hasHandlers);
//!   * `extensions-runner.test.ts` — `emitBeforeAgentStart` (chained
//!     `ctx.getSystemPrompt()`), `emitToolResult` (content chain + partial-patch
//!     merge), `emitBeforeProviderHeaders` (in-place mutate + throw isolation),
//!     and the `emitContext` error-listener case.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use std::sync::{Arc, Mutex};

use serde_json::json;

use atilla_coding::core::extensions::dispatch::BeforeAgentStartCombinedResult;
use atilla_coding::core::extensions::events::{
    InputEventResult, InputSource, ProjectTrustEventDecision, ProjectTrustEventResult,
    ToolResultEvent, ToolResultEventResult,
};
use atilla_coding::core::extensions::hook::HookEvent;

use atilla_extensions::{ExtensionRunner, JsPlaneHandle, LoadedExtension, SourceLanguage};

/// Spawn a plane, load the given extension sources in order, and build a runner
/// over them. The JS-side handler lists accumulate in load order, matching the
/// per-extension inventory order the runner records.
async fn runner_with(sources: &[&str]) -> ExtensionRunner {
    let plane = JsPlaneHandle::spawn();
    let mut extensions = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        let inventory = plane
            .load_extension_source(format!("e{i}"), *source, SourceLanguage::TypeScript)
            .await
            .expect("extension loads");
        extensions.push(LoadedExtension::new(format!("e{i}.ts"), &inventory));
    }
    ExtensionRunner::new(plane, extensions)
}

// -------------------------------------------------------------------------
// emitInput (extensions-input-event.test.ts)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_continues_on_no_handler_undefined_or_explicit_continue() {
    // No handlers.
    let r = runner_with(&[]).await;
    let result = r
        .emit_input("x", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(result, InputEventResult::Continue);
    r.shutdown().await;

    // Handler returns undefined.
    let r = runner_with(&[r#"export default p => p.on("input", async () => {});"#]).await;
    let result = r
        .emit_input("x", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(result, InputEventResult::Continue);
    r.shutdown().await;

    // Handler returns explicit continue.
    let r = runner_with(&[
        r#"export default p => p.on("input", async () => ({ action: "continue" }));"#,
    ])
    .await;
    let result = r
        .emit_input("x", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(result, InputEventResult::Continue);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_transforms_text_and_preserves_images_when_omitted() {
    let r = runner_with(&[
        r#"export default p => p.on("input", async e => ({ action: "transform", text: "T:" + e.text }));"#,
    ])
    .await;
    let imgs = vec![json!({ "type": "image", "data": "orig", "mimeType": "image/png" })];
    let result = r
        .emit_input("hi", Some(imgs.clone()), InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(
        result,
        InputEventResult::Transform {
            text: "T:hi".into(),
            images: Some(imgs),
        }
    );
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_transforms_and_replaces_images_when_provided() {
    let r = runner_with(&[
        r#"export default p => p.on("input", async () => ({ action: "transform", text: "X", images: [{ type: "image", data: "new", mimeType: "image/jpeg" }] }));"#,
    ])
    .await;
    let result = r
        .emit_input(
            "hi",
            Some(vec![
                json!({ "type": "image", "data": "orig", "mimeType": "image/png" }),
            ]),
            InputSource::Interactive,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        InputEventResult::Transform {
            text: "X".into(),
            images: Some(vec![
                json!({ "type": "image", "data": "new", "mimeType": "image/jpeg" }),
            ]),
        }
    );
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_chains_transforms_across_handlers() {
    let r = runner_with(&[
        r#"export default p => p.on("input", async e => ({ action: "transform", text: e.text + "[1]" }));"#,
        r#"export default p => p.on("input", async e => ({ action: "transform", text: e.text + "[2]" }));"#,
    ])
    .await;
    let result = r
        .emit_input("X", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(
        result,
        InputEventResult::Transform {
            text: "X[1][2]".into(),
            images: None,
        }
    );
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_short_circuits_on_handled_and_skips_subsequent_handlers() {
    let r = runner_with(&[
        r#"export default p => p.on("input", async () => ({ action: "handled" }));"#,
        r#"export default p => p.on("input", async () => { globalThis.testVar = true; });"#,
    ])
    .await;
    // Seed the flag so we can prove the second handler never ran.
    r.plane().eval("globalThis.testVar = false").await.unwrap();

    let result = r
        .emit_input("X", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(result, InputEventResult::Handled);

    let test_var = r.plane().eval("globalThis.testVar").await.unwrap();
    assert_eq!(test_var, json!(false), "second handler must not have run");
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_passes_source_correctly_for_all_source_types() {
    let r = runner_with(&[
        r#"export default p => p.on("input", async e => { globalThis.testVar = e.source; return { action: "continue" }; });"#,
    ])
    .await;
    for (source, name) in [
        (InputSource::Interactive, "interactive"),
        (InputSource::Rpc, "rpc"),
        (InputSource::Extension, "extension"),
    ] {
        r.emit_input("x", None, source, None).await.unwrap();
        let test_var = r.plane().eval("globalThis.testVar").await.unwrap();
        assert_eq!(test_var, json!(name));
    }
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_passes_streaming_behavior_correctly() {
    use atilla_coding::core::extensions::events::StreamingBehavior;
    let r = runner_with(&[
        r#"export default p => p.on("input", async e => { globalThis.testVar = e.streamingBehavior ?? "none"; return { action: "continue" }; });"#,
    ])
    .await;

    r.emit_input(
        "x",
        None,
        InputSource::Interactive,
        Some(StreamingBehavior::Steer),
    )
    .await
    .unwrap();
    assert_eq!(
        r.plane().eval("globalThis.testVar").await.unwrap(),
        json!("steer")
    );

    r.emit_input(
        "x",
        None,
        InputSource::Interactive,
        Some(StreamingBehavior::FollowUp),
    )
    .await
    .unwrap();
    assert_eq!(
        r.plane().eval("globalThis.testVar").await.unwrap(),
        json!("followUp")
    );

    // Omitted streamingBehavior must arrive as undefined (fixture maps to "none").
    r.emit_input("x", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(
        r.plane().eval("globalThis.testVar").await.unwrap(),
        json!("none")
    );
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_catches_handler_errors_and_continues() {
    let r = runner_with(&[
        r#"export default p => p.on("input", async () => { throw new Error("boom"); });"#,
    ])
    .await;
    let collected: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collected.clone();
    r.on_error(move |e| sink.lock().unwrap().push(e.error.clone()));

    let result = r
        .emit_input("x", None, InputSource::Interactive, None)
        .await
        .unwrap();
    assert_eq!(result, InputEventResult::Continue);

    assert!(collected.lock().unwrap().iter().any(|e| e == "boom"));
    assert!(r.errors().iter().any(|e| e.error == "boom"));
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn has_handlers_returns_correct_value() {
    let r = runner_with(&[]).await;
    assert!(!r.has_handlers(HookEvent::Input));
    r.shutdown().await;

    let r = runner_with(&[r#"export default p => p.on("input", async () => {});"#]).await;
    assert!(r.has_handlers(HookEvent::Input));
    r.shutdown().await;
}

// -------------------------------------------------------------------------
// emitBeforeAgentStart (extensions-runner.test.ts)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_agent_start_keeps_get_system_prompt_in_sync() {
    let ext1 = r#"
        export default function(pi) {
            pi.on("before_agent_start", async (_event, ctx) => {
                return { systemPrompt: ctx.getSystemPrompt() + "\nfirst" };
            });
        }
    "#;
    let ext2 = r#"
        export default function(pi) {
            pi.on("before_agent_start", async (_event, ctx) => {
                return { systemPrompt: ctx.getSystemPrompt() + "\nsecond" };
            });
        }
    "#;
    let r = runner_with(&[ext1, ext2]).await;
    let chained = r
        .emit_before_agent_start("hello", None, "base", json!({ "cwd": "/tmp" }))
        .await
        .unwrap();

    assert_eq!(r.errors(), vec![]);
    assert_eq!(
        chained,
        Some(BeforeAgentStartCombinedResult {
            messages: None,
            system_prompt: Some("base\nfirst\nsecond".into()),
        })
    );
    r.shutdown().await;
}

// -------------------------------------------------------------------------
// emitToolResult (extensions-runner.test.ts)
// -------------------------------------------------------------------------

fn base_tool_result(call_id: &str) -> ToolResultEvent {
    ToolResultEvent {
        tool_call_id: call_id.into(),
        tool_name: "my_tool".into(),
        input: json!({}),
        content: vec![json!({ "type": "text", "text": "base" })],
        is_error: false,
        details: json!({ "initial": true }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_chains_content_modifications_across_handlers() {
    let ext1 = r#"
        export default function(pi) {
            pi.on("tool_result", async (event) => {
                return { content: [...event.content, { type: "text", text: "ext1" }] };
            });
        }
    "#;
    let ext2 = r#"
        export default function(pi) {
            pi.on("tool_result", async (event) => {
                return { content: [...event.content, { type: "text", text: "ext2" }] };
            });
        }
    "#;
    let r = runner_with(&[ext1, ext2]).await;
    let chained = r
        .emit_tool_result(base_tool_result("call-1"))
        .await
        .unwrap()
        .expect("modified");

    let content = chained.content.expect("content");
    assert_eq!(content.len(), 3);
    assert_eq!(content[0], json!({ "type": "text", "text": "base" }));
    let mut appended: Vec<String> = content[1..]
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();
    appended.sort();
    assert_eq!(appended, vec!["ext1", "ext2"]);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_preserves_previous_modifications_on_partial_patch() {
    let ext1 = r#"
        export default function(pi) {
            pi.on("tool_result", async () => {
                return { content: [{ type: "text", text: "first" }], details: { source: "ext1" } };
            });
        }
    "#;
    let ext2 = r#"
        export default function(pi) {
            pi.on("tool_result", async () => {
                return { isError: true };
            });
        }
    "#;
    let r = runner_with(&[ext1, ext2]).await;
    let chained = r
        .emit_tool_result(base_tool_result("call-2"))
        .await
        .unwrap();

    assert_eq!(
        chained,
        Some(ToolResultEventResult {
            content: Some(vec![json!({ "type": "text", "text": "first" })]),
            details: Some(json!({ "source": "ext1" })),
            is_error: Some(true),
        })
    );
    r.shutdown().await;
}

// -------------------------------------------------------------------------
// emitBeforeProviderHeaders (extensions-runner.test.ts)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_provider_headers_mutates_in_place_and_preserves_existing() {
    let r = runner_with(&[r#"
        export default function(pi) {
            pi.on("before_provider_headers", (event) => {
                event.headers["X-Turn-Index"] = "3";
            });
        }
    "#])
    .await;
    assert!(r.has_handlers(HookEvent::BeforeProviderHeaders));

    let headers = r
        .emit_before_provider_headers(json!({ "User-Agent": "kimchi/1.0" }))
        .await
        .unwrap();
    assert_eq!(headers["X-Turn-Index"], json!("3"));
    assert_eq!(headers["User-Agent"], json!("kimchi/1.0"));
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_provider_headers_isolates_a_throwing_handler() {
    let throwing = r#"
        export default function(pi) {
            pi.on("before_provider_headers", () => { throw new Error("header handler boom"); });
        }
    "#;
    let good = r#"
        export default function(pi) {
            pi.on("before_provider_headers", (event) => { event.headers["X-Good"] = "yes"; });
        }
    "#;
    let r = runner_with(&[throwing, good]).await;

    let headers = r
        .emit_before_provider_headers(json!({ "User-Agent": "x" }))
        .await
        .unwrap();

    assert_eq!(headers["X-Good"], json!("yes"));
    assert_eq!(headers["User-Agent"], json!("x"));
    let errors = r.errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].event, "before_provider_headers");
    assert!(errors[0].error.contains("header handler boom"));
    r.shutdown().await;
}

// -------------------------------------------------------------------------
// emitContext error isolation (extensions-runner.test.ts)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_calls_error_listeners_when_handler_throws() {
    let r = runner_with(&[r#"
        export default function(pi) {
            pi.on("context", async () => { throw new Error("Handler error!"); });
        }
    "#])
    .await;

    let messages = r.emit_context(vec![]).await.unwrap();
    assert_eq!(messages, Vec::<serde_json::Value>::new());

    let errors = r.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].error.contains("Handler error!"));
    assert_eq!(errors[0].event, "context");
    r.shutdown().await;
}

// -------------------------------------------------------------------------
// emitProjectTrustEvent (extensions-runner.test.ts)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_trust_continues_past_undecided_and_returns_first_decision() {
    // Mirrors extensions-runner.test.ts "project_trust": an undecided handler
    // falls through to a decided one, and the emitter yields
    // { trusted: "no", remember: true } with no errors.
    let undecided = r#"
        export default function(pi) {
            pi.on("project_trust", () => ({ trusted: "undecided", remember: true }));
        }
    "#;
    let decided = r#"
        export default function(pi) {
            pi.on("project_trust", () => ({ trusted: "no", remember: true }));
        }
    "#;
    let r = runner_with(&[undecided, decided]).await;

    let result = r.emit_project_trust("/tmp/project").await.unwrap();

    assert_eq!(
        result,
        Some(ProjectTrustEventResult {
            trusted: ProjectTrustEventDecision::No,
            remember: Some(true),
        })
    );
    assert_eq!(r.errors(), vec![]);
    r.shutdown().await;
}
