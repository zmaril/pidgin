//! The coding-agent SDK entry point, ported from pi's `core/sdk.ts`.
//!
//! pi's `createAgentSession` is the one-call factory that wires the whole
//! coding-agent stack together: it resolves the working / config directories,
//! builds (or accepts) the [`ModelRuntime`], [`SettingsManager`],
//! [`SessionManager`], and [`DefaultResourceLoader`], restores the model and
//! thinking level from an existing session (falling back through settings and
//! provider defaults), computes the tool allow/deny/default sets, constructs the
//! wrapped [`Agent`], seeds / restores the session tree, and finally hands the
//! whole bag to [`AgentSession::new`].
//!
//! # What this port lands: the OFFLINE slice
//!
//! Every step of `createAgentSession`'s control flow is mirrored **except** the
//! live-provider and extension-host closures, which are documented seams rather
//! than stubs invented here:
//!
//! - **`stream_fn` (pi `sdk.ts:297-325`) — wired (buffered).** pi's `streamFn`
//!   closure snapshots the retry/timeout knobs from the settings manager, layers
//!   `mergeProviderAttributionHeaders`, and calls `modelRuntime.streamSimple(...)`.
//!   The port builds a `Send + Sync` [`StreamFn`](pidgin_agent::types::StreamFn)
//!   that captures a standalone ai [`Models`](pidgin_ai::providers::registry::Models)
//!   handle cloned from the runtime (`Models: Clone`, sharing the original
//!   `auth_context`) plus
//!   a value snapshot of the retry/timeout/telemetry settings read once at factory
//!   time (the [`SettingsManager`] and [`ModelRuntime`] are both `!Send`, so neither
//!   is captured), and delegates through the shared
//!   [`stream_simple_with_attribution`](crate::core::model_runtime::stream_simple_with_attribution)
//!   helper. The [`Agent`] is built with `stream_fn: Some(...)`, so the session
//!   streams from real providers (buffered — a whole turn resolves before the loop
//!   iterates its events). Remaining follow-ups: **incremental streaming**
//!   ([`Models::stream_incremental`](pidgin_ai::providers::registry::Models::stream_incremental),
//!   the per-event `IncrementalStreamFn` path), the extension
//!   `before_provider_headers` header hook (#186), a full
//!   faux-turn end-to-end test (converges on #269), and the `model_runtime`
//!   rebuild-per-call sharing gap (below). The five [`AgentOptions`] fields the
//!   ported struct omits (`onPayload`, `onResponse`, `transport`, `thinkingBudgets`,
//!   `maxRetryDelayMs`) are unchanged deferrals — see [`AgentOptions`]'s own doc.
//! - **the block-images `convertToLlm` wrapper (pi `sdk.ts:251-285`) — landed.**
//!   pi wraps `convertToLlm` in a closure that re-reads
//!   `settingsManager.getBlockImages()` on every call. The port runs the wrapper
//!   at the Agent's [`pidgin_agent::types::ConvertToLlm`] seam, on the
//!   *already-converted* [`pidgin_ai::Message`] list produced by
//!   [`pidgin_agent::harness::messages::convert_to_llm`] — a pure transform that
//!   avoids bridging the coding crate's typed mirror messages
//!   (`core::messages`). Since [`SettingsManager`] is `!Send` and cannot be
//!   captured by the `Send + Sync` closure, the setting is read through a shared
//!   `Arc<AtomicBool>` mirror (`SettingsManager::block_images_flag`) that the
//!   manager keeps in sync with `getBlockImages()`, so a mid-session
//!   `setBlockImages` toggle is observed live. See [`crate::core::block_images`].
//! - **`transform_context` (pi `sdk.ts:345-349`) — deferred.** pi routes context
//!   through `extensionRunnerRef.current.emitContext`; the extension-runner ref is
//!   the extension-host wiring tracked by #186. Built with `None`.
//! - **`extension_runner` — deferred to the stub.** pi passes a mutable
//!   `extensionRunnerRef`; the ported [`AgentSessionConfig`] takes an owned
//!   `Option<Box<dyn ExtensionRunner>>`, and `None` defaults to
//!   `StubExtensionRunner`. Real wiring is the extension host, #186.
//!
//! Two further faithful-adaptation notes, forced by the ported surface:
//!
//! - `time("resourceLoader.reload")` (pi `sdk.ts:179`) is **omitted**: the ported
//!   [`crate::core::timings`] API is instance-based (`&mut self`, a `Namespace`,
//!   and an explicit `now_ms`) with no module-global singleton, so the pure
//!   debug-timing hook has no destination.
//! - pi's per-tool factory re-exports (`createReadTool` / `createBashTool` / …)
//!   are **not re-exported**: the ported tools module exposes
//!   `create_coding_tools` / `create_read_only_tools` / `create_tool` / `ToolName`
//!   and an async `with_file_mutation_queue` instead of pi's individual
//!   `AgentTool` factories, so there are no matching symbols to re-export. Per
//!   `notes/conventions.md` these pure re-exports are skipped rather than
//!   invented; they are public-API convenience only and unused by
//!   `create_agent_session`.
//!
//! Finally, mirroring pi's `export * from "./agent-session-runtime.ts"` (pi
//! `sdk.ts:94`), the SDK surfaces the [`AgentSessionRuntime`] lifecycle orchestrator
//! and a runtime-returning entry point ([`create_agent_session_runtime_from_options`])
//! that wraps this offline [`create_agent_session`] façade in a session factory and
//! hands it to [`create_agent_session_runtime`]. The runtime owns the current session
//! and swaps it on `/new`, `/resume`, and `/fork`. The `AgentSessionRuntime` type
//! itself lives in the AgentSession lane (`agent_session::runtime`); the SDK only
//! re-exports it and supplies the factory adaptation, mirroring pi's production
//! `createRuntime` closure (pi `main.ts:615`) minus the still-unported services /
//! diagnostics / project-trust surface.
//!
//! # Runtime factory divergence: per-call `model_runtime` rebuild
//!
//! pi shares one `modelRuntime` across the runtime's whole life (built once in
//! `createAgentSessionServices` and reused for every `/new`, `/resume`, and `/fork`).
//! The ported [`create_agent_session`] consumes [`ModelRuntime`] **by value** (it is
//! `!Send` and not `Clone`), so the factory cannot re-hand the same runtime to each
//! call; it passes `model_runtime: None`, which rebuilds a fresh runtime from
//! `{agent_dir}/auth.json` + `{agent_dir}/models.json` on every replacement. Any
//! runtime-set API key or interactive login (which live only on the in-memory
//! [`ModelRuntime`], not on disk) is therefore dropped across a session switch. The
//! faithful fix needs a shareable / re-lent `ModelRuntime`; this is a named
//! follow-up. See the `TODO(follow-up)` at the factory site.

// straitjacket-allow-file:duplication

use std::collections::HashSet;

use std::sync::Arc;

use pidgin_agent::agent::{Agent, AgentOptions, InitialAgentState, QueueMode};
use pidgin_agent::types::{AgentTool, StreamFn};

use crate::core::system_prompt::{build_system_prompt, BuildSystemPromptOptions};
use crate::core::tools::index::{create_coding_tool_definitions, ToolsOptions};
use crate::core::tools::tool_definition_wrapper::wrap_tool_definition;
use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{clamp_thinking_level, Context, Model, ModelThinkingLevel, SimpleStreamOptions};

use crate::core::agent_session::{AgentSession, AgentSessionConfig, ScopedModel};
use crate::core::auth::auth_guidance::format_no_models_available_message;
use crate::core::block_images::block_images_converter;
use crate::core::defaults::DEFAULT_THINKING_LEVEL;
use crate::core::extensions::events::session::SessionStartEvent;
use crate::core::extensions::loader::LoadExtensionsResult;
use crate::core::extensions::types::ToolDefinition;
use crate::core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS;
use crate::core::model_resolver::{find_initial_model, FindInitialModelOptions, ModelRuntimeView};
use crate::core::model_runtime::{
    stream_simple_with_attribution, CreateModelRuntimeOptions, ModelRuntime, ModelsPath,
};
use crate::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ReloadOptions,
};
use crate::core::session_cwd::MissingSessionCwdError;
use crate::core::session_manager::{get_default_session_dir, SessionEntry, SessionManager};
use crate::core::settings_manager::SettingsManager;
use crate::core::skills::get_agent_dir;
use crate::utils::paths::{resolve_path, PathInputOptions};

// Re-exports (pi `sdk.ts:92-94`: `export * from "./agent-session-runtime.ts"`).
//
// The SDK surfaces the `AgentSessionRuntime` session-lifecycle orchestrator and its
// public surface. The type is owned by the AgentSession lane (`agent_session::runtime`);
// the SDK only re-exports it and supplies the factory adaptation below.
pub use crate::core::agent_session::{
    create_agent_session_runtime, AgentSessionRuntime, AgentSessionRuntimeError,
    AgentSessionRuntimeFactoryOptions, AgentSessionRuntimeResult, BeforeSessionInvalidate,
    CreateAgentSessionRuntimeFactory, ForkOptions, ForkResult, NewSessionOptions, RebindSession,
    SwitchResult,
};

/// Default tool suppression mode when no explicit allowlist is provided (pi's
/// `noTools?: "all" | "builtin"`, `sdk.ts:56`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoTools {
    /// Start with no tools enabled.
    All,
    /// Disable the default built-in tools (read, bash, edit, write) but keep
    /// extension/custom tools enabled.
    Builtin,
}

/// Options for [`create_agent_session`] (pi's `CreateAgentSessionOptions`,
/// `sdk.ts:33-80`).
///
/// Derives [`Default`] so callers spell only the fields they set (pi's
/// `options: CreateAgentSessionOptions = {}`).
#[derive(Default)]
pub struct CreateAgentSessionOptions {
    /// Working directory for project-local discovery. Default: process cwd.
    pub cwd: Option<String>,
    /// Global config directory. Default: `~/.pi/agent`.
    pub agent_dir: Option<String>,
    /// Canonical model/auth runtime. Default: a runtime over
    /// `agent_dir/auth.json` and `agent_dir/models.json`.
    pub model_runtime: Option<ModelRuntime>,
    /// Model to use. Default: from settings, else first available.
    pub model: Option<Model>,
    /// Thinking level. Default: from settings, else `medium` (clamped).
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Models available for cycling (Ctrl+P in interactive mode).
    pub scoped_models: Vec<ScopedModel>,
    /// Default tool suppression when no explicit allowlist is provided.
    pub no_tools: Option<NoTools>,
    /// Allowlist of tool names. When provided, only these are enabled.
    pub tools: Option<Vec<String>>,
    /// Denylist of tool names, applied after `tools`.
    pub exclude_tools: Option<Vec<String>>,
    /// Custom tools to register in addition to built-in tools.
    pub custom_tools: Vec<ToolDefinition>,
    /// Resource loader. When omitted, [`DefaultResourceLoader`] is built + reloaded.
    pub resource_loader: Option<DefaultResourceLoader>,
    /// Session manager. Default: `SessionManager::create(cwd, ...)`.
    pub session_manager: Option<SessionManager>,
    /// Settings manager. Default: `SettingsManager::create(cwd, agent_dir)`.
    pub settings_manager: Option<SettingsManager>,
    /// Session start event metadata for extension runtime startup.
    pub session_start_event: Option<SessionStartEvent>,
}

/// Result from [`create_agent_session`] (pi's `CreateAgentSessionResult`,
/// `sdk.ts:83-90`).
pub struct CreateAgentSessionResult {
    /// The created session.
    pub session: AgentSession,
    /// Extensions result (for UI context setup in interactive mode).
    ///
    /// pi returns `resourceLoader.getExtensions()` by JS reference, sharing the
    /// one object with the session's loader. The ported [`AgentSession`] takes
    /// the loader **by value**, and [`LoadExtensionsResult`] is not `Clone` (its
    /// `runtime` handle is move-only), so this field carries a clone of the
    /// loader's `extensions` / `errors` with `runtime: None`; the live runtime
    /// handle stays with the session's loader.
    pub extensions_result: LoadExtensionsResult,
    /// Warning if the session was restored with a different model than saved.
    pub model_fallback_message: Option<String>,
}

// Helper Functions

/// pi's `getDefaultAgentDir` (`sdk.ts:125-127`).
fn get_default_agent_dir() -> String {
    get_agent_dir()
}

/// The process working directory as a string, mirroring pi's `process.cwd()`.
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// pi's `DEFAULT_THINKING_LEVEL` ("medium") lifted into the ported
/// [`ModelThinkingLevel`] enum. The ported [`crate::core::defaults`] still
/// carries the constant as its string tag.
fn default_thinking_level() -> ModelThinkingLevel {
    parse_model_thinking_level(DEFAULT_THINKING_LEVEL).unwrap_or(ModelThinkingLevel::Medium)
}

/// Parse a stored thinking-level tag into [`ModelThinkingLevel`] (the inverse of
/// [`model_thinking_level_tag`]). `None` for an unrecognized tag.
///
/// pi treats thinking levels as plain string literals; the ported enum carries
/// `#[serde(rename_all = "lowercase")]`, so this round-trips through that
/// representation rather than hand-rolling a match (which would also clone the
/// identical arm sequence found elsewhere in the workspace).
fn parse_model_thinking_level(tag: &str) -> Option<ModelThinkingLevel> {
    serde_json::from_value(serde_json::Value::String(tag.to_string())).ok()
}

/// The lowercase serde tag for a [`ModelThinkingLevel`], for persistence through
/// [`SessionManager::append_thinking_level_change`] (which stores a `&str`). The
/// inverse of [`parse_model_thinking_level`], via the same serde representation.
fn model_thinking_level_tag(level: ModelThinkingLevel) -> String {
    match serde_json::to_value(level) {
        Ok(serde_json::Value::String(tag)) => tag,
        _ => String::new(),
    }
}

/// Parse a settings queue-mode string into [`QueueMode`] (pi passes the raw
/// string; the ported [`Agent`] takes the enum). `None` for an unrecognized
/// value, which lets the [`Agent`] apply its own default.
fn parse_queue_mode(mode: &str) -> Option<QueueMode> {
    match mode {
        "all" => Some(QueueMode::All),
        "one-at-a-time" => Some(QueueMode::OneAtATime),
        _ => None,
    }
}

/// Clamp the resolved thinking level to the model's capabilities, mirroring pi's
/// `sdk.ts:233-238`: `off` when no model, otherwise `clampThinkingLevel`.
fn finalize_thinking_level(model: Option<&Model>, level: ModelThinkingLevel) -> ModelThinkingLevel {
    match model {
        None => ModelThinkingLevel::Off,
        Some(model) => clamp_thinking_level(model, level),
    }
}

/// The resolved tool-name sets (pi `sdk.ts:240-246`).
struct ResolvedToolNames {
    /// pi's `allowedToolNames`: `options.tools ?? (noTools === "all" ? [] :
    /// undefined)`.
    allowed: Option<Vec<String>>,
    /// pi's `initialActiveToolNames`, after applying the exclude filter.
    initial_active: Vec<String>,
}

/// Compute the tool allow / default-active sets (pi's `sdk.ts:240-246`).
fn resolve_tool_names(
    tools: Option<&Vec<String>>,
    no_tools: Option<NoTools>,
    exclude_tools: Option<&Vec<String>>,
) -> ResolvedToolNames {
    let default_active: Vec<String> = ["read", "bash", "edit", "write"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // allowedToolNames = options.tools ?? (options.noTools === "all" ? [] : undefined)
    let allowed = match tools {
        Some(t) => Some(t.clone()),
        None => match no_tools {
            Some(NoTools::All) => Some(Vec::new()),
            _ => None,
        },
    };

    let excluded_set: Option<HashSet<&String>> = exclude_tools.map(|v| v.iter().collect());

    // (options.tools ? [...tools] : options.noTools ? [] : defaultActiveToolNames)
    //   .filter((name) => !excludedToolNameSet?.has(name))
    let base = match tools {
        Some(t) => t.clone(),
        None if no_tools.is_some() => Vec::new(),
        None => default_active,
    };
    let initial_active = base
        .into_iter()
        .filter(|name| !excluded_set.as_ref().is_some_and(|s| s.contains(name)))
        .collect();

    ResolvedToolNames {
        allowed,
        initial_active,
    }
}

/// Adapt the concrete [`ModelRuntime`] to the pure resolver's
/// [`ModelRuntimeView`] seam so [`find_initial_model`] can consult it (pi passes
/// the runtime directly; the ported resolver is generic over the view). Inherent
/// methods win over the same-named trait methods, so these forward without
/// recursing.
impl ModelRuntimeView for ModelRuntime {
    fn get_models(&self) -> Vec<Model> {
        self.get_models(None)
    }

    fn get_available(&self) -> Vec<Model> {
        // pi's `getAvailable()` returns the cached availability snapshot, which is
        // the set `has_configured_auth` is derived from — using it keeps the two
        // consistent for the resolver's "first available" pass.
        self.get_available_snapshot().to_vec()
    }

    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.get_model(provider, model_id)
    }

    fn has_configured_auth(&self, provider: &str) -> bool {
        self.has_configured_auth(provider)
    }
}

/// Build the buffered [`StreamFn`] the [`Agent`] uses to reach real providers,
/// pi's `streamFn` closure (`sdk.ts:297-325`).
///
/// The returned closure is `Send + Sync`, so it captures neither the `!Send`
/// [`ModelRuntime`] nor the `!Send` [`SettingsManager`]: it takes a standalone ai
/// [`Models`](pidgin_ai::providers::registry::Models) handle cloned from the
/// runtime (`Models: Clone`, so the clone shares the runtime's original
/// `auth_context` rather than reconstructing a fresh one) and a value snapshot of
/// the retry / timeout / telemetry settings, both read once here at factory time.
///
/// Per call it mirrors pi's option resolution: `timeout_ms`,
/// `websocket_connect_timeout_ms`, `max_retries`, and `max_retry_delay_ms` fall
/// back through the caller's options to the snapshot (pi's `??` chain, with pi's
/// `httpIdleTimeoutMs === 0 ? 2147483647` "disable" sentinel), then it delegates
/// through the shared [`stream_simple_with_attribution`] helper (attribution +
/// [`Models::stream_simple`](pidgin_ai::providers::registry::Models::stream_simple)).
/// The attribution session id is read from the caller's `options.session_id`,
/// exactly as pi passes `options?.sessionId`. The extension
/// `before_provider_headers` header hook (pi `sdk.ts:320-322`) is deferred (#186).
fn build_stream_fn(model_runtime: &ModelRuntime, settings_manager: &SettingsManager) -> StreamFn {
    let models = model_runtime.models.clone();
    let provider_retry = settings_manager.get_provider_retry_settings();
    let http_idle_timeout_ms = settings_manager
        .get_http_idle_timeout_ms()
        .unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS);
    let websocket_connect_timeout_ms = settings_manager
        .get_websocket_connect_timeout_ms()
        .ok()
        .flatten();
    let telemetry_enabled = settings_manager.get_enable_install_telemetry();

    Arc::new(
        move |model: &Model,
              context: &Context,
              options: Option<&SimpleStreamOptions>,
              signal: Option<&AbortSignal>|
              -> StreamResult {
            // pi: `effectiveTimeoutMs = httpIdleTimeoutMs === 0 ? 2147483647 : httpIdleTimeoutMs`.
            // SDKs treat timeout=0 as an immediate timeout, so max int32 effectively disables it.
            let effective_timeout_ms = if http_idle_timeout_ms == 0 {
                i32::MAX as u64
            } else {
                http_idle_timeout_ms
            };
            // pi's `??` fallbacks: caller option, then the settings snapshot. The
            // retry/timeout tuning lives on the base StreamOptions; the caller's
            // `reasoning`/`thinking_budgets` ride alongside and are preserved by the
            // clone below so they reach the drivers.
            let timeout_ms = options
                .and_then(|o| o.base.timeout_ms)
                .or_else(|| provider_retry.timeout_ms.map(|ms| ms as u64))
                .unwrap_or(effective_timeout_ms);
            let websocket_connect_timeout_ms = options
                .and_then(|o| o.base.websocket_connect_timeout_ms)
                .or(websocket_connect_timeout_ms);
            let max_retries = options
                .and_then(|o| o.base.max_retries)
                .or_else(|| provider_retry.max_retries.map(|n| n as u32));
            let max_retry_delay_ms = options
                .and_then(|o| o.base.max_retry_delay_ms)
                .unwrap_or(provider_retry.max_retry_delay_ms as u64);

            // Build the effective options on top of the caller's (pi's `{ ...options, ... }`).
            let mut opts = options.cloned().unwrap_or_default();
            opts.base.timeout_ms = Some(timeout_ms);
            opts.base.websocket_connect_timeout_ms = websocket_connect_timeout_ms;
            opts.base.max_retries = max_retries;
            opts.base.max_retry_delay_ms = Some(max_retry_delay_ms);

            // pi threads `options?.sessionId` into `mergeProviderAttributionHeaders`.
            let session_id = opts.base.session_id.clone();
            stream_simple_with_attribution(
                &models,
                telemetry_enabled,
                session_id.as_deref(),
                model,
                context,
                Some(&opts),
                signal,
            )
        },
    )
}

/// Create an [`AgentSession`] with the specified options (pi's
/// `createAgentSession`, `sdk.ts:164-393`).
///
/// pi's function is `async` and can throw; the ported collaborators are all
/// The builtin providers used to seed a default [`ModelRuntime`] built by
/// [`create_agent_session`], with a live reqwest transport bound when
/// `native-http` is enabled (the shipped CLI default). Mirrors
/// [`modes::print`](crate::modes::print)'s `builtin_registry_providers` split.
#[cfg(feature = "native-http")]
fn live_builtin_providers() -> Vec<pidgin_ai::RegistryProvider> {
    use pidgin_ai::seams::clock::{Clock, SystemClock};
    use pidgin_ai::seams::http::HttpTransport;
    use pidgin_ai::seams::ReqwestTransport;

    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::builder().build());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    pidgin_ai::builtin_providers_with_transport(transport, clock)
}

/// Without `native-http` the builtins carry no transport: every model routes
/// `Unimplemented`, matching the reqwest-free default/test build.
#[cfg(not(feature = "native-http"))]
fn live_builtin_providers() -> Vec<pidgin_ai::RegistryProvider> {
    pidgin_ai::builtin_providers()
}

/// Build the default coding tool set and its matching system prompt for `cwd`,
/// mirroring [`modes::print::coding_harness_setup`](crate::modes::print). The
/// per-tool prompt snippets and guidelines are gathered from the active tool
/// definitions (pi's `AgentSession._rebuildSystemPrompt`) and fed to
/// [`build_system_prompt`], so the session receives real tools and a non-empty
/// system prompt.
fn build_coding_harness(cwd: &str) -> (Vec<AgentTool>, String) {
    let definitions = create_coding_tool_definitions(cwd, ToolsOptions::default());

    let mut active_tool_names = Vec::with_capacity(definitions.len());
    let mut tool_snippets: Vec<(String, String)> = Vec::new();
    let mut prompt_guidelines: Vec<String> = Vec::new();
    for definition in &definitions {
        active_tool_names.push(definition.name.clone());
        if let Some(snippet) = &definition.prompt_snippet {
            tool_snippets.push((definition.name.clone(), snippet.clone()));
        }
        if let Some(guidelines) = &definition.prompt_guidelines {
            prompt_guidelines.extend(guidelines.iter().cloned());
        }
    }

    let system_prompt = build_system_prompt(&BuildSystemPromptOptions {
        cwd: cwd.to_string(),
        selected_tools: Some(active_tool_names),
        tool_snippets,
        prompt_guidelines,
        ..Default::default()
    });

    let tools = definitions
        .into_iter()
        .map(|definition| wrap_tool_definition(definition, None))
        .collect();

    (tools, system_prompt)
}

/// synchronous and infallible on this offline slice, so this mirror is a plain
/// synchronous function. See the module docs for the deferred live-provider and
/// extension-host seams.
pub fn create_agent_session(options: CreateAgentSessionOptions) -> CreateAgentSessionResult {
    let base = process_cwd();
    let path_opts = PathInputOptions::default();

    // cwd = resolvePath(options.cwd ?? sessionManager?.getCwd() ?? process.cwd())
    let cwd_input = options
        .cwd
        .clone()
        .or_else(|| {
            options
                .session_manager
                .as_ref()
                .map(|sm| sm.get_cwd().to_string())
        })
        .unwrap_or_else(|| base.clone());
    let cwd = resolve_path(&cwd_input, &base, &path_opts).unwrap_or(cwd_input);

    // agentDir = options.agentDir ? resolvePath(options.agentDir) : getDefaultAgentDir()
    let agent_dir = match &options.agent_dir {
        Some(dir) => resolve_path(dir, &base, &path_opts).unwrap_or_else(|_| dir.clone()),
        None => get_default_agent_dir(),
    };

    // authPath/modelsPath are set only when options.agentDir was explicitly given.
    let (auth_path, models_path) = if options.agent_dir.is_some() {
        (
            Some(format!("{agent_dir}/auth.json")),
            ModelsPath::Path(format!("{agent_dir}/models.json")),
        )
    } else {
        (None, ModelsPath::Default)
    };
    let model_runtime = options.model_runtime.unwrap_or_else(|| {
        ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path,
            models_path,
            // Bind the live reqwest transport into the builtin provider set under
            // `native-http` (the shipped CLI default), so an `AgentSession` built
            // through this path can actually reach a provider. Without the feature
            // the builtins have no transport and route `Unimplemented`, exactly as
            // before — the offline/test build is unchanged. Mirrors the split
            // `modes::print` already applies to its own registry.
            builtins: Some(live_builtin_providers()),
            ..Default::default()
        })
    });

    let settings_manager = options
        .settings_manager
        .unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir));
    let mut session_manager = options.session_manager.unwrap_or_else(|| {
        SessionManager::create(&cwd, Some(&get_default_session_dir(&cwd)), None)
    });

    // resourceLoader: build + reload the default only when none is supplied. pi
    // passes its `settingsManager` into the loader (one shared reference); the
    // ported SettingsManager is a move-only value, so the default loader builds
    // its own equivalent instance from the same cwd/agent_dir.
    let resource_loader = match options.resource_loader {
        Some(loader) => loader,
        None => {
            let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: cwd.clone(),
                agent_dir: agent_dir.clone(),
                ..Default::default()
            });
            loader.reload(ReloadOptions::default());
            // pi calls `time("resourceLoader.reload")` here; omitted — see module docs.
            loader
        }
    };

    // Check if the session has existing data to restore.
    let existing_session = session_manager.build_session_context();
    let has_existing_session = !existing_session.messages.is_empty();
    let has_thinking_entry = session_manager
        .get_branch(None)
        .iter()
        .any(|entry| matches!(entry, SessionEntry::ThinkingLevelChange(_)));

    let mut model = options.model.clone();
    let mut model_fallback_message: Option<String> = None;

    // If the session has data, try to restore the model from it.
    if model.is_none() && has_existing_session {
        if let Some(saved) = &existing_session.model {
            if let Some(restored) = model_runtime.get_model(&saved.provider, &saved.model_id) {
                if model_runtime.has_configured_auth(&restored.provider) {
                    model = Some(restored);
                }
            }
            if model.is_none() {
                model_fallback_message = Some(format!(
                    "Could not restore model {}/{}",
                    saved.provider, saved.model_id
                ));
            }
        }
    }

    // If still no model, use findInitialModel (settings default, then provider defaults).
    if model.is_none() {
        let default_provider = settings_manager.get_default_provider();
        let default_model_id = settings_manager.get_default_model();
        let default_tl = settings_manager.get_default_thinking_level();
        let result = find_initial_model(
            FindInitialModelOptions {
                is_continuing: has_existing_session,
                default_provider: default_provider.as_deref(),
                default_model_id: default_model_id.as_deref(),
                default_thinking_level: default_tl,
                ..Default::default()
            },
            &model_runtime,
        );
        model = result.ok().and_then(|r| r.model);
        match &model {
            None => model_fallback_message = Some(format_no_models_available_message()),
            Some(m) => {
                if let Some(msg) = model_fallback_message.take() {
                    model_fallback_message = Some(format!("{msg}. Using {}/{}", m.provider, m.id));
                }
            }
        }
    }

    let mut thinking_level = options.thinking_level;

    // If the session has data, restore the thinking level from it.
    if thinking_level.is_none() && has_existing_session {
        thinking_level = Some(if has_thinking_entry {
            parse_model_thinking_level(&existing_session.thinking_level)
                .unwrap_or_else(default_thinking_level)
        } else {
            settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(default_thinking_level)
        });
    }

    // Fall back to the settings default.
    if thinking_level.is_none() {
        thinking_level = Some(
            settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(default_thinking_level),
        );
    }

    // Clamp to model capabilities (or `off` when there is no model).
    let thinking_level = finalize_thinking_level(model.as_ref(), thinking_level.unwrap());

    let ResolvedToolNames {
        allowed: allowed_tool_names,
        initial_active: initial_active_tool_names,
    } = resolve_tool_names(
        options.tools.as_ref(),
        options.no_tools,
        options.exclude_tools.as_ref(),
    );
    let excluded_tool_names = options.exclude_tools.clone();

    // The block-images convertToLlm wrapper (pi `sdk.ts:251-285`) filters image
    // blocks out of the converted output when `getBlockImages()` is set; it reads
    // a shared `Arc<AtomicBool>` mirror so a mid-session toggle takes effect live,
    // matching pi's per-call setting read. The onPayload / onResponse / transport /
    // thinkingBudgets / maxRetryDelayMs closures and options (pi `sdk.ts:326-355`)
    // are deferred — see the module docs. The streamFn is now wired (buffered) below.
    let session_id = session_manager.get_session_id().to_string();
    let convert_to_llm = block_images_converter(settings_manager.block_images_flag());

    // Build the buffered `stream_fn` (pi's `streamFn`, `sdk.ts:297-325`). Snapshot
    // the ai `Models` handle and the retry/timeout/telemetry settings BEFORE
    // `model_runtime` is moved into `AgentSessionConfig` and while `settings_manager`
    // is still in scope: both are `!Send`, so the `Send + Sync` closure captures only
    // the extracted `Models` handle plus a value snapshot of the settings.
    let stream_fn = build_stream_fn(&model_runtime, &settings_manager);

    // Build the coding tool set and the matching system prompt (mirrors
    // `modes::print::coding_harness_setup`). Without this the agent would carry an
    // empty system prompt — an empty system block with cache_control is rejected
    // by Anthropic (`400 cache_control cannot be set for empty text blocks`) — and
    // no tools, so the model could not read/edit/run anything.
    let (coding_tools, coding_system_prompt) = build_coding_harness(&cwd);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some(coding_system_prompt),
            model: model.clone(),
            thinking_level: Some(thinking_level),
            tools: Some(coding_tools),
            ..Default::default()
        }),
        convert_to_llm: Some(convert_to_llm),
        steering_mode: parse_queue_mode(&settings_manager.get_steering_mode()),
        follow_up_mode: parse_queue_mode(&settings_manager.get_follow_up_mode()),
        session_id: Some(session_id),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    // Restore messages if the session has existing data (pi `sdk.ts:357-369`).
    if has_existing_session {
        agent.set_messages(existing_session.messages);
        if !has_thinking_entry {
            session_manager.append_thinking_level_change(&model_thinking_level_tag(thinking_level));
        }
    } else {
        // Save the initial model and thinking level for new sessions so they can
        // be restored on resume.
        if let Some(m) = &model {
            session_manager.append_model_change(&m.provider, &m.id);
        }
        session_manager.append_thinking_level_change(&model_thinking_level_tag(thinking_level));
    }

    // pi returns `resourceLoader.getExtensions()` after constructing the session
    // (JS shares the object by reference). The ported loader is moved into the
    // session, so snapshot the extensions/errors first; see the field doc.
    let extensions_result = {
        let extensions = resource_loader.get_extensions();
        LoadExtensionsResult {
            extensions: extensions.extensions.clone(),
            errors: extensions.errors.clone(),
            runtime: None,
        }
    };

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd,
        scoped_models: options.scoped_models,
        resource_loader,
        custom_tools: options.custom_tools,
        model_runtime,
        initial_active_tool_names: Some(initial_active_tool_names),
        allowed_tool_names,
        excluded_tool_names,
        base_tools_override: None,
        // pi passes a mutable `extensionRunnerRef`; deferred to the stub (#186).
        extension_runner: None,
        session_start_event: options.session_start_event,
        // pi passes `this.agent.streamFn` for compaction; the offline slice has no
        // summarizer, so compaction summarization is part of the deferred
        // credential-aware streaming surface.
        summarization_models: None,
    });

    CreateAgentSessionResult {
        session,
        extensions_result,
        model_fallback_message,
    }
}

// Runtime wiring
//
// pi's production `createRuntime` closure (pi `main.ts:615-739`) closes over the
// process-global "fixed" session inputs, recreates the cwd-bound services for the
// per-call cwd, and calls `createAgentSessionFromServices` -> `createAgentSession`.
// The ported surface trims the services / diagnostics / project-trust layer, so the
// factory here wraps the plain [`create_agent_session`] instead.

/// The fixed [`create_agent_session`] inputs the runtime's session factory closes
/// over, mirroring the process-global options pi's `createRuntime` captures (pi
/// `main.ts:615`).
///
/// Each factory call clones these into a [`CreateAgentSessionOptions`] and merges the
/// per-call cwd / agent_dir / session_manager / session_start_event handed in by the
/// runtime. The per-call `model_runtime` / `settings_manager` / `resource_loader` are
/// left `None` so [`create_agent_session`] rebuilds them cwd-bound — mirroring pi's
/// per-cwd `createAgentSessionServices` rebuild (with the `model_runtime` divergence
/// noted in the module docs).
#[derive(Default, Clone)]
pub struct CreateAgentSessionRuntimeFixedOptions {
    /// Model to use. Default: from settings, else first available.
    pub model: Option<Model>,
    /// Thinking level. Default: from settings, else `medium` (clamped).
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Models available for cycling (Ctrl+P in interactive mode).
    pub scoped_models: Vec<ScopedModel>,
    /// Default tool suppression when no explicit allowlist is provided.
    pub no_tools: Option<NoTools>,
    /// Allowlist of tool names. When provided, only these are enabled.
    pub tools: Option<Vec<String>>,
    /// Denylist of tool names, applied after `tools`.
    pub exclude_tools: Option<Vec<String>>,
    /// Custom tools to register in addition to built-in tools.
    pub custom_tools: Vec<ToolDefinition>,
}

/// Build the session factory the runtime reuses for every `/new`, `/resume`, and
/// `/fork` (pi's `createRuntime` closure, `main.ts:615-739`).
///
/// The returned closure captures the fixed inputs and, per call, merges the runtime's
/// [`AgentSessionRuntimeFactoryOptions`] into a [`CreateAgentSessionOptions`], calls
/// [`create_agent_session`], and adapts the [`CreateAgentSessionResult`] into an
/// [`AgentSessionRuntimeResult`] — passing the per-call `cwd` straight through (pi
/// returns `services.cwd`) and dropping `extensions_result` (the runtime does not
/// carry it; the services / diagnostics surface is unported — see the runtime module
/// docs).
pub fn build_create_runtime_factory(
    fixed: CreateAgentSessionRuntimeFixedOptions,
) -> CreateAgentSessionRuntimeFactory {
    Box::new(move |options: AgentSessionRuntimeFactoryOptions| {
        // pi returns `services.cwd`; the runtime resolves cwd before calling the
        // factory, so pass it straight through (no refactor of create_agent_session's
        // internal cwd resolution).
        let cwd = options.cwd.clone();

        // TODO(follow-up): model_runtime rebuild-per-call divergence vs pi. pi shares
        // one modelRuntime across the runtime's life; create_agent_session consumes
        // ModelRuntime by value (!Send, not Clone), so passing None here REBUILDS it
        // from {agent_dir}/auth.json + {agent_dir}/models.json on every new/resume/fork,
        // dropping runtime-set API keys / interactive login. The fix needs a shareable
        // or re-lent ModelRuntime. See the module docs.
        let result = create_agent_session(CreateAgentSessionOptions {
            // Per-call inputs from the runtime.
            cwd: Some(options.cwd),
            agent_dir: Some(options.agent_dir),
            session_manager: Some(options.session_manager),
            session_start_event: options.session_start_event,
            // Fixed inputs captured from the caller.
            model: fixed.model.clone(),
            thinking_level: fixed.thinking_level,
            scoped_models: fixed.scoped_models.clone(),
            no_tools: fixed.no_tools,
            tools: fixed.tools.clone(),
            exclude_tools: fixed.exclude_tools.clone(),
            custom_tools: fixed.custom_tools.clone(),
            // Rebuilt cwd-bound per call (pi's per-cwd createAgentSessionServices).
            model_runtime: None,
            settings_manager: None,
            resource_loader: None,
        });

        AgentSessionRuntimeResult {
            session: result.session,
            cwd,
            model_fallback_message: result.model_fallback_message,
        }
    })
}

/// Create an [`AgentSessionRuntime`] wrapping the offline [`create_agent_session`]
/// façade (mirrors pi's `createAgentSessionRuntime` production surface, wired in pi
/// `main.ts:615-745`).
///
/// Builds the session factory from `fixed` and hands it, together with the `initial`
/// target, to [`create_agent_session_runtime`]. The same factory is reused for every
/// subsequent `/new`, `/resume`, and `/fork`.
pub fn create_agent_session_runtime_from_options(
    fixed: CreateAgentSessionRuntimeFixedOptions,
    initial: AgentSessionRuntimeFactoryOptions,
) -> Result<AgentSessionRuntime, MissingSessionCwdError> {
    let factory = build_create_runtime_factory(fixed);
    create_agent_session_runtime(factory, initial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_level_tag_round_trips() {
        for level in [
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
            ModelThinkingLevel::Max,
        ] {
            let tag = model_thinking_level_tag(level);
            assert_eq!(parse_model_thinking_level(&tag), Some(level));
        }
    }

    #[test]
    fn parse_thinking_level_rejects_unknown() {
        assert_eq!(parse_model_thinking_level("bogus"), None);
    }

    #[test]
    fn default_thinking_level_is_medium() {
        // Mirrors pi's DEFAULT_THINKING_LEVEL = "medium".
        assert_eq!(default_thinking_level(), ModelThinkingLevel::Medium);
        assert_eq!(DEFAULT_THINKING_LEVEL, "medium");
    }

    #[test]
    fn queue_mode_parses_known_values() {
        assert_eq!(parse_queue_mode("all"), Some(QueueMode::All));
        assert_eq!(
            parse_queue_mode("one-at-a-time"),
            Some(QueueMode::OneAtATime)
        );
        assert_eq!(parse_queue_mode(""), None);
    }

    #[test]
    fn finalize_thinking_level_is_off_without_a_model() {
        assert_eq!(
            finalize_thinking_level(None, ModelThinkingLevel::High),
            ModelThinkingLevel::Off
        );
    }

    #[test]
    fn finalize_thinking_level_delegates_to_clamp_with_a_model() {
        let model = crate::core::test_model("m", "p");
        assert_eq!(
            finalize_thinking_level(Some(&model), ModelThinkingLevel::High),
            clamp_thinking_level(&model, ModelThinkingLevel::High)
        );
    }

    // pi `sdk.ts:240-246` — tool-name allow/deny/default computation.

    #[test]
    fn tool_names_default_to_the_builtin_four() {
        let resolved = resolve_tool_names(None, None, None);
        assert_eq!(resolved.allowed, None);
        assert_eq!(
            resolved.initial_active,
            vec!["read", "bash", "edit", "write"]
        );
    }

    #[test]
    fn tool_names_no_tools_all_yields_empty_allowlist_and_active() {
        let resolved = resolve_tool_names(None, Some(NoTools::All), None);
        assert_eq!(resolved.allowed, Some(Vec::new()));
        assert!(resolved.initial_active.is_empty());
    }

    #[test]
    fn tool_names_no_tools_builtin_disables_defaults_without_an_allowlist() {
        let resolved = resolve_tool_names(None, Some(NoTools::Builtin), None);
        // pi: allowedToolNames stays undefined for "builtin"...
        assert_eq!(resolved.allowed, None);
        // ...but the default active set is emptied.
        assert!(resolved.initial_active.is_empty());
    }

    #[test]
    fn tool_names_explicit_allowlist_wins() {
        let tools = vec!["read".to_string(), "grep".to_string()];
        let resolved = resolve_tool_names(Some(&tools), None, None);
        assert_eq!(resolved.allowed, Some(tools.clone()));
        assert_eq!(resolved.initial_active, tools);
    }

    #[test]
    fn tool_names_exclude_filters_the_active_set() {
        let exclude = vec!["bash".to_string(), "write".to_string()];
        let resolved = resolve_tool_names(None, None, Some(&exclude));
        // allowlist untouched by exclude alone.
        assert_eq!(resolved.allowed, None);
        assert_eq!(resolved.initial_active, vec!["read", "edit"]);
    }

    #[test]
    fn tool_names_exclude_applies_after_an_explicit_allowlist() {
        let tools = vec!["read".to_string(), "bash".to_string(), "edit".to_string()];
        let exclude = vec!["bash".to_string()];
        let resolved = resolve_tool_names(Some(&tools), None, Some(&exclude));
        assert_eq!(resolved.allowed, Some(tools));
        assert_eq!(resolved.initial_active, vec!["read", "edit"]);
    }

    // The buffered stream_fn (pi `sdk.ts:297-325`) — the closure captures a
    // Send + Sync `Models` handle plus a value snapshot of the settings, so the
    // built `StreamFn` type-checks as `Send + Sync`. Building it here (offline)
    // proves `build_stream_fn` returns and its captures satisfy the bound.
    #[test]
    fn build_stream_fn_produces_a_send_sync_closure() {
        fn assert_send_sync<T: Send + Sync>(_: &T) {}

        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let settings_manager = SettingsManager::create(&cwd, &cwd);
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            models_path: ModelsPath::Disabled,
            ..Default::default()
        });

        let stream_fn = build_stream_fn(&model_runtime, &settings_manager);
        // The StreamFn alias is `Arc<dyn Fn(...) + Send + Sync>`; this fails to
        // compile if the captured Models / settings snapshot were not Send + Sync.
        assert_send_sync(&stream_fn);
    }

    // A construction test that builds a real AgentSession fully offline: an
    // in-memory session (no disk), no configured providers, and a freshly built
    // resource loader. Exercises the no-model fallback path end to end. With the
    // stream_fn now wired (Some), this also proves the live closure does not break
    // offline construction (it is never invoked at construction time).
    #[test]
    fn create_agent_session_builds_offline_with_no_models() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();

        let session_manager = SessionManager::in_memory(&cwd);
        let settings_manager = SettingsManager::create(&cwd, &cwd);
        let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: cwd.clone(),
            ..Default::default()
        });
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            models_path: ModelsPath::Disabled,
            ..Default::default()
        });

        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd),
            session_manager: Some(session_manager),
            settings_manager: Some(settings_manager),
            resource_loader: Some(resource_loader),
            model_runtime: Some(model_runtime),
            ..Default::default()
        });

        // No providers configured -> the no-models-available fallback fires.
        assert_eq!(
            result.model_fallback_message,
            Some(format_no_models_available_message())
        );
        // No agent model, so the thinking level clamps to `off`.
        assert_eq!(
            result.session.agent.thinking_level(),
            ModelThinkingLevel::Off
        );
        // The freshly built loader has no extensions.
        assert!(result.extensions_result.extensions.is_empty());
        assert!(result.extensions_result.runtime.is_none());
    }

    // Build an AgentSessionRuntime through the REAL create_agent_session factory
    // (offline, no configured providers) and drive one `new_session` replacement. This
    // exercises the factory adaptation (CreateAgentSessionResult -> AgentSessionRuntimeResult,
    // cwd passed through) and the rebuild-per-call path: `new_session` invokes the
    // factory again, which rebuilds cwd-bound services and re-runs create_agent_session.
    #[test]
    fn runtime_builds_offline_and_new_session_rebuilds_through_the_factory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let agent_dir = tmp.path().join(".agent").to_string_lossy().into_owned();

        let mut runtime = create_agent_session_runtime_from_options(
            CreateAgentSessionRuntimeFixedOptions::default(),
            AgentSessionRuntimeFactoryOptions {
                cwd: cwd.clone(),
                agent_dir,
                // In-memory: no disk session file needed for the offline path.
                session_manager: SessionManager::in_memory(&cwd),
                session_start_event: None,
            },
        )
        .expect("initial runtime builds offline");

        // The initial session bound to the passed-through cwd, and — with no providers
        // configured — reports the no-models fallback and clamps thinking to `off`.
        assert_eq!(runtime.cwd(), cwd);
        assert_eq!(
            runtime.model_fallback_message(),
            Some(format_no_models_available_message().as_str())
        );
        assert_eq!(
            runtime.session().agent.thinking_level(),
            ModelThinkingLevel::Off
        );

        // Drive one `/new`: no lifecycle handlers are registered (the offline session
        // uses the stub extension runner), so the switch is not cancelled and the
        // factory is invoked a second time, rebuilding the whole stack cwd-bound.
        let result = runtime.new_session(NewSessionOptions::default());
        assert!(!result.cancelled);

        // The rebuilt session still binds to the same cwd and reports the no-models
        // fallback, proving the factory adaptation runs on replacement too.
        assert_eq!(runtime.cwd(), cwd);
        assert_eq!(
            runtime.model_fallback_message(),
            Some(format_no_models_available_message().as_str())
        );
        assert_eq!(
            runtime.session().agent.thinking_level(),
            ModelThinkingLevel::Off
        );
    }
}
