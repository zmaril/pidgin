//! The `AgentSession` struct scaffold, ported from pi's `AgentSession` class
//! (`packages/coding-agent/src/core/agent-session.ts:284-382`, plus the
//! `subscribe`/`_emit` machinery at L527/L788).
//!
//! pi's `AgentSession` wraps [`pidgin_agent::agent::Agent`] with the coding-agent
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

use tokio::sync::watch;

use pidgin_agent::agent::{Agent, Subscription};
use pidgin_agent::types::{AgentMessage, AgentTool};
use pidgin_ai::seams::AbortSignal;
use pidgin_ai::{Model, ThinkingLevel};

use crate::core::compaction::Models;
use crate::core::extensions::events::session::{SessionStartEvent, SessionStartReason};
use crate::core::extensions::runner::{ExtensionRunner, StubExtensionRunner};
use crate::core::extensions::types::ToolDefinition;
use crate::core::model_runtime::ModelRuntime;
use crate::core::resource_loader_orchestrator::DefaultResourceLoader;
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::{RetryResolved, SettingsManager};

use super::events::{AgentSessionEvent, AgentSessionEventListener};
use super::turn::{build_agent_listener, emit_to_listeners, AgentEventHandler};

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
    /// The provider seam compaction summarizes through (pi passes
    /// `this.agent.streamFn` to `compact`; the ported `core::compaction::compact`
    /// takes a [`Models`] instead — see [`super::compaction_turn`]).
    ///
    /// `Some` is the analog of pi's "custom `streamFn`" (a `streamFn` other than
    /// `streamSimple`): compaction summarization runs through it and the
    /// configured-auth gate is bypassed. `None` is the analog of the default
    /// `streamSimple`: the configured-auth gate applies, and the runtime-driven
    /// summarization it implies is part of the deferred credential-aware
    /// `ModelRuntime` streaming surface (never reached by the offline suites,
    /// which either supply a summarizer or have an extension provide the
    /// compaction).
    pub summarization_models: Option<Box<dyn Models>>,
}

/// The coding-agent turn-runner supervisor around [`pidgin_agent::agent::Agent`]
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
    /// Session-tree persistence (pi `readonly sessionManager`). Held in an
    /// `Arc<Mutex<..>>` so the internal `agent.subscribe` handler (a `'static`
    /// closure) can append finalized messages during a run. `pub(super)` so the
    /// extension host bridge in [`super::host`] can share the same handle.
    pub(super) session_manager: Arc<Mutex<SessionManager>>,
    /// Settings access (pi `readonly settingsManager`).
    pub settings_manager: SettingsManager,

    /// The working directory (pi `_cwd`).
    cwd: String,
    /// The canonical model/auth runtime (pi `_modelRuntime`).
    model_runtime: ModelRuntime,
    /// The extension runner, defaulted to the stub (pi `_extensionRunner`). An
    /// `Arc` so the internal agent-event handler can emit through it.
    extension_runner: Arc<dyn ExtensionRunner>,
    /// Whether the project is trusted, snapshotted from the settings manager at
    /// construction (pi `settingsManager.isProjectTrusted()`). The
    /// `SettingsManager` is `!Send`, so the `Send + Sync` extension host bridge in
    /// [`super::host`] cannot hold it; this shared snapshot lets the bridge answer
    /// `isProjectTrusted` without crossing the `!Send` boundary.
    pub(super) project_trusted: Arc<Mutex<bool>>,

    /// Models cycled with Ctrl+P (pi `_scopedModels`).
    scoped_models: Mutex<Vec<ScopedModel>>,

    // unit5: the following configuration is consumed by the runtime/tool-registry
    // PR (`_buildRuntime`/`_refreshToolRegistry`); held here so construction is a
    // faithful move-out of the config.
    /// Resource loader (pi `_resourceLoader`). Read by the extension-turn skill /
    /// prompt-template expansion in [`super::extension_turn`].
    pub(super) resource_loader: DefaultResourceLoader,
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
    /// closure that removes by id. `pub(super)` so the extension host bridge in
    /// [`super::host`] can emit `entry_appended` through the same registry.
    pub(super) listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>>,
    /// Monotonic subscription-id source for the listener registry.
    next_listener_id: AtomicU64,

    /// The internal agent-event subscription (pi `_unsubscribeAgent`).
    // unit5: read by `_disconnect_from_agent`/`_reconnect_to_agent`/`dispose` in
    // the events PR; the real `_handle_agent_event` handler lands there too.
    #[allow(dead_code)]
    agent_subscription: Mutex<Option<Subscription>>,

    /// Pending steering messages for UI display (pi `_steeringMessages`). Shared
    /// with the agent-event handler, which splices a mirrored entry out on the
    /// matching user `message_start`. `pub(super)` so the queue mutators in
    /// [`super::queue`] can push to it.
    pub(super) steering_messages: Arc<Mutex<Vec<String>>>,
    /// Pending follow-up messages for UI display (pi `_followUpMessages`).
    pub(super) follow_up_messages: Arc<Mutex<Vec<String>>>,

    /// Custom messages queued for the next prompt turn (pi
    /// `_pendingNextTurnMessages`). Drained into the user message batch by
    /// [`AgentSession::prompt`]; pushed by `send_custom_message(deliverAs:
    /// nextTurn)`.
    pub(super) pending_next_turn_messages: Arc<Mutex<Vec<AgentMessage>>>,

    /// The last assistant message, set by the agent-event handler and consumed by
    /// the post-run loop (pi `_lastAssistantMessage`).
    pub(super) last_assistant_message: Arc<Mutex<Option<AgentMessage>>>,

    /// The provider seam compaction summarizes through (pi passes
    /// `this.agent.streamFn`); see [`AgentSessionConfig::summarization_models`].
    /// `Some` bypasses the configured-auth gate (pi's custom-`streamFn` branch).
    pub(super) summarization_models: Option<Box<dyn Models>>,
    /// Guards context-overflow recovery to a single compact-and-retry attempt (pi
    /// `_overflowRecoveryAttempted`). Shared with the agent-event handler, which
    /// clears it on a user `message_start` and on a non-error assistant
    /// `message_end`; read and set by `check_compaction`.
    pub(super) overflow_recovery_attempted: Arc<Mutex<bool>>,
    /// The abort signal for an in-progress manual `compact()` (pi
    /// `_compactionAbortController`); `Some` only while `compact` runs. Tripped by
    /// [`AgentSession::abort_compaction`].
    pub(super) compaction_abort_signal: Arc<Mutex<Option<AbortSignal>>>,
    /// The abort signal for an in-progress auto-compaction (pi
    /// `_autoCompactionAbortController`); `Some` only while `run_auto_compaction`
    /// runs. Tripped by [`AgentSession::abort_compaction`].
    pub(super) auto_compaction_abort_signal: Arc<Mutex<Option<AbortSignal>>>,

    /// The current auto-retry attempt count (pi `_retryAttempt`). Shared with the
    /// agent-event handler, which resets it on a successful assistant response and
    /// reads it to compute `will_retry` for `agent_end`.
    pub(super) retry_attempt: Arc<Mutex<u32>>,
    /// The abort signal for the in-progress retry backoff (pi
    /// `_retryAbortController`); `Some` only while `_prepare_retry` is sleeping.
    /// [`AgentSession::is_retrying`] reads it and [`AgentSession::abort_retry`]
    /// trips it. Held behind a shared handle so the actor-pattern owner can trip
    /// the backoff from the drive thread.
    pub(super) retry_abort_signal: Arc<Mutex<Option<AbortSignal>>>,
    /// A snapshot of the resolved retry settings (pi's `getRetrySettings`). Shared
    /// with the agent-event handler so its `will_retry` computation reads the same
    /// `enabled`/`maxRetries` the drive thread does; the `SettingsManager` itself
    /// is `!Send` and cannot cross into the `Send + Sync` handler closure. Kept in
    /// sync by [`AgentSession::set_auto_retry_enabled`].
    pub(super) retry_settings: Arc<Mutex<RetryResolved>>,

    /// Whether an agent run or post-run continuation is active (pi
    /// `_isAgentRunActive`). Drives [`AgentSession::is_idle`]. An `Arc` so the
    /// `Send + Sync` extension host bridge in [`super::host`] can answer `isIdle`
    /// against the same flag.
    pub(super) is_agent_run_active: Arc<AtomicBool>,
    /// Whether an abort is in progress.
    // unit5: set/cleared by the turn-runner PR's `abort`/`prompt` flow.
    #[allow(dead_code)]
    is_aborting: AtomicBool,

    /// The base (tool-derived) system prompt (pi `_baseSystemPrompt`). Captured at
    /// construction from the agent's initial system prompt; an extension
    /// `before_agent_start` override layers on top of it and resets back to it on
    /// the next turn. Shared with the host bridge's `getSystemPrompt`.
    pub(super) base_system_prompt: Arc<Mutex<String>>,
    /// The active extension system-prompt override (pi `_systemPromptOverride`);
    /// `Some` only for the duration of a turn whose `before_agent_start` handler
    /// supplied one. Cleared in `run_agent_prompt`'s finally block.
    pub(super) system_prompt_override: Arc<Mutex<Option<String>>>,

    /// Bash execution results deferred while a run is streaming (pi
    /// `_pendingBashMessages`). `record_bash_result` pushes here when
    /// [`AgentSession::is_streaming`] is true (to preserve tool_use/tool_result
    /// ordering); [`AgentSession::flush_pending_bash_messages`] drains them into
    /// agent state + the session before the next prompt and after each run. Owned
    /// solely by the turn thread, so a plain `Mutex` (no cross-thread handler
    /// access) suffices. See [`super::bash`].
    pub(super) pending_bash_messages: Mutex<Vec<AgentMessage>>,
    /// The abort handle for the in-progress bash command (pi
    /// `_bashAbortController`); `Some` only while [`AgentSession::execute_bash`]
    /// runs. [`AgentSession::abort_bash`] trips it and
    /// [`AgentSession::is_bash_running`] reads its presence. A
    /// [`watch::Sender<bool>`] bridges the session's abort into the
    /// [`BashOperations`](crate::core::tools::bash::BashOperations) `exec` signal.
    pub(super) bash_abort: Mutex<Option<watch::Sender<bool>>>,
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
            summarization_models,
        } = config;

        let extension_runner: Arc<dyn ExtensionRunner> = match extension_runner {
            Some(runner) => Arc::from(runner),
            None => Arc::new(StubExtensionRunner),
        };
        let session_start_event = session_start_event.unwrap_or(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        });

        let session_manager = Arc::new(Mutex::new(session_manager));
        let listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let steering_messages = Arc::new(Mutex::new(Vec::new()));
        let follow_up_messages = Arc::new(Mutex::new(Vec::new()));
        let pending_next_turn_messages = Arc::new(Mutex::new(Vec::new()));
        let last_assistant_message = Arc::new(Mutex::new(None));
        // The extension-facing turn index (pi `_turnIndex`) is owned solely by the
        // agent-event handler, so it is created here and moved into the handler.
        let turn_index = Arc::new(Mutex::new(0i64));
        // Auto-retry state (pi `_retryAttempt`/`_retryAbortController`). The
        // attempt counter and a settings snapshot are shared with the handler so it
        // can reset on success and compute `will_retry`; the abort signal lives on
        // the session only (the backoff sleeps on the drive thread).
        let retry_attempt = Arc::new(Mutex::new(0u32));
        let retry_abort_signal = Arc::new(Mutex::new(None));
        let retry_settings = Arc::new(Mutex::new(settings_manager.get_retry_settings()));
        // The one-shot overflow-recovery guard (pi `_overflowRecoveryAttempted`) is
        // shared with the handler so it can clear it on user `message_start` and on
        // a successful assistant `message_end`.
        let overflow_recovery_attempted = Arc::new(Mutex::new(false));
        // A cheap handle to the same shared agent state so the handler can read the
        // live model (pi `this.model`) when deciding `will_retry`.
        let handler_agent = agent.clone();

        // Capture the base system prompt (pi `_baseSystemPrompt`) from the agent's
        // initial system prompt. Extension `before_agent_start` overrides layer on
        // top of this and reset back to it (see [`super::extension_turn`]).
        let base_system_prompt = Arc::new(Mutex::new(agent.system_prompt()));
        let system_prompt_override = Arc::new(Mutex::new(None));
        let is_agent_run_active = Arc::new(AtomicBool::new(false));
        // Snapshot project trust (pi `settingsManager.isProjectTrusted()`) for the
        // `Send + Sync` host bridge, which cannot hold the `!Send` settings manager.
        let project_trusted = Arc::new(Mutex::new(settings_manager.is_project_trusted()));

        // pi always subscribes an internal `_handleAgentEvent` handler here for
        // session persistence, extension bridging, and (later) auto-retry /
        // compaction. Wire it with `Arc` clones of the shared turn state so the
        // `'static` listener closure can reach it.
        let handler = AgentEventHandler {
            session_manager: Arc::clone(&session_manager),
            extension_runner: Arc::clone(&extension_runner),
            listeners: Arc::clone(&listeners),
            steering_messages: Arc::clone(&steering_messages),
            follow_up_messages: Arc::clone(&follow_up_messages),
            last_assistant_message: Arc::clone(&last_assistant_message),
            turn_index,
            agent: handler_agent,
            retry_attempt: Arc::clone(&retry_attempt),
            retry_settings: Arc::clone(&retry_settings),
            overflow_recovery_attempted: Arc::clone(&overflow_recovery_attempted),
        };
        let agent_subscription = agent.subscribe(build_agent_listener(handler));

        // unit5: pi's constructor also calls `_installAgentToolHooks`,
        // `_installAgentNextTurnRefresh`, and `_buildRuntime`; those land with the
        // runtime PR.

        Self {
            agent,
            session_manager,
            settings_manager,
            cwd,
            model_runtime,
            extension_runner,
            project_trusted,
            scoped_models: Mutex::new(scoped_models),
            resource_loader,
            custom_tools,
            initial_active_tool_names,
            allowed_tool_names: allowed_tool_names.map(|names| names.into_iter().collect()),
            excluded_tool_names: excluded_tool_names.map(|names| names.into_iter().collect()),
            base_tools_override,
            session_start_event,
            listeners,
            next_listener_id: AtomicU64::new(0),
            agent_subscription: Mutex::new(Some(agent_subscription)),
            steering_messages,
            follow_up_messages,
            pending_next_turn_messages,
            last_assistant_message,
            retry_attempt,
            retry_abort_signal,
            retry_settings,
            summarization_models,
            overflow_recovery_attempted,
            compaction_abort_signal: Arc::new(Mutex::new(None)),
            auto_compaction_abort_signal: Arc::new(Mutex::new(None)),
            is_agent_run_active,
            is_aborting: AtomicBool::new(false),
            base_system_prompt,
            system_prompt_override,
            pending_bash_messages: Mutex::new(Vec::new()),
            bash_abort: Mutex::new(None),
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
    pub(super) fn emit(&self, event: &AgentSessionEvent) {
        emit_to_listeners(&self.listeners, event);
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

    /// Set the active-run flag (pi assigns `_isAgentRunActive` in
    /// `_runAgentPrompt`/`_emitAgentSettled`). Used by the turn-runner methods.
    pub(super) fn set_agent_run_active(&self, active: bool) {
        self.is_agent_run_active.store(active, Ordering::Relaxed);
    }

    /// Session-tree persistence access (pi's `readonly sessionManager`). Returns a
    /// lock guard; the manager is shared with the internal agent-event handler.
    pub fn session_manager(&self) -> std::sync::MutexGuard<'_, SessionManager> {
        self.session_manager.lock().unwrap()
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
    use pidgin_agent::agent::AgentOptions;
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
            summarization_models: None,
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
