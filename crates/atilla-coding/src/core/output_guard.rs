//! Stdout takeover guard for protocol-clean output.
//!
//! Ported from pi's `core/output-guard.ts`. pi runs a machine-readable
//! protocol on `stdout`; libraries that write to `stdout` directly would
//! corrupt it. The guard "takes over" `stdout` by rerouting stray writes to
//! `stderr`, and reserves the real `stdout` for deliberate raw writes. It also
//! retries transient, backpressure-style write errors instead of dropping
//! output.
//!
//! # Seams and deferrals
//!
//! pi manipulates the Node globals `process.stdout` / `process.stderr` and
//! serializes raw writes through a promise chain
//! (`rawStdoutWriteTail`/`waitForRawStdoutBackpressure`/`flushRawStdout`),
//! calling `process.exit(1)` on unrecoverable failure. Those depend on the
//! Node event loop and process lifecycle and are **deferred**. This port keeps
//! the reviewable, runtime-agnostic core:
//!
//! - the idempotent takeover state machine ([`OutputGuard::take_over`] /
//!   [`OutputGuard::restore`] / [`OutputGuard::is_taken_over`]),
//! - the stdout→stderr rerouting while taken over,
//! - the transient-error retry loop ([`OutputGuard::write_stdout`]) and its
//!   classifier ([`is_transient_write_error`]).
//!
//! The concrete sinks are injected via [`ByteSink`] rather than reaching for
//! process globals, so the state machine is unit-testable.

use std::io;

/// A destination for text chunks. Mirrors the single capability the guard
/// needs from `process.stdout` / `process.stderr`: write a string, reporting
/// transient backpressure as an [`io::Error`].
pub trait ByteSink {
    /// Write `chunk`, returning an error on failure. Transient backpressure
    /// errors (see [`is_transient_write_error`]) trigger a retry.
    fn write_chunk(&mut self, chunk: &str) -> io::Result<()>;
}

/// Classify a write error as transient backpressure worth retrying.
///
/// Mirrors pi's check for `ENOBUFS` / `EAGAIN` / `EWOULDBLOCK`. On this
/// platform `EAGAIN` and `EWOULDBLOCK` share errno 11 and both surface as
/// [`io::ErrorKind::WouldBlock`]; `ENOBUFS` is errno 105.
pub fn is_transient_write_error(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    // ENOBUFS (105), EAGAIN/EWOULDBLOCK (11) by raw errno, for platforms/paths
    // that do not map them onto `WouldBlock`.
    matches!(error.raw_os_error(), Some(11) | Some(105))
}

/// Guards `stdout` by rerouting stray writes to `stderr` while taken over.
///
/// Generic over the two sinks so tests can inject fakes; in production these
/// wrap the real standard streams.
pub struct OutputGuard<Out: ByteSink, Err: ByteSink> {
    stdout: Out,
    stderr: Err,
    taken_over: bool,
}

impl<Out: ByteSink, Err: ByteSink> OutputGuard<Out, Err> {
    /// Create a guard over the given sinks, initially not taken over.
    pub fn new(stdout: Out, stderr: Err) -> Self {
        OutputGuard {
            stdout,
            stderr,
            taken_over: false,
        }
    }

    /// Whether stdout is currently taken over. Port of `isStdoutTakenOver`.
    pub fn is_taken_over(&self) -> bool {
        self.taken_over
    }

    /// Take over stdout so stray writes route to stderr. Idempotent: returns
    /// `true` only on the transition, `false` if already taken over. Port of
    /// `takeOverStdout`.
    pub fn take_over(&mut self) -> bool {
        if self.taken_over {
            return false;
        }
        self.taken_over = true;
        true
    }

    /// Restore stdout. Idempotent: returns `true` only on the transition,
    /// `false` if not taken over. Port of `restoreStdout`.
    pub fn restore(&mut self) -> bool {
        if !self.taken_over {
            return false;
        }
        self.taken_over = false;
        true
    }

    /// Route a stray stdout write. While taken over it goes to stderr;
    /// otherwise straight to stdout. Empty writes are dropped, matching pi's
    /// `writeRawStdout` early return. Transient backpressure errors are
    /// retried; any other error propagates.
    pub fn write_stdout(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self.taken_over {
            write_with_retry(&mut self.stderr, text)
        } else {
            write_with_retry(&mut self.stdout, text)
        }
    }

    /// Deliberately write to the real stdout, bypassing the reroute. Retries
    /// transient errors. This is the raw-write path pi reserves for protocol
    /// output.
    pub fn write_raw_stdout(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        write_with_retry(&mut self.stdout, text)
    }
}

/// Write `text` to `sink`, retrying while the error is transient backpressure.
/// Mirrors pi's `writeRawStdoutChunk` retry loop (minus the async 10ms delay,
/// which has no synchronous analogue).
fn write_with_retry(sink: &mut impl ByteSink, text: &str) -> io::Result<()> {
    loop {
        match sink.write_chunk(text) {
            Ok(()) => return Ok(()),
            Err(err) if is_transient_write_error(&err) => continue,
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink that records what it received and can be programmed to fail a
    /// number of times (with a chosen error) before succeeding.
    #[derive(Default)]
    struct RecordingSink {
        written: Vec<String>,
        fail_times: usize,
        fail_kind: Option<io::ErrorKind>,
        fail_errno: Option<i32>,
    }

    impl RecordingSink {
        fn joined(&self) -> String {
            self.written.concat()
        }
    }

    impl ByteSink for RecordingSink {
        fn write_chunk(&mut self, chunk: &str) -> io::Result<()> {
            if self.fail_times > 0 {
                self.fail_times -= 1;
                let err = match (self.fail_kind, self.fail_errno) {
                    (Some(kind), _) => io::Error::from(kind),
                    (None, Some(errno)) => io::Error::from_raw_os_error(errno),
                    (None, None) => io::Error::other("boom"),
                };
                return Err(err);
            }
            self.written.push(chunk.to_string());
            Ok(())
        }
    }

    fn guard() -> OutputGuard<RecordingSink, RecordingSink> {
        OutputGuard::new(RecordingSink::default(), RecordingSink::default())
    }

    #[test]
    fn take_over_and_restore_are_idempotent() {
        let mut g = guard();
        assert!(!g.is_taken_over());
        assert!(g.take_over());
        assert!(!g.take_over()); // no-op second time
        assert!(g.is_taken_over());
        assert!(g.restore());
        assert!(!g.restore()); // no-op second time
        assert!(!g.is_taken_over());
    }

    #[test]
    fn writes_go_to_stdout_until_taken_over_then_stderr() {
        let mut g = guard();
        g.write_stdout("before\n").unwrap();
        g.take_over();
        g.write_stdout("stray\n").unwrap();
        assert_eq!(g.stdout.joined(), "before\n");
        assert_eq!(g.stderr.joined(), "stray\n");
    }

    #[test]
    fn raw_write_always_targets_stdout() {
        let mut g = guard();
        g.take_over();
        g.write_raw_stdout("protocol\n").unwrap();
        assert_eq!(g.stdout.joined(), "protocol\n");
        assert_eq!(g.stderr.joined(), "");
    }

    #[test]
    fn empty_writes_are_dropped() {
        let mut g = guard();
        g.write_stdout("").unwrap();
        g.write_raw_stdout("").unwrap();
        assert_eq!(g.stdout.joined(), "");
    }

    #[test]
    fn transient_errors_are_retried_until_success() {
        let mut g = guard();
        g.stdout.fail_times = 3;
        g.stdout.fail_kind = Some(io::ErrorKind::WouldBlock);
        g.write_stdout("payload").unwrap();
        assert_eq!(g.stdout.joined(), "payload");
    }

    #[test]
    fn enobufs_errno_is_transient() {
        let e = io::Error::from_raw_os_error(105);
        assert!(is_transient_write_error(&e));
        let would_block = io::Error::from(io::ErrorKind::WouldBlock);
        assert!(is_transient_write_error(&would_block));
    }

    #[test]
    fn non_transient_errors_propagate() {
        let mut g = guard();
        g.stdout.fail_times = 1;
        g.stdout.fail_errno = Some(32); // EPIPE — not retryable
        let err = g.write_stdout("payload").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(32));
        assert!(!is_transient_write_error(&err));
    }
}
