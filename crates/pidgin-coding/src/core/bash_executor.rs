//! The bash executor engine, ported from pi's
//! `packages/coding-agent/src/core/bash-executor.ts`.
//!
//! [`execute_bash_with_operations`] runs a shell command through a pluggable
//! [`BashOperations`](crate::core::tools::bash::BashOperations) backend (reusing
//! the one ported in [`crate::core::tools::bash`]), streams its output through a
//! bounded rolling buffer with the same sanitize / truncate / spill-to-temp-file
//! behavior pi's executor has, and returns the outcome as a [`BashResult`].
//!
//! This module has no dependency on
//! [`AgentSession`](crate::core::agent_session::AgentSession): the dependency
//! runs one way — the session's `execute_bash` (in
//! [`crate::core::agent_session::bash`]) delegates here, mirroring pi's
//! `agent-session.ts` importing from `bash-executor.ts`.
//!
//! Source of truth: `packages/coding-agent/src/core/bash-executor.ts`.

// straitjacket-allow-file:duplication

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use tokio::sync::watch;

use crate::core::tools::bash::{BashError, BashExecOptions, BashOperations};
use crate::core::tools::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES};
use crate::utils::ansi::strip_ansi;
use crate::utils::shell::sanitize_binary_output;

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
