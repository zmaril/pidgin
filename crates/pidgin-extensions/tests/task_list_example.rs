//! Offline parity test that loads the in-repo Python `task-list-py` example
//! through the real Python extension engine.
//!
//! Unlike [`python_engine`](super), which writes a throwaway fixture to a
//! tempdir, this test points the loader at the SHIPPED example file
//! (`examples/extensions/task-list-py/index.py`) — the Python twin of the JS
//! `task-list` example (#188) — and proves it registers and dispatches through
//! the `--features python` engine. The path is resolved from `CARGO_MANIFEST_DIR`
//! so it works in-tree and in CI without a cwd assumption.
//!
//! It asserts registration parity (command `task`, tool `list_tasks`, hooks
//! `session_start` + `tool_call`) and drives the wired paths with no network and
//! no API key: `emit_tool_call` blocks a bash `rm -rf /` and lets a benign
//! command through, and invoking the `task` command adds a task (observed back
//! through the `list_tasks` tool's count).
#![cfg(feature = "python")]

use std::path::PathBuf;

use serde_json::{json, Value};

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::command::CommandContext;
use pidgin_coding::core::extensions::events::session::{SessionStartEvent, SessionStartReason};
use pidgin_coding::core::extensions::events::tool::ToolCallEvent;
use pidgin_coding::core::extensions::loader::ExtensionLoader;
use pidgin_coding::core::extensions::runner::ExtensionDispatchEvent;
use pidgin_coding::core::extensions::types::ExtensionContext;

use pidgin_extensions::{create_python_extension_runner, PythonExtensionLoader};

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

    // ---- load the SHIPPED example through the ExtensionLoader seam --------
    let loader = PythonExtensionLoader::new();
    let bus = EventBus::new();
    let result =
        loader.load_extensions_cached(std::slice::from_ref(&ext_path_str), &cwd, &bus, None);

    assert!(
        result.errors.is_empty(),
        "unexpected load errors: {:?}",
        result.errors
    );
    assert_eq!(result.extensions.len(), 1, "one extension loaded");
    let extension = &result.extensions[0];
    assert_eq!(extension.path, ext_path_str);
    assert!(
        extension.tool_names().any(|name| name == "list_tasks"),
        "tool list_tasks present, got {:?}",
        extension.tools
    );
    assert!(
        extension.commands.iter().any(|name| name == "task"),
        "command task present, got {:?}",
        extension.commands
    );

    let runtime = result.runtime.expect("loader mints a runtime");

    // ---- build the runner via the factory, reusing the runtime -----------
    let runner = create_python_extension_runner(result.extensions.clone(), runtime, cwd);

    // Inventory-backed queries: registration parity with the JS twin.
    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool.name, "list_tasks");
    assert_eq!(tools[0].tool.label, "List Tasks");

    let commands = runner.get_registered_commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].invocation_name, "task");

    // has_handlers gating: wired events with a handler -> true; an event with no
    // registered handler (a stub for this example) -> false.
    assert!(runner.has_handlers("tool_call"), "tool_call is wired");
    assert!(
        runner.has_handlers("session_start"),
        "session_start is wired"
    );
    assert!(
        !runner.has_handlers("tool_result"),
        "tool_result has no handler here, so has_handlers must be false"
    );

    // ---- WIRED emit_tool_call: block on rm -rf, None otherwise -----------
    let dangerous = ToolCallEvent {
        tool_call_id: "call-1".to_string(),
        tool_name: "bash".to_string(),
        input: json!({ "command": "rm -rf /" }),
    };
    let decision = runner
        .emit_tool_call(&dangerous)
        .expect("a block decision for rm -rf");
    assert_eq!(decision.block, Some(true));
    assert_eq!(
        decision.reason.as_deref(),
        Some("Blocked destructive `rm -rf` command by task-list guardrail")
    );

    let benign = ToolCallEvent {
        tool_call_id: "call-2".to_string(),
        tool_name: "bash".to_string(),
        input: json!({ "command": "ls -la" }),
    };
    assert!(
        runner.emit_tool_call(&benign).is_none(),
        "benign command is not blocked"
    );

    // A non-bash tool call is never blocked (mirrors the JS early return).
    let other_tool = ToolCallEvent {
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
    runner.emit(&ExtensionDispatchEvent::SessionStart(SessionStartEvent {
        reason: SessionStartReason::Startup,
        previous_session_file: None,
    }));

    // ---- WIRED command handler: invoking `task` adds a task --------------
    // Observe the count through the list_tasks tool before and after.
    let before = run_list_tasks(runner.as_ref());
    assert_eq!(before["details"]["total"], json!(0), "no tasks initially");
    assert_eq!(
        before["content"][0]["text"],
        json!("No tasks yet. Add one with /task <text>.")
    );

    let command = runner.get_command("task").expect("task command resolves");
    let ctx: Box<dyn CommandContext> = runner.create_command_context();
    (command.command.handler)("write the report", ctx.as_ref()).expect("task handler runs");

    let after = run_list_tasks(runner.as_ref());
    assert_eq!(after["details"]["total"], json!(1), "one task after /task");
    assert_eq!(after["details"]["count"], json!(1));
    assert_eq!(after["content"][0]["text"], json!("#1: write the report"));
}
