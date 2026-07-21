//! The bash HOST seam: a `Send + Sync` handle that lets a bound host run a
//! single bash command through [`create_bash_tool`]`(...).execute()` and get a
//! plain, synchronous outcome back.
//!
//! The ext-plane runs on a dedicated worker thread that is not itself a tokio
//! runtime, and needs a *synchronous* `run(command, cwd) -> outcome` call it can
//! block on. [`create_bash_tool`]'s `execute` is `async`, so this seam is the
//! async->sync bridge: it drives the `execute` future to completion on a
//! multi-thread runtime [`Handle`](tokio::runtime::Handle) and blocks the caller
//! on an `mpsc` channel until the final result arrives. It is RUN-TO-COMPLETION:
//! the caller gets the final output only.
//!
//! This mirrors the shape of the [`super::super::extensions::notify`]
//! notify-sink seam: a `Send + Sync` trait, a channel-backed concrete impl, and
//! an unbound no-op default.
//!
//! # Documented deviations (boundary omissions at the sync host)
//!
//! These are limits of *this synchronous host*, not of [`create_bash_tool`],
//! whose `execute` fully supports the features below:
//!
//! - **No live `onUpdate` streaming.** `execute` accepts an `on_update`
//!   callback, but [`RealBashToolHost::run`] passes `None`: forwarding
//!   throttled snapshots would require Rust->JS reentrancy mid-`execute`, which
//!   is parked-adjacent. v1 is final-output-only.
//! - **No abort.** `execute` accepts a `signal` watch receiver; `run` passes
//!   `None`. Wiring cancellation has the same reentrancy cost and is deferred.
//! - **Env rewrite deferred.** `run` honors `command` and `cwd`; a per-command
//!   env override is a follow-up op parameter, not yet threaded through.
//!
//! # Bridge deviation from the literal seam sketch
//!
//! [`BashOperations::exec`](super::bash::BashOperations) returns a
//! `Pin<Box<dyn Future + 'a>>` with no `Send` bound, so the future returned by
//! `execute` is `!Send` and cannot be handed to `Handle::spawn` (which requires
//! `Send`). The bridge therefore drives it with `Handle::block_on` on a
//! dedicated `std::thread` (which carries no ambient runtime and so cannot
//! reenter one), and the calling thread blocks on `mpsc::Receiver::recv`. This
//! keeps the intended shape -- drive on the multi-thread handle, block the plane
//! thread on the channel -- without a `Send` future.

use std::sync::mpsc;

use super::bash::{create_bash_tool, BashToolDetails, BashToolResult};

/// A `Send + Sync` host that runs one bash command to completion.
///
/// The consumer holds a `dyn BashToolHost` and calls [`run`](Self::run)
/// synchronously; before a real host is bound it uses [`UnboundBashToolHost`].
pub trait BashToolHost: Send + Sync {
    /// Run `command` in `cwd` to completion and return the outcome. Never
    /// panics: an internal failure surfaces as an `ok == false` outcome.
    fn run(&self, command: &str, cwd: &str) -> BashRunOutcome;
}

/// The plain, synchronous result of [`BashToolHost::run`].
///
/// Maps [`create_bash_tool`]`(...).execute()`'s `Result` onto flat fields:
/// `Ok` -> `ok == true` with the raw `output`; `Err` -> `ok == false` with the
/// pi-exact `error` message. A non-zero exit is an `Err` in this port (its
/// `Command exited with code N` footer rides inside `error`), so it surfaces as
/// `ok == false`.
#[derive(Debug, Clone)]
pub struct BashRunOutcome {
    /// `true` when `execute` returned `Ok` (the command ran to a zero exit).
    pub ok: bool,
    /// The command output on success (`execute`'s `content`); empty on error.
    pub output: String,
    /// Truncation / full-output details, when `execute` supplied them.
    pub details: Option<BashToolDetails>,
    /// The pi-exact error message on failure (`None` on success). Carries the
    /// aborted / timeout / bad-cwd / non-zero-exit strings verbatim.
    pub error: Option<String>,
}

/// The channel-backed [`BashToolHost`]: runs each command through
/// [`create_bash_tool`] on a multi-thread runtime [`Handle`](tokio::runtime::Handle).
///
/// `Send + Sync` because its only field is a `tokio::runtime::Handle`.
pub struct RealBashToolHost {
    handle: tokio::runtime::Handle,
}

impl RealBashToolHost {
    /// Build a host that drives `execute` on `handle`, which must belong to a
    /// multi-thread runtime with IO and time enabled (`enable_all`).
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }
}

impl BashToolHost for RealBashToolHost {
    fn run(&self, command: &str, cwd: &str) -> BashRunOutcome {
        // Off-runtime drive: the `execute` future is `!Send`, so it cannot be
        // spawned onto the runtime. Instead a dedicated thread -- which has no
        // ambient runtime and therefore cannot reenter one -- drives it with
        // `Handle::block_on`, and this thread blocks on `rx.recv` until done.
        let (tx, rx) = mpsc::sync_channel::<Result<BashToolResult, String>>(1);
        let handle = self.handle.clone();
        let command = command.to_string();
        let cwd = cwd.to_string();

        std::thread::spawn(move || {
            let tool = create_bash_tool(cwd, None);
            // Run to completion: no streaming (`on_update`) and no abort
            // (`signal`) are wired -- both are documented boundary omissions.
            let result = handle.block_on(tool.execute(&command, None, None, None));
            // A dropped receiver (caller gone) simply drops the result.
            let _ = tx.send(result);
        });

        match rx.recv() {
            Ok(Ok(BashToolResult { content, details })) => BashRunOutcome {
                ok: true,
                output: content,
                details,
                error: None,
            },
            Ok(Err(message)) => BashRunOutcome {
                ok: false,
                output: String::new(),
                details: None,
                error: Some(message),
            },
            Err(_) => BashRunOutcome {
                ok: false,
                output: String::new(),
                details: None,
                error: Some("bash host worker terminated before completion".to_string()),
            },
        }
    }
}

/// The no-op [`BashToolHost`] used before a real host is bound: every `run`
/// fails with a fixed "not bound" message. Keeps the consumer total (it always
/// has a host) without pretending to execute anything.
pub struct UnboundBashToolHost;

impl BashToolHost for UnboundBashToolHost {
    fn run(&self, _command: &str, _cwd: &str) -> BashRunOutcome {
        BashRunOutcome {
            ok: false,
            output: String::new(),
            details: None,
            error: Some("bash host is not bound".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `RealBashToolHost` over a fresh multi-thread runtime, returning
    /// both so the runtime outlives the host for the duration of the test.
    fn host() -> (tokio::runtime::Runtime, RealBashToolHost) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let host = RealBashToolHost::new(runtime.handle().clone());
        (runtime, host)
    }

    #[test]
    fn runs_command_to_completion() {
        let (_runtime, host) = host();
        let cwd = std::env::temp_dir();
        let outcome = host.run("echo hi", cwd.to_str().unwrap());
        assert!(outcome.ok, "expected ok, got {outcome:?}");
        assert!(
            outcome.output.contains("hi"),
            "output missing 'hi': {:?}",
            outcome.output
        );
        assert!(outcome.error.is_none());
    }

    #[test]
    fn non_zero_exit_is_an_error_with_the_pi_footer() {
        // In this faithful port a non-zero exit maps to `execute` -> `Err`
        // ending in the `append_status` footer `Command exited with code <N>`
        // (bash.rs), so it surfaces as `ok == false` here.
        let (_runtime, host) = host();
        let cwd = std::env::temp_dir();
        let outcome = host.run("exit 3", cwd.to_str().unwrap());
        assert!(!outcome.ok, "expected failure, got {outcome:?}");
        let error = outcome.error.expect("expected an error message");
        assert!(
            error.contains("Command exited with code 3"),
            "error missing exact footer: {error:?}"
        );
    }

    #[test]
    fn bad_cwd_carries_the_pi_exact_message() {
        let (_runtime, host) = host();
        let outcome = host.run("echo hi", "/nonexistent/does/not/exist");
        assert!(!outcome.ok, "expected failure, got {outcome:?}");
        assert_eq!(
            outcome.error.as_deref(),
            Some(
                "Working directory does not exist: /nonexistent/does/not/exist\nCannot execute bash commands."
            )
        );
    }

    #[test]
    fn unbound_host_fails() {
        let outcome = UnboundBashToolHost.run("echo hi", "/tmp");
        assert!(!outcome.ok);
        assert!(outcome.error.is_some());
    }
}
