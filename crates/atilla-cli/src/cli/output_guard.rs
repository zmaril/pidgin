//! Stdout takeover / cleanliness guard.
//!
//! Mirrors pi's `packages/coding-agent/src/core/output-guard.ts`. In json/print
//! modes pi replaces `process.stdout.write` so that everything written to
//! "stdout" is redirected to stderr, keeping stdout clean for the structured
//! payload. The Rust CLI owns all of its own output, so instead of patching a
//! global stream we route every "console.log"-equivalent write through
//! [`out_line`], which honors the takeover flag. "console.error"-equivalent
//! writes always go to stderr via [`err_line`].

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static TAKEN_OVER: AtomicBool = AtomicBool::new(false);

/// Redirect subsequent stdout-destined writes to stderr. Mirrors `takeOverStdout()`.
pub fn take_over_stdout() {
    TAKEN_OVER.store(true, Ordering::SeqCst);
}

/// Whether stdout has been taken over. Mirrors `isStdoutTakenOver()`.
pub fn is_stdout_taken_over() -> bool {
    TAKEN_OVER.load(Ordering::SeqCst)
}

/// Write a line to stdout — or to stderr if stdout has been taken over.
/// This is the equivalent of pi's `console.log` under the output guard.
pub fn out_line(text: &str) {
    if is_stdout_taken_over() {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{text}");
    } else {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
    }
}

/// Write a line to stderr unconditionally. Equivalent to pi's `console.error`.
pub fn err_line(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{text}");
}
