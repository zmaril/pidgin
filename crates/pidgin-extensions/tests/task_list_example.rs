//! Offline parity test that loads the in-repo Python `task-list-py` example
//! through the real Python extension engine.
//!
//! Unlike [`python_engine`](../python_engine), which writes a throwaway fixture
//! to a tempdir, this test points the loader at the SHIPPED example file
//! (`examples/extensions/task-list-py/index.py`) — the Python twin of the JS
//! `task-list` example (#188) — and proves it registers and dispatches through
//! the `--features python` engine. The path is resolved from `CARGO_MANIFEST_DIR`
//! so it works in-tree and in CI without a cwd assumption.
//!
//! The load/registration/guardrail scaffolding it shares with `python_engine`
//! lives in the [`python_support`] module; this file keeps only its unique
//! example-path resolution and its task-count observations.
//!
//! It asserts registration parity (command `task`, tool `list_tasks`, hooks
//! `session_start` + `tool_call`) and drives the wired paths with no network and
//! no API key: `emit_tool_call` blocks a bash `rm -rf /` and lets a benign
//! command through, and invoking the `task` command adds a task (observed back
//! through the `list_tasks` tool's count).
#![cfg(feature = "python")]

mod python_support;

use std::path::PathBuf;

use serde_json::{json, Value};

use pidgin_coding::core::extensions::types::ExtensionContext;

/// A trivial [`ExtensionContext`] for driving the `list_tasks` tool's `execute`
/// closure (the engine ignores the ctx, but the seam signature requires one).
struct NoopCtx;
impl ExtensionContext for NoopCtx {}

/// Absolute path to the shipped example, resolved relative to this crate so the
/// test is cwd-independent (works in-tree and in the CI `python` job).
fn example_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/extensions/task-list-py/index.py");
    path.canonicalize()
        .unwrap_or_else(|error| panic!("example path {path:?} must exist: {error}"))
}

/// Run the `list_tasks` tool and return its result as JSON (avoids naming the
/// `ContentBlock` enum; the shape is what the extension author returns).
fn run_list_tasks(runner: &dyn pidgin_coding::core::extensions::runner::ExtensionRunner) -> Value {
    let tools = runner.get_all_registered_tools();
    let tool = tools
        .iter()
        .find(|registered| registered.tool.name == "list_tasks")
        .expect("list_tasks tool present");
    let ctx = NoopCtx;
    let result = (tool.tool.execute)("call-list", &json!({}), None, None, &ctx);
    serde_json::to_value(&result).expect("tool result serializes")
}

#[test]
fn task_list_example_loads_and_dispatches() {
    let ext_path = example_path();
    let ext_path_str = ext_path.to_str().unwrap().to_string();
    let cwd = ext_path.parent().unwrap().to_str().unwrap().to_string();

    // ---- load the SHIPPED example + build runner + assert parity ---------
    let runner = python_support::load_runner(&ext_path_str, &cwd);
    python_support::assert_registration_parity(runner.as_ref(), "List Tasks");

    // Engine-specific has_handlers gate: an event with no registered handler (a
    // stub for this example) -> false. (The tempdir twin stubs `input` instead.)
    assert!(
        !runner.has_handlers("tool_result"),
        "tool_result has no handler here, so has_handlers must be false"
    );

    // ---- WIRED emit_tool_call: block on rm -rf, None otherwise -----------
    python_support::assert_tool_call_guardrail(
        runner.as_ref(),
        "Blocked destructive `rm -rf` command by task-list guardrail",
    );

    // A non-bash tool call is never blocked (mirrors the JS early return); this
    // is unique to the shipped example's guardrail.
    let other_tool = pidgin_coding::core::extensions::events::tool::ToolCallEvent {
        tool_call_id: "call-3".to_string(),
        tool_name: "read".to_string(),
        input: json!({ "command": "rm -rf /" }),
    };
    assert!(
        runner.emit_tool_call(&other_tool).is_none(),
        "a non-bash tool is out of the guardrail's scope"
    );

    // ---- WIRED session_start hook via the generic emit -------------------
    // The example's handler only notifies (ctx is None offline, so it no-ops);
    // this proves the dispatch path runs without unwinding.
    python_support::emit_session_start(runner.as_ref());

    // ---- WIRED command handler: invoking `task` adds a task --------------
    // Observe the count through the list_tasks tool before and after.
    let before = run_list_tasks(runner.as_ref());
    assert_eq!(before["details"]["total"], json!(0), "no tasks initially");
    assert_eq!(
        before["content"][0]["text"],
        json!("No tasks yet. Add one with /task <text>.")
    );

    python_support::run_task_command(runner.as_ref(), "write the report");

    let after = run_list_tasks(runner.as_ref());
    assert_eq!(after["details"]["total"], json!(1), "one task after /task");
    assert_eq!(after["details"]["count"], json!(1));
    assert_eq!(after["content"][0]["text"], json!("#1: write the report"));
}
