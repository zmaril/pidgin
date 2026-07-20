//! Shared offline scaffolding for the `python`-gated extension-engine tests.
//!
//! Both [`python_engine`](../python_engine) (a throwaway tempdir fixture) and
//! [`task_list_example`](../task_list_example) (the shipped example) load a
//! Python extension through the real [`PythonExtensionLoader`] seam, build a
//! runner via the factory, and assert the same registration + guardrail parity.
//! That common scaffolding lives here so each test keeps only its unique fixture
//! and assertions, rather than cloning the load/assert boilerplate.
#![cfg(feature = "python")]
// Each python test binary uses a subset of these helpers.
#![allow(dead_code)]

use serde_json::json;

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::command::CommandContext;
use pidgin_coding::core::extensions::events::session::{SessionStartEvent, SessionStartReason};
use pidgin_coding::core::extensions::events::tool::ToolCallEvent;
use pidgin_coding::core::extensions::loader::ExtensionLoader;
use pidgin_coding::core::extensions::runner::{ExtensionDispatchEvent, ExtensionRunner};

use pidgin_extensions::{create_python_extension_runner, PythonExtensionLoader};

/// Load a Python extension file through the [`ExtensionLoader`] seam, assert the
/// common registration surface (one `list_tasks` tool, one `task` command), and
/// build the runner via the factory (reusing the loader's runtime, no re-import).
pub fn load_runner(ext_path: &str, cwd: &str) -> Box<dyn ExtensionRunner> {
    let loader = PythonExtensionLoader::new();
    let bus = EventBus::new();
    let result =
        loader.load_extensions_cached(std::slice::from_ref(&ext_path.to_string()), cwd, &bus, None);

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
        "tool list_tasks present, got {:?}",
        extension.tools
    );
    assert!(
        extension.commands.iter().any(|name| name == "task"),
        "command task present, got {:?}",
        extension.commands
    );

    let runtime = result.runtime.expect("loader mints a runtime");
    create_python_extension_runner(result.extensions.clone(), runtime, cwd.to_string())
}

/// Assert the registration parity shared by both engines: a single `list_tasks`
/// tool with `expected_label`, a single `task` command, and the `tool_call` +
/// `session_start` events wired.
pub fn assert_registration_parity(runner: &dyn ExtensionRunner, expected_label: &str) {
    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool.name, "list_tasks");
    assert_eq!(tools[0].tool.label, expected_label);

    let commands = runner.get_registered_commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].invocation_name, "task");

    assert!(runner.has_handlers("tool_call"), "tool_call is wired");
    assert!(
        runner.has_handlers("session_start"),
        "session_start is wired"
    );
}

/// Drive the wired `emit_tool_call` path: a bash `rm -rf /` blocks with
/// `expected_reason`, a benign bash command passes through unblocked.
pub fn assert_tool_call_guardrail(runner: &dyn ExtensionRunner, expected_reason: &str) {
    let dangerous = ToolCallEvent {
        tool_call_id: "call-1".to_string(),
        tool_name: "bash".to_string(),
        input: json!({ "command": "rm -rf /" }),
    };
    let decision = runner
        .emit_tool_call(&dangerous)
        .expect("a block decision for rm -rf");
    assert_eq!(decision.block, Some(true));
    assert_eq!(decision.reason.as_deref(), Some(expected_reason));

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

/// Invoke the `task` command handler with `args` through the runner's command
/// context (the shared resolve + create-context + call boilerplate). Each test
/// then observes the side effect its own fixture produced.
pub fn run_task_command(runner: &dyn ExtensionRunner, args: &str) {
    let command = runner.get_command("task").expect("task command resolves");
    let ctx: Box<dyn CommandContext> = runner.create_command_context();
    (command.command.handler)(args, ctx.as_ref()).expect("task handler runs");
}

/// Fire the wired `session_start` hook via the generic emit path (`Startup`
/// reason, no previous-session file).
pub fn emit_session_start(runner: &dyn ExtensionRunner) {
    runner.emit(&ExtensionDispatchEvent::SessionStart(SessionStartEvent {
        reason: SessionStartReason::Startup,
        previous_session_file: None,
    }));
}
