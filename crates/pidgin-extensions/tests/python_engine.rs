//! Offline integration test for the PyO3-backed Python extension engine.
//!
//! Writes a small pi-style Python extension to a tempdir, loads it through the
//! [`PythonExtensionLoader`], asserts the produced host records mirror the deno
//! engine's, then builds a `PythonExtensionRunner` from the loader's runtime (no
//! re-import) and drives the three wired paths: `emit_tool_call` (block on `rm
//! -rf`, `None` otherwise), the `task` command handler, and the `session_start`
//! hook. The handlers write sentinel files so the test can prove they actually ran.
//!
//! The load/registration/guardrail scaffolding it shares with
//! [`task_list_example`](../task_list_example) lives in the [`python_support`]
//! module; this file keeps only its unique tempdir fixture and sentinel checks.
//!
//! No network, no API key, no V8 — libpython is embedded via PyO3's
//! `auto-initialize`, so the whole file builds and runs in-sandbox. Gated on the
//! `python` feature (the crate is empty without a feature).
#![cfg(feature = "python")]

mod python_support;

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

    // ---- load + build the runner + assert registration parity ------------
    let runner = python_support::load_runner(&ext_path_str, dir.to_str().unwrap());
    python_support::assert_registration_parity(runner.as_ref(), "List tasks");

    // A registered but stubbed event -> has_handlers must be false. (This is the
    // engine-specific gate; the shipped-example twin stubs `tool_result` instead.)
    assert!(
        !runner.has_handlers("input"),
        "input is registered but stubbed, so has_handlers must be false"
    );

    // ---- WIRED emit_tool_call: block on rm -rf, None otherwise -----------
    python_support::assert_tool_call_guardrail(
        runner.as_ref(),
        "refusing to run a destructive command",
    );

    // ---- WIRED command handler: invoking `task` runs task_handler --------
    python_support::run_task_command(runner.as_ref(), "add milk");
    let task_marker = std::fs::read_to_string(marker_dir.join("task.txt"))
        .expect("task handler wrote its sentinel");
    assert_eq!(task_marker, "add milk");

    // ---- WIRED session_start hook via the generic emit -------------------
    python_support::emit_session_start(runner.as_ref());
    let session_marker = std::fs::read_to_string(marker_dir.join("session_start.txt"))
        .expect("session_start handler wrote its sentinel");
    assert_eq!(session_marker, "startup");

    let _ = std::fs::remove_dir_all(&dir);
}
