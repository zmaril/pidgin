//! Child-process spawning and exit supervision.
//!
//! Ported from pi's `utils/child-process.ts`. The load-bearing piece is
//! [`wait_for_child_process`], which resolves a child's exit code without
//! hanging on inherited stdio held by detached descendants.
//!
//! ## The pi#5303 fix
//!
//! A short-lived child can *exit* while a detached descendant keeps its
//! stdout/stderr pipe open. If we resolved and destroyed the streams on a fixed
//! deadline measured from `exit`, output still being written past that deadline
//! would be silently lost (truncated tails —
//! `earendil-works/pi#5303`). Instead, after the process exits we wait for the
//! pipes to fall *idle*: a grace timer ([`EXIT_STDIO_GRACE_MS`]) is re-armed on
//! every chunk, so an actively writing descendant keeps us reading, while a
//! quiet inherited handle (e.g. a daemonized descendant whose `close` never
//! fires) still releases us after the grace elapses. If both pipes reach EOF
//! after exit, we finalize immediately (pi's `close` / `maybeFinalizeAfterExit`
//! path).
//!
//! Rust adaptation: pi attaches extra `data` listeners to Node streams (which
//! are multi-consumer `EventEmitter`s). tokio pipes are single-consumer, so
//! this function *owns* the child's `stdout`/`stderr`, reads them itself, and
//! forwards every chunk to a caller-supplied sink. That keeps the grace-timer
//! state machine and the output delivery in one place.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

/// Grace window: after the process exits, wait until no pipe data has arrived
/// for this long before finalizing. Re-armed on every chunk. Matches pi's
/// `EXIT_STDIO_GRACE_MS = 100`.
pub const EXIT_STDIO_GRACE_MS: u64 = 100;

/// Which pipe a forwarded chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkSource {
    /// The child's standard output.
    Stdout,
    /// The child's standard error.
    Stderr,
}

/// Wait for a child process to terminate without hanging on inherited stdio.
///
/// Reads the child's `stdout`/`stderr` (whichever were configured as pipes),
/// forwarding every chunk to `on_chunk` as it arrives, and returns the process
/// exit code once the process has exited *and* the pipes have gone idle for
/// [`EXIT_STDIO_GRACE_MS`] (or both reached EOF). See the module docs for the
/// pi#5303 rationale.
///
/// `on_chunk` is invoked from this single reading task, so a `FnMut` is safe.
/// The returned code is `None` when the process was terminated by a signal
/// without an exit code (mirroring pi resolving `number | null`).
pub async fn wait_for_child_process<F>(
    child: &mut Child,
    mut on_chunk: F,
) -> std::io::Result<Option<i32>>
where
    F: FnMut(ChunkSource, &[u8]),
{
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdout_buf = [0u8; 8192];
    let mut stderr_buf = [0u8; 8192];

    let mut stdout_ended = stdout.is_none();
    let mut stderr_ended = stderr.is_none();
    let mut exited = false;
    let mut exit_code: Option<i32> = None;

    // Idle timer: only meaningful after exit. `enabled` gates the select branch
    // so it never fires before the process has exited.
    let idle = tokio::time::sleep(Duration::from_millis(EXIT_STDIO_GRACE_MS));
    tokio::pin!(idle);
    let mut idle_enabled = false;

    loop {
        // Finalize once the process has exited and both pipes have drained
        // (pi's `maybeFinalizeAfterExit` / `onClose`).
        if exited && stdout_ended && stderr_ended {
            break;
        }

        tokio::select! {
            biased;

            // Process exit.
            status = child.wait(), if !exited => {
                let status = status?;
                exited = true;
                exit_code = status.code();
                // Arm the idle timer; if the pipes are already drained the
                // loop head will finalize on the next iteration instead.
                idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                idle_enabled = true;
            }

            // stdout data.
            r = read_opt(&mut stdout, &mut stdout_buf), if !stdout_ended => {
                match r {
                    Ok(0) => stdout_ended = true,
                    Ok(n) => {
                        on_chunk(ChunkSource::Stdout, &stdout_buf[..n]);
                        if exited {
                            // Output still arriving after exit: defer finalize.
                            idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            idle_enabled = true;
                        }
                    }
                    Err(_) => stdout_ended = true,
                }
            }

            // stderr data.
            r = read_opt(&mut stderr, &mut stderr_buf), if !stderr_ended => {
                match r {
                    Ok(0) => stderr_ended = true,
                    Ok(n) => {
                        on_chunk(ChunkSource::Stderr, &stderr_buf[..n]);
                        if exited {
                            idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            idle_enabled = true;
                        }
                    }
                    Err(_) => stderr_ended = true,
                }
            }

            // Grace window elapsed after exit with the pipes quiet: release.
            _ = &mut idle, if idle_enabled && exited => {
                break;
            }
        }
    }

    Ok(exit_code)
}

/// Read into `buf` from an optional reader. When `None`, parks forever — the
/// caller gates the corresponding select branch with an `ended` flag, so this
/// fallback is never actually polled to completion.
async fn read_opt<R: AsyncReadExt + Unpin>(
    reader: &mut Option<R>,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    match reader {
        Some(inner) => inner.read(buf).await,
        None => std::future::pending().await,
    }
}

/// Thin async spawn helper (pi's `spawnProcess`).
///
/// Builds a [`tokio::process::Command`] for `command`/`args`, lets the caller
/// configure it (stdio, cwd, env, process group, …) via `configure`, and
/// spawns it. Windows `cross-spawn` argument-quoting parity is deferred; on
/// Windows this uses std/tokio's default argument handling.
pub fn spawn_process<F>(command: &str, args: &[String], configure: F) -> std::io::Result<Child>
where
    F: FnOnce(&mut Command),
{
    let mut cmd = Command::new(command);
    cmd.args(args);
    configure(&mut cmd);
    cmd.spawn()
}

/// Thin synchronous spawn helper (pi's `spawnProcessSync`).
///
/// Runs `command`/`args` to completion and returns its captured
/// [`std::process::Output`]. `configure` may adjust stdio, cwd, env, etc. As
/// with [`spawn_process`], Windows `cross-spawn` parity is deferred.
pub fn spawn_process_sync<F>(
    command: &str,
    args: &[String],
    configure: F,
) -> std::io::Result<std::process::Output>
where
    F: FnOnce(&mut std::process::Command),
{
    let mut cmd = std::process::Command::new(command);
    cmd.args(args);
    configure(&mut cmd);
    cmd.output()
}

/// Configure a command's stdio as `[null, pipe, pipe]` (stdin ignored,
/// stdout/stderr captured) — the shape [`wait_for_child_process`] expects.
/// Small convenience mirroring pi's `StdioNull, StdioPipe, StdioPipe` tuple.
pub fn pipe_stdout_stderr(cmd: &mut Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

#[cfg(test)]
mod tests {
    use super::*;

    type SharedBuf = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    fn collect() -> (SharedBuf, SharedBuf) {
        (
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        )
    }

    /// Drive `child` to completion, funnelling stdout/stderr into buffers.
    /// Returns `(exit_code, stdout_bytes, stderr_bytes)`.
    async fn wait_and_collect(child: &mut Child) -> (Option<i32>, Vec<u8>, Vec<u8>) {
        let (out, err) = collect();
        let (o, e) = (out.clone(), err.clone());
        let code = wait_for_child_process(child, move |src, data| match src {
            ChunkSource::Stdout => o.lock().unwrap().extend_from_slice(data),
            ChunkSource::Stderr => e.lock().unwrap().extend_from_slice(data),
        })
        .await
        .unwrap();
        let out = out.lock().unwrap().clone();
        let err = err.lock().unwrap().clone();
        (code, out, err)
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let mut child = spawn_process("sh", &["-c".into(), "printf hello".into()], |c| {
            pipe_stdout_stderr(c);
        })
        .unwrap();
        let (code, out, err) = wait_and_collect(&mut child).await;
        assert_eq!(code, Some(0));
        assert_eq!(out, b"hello");
        assert!(err.is_empty());
    }

    #[tokio::test]
    async fn captures_stderr_and_nonzero_exit() {
        let mut child = spawn_process(
            "sh",
            &["-c".into(), "printf oops 1>&2; exit 3".into()],
            pipe_stdout_stderr,
        )
        .unwrap();
        let (code, _out, err) = wait_and_collect(&mut child).await;
        assert_eq!(code, Some(3));
        assert_eq!(err, b"oops");
    }

    #[tokio::test]
    async fn captures_tail_written_after_parent_exits() {
        // The parent `sh` exits immediately, but a backgrounded descendant
        // inherits the stdout pipe and writes shortly after. Without the
        // grace-timer fix (pi#5303) this tail would be truncated. The 20ms
        // descendant delay sits comfortably inside the 100ms grace window.
        let mut child = spawn_process(
            "sh",
            &[
                "-c".into(),
                "printf head; { sleep 0.02; printf tail; } &".into(),
            ],
            pipe_stdout_stderr,
        )
        .unwrap();
        let (code, out, _err) = wait_and_collect(&mut child).await;
        assert_eq!(code, Some(0));
        assert_eq!(out, b"headtail");
    }

    #[test]
    fn spawn_process_sync_captures_output() {
        let output =
            spawn_process_sync("sh", &["-c".into(), "printf sync-ok".into()], |_| {}).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"sync-ok");
    }
}
