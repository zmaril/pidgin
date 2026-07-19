//! Integration tests for the concrete [`DenoExtensionRunner`] — the deno-backed
//! `ExtensionRunner` seam impl.
//!
//! These load small inline pi-style extensions onto the real embedded
//! `deno_core` runtime, build a `DenoExtensionRunner`, and drive its **sync**
//! trait surface (the `block_on`-off-ambient bridge). They assert the real-logic
//! methods:
//!
//!   * the enum-dispatch generic `emit` (only `session_before_compact` /
//!     `session_before_tree` fold to an outcome; every other event → `None`);
//!   * the covered-3 pass-through (`emit_input` still transforms through the
//!     trait);
//!   * the Inventory-backed sync queries (`get_all_registered_tools` /
//!     `get_flag_values` / `get_registered_commands` / `get_command`);
//!   * `emit_resources_discover` stamps each path with its `extension_path`;
//!   * `has_handlers` via the `&str`→`HookEvent` adapter;
//!   * `bind_core` with minimal mocks of the four host traits +
//!     `create_command_context`.
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox.
#![cfg(feature = "deno")]

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use pidgin_coding::core::extensions::command::ResolvedCommand;
use pidgin_coding::core::extensions::events::agent::AgentStartEvent;
use pidgin_coding::core::extensions::events::common::BuildSystemPromptOptions;
use pidgin_coding::core::extensions::events::session::{
    SessionBeforeCompactEvent, SessionBeforeTreeEvent, TreePreparation,
};
use pidgin_coding::core::extensions::events::{
    CompactionReason, InputEventResult, InputSource, ResourcesDiscoverReason,
};
use pidgin_coding::core::extensions::hook::HookEvent;
use pidgin_coding::core::extensions::runner::{
    ExtensionCommandContextHost, ExtensionDispatchEvent, ExtensionEmitOutcome, ExtensionMode,
    ExtensionRunner as RunnerTrait, FlagValue, ProviderRegistrationHost, SessionContextHost,
    SessionControlHost,
};

use pidgin_extensions::{hook_event_from_str, DenoExtensionRunner, JsPlaneHandle, SourceLanguage};

/// Spawn a plane, load each `(path, source)` fixture in order, and build a
/// `DenoExtensionRunner` over them (the handlers stay live in the plane).
async fn deno_runner(sources: &[(&str, &str)]) -> DenoExtensionRunner {
    let plane = JsPlaneHandle::spawn();
    let mut loaded = Vec::new();
    for (i, (path, source)) in sources.iter().enumerate() {
        let inventory = plane
            .load_extension_source(format!("e{i}"), *source, SourceLanguage::TypeScript)
            .await
            .expect("extension loads");
        loaded.push((path.to_string(), inventory));
    }
    DenoExtensionRunner::from_loaded(plane, loaded, "/project")
}

// -------------------------------------------------------------------------
// Generic emit dispatch
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emit_folds_session_before_compact_to_outcome() {
    let runner = deno_runner(&[(
        "cancel.ts",
        r#"export default (pi) => { pi.on("session_before_compact", async () => ({ cancel: true })); };"#,
    )])
    .await;

    let event = ExtensionDispatchEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
        preparation: json!({}),
        branch_entries: vec![],
        custom_instructions: None,
        reason: CompactionReason::Manual,
        will_retry: false,
    });

    match runner.emit(&event) {
        ExtensionEmitOutcome::BeforeCompact(result) => assert_eq!(result.cancel, Some(true)),
        _ => panic!("expected a BeforeCompact outcome"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emit_folds_session_before_tree_to_outcome() {
    let runner = deno_runner(&[(
        "tree.ts",
        r#"export default (pi) => { pi.on("session_before_tree", async () => ({ cancel: false, label: "override" })); };"#,
    )])
    .await;

    let event = ExtensionDispatchEvent::SessionBeforeTree(SessionBeforeTreeEvent {
        preparation: TreePreparation {
            target_id: "n1".into(),
            old_leaf_id: None,
            common_ancestor_id: None,
            entries_to_summarize: vec![],
            user_wants_summary: false,
            custom_instructions: None,
            replace_instructions: None,
            label: None,
        },
    });

    match runner.emit(&event) {
        ExtensionEmitOutcome::BeforeTree(result) => {
            assert_eq!(result.label.as_deref(), Some("override"));
            assert_eq!(result.cancel, Some(false));
        }
        _ => panic!("expected a BeforeTree outcome"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emit_non_before_event_returns_none() {
    // A handler that returns a value on a non-`session_before_*` event must not
    // produce an outcome — only the two before-events fold.
    let runner = deno_runner(&[(
        "agent.ts",
        r#"export default (pi) => { pi.on("agent_start", async () => ({ cancel: true })); };"#,
    )])
    .await;

    assert!(matches!(
        runner.emit(&ExtensionDispatchEvent::AgentStart(AgentStartEvent {})),
        ExtensionEmitOutcome::None
    ));
}

// -------------------------------------------------------------------------
// Covered-3 pass-through
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emit_input_transforms_through_the_trait() {
    let runner = deno_runner(&[(
        "input.ts",
        r#"export default (pi) => { pi.on("input", async (e) => ({ action: "transform", text: e.text + "!" })); };"#,
    )])
    .await;

    match runner.emit_input("hi", None, InputSource::Interactive, None) {
        InputEventResult::Transform { text, .. } => assert_eq!(text, "hi!"),
        other => panic!("expected a transform, got {other:?}"),
    }
}

// -------------------------------------------------------------------------
// Inventory-backed sync queries
// -------------------------------------------------------------------------

const REGISTRY_FIXTURE: &str = r#"
export default (pi) => {
    pi.registerTool({
        name: "greet",
        label: "Greet",
        description: "Greets a user",
        parameters: { type: "object" },
        execute: async () => ({ content: [] }),
    });
    pi.registerFlag("verbose", { type: "boolean", default: true });
    pi.registerFlag("mode", { type: "string", default: "fast" });
    pi.registerCommand("hello", { description: "say hi", handler: async () => {} });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_read_the_loaded_inventory() {
    let runner = deno_runner(&[("reg.ts", REGISTRY_FIXTURE)]).await;

    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool.name, "greet");
    assert_eq!(tools[0].tool.label, "Greet");
    assert_eq!(tools[0].source_info.path, "reg.ts");

    let flags = runner.get_flag_values();
    assert_eq!(flags.get("verbose"), Some(&FlagValue::Bool(true)));
    assert_eq!(flags.get("mode"), Some(&FlagValue::Str("fast".to_string())));

    let commands = runner.get_registered_commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].invocation_name, "hello");
    assert!(runner.get_command("hello").is_some());
    assert!(runner.get_command("missing").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_registered_commands_disambiguates_collisions() {
    let runner = deno_runner(&[
        (
            "a.ts",
            r#"export default (pi) => { pi.registerCommand("dup", { handler: async () => {} }); };"#,
        ),
        (
            "b.ts",
            r#"export default (pi) => { pi.registerCommand("dup", { handler: async () => {} }); };"#,
        ),
    ])
    .await;

    let names: Vec<String> = runner
        .get_registered_commands()
        .into_iter()
        .map(|command| command.invocation_name)
        .collect();
    assert_eq!(names, vec!["dup:1".to_string(), "dup:2".to_string()]);
}

// -------------------------------------------------------------------------
// emit_resources_discover extension_path widening
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resources_discover_stamps_extension_path() {
    let runner = deno_runner(&[(
        "res.ts",
        r#"export default (pi) => { pi.on("resources_discover", async () => ({ skillPaths: ["/s/one"], themePaths: ["/t/one"] })); };"#,
    )])
    .await;

    let result = runner.emit_resources_discover("/project", ResourcesDiscoverReason::Startup);
    assert_eq!(result.skill_paths.len(), 1);
    assert_eq!(result.skill_paths[0].path, "/s/one");
    assert_eq!(result.skill_paths[0].extension_path, "res.ts");
    assert_eq!(result.theme_paths.len(), 1);
    assert_eq!(result.theme_paths[0].path, "/t/one");
    assert_eq!(result.theme_paths[0].extension_path, "res.ts");
    assert!(result.prompt_paths.is_empty());
}

// -------------------------------------------------------------------------
// has_handlers via the &str -> HookEvent adapter
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn has_handlers_uses_the_str_adapter() {
    let runner = deno_runner(&[(
        "hook.ts",
        r#"export default (pi) => { pi.on("input", async () => {}); };"#,
    )])
    .await;

    assert!(runner.has_handlers("input"));
    assert!(!runner.has_handlers("tool_call"));

    // The adapter itself: recognized names resolve, unknown names do not.
    assert_eq!(hook_event_from_str("input"), Some(HookEvent::Input));
    assert_eq!(
        hook_event_from_str("session_before_tree"),
        Some(HookEvent::SessionBeforeTree)
    );
    assert_eq!(hook_event_from_str("not_a_real_event"), None);
}

// -------------------------------------------------------------------------
// bind_core with the four host mocks + create_command_context
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_core_binds_hosts_and_mints_a_command_context() {
    let runner = deno_runner(&[]).await;

    runner.bind_core(
        Arc::new(MockControlHost),
        Arc::new(MockContextHost),
        Some(Arc::new(MockProviderHost)),
    );
    runner.set_ui_context(None, ExtensionMode::Print);
    runner.bind_command_context(Some(Arc::new(MockCommandContextHost)));

    // A valid CommandContext trait object is produced (reads args/flags from the
    // bound host without panicking).
    let _ctx = runner.create_command_context();

    // on_error / emit_error round-trip through the shared listener registry.
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let unsubscribe = runner.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.event.clone());
    }));
    runner.emit_error(pidgin_coding::core::extensions::dispatch::ExtensionError {
        extension_path: "x.ts".into(),
        event: "input".into(),
        error: "boom".into(),
        stack: None,
    });
    assert_eq!(seen.lock().unwrap().clone(), vec!["input".to_string()]);
    unsubscribe();
    runner.invalidate("stale");
}

// -------------------------------------------------------------------------
// Minimal host-trait mocks
// -------------------------------------------------------------------------

struct MockControlHost;
impl SessionControlHost for MockControlHost {
    fn send_message(&self, _content: &Value, _options: Option<&Value>) {}
    fn send_user_message(&self, _content: &Value, _options: Option<&Value>) {}
    fn append_entry(&self, _custom_type: &str, _data: &Value) -> String {
        String::new()
    }
    fn set_session_name(&self, _name: &str) {}
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn set_label(&self, _entry_id: &str, _label: &str) {}
    fn get_active_tools(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_all_tools(&self) -> Vec<Value> {
        Vec::new()
    }
    fn set_active_tools(&self, _names: &[String]) {}
    fn refresh_tools(&self) {}
    fn get_commands(&self) -> Vec<ResolvedCommand> {
        Vec::new()
    }
    fn set_model(&self, _model: &Value) {}
    fn get_thinking_level(&self) -> pidgin_agent::types::ThinkingLevel {
        pidgin_agent::types::ThinkingLevel::Off
    }
    fn set_thinking_level(&self, _level: pidgin_agent::types::ThinkingLevel) {}
}

struct MockContextHost;
impl SessionContextHost for MockContextHost {
    fn get_model(&self) -> Value {
        Value::Null
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
    fn get_signal(&self) -> pidgin_ai::seams::AbortSignal {
        pidgin_ai::seams::AbortSignal::new()
    }
    fn abort(&self) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<Value> {
        None
    }
    fn compact(&self) {}
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        Value::Null
    }
}

struct MockProviderHost;
impl ProviderRegistrationHost for MockProviderHost {
    fn register_provider(&self, _provider: &Value) {}
    fn register_native_provider(&self, _provider: &Value) {}
    fn unregister_provider(&self, _id: &str) {}
}

struct MockCommandContextHost;
impl ExtensionCommandContextHost for MockCommandContextHost {
    fn get_args(&self) -> String {
        "the args".to_string()
    }
    fn get_flags(&self) -> BTreeMap<String, FlagValue> {
        let mut flags = BTreeMap::new();
        flags.insert("verbose".to_string(), FlagValue::Bool(true));
        flags
    }
}
