//! The `AgentSession` struct scaffold, ported from pi's `AgentSession` class
//! (`packages/coding-agent/src/core/agent-session.ts:284-382`, plus the
//! `subscribe`/`_emit` machinery at L527/L788).
//!
//! pi's `AgentSession` wraps [`atilla_agent::agent::Agent`] with the coding-agent
//! session tree, steering/follow-up queues, compaction, auto-retry, and the
//! TUI-facing event channel. This PR lands only the **struct scaffold**: the
//! [`AgentSessionConfig`] options bag, the struct and its fields, the constructor,
//! and the [`AgentSession::subscribe`]/[`AgentSession::emit`] event machinery. The
//! turn-runner methods (`prompt`/`steer`/`follow_up`/`compact`/tree-nav/stats/
//! export) and the runtime/tool-registry wiring land in later PRs.

// straitjacket-allow-file:duplication

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atilla_agent::agent::{Agent, Listener, Subscription};
use atilla_agent::types::{AgentEvent, AgentTool};
use atilla_ai::seams::AbortSignal;
use atilla_ai::{Model, ThinkingLevel};

use crate::core::extensions::events::session::{SessionStartEvent, SessionStartReason};
use crate::core::extensions::runner::{ExtensionRunner, StubExtensionRunner};
use crate::core::extensions::types::ToolDefinition;
use crate::core::model_runtime::ModelRuntime;
use crate::core::resource_loader_orchestrator::DefaultResourceLoader;
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;

use super::events::{AgentSessionEvent, AgentSessionEventListener};

/// A model paired with an optional thinking level, cycled with Ctrl+P (pi's
/// `AgentSessionConfig.scopedModels` element, `agent-session.ts:183`).
#[derive(Debug, Clone)]
pub struct ScopedModel {
    /// The model to switch to.
    pub model: Model,
    /// The thinking level to apply when this model becomes active.
    pub thinking_level: Option<ThinkingLevel>,
}

/// Constructor options for [`AgentSession`] (pi's `AgentSessionConfig`,
/// `agent-session.ts:177`).
///
/// The three canonical services (`agent`, `session_manager`, `settings_manager`)
/// and the runtime collaborators (`resource_loader`, `model_runtime`) are moved
/// into the session. `extension_runner` mirrors pi's mutable
/// `extensionRunnerRef` collapsed to a directly-held handle for the scaffold; it
/// defaults to the always-compiled [`StubExtensionRunner`] when `None` (the
/// runner-seam analog of `StubExtensionLoader`).
pub struct AgentSessionConfig {
    /// The wrapped agent (pi `agent`).
    pub agent: Agent,
    /// Session-tree persistence (pi `sessionManager`).
    pub session_manager: SessionManager,
    /// Settings access (pi `settingsManager`).
    pub settings_manager: SettingsManager,
    /// The working directory (pi `cwd`).
    pub cwd: String,
    /// Models cycled with Ctrl+P, from `--models` (pi `scopedModels`).
    pub scoped_models: Vec<ScopedModel>,
    /// Resource loader for extensions, skills, prompts, themes, and the system
    /// prompt (pi `resourceLoader`).
    pub resource_loader: DefaultResourceLoader,
    /// SDK custom tools registered outside extensions (pi `customTools`).
    pub custom_tools: Vec<ToolDefinition>,
    /// The canonical model/auth runtime (pi `modelRuntime`).
    pub model_runtime: ModelRuntime,
    /// Initial active built-in tool names; default `[read, bash, edit, write]`
    /// is applied by the runtime PR (pi `initialActiveToolNames`).
    pub initial_active_tool_names: Option<Vec<String>>,
    /// Optional allowlist of tool names (pi `allowedToolNames`).
    pub allowed_tool_names: Option<Vec<String>>,
    /// Optional denylist of tool names (pi `excludedToolNames`).
    pub excluded_tool_names: Option<Vec<String>>,
    /// Override base tools for custom runtimes (pi `baseToolsOverride`).
    pub base_tools_override: Option<HashMap<String, AgentTool>>,
    /// The extension runner. `None` defaults to [`StubExtensionRunner`] (pi
    /// `extensionRunnerRef`).
    pub extension_runner: Option<Box<dyn ExtensionRunner>>,
    /// Session-start metadata emitted when extensions bind (pi
    /// `sessionStartEvent`); `None` defaults to `{ reason: "startup" }`.
    pub session_start_event: Option<SessionStartEvent>,
}

/// The coding-agent turn-runner supervisor around [`atilla_agent::agent::Agent`]
/// (pi's `AgentSession`, `agent-session.ts:284`).
///
/// This PR carries the scaffold: the services from [`AgentSessionConfig`], the
/// extension runner, the listener registry and its monotonic id counter, the
/// steering/follow-up queues, and the idle/aborting state flags. Fields consumed
/// only by later PRs' methods are annotated `#[allow(dead_code)]` with a
/// `// unit5:` note.
///
/// The listener registry, queues, and state flags use interior mutability so
/// `subscribe`/`emit` and the lifecycle methods take `&self` (the session is a
/// shared handle the TUI worker thread and the render loop both hold).
pub struct AgentSession {
    /// The wrapped agent (pi `readonly agent`).
    pub agent: Agent,
    /// Session-tree persistence (pi `readonly sessionManager`).
    pub session_manager: SessionManager,
    /// Settings access (pi `readonly settingsManager`).
    pub settings_manager: SettingsManager,

    /// The working directory (pi `_cwd`).
    cwd: String,
    /// The canonical model/auth runtime (pi `_modelRuntime`).
    model_runtime: ModelRuntime,
    /// The extension runner, defaulted to the stub (pi `_extensionRunner`).
    extension_runner: Box<dyn ExtensionRunner>,

    /// Models cycled with Ctrl+P (pi `_scopedModels`).
    scoped_models: Mutex<Vec<ScopedModel>>,

    // unit5: the following configuration is consumed by the runtime/tool-registry
    // PR (`_buildRuntime`/`_refreshToolRegistry`); held here so construction is a
    // faithful move-out of the config.
    /// Resource loader (pi `_resourceLoader`).
    #[allow(dead_code)]
    resource_loader: DefaultResourceLoader,
    /// SDK custom tools (pi `_customTools`).
    #[allow(dead_code)]
    custom_tools: Vec<ToolDefinition>,
    /// Initial active tool names (pi `_initialActiveToolNames`).
    #[allow(dead_code)]
    initial_active_tool_names: Option<Vec<String>>,
    /// Tool allowlist (pi `_allowedToolNames`).
    #[allow(dead_code)]
    allowed_tool_names: Option<HashSet<String>>,
    /// Tool denylist (pi `_excludedToolNames`).
    #[allow(dead_code)]
    excluded_tool_names: Option<HashSet<String>>,
    /// Base-tool overrides (pi `_baseToolsOverride`).
    #[allow(dead_code)]
    base_tools_override: Option<HashMap<String, AgentTool>>,
    /// Session-start metadata emitted on extension bind (pi `_sessionStartEvent`).
    // unit5: consumed by the runtime PR's `bind_extensions`/`session_start` emit.
    #[allow(dead_code)]
    session_start_event: SessionStartEvent,

    /// TUI-facing listeners keyed by a monotonic id (pi `_eventListeners`). Held
    /// in an `Arc<Mutex<..>>` so `subscribe` can hand back a `'static` unsubscribe
    /// closure that removes by id.
    listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>>,
    /// Monotonic subscription-id source for the listener registry.
    next_listener_id: AtomicU64,

    /// The internal agent-event subscription (pi `_unsubscribeAgent`).
    // unit5: read by `_disconnect_from_agent`/`_reconnect_to_agent`/`dispose` in
    // the events PR; the real `_handle_agent_event` handler lands there too.
    #[allow(dead_code)]
    agent_subscription: Mutex<Option<Subscription>>,

    /// Pending steering messages for UI display (pi `_steeringMessages`).
    steering_messages: Mutex<Vec<String>>,
    /// Pending follow-up messages for UI display (pi `_followUpMessages`).
    follow_up_messages: Mutex<Vec<String>>,

    /// Whether an agent run or post-run continuation is active (pi
    /// `_isAgentRunActive`). Drives [`AgentSession::is_idle`].
    is_agent_run_active: AtomicBool,
    /// Whether an abort is in progress.
    // unit5: set/cleared by the turn-runner PR's `abort`/`prompt` flow.
    #[allow(dead_code)]
    is_aborting: AtomicBool,
}

impl AgentSession {
    /// Construct a session from `config` (pi's `constructor`,
    /// `agent-session.ts:356`).
    ///
    /// Moves the services out of `config`, defaults the runner to
    /// [`StubExtensionRunner`] and the session-start event to `startup`, and
    /// initializes empty listeners/queues and idle state. Total and panic-free.
    pub fn new(config: AgentSessionConfig) -> Self {
        let AgentSessionConfig {
            agent,
            session_manager,
            settings_manager,
            cwd,
            scoped_models,
            resource_loader,
            custom_tools,
            model_runtime,
            initial_active_tool_names,
            allowed_tool_names,
            excluded_tool_names,
            base_tools_override,
            extension_runner,
            session_start_event,
        } = config;

        let extension_runner: Box<dyn ExtensionRunner> =
            extension_runner.unwrap_or_else(|| Box::new(StubExtensionRunner));
        let session_start_event = session_start_event.unwrap_or(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        });

        // pi always subscribes an internal `_handleAgentEvent` handler here (for
        // session persistence, extension bridging, and auto-compaction/retry).
        // That handler lands with the turn-runner PR; the scaffold registers a
        // no-op so the subscription field is wired and later PRs can tear it down
        // and swap in the real handler.
        let noop_handler: Listener = Arc::new(|_event: &AgentEvent, _signal: &AbortSignal| {});
        let agent_subscription = agent.subscribe(noop_handler);

        // unit5: pi's constructor also calls `_installAgentToolHooks`,
        // `_installAgentNextTurnRefresh`, and `_buildRuntime`; those land with the
        // runtime and turn-runner PRs.

        Self {
            agent,
            session_manager,
            settings_manager,
            cwd,
            model_runtime,
            extension_runner,
            scoped_models: Mutex::new(scoped_models),
            resource_loader,
            custom_tools,
            initial_active_tool_names,
            allowed_tool_names: allowed_tool_names.map(|names| names.into_iter().collect()),
            excluded_tool_names: excluded_tool_names.map(|names| names.into_iter().collect()),
            base_tools_override,
            session_start_event,
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: AtomicU64::new(0),
            agent_subscription: Mutex::new(Some(agent_subscription)),
            steering_messages: Mutex::new(Vec::new()),
            follow_up_messages: Mutex::new(Vec::new()),
            is_agent_run_active: AtomicBool::new(false),
            is_aborting: AtomicBool::new(false),
        }
    }

    // =========================================================================
    // Event subscription (pi `subscribe`/`_emit`, agent-session.ts:788/527)
    // =========================================================================

    /// Subscribe to session events; returns an unsubscribe closure that removes
    /// this listener by id (pi's `subscribe`, `agent-session.ts:788`).
    ///
    /// Listeners fire synchronously in registration order on every
    /// [`AgentSession::emit`]. The returned [`FnOnce`] is `'static` (it captures a
    /// clone of the shared registry plus the id), so callers may hold it beyond
    /// the borrow of `&self`.
    pub fn subscribe(&self, listener: AgentSessionEventListener) -> impl FnOnce() {
        let id = self.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().unwrap().push((id, listener));

        let listeners = Arc::clone(&self.listeners);
        move || {
            listeners
                .lock()
                .unwrap()
                .retain(|(existing, _)| *existing != id);
        }
    }

    /// Fan an event out to every listener synchronously in registration order
    /// (pi's `_emit`, `agent-session.ts:527`).
    ///
    /// The registry snapshot is taken (cloning the cheap `Arc` handles) before the
    /// lock is released, so a listener that re-enters `subscribe`/unsubscribe
    /// cannot deadlock or observe a half-mutated registry.
    // unit5: the turn-runner, compaction, and queue PRs call this to publish
    // their session events; the scaffold exercises it only from tests.
    #[allow(dead_code)]
    fn emit(&self, event: &AgentSessionEvent) {
        let snapshot: Vec<AgentSessionEventListener> = {
            let listeners = self.listeners.lock().unwrap();
            listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        for listener in &snapshot {
            listener(event);
        }
    }

    // =========================================================================
    // Minimal lifecycle + read-only accessors
    // =========================================================================

    /// Abort the current operation by delegating to the agent (a subset of pi's
    /// `abort`, `agent-session.ts:1530`; retry/compaction/bash aborts and
    /// idle-wait land with their respective PRs).
    pub fn abort(&self) {
        self.agent.abort();
    }

    /// The working directory (pi holds this as `_cwd`).
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Whether the session has no active agent run (pi's `get isIdle`,
    /// `agent-session.ts:869`).
    pub fn is_idle(&self) -> bool {
        !self.is_agent_run_active.load(Ordering::Relaxed)
    }

    /// The canonical model/auth runtime (pi's `get modelRuntime`,
    /// `agent-session.ts:384`).
    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.model_runtime
    }

    /// The bound extension runner (pi's `get extensionRunner`).
    pub fn extension_runner(&self) -> &dyn ExtensionRunner {
        &*self.extension_runner
    }

    /// The scoped models for cycling (pi's `get scopedModels`,
    /// `agent-session.ts:971`).
    pub fn scoped_models(&self) -> Vec<ScopedModel> {
        self.scoped_models.lock().unwrap().clone()
    }

    /// Pending steering messages for UI display (pi's `getSteeringMessages`,
    /// `agent-session.ts:1514`).
    pub fn get_steering_messages(&self) -> Vec<String> {
        self.steering_messages.lock().unwrap().clone()
    }

    /// Pending follow-up messages for UI display (pi's `getFollowUpMessages`,
    /// `agent-session.ts:1519`).
    pub fn get_follow_up_messages(&self) -> Vec<String> {
        self.follow_up_messages.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelsPath};
    use crate::core::resource_loader_orchestrator::DefaultResourceLoaderOptions;
    use atilla_agent::agent::AgentOptions;
    use std::sync::atomic::AtomicUsize;

    /// Build a minimal, fully in-memory session for the scaffold tests: a default
    /// agent (provider-unavailable stub stream fn), an in-memory session manager,
    /// a tmp-rooted settings manager, an offline model runtime (no `models.json`),
    /// and the default [`StubExtensionRunner`]. Mirrors pi's test harness adapted
    /// to the Rust in-memory fixtures.
    fn build_session() -> AgentSession {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().to_string();
        let agent_dir = tmp.path().join(".agent").to_string_lossy().to_string();

        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            models_path: ModelsPath::Disabled,
            ..Default::default()
        });
        let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            ..Default::default()
        });

        AgentSession::new(AgentSessionConfig {
            agent: Agent::new(AgentOptions::default()),
            session_manager: SessionManager::in_memory(&cwd),
            settings_manager: SettingsManager::create(&cwd, &agent_dir),
            cwd,
            scoped_models: Vec::new(),
            resource_loader,
            custom_tools: Vec::new(),
            model_runtime,
            initial_active_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            extension_runner: None,
            session_start_event: None,
        })
    }

    #[test]
    fn construction_succeeds_and_starts_idle() {
        let session = build_session();
        assert!(session.is_idle());
        assert!(session.get_steering_messages().is_empty());
        assert!(session.get_follow_up_messages().is_empty());
        // The default runner is the always-compiled stub (no handlers).
        assert!(!session.extension_runner().has_handlers("input"));
    }

    #[test]
    fn subscribe_then_emit_fans_out_to_listener() {
        let session = build_session();
        let seen = Arc::new(AtomicUsize::new(0));

        let seen_clone = Arc::clone(&seen);
        let _unsubscribe = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
            if matches!(event, AgentSessionEvent::AgentSettled) {
                seen_clone.fetch_add(1, Ordering::Relaxed);
            }
        }));

        session.emit(&AgentSessionEvent::AgentSettled);
        assert_eq!(seen.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unsubscribe_removes_the_listener() {
        let session = build_session();
        let seen = Arc::new(AtomicUsize::new(0));

        let seen_clone = Arc::clone(&seen);
        let unsubscribe = session.subscribe(Arc::new(move |_event: &AgentSessionEvent| {
            seen_clone.fetch_add(1, Ordering::Relaxed);
        }));

        session.emit(&AgentSessionEvent::AgentSettled);
        assert_eq!(seen.load(Ordering::Relaxed), 1);

        // After unsubscribing, a subsequent emit does not reach the listener.
        unsubscribe();
        session.emit(&AgentSessionEvent::AgentSettled);
        assert_eq!(seen.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn two_listeners_both_receive_in_registration_order() {
        let session = build_session();
        let order = Arc::new(Mutex::new(Vec::<u8>::new()));

        let order_first = Arc::clone(&order);
        let _first = session.subscribe(Arc::new(move |_event: &AgentSessionEvent| {
            order_first.lock().unwrap().push(1);
        }));
        let order_second = Arc::clone(&order);
        let _second = session.subscribe(Arc::new(move |_event: &AgentSessionEvent| {
            order_second.lock().unwrap().push(2);
        }));

        session.emit(&AgentSessionEvent::AgentSettled);
        assert_eq!(*order.lock().unwrap(), vec![1, 2]);
    }
}
