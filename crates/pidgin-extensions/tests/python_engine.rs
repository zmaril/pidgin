//! Offline integration test for the PyO3-backed Python extension engine.
//!
//! Writes a small pi-style Python extension to a tempdir, loads it through the
//! [`PythonExtensionLoader`], asserts the produced host records mirror the deno
//! engine's, then builds a [`PythonExtensionRunner`] from the loader's runtime (no
//! re-import) and drives the three wired paths: `emit_tool_call` (block on `rm
//! -rf`, `None` otherwise), the `task` command handler, and the `session_start`
//! hook. The handlers write sentinel files so the test can prove they actually ran.
//!
//! No network, no API key, no V8 — libpython is embedded via PyO3's
//! `auto-initialize`, so the whole file builds and runs in-sandbox. Gated on the
//! `python` feature (the crate is empty without a feature).
#![cfg(feature = "python")]

use serde_json::json;

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::command::CommandContext;
use pidgin_coding::core::extensions::events::session::{SessionStartEvent, SessionStartReason};
use pidgin_coding::core::extensions::events::tool::ToolCallEvent;
use pidgin_coding::core::extensions::loader::ExtensionLoader;
use pidgin_coding::core::extensions::runner::ExtensionDispatchEvent;

use pidgin_extensions::{create_python_extension_runner, PythonExtensionLoader};

/// The pi-style Python extension fixture. `{marker_dir}` is interpolated with the
/// tempdir path so the `session_start` and `task` handlers can drop sentinel files
/// the test then observes.
fn fixture(marker_dir: &str) -> String {
    format!(
        r#"
import os

MARKER_DIR = {marker_dir:?}


def extension(pi):
    def on_session_start(event, ctx):
        with open(os.path.join(MARKER_DIR, "session_start.txt"), "w") as handle:
            handle.write(event["reason"])

    pi.on("session_start", on_session_start)

    def task_handler(args, ctx):
        with open(os.path.join(MARKER_DIR, "task.txt"), "w") as handle:
            handle.write(args)

    pi.register_command("task", description="manage the task list", handler=task_handler)

    def list_tasks(args):
        return {{"content": [{{"type": "text", "text": "no tasks"}}], "details": None}}

    pi.register_tool({{
        "name": "list_tasks",
        "label": "List tasks",
        "description": "list the current tasks",
        "parameters": {{"type": "object", "properties": {{}}}},
        "execute": list_tasks,
    }})

    def on_tool_call(event, ctx):
        if "rm -rf" in str(event["input"]):
            return {{"block": True, "reason": "refusing to run a destructive command"}}
        return None

    pi.on("tool_call", on_tool_call)

    # A registered-but-stubbed event: `input` is not one of the wired emitters, so
    # `has_handlers("input")` must be false even though a handler is registered.
    def on_input(text, ctx):
        return None

    pi.on("input", on_input)
"#
    )
}

#[test]
fn python_engine_loads_and_dispatches() {
    let dir = std::env::temp_dir().join(format!("pidgin-py-ext-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker_dir = dir.join("markers");
    std::fs::create_dir_all(&marker_dir).unwrap();

    let ext_path = dir.join("tasks.py");
    std::fs::write(&ext_path, fixture(marker_dir.to_str().unwrap())).unwrap();
    let ext_path_str = ext_path.to_str().unwrap().to_string();

    // ---- load through the ExtensionLoader seam ---------------------------
    let loader = PythonExtensionLoader::new();
    let bus = EventBus::new();
    let result = loader.load_extensions_cached(
        std::slice::from_ref(&ext_path_str),
        dir.to_str().unwrap(),
        &bus,
        None,
    );

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
    let runner = create_python_extension_runner(
        result.extensions.clone(),
        runtime,
        dir.to_str().unwrap().to_string(),
    );

    // Inventory-backed queries.
    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool.name, "list_tasks");
    assert_eq!(tools[0].tool.label, "List tasks");

    let commands = runner.get_registered_commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].invocation_name, "task");

    // has_handlers gating: wired events with a handler -> true; a registered but
    // stubbed event -> false.
    assert!(runner.has_handlers("tool_call"), "tool_call is wired");
    assert!(
        runner.has_handlers("session_start"),
        "session_start is wired"
    );
    assert!(
        !runner.has_handlers("input"),
        "input is registered but stubbed, so has_handlers must be false"
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
        Some("refusing to run a destructive command")
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

    // ---- WIRED command handler: invoking `task` runs task_handler --------
    let command = runner.get_command("task").expect("task command resolves");
    let ctx: Box<dyn CommandContext> = runner.create_command_context();
    (command.command.handler)("add milk", ctx.as_ref()).expect("task handler runs");
    let task_marker = std::fs::read_to_string(marker_dir.join("task.txt"))
        .expect("task handler wrote its sentinel");
    assert_eq!(task_marker, "add milk");

    // ---- WIRED session_start hook via the generic emit -------------------
    runner.emit(&ExtensionDispatchEvent::SessionStart(SessionStartEvent {
        reason: SessionStartReason::Startup,
        previous_session_file: None,
    }));
    let session_marker = std::fs::read_to_string(marker_dir.join("session_start.txt"))
        .expect("session_start handler wrote its sentinel");
    assert_eq!(session_marker, "startup");

    let _ = std::fs::remove_dir_all(&dir);
}
