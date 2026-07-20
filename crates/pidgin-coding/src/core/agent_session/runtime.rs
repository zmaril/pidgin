//! The `AgentSessionRuntime` session-lifecycle orchestrator, ported from pi's
//! `packages/coding-agent/src/core/agent-session-runtime.ts`.
//!
//! pi's `AgentSessionRuntime` owns **the** current [`AgentSession`] and swaps it
//! out on `/new`, `/resume`, and `/fork`. Each replacement runs the same
//! lifecycle handshake through the current session's extension runner:
//!
//! 1. **`session_before_switch`** (for `/new` and `/resume`) or
//!    **`session_before_fork`** (for `/fork`) — a handler may cancel the switch,
//!    in which case the current session is left untouched.
//! 2. **`session_shutdown`** — fired on the outgoing session's runner, then the
//!    optional `before_session_invalidate` host hook runs and the outgoing session
//!    is [`disposed`](AgentSession::dispose).
//! 3. The injected session factory builds the replacement session (carrying a
//!    `session_start` metadata event), which [`apply`](AgentSessionRuntime) makes
//!    current.
//! 4. **`session_start`** — emitted by the new session's
//!    [`bind_extensions`](AgentSession::bind_extensions) (called by the host after
//!    the replacement completes), and the optional `rebind_session` host hook runs.
//!
//! # Ownership and `!Send`
//!
//! The runtime **owns** the current [`AgentSession`], which is intentionally
//! `!Send`/`!Sync` (see the [`agent_session` module docs](super)). The runtime is
//! therefore itself `!Send`, and that is faithful: it lives on the same owned
//! session thread and is driven through the session-actor command channel like the
//! session it wraps. Do **not** try to make it `Send`. The injected session factory
//! ([`CreateAgentSessionRuntimeFactory`]) produces `!Send` sessions, so it too is a
//! non-`Send` boxed closure held on the owned thread. The lifecycle events flow
//! through the session's extension runner (whose own listeners follow the same
//! subscribe/emit registry contract as [`AgentSessionEvent`](super::AgentSessionEvent)
//! listeners); a cross-thread consumer receives them via the runner's own bridge.
//!
//! # Faithful-adaptation deferrals
//!
//! pi's runtime carries several fields/paths this offline slice does not yet reach;
//! each is a documented seam, not an invented stub:
//!
//! - **`services` / `diagnostics`** (pi `AgentSessionServices` /
//!   `AgentSessionRuntimeDiagnostic`, `agent-session-services.ts`) are net-new and
//!   unported. The runtime keeps only what the lifecycle needs: the resolved `cwd`
//!   and the `model_fallback_message` (both carried on
//!   [`AgentSessionRuntimeResult`]).
//! - **`withSession` / `createReplacedSessionContext`** (pi `ReplacedSessionContext`)
//!   is unported — the replaced-session extension context type does not exist yet —
//!   so the option is omitted from the replacement methods. `rebind_session` (which
//!   only needs the new session) is ported.
//! - **`cwdOverride` / `projectTrustContextFactory`** on `switchSession` are
//!   project-trust/sdk-driven inputs that are out of this lane (see the port
//!   design), so `switch_session` takes only the target path.
//! - **`importFromJsonl`** is a distinct `/import` surface with its own suite; it is
//!   deferred to that slice.
//! - The extension-context staleness that pi's `dispose` enforces via
//!   `extensionRunner.invalidate(...)` is wired ([`AgentSession::dispose`] calls
//!   `invalidate`), but the runner seam exposes no `createContext`, so the
//!   "captured ctx throws after replacement" assertion is not reproduced offline.

// straitjacket-allow-file:duplication

use std::error::Error;
use std::fmt;
use std::path::Path;

use serde_json::Value;

use crate::core::extensions::events::session::{
    ForkPosition, SessionBeforeForkEvent, SessionBeforeSwitchEvent, SessionBeforeSwitchReason,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason,
};
use crate::core::extensions::runner::{ExtensionDispatchEvent, ExtensionEmitOutcome};
use crate::core::session_cwd::{assert_session_cwd_exists, MissingSessionCwdError};
use crate::core::session_manager::{
    NewSessionOptions as SmNewSessionOptions, SessionEntry, SessionManager,
};

use super::session::AgentSession;

// ---------------------------------------------------------------------------
// Factory seam
// ---------------------------------------------------------------------------

/// The inputs the runtime hands its session factory (pi's
/// `CreateAgentSessionRuntimeFactory` options, `agent-session-runtime.ts:35`).
///
/// Also the constructor options for [`create_agent_session_runtime`]: pi reuses the
/// same shape for the initial build and each subsequent replacement (minus the
/// deferred `projectTrustContext`).
pub struct AgentSessionRuntimeFactoryOptions {
    /// The working directory for the session (pi `cwd`).
    pub cwd: String,
    /// The global config directory (pi `agentDir`).
    pub agent_dir: String,
    /// The prepared session manager the factory wires the session around (pi
    /// `sessionManager`). The runtime builds/opens/branches it before the call.
    pub session_manager: SessionManager,
    /// The `session_start` metadata the built session emits on
    /// [`bind_extensions`](AgentSession::bind_extensions) (pi `sessionStartEvent`).
    pub session_start_event: Option<SessionStartEvent>,
}

/// What the session factory returns (pi's `CreateAgentSessionRuntimeResult`,
/// `agent-session-runtime.ts:23`, trimmed to the ported surface).
pub struct AgentSessionRuntimeResult {
    /// The freshly built session (pi `session`).
    pub session: AgentSession,
    /// The resolved working directory the session bound to (pi `services.cwd`).
    pub cwd: String,
    /// A warning if the session was restored with a different model than saved (pi
    /// `modelFallbackMessage`).
    pub model_fallback_message: Option<String>,
}

/// The session factory the runtime owns and reuses for every `/new`, `/resume`,
/// and `/fork` (pi's `CreateAgentSessionRuntimeFactory`).
///
/// It produces `!Send` [`AgentSession`]s, so it is a non-`Send` boxed closure held
/// on the owned session thread (see the [module docs](self)). The concrete factory
/// is supplied by the caller (the `sdk.ts` lane in production; an in-memory faux
/// factory in tests).
pub type CreateAgentSessionRuntimeFactory =
    Box<dyn FnMut(AgentSessionRuntimeFactoryOptions) -> AgentSessionRuntimeResult>;

/// A `rebind_session` host hook, run after a replacement completes with the new
/// session (pi's `rebindSession`, `agent-session-runtime.ts:75`).
pub type RebindSession = Box<dyn FnMut(&AgentSession)>;

/// A `before_session_invalidate` host hook, run after `session_shutdown` handlers
/// finish but before the outgoing session is disposed (pi's
/// `beforeSessionInvalidate`, `agent-session-runtime.ts:76`).
pub type BeforeSessionInvalidate = Box<dyn FnMut()>;

// ---------------------------------------------------------------------------
// Method options / results
// ---------------------------------------------------------------------------

/// Options for [`AgentSessionRuntime::new_session`] (pi's `newSession` options,
/// `agent-session-runtime.ts:223`). `with_session` is deferred (see module docs).
#[derive(Default)]
pub struct NewSessionOptions {
    /// A parent session file recorded on the new session's header (pi
    /// `parentSession`).
    pub parent_session: Option<String>,
    /// A synchronous setup pass over the new session manager before its context is
    /// restored into the agent (pi `setup`).
    #[allow(clippy::type_complexity)]
    pub setup: Option<Box<dyn FnOnce(&mut SessionManager)>>,
}

/// Options for [`AgentSessionRuntime::fork`] (pi's `fork` options,
/// `agent-session-runtime.ts:261`). `with_session` is deferred (see module docs).
#[derive(Default)]
pub struct ForkOptions {
    /// Whether to fork `before` (default) or `at` the anchor entry (pi `position`).
    pub position: Option<ForkPosition>,
}

/// The result of a `/new` or `/resume` replacement (pi's `{ cancelled: boolean }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchResult {
    /// Whether a `session_before_switch` handler cancelled the replacement.
    pub cancelled: bool,
}

/// The result of a `/fork` (pi's `{ cancelled: boolean; selectedText?: string }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkResult {
    /// Whether a `session_before_fork` handler cancelled the fork.
    pub cancelled: bool,
    /// The forked-from user message text, for a `before`-position fork (pi
    /// `selectedText`).
    pub selected_text: Option<String>,
}

/// Errors raised by the runtime's replacement methods (pi throws `Error` with
/// these messages).
#[derive(Debug)]
pub enum AgentSessionRuntimeError {
    /// The target session's stored cwd no longer exists (pi's `MissingSessionCwdError`).
    MissingSessionCwd(MissingSessionCwdError),
    /// [`SessionManager::open`] failed to load the target session file.
    OpenSession(String),
    /// pi's "Invalid entry ID for forking".
    InvalidForkEntry,
    /// pi's "This session has not been saved yet. ...".
    SessionNotSaved,
    /// pi's "Persisted session is missing a session file".
    MissingSessionFile,
    /// pi's "Failed to create forked session".
    ForkFailed,
    /// An underlying [`SessionManager::create_branched_session`] failure.
    Branch(String),
}

impl fmt::Display for AgentSessionRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSessionCwd(err) => write!(f, "{err}"),
            Self::OpenSession(message) => write!(f, "{message}"),
            Self::InvalidForkEntry => write!(f, "Invalid entry ID for forking"),
            Self::SessionNotSaved => write!(
                f,
                "This session has not been saved yet. Wait for the first assistant response before cloning or forking it."
            ),
            Self::MissingSessionFile => write!(f, "Persisted session is missing a session file"),
            Self::ForkFailed => write!(f, "Failed to create forked session"),
            Self::Branch(message) => write!(f, "{message}"),
        }
    }
}

impl Error for AgentSessionRuntimeError {}

// ---------------------------------------------------------------------------
// AgentSessionRuntime
// ---------------------------------------------------------------------------

/// Owns the current [`AgentSession`] and swaps it on `/new`, `/resume`, and
/// `/fork` (pi's `AgentSessionRuntime`, `agent-session-runtime.ts:74`).
pub struct AgentSessionRuntime {
    /// The current session (pi `_session`). Replaced by [`apply`](Self::apply).
    session: AgentSession,
    /// The resolved working directory (pi `_services.cwd`).
    services_cwd: String,
    /// The global config directory threaded into each factory call (pi
    /// `_services.agentDir`).
    agent_dir: String,
    /// The session factory reused for every replacement (pi `createRuntime`).
    create_runtime: CreateAgentSessionRuntimeFactory,
    /// A warning when the current session restored a different model than saved (pi
    /// `_modelFallbackMessage`).
    model_fallback_message: Option<String>,
    /// The `rebind_session` host hook (pi `rebindSession`).
    rebind_session: Option<RebindSession>,
    /// The `before_session_invalidate` host hook (pi `beforeSessionInvalidate`).
    before_session_invalidate: Option<BeforeSessionInvalidate>,
}

impl AgentSessionRuntime {
    /// The current session (pi's `get session`, `agent-session-runtime.ts:101`).
    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    /// The working directory (pi's `get cwd`, `agent-session-runtime.ts:105`).
    pub fn cwd(&self) -> &str {
        &self.services_cwd
    }

    /// The model-fallback warning, if any (pi's `get modelFallbackMessage`,
    /// `agent-session-runtime.ts:113`).
    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    /// Set (or clear) the `rebind_session` host hook (pi's `setRebindSession`,
    /// `agent-session-runtime.ts:117`).
    pub fn set_rebind_session(&mut self, rebind_session: Option<RebindSession>) {
        self.rebind_session = rebind_session;
    }

    /// Set (or clear) the `before_session_invalidate` host hook (pi's
    /// `setBeforeSessionInvalidate`, `agent-session-runtime.ts:129`).
    pub fn set_before_session_invalidate(
        &mut self,
        before_session_invalidate: Option<BeforeSessionInvalidate>,
    ) {
        self.before_session_invalidate = before_session_invalidate;
    }

    /// Start a brand-new session in the current cwd (pi's `newSession`,
    /// `agent-session-runtime.ts:223`).
    ///
    /// A `session_before_switch` handler may cancel it, in which case the current
    /// session is untouched and `cancelled` is `true`.
    pub fn new_session(&mut self, options: NewSessionOptions) -> SwitchResult {
        if self.emit_before_switch(SessionBeforeSwitchReason::New, None) {
            return SwitchResult { cancelled: true };
        }

        let previous_session_file = self.session.session_file();
        let (session_manager, target_file) = {
            let (session_dir, persisted) = {
                let manager = self.session.session_manager();
                (
                    manager.get_session_dir().to_string(),
                    manager.is_persisted(),
                )
            };
            let mut manager = if persisted {
                SessionManager::create(&self.services_cwd, Some(&session_dir), None)
            } else {
                SessionManager::in_memory(&self.services_cwd)
            };
            if let Some(parent_session) = options.parent_session {
                let _ = manager.new_session(SmNewSessionOptions {
                    parent_session: Some(parent_session),
                    ..SmNewSessionOptions::default()
                });
            }
            let target = manager.get_session_file().map(String::from);
            (manager, target)
        };

        self.teardown_current(SessionShutdownReason::New, target_file);
        let result = (self.create_runtime)(AgentSessionRuntimeFactoryOptions {
            cwd: self.services_cwd.clone(),
            agent_dir: self.agent_dir.clone(),
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::New,
                previous_session_file,
            }),
        });
        self.apply(result);

        if let Some(setup) = options.setup {
            {
                let mut manager = self.session.session_manager();
                setup(&mut manager);
            }
            let messages = self
                .session
                .session_manager()
                .build_session_context()
                .messages;
            self.session.agent.set_messages(messages);
        }

        self.finish_session_replacement();
        SwitchResult { cancelled: false }
    }

    /// Switch to (resume) a persisted session file (pi's `switchSession`,
    /// `agent-session-runtime.ts:193`).
    pub fn switch_session(
        &mut self,
        session_path: &str,
    ) -> Result<SwitchResult, AgentSessionRuntimeError> {
        if self.emit_before_switch(
            SessionBeforeSwitchReason::Resume,
            Some(session_path.to_string()),
        ) {
            return Ok(SwitchResult { cancelled: true });
        }

        let previous_session_file = self.session.session_file();
        // pi: SessionManager.open(sessionPath, undefined, options?.cwdOverride) —
        // cwdOverride is deferred (see module docs).
        let session_manager =
            SessionManager::open(session_path).map_err(AgentSessionRuntimeError::OpenSession)?;
        assert_session_cwd_exists(&session_manager, &self.services_cwd)
            .map_err(AgentSessionRuntimeError::MissingSessionCwd)?;

        let target_file = session_manager.get_session_file().map(String::from);
        let cwd = session_manager.get_cwd().to_string();
        self.teardown_current(SessionShutdownReason::Resume, target_file);
        let result = (self.create_runtime)(AgentSessionRuntimeFactoryOptions {
            cwd,
            agent_dir: self.agent_dir.clone(),
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::Resume,
                previous_session_file,
            }),
        });
        self.apply(result);
        self.finish_session_replacement();
        Ok(SwitchResult { cancelled: false })
    }

    /// Fork the session at (or before) an entry (pi's `fork`,
    /// `agent-session-runtime.ts:259`).
    pub fn fork(
        &mut self,
        entry_id: &str,
        options: ForkOptions,
    ) -> Result<ForkResult, AgentSessionRuntimeError> {
        let position = options.position.unwrap_or(ForkPosition::Before);
        if self.emit_before_fork(entry_id, position) {
            return Ok(ForkResult {
                cancelled: true,
                selected_text: None,
            });
        }

        let Some(selected_entry) = self.session.session_manager().get_entry(entry_id) else {
            return Err(AgentSessionRuntimeError::InvalidForkEntry);
        };

        let (target_leaf_id, selected_text): (Option<String>, Option<String>) = match position {
            ForkPosition::At => (Some(selected_entry.id().to_string()), None),
            ForkPosition::Before => {
                let SessionEntry::Message(message_entry) = &selected_entry else {
                    return Err(AgentSessionRuntimeError::InvalidForkEntry);
                };
                if message_role(&message_entry.message) != Some("user") {
                    return Err(AgentSessionRuntimeError::InvalidForkEntry);
                }
                let text = extract_user_message_text(message_entry.message.get("content"));
                (selected_entry.parent_id().map(String::from), Some(text))
            }
        };

        let previous_session_file = self.session.session_file();
        let (persisted, session_dir, current_session_file) = {
            let manager = self.session.session_manager();
            (
                manager.is_persisted(),
                manager.get_session_dir().to_string(),
                manager.get_session_file().map(String::from),
            )
        };

        if persisted {
            let Some(current_session_file) = current_session_file else {
                return Err(AgentSessionRuntimeError::MissingSessionFile);
            };

            if target_leaf_id.is_none() {
                let mut manager =
                    SessionManager::create(&self.services_cwd, Some(&session_dir), None);
                let _ = manager.new_session(SmNewSessionOptions {
                    parent_session: Some(current_session_file),
                    ..SmNewSessionOptions::default()
                });
                let target_file = manager.get_session_file().map(String::from);
                let cwd = manager.get_cwd().to_string();
                self.teardown_current(SessionShutdownReason::Fork, target_file);
                let options = self.fork_factory_options(cwd, manager, previous_session_file);
                let result = (self.create_runtime)(options);
                self.apply(result);
                self.finish_session_replacement();
                return Ok(ForkResult {
                    cancelled: false,
                    selected_text,
                });
            }

            if !Path::new(&current_session_file).exists() {
                return Err(AgentSessionRuntimeError::SessionNotSaved);
            }

            // pi: SessionManager.open(currentSessionFile, sessionDir) — the session
            // dir is derived from the file's parent by the ported `open`.
            let mut manager = SessionManager::open(&current_session_file)
                .map_err(AgentSessionRuntimeError::OpenSession)?;
            let leaf = target_leaf_id.expect("target_leaf_id is Some in this branch");
            let forked = manager
                .create_branched_session(&leaf)
                .map_err(|err| AgentSessionRuntimeError::Branch(err.to_string()))?;
            if forked.is_none() {
                return Err(AgentSessionRuntimeError::ForkFailed);
            }
            let target_file = manager.get_session_file().map(String::from);
            let cwd = manager.get_cwd().to_string();
            self.teardown_current(SessionShutdownReason::Fork, target_file);
            let options = self.fork_factory_options(cwd, manager, previous_session_file);
            let result = (self.create_runtime)(options);
            self.apply(result);
            self.finish_session_replacement();
            return Ok(ForkResult {
                cancelled: false,
                selected_text,
            });
        }

        // In-memory: pi reuses `this.session.sessionManager` (a shared JS
        // reference). Mutate the branch in place, then extract the owned manager
        // for the replacement session (see `AgentSession::swap_out_session_manager`).
        {
            let mut manager = self.session.session_manager();
            match &target_leaf_id {
                None => {
                    let _ = manager.new_session(SmNewSessionOptions {
                        parent_session: current_session_file,
                        ..SmNewSessionOptions::default()
                    });
                }
                Some(leaf) => {
                    manager
                        .create_branched_session(leaf)
                        .map_err(|err| AgentSessionRuntimeError::Branch(err.to_string()))?;
                }
            }
        }
        let target_file = self.session.session_file();
        self.teardown_current(SessionShutdownReason::Fork, target_file);
        let manager = self.session.swap_out_session_manager();
        let cwd = self.services_cwd.clone();
        let options = self.fork_factory_options(cwd, manager, previous_session_file);
        let result = (self.create_runtime)(options);
        self.apply(result);
        self.finish_session_replacement();
        Ok(ForkResult {
            cancelled: false,
            selected_text,
        })
    }

    /// Dispose the runtime and its current session (pi's `dispose`,
    /// `agent-session-runtime.ts:395`).
    ///
    /// Fires `session_shutdown` (reason `quit`) on the current runner, runs the
    /// `before_session_invalidate` host hook, and disposes the session.
    pub fn dispose(&mut self) {
        {
            let runner = self.session.extension_runner();
            if runner.has_handlers("session_shutdown") {
                runner.emit_session_shutdown(SessionShutdownEvent {
                    reason: SessionShutdownReason::Quit,
                    target_session_file: None,
                });
            }
        }
        if let Some(mut callback) = self.before_session_invalidate.take() {
            callback();
            self.before_session_invalidate = Some(callback);
        }
        self.session.dispose();
    }

    // ---- private helpers --------------------------------------------------

    /// Build the factory options for a `/fork` replacement (the three fork branches
    /// differ only in the prepared manager + cwd).
    fn fork_factory_options(
        &self,
        cwd: String,
        session_manager: SessionManager,
        previous_session_file: Option<String>,
    ) -> AgentSessionRuntimeFactoryOptions {
        AgentSessionRuntimeFactoryOptions {
            cwd,
            agent_dir: self.agent_dir.clone(),
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::Fork,
                previous_session_file,
            }),
        }
    }

    /// Emit `session_before_switch` and report whether a handler cancelled (pi's
    /// `emitBeforeSwitch`, `agent-session-runtime.ts:133`).
    fn emit_before_switch(
        &self,
        reason: SessionBeforeSwitchReason,
        target_session_file: Option<String>,
    ) -> bool {
        let runner = self.session.extension_runner();
        if !runner.has_handlers("session_before_switch") {
            return false;
        }
        let outcome = runner.emit(&ExtensionDispatchEvent::SessionBeforeSwitch(
            SessionBeforeSwitchEvent {
                reason,
                target_session_file,
            },
        ));
        matches!(outcome, ExtensionEmitOutcome::BeforeSwitch(result) if result.cancel == Some(true))
    }

    /// Emit `session_before_fork` and report whether a handler cancelled (pi's
    /// `emitBeforeFork`, `agent-session-runtime.ts:150`).
    fn emit_before_fork(&self, entry_id: &str, position: ForkPosition) -> bool {
        let runner = self.session.extension_runner();
        if !runner.has_handlers("session_before_fork") {
            return false;
        }
        let outcome = runner.emit(&ExtensionDispatchEvent::SessionBeforeFork(
            SessionBeforeForkEvent {
                entry_id: entry_id.to_string(),
                position,
            },
        ));
        matches!(outcome, ExtensionEmitOutcome::BeforeFork(result) if result.cancel == Some(true))
    }

    /// Fire `session_shutdown` on the current runner, run the
    /// `before_session_invalidate` hook, and dispose the outgoing session (pi's
    /// `teardownCurrent`, `agent-session-runtime.ts:167`).
    fn teardown_current(
        &mut self,
        reason: SessionShutdownReason,
        target_session_file: Option<String>,
    ) {
        {
            let runner = self.session.extension_runner();
            if runner.has_handlers("session_shutdown") {
                runner.emit_session_shutdown(SessionShutdownEvent {
                    reason,
                    target_session_file,
                });
            }
        }
        if let Some(mut callback) = self.before_session_invalidate.take() {
            callback();
            self.before_session_invalidate = Some(callback);
        }
        self.session.dispose();
    }

    /// Make the factory result current (pi's `apply`, `agent-session-runtime.ts:177`).
    fn apply(&mut self, result: AgentSessionRuntimeResult) {
        self.session = result.session;
        self.services_cwd = result.cwd;
        self.model_fallback_message = result.model_fallback_message;
    }

    /// Run the post-replacement `rebind_session` hook (pi's
    /// `finishSessionReplacement`, `agent-session-runtime.ts:184`). pi's `withSession`
    /// branch is deferred (see module docs).
    fn finish_session_replacement(&mut self) {
        if let Some(mut callback) = self.rebind_session.take() {
            callback(&self.session);
            self.rebind_session = Some(callback);
        }
    }
}

/// Create the initial runtime from a session factory and an initial target (pi's
/// `createAgentSessionRuntime`, `agent-session-runtime.ts:411`).
///
/// The same factory is stored on the returned runtime and reused for later `/new`,
/// `/resume`, and `/fork` flows.
pub fn create_agent_session_runtime(
    mut create_runtime: CreateAgentSessionRuntimeFactory,
    options: AgentSessionRuntimeFactoryOptions,
) -> Result<AgentSessionRuntime, MissingSessionCwdError> {
    assert_session_cwd_exists(&options.session_manager, &options.cwd)?;
    let agent_dir = options.agent_dir.clone();
    let result = create_runtime(options);
    Ok(AgentSessionRuntime {
        session: result.session,
        services_cwd: result.cwd,
        agent_dir,
        create_runtime,
        model_fallback_message: result.model_fallback_message,
        rebind_session: None,
        before_session_invalidate: None,
    })
}

/// The `role` of a message value, if any (pi reads `message.role`).
fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// Extract the plain text of a user message's content (pi's `extractUserMessageText`,
/// `agent-session-runtime.ts:56`): a bare string is returned as-is; an array of
/// content parts contributes each `text` part, joined.
fn extract_user_message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests;
