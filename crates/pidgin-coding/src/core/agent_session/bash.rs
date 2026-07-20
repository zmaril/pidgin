//! Bash execution + persistence for [`AgentSession`], ported from pi's
//! `AgentSession.executeBash` / `recordBashResult` / `_flushPendingBashMessages`
//! (`packages/coding-agent/src/core/agent-session.ts`, the "Bash Execution"
//! section) plus the `executeBashWithOperations` helper it delegates to
//! (`packages/coding-agent/src/core/bash-executor.ts`).
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

// straitjacket-allow-file:duplication

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::watch;

use pidgin_agent::types::AgentMessage;

use crate::core::tools::bash::{
    create_local_bash_operations, BashError, BashExecOptions, BashOperations,
};
use crate::core::tools::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES};
use crate::utils::ansi::strip_ansi;
use crate::utils::shell::sanitize_binary_output;

use super::session::AgentSession;
use super::turn::now_ms;

/// The outcome of a bash execution (pi's `BashResult`, `bash-executor.ts`).
///
/// `output` is the sanitized combined stdout+stderr (possibly truncated);
/// `exit_code` is `None` when the process was killed / cancelled;
/// `full_output_path` names the temp file holding the untruncated output when one
/// was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashResult {
    /// Combined stdout + stderr output (sanitized, possibly truncated).
    pub output: String,
    /// Process exit code (`None` if killed / cancelled).
    pub exit_code: Option<i32>,
    /// Whether the command was cancelled via the abort signal.
    pub cancelled: bool,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Path to the temp file with the full output (when output was spilled).
    pub full_output_path: Option<String>,
}

/// A streaming output callback invoked with each sanitized chunk (pi's
/// `onChunk?: (chunk: string) => void`).
pub type OnChunk = Box<dyn FnMut(&str)>;

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

/// An incremental UTF-8 decoder mirroring `TextDecoder`'s `{ stream: true }`
/// behavior: an incomplete trailing multi-byte sequence is held back until the
/// next chunk completes it; invalid sequences become U+FFFD. Faithful to pi's
/// per-chunk `decoder.decode(data, { stream: true })` so a multi-byte character
/// split across two `on_data` chunks still decodes correctly.
#[derive(Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// Decode `data`, prepending any bytes held from the previous call and
    /// holding back an incomplete trailing sequence (never flushed — pi's
    /// executor never issues a final non-stream `decode`).
    fn feed(&mut self, data: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);
        let mut out = String::new();
        let mut idx = 0;
        while idx < buf.len() {
            match std::str::from_utf8(&buf[idx..]) {
                Ok(s) => {
                    out.push_str(s);
                    idx = buf.len();
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        out.push_str(std::str::from_utf8(&buf[idx..idx + valid]).unwrap());
                        idx += valid;
                    }
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            idx += n;
                        }
                        None => break,
                    }
                }
            }
        }
        if idx < buf.len() {
            self.pending = buf[idx..].to_vec();
        }
        out
    }
}

/// The bounded rolling-buffer accumulator state (pi's `outputChunks` /
/// `outputBytes` / temp-file locals inside `executeBashWithOperations`). Held
/// behind an `Rc<RefCell<..>>` so the `'static`
/// [`OnData`](crate::core::tools::bash::OnData) closure can own a handle while
/// the executor reads the final state after `exec` returns.
#[derive(Default)]
struct AccState {
    output_chunks: Vec<String>,
    output_bytes: usize,
    total_bytes: usize,
    temp_file: Option<(std::fs::File, String)>,
    decoder: Utf8StreamDecoder,
}

impl AccState {
    /// Lazily create `<tmpdir>/pi-bash-<random>.log` and backfill the chunks
    /// buffered so far (pi's `ensureTempFile`). A creation failure leaves
    /// `temp_file` `None` — no output path is reported, matching a best-effort
    /// spill.
    fn ensure_temp_file(&mut self) {
        if self.temp_file.is_some() {
            return;
        }
        let Some((mut file, path)) = create_temp_file() else {
            return;
        };
        for chunk in &self.output_chunks {
            let _ = file.write_all(chunk.as_bytes());
        }
        self.temp_file = Some((file, path));
    }
}

/// Create the spill file `<tmpdir>/pi-bash-<random>.log`, returning its handle
/// and path. Mirrors pi's `join(tmpdir(), "pi-bash-<hex>.log")`; the random
/// component comes from `tempfile`'s charset rather than strict hex (the
/// observable contract is the `pi-bash-*.log` shape, which is preserved).
fn create_temp_file() -> Option<(std::fs::File, String)> {
    let named = tempfile::Builder::new()
        .prefix("pi-bash-")
        .suffix(".log")
        .rand_bytes(16)
        .tempfile_in(std::env::temp_dir())
        .ok()?;
    let (file, path) = named.keep().ok()?;
    Some((file, path.to_string_lossy().into_owned()))
}

/// Execute `command` in `cwd` through `operations`, accumulating sanitized
/// output with the same bounded rolling buffer, spill-to-temp-file, and
/// tail-truncation behavior as pi's `executeBashWithOperations`
/// (`bash-executor.ts`).
///
/// `signal` is the cooperative abort handle (a `watch::Receiver<bool>` bridged
/// from the session's `_bashAbortController`). On a non-abort `exec` failure the
/// error propagates; when the signal is tripped the partial output is returned as
/// a `cancelled` [`BashResult`] (pi's catch-if-aborted branch).
pub async fn execute_bash_with_operations(
    command: &str,
    cwd: &str,
    operations: &dyn BashOperations,
    signal: Option<watch::Receiver<bool>>,
    mut on_chunk: Option<OnChunk>,
) -> Result<BashResult, BashError> {
    // pi keeps a rolling window of `DEFAULT_MAX_BYTES * 2`; older chunks are
    // dropped from memory once the temp file (if any) holds them.
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;

    let state = Rc::new(RefCell::new(AccState::default()));
    let state_for_closure = Rc::clone(&state);

    let on_data: crate::core::tools::bash::OnData = Box::new(move |data: &[u8]| {
        let mut state = state_for_closure.borrow_mut();
        state.total_bytes += data.len();

        // Sanitize: strip ANSI, replace binary garbage, normalize newlines
        // (pi: `sanitizeBinaryOutput(stripAnsi(decode(data))).replace(/\r/g, "")`).
        let decoded = state.decoder.feed(data);
        let text = sanitize_binary_output(&strip_ansi(&decoded)).replace('\r', "");

        // Start spilling to a temp file once the raw stream exceeds the threshold.
        if state.total_bytes > DEFAULT_MAX_BYTES {
            state.ensure_temp_file();
        }
        if let Some((file, _)) = state.temp_file.as_mut() {
            let _ = file.write_all(text.as_bytes());
        }

        // Keep the rolling buffer, dropping the oldest chunks past the window.
        state.output_bytes += text.len();
        state.output_chunks.push(text.clone());
        while state.output_bytes > max_output_bytes && state.output_chunks.len() > 1 {
            let removed = state.output_chunks.remove(0);
            state.output_bytes -= removed.len();
        }

        if let Some(cb) = on_chunk.as_mut() {
            cb(&text);
        }
    });

    let exec_result = operations
        .exec(
            command,
            cwd,
            BashExecOptions {
                on_data,
                signal: signal.clone(),
                timeout: None,
                env: None,
            },
        )
        .await;

    let cancelled = signal.as_ref().map(|s| *s.borrow()).unwrap_or(false);

    match exec_result {
        Ok(result) => Ok(finalize(
            &state,
            if cancelled { None } else { result.exit_code },
            cancelled,
        )),
        Err(err) => {
            // An abort surfaces the partial output as a cancelled result (pi's
            // `if (options.signal?.aborted)` catch branch); any other failure
            // propagates.
            if cancelled {
                Ok(finalize(&state, None, true))
            } else {
                Err(err)
            }
        }
    }
}

/// Compose the final [`BashResult`] from the accumulated state, applying pi's
/// tail truncation and spilling on truncation.
fn finalize(state: &Rc<RefCell<AccState>>, exit_code: Option<i32>, cancelled: bool) -> BashResult {
    let mut state = state.borrow_mut();
    let full_output = state.output_chunks.concat();
    let truncation = truncate_tail(&full_output, TruncationOptions::default());
    if truncation.truncated {
        state.ensure_temp_file();
    }
    let output = if truncation.truncated {
        truncation.content.clone()
    } else {
        full_output
    };
    BashResult {
        output,
        exit_code,
        cancelled,
        truncated: truncation.truncated,
        full_output_path: state.temp_file.as_ref().map(|(_, p)| p.clone()),
    }
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
