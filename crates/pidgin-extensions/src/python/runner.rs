//! The concrete [`ExtensionRunner`] seam implementation for the Python engine —
//! [`PythonExtensionRunner`] — plus the [`create_python_extension_runner`]
//! factory.
//!
//! pidgin-coding defines the `ExtensionRunner` trait (PR0) and ships a no-op
//! `StubExtensionRunner`; this module supplies the real Python-backed impl, the
//! offline sibling of `DenoExtensionRunner`. It answers the sync queries from the
//! loaded [`Inventory`] and runs three WIRED emitters against real Python handlers
//! under [`Python::with_gil`](pyo3::Python::with_gil):
//!
//!   1. `emit_tool_call` — runs the registered `tool_call` hook, interpreting a
//!      returned `{block, reason}` dict as a [`ToolCallEventResult`];
//!   2. the **command handler** path — [`get_command`](PythonExtensionRunner) /
//!      [`get_registered_commands`](PythonExtensionRunner) build a
//!      [`RegisteredCommand`] whose `handler` invokes the Python `handler(args,
//!      ctx)`;
//!   3. `session_start` — dispatched through the generic
//!      [`emit`](ExtensionRunnerTrait::emit), running the registered
//!      `session_start` hook.
//!
//! Every other emitter/method is a sanctioned no-op/None default copied from
//! `StubExtensionRunner`. `has_handlers` returns true ONLY for an event that both
//! has a registered handler AND is one of the wired emitters, so the turn loop's
//! emitter gating never calls into a stubbed no-op nor skips a wired handler.
//!
//! # GIL, not an off-thread plane
//!
//! Unlike deno's `!Send` `JsRuntime` (which needs a dedicated owner thread and a
//! `block_on`-off-ambient bridge), embedded CPython's GIL makes cross-thread calls
//! to synchronous handlers safe: every Python call simply takes the GIL via
//! `Python::with_gil`. No dedicated thread, no async bridge.
//!
//! # Host binding (#167 / #186)
//!
//! `bind_core` stores the merged `Arc<dyn>` host handles from
//! `core::extensions::runner` (the #167 traits). The integration test never
//! invokes a host callback, so this is store-and-hold; the concrete
//! `SessionHostBridge` (lands with #186, the AgentSession lane's) is reused
//! unchanged — this engine builds no second host impl.

// straitjacket-allow-file:duplication -- the command-collision resolution mirrors
// the deno `runner_impl::queries` (pi's `resolveRegisteredCommands`), and the
// tool/command/flag/source-info lowering helpers mirror its `getAllRegisteredTools`
// / `getFlagValues` folds and the `StubExtensionRunner` no-op defaults; the
// parallel structure is faithful to the shared `ExtensionRunner` seam both engines
// implement, not incidental repetition.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use pyo3::prelude::*;
use serde_json::json;

use pidgin_agent::types::AgentToolResult;
use pidgin_coding::core::extensions::command::{
    CommandContext, RegisteredCommand, ResolvedCommand,
};
use pidgin_coding::core::extensions::dispatch::{BeforeAgentStartCombinedResult, ExtensionError};
use pidgin_coding::core::extensions::events::common::{
    AgentMessage, BuildSystemPromptOptions, ImageContent,
};
use pidgin_coding::core::extensions::events::selection::{
    InputEventResult, InputSource, StreamingBehavior,
};
use pidgin_coding::core::extensions::events::session::{
    ResourcesDiscoverReason, ResourcesDiscoverResult, SessionShutdownEvent,
};
use pidgin_coding::core::extensions::events::tool::{
    ToolCallEvent, ToolCallEventResult, ToolResultEvent, ToolResultEventResult,
};
use pidgin_coding::core::extensions::events::turn::MessageEndEvent;
use pidgin_coding::core::extensions::loader::{Extension, ExtensionRuntime};
use pidgin_coding::core::extensions::runner::{
    ExtensionCommandContextHost, ExtensionDispatchEvent, ExtensionEmitOutcome,
    ExtensionErrorListener, ExtensionMode, ExtensionRunner as ExtensionRunnerTrait,
    ExtensionUIContext, FlagValue, ProviderRegistrationHost, RegisteredTool, SessionContextHost,
    SessionControlHost, UnsubscribeFn,
};
use pidgin_coding::core::extensions::types::{ExtensionContext, ToolDefinition};
use pidgin_coding::core::source_info::{SourceInfo, SourceOrigin, SourceScope};

use super::convert::{json_to_py, py_to_json};
use super::engine::{load_python_extension, LoadedPyExtension};
use super::loader::PythonExtensionRuntime;

/// The snake_case hook events this runner actually dispatches (the WIRED
/// emitters). `has_handlers` returns true only for these — every other event is a
/// sanctioned no-op, so returning true for it would call into nothing.
const WIRED_EVENTS: &[&str] = &["tool_call", "session_start"];

/// The registered `onError` listeners (pi's `onError` surface).
#[derive(Default)]
struct ListenerRegistry {
    listeners: Mutex<Vec<(u64, ExtensionErrorListener)>>,
    next_id: AtomicU64,
}

impl ListenerRegistry {
    fn add(&self, listener: ExtensionErrorListener) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().unwrap().push((id, listener));
        id
    }

    fn remove(&self, id: u64) {
        self.listeners.lock().unwrap().retain(|(lid, _)| *lid != id);
    }

    fn dispatch(&self, error: &ExtensionError) {
        for (_, listener) in self.listeners.lock().unwrap().iter() {
            listener(error);
        }
    }
}

/// Mutable binding state set through `bind_core` / `set_ui_context` /
/// `bind_command_context` (stored-and-held; the test drives no host callback).
#[derive(Default)]
struct BindingState {
    control_host: Option<Arc<dyn SessionControlHost>>,
    context_host: Option<Arc<dyn SessionContextHost>>,
    provider_host: Option<Arc<dyn ProviderRegistrationHost>>,
    command_context_host: Option<Arc<dyn ExtensionCommandContextHost>>,
    ui_context: Option<ExtensionUIContext>,
    mode: ExtensionMode,
}

/// The Python-backed [`ExtensionRunner`](ExtensionRunnerTrait) implementation.
pub struct PythonExtensionRunner {
    /// The loaded extensions (inventories + live Python handlers), shared with the
    /// loader's [`PythonExtensionRuntime`] via `Arc`.
    loaded: Vec<Arc<LoadedPyExtension>>,
    /// The project cwd (pi's `ContextConfig.cwd`); held for parity.
    #[allow(dead_code)]
    cwd: String,
    /// The shared `onError` listener registry.
    listeners: Arc<ListenerRegistry>,
    /// The bindings set through `bind_core` / `set_ui_context` /
    /// `bind_command_context`.
    bindings: Mutex<BindingState>,
    /// The stale message set once by `invalidate` (pi's `staleMessage`).
    stale: Mutex<Option<String>>,
}

impl PythonExtensionRunner {
    /// Build a runner over an already-loaded extension set (reusing the loader's
    /// imported handlers). Used by [`create_python_extension_runner`] and tests.
    pub fn from_loaded(loaded: Vec<Arc<LoadedPyExtension>>, cwd: impl Into<String>) -> Self {
        PythonExtensionRunner {
            loaded,
            cwd: cwd.into(),
            listeners: Arc::new(ListenerRegistry::default()),
            bindings: Mutex::new(BindingState::default()),
            stale: Mutex::new(None),
        }
    }

    /// Every registered hook handler for `event`, across all loaded extensions in
    /// load-then-registration order.
    fn hook_handlers(&self, event: &str) -> Vec<Arc<Py<PyAny>>> {
        self.loaded
            .iter()
            .filter_map(|ext| ext.handlers.hooks.get(event))
            .flatten()
            .cloned()
            .collect()
    }
}

impl ExtensionRunnerTrait for PythonExtensionRunner {
    // ---- lifecycle -------------------------------------------------------
    fn emit_session_shutdown(&self, _event: SessionShutdownEvent) {}

    // ---- generic dispatch (WIRED: session_start) -------------------------
    fn emit(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        if let ExtensionDispatchEvent::SessionStart(start) = event {
            if let Ok(event_json) = serde_json::to_value(start) {
                for handler in self.hook_handlers("session_start") {
                    run_plain_hook(&handler, &event_json, &self.listeners);
                }
            }
        }
        ExtensionEmitOutcome::None
    }

    // ---- dedicated emitters ----------------------------------------------
    fn emit_message_end(&self, _event: &MessageEndEvent) -> Option<AgentMessage> {
        None
    }

    fn emit_input(
        &self,
        _text: &str,
        _images: Option<&[ImageContent]>,
        _source: InputSource,
        _streaming_behavior: Option<StreamingBehavior>,
    ) -> InputEventResult {
        InputEventResult::Continue
    }

    fn emit_before_agent_start(
        &self,
        _prompt: &str,
        _images: Option<&[ImageContent]>,
        _system_prompt: &str,
        _system_prompt_options: &BuildSystemPromptOptions,
    ) -> Option<BeforeAgentStartCombinedResult> {
        None
    }

    fn emit_resources_discover(
        &self,
        _cwd: &str,
        _reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult {
        ResourcesDiscoverResult::default()
    }

    // ---- WIRED: emit_tool_call -------------------------------------------
    fn emit_tool_call(&self, event: &ToolCallEvent) -> Option<ToolCallEventResult> {
        let event_json = serde_json::to_value(event).ok()?;
        for handler in self.hook_handlers("tool_call") {
            if let Some(result) = run_tool_call_handler(&handler, &event_json, &self.listeners) {
                // pi short-circuits on the first block decision.
                return Some(result);
            }
        }
        None
    }

    fn emit_tool_result(&self, _event: &ToolResultEvent) -> Option<ToolResultEventResult> {
        None
    }

    // ---- sync queries ----------------------------------------------------
    fn has_handlers(&self, event_type: &str) -> bool {
        // True ONLY for a wired event that also has a registered handler: a
        // registered-but-stubbed event returns false (the turn loop must not call
        // a no-op emitter), and a wired event with no handler returns false too.
        WIRED_EVENTS.contains(&event_type) && !self.hook_handlers(event_type).is_empty()
    }

    fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.get_registered_commands()
            .into_iter()
            .find(|command| command.invocation_name == name)
    }

    fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for ext in &self.loaded {
            for command in &ext.inventory.commands {
                *counts.entry(command.name.as_str()).or_insert(0) += 1;
            }
        }

        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut resolved = Vec::new();

        for ext in &self.loaded {
            for command in &ext.inventory.commands {
                let occurrence = {
                    let entry = seen.entry(command.name.as_str()).or_insert(0);
                    *entry += 1;
                    *entry
                };
                let mut invocation_name =
                    if counts.get(command.name.as_str()).copied().unwrap_or(0) > 1 {
                        format!("{}:{}", command.name, occurrence)
                    } else {
                        command.name.clone()
                    };
                if taken.contains(&invocation_name) {
                    let mut suffix = occurrence;
                    loop {
                        suffix += 1;
                        invocation_name = format!("{}:{}", command.name, suffix);
                        if !taken.contains(&invocation_name) {
                            break;
                        }
                    }
                }
                taken.insert(invocation_name.clone());

                let handler = ext.handlers.commands.get(&command.name).cloned();
                resolved.push(ResolvedCommand {
                    command: registered_command(
                        &ext.path,
                        &command.name,
                        command.description.clone(),
                        handler,
                        Arc::clone(&self.listeners),
                    ),
                    invocation_name,
                });
            }
        }
        resolved
    }

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut tools = Vec::new();
        for ext in &self.loaded {
            for record in &ext.inventory.tools {
                if seen.insert(record.name.clone()) {
                    let execute = ext.handlers.tools.get(&record.name).cloned();
                    tools.push(RegisteredTool {
                        tool: tool_definition(record, execute, Arc::clone(&self.listeners)),
                        source_info: synthetic_source_info(&ext.path),
                    });
                }
            }
        }
        tools
    }

    fn get_flag_values(&self) -> BTreeMap<String, FlagValue> {
        let mut values = BTreeMap::new();
        for ext in &self.loaded {
            for flag in &ext.inventory.flags {
                if let Some(value) = flag
                    .value
                    .clone()
                    .or_else(|| flag.default.clone())
                    .and_then(flag_value)
                {
                    values.insert(flag.name.clone(), value);
                }
            }
        }
        values
    }

    fn create_command_context(&self) -> Box<dyn CommandContext> {
        let bindings = self.bindings.lock().unwrap();
        let (args, flags) = match &bindings.command_context_host {
            Some(host) => (host.get_args(), host.get_flags()),
            None => (String::new(), BTreeMap::new()),
        };
        Box::new(PythonCommandContext { args, flags })
    }

    // ---- binding / mutation ----------------------------------------------
    fn bind_core(
        &self,
        actions: Arc<dyn SessionControlHost>,
        context_actions: Arc<dyn SessionContextHost>,
        provider_actions: Option<Arc<dyn ProviderRegistrationHost>>,
    ) {
        let mut bindings = self.bindings.lock().unwrap();
        bindings.control_host = Some(actions);
        bindings.context_host = Some(context_actions);
        bindings.provider_host = provider_actions;
    }

    fn set_ui_context(&self, ui_context: Option<ExtensionUIContext>, mode: ExtensionMode) {
        let mut bindings = self.bindings.lock().unwrap();
        bindings.ui_context = ui_context;
        bindings.mode = mode;
    }

    fn bind_command_context(&self, actions: Option<Arc<dyn ExtensionCommandContextHost>>) {
        self.bindings.lock().unwrap().command_context_host = actions;
    }

    fn on_error(&self, listener: ExtensionErrorListener) -> UnsubscribeFn {
        let id = self.listeners.add(listener);
        let listeners = Arc::clone(&self.listeners);
        Box::new(move || listeners.remove(id))
    }

    fn emit_error(&self, error: ExtensionError) {
        self.listeners.dispatch(&error);
    }

    fn invalidate(&self, message: &str) {
        let mut stale = self.stale.lock().unwrap();
        if stale.is_none() {
            *stale = Some(message.to_string());
        }
    }
}

/// The host factory, mirroring `create_deno_extension_runner`: recover the loader's
/// [`PythonExtensionRuntime`] via [`ExtensionRuntime::as_any`] and REUSE its
/// already-imported handlers + inventories (no `extension(pi)` re-run); on a failed
/// downcast (e.g. a `StubExtensionRuntime` in a non-Python context) fall back to
/// reloading each extension from its resolved path.
///
/// Unlike the deno factory this takes no `SessionManager` / `ModelRegistry`: the
/// offline Python engine wires none of the provider-registration host path yet, so
/// it holds no such handles (a deliberate, documented divergence from the deno
/// factory's signature; the `ExtensionRunner` seam itself is identical).
pub fn create_python_extension_runner(
    extensions: Vec<Extension>,
    runtime: Box<dyn ExtensionRuntime>,
    cwd: impl Into<String>,
) -> Box<dyn ExtensionRunnerTrait> {
    let cwd = cwd.into();

    // Preferred: the loader handed us its real runtime. Reuse the extensions it
    // already imported (their handlers stay live in the interpreter).
    if let Some(real) = runtime.as_any().downcast_ref::<PythonExtensionRuntime>() {
        return Box::new(PythonExtensionRunner::from_loaded(
            real.loaded().to_vec(),
            cwd,
        ));
    }

    // Fallback: reload each extension from its resolved path.
    let mut loaded = Vec::new();
    for extension in &extensions {
        let path = if extension.resolved_path.is_empty() {
            &extension.path
        } else {
            &extension.resolved_path
        };
        if let Ok(loaded_ext) = load_python_extension(path) {
            loaded.push(Arc::new(loaded_ext));
        }
    }
    Box::new(PythonExtensionRunner::from_loaded(loaded, cwd))
}

// ---------------------------------------------------------------------------
// Python handler dispatch (all under the GIL)
// ---------------------------------------------------------------------------

/// Run a `tool_call` handler with the event, interpreting its return: `None` ->
/// `None`; a `{block, reason}` dict -> [`ToolCallEventResult`]. A raised exception
/// is isolated into the error listeners and treated as `None` (never unwinds).
fn run_tool_call_handler(
    handler: &Py<PyAny>,
    event_json: &serde_json::Value,
    listeners: &ListenerRegistry,
) -> Option<ToolCallEventResult> {
    Python::with_gil(|py| {
        let event = json_to_py(py, event_json).ok()?;
        let ctx = py.None();
        match handler.bind(py).call1((event, ctx)) {
            Ok(ret) if ret.is_none() => None,
            Ok(ret) => {
                let value = py_to_json(&ret).ok()?;
                serde_json::from_value::<ToolCallEventResult>(value).ok()
            }
            Err(error) => {
                report_handler_error(py, listeners, "tool_call", error);
                None
            }
        }
    })
}

/// Run a plain (result-less) hook handler with the event; a raised exception is
/// isolated into the error listeners.
fn run_plain_hook(
    handler: &Py<PyAny>,
    event_json: &serde_json::Value,
    listeners: &ListenerRegistry,
) {
    Python::with_gil(|py| {
        let Ok(event) = json_to_py(py, event_json) else {
            return;
        };
        let ctx = py.None();
        if let Err(error) = handler.bind(py).call1((event, ctx)) {
            report_handler_error(py, listeners, "session_start", error);
        }
    });
}

/// Deliver a Python handler exception to the registered `onError` listeners.
fn report_handler_error(py: Python<'_>, listeners: &ListenerRegistry, event: &str, error: PyErr) {
    let message = error
        .value(py)
        .str()
        .ok()
        .map(|s| s.to_string())
        .unwrap_or_else(|| error.to_string());
    listeners.dispatch(&ExtensionError {
        extension_path: String::new(),
        event: event.to_string(),
        error: message,
        stack: None,
    });
}

/// Build a [`RegisteredCommand`] whose handler runs the Python `handler(args,
/// ctx)` under the GIL (the WIRED command path); a throw surfaces as an `Err`.
fn registered_command(
    extension_path: &str,
    name: &str,
    description: Option<String>,
    handler: Option<Arc<Py<PyAny>>>,
    _listeners: Arc<ListenerRegistry>,
) -> RegisteredCommand {
    RegisteredCommand {
        name: name.to_string(),
        source_info: synthetic_source_info(extension_path),
        description,
        get_argument_completions: None,
        handler: Arc::new(move |args, _ctx| match &handler {
            Some(handler) => run_command_handler(handler, args),
            None => Ok(()),
        }),
    }
}

/// Run a Python command handler `handler(args, ctx)` under the GIL.
fn run_command_handler(handler: &Py<PyAny>, args: &str) -> anyhow::Result<()> {
    Python::with_gil(|py| {
        let ctx = py.None();
        handler
            .bind(py)
            .call1((args, ctx))
            .map(|_| ())
            .map_err(|error| {
                let message = error
                    .value(py)
                    .str()
                    .ok()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| error.to_string());
                anyhow!("python command handler failed: {message}")
            })
    })
}

/// Lower an inventory `ToolRecord` into a [`ToolDefinition`] whose `execute` runs
/// the Python `execute` callable under the GIL (best-effort; not one of the three
/// wired emitters, but wired for parity with deno). A missing callable or an
/// unparseable result surfaces as a tool-error result rather than unwinding.
fn tool_definition(
    record: &crate::inventory::ToolRecord,
    execute: Option<Arc<Py<PyAny>>>,
    _listeners: Arc<ListenerRegistry>,
) -> ToolDefinition {
    let tool_name = record.name.clone();
    ToolDefinition {
        name: record.name.clone(),
        label: record.label.clone(),
        description: record.description.clone(),
        parameters: record.parameters.clone(),
        execution_mode: None,
        execute: Arc::new(move |_id, args, _signal, _on_update, _ctx| match &execute {
            Some(execute) => run_tool_execute(execute, &tool_name, args),
            None => tool_error_result(format!("tool '{tool_name}' has no execute closure")),
        }),
        prepare_arguments: None,
        prompt_snippet: record.prompt_snippet.clone(),
        prompt_guidelines: record.prompt_guidelines.clone(),
        render_shell: None,
    }
}

/// Run a Python tool `execute(args)` under the GIL and shape its return into an
/// [`AgentToolResult`].
fn run_tool_execute(
    execute: &Py<PyAny>,
    tool_name: &str,
    args: &serde_json::Value,
) -> AgentToolResult {
    Python::with_gil(|py| {
        let py_args = match json_to_py(py, args) {
            Ok(value) => value,
            Err(error) => return tool_error_result(format!("bad args for '{tool_name}': {error}")),
        };
        match execute.bind(py).call1((py_args,)) {
            Ok(ret) => match py_to_json(&ret) {
                Ok(value) => {
                    serde_json::from_value::<AgentToolResult>(value).unwrap_or_else(|err| {
                        tool_error_result(format!(
                            "tool '{tool_name}' returned an unparseable result: {err}"
                        ))
                    })
                }
                Err(err) => {
                    tool_error_result(format!("tool '{tool_name}' result unreadable: {err}"))
                }
            },
            Err(error) => tool_error_result(format!("tool '{tool_name}' execute threw: {error}")),
        }
    })
}

/// The error-details [`AgentToolResult`] used when a tool `execute` fails or
/// returns an unparseable shape (mirrors the deno engine).
fn tool_error_result(message: String) -> AgentToolResult {
    AgentToolResult {
        content: Vec::new(),
        details: json!({ "error": message }),
        added_tool_names: None,
        terminate: None,
    }
}

/// Build a synthetic [`SourceInfo`] attributing a resource to the extension at
/// `path` (the orchestrator re-stamps provenance later).
fn synthetic_source_info(path: &str) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: path.to_string(),
        scope: SourceScope::Project,
        origin: SourceOrigin::TopLevel,
        base_dir: None,
    }
}

/// Map a flag's JSON value to the seam's `boolean | string` [`FlagValue`].
fn flag_value(value: serde_json::Value) -> Option<FlagValue> {
    match value {
        serde_json::Value::Bool(boolean) => Some(FlagValue::Bool(boolean)),
        serde_json::Value::String(string) => Some(FlagValue::Str(string)),
        _ => None,
    }
}

/// The concrete [`CommandContext`] the runner mints — the args/flags snapshot from
/// the bound command-context host (empty when none is bound).
struct PythonCommandContext {
    #[allow(dead_code)]
    args: String,
    #[allow(dead_code)]
    flags: BTreeMap<String, FlagValue>,
}

impl ExtensionContext for PythonCommandContext {}
impl CommandContext for PythonCommandContext {}
