//! Shell-output capture, mirroring
//! `packages/agent/src/harness/utils/shell-output.ts`.
//!
//! [`execute_shell_with_capture`] runs a command through an [`ExecutionEnv`],
//! sanitizing and windowing streamed output, spilling the full output to a temp
//! file once it grows past [`DEFAULT_MAX_BYTES`], and returning a tail-truncated
//! view.
//!
//! # Faithful divergences from pi
//!
//! - **Synchronous.** pi sequences file writes through a `writeChain` promise
//!   and `await`s it after `exec`. This port performs each write eagerly inline
//!   and records the first failure in `write_error`, checked in the same order
//!   pi checks `writeChain` then `captureError`.
//! - **No `AbortSignal`.** With no async cancellation, `options.abortSignal` is
//!   gone; `cancelled` is derived solely from an `aborted`-coded
//!   [`ExecutionError`], exactly the branch pi keeps when the signal is absent.
//! - **`outputBytes` window.** pi accumulates `text.length` (UTF-16 code units)
//!   for its sliding-window trim; this port mirrors that with
//!   [`str::encode_utf16`] rather than byte length.

use std::collections::BTreeMap;

use super::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES};
use crate::harness::env::{
    ExecutionEnv, ExecutionError, ExecutionErrorCode, FileContent, OutputCallback, ShellExecOptions,
};

/// Options for [`execute_shell_with_capture`]. Mirrors pi's
/// `ShellCaptureOptions` (`ShellExecOptions` minus the stream callbacks, plus a
/// single `on_chunk`).
#[derive(Default)]
pub struct ShellCaptureOptions<'a> {
    /// Working directory for the command.
    pub cwd: Option<String>,
    /// Additional environment variables.
    pub env: Option<BTreeMap<String, String>>,
    /// Timeout in seconds.
    pub timeout: Option<f64>,
    /// Called with each sanitized output chunk as it is produced.
    pub on_chunk: Option<OutputCallback<'a>>,
}

/// The result of a captured shell command. Mirrors pi's `ShellCaptureResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCaptureResult {
    /// The tail-truncated (or full, when short) output.
    pub output: String,
    /// Exit code, or `None` when cancelled.
    pub exit_code: Option<i32>,
    /// Whether the command was cancelled.
    pub cancelled: bool,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Path to the full-output temp file, when one was spilled.
    pub full_output_path: Option<String>,
}

/// Coerce a [`FileError`](crate::harness::env::FileError) (or any other cause)
/// message into an `unknown`-coded [`ExecutionError`]. Mirrors pi's
/// `toExecutionError`.
fn file_error_to_execution_error(message: String) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::Unknown, message)
}

/// Strip control characters that should not appear in captured shell output,
/// keeping tab/newline/carriage-return. Mirrors pi's `sanitizeBinaryOutput`.
pub fn sanitize_binary_output(input: &str) -> String {
    input
        .chars()
        .filter(|character| {
            let code = *character as u32;
            if code == 0x09 || code == 0x0a || code == 0x0d {
                return true;
            }
            if code <= 0x1f {
                return false;
            }
            if (0xfff9..=0xfffb).contains(&code) {
                return false;
            }
            true
        })
        .collect()
}

/// Mutable capture state accumulated across streamed chunks. Kept separate from
/// the streaming callbacks so both `on_stdout` and `on_stderr` can drive it.
struct Capture<'a> {
    env: &'a dyn ExecutionEnv,
    chunks: Vec<String>,
    /// Sliding-window length in UTF-16 code units (pi's `outputBytes`).
    output_len: usize,
    /// Running raw-chunk byte total (pi's `totalBytes`).
    total_bytes: usize,
    full_output_path: Option<String>,
    write_error: Option<ExecutionError>,
    capture_error: Option<ExecutionError>,
    on_chunk: Option<OutputCallback<'a>>,
}

impl Capture<'_> {
    /// pi's `appendFullOutput`: append `text` to the spilled file if one exists.
    fn append_full_output(&mut self, text: &str) {
        if self.full_output_path.is_none()
            || self.capture_error.is_some()
            || self.write_error.is_some()
        {
            return;
        }
        let path = self.full_output_path.clone().unwrap();
        // This consumer carries no abort signal (see the module divergence note),
        // so `None` is passed where pi threads `options?.abortSignal`.
        if let Err(error) = self.env.append_file(&path, FileContent::Text(text), None) {
            self.write_error = Some(file_error_to_execution_error(error.message));
        }
    }

    /// pi's `ensureFullOutputFile`: create the spill file and seed it with
    /// `initial_content` if one does not exist yet.
    fn ensure_full_output_file(&mut self, initial_content: &str) {
        if self.full_output_path.is_some()
            || self.capture_error.is_some()
            || self.write_error.is_some()
        {
            return;
        }
        let temp_file = match self.env.create_temp_file("bash-", ".log", None) {
            Ok(path) => path,
            Err(error) => {
                self.write_error = Some(file_error_to_execution_error(error.message));
                return;
            }
        };
        self.full_output_path = Some(temp_file.clone());
        if let Err(error) =
            self.env
                .append_file(&temp_file, FileContent::Text(initial_content), None)
        {
            self.write_error = Some(file_error_to_execution_error(error.message));
        }
    }

    /// pi's `onChunk`: sanitize, spill/append, window-trim, and forward.
    fn on_chunk(&mut self, chunk: &str) {
        let max_output_bytes = DEFAULT_MAX_BYTES * 2;
        self.total_bytes += chunk.len();
        let text = sanitize_binary_output(chunk).replace('\r', "");
        if self.total_bytes > DEFAULT_MAX_BYTES && self.full_output_path.is_none() {
            let seed = format!("{}{text}", self.chunks.concat());
            self.ensure_full_output_file(&seed);
        } else {
            self.append_full_output(&text);
        }
        self.output_len += text.encode_utf16().count();
        self.chunks.push(text.clone());
        while self.output_len > max_output_bytes && self.chunks.len() > 1 {
            let removed = self.chunks.remove(0);
            self.output_len -= removed.encode_utf16().count();
        }
        if let Some(callback) = self.on_chunk.as_mut() {
            callback(&text);
        }
    }
}

/// Execute `command` through `env`, capturing sanitized output with a
/// sliding-window tail and a full-output spill file. Mirrors pi's
/// `executeShellWithCapture`.
pub fn execute_shell_with_capture(
    env: &dyn ExecutionEnv,
    command: &str,
    options: Option<ShellCaptureOptions<'_>>,
) -> Result<ShellCaptureResult, ExecutionError> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let ShellCaptureOptions {
        cwd,
        env: env_vars,
        timeout,
        on_chunk,
    } = options.unwrap_or_default();

    let capture = Rc::new(RefCell::new(Capture {
        env,
        chunks: Vec::new(),
        output_len: 0,
        total_bytes: 0,
        full_output_path: None,
        write_error: None,
        capture_error: None,
        on_chunk,
    }));

    // Both stdout and stderr feed the same `on_chunk`, as in pi.
    let stdout_capture = capture.clone();
    let stderr_capture = capture.clone();
    let exec_options = ShellExecOptions {
        cwd,
        env: env_vars,
        timeout,
        abort_signal: None,
        on_stdout: Some(Box::new(move |chunk: &str| {
            stdout_capture.borrow_mut().on_chunk(chunk)
        })),
        on_stderr: Some(Box::new(move |chunk: &str| {
            stderr_capture.borrow_mut().on_chunk(chunk)
        })),
    };

    let result = env.exec(command, exec_options);

    // The stream callbacks (and their Rc clones) are dropped with `exec_options`
    // above, leaving `capture` uniquely owned.
    let mut capture = Rc::try_unwrap(capture)
        .unwrap_or_else(|_| unreachable!("stream callbacks dropped with exec options"))
        .into_inner();

    let tail_output = capture.chunks.concat();
    let truncation = truncate_tail(&tail_output, TruncationOptions::default());
    if truncation.truncated && capture.full_output_path.is_none() {
        capture.ensure_full_output_file(&tail_output);
    }

    if let Some(error) = capture.write_error {
        return Err(error);
    }
    if let Some(error) = capture.capture_error {
        return Err(error);
    }

    let output = if truncation.truncated {
        truncation.content.clone()
    } else {
        tail_output.clone()
    };

    match result {
        Err(error) => {
            if error.code == ExecutionErrorCode::Aborted {
                return Ok(ShellCaptureResult {
                    output,
                    exit_code: None,
                    cancelled: true,
                    truncated: truncation.truncated,
                    full_output_path: capture.full_output_path,
                });
            }
            Err(error)
        }
        Ok(exec_output) => Ok(ShellCaptureResult {
            output,
            exit_code: Some(exec_output.exit_code),
            cancelled: false,
            truncated: truncation.truncated,
            full_output_path: capture.full_output_path,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::env::{FileSystem, MemoryExecutionEnv};

    #[test]
    fn sanitize_strips_control_chars_but_keeps_whitespace() {
        // Bell (0x07) dropped; tab/newline/carriage-return kept; interlinear
        // annotation anchors (0xfff9..=0xfffb) dropped.
        let input = "a\u{7}b\tc\nd\re\u{fffa}f";
        assert_eq!(sanitize_binary_output(input), "ab\tc\nd\ref");
    }

    #[test]
    fn captures_small_output_without_spilling() {
        let env = MemoryExecutionEnv::new("/work");
        env.push_exec_output(vec!["hello\r\n".into(), "world\n".into()], vec![], 0);
        let result = execute_shell_with_capture(&env, "echo", None).unwrap();
        // Carriage returns stripped; small output not truncated, no spill file.
        assert_eq!(result.output, "hello\nworld\n");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert!(!result.truncated);
        assert!(result.full_output_path.is_none());
    }

    #[test]
    fn forwards_chunks_to_on_chunk_callback() {
        let env = MemoryExecutionEnv::new("/work");
        env.push_exec_output(vec!["a".into(), "b".into()], vec![], 0);
        let seen = std::cell::RefCell::new(Vec::new());
        let options = ShellCaptureOptions {
            on_chunk: Some(Box::new(|chunk: &str| {
                seen.borrow_mut().push(chunk.to_string())
            })),
            ..Default::default()
        };
        execute_shell_with_capture(&env, "cmd", Some(options)).unwrap();
        assert_eq!(*seen.borrow(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn captures_large_output_to_a_full_output_file() {
        // Mirrors nodejs-env.test.ts "captures large shell output to a full
        // output file", driven through the in-memory env instead of a real shell.
        let env = MemoryExecutionEnv::new("/work");
        let chunks: Vec<String> = (0..15_000).map(|_| "line\n".to_string()).collect();
        env.push_exec_output(chunks, vec![], 0);

        let result = execute_shell_with_capture(&env, "yes line | head -n 15000", None).unwrap();
        assert!(result.truncated);
        let full_path = result.full_output_path.clone().expect("spill file created");
        let full_output = env.read_text_file(&full_path, None).unwrap();
        assert!(full_output.split('\n').count() > 10_000);
        assert!(result.output.len() < full_output.len());
    }

    #[test]
    fn propagates_non_aborted_exec_errors() {
        let env = MemoryExecutionEnv::new("/work");
        env.push_exec_failure(ExecutionError::new(ExecutionErrorCode::SpawnError, "boom"));
        let error = execute_shell_with_capture(&env, "cmd", None).unwrap_err();
        assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    }

    #[test]
    fn aborted_exec_error_yields_cancelled_result() {
        let env = MemoryExecutionEnv::new("/work");
        env.push_exec_failure(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        let result = execute_shell_with_capture(&env, "cmd", None).unwrap();
        assert!(result.cancelled);
        assert_eq!(result.exit_code, None);
        assert!(!result.truncated);
    }
}
