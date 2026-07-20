//! Port of pi's `core/tools/bash.ts`.
//!
//! The bash tool spawns a shell subprocess, streams its stdout/stderr through
//! an [`OutputAccumulator`], and enforces timeouts and abort signals. Command
//! execution goes through the pluggable [`BashOperations`] trait (pi's
//! `BashOperations` interface) so extensions can swap in SSH/container
//! backends; [`create_local_bash_operations`] returns the default
//! `tokio::process`-backed local shell implementation.
//!
//! ## Process model (see notes/startup/communications.md §6.2)
//!
//! The local backend spawns the shell with **pipes, never PTYs**. On Unix the
//! child is placed in its own process group via
//! [`tokio::process::Command::process_group`]`(0)` (pi's `detached: true`), so
//! killing the negative PID (`killpg`, via [`kill_process_tree`]) reaps the
//! entire descendant tree — not just the shell. Timeouts and aborts both funnel
//! through `kill_process_tree`. Exit supervision uses
//! [`wait_for_child_process`], which drains the pipes past a grace window so a
//! backgrounded descendant's output tail is not truncated (pi#5303).
//!
//! ## Where ANSI stripping happens
//!
//! Faithful to pi: the accumulator stores the *raw* decoded bytes (so its
//! byte/line accounting — and therefore the truncation footer's line numbers —
//! matches pi exactly), and [`BashToolResult::content`] is the raw text with
//! ANSI intact, byte-for-byte with pi's `execute` return value. pi strips ANSI
//! and sanitizes only when *materializing* the text for display/model
//! consumption, in `render-utils.ts`'s `getTextOutput`. That transform is
//! reproduced here as [`get_text_output`]; callers (and the eventual render
//! layer) apply it at the same point pi does.
//!
//! ## Deferred
//!
//! The pi-tui `renderCall` / `renderResult` components (elapsed-time ticking,
//! preview truncation, styled footers) are TUI-only and depend on the theme
//! layer, so — as with the other ported tools — they are not reproduced here;
//! only the `execute` path is ported.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::utils::child_process::wait_for_child_process;
use crate::utils::shell::{
    get_shell_config, get_shell_env, kill_process_tree, track_detached_child_pid,
    untrack_detached_child_pid,
};

use super::output_accumulator::{
    OutputAccumulator, OutputAccumulatorOptions, OutputSnapshot, TempFileSink,
};
use super::truncate::{format_size, TruncatedBy, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

// pi materializes display text in `render-utils.ts`'s `getTextOutput`; the port
// keeps that transform at that layer. Re-exported so existing callers (and
// tests) can still reach it as `bash::get_text_output`.
pub use super::render_utils::get_text_output;

/// Hard ceiling on the resolved timeout, in milliseconds (pi's
/// `MAX_TIMEOUT_MS = 2_147_483_647`).
pub const MAX_TIMEOUT_MS: u64 = 2_147_483_647;

/// The tool's name (pi's `name: "bash"`).
pub const NAME: &str = "bash";

/// Throttle window for streaming `on_update` snapshots (pi's
/// `BASH_UPDATE_THROTTLE_MS = 100`).
const BASH_UPDATE_THROTTLE_MS: i64 = 100;

/// Maximum resolved timeout in seconds (pi's `MAX_TIMEOUT_SECONDS`).
fn max_timeout_seconds() -> f64 {
    MAX_TIMEOUT_MS as f64 / 1000.0
}

/// Render a JavaScript `number` the way `${value}` would.
///
/// For finite integer-valued and simple fractional values this coincides with
/// Rust's `Display` (`1.0 -> "1"`, `1.5 -> "1.5"`), which is all the bash tool
/// ever produces (whole/second-fractional timeouts). Very large magnitudes
/// where V8 switches to exponential notation are a documented edge that does
/// not arise in practice.
fn js_number_to_string(value: f64) -> String {
    format!("{value}")
}

/// Convert a timeout in seconds to milliseconds, mirroring pi's
/// `resolveTimeoutMs`. Returns [`BashError::InvalidTimeout`] with pi's exact
/// messages for non-finite/non-positive or over-ceiling values.
fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<u64>, BashError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(BashError::InvalidTimeout(
            "Invalid timeout: must be a finite number of seconds".to_string(),
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS as f64 {
        return Err(BashError::InvalidTimeout(format!(
            "Invalid timeout: maximum is {} seconds",
            js_number_to_string(max_timeout_seconds())
        )));
    }
    Ok(Some(timeout_ms as u64))
}

/// The tool description string (pi's `description`), kept for parity even
/// though the render layer that surfaces it is deferred.
pub fn description() -> String {
    format!(
        "Execute a bash command in the current working directory. Returns stdout and stderr. \
Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full \
output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    )
}

/// Errors surfaced by [`BashOperations::exec`], reproducing pi's thrown
/// `Error(message)` strings byte-for-byte via [`BashError::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashError {
    /// The abort signal fired (pi throws `Error("aborted")`).
    Aborted,
    /// The timeout elapsed. Carries the seconds substring exactly as it appears
    /// in pi's `Error("timeout:<secs>")`.
    Timeout(String),
    /// An invalid timeout argument (pi's `resolveTimeoutMs` throws).
    InvalidTimeout(String),
    /// The working directory does not exist (pi throws
    /// `Error("Working directory does not exist: <cwd>\nCannot execute bash commands.")`).
    WorkingDirectoryMissing(String),
    /// The shell could not be resolved or spawned (an I/O error message).
    Spawn(String),
}

impl BashError {
    /// The exact message string pi would carry on the thrown `Error`.
    pub fn message(&self) -> String {
        match self {
            BashError::Aborted => "aborted".to_string(),
            BashError::Timeout(secs) => format!("timeout:{secs}"),
            BashError::InvalidTimeout(msg) => msg.clone(),
            BashError::WorkingDirectoryMissing(cwd) => {
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands.")
            }
            BashError::Spawn(msg) => msg.clone(),
        }
    }
}

impl std::fmt::Display for BashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for BashError {}

/// The successful outcome of [`BashOperations::exec`] (pi's
/// `{ exitCode: number | null }`). `exit_code` is `None` when the process was
/// terminated by a signal without an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashExecResult {
    /// The child's exit code, or `None` when signalled.
    pub exit_code: Option<i32>,
}

/// Streaming sink invoked with each raw stdout/stderr chunk (pi's
/// `onData: (data: Buffer) => void`).
pub type OnData = Box<dyn FnMut(&[u8])>;

/// Options passed to [`BashOperations::exec`], mirroring pi's exec options
/// object (`{ onData, signal, timeout, env }`).
pub struct BashExecOptions {
    /// Streaming sink invoked with each raw stdout/stderr chunk, in arrival
    /// order (pi's `onData: (data: Buffer) => void`).
    pub on_data: OnData,
    /// Optional cancellation signal (a `watch::Receiver<bool>` that flips to
    /// `true` on abort, mirroring `AbortSignal`).
    pub signal: Option<watch::Receiver<bool>>,
    /// Optional timeout **in seconds** (pi's `timeout`, a `number` of seconds).
    pub timeout: Option<f64>,
    /// Optional environment; when `None` the backend uses [`get_shell_env`].
    pub env: Option<Vec<(String, String)>>,
}

/// Pluggable command-execution backend for the bash tool, mirroring pi's
/// `BashOperations` interface. Kept a `dyn`-object-safe trait (the method
/// returns a boxed future) so a custom backend can be injected as
/// `Arc<dyn BashOperations>` — for example across the napi boundary — while pi's
/// standard streaming/timeout/abort semantics stay in the `execute` layer.
///
/// The `Send + Sync` supertrait makes the *backend value* shareable across
/// threads (`Arc<dyn BashOperations>: Send + Sync`). The returned future is
/// deliberately **not** `+ Send`: it captures `opts.on_data`, a
/// `Box<dyn FnMut>` that is itself `!Send`. The `execute` layer and the tools
/// bridge (`block_on`) drive `!Send` futures, so this is intentional.
pub trait BashOperations: Send + Sync {
    /// Execute `command` in `cwd`, streaming output through `opts.on_data`, and
    /// resolve to the exit code (or a [`BashError`]).
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        opts: BashExecOptions,
    ) -> Pin<Box<dyn Future<Output = Result<BashExecResult, BashError>> + 'a>>;
}

/// Context describing a spawn, passed to a [`BashSpawnHook`] (pi's
/// `BashSpawnContext`).
#[derive(Debug, Clone)]
pub struct BashSpawnContext {
    /// The command to execute (after any command-prefix has been prepended).
    pub command: String,
    /// The working directory.
    pub cwd: String,
    /// The environment the command will run with.
    pub env: Vec<(String, String)>,
}

/// A hook to observe or rewrite a spawn before it happens (pi's
/// `BashSpawnHook = (context) => context`).
pub type BashSpawnHook = Box<dyn Fn(BashSpawnContext) -> BashSpawnContext>;

fn resolve_spawn_context(
    command: String,
    cwd: String,
    spawn_hook: Option<&BashSpawnHook>,
) -> BashSpawnContext {
    let base = BashSpawnContext {
        command,
        cwd,
        env: get_shell_env(),
    };
    match spawn_hook {
        Some(hook) => hook(base),
        None => base,
    }
}

/// The default local-shell [`BashOperations`], backed by `tokio::process`.
#[derive(Debug, Clone, Default)]
pub struct LocalBashOperations {
    shell_path: Option<String>,
}

/// Construct the local-shell operations backend (pi's
/// `createLocalBashOperations`). `shell_path` optionally overrides shell
/// resolution (pi's `options.shellPath`).
pub fn create_local_bash_operations(shell_path: Option<String>) -> LocalBashOperations {
    LocalBashOperations { shell_path }
}

impl BashOperations for LocalBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        opts: BashExecOptions,
    ) -> Pin<Box<dyn Future<Output = Result<BashExecResult, BashError>> + 'a>> {
        Box::pin(async move {
            use std::process::Stdio;
            use tokio::process::Command;

            let BashExecOptions {
                mut on_data,
                signal,
                timeout,
                env,
            } = opts;

            let timeout_ms = resolve_timeout_ms(timeout)?;

            // Fast-path abort before doing any work (pi's `if (signal?.aborted)`).
            if let Some(sig) = &signal {
                if *sig.borrow() {
                    return Err(BashError::Aborted);
                }
            }

            let shell_config = get_shell_config(self.shell_path.as_deref())
                .map_err(|e| BashError::Spawn(e.to_string()))?;

            // Verify the working directory exists (pi's `fsAccess(cwd, F_OK)`).
            if tokio::fs::metadata(cwd).await.is_err() {
                return Err(BashError::WorkingDirectoryMissing(cwd.to_string()));
            }

            let use_stdin = shell_config.use_stdin_transport();

            let mut cmd = Command::new(&shell_config.shell);
            cmd.args(&shell_config.args);
            if !use_stdin {
                // Non-legacy shells take the command as an argv element (`-c <cmd>`).
                cmd.arg(command);
            }
            cmd.current_dir(cwd);

            // pi passes a full `env` object, which Node uses to *replace* the
            // environment. `get_shell_env` already returns the complete env with
            // the agent bin dir prepended, so clear + set reproduces that.
            cmd.env_clear();
            for (key, value) in env.unwrap_or_else(get_shell_env) {
                cmd.env(key, value);
            }

            // Pipes, never PTYs. stdin is piped only for the legacy-WSL stdin
            // transport; otherwise it is null (pi's `stdio: [ignore|pipe, pipe, pipe]`).
            cmd.stdin(if use_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            });
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            // Own process group so `killpg` reaps the whole tree (pi's
            // `detached: true`). Unix is the tested path; on Windows tree reaping
            // relies on `taskkill /T` inside `kill_process_tree`.
            #[cfg(unix)]
            {
                cmd.process_group(0);
            }
            // Safety net: if this future is dropped, kill the direct child.
            cmd.kill_on_drop(true);

            let mut child = cmd.spawn().map_err(|e| BashError::Spawn(e.to_string()))?;

            // Legacy-WSL stdin transport: feed the command over stdin and close it
            // (pi's `child.stdin?.end(command)`, errors ignored).
            if use_stdin {
                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    let _ = stdin.write_all(command.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                }
            }

            let pid = child.id().map(|p| p as i32);
            if let Some(pid) = pid {
                track_detached_child_pid(pid);
            }

            // Timeout monitor: on elapse, flag it and kill the tree (pi's
            // `setTimeout` -> `killProcessTree`).
            let timed_out = Arc::new(AtomicBool::new(false));
            let timeout_task = match (timeout_ms, pid) {
                (Some(ms), Some(pid)) => {
                    let timed_out = Arc::clone(&timed_out);
                    Some(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        timed_out.store(true, Ordering::SeqCst);
                        kill_process_tree(pid);
                    }))
                }
                _ => None,
            };

            // Abort monitor: on abort, kill the tree (pi's `signal.addEventListener`).
            let abort_task = match (&signal, pid) {
                (Some(sig), Some(pid)) => {
                    let mut rx = sig.clone();
                    Some(tokio::spawn(async move {
                        loop {
                            if *rx.borrow() {
                                kill_process_tree(pid);
                                return;
                            }
                            if rx.changed().await.is_err() {
                                return;
                            }
                        }
                    }))
                }
                _ => None,
            };

            // Wait for the process, forwarding every stdout/stderr chunk to
            // `on_data` (pi attaches `onData` to both streams).
            let wait_result = wait_for_child_process(&mut child, |_src, data| on_data(data)).await;

            if let Some(task) = timeout_task {
                task.abort();
            }
            if let Some(task) = abort_task {
                task.abort();
            }
            if let Some(pid) = pid {
                untrack_detached_child_pid(pid);
            }

            let exit_code = wait_result.map_err(|e| BashError::Spawn(e.to_string()))?;

            // pi order: aborted takes precedence over timed-out.
            if let Some(sig) = &signal {
                if *sig.borrow() {
                    return Err(BashError::Aborted);
                }
            }
            if timed_out.load(Ordering::SeqCst) {
                let secs = timeout.map(js_number_to_string).unwrap_or_default();
                return Err(BashError::Timeout(secs));
            }

            Ok(BashExecResult { exit_code })
        })
    }
}

/// Details attached to a bash result (pi's `BashToolDetails`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashToolDetails {
    /// Truncation accounting, present only when the output was truncated.
    pub truncation: Option<super::truncate::TruncationResult>,
    /// Path to the persisted full output, when one was written.
    pub full_output_path: Option<String>,
}

/// A successful bash tool result (pi's `execute` return value). `content` is
/// the raw text (ANSI intact, truncation footer appended) — apply
/// [`get_text_output`] to materialize it for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashToolResult {
    /// The raw output text with the truncation footer (if any) appended.
    pub content: String,
    /// Truncation details, when the output was truncated.
    pub details: Option<BashToolDetails>,
}

/// A streaming update delivered to the `on_update` callback (pi's `onUpdate`
/// argument). The initial update pi sends before execution carries no content
/// block (`content == None`); subsequent updates carry the current snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashUpdate {
    /// The current output snapshot, or `None` for the initial empty update.
    pub content: Option<String>,
    /// Truncation details for the snapshot, when applicable.
    pub details: Option<BashToolDetails>,
}

/// Callback type for streaming updates.
pub type OnUpdate = Box<dyn FnMut(BashUpdate)>;

/// Options for [`create_bash_tool`], mirroring pi's `BashToolOptions`. A custom
/// `operations` backend can be injected here (pi's `options.operations`); when
/// absent, the default local-shell backend is built from `shell_path`.
#[derive(Default)]
pub struct BashToolOptions {
    /// Custom command-execution backend (pi's `operations`). When `None`, a
    /// [`LocalBashOperations`] built from `shell_path` is used.
    pub operations: Option<Arc<dyn BashOperations>>,
    /// Command prefix prepended to every command (pi's `commandPrefix`).
    pub command_prefix: Option<String>,
    /// Explicit shell path (pi's `shellPath`). Ignored when `operations` is set.
    pub shell_path: Option<String>,
    /// Hook to adjust command/cwd/env before execution (pi's `spawnHook`).
    pub spawn_hook: Option<BashSpawnHook>,
}

/// The bash tool: an execution backend plus the streaming/truncation/exit-code
/// `execute` layer (pi's `createBashToolDefinition`). The backend is held as
/// `Arc<dyn BashOperations>` so the tool is non-generic and a custom backend can
/// be injected through [`BashToolOptions::operations`].
pub struct BashTool {
    cwd: String,
    ops: Arc<dyn BashOperations>,
    command_prefix: Option<String>,
    spawn_hook: Option<BashSpawnHook>,
}

impl BashTool {
    /// Construct a bash tool with an explicit operations backend (pi's
    /// `options.operations`). Accepts any concrete backend and stores it as
    /// `Arc<dyn BashOperations>`; use [`create_bash_tool`] to inject an
    /// already-boxed `Arc<dyn BashOperations>`.
    pub fn new(cwd: impl Into<String>, ops: impl BashOperations + 'static) -> Self {
        Self {
            cwd: cwd.into(),
            ops: Arc::new(ops),
            command_prefix: None,
            spawn_hook: None,
        }
    }

    /// Set the command prefix (pi's `commandPrefix`).
    pub fn with_command_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.command_prefix = Some(prefix.into());
        self
    }

    /// Set the spawn hook (pi's `spawnHook`).
    pub fn with_spawn_hook(mut self, hook: BashSpawnHook) -> Self {
        self.spawn_hook = Some(hook);
        self
    }

    /// Execute a bash command, mirroring pi's `execute`.
    ///
    /// Streams output into an [`OutputAccumulator`] (temp-file sink prefixed
    /// `pi-bash`), throttles `on_update` snapshots to ~100ms, composes the final
    /// snapshot with pi's truncation footer, and maps the outcome to pi's exact
    /// surfaces:
    /// - non-zero exit -> `Err` ending in `Command exited with code <N>`;
    /// - abort -> `Err` ending in `Command aborted`;
    /// - timeout -> `Err` ending in `Command timed out after <secs> seconds`;
    /// - other exec errors (invalid timeout, missing cwd, spawn) -> `Err` with
    ///   the raw [`BashError::message`].
    ///
    /// A signalled exit (`exit_code == None`) is treated as success, matching
    /// pi's `exitCode !== 0 && exitCode !== null`.
    pub async fn execute(
        &self,
        command: &str,
        timeout: Option<f64>,
        signal: Option<watch::Receiver<bool>>,
        mut on_update: Option<OnUpdate>,
    ) -> Result<BashToolResult, String> {
        let resolved_command = match &self.command_prefix {
            Some(prefix) => format!("{prefix}\n{command}"),
            None => command.to_string(),
        };
        let spawn_context =
            resolve_spawn_context(resolved_command, self.cwd.clone(), self.spawn_hook.as_ref());

        let mut output = OutputAccumulator::new(OutputAccumulatorOptions::default());
        output.set_sink(Box::new(TempFileSink::new("pi-bash")));

        // Throttle bookkeeping (pi's updateDirty / lastUpdateAt / updateTimer).
        let mut update_dirty = false;
        let mut last_update_at: Option<tokio::time::Instant> = None;
        let mut next_deadline: Option<tokio::time::Instant> = None;

        // pi emits an initial empty update before execution starts.
        if let Some(cb) = on_update.as_mut() {
            cb(BashUpdate {
                content: None,
                details: None,
            });
        }

        // Chunks flow through a channel so throttled updates and the timer can
        // run concurrently with the backend's `exec` future in this one task.
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let exec_opts = BashExecOptions {
            on_data: Box::new(move |data: &[u8]| {
                let _ = tx.send(data.to_vec());
            }),
            signal,
            timeout,
            // Move the assembled env in (partial move); `command`/`cwd` are still
            // borrowed by `exec` below, which a partial move leaves valid.
            env: Some(spawn_context.env),
        };

        // `exec` returns an already-boxed `Pin<Box<dyn Future>>`, which is
        // `Unpin`, so `&mut exec_fut` is directly pollable in the `select!`.
        let mut exec_fut = self
            .ops
            .exec(&spawn_context.command, &spawn_context.cwd, exec_opts);

        let exec_result: Result<BashExecResult, BashError> = loop {
            let deadline = next_deadline;
            tokio::select! {
                biased;

                res = &mut exec_fut => {
                    break res;
                }

                maybe_chunk = rx.recv() => {
                    if let Some(data) = maybe_chunk {
                        output.append(&data);
                        if on_update.is_some() {
                            update_dirty = true;
                            let delay = match last_update_at {
                                None => 0i64,
                                Some(t) => BASH_UPDATE_THROTTLE_MS
                                    - (tokio::time::Instant::now().duration_since(t).as_millis()
                                        as i64),
                            };
                            if delay <= 0 {
                                next_deadline = None;
                                emit_update(
                                    &mut output,
                                    &mut on_update,
                                    &mut update_dirty,
                                    &mut last_update_at,
                                );
                            } else if next_deadline.is_none() {
                                next_deadline = Some(
                                    tokio::time::Instant::now()
                                        + Duration::from_millis(delay as u64),
                                );
                            }
                        }
                    }
                }

                _ = async move {
                    match deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    next_deadline = None;
                    emit_update(
                        &mut output,
                        &mut on_update,
                        &mut update_dirty,
                        &mut last_update_at,
                    );
                }
            }
        };

        // Drain any chunks buffered before `exec` resolved. For every backend
        // in this repo, `exec` returns only after all output has been forwarded,
        // so this collects the complete tail.
        while let Ok(data) = rx.try_recv() {
            output.append(&data);
        }

        // finishOutput: flush the decoder, force a final pending update, snapshot.
        output.finish();
        emit_update(
            &mut output,
            &mut on_update,
            &mut update_dirty,
            &mut last_update_at,
        );
        let snapshot = output.snapshot(true);

        match exec_result {
            Ok(result) => {
                let (text, details) = format_output(&snapshot, &output, "(no output)");
                if let Some(code) = result.exit_code {
                    if code != 0 {
                        return Err(append_status(
                            &text,
                            &format!("Command exited with code {code}"),
                        ));
                    }
                }
                Ok(BashToolResult {
                    content: text,
                    details,
                })
            }
            Err(err) => {
                let (text, _details) = format_output(&snapshot, &output, "");
                match err {
                    BashError::Aborted => Err(append_status(&text, "Command aborted")),
                    BashError::Timeout(secs) => Err(append_status(
                        &text,
                        &format!("Command timed out after {secs} seconds"),
                    )),
                    other => Err(other.message()),
                }
            }
        }
    }
}

/// Convenience constructor for the bash tool (pi's `createBashTool`). Uses the
/// injected [`BashToolOptions::operations`] backend when present, otherwise a
/// default local-shell backend built from `shell_path`.
pub fn create_bash_tool(cwd: impl Into<String>, options: Option<BashToolOptions>) -> BashTool {
    let BashToolOptions {
        operations,
        command_prefix,
        shell_path,
        spawn_hook,
    } = options.unwrap_or_default();
    let ops: Arc<dyn BashOperations> =
        operations.unwrap_or_else(|| Arc::new(create_local_bash_operations(shell_path)));
    BashTool {
        cwd: cwd.into(),
        ops,
        command_prefix,
        spawn_hook,
    }
}

/// Emit a throttled update if one is pending (pi's `emitOutputUpdate`).
fn emit_update(
    output: &mut OutputAccumulator,
    on_update: &mut Option<OnUpdate>,
    update_dirty: &mut bool,
    last_update_at: &mut Option<tokio::time::Instant>,
) {
    let Some(cb) = on_update.as_mut() else {
        return;
    };
    if !*update_dirty {
        return;
    }
    *update_dirty = false;
    *last_update_at = Some(tokio::time::Instant::now());
    let snapshot = output.snapshot(true);
    let details = Some(BashToolDetails {
        truncation: snapshot
            .truncation
            .truncated
            .then(|| snapshot.truncation.clone()),
        full_output_path: snapshot.full_output_path.clone(),
    });
    cb(BashUpdate {
        content: Some(snapshot.content),
        details,
    });
}

/// Compose the final output text and details, reproducing pi's `formatOutput`
/// truncation footer verbatim.
fn format_output(
    snapshot: &OutputSnapshot,
    output: &OutputAccumulator,
    empty_text: &str,
) -> (String, Option<BashToolDetails>) {
    let truncation = &snapshot.truncation;
    // JS `snapshot.content || emptyText`: empty string is falsy.
    let mut text = if snapshot.content.is_empty() {
        empty_text.to_string()
    } else {
        snapshot.content.clone()
    };
    let mut details = None;

    if truncation.truncated {
        details = Some(BashToolDetails {
            truncation: Some(truncation.clone()),
            full_output_path: snapshot.full_output_path.clone(),
        });
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        let full = snapshot.full_output_path.clone().unwrap_or_default();

        if truncation.last_line_partial {
            let last_line_size = format_size(output.last_line_bytes());
            text += &format!(
                "\n\n[Showing last {} of line {} (line is {}). Full output: {}]",
                format_size(truncation.output_bytes),
                end_line,
                last_line_size,
                full
            );
        } else if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text += &format!(
                "\n\n[Showing lines {}-{} of {}. Full output: {}]",
                start_line, end_line, truncation.total_lines, full
            );
        } else {
            text += &format!(
                "\n\n[Showing lines {}-{} of {} ({} limit). Full output: {}]",
                start_line,
                end_line,
                truncation.total_lines,
                format_size(DEFAULT_MAX_BYTES),
                full
            );
        }
    }

    (text, details)
}

/// Compose text with a trailing status line, mirroring pi's `appendStatus`.
fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Injected-fake tests (deterministic, no real subprocess) ----

    /// A canned [`BashOperations`] that emits scripted chunks (each after an
    /// optional delay) then returns a fixed exit code. Proves the seam is
    /// pluggable and drives the `execute` layer without a real shell.
    struct FakeOps {
        chunks: Vec<(u64, Vec<u8>)>,
        exit_code: Option<i32>,
    }

    impl BashOperations for FakeOps {
        fn exec<'a>(
            &'a self,
            _command: &'a str,
            _cwd: &'a str,
            mut opts: BashExecOptions,
        ) -> Pin<Box<dyn Future<Output = Result<BashExecResult, BashError>> + 'a>> {
            Box::pin(async move {
                for (delay_ms, data) in &self.chunks {
                    if *delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
                    }
                    (opts.on_data)(data);
                }
                Ok(BashExecResult {
                    exit_code: self.exit_code,
                })
            })
        }
    }

    /// Drive the fake backend end-to-end: build a [`BashTool`] over a [`FakeOps`]
    /// that emits `chunks` then returns `exit_code`, and run `execute("noop", …)`
    /// with no timeout, signal, or update sink. Centralizes the tool-construction
    /// and `execute` boilerplate the scripted-chunk tests would otherwise repeat.
    async fn run_fake(
        chunks: Vec<(u64, Vec<u8>)>,
        exit_code: Option<i32>,
    ) -> Result<BashToolResult, String> {
        BashTool::new("/tmp", FakeOps { chunks, exit_code })
            .execute("noop", None, None, None)
            .await
    }

    #[tokio::test]
    async fn fake_accumulates_output_and_zero_exit() {
        let result = run_fake(
            vec![(0, b"hello ".to_vec()), (0, b"world\n".to_vec())],
            Some(0),
        )
        .await
        .unwrap();
        // The accumulator preserves the trailing newline, matching pi.
        assert_eq!(result.content, "hello world\n");
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn fake_maps_nonzero_exit_to_message() {
        let err = run_fake(vec![(0, b"some output\n".to_vec())], Some(7))
            .await
            .unwrap_err();
        // Output keeps its trailing newline; the status is appended after "\n\n".
        assert_eq!(err, "some output\n\n\nCommand exited with code 7");
    }

    #[tokio::test]
    async fn fake_signalled_exit_is_success() {
        // exitCode == null is treated as success (pi's `!== 0 && !== null`).
        let result = run_fake(vec![(0, b"partial".to_vec())], None)
            .await
            .unwrap();
        assert_eq!(result.content, "partial");
    }

    #[tokio::test]
    async fn fake_truncation_footer_matches_pi() {
        // 2001 lines exceeds the 2000-line default -> truncated by lines.
        let data = "x\n".repeat(2001);
        let result = run_fake(vec![(0, data.into_bytes())], Some(0))
            .await
            .unwrap();

        let details = result.details.expect("truncated output has details");
        let path = details
            .full_output_path
            .as_deref()
            .expect("truncated output persists a full-output path");
        // Footer: lines 2..2001 of 2001 (last 2000 shown), naming the temp file.
        let expected_footer = format!("\n\n[Showing lines 2-2001 of 2001. Full output: {path}]");
        assert!(
            result.content.ends_with(&expected_footer),
            "content tail was: {:?}",
            &result.content[result.content.len().saturating_sub(120)..]
        );

        // The persisted temp file exists with the `pi-bash-` prefix.
        let p = std::path::Path::new(path);
        assert!(p.exists(), "temp file should exist at {p:?}");
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("pi-bash-"), "name was {name}");
        assert!(name.ends_with(".log"), "name was {name}");
        std::fs::remove_file(p).unwrap();
    }

    #[tokio::test]
    async fn fake_throttles_updates_to_100ms() {
        // Six chunks 15ms apart (~90ms total, under the 100ms window) must
        // coalesce: fewer content-bearing updates than chunks, with a leading
        // emit and a correct final snapshot.
        let chunks: Vec<(u64, Vec<u8>)> = (0..6)
            .map(|i| (15u64, format!("chunk{i}\n").into_bytes()))
            .collect();
        let ops = FakeOps {
            chunks,
            exit_code: Some(0),
        };
        let tool = BashTool::new("/tmp", ops);

        let updates: Arc<std::sync::Mutex<Vec<Option<String>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&updates);
        let on_update: OnUpdate = Box::new(move |u: BashUpdate| {
            sink.lock().unwrap().push(u.content);
        });

        let result = tool
            .execute("noop", None, None, Some(on_update))
            .await
            .unwrap();
        assert_eq!(
            result.content,
            "chunk0\nchunk1\nchunk2\nchunk3\nchunk4\nchunk5\n"
        );

        let recorded = updates.lock().unwrap().clone();
        // First update is the initial empty one (content: None).
        assert_eq!(recorded[0], None);
        // Content-bearing updates coalesced to fewer than the six chunks.
        let content_updates = recorded.iter().filter(|c| c.is_some()).count();
        assert!(content_updates >= 1, "expected at least one content update");
        assert!(
            content_updates < 6,
            "expected throttling to coalesce updates, got {content_updates}"
        );
        // The last content-bearing update carries the full snapshot.
        let last_content = recorded
            .iter()
            .rev()
            .find_map(|c| c.clone())
            .expect("a content update");
        assert_eq!(
            last_content,
            "chunk0\nchunk1\nchunk2\nchunk3\nchunk4\nchunk5\n"
        );
    }

    #[tokio::test]
    async fn fake_decodes_utf8_split_across_chunks() {
        // pi's "should decode UTF-8 characters split across output chunks": the
        // euro sign is 3 UTF-8 bytes (E2 82 AC); split it after the first byte so
        // the streaming decoder must coalesce across `on_data` boundaries.
        let euro = "\u{20AC}\n".as_bytes().to_vec();
        assert_eq!(euro, vec![0xE2, 0x82, 0xAC, 0x0A]);
        let result = run_fake(
            vec![(0, euro[0..1].to_vec()), (0, euro[1..].to_vec())],
            Some(0),
        )
        .await
        .unwrap();
        // Materialized output is the intact euro sign (pi asserts the trimmed
        // `getTextOutput` equals "€").
        assert_eq!(get_text_output(&result.content).trim(), "\u{20AC}");
    }

    // ---- Real-subprocess integration tests (unix, hermetic) ----

    #[cfg(unix)]
    mod real {
        use super::*;
        use crate::core::tools::test_support::TempDir;
        use std::sync::Mutex;
        use std::time::Instant;

        /// Run the local backend, capturing all forwarded output bytes.
        async fn run_local(
            command: &str,
            cwd: &str,
            timeout: Option<f64>,
            signal: Option<watch::Receiver<bool>>,
        ) -> (Result<BashExecResult, BashError>, Vec<u8>) {
            let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&captured);
            let opts = BashExecOptions {
                on_data: Box::new(move |data: &[u8]| {
                    sink.lock().unwrap().extend_from_slice(data);
                }),
                signal,
                timeout,
                env: None,
            };
            let ops = create_local_bash_operations(None);
            let result = ops.exec(command, cwd, opts).await;
            let bytes = captured.lock().unwrap().clone();
            (result, bytes)
        }

        /// Run a long-running command with a 1s timeout, retrying past a
        /// concurrency hazard until we observe *our own* timeout.
        ///
        /// The detached-pid tracking set is process-global (a faithful port of
        /// pi's module-level set). `shell.rs`'s `detached_pid_tracking_roundtrip`
        /// unit test calls the destructive `kill_tracked_detached_children()` on
        /// that shared set; when it runs concurrently with a real child tracked
        /// here it reaps our child early (exit code `None`) before our timeout
        /// fires. That saboteur runs once and clears the set, so a bounded retry
        /// yields a clean, uncontended run. Returns the winning attempt's
        /// result, captured output, and elapsed time.
        async fn run_local_timeout(
            command: &str,
            cwd: &str,
        ) -> (Result<BashExecResult, BashError>, Vec<u8>, Duration) {
            for _ in 0..8 {
                let start = Instant::now();
                let (result, out) = run_local(command, cwd, Some(1.0), None).await;
                let elapsed = start.elapsed();
                if matches!(result, Err(BashError::Timeout(_))) {
                    return (result, out, elapsed);
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            panic!("never observed our own timeout across retries");
        }

        #[tokio::test]
        async fn echo_hello_stdout_and_zero_exit() {
            let tmp = TempDir::new("bash-echo");
            let (result, out) = run_local("echo hello", tmp.cwd(), None, None).await;
            assert_eq!(result.unwrap().exit_code, Some(0));
            assert!(String::from_utf8_lossy(&out).contains("hello"));
        }

        #[tokio::test]
        async fn stderr_is_captured() {
            let tmp = TempDir::new("bash-stderr");
            let (result, out) = run_local("echo err 1>&2", tmp.cwd(), None, None).await;
            assert_eq!(result.unwrap().exit_code, Some(0));
            assert!(String::from_utf8_lossy(&out).contains("err"));
        }

        #[tokio::test]
        async fn exit_code_message_matches_pi() {
            let tmp = TempDir::new("bash-exit3");
            let tool = BashTool::new(tmp.cwd(), create_local_bash_operations(None));
            let err = tool.execute("exit 3", None, None, None).await.unwrap_err();
            // No output -> "(no output)" empty-text, then the status is appended.
            assert_eq!(err, "(no output)\n\nCommand exited with code 3");
        }

        #[tokio::test]
        async fn timeout_surfaces_pi_format_and_returns_promptly() {
            let tmp = TempDir::new("bash-timeout");
            let (result, _out, elapsed) = run_local_timeout("sleep 5", tmp.cwd()).await;
            assert!(
                matches!(result, Err(BashError::Timeout(ref s)) if s == "1"),
                "expected timeout:1, got {result:?}"
            );
            assert!(
                elapsed < Duration::from_secs(4),
                "should return well under the 5s sleep, took {elapsed:?}"
            );
        }

        #[tokio::test]
        async fn ansi_is_stripped_at_materialization() {
            let tmp = TempDir::new("bash-ansi");
            let tool = BashTool::new(tmp.cwd(), create_local_bash_operations(None));
            let result = tool
                .execute("printf '\\033[31mred\\033[0m'", None, None, None)
                .await
                .unwrap();
            // Raw content keeps the escape (byte-exact with pi's execute).
            assert!(result.content.contains('\u{1b}'), "raw ESC preserved");
            // Materialized output strips ANSI (byte-exact with pi getTextOutput).
            assert_eq!(get_text_output(&result.content), "red");
        }

        /// THE §6.2 guarantee: killing the process group reaps the whole tree,
        /// not just the shell. Background a long-lived grandchild, capture its
        /// pid, let the timeout fire, and assert the grandchild is dead.
        #[tokio::test]
        async fn timeout_reaps_whole_process_tree() {
            let tmp = TempDir::new("bash-reap");
            // The shell backgrounds `sleep 30`, prints its pid, then `wait`s —
            // so the shell stays alive until the timeout kills the group.
            let (result, out, elapsed) =
                run_local_timeout("sleep 30 & echo $!; wait", tmp.cwd()).await;
            assert!(
                elapsed < Duration::from_secs(10),
                "timeout should terminate promptly"
            );
            assert!(
                matches!(result, Err(BashError::Timeout(ref s)) if s == "1"),
                "expected timeout:1, got {result:?}"
            );

            let stdout = String::from_utf8_lossy(&out);
            let grandchild: i32 = stdout
                .lines()
                .next()
                .expect("grandchild pid on stdout")
                .trim()
                .parse()
                .expect("pid parses");

            // Poll until the grandchild is gone (ESRCH). killpg is asynchronous
            // w.r.t. this process, so give it a short window.
            use nix::sys::signal::kill;
            use nix::unistd::Pid;
            let mut dead = false;
            for _ in 0..100 {
                match kill(Pid::from_raw(grandchild), None) {
                    Err(nix::errno::Errno::ESRCH) => {
                        dead = true;
                        break;
                    }
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
            assert!(
                dead,
                "grandchild {grandchild} should have been reaped by killpg"
            );
        }

        #[tokio::test]
        async fn abort_reaps_and_surfaces_aborted() {
            let tmp = TempDir::new("bash-abort");
            // Retry past the shared-global reaper hazard (see `run_local_timeout`)
            // until we observe our *own* abort rather than an external reap.
            let mut outcome = None;
            for _ in 0..8 {
                let (tx, rx) = watch::channel(false);
                // Abort shortly after launch.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let _ = tx.send(true);
                });
                let start = Instant::now();
                let (result, _out) = run_local("sleep 30", tmp.cwd(), None, Some(rx)).await;
                if matches!(result, Err(BashError::Aborted)) {
                    outcome = Some((result, start.elapsed()));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            let (result, elapsed) = outcome.expect("observed our own abort");
            assert!(elapsed < Duration::from_secs(10));
            assert!(
                matches!(result, Err(BashError::Aborted)),
                "expected aborted, got {result:?}"
            );
        }

        #[tokio::test]
        async fn missing_cwd_surfaces_pi_message() {
            let ops = create_local_bash_operations(None);
            let opts = BashExecOptions {
                on_data: Box::new(|_| {}),
                signal: None,
                timeout: None,
                env: None,
            };
            let missing = "/nonexistent/pidgin/bash/cwd/xyz";
            let err = ops.exec("echo hi", missing, opts).await.unwrap_err();
            assert_eq!(
                err.message(),
                format!(
                    "Working directory does not exist: {missing}\nCannot execute bash commands."
                )
            );
        }
    }
}
