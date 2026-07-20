//! The combined (deno + python) extension engine — one [`ExtensionLoader`] /
//! [`ExtensionRunner`] pair that fans a single extension set out across BOTH
//! embedded engines, so the CLI can run either or both at once.
//!
//! # What it is
//!
//! [`CombinedExtensionLoader`] wraps an optional deno inner ([`RealExtensionLoader`])
//! and an optional python inner ([`PythonExtensionLoader`]), each built only when
//! its Cargo feature is compiled AND the caller's [`EngineSelection`] enables it.
//! On `load_extensions_cached` it:
//!
//!   1. runs ONE dedupe pass over the merged input paths, keyed by the SAME
//!      logical `resolved_path` the two engines use (see [`logical_resolved_path`]);
//!   2. classifies each surviving path by suffix (`.ts`/`.js` → deno, `.py` →
//!      python, else unknown) and buckets it per engine;
//!   3. calls each inner loader ONCE with its bucket (batched), then merges both
//!      inners' `extensions` + `errors` — returning the FULL UNION of both
//!      engines' extensions WITHOUT any conflict-diagnostic pass. Cross-engine
//!      TOOL/flag-name conflicts are detected once by the orchestrator's
//!      `add_extension_conflict_diagnostics` over this union (single source of
//!      truth); duplicating that check here would double-report. COMMAND-name
//!      collisions are NOT an error — the combined runner resolves them deno-first
//!      and each shadow is logged (a runner concern, not a load diagnostic);
//!   4. recombines the two inner runtimes into a [`CombinedExtensionRuntime`] so
//!      the trust two-pass can thread them back through identity-preserving.
//!
//! A path whose engine is not compiled/enabled degrades gracefully to an
//! [`ExtensionLoadError`] ("… not compiled in (rebuild with `--features …`)") —
//! it NEVER panics.
//!
//! [`CombinedExtensionRunner`] fans every [`ExtensionRunner`] method out across the
//! inner runners in DENO-FIRST order: query methods concat, `get_command` /
//! short-circuit emitters (`emit_tool_call`, …) take the first inner that answers,
//! `has_handlers` ORs, and the binding methods forward to every inner.
//!
//! # Additive, engine-`spawn`-preserving
//!
//! The deno and python engines' own `spawn` / factory entry points are untouched;
//! this module composes them. It reuses each engine's by-**reference** runner
//! factory (`create_*_extension_runner_from_runtime_ref`) so it can build each
//! inner runner from a borrowed inner runtime it cannot move out of the shared
//! combined runtime.

// straitjacket-allow-file:duplication -- the logical `resolved_path` recipe (the
// `resolve_path(..., normalize_unicode_spaces: true)` call) and the per-engine
// dedupe/merge loop are transcribed from the deno `resource_loader_impl` and its
// python sibling on purpose: the combined loader implements the SAME orchestrator
// seam the SAME way (dedupe by resolved_path, thread the opaque runtime handle,
// map inventory names into an Extension), and the fan-out runner's no-op/merge
// arms mirror the shared `ExtensionRunner` seam both engines implement. The
// parallel structure is faithful to the shared seam, not incidental repetition.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::command::{CommandContext, ResolvedCommand};
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
use pidgin_coding::core::extensions::loader::{
    Extension, ExtensionLoadError, ExtensionLoader, ExtensionRuntime, LoadExtensionsResult,
};
use pidgin_coding::core::extensions::runner::{
    ExtensionCommandContextHost, ExtensionDispatchEvent, ExtensionEmitOutcome,
    ExtensionErrorListener, ExtensionMode, ExtensionRunner as ExtensionRunnerTrait,
    ExtensionUIContext, FlagValue, ProviderRegistrationHost, RegisteredTool, SessionContextHost,
    SessionControlHost, UnsubscribeFn,
};
use pidgin_coding::core::extensions::types::ExtensionContext;
use pidgin_coding::utils::paths::{resolve_path, PathInputOptions};

#[cfg(feature = "deno")]
use crate::resource_loader_impl::RealExtensionLoader;
#[cfg(feature = "deno")]
use crate::runner_impl::create_deno_extension_runner_from_runtime_ref;
#[cfg(feature = "deno")]
use pidgin_coding::core::model_registry::ModelRegistry;
#[cfg(feature = "deno")]
use pidgin_coding::core::session_manager::SessionManager;

#[cfg(feature = "python")]
use crate::python::{create_python_extension_runner_from_runtime_ref, PythonExtensionLoader};

// ---------------------------------------------------------------------------
// EngineSelection
// ---------------------------------------------------------------------------

/// Which extension engines the caller wants enabled. The CLI derives this from
/// the compiled feature set (`deno: cfg!(feature = "deno")`, likewise `python`).
///
/// Selecting an engine that is not compiled in is harmless: the loader simply
/// has no inner for it, so every path of that engine's language degrades to a
/// graceful "not compiled in" error.
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineSelection {
    /// Enable the deno (JavaScript / TypeScript) engine.
    pub deno: bool,
    /// Enable the python engine.
    pub python: bool,
}

// ---------------------------------------------------------------------------
// CombinedExtensionLoader
// ---------------------------------------------------------------------------

/// The combined `impl ExtensionLoader`: an optional deno inner and an optional
/// python inner, each present only when its feature is compiled AND the
/// [`EngineSelection`] enabled it. Routes each extension path to the matching
/// inner by suffix and merges the results.
pub struct CombinedExtensionLoader {
    #[cfg(feature = "deno")]
    deno: Option<RealExtensionLoader>,
    #[cfg(feature = "python")]
    python: Option<PythonExtensionLoader>,
}

impl CombinedExtensionLoader {
    /// Build the combined loader for `sel`, spawning each engine's inner only
    /// when that engine is both compiled in and selected. Returns a boxed trait
    /// object (like each engine's own `spawn`), so no extra `Box::new` is needed
    /// at the call site.
    pub fn spawn(sel: EngineSelection) -> Box<dyn ExtensionLoader> {
        Box::new(CombinedExtensionLoader {
            #[cfg(feature = "deno")]
            deno: sel.deno.then(RealExtensionLoader::spawn),
            #[cfg(feature = "python")]
            python: sel.python.then(PythonExtensionLoader::new),
        })
    }
}

/// The engine a given extension path routes to, by suffix.
enum PathEngine {
    /// `.ts` / `.js` → the deno engine.
    Deno,
    /// `.py` → the python engine.
    Python,
    /// Any other suffix → no engine.
    Unknown,
}

/// Classify an extension path by its file suffix.
fn classify(path: &str) -> PathEngine {
    let suffix = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str());
    match suffix {
        Some("ts") | Some("js") => PathEngine::Deno,
        Some("py") => PathEngine::Python,
        _ => PathEngine::Unknown,
    }
}

/// The graceful degradation error for a path whose engine is not compiled in /
/// enabled: never a panic, always an actionable rebuild hint.
fn not_compiled_error(path: &str, lang: &str, feature: &str) -> ExtensionLoadError {
    ExtensionLoadError {
        path: path.to_string(),
        error: format!(
            "extension support for {lang} not compiled in (rebuild with `--features {feature}`)"
        ),
    }
}

/// The error for a path whose suffix matches no engine.
fn unknown_engine_error(path: &str) -> ExtensionLoadError {
    ExtensionLoadError {
        path: path.to_string(),
        error: format!(
            "unrecognized extension type for `{path}` \
             (expected a `.ts`/`.js` deno extension or a `.py` python extension)"
        ),
    }
}

impl ExtensionLoader for CombinedExtensionLoader {
    fn load_extensions_cached(
        &self,
        paths: &[String],
        cwd: &str,
        event_bus: &EventBus,
        runtime: Option<Box<dyn ExtensionRuntime>>,
    ) -> LoadExtensionsResult {
        let mut extensions: Vec<Extension> = Vec::new();
        let mut errors: Vec<ExtensionLoadError> = Vec::new();

        // Pass 1: ONE dedupe over the merged paths by logical resolved_path
        // (identical recipe to each engine's own loader), then classify each
        // surviving path by suffix and bucket it per engine.
        let mut seen: Vec<String> = Vec::new();
        let mut deno_paths: Vec<String> = Vec::new();
        let mut python_paths: Vec<String> = Vec::new();
        for path in paths {
            let resolved_path = logical_resolved_path(path, cwd);
            if seen.contains(&resolved_path) {
                continue;
            }
            seen.push(resolved_path);
            match classify(path) {
                PathEngine::Deno => deno_paths.push(path.clone()),
                PathEngine::Python => python_paths.push(path.clone()),
                PathEngine::Unknown => errors.push(unknown_engine_error(path)),
            }
        }

        // Recover each inner runtime from a supplied combined runtime (pass 2),
        // so the trust two-pass threads identity-preserving; a foreign/absent
        // runtime yields None and each inner mints a fresh one.
        let split = split_runtime(runtime);

        // Command names per engine, for cross-engine shadow logging.
        #[allow(unused_mut)]
        let mut deno_commands: Vec<String> = Vec::new();
        #[allow(unused_mut)]
        let mut python_commands: Vec<String> = Vec::new();

        // ---- deno engine (batched: one inner call) ----
        #[cfg(feature = "deno")]
        let deno_runtime: Option<Box<dyn ExtensionRuntime>> = {
            if let Some(deno) = &self.deno {
                let mut result =
                    deno.load_extensions_cached(&deno_paths, cwd, event_bus, split.deno);
                for ext in &result.extensions {
                    deno_commands.extend(ext.commands.iter().cloned());
                }
                extensions.append(&mut result.extensions);
                errors.append(&mut result.errors);
                result.runtime
            } else {
                for path in &deno_paths {
                    errors.push(not_compiled_error(path, "deno", "deno"));
                }
                None
            }
        };
        #[cfg(not(feature = "deno"))]
        for path in &deno_paths {
            errors.push(not_compiled_error(path, "deno", "deno"));
        }

        // ---- python engine (batched: one inner call) ----
        #[cfg(feature = "python")]
        let python_runtime: Option<Box<dyn ExtensionRuntime>> = {
            if let Some(python) = &self.python {
                let mut result =
                    python.load_extensions_cached(&python_paths, cwd, event_bus, split.python);
                for ext in &result.extensions {
                    python_commands.extend(ext.commands.iter().cloned());
                }
                extensions.append(&mut result.extensions);
                errors.append(&mut result.errors);
                result.runtime
            } else {
                for path in &python_paths {
                    errors.push(not_compiled_error(path, "python", "python"));
                }
                None
            }
        };
        #[cfg(not(feature = "python"))]
        for path in &python_paths {
            errors.push(not_compiled_error(path, "python", "python"));
        }

        // Cross-engine COMMAND-name collisions are NOT an error: the combined
        // runner resolves them deno-first (deterministic). Log each shadow so a
        // `.py` command silently masked by a same-named `.ts` command is visible.
        for name in &python_commands {
            if deno_commands.iter().any(|deno_name| deno_name == name) {
                eprintln!(
                    "note: command `{name}` is registered by both a deno and a python \
                     extension; the deno registration takes precedence (the python one \
                     is shadowed)"
                );
            }
        }

        // Cross-engine TOOL/flag-name conflict diagnostics are NOT emitted here:
        // the loader returns the FULL UNION of both engines' extensions and the
        // orchestrator's `add_extension_conflict_diagnostics` runs the shared
        // `detect_extension_conflicts` over that union exactly once (single source
        // of truth) — matching single-engine behavior. Duplicating the check here
        // would double-report a cross-engine collision.
        LoadExtensionsResult {
            extensions,
            errors,
            runtime: Some(Box::new(CombinedExtensionRuntime {
                #[cfg(feature = "deno")]
                deno: Mutex::new(deno_runtime),
                #[cfg(feature = "python")]
                python: Mutex::new(python_runtime),
            })),
        }
    }
}

/// Compute an [`Extension`]'s `resolved_path` the SAME way the orchestrator's
/// `resolveExtensionLoadPath` and both engine loaders do — a **logical**
/// `resolve_path(path, cwd, { normalize_unicode_spaces: true })`, NOT a realpath —
/// so the two-pass dedup keys match. Falls back to the input path on failure.
fn logical_resolved_path(path: &str, cwd: &str) -> String {
    let options = PathInputOptions {
        normalize_unicode_spaces: true,
        ..PathInputOptions::default()
    };
    resolve_path(path, cwd, &options).unwrap_or_else(|_| path.to_string())
}

// ---------------------------------------------------------------------------
// CombinedExtensionRuntime
// ---------------------------------------------------------------------------

/// The combined [`ExtensionRuntime`], holding each engine's inner runtime behind
/// a `Mutex<Option<..>>` so pass-2 (and the runner factory) can `take()` /
/// borrow through the shared `&self` the seam threads. Opaque to the orchestrator
/// (it only moves the handle); the combined runner factory recovers it via
/// [`ExtensionRuntime::as_any`].
pub struct CombinedExtensionRuntime {
    #[cfg(feature = "deno")]
    deno: Mutex<Option<Box<dyn ExtensionRuntime>>>,
    #[cfg(feature = "python")]
    python: Mutex<Option<Box<dyn ExtensionRuntime>>>,
}

impl ExtensionRuntime for CombinedExtensionRuntime {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl CombinedExtensionRuntime {
    /// Take the deno inner runtime out through the shared handle (pass-2 threading).
    #[cfg(feature = "deno")]
    fn take_deno(&self) -> Option<Box<dyn ExtensionRuntime>> {
        self.deno.lock().unwrap().take()
    }

    /// Take the python inner runtime out through the shared handle.
    #[cfg(feature = "python")]
    fn take_python(&self) -> Option<Box<dyn ExtensionRuntime>> {
        self.python.lock().unwrap().take()
    }

    /// Build the deno inner runner from the BORROWED deno inner runtime (held
    /// under the lock for the duration of `build`), if present. The produced
    /// runner is owned and borrows nothing from the guard.
    #[cfg(feature = "deno")]
    fn build_deno_runner<R>(&self, build: impl FnOnce(&dyn ExtensionRuntime) -> R) -> Option<R> {
        let guard = self.deno.lock().unwrap();
        guard.as_deref().map(build)
    }

    /// Build the python inner runner from the BORROWED python inner runtime.
    #[cfg(feature = "python")]
    fn build_python_runner<R>(&self, build: impl FnOnce(&dyn ExtensionRuntime) -> R) -> Option<R> {
        let guard = self.python.lock().unwrap();
        guard.as_deref().map(build)
    }
}

/// The two inner runtimes split out of an incoming combined runtime (pass-2).
#[derive(Default)]
struct SplitRuntimes {
    #[cfg(feature = "deno")]
    deno: Option<Box<dyn ExtensionRuntime>>,
    #[cfg(feature = "python")]
    python: Option<Box<dyn ExtensionRuntime>>,
}

/// Split a supplied combined runtime into its per-engine inner runtimes, taking
/// each inner out through the shared handle. A foreign or absent runtime yields
/// both `None`, so each inner loader mints a fresh runtime instead.
fn split_runtime(runtime: Option<Box<dyn ExtensionRuntime>>) -> SplitRuntimes {
    if let Some(runtime) = runtime {
        if let Some(combined) = runtime.as_any().downcast_ref::<CombinedExtensionRuntime>() {
            return SplitRuntimes {
                #[cfg(feature = "deno")]
                deno: combined.take_deno(),
                #[cfg(feature = "python")]
                python: combined.take_python(),
            };
        }
    }
    SplitRuntimes::default()
}

// ---------------------------------------------------------------------------
// CombinedExtensionRunner
// ---------------------------------------------------------------------------

/// The combined `impl ExtensionRunner`: it fans every method out across the inner
/// runners in DENO-FIRST order. Query methods concat, `get_command` and the
/// short-circuit emitters take the first inner that answers, `has_handlers` ORs,
/// and the binding methods forward to every inner.
pub struct CombinedExtensionRunner {
    /// The inner runners, DENO-FIRST (so first-decision-wins emitters and
    /// `get_command` resolve deno-first).
    inners: Vec<Box<dyn ExtensionRunnerTrait>>,
}

/// Build the combined runner over the loader's combined runtime, constructing
/// each inner runner from a BORROWED inner runtime (the combined runtime owns each
/// inner box and cannot move it out). DENO-FIRST ordering. On a foreign runtime
/// the runner has no inners (every method is inert).
///
/// The deno host handles (`session_manager` / `model_registry`) are present only
/// on builds that compile the deno engine — the python-only build's factory has
/// the python factory's leaner shape, matching each engine's own factory.
pub fn create_combined_extension_runner(
    extensions: Vec<Extension>,
    runtime: Box<dyn ExtensionRuntime>,
    cwd: impl Into<String>,
    #[cfg(feature = "deno")] session_manager: Arc<SessionManager>,
    #[cfg(feature = "deno")] model_registry: Arc<ModelRegistry>,
) -> Box<dyn ExtensionRunnerTrait> {
    let cwd = cwd.into();
    let mut inners: Vec<Box<dyn ExtensionRunnerTrait>> = Vec::new();

    if let Some(combined) = runtime.as_any().downcast_ref::<CombinedExtensionRuntime>() {
        // DENO-FIRST: push the deno inner before the python inner.
        #[cfg(feature = "deno")]
        if let Some(runner) = combined.build_deno_runner(|rt| {
            create_deno_extension_runner_from_runtime_ref(
                extensions.clone(),
                rt,
                cwd.clone(),
                Arc::clone(&session_manager),
                Arc::clone(&model_registry),
            )
        }) {
            inners.push(runner);
        }

        #[cfg(feature = "python")]
        if let Some(runner) = combined.build_python_runner(|rt| {
            create_python_extension_runner_from_runtime_ref(extensions.clone(), rt, cwd.clone())
        }) {
            inners.push(runner);
        }
    }

    Box::new(CombinedExtensionRunner { inners })
}

impl ExtensionRunnerTrait for CombinedExtensionRunner {
    // ---- lifecycle -------------------------------------------------------
    fn emit_session_shutdown(&self, event: SessionShutdownEvent) {
        for inner in &self.inners {
            inner.emit_session_shutdown(event.clone());
        }
    }

    // ---- generic dispatch ------------------------------------------------
    fn emit(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        // Run every inner (side effects — e.g. session_start hooks — must fire on
        // both engines), keeping the FIRST non-None outcome (deno-first).
        let mut outcome = ExtensionEmitOutcome::None;
        for inner in &self.inners {
            let inner_outcome = inner.emit(event);
            if matches!(outcome, ExtensionEmitOutcome::None)
                && !matches!(inner_outcome, ExtensionEmitOutcome::None)
            {
                outcome = inner_outcome;
            }
        }
        outcome
    }

    // ---- dedicated emitters ----------------------------------------------
    fn emit_message_end(&self, event: &MessageEndEvent) -> Option<AgentMessage> {
        for inner in &self.inners {
            if let Some(message) = inner.emit_message_end(event) {
                return Some(message);
            }
        }
        None
    }

    fn emit_input(
        &self,
        text: &str,
        images: Option<&[ImageContent]>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> InputEventResult {
        // First inner that transforms or handles the input wins (deno-first);
        // otherwise the input passes through unchanged.
        for inner in &self.inners {
            let result = inner.emit_input(text, images, source, streaming_behavior);
            if !matches!(result, InputEventResult::Continue) {
                return result;
            }
        }
        InputEventResult::Continue
    }

    fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<&[ImageContent]>,
        system_prompt: &str,
        system_prompt_options: &BuildSystemPromptOptions,
    ) -> Option<BeforeAgentStartCombinedResult> {
        for inner in &self.inners {
            if let Some(result) =
                inner.emit_before_agent_start(prompt, images, system_prompt, system_prompt_options)
            {
                return Some(result);
            }
        }
        None
    }

    fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult {
        // Concat every inner's discovered resources.
        let mut merged = ResourcesDiscoverResult::default();
        for inner in &self.inners {
            let mut result = inner.emit_resources_discover(cwd, reason);
            merged.skill_paths.append(&mut result.skill_paths);
            merged.prompt_paths.append(&mut result.prompt_paths);
            merged.theme_paths.append(&mut result.theme_paths);
        }
        merged
    }

    fn emit_tool_call(&self, event: &ToolCallEvent) -> Option<ToolCallEventResult> {
        // First-decision-wins, deno-first: short-circuit on the first block.
        for inner in &self.inners {
            if let Some(result) = inner.emit_tool_call(event) {
                return Some(result);
            }
        }
        None
    }

    fn emit_tool_result(&self, event: &ToolResultEvent) -> Option<ToolResultEventResult> {
        for inner in &self.inners {
            if let Some(result) = inner.emit_tool_result(event) {
                return Some(result);
            }
        }
        None
    }

    // ---- sync queries ----------------------------------------------------
    fn has_handlers(&self, event_type: &str) -> bool {
        self.inners
            .iter()
            .any(|inner| inner.has_handlers(event_type))
    }

    fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        // Deno-first: the first inner that resolves the name wins (a cross-engine
        // command shadow, already logged at load time).
        for inner in &self.inners {
            if let Some(command) = inner.get_command(name) {
                return Some(command);
            }
        }
        None
    }

    fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        let mut commands = Vec::new();
        for inner in &self.inners {
            commands.extend(inner.get_registered_commands());
        }
        commands
    }

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut tools = Vec::new();
        for inner in &self.inners {
            tools.extend(inner.get_all_registered_tools());
        }
        tools
    }

    fn get_flag_values(&self) -> BTreeMap<String, FlagValue> {
        // Deno-first precedence: the first inner to set a flag name keeps it.
        let mut values: BTreeMap<String, FlagValue> = BTreeMap::new();
        for inner in &self.inners {
            for (name, value) in inner.get_flag_values() {
                values.entry(name).or_insert(value);
            }
        }
        values
    }

    fn create_command_context(&self) -> Box<dyn CommandContext> {
        // Deno-first: the first inner mints the context; with no inner a trivial
        // empty context is returned.
        match self.inners.first() {
            Some(inner) => inner.create_command_context(),
            None => Box::new(CombinedEmptyCommandContext),
        }
    }

    // ---- binding / mutation (forward to ALL inners) ----------------------
    fn bind_core(
        &self,
        actions: Arc<dyn SessionControlHost>,
        context_actions: Arc<dyn SessionContextHost>,
        provider_actions: Option<Arc<dyn ProviderRegistrationHost>>,
    ) {
        for inner in &self.inners {
            inner.bind_core(
                Arc::clone(&actions),
                Arc::clone(&context_actions),
                provider_actions.clone(),
            );
        }
    }

    fn set_ui_context(&self, ui_context: Option<ExtensionUIContext>, mode: ExtensionMode) {
        for inner in &self.inners {
            inner.set_ui_context(ui_context.clone(), mode);
        }
    }

    fn bind_command_context(&self, actions: Option<Arc<dyn ExtensionCommandContextHost>>) {
        for inner in &self.inners {
            inner.bind_command_context(actions.clone());
        }
    }

    fn on_error(&self, listener: ExtensionErrorListener) -> UnsubscribeFn {
        // Register with every inner; the returned unsubscribe removes all of them.
        let unsubscribes: Vec<UnsubscribeFn> = self
            .inners
            .iter()
            .map(|inner| inner.on_error(Arc::clone(&listener)))
            .collect();
        Box::new(move || {
            for unsubscribe in unsubscribes {
                unsubscribe();
            }
        })
    }

    fn emit_error(&self, error: ExtensionError) {
        for inner in &self.inners {
            inner.emit_error(error.clone());
        }
    }

    fn invalidate(&self, message: &str) {
        for inner in &self.inners {
            inner.invalidate(message);
        }
    }
}

/// The trivial [`CommandContext`] the combined runner mints when it holds no
/// inner runners (nothing to delegate to).
struct CombinedEmptyCommandContext;

impl ExtensionContext for CombinedEmptyCommandContext {}
impl CommandContext for CombinedEmptyCommandContext {}
