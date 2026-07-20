//! Offline (python-only) integration test for the COMBINED deno+python
//! extension loader.
//!
//! Gated `all(feature = "python", not(feature = "deno"))` so it runs ONLY in the
//! offline python-only build — never in the V8 job (V8's blob 403s in-sandbox, so
//! the deno + deno,python paths are compiled/verified by CI, not here). This file
//! covers the fully-offline-verifiable slice of the combined loader:
//!
//!   1. a `.py` extension loads through `CombinedExtensionLoader` and its command
//!      / tool / hooks register, and the combined RUNNER's `has_handlers` +
//!      `emit_tool_call` fan out to the python inner;
//!   2. a `.ts` path degrades GRACEFULLY to an `ExtensionLoadError` containing
//!      "not compiled in" (NOT a panic, NOT a load), because the deno engine is
//!      absent from this build;
//!   3. two python extensions registering the SAME tool name, loaded THROUGH the
//!      orchestrator (`DefaultResourceLoader`) with the combined loader injected,
//!      trip the tool-conflict error EXACTLY ONCE. The combined loader returns the
//!      full union with NO loader-level conflict diagnostic; the orchestrator's
//!      `add_extension_conflict_diagnostics` is the single source of truth.
#![cfg(all(feature = "python", not(feature = "deno")))]

// straitjacket-allow-file:duplication -- the fixture text, the load-then-assert
// shape, and the `emit_tool_call` guardrail drive (block `rm -rf`, pass a benign
// command) are transcribed from the python engine's own tests
// (tests/python_engine.rs, tests/python_support/mod.rs): both exercise the SAME
// ExtensionLoader/ExtensionRunner seam the SAME way, so the parallel test
// scaffolding is faithful to the shared seam, not incidental repetition.

use std::path::{Path, PathBuf};

use serde_json::json;

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::events::tool::ToolCallEvent;
use pidgin_coding::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ReloadOptions,
};

use pidgin_extensions::{
    create_combined_extension_runner, CombinedExtensionLoader, EngineSelection,
};

/// A pi-style Python extension fixture registering one `task` command, one
/// `list_tasks` tool, a `tool_call` guardrail hook (blocks `rm -rf`), and a
/// `session_start` hook. `{tool_name}` lets a caller vary the tool name to force
/// a cross-extension tool-name collision.
fn fixture(tool_name: &str) -> String {
    format!(
        r#"
def extension(pi):
    def task_handler(args, ctx):
        return None

    pi.register_command("task", description="manage the task list", handler=task_handler)

    def the_tool(args):
        return {{"content": [{{"type": "text", "text": "no tasks"}}], "details": None}}

    pi.register_tool({{
        "name": {tool_name:?},
        "label": "List tasks",
        "description": "list the current tasks",
        "parameters": {{"type": "object", "properties": {{}}}},
        "execute": the_tool,
    }})

    def on_tool_call(event, ctx):
        if "rm -rf" in str(event["input"]):
            return {{"block": True, "reason": "refusing to run a destructive command"}}
        return None

    pi.on("tool_call", on_tool_call)

    def on_session_start(event, ctx):
        return None

    pi.on("session_start", on_session_start)
"#
    )
}

/// A unique tempdir for this test process + a caller-chosen tag.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pidgin-combined-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `fixture(tool_name)` to `<dir>/<name>.py` and return its path string.
fn write_fixture(dir: &Path, name: &str, tool_name: &str) -> String {
    let path = dir.join(format!("{name}.py"));
    std::fs::write(&path, fixture(tool_name)).unwrap();
    path.to_string_lossy().into_owned()
}

/// (1) A `.py` extension loads through the combined loader and dispatches through
/// the combined runner's fan-out (python inner only, on this build).
#[test]
fn combined_loader_loads_python_and_dispatches_through_fanout() {
    let dir = temp_dir("dispatch");
    let ext_path = write_fixture(&dir, "tasks", "list_tasks");

    let loader = CombinedExtensionLoader::spawn(EngineSelection {
        deno: false,
        python: true,
    });
    let bus = EventBus::new();
    let cwd = dir.to_string_lossy().into_owned();
    let result = loader.load_extensions_cached(std::slice::from_ref(&ext_path), &cwd, &bus, None);

    assert!(
        result.errors.is_empty(),
        "unexpected load errors: {:?}",
        result.errors
    );
    assert_eq!(result.extensions.len(), 1, "one extension loaded");
    let extension = &result.extensions[0];
    assert_eq!(extension.path, ext_path);
    assert!(
        extension.tool_names().any(|name| name == "list_tasks"),
        "tool list_tasks registered, got {:?}",
        extension.tools
    );
    assert!(
        extension.commands.iter().any(|name| name == "task"),
        "command task registered, got {:?}",
        extension.commands
    );

    // Build the combined runner from the loader's combined runtime and confirm the
    // python inner's handlers reach through the fan-out.
    let runtime = result.runtime.expect("combined loader mints a runtime");
    let runner = create_combined_extension_runner(result.extensions.clone(), runtime, cwd);

    assert!(
        runner.has_handlers("tool_call"),
        "tool_call is wired through the fan-out"
    );
    assert!(
        runner.has_handlers("session_start"),
        "session_start is wired through the fan-out"
    );
    assert_eq!(runner.get_all_registered_tools().len(), 1);
    assert_eq!(runner.get_registered_commands().len(), 1);

    // The wired emit_tool_call guardrail blocks a destructive bash command...
    let dangerous = ToolCallEvent {
        tool_call_id: "call-1".to_string(),
        tool_name: "bash".to_string(),
        input: json!({ "command": "rm -rf /" }),
    };
    let decision = runner
        .emit_tool_call(&dangerous)
        .expect("a block decision for rm -rf through the fan-out");
    assert_eq!(decision.block, Some(true));
    assert_eq!(
        decision.reason.as_deref(),
        Some("refusing to run a destructive command")
    );

    // ...and lets a benign one pass through unblocked.
    let benign = ToolCallEvent {
        tool_call_id: "call-2".to_string(),
        tool_name: "bash".to_string(),
        input: json!({ "command": "ls -la" }),
    };
    assert!(
        runner.emit_tool_call(&benign).is_none(),
        "benign command is not blocked"
    );
}

/// (2) A `.ts` path degrades GRACEFULLY — a "not compiled in" error, not a panic
/// and not a load — because this build has no deno engine.
#[test]
fn combined_loader_rejects_ts_path_gracefully_when_deno_absent() {
    let loader = CombinedExtensionLoader::spawn(EngineSelection {
        deno: false,
        python: true,
    });
    let bus = EventBus::new();
    let ts_path = "/nonexistent/phantom.ts".to_string();
    let result = loader.load_extensions_cached(std::slice::from_ref(&ts_path), ".", &bus, None);

    assert!(
        result.extensions.is_empty(),
        "a .ts path loads nothing without the deno engine"
    );
    assert_eq!(result.errors.len(), 1, "exactly one graceful error");
    let error = &result.errors[0];
    assert_eq!(error.path, ts_path);
    assert!(
        error.error.contains("not compiled in"),
        "graceful degradation message, got {:?}",
        error.error
    );
    // A runtime is still minted (the seam always returns one).
    assert!(result.runtime.is_some());
}

/// (3) Two python extensions registering the SAME tool name, loaded THROUGH the
/// orchestrator (`DefaultResourceLoader`) with the combined loader injected, trip
/// the tool-conflict error EXACTLY ONCE.
///
/// The combined loader returns the FULL UNION of both extensions with NO
/// loader-level conflict diagnostic; the orchestrator's
/// `add_extension_conflict_diagnostics` is the single source of truth, so the
/// cross-extension collision is reported once — not zero (the loader dropped no
/// duplicate) and not twice (the loader no longer double-reports). This mirrors
/// the within-engine `detect_tool_conflicts_between_extensions` orchestrator test
/// setup, exercising it on the python-only build offline.
#[test]
fn orchestrator_detects_tool_conflict_exactly_once_over_combined_union() {
    let dir = temp_dir("conflict");
    let cwd = dir.join("project");
    let agent = dir.join("agent");
    let home = dir.join("home");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // Two `.py` fixtures OUTSIDE the discovery roots, each registering the same
    // tool name, injected as explicit CLI extension paths.
    let first = write_fixture(&dir, "first", "shared_tool");
    let second = write_fixture(&dir, "second", "shared_tool");

    let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: agent.to_string_lossy().into_owned(),
        home_dir: Some(home.to_string_lossy().into_owned()),
        extension_loader: Some(CombinedExtensionLoader::spawn(EngineSelection {
            deno: false,
            python: true,
        })),
        additional_extension_paths: vec![first.clone(), second.clone()],
        ..Default::default()
    });
    loader.reload(ReloadOptions::default());

    let result = loader.get_extensions();
    // The FULL union of both extensions loads (the loader drops no duplicate)...
    assert_eq!(
        result.extensions.len(),
        2,
        "both extensions load in the union, got {:?}",
        result
            .extensions
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>()
    );
    // ...and the orchestrator emits the tool-conflict error EXACTLY once.
    let conflicts = result
        .errors
        .iter()
        .filter(|error| {
            error.error.contains("shared_tool") && error.error.contains("conflicts with")
        })
        .count();
    assert_eq!(
        conflicts, 1,
        "cross-extension tool conflict reported exactly once, got errors {:?}",
        result.errors
    );
}
