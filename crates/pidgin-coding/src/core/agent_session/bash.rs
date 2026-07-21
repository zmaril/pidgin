//! Bash execution + persistence for [`AgentSession`], ported from pi's
//! `AgentSession.executeBash` / `recordBashResult` / `_flushPendingBashMessages`
//! (`packages/coding-agent/src/core/agent-session.ts`, the "Bash Execution"
//! section) plus the `executeBashWithOperations` helper it delegates to
//! (`packages/coding-agent/src/core/bash-executor.ts`, ported in
//! [`crate::core::bash_executor`]).
//!
//! [`AgentSession::execute_bash`] runs a shell command through a pluggable
//! [`BashOperations`] backend (reusing the one ported in
//! [`crate::core::tools::bash`]), streams its output through a bounded rolling
//! buffer with the same sanitize / truncate / spill-to-temp-file behavior pi's
//! executor has, and records the outcome as a `bashExecution` message.
//!
//! [`AgentSession::record_bash_result`] appends that message to agent state +
//! the session **immediately while idle**, or **defers it** into the
//! `_pendingBashMessages` buffer while a run is streaming (so a bash result
//! recorded mid-turn does not break tool_use / tool_result ordering).
//! [`AgentSession::flush_pending_bash_messages`] drains the buffer into agent
//! state + the session; the turn spine calls it in the prompt preflight (before
//! the next turn) and in `run_agent_prompt`'s finally block (after each run),
//! mirroring pi's flush points.
//!
//! Source of truth: `packages/coding-agent/src/core/agent-session.ts` +
//! `packages/coding-agent/src/core/bash-executor.ts`.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::watch;

use pidgin_agent::types::AgentMessage;

use crate::core::tools::bash::{create_local_bash_operations, BashError, BashOperations};

use super::session::AgentSession;
use super::turn::now_ms;

/// Re-export the executor engine types (moved to [`crate::core::bash_executor`]).
///
/// A single `pub use` both re-exports these names — so `agent_session/mod.rs`'s
/// `pub use bash::*;` keeps lifting them into [`crate::core::agent_session`],
/// preserving every existing caller and `bash/tests.rs`'s
/// `use super::{BashResult, ExecuteBashOptions};` unchanged — and brings them
/// into this module's scope, so `execute_bash` below still calls
/// [`execute_bash_with_operations`] directly.
pub use crate::core::bash_executor::{execute_bash_with_operations, BashResult, OnChunk};

/// Options for [`AgentSession::execute_bash`] (pi's `executeBash` `options`
/// argument, `{ excludeFromContext?, operations? }`).
#[derive(Default)]
pub struct ExecuteBashOptions {
    /// When `Some(true)`, the recorded message is excluded from LLM context
    /// (pi's `!!` prefix). `None` mirrors pi's `undefined` (the key is omitted).
    pub exclude_from_context: Option<bool>,
    /// A custom command-execution backend (pi's `options.operations`, e.g. remote
    /// execution). When `None`, a local-shell backend is built from the settings
    /// manager's shell path.
    pub operations: Option<Arc<dyn BashOperations>>,
}

/// Build the `bashExecution` message value (pi's `BashExecutionMessage` object
/// literal). `undefined`-valued fields (`exitCode` / `fullOutputPath` / an unset
/// `excludeFromContext`) are omitted, matching pi's `JSON.stringify` shape.
fn build_bash_message(
    command: &str,
    result: &BashResult,
    exclude_from_context: Option<bool>,
) -> AgentMessage {
    let mut map = serde_json::Map::new();
    map.insert("role".to_string(), json!("bashExecution"));
    map.insert("command".to_string(), json!(command));
    map.insert("output".to_string(), json!(result.output));
    if let Some(code) = result.exit_code {
        map.insert("exitCode".to_string(), json!(code));
    }
    map.insert("cancelled".to_string(), json!(result.cancelled));
    map.insert("truncated".to_string(), json!(result.truncated));
    if let Some(path) = &result.full_output_path {
        map.insert("fullOutputPath".to_string(), json!(path));
    }
    map.insert("timestamp".to_string(), json!(now_ms()));
    if let Some(exclude) = exclude_from_context {
        map.insert("excludeFromContext".to_string(), json!(exclude));
    }
    Value::Object(map)
}

impl AgentSession {
    /// Execute a bash command, recording its result in session history (pi's
    /// `executeBash`, agent-session.ts).
    ///
    /// Applies the configured shell command prefix (pi's `shopt -s
    /// expand_aliases`-style prefix), runs through `options.operations` or a
    /// local-shell backend built from the settings manager's shell path, records
    /// the outcome via [`AgentSession::record_bash_result`], and returns the
    /// [`BashResult`]. While the command runs, [`AgentSession::is_bash_running`]
    /// is true and [`AgentSession::abort_bash`] can cancel it.
    pub async fn execute_bash(
        &self,
        command: &str,
        on_chunk: Option<OnChunk>,
        options: ExecuteBashOptions,
    ) -> Result<BashResult, BashError> {
        // Install the abort handle (pi's `this._bashAbortController = new
        // AbortController()`).
        let (tx, rx) = watch::channel(false);
        *self.bash_abort.lock().unwrap() = Some(tx);

        // Apply the command prefix if configured (pi's `prefix ? ...`).
        let prefix = self.settings_manager.get_shell_command_prefix();
        let shell_path = self.settings_manager.get_shell_path();
        let resolved_command = match prefix {
            Some(prefix) => format!("{prefix}\n{command}"),
            None => command.to_string(),
        };

        let cwd = self.session_manager().get_cwd().to_string();

        let local_ops;
        let operations: &dyn BashOperations = match options.operations.as_ref() {
            Some(ops) => ops.as_ref(),
            None => {
                local_ops = create_local_bash_operations(shell_path);
                &local_ops
            }
        };

        let result =
            execute_bash_with_operations(&resolved_command, &cwd, operations, Some(rx), on_chunk)
                .await;

        // finally: clear the abort handle (pi's `this._bashAbortController =
        // undefined`).
        *self.bash_abort.lock().unwrap() = None;

        let result = result?;
        self.record_bash_result(command, &result, options.exclude_from_context);
        Ok(result)
    }

    /// Record a bash execution result in session history (pi's
    /// `recordBashResult`). Used by [`AgentSession::execute_bash`] and by
    /// extensions that handle bash execution themselves.
    ///
    /// While a run is streaming the message is deferred to the pending buffer to
    /// avoid breaking tool_use / tool_result ordering; while idle it is added to
    /// agent state and the session immediately.
    pub fn record_bash_result(
        &self,
        command: &str,
        result: &BashResult,
        exclude_from_context: Option<bool>,
    ) {
        let bash_message = build_bash_message(command, result, exclude_from_context);
        if self.is_streaming() {
            self.pending_bash_messages
                .lock()
                .unwrap()
                .push(bash_message);
        } else {
            self.agent.push_message(bash_message.clone());
            self.session_manager().append_message(bash_message);
        }
    }

    /// Cancel the running bash command (pi's `abortBash`).
    pub fn abort_bash(&self) {
        if let Some(tx) = self.bash_abort.lock().unwrap().as_ref() {
            let _ = tx.send(true);
        }
    }

    /// Whether a bash command is currently running (pi's `get isBashRunning`).
    pub fn is_bash_running(&self) -> bool {
        self.bash_abort.lock().unwrap().is_some()
    }

    /// Whether there are pending bash messages waiting to be flushed (pi's `get
    /// hasPendingBashMessages`).
    pub fn has_pending_bash_messages(&self) -> bool {
        !self.pending_bash_messages.lock().unwrap().is_empty()
    }

    /// Flush pending bash messages to agent state and the session (pi's
    /// `_flushPendingBashMessages`). Called before the next prompt and after each
    /// run to maintain proper message ordering.
    pub(super) fn flush_pending_bash_messages(&self) {
        let pending = {
            let mut guard = self.pending_bash_messages.lock().unwrap();
            if guard.is_empty() {
                return;
            }
            std::mem::take(&mut *guard)
        };
        for bash_message in pending {
            self.agent.push_message(bash_message.clone());
            self.session_manager().append_message(bash_message);
        }
    }
}

#[cfg(test)]
mod tests;
